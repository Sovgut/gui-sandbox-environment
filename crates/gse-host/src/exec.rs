//! Reading the machine into a [`Snapshot`] and carrying out a [`Plan`].

use std::process::Command;
use std::thread::sleep;
use std::time::{Duration, Instant};

use tracing::{info, warn};

use crate::holders::{self, Holder};
use crate::pci::{Gpu, PciAddress};
use crate::plan::{DISPLAY_MANAGER, GPU_SERVICES, Plan, Snapshot, Step};
use crate::session;
use crate::{Error, Result, Root};

/// Runs programs on the host. Tests substitute their own.
pub trait System {
    /// Runs `program` and returns its standard output; a non-zero exit is an
    /// error carrying standard error.
    fn run(&self, program: &str, args: &[&str]) -> Result<String>;
}

pub struct HostSystem;

impl System for HostSystem {
    fn run(&self, program: &str, args: &[&str]) -> Result<String> {
        let line = || format!("{program} {}", args.join(" "));
        let output = Command::new(program)
            .args(args)
            .output()
            .map_err(|e| Error::Command(format!("{}: {e}", line())))?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            Err(Error::Command(format!(
                "{}: {}: {}",
                line(),
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            )))
        }
    }
}

pub fn snapshot(root: &Root, system: &dyn System, gpu: PciAddress) -> Result<Snapshot> {
    let gpu = Gpu::read(root, gpu)?;
    let active = |unit: &str| {
        system
            .run("systemctl", &["is-active", "--quiet", unit])
            .is_ok()
    };
    let mut graphical_sessions = Vec::new();
    let list = system.run("loginctl", &["list-sessions", "--no-legend"])?;
    for id in session::parse_session_ids(&list) {
        let properties = system.run(
            "loginctl",
            &["show-session", &id, "-p", "Type", "-p", "Name"],
        )?;
        graphical_sessions.extend(session::parse_graphical_session(&id, &properties));
    }
    Ok(Snapshot {
        gpu,
        display_manager_active: active(DISPLAY_MANAGER),
        graphical_sessions,
        gpu_services: GPU_SERVICES
            .iter()
            .filter(|u| active(u))
            .map(|u| (*u).to_owned())
            .collect(),
    })
}

pub struct Executor<'a> {
    pub root: &'a Root,
    pub system: &'a dyn System,
    /// Log every step and change nothing.
    pub dry_run: bool,
}

impl Executor<'_> {
    pub fn run(&self, plan: &Plan) -> Result<()> {
        for step in &plan.steps {
            if self.dry_run {
                info!("dry run: {step}");
                continue;
            }
            info!("{step}");
            self.step(plan.gpu, step)?;
        }
        Ok(())
    }

    fn step(&self, gpu: PciAddress, step: &Step) -> Result<()> {
        match step {
            Step::StopUnit(unit) => self.system.run("systemctl", &["stop", unit]).map(drop),
            Step::StartUnit(unit) => self.system.run("systemctl", &["start", unit]).map(drop),
            // Once the display manager stopped, the session and its user
            // manager may be ending already and refuse; that is no failure.
            Step::StopUserUnit { user, unit } => {
                let machine = format!("--machine={user}@");
                if let Err(e) = self
                    .system
                    .run("systemctl", &["--user", &machine, "stop", unit])
                {
                    warn!("{e}");
                }
                Ok(())
            }
            Step::TerminateSession(id) => {
                if let Err(e) = self.system.run("loginctl", &["terminate-session", id]) {
                    warn!("{e}");
                }
                Ok(())
            }
            Step::StopUserManager(user) => {
                let stopped = self
                    .system
                    .run("id", &["-u", user])
                    .and_then(|uid| {
                        let unit = format!("user@{}.service", uid.trim());
                        self.system.run("systemctl", &["stop", &unit])
                    });
                if let Err(e) = stopped {
                    warn!("{e}");
                }
                Ok(())
            }
            Step::UnbindConsoles => {
                self.bind_consoles(false);
                Ok(())
            }
            Step::BindConsoles => {
                self.bind_consoles(true);
                Ok(())
            }
            Step::AwaitSessionsEnd(wait) => {
                let left = wait_until_empty(*wait, || self.graphical_sessions());
                for id in &left {
                    warn!("graphical session {id} is still open: terminating it");
                    if let Err(e) = self.system.run("loginctl", &["terminate-session", id]) {
                        warn!("{e}");
                    }
                }
                if !left.is_empty() {
                    let left = wait_until_empty(Duration::from_secs(5), || self.graphical_sessions());
                    if !left.is_empty() {
                        warn!("graphical sessions still open: {}", left.join(" "));
                    }
                }
                Ok(())
            }
            Step::ReleaseGpu {
                wait,
                signal_leftovers,
                allow_idle_compositor,
            } => self.release(gpu, *wait, *signal_leftovers, *allow_idle_compositor),
            Step::UnloadModules(modules) => self.modprobe(&["-r"], modules),
            Step::LoadModules(modules) => self.modprobe(&["-a"], modules),
            Step::SetOverride { function, driver } => self.root.write(
                function.sysfs().join("driver_override"),
                driver.as_deref().unwrap_or(""),
            ),
            Step::Unbind { function, driver } => {
                let dir = function.sysfs();
                match self.root.link_name(dir.join("driver")) {
                    Some(bound) if driver.as_ref().is_none_or(|d| *d == bound) => self
                        .root
                        .write(dir.join("driver/unbind"), &function.to_string()),
                    _ => Ok(()),
                }
            }
            Step::Probe(function) => {
                if self.root.exists(function.sysfs().join("driver")) {
                    Ok(())
                } else {
                    self.root
                        .write("/sys/bus/pci/drivers_probe", &function.to_string())
                }
            }
            Step::ExpectDriver { function, driver } => {
                let bound = self.root.link_name(function.sysfs().join("driver"));
                if bound == *driver {
                    Ok(())
                } else {
                    Err(Error::Step(format!(
                        "{function} is on {}, expected {}",
                        bound.as_deref().unwrap_or("no driver"),
                        driver.as_deref().unwrap_or("no driver")
                    )))
                }
            }
        }
    }

    fn modprobe(&self, flags: &[&str], modules: &[String]) -> Result<()> {
        let mut args: Vec<&str> = flags.to_vec();
        args.extend(modules.iter().map(String::as_str));
        self.system.run("modprobe", &args).map(drop)
    }

    fn holders(&self, gpu: PciAddress) -> Vec<Holder> {
        match Gpu::read(self.root, gpu) {
            Ok(gpu) => holders::scan(self.root, &holders::device_nodes(self.root, &gpu)),
            Err(_) => Vec::new(),
        }
    }

    /// User processes holding the GPU, leaving out an idle compositor when
    /// that is allowed.
    fn user_holders(&self, gpu: PciAddress, allow_idle_compositor: bool) -> Vec<Holder> {
        self.holders(gpu)
            .into_iter()
            .filter(Holder::is_user)
            .filter(|h| {
                let idle = allow_idle_compositor && h.is_idle_compositor();
                if idle {
                    info!("{} {} holds only an unused render node", h.pid, h.comm);
                }
                !idle
            })
            .collect()
    }

    fn release(
        &self,
        gpu: PciAddress,
        wait: Duration,
        signal: bool,
        allow_idle_compositor: bool,
    ) -> Result<()> {
        let users_now = || self.user_holders(gpu, allow_idle_compositor);
        let mut users = wait_until_empty(wait, users_now);
        if signal {
            for sig in ["TERM", "KILL"] {
                if users.is_empty() {
                    break;
                }
                for holder in &users {
                    warn!(
                        "SIG{sig} to {} {} ({})",
                        holder.pid, holder.comm, holder.cgroup
                    );
                    if let Err(e) = self
                        .system
                        .run("kill", &["-s", sig, &holder.pid.to_string()])
                    {
                        warn!("{e}");
                    }
                }
                users = wait_until_empty(Duration::from_secs(3), users_now);
            }
        }
        let system_holders = wait_until_empty(Duration::from_secs(5), || {
            self.holders(gpu)
                .into_iter()
                .filter(|h| !h.is_user())
                .collect()
        });
        for holder in system_holders.iter().chain(&users) {
            warn!(
                "still held by {} {} (uid {}, {})",
                holder.pid, holder.comm, holder.uid, holder.cgroup
            );
        }
        if !users.is_empty() {
            let names: Vec<String> = users
                .iter()
                .map(|h| format!("{} ({})", h.comm, h.pid))
                .collect();
            return Err(Error::Step(format!(
                "the GPU is in use by {}; close it and try again",
                names.join(", ")
            )));
        }
        Ok(())
    }
}

impl Executor<'_> {
    /// Binds or unbinds every text console that sits on a framebuffer.
    fn bind_consoles(&self, bind: bool) {
        for console in self.root.list("/sys/class/vtconsole") {
            let dir = std::path::Path::new("/sys/class/vtconsole").join(&console);
            let on_framebuffer = self
                .root
                .read_opt(dir.join("name"))
                .is_some_and(|name| name.contains("frame buffer"));
            if on_framebuffer
                && let Err(e) = self.root.write(dir.join("bind"), if bind { "1" } else { "0" })
            {
                warn!("{console}: {e}");
            }
        }
    }

    /// The ids of the graphical sessions open now.
    fn graphical_sessions(&self) -> Vec<String> {
        let Ok(list) = self.system.run("loginctl", &["list-sessions", "--no-legend"]) else {
            return Vec::new();
        };
        session::parse_session_ids(&list)
            .into_iter()
            .filter(|id| {
                self.system
                    .run("loginctl", &["show-session", id, "-p", "Type", "-p", "Name"])
                    .ok()
                    .and_then(|properties| session::parse_graphical_session(id, &properties))
                    .is_some()
            })
            .collect()
    }
}

fn wait_until_empty<T>(wait: Duration, mut probe: impl FnMut() -> Vec<T>) -> Vec<T> {
    let deadline = Instant::now() + wait;
    loop {
        let found = probe();
        if found.is_empty() || Instant::now() >= deadline {
            return found;
        }
        sleep(Duration::from_millis(250));
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::fs;
    use std::os::unix::fs::{MetadataExt, symlink};

    use super::*;
    use crate::pci::fixture;
    use crate::plan;
    use crate::session::Session;

    #[derive(Default)]
    struct Recorder {
        calls: RefCell<Vec<String>>,
    }

    impl System for Recorder {
        fn run(&self, program: &str, args: &[&str]) -> Result<String> {
            self.calls
                .borrow_mut()
                .push(format!("{program} {}", args.join(" ")));
            match (program, args.first().copied()) {
                ("loginctl", Some("list-sessions")) => {
                    Ok("2 1000 sovhut seat0 1 user tty2 no -\n".into())
                }
                ("loginctl", Some("show-session")) => Ok("Type=wayland\nName=sovhut\n".into()),
                ("systemctl", Some("is-active")) => {
                    if args.contains(&DISPLAY_MANAGER) {
                        Ok(String::new())
                    } else {
                        Err(Error::Command("inactive".into()))
                    }
                }
                _ => Ok(String::new()),
            }
        }
    }

    #[test]
    fn snapshot_reads_sessions_and_units() {
        let (_dir, root) = fixture::dev_pc();
        let snapshot = snapshot(&root, &Recorder::default(), "0e:00".parse().unwrap()).unwrap();
        assert!(snapshot.display_manager_active);
        assert_eq!(
            snapshot.graphical_sessions,
            [Session {
                id: "2".into(),
                user: "sovhut".into()
            }]
        );
        assert_eq!(snapshot.gpu_services, Vec::<String>::new());
    }

    #[test]
    fn sysfs_steps_write_the_attributes() {
        let (_dir, root) = fixture::dev_pc();
        let recorder = Recorder::default();
        let executor = Executor {
            root: &root,
            system: &recorder,
            dry_run: false,
        };
        let audio: PciAddress = "0000:0e:00.1".parse().unwrap();
        let dir = root.path(audio.sysfs());
        // The fixture's driver links point into a tree that does not exist;
        // give the unbind attribute somewhere to land.
        fs::create_dir_all(root.path("/sys/bus/pci/drivers/snd_hda_intel")).unwrap();
        fs::create_dir_all(root.path("/sys/bus/pci")).unwrap();

        let plan = Plan {
            gpu: "0e:00".parse().unwrap(),
            steps: vec![
                Step::SetOverride {
                    function: audio,
                    driver: Some("vfio-pci".into()),
                },
                Step::Unbind {
                    function: audio,
                    driver: Some("i915".into()),
                },
                Step::SetOverride {
                    function: audio,
                    driver: None,
                },
            ],
        };
        executor.run(&plan).unwrap();
        assert_eq!(
            fs::read_to_string(dir.join("driver_override")).unwrap(),
            "\n"
        );
        assert!(
            !root.exists("/sys/bus/pci/drivers/snd_hda_intel/unbind"),
            "bound to another driver, so left alone"
        );
    }

    #[test]
    fn expect_driver_reports_a_mismatch() {
        let (_dir, root) = fixture::dev_pc();
        let executor = Executor {
            root: &root,
            system: &Recorder::default(),
            dry_run: false,
        };
        let gpu: PciAddress = "0e:00".parse().unwrap();
        let plan = Plan {
            gpu,
            steps: vec![Step::ExpectDriver {
                function: gpu,
                driver: Some("vfio-pci".into()),
            }],
        };
        let error = executor.run(&plan).unwrap_err().to_string();
        assert_eq!(error, "0000:0e:00.0 is on nouveau, expected vfio-pci");
    }

    fn fake_holder(root: &Root, pid: u32, comm: &str, node: &str) {
        let proc_fd = root.path(format!("/proc/{pid}/fd"));
        fs::create_dir_all(&proc_fd).unwrap();
        fs::write(root.path(format!("/proc/{pid}/comm")), format!("{comm}\n")).unwrap();
        symlink(node, proc_fd.join("7")).unwrap();
    }

    fn release(root: &Root, recorder: &Recorder) -> Result<()> {
        let executor = Executor {
            root,
            system: recorder,
            dry_run: false,
        };
        executor.run(&Plan {
            gpu: "0e:00".parse().unwrap(),
            steps: vec![Step::ReleaseGpu {
                wait: Duration::ZERO,
                signal_leftovers: false,
                allow_idle_compositor: true,
            }],
        })
    }

    /// The fixture is owned by the test user, so its processes count as user
    /// processes only when the tests run as a regular user.
    fn runs_as_user(root: &Root) -> bool {
        fs::metadata(root.path("/proc")).unwrap().uid() >= holders::FIRST_USER_UID
    }

    #[test]
    fn an_idle_compositor_does_not_block_the_release() {
        let (_dir, root) = fixture::dev_pc();
        fake_holder(&root, 4242, "kwin_wayland", "/dev/dri/renderD129");
        assert!(release(&root, &Recorder::default()).is_ok());
    }

    #[test]
    fn an_application_on_the_gpu_blocks_the_release_and_is_not_signalled() {
        let (_dir, root) = fixture::dev_pc();
        fake_holder(&root, 4242, "kwin_wayland", "/dev/dri/renderD129");
        fake_holder(&root, 4343, "zed-editor", "/dev/dri/renderD129");
        let recorder = Recorder::default();
        let result = release(&root, &recorder);
        if runs_as_user(&root) {
            let error = result.unwrap_err().to_string();
            assert_eq!(
                error,
                "the GPU is in use by zed-editor (4343); close it and try again"
            );
        }
        assert!(
            recorder
                .calls
                .borrow()
                .iter()
                .all(|c| !c.starts_with("kill"))
        );
    }

    #[test]
    fn dry_run_changes_nothing() {
        let (_dir, root) = fixture::dev_pc();
        let recorder = Recorder::default();
        let snapshot = snapshot(&root, &recorder, "0e:00".parse().unwrap()).unwrap();
        recorder.calls.borrow_mut().clear();
        let (plan, _) = plan::attach(&snapshot);
        Executor {
            root: &root,
            system: &recorder,
            dry_run: true,
        }
        .run(&plan)
        .unwrap();
        assert!(recorder.calls.borrow().is_empty());
        assert_eq!(
            root.read_opt("/sys/bus/pci/devices/0000:0e:00.0/driver_override")
                .as_deref(),
            Some("(null)")
        );
    }
}
