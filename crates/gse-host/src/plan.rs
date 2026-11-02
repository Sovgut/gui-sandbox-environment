//! Plans that move a GPU from the host to `vfio-pci` and back.
//!
//! A plan is built from a [`Snapshot`] of the machine and is pure data, so it
//! can be printed, tested and dry-run. Steps that depend on the state at the
//! moment they run (whether a function is still bound, who still holds the
//! GPU) check it themselves in the executor.

use std::fmt::{self, Write as _};
use std::time::Duration;

use crate::pci::{Gpu, PciAddress};
use crate::session::Session;
use crate::{Error, Result};

pub const DISPLAY_MANAGER: &str = "display-manager.service";
pub const GRAPHICAL_SESSION: &str = "graphical-session.target";
pub const VFIO_PCI: &str = "vfio-pci";

/// Services that keep the NVIDIA driver busy without holding a device node.
pub const GPU_SERVICES: [&str; 2] = ["nvidia-powerd.service", "nvidia-persistenced.service"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The GPU drives the host desktop, which has to stop while an environment runs.
    Single,
    /// Another GPU drives the desktop; only this GPU changes hands.
    Secondary,
}

impl Mode {
    #[must_use]
    pub fn for_gpu(gpu: &Gpu) -> Self {
        if gpu.drives_display() {
            Self::Single
        } else {
            Self::Secondary
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Single => "single",
            Self::Secondary => "secondary",
        })
    }
}

/// What a plan needs to know about the machine.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub gpu: Gpu,
    pub display_manager_active: bool,
    pub graphical_sessions: Vec<Session>,
    /// Running units from [`GPU_SERVICES`].
    pub gpu_services: Vec<String>,
}

/// What the host had before the GPU left it, so detaching can restore it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handoff {
    pub mode: Mode,
    pub display_manager_was_active: bool,
    pub gpu_services: Vec<String>,
    /// Each function and the driver it had.
    pub drivers: Vec<(PciAddress, Option<String>)>,
}

impl Handoff {
    #[must_use]
    pub fn host_driver(&self) -> Option<&str> {
        self.drivers.first().and_then(|(_, d)| d.as_deref())
    }

    /// One `key value...` line per fact.
    #[must_use]
    pub fn to_text(&self) -> String {
        let mut out = format!(
            "mode {}\ndisplay-manager {}\n",
            self.mode,
            u8::from(self.display_manager_was_active)
        );
        for service in &self.gpu_services {
            let _ = writeln!(out, "service {service}");
        }
        for (address, driver) in &self.drivers {
            let _ = writeln!(out, "driver {address} {}", driver.as_deref().unwrap_or("-"));
        }
        out
    }

    pub fn from_text(text: &str) -> Result<Self> {
        let bad = |line: &str| Error::Step(format!("unreadable handoff line '{line}'"));
        let mut handoff = Self {
            mode: Mode::Secondary,
            display_manager_was_active: false,
            gpu_services: Vec::new(),
            drivers: Vec::new(),
        };
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let fields: Vec<&str> = line.split_whitespace().collect();
            match fields.as_slice() {
                ["mode", "single"] => handoff.mode = Mode::Single,
                ["mode", "secondary"] => handoff.mode = Mode::Secondary,
                ["display-manager", v] => handoff.display_manager_was_active = *v == "1",
                ["service", s] => handoff.gpu_services.push((*s).to_owned()),
                ["driver", address, driver] => handoff.drivers.push((
                    address.parse()?,
                    (*driver != "-").then(|| (*driver).to_owned()),
                )),
                _ => return Err(bad(line)),
            }
        }
        Ok(handoff)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    StopUnit(String),
    StartUnit(String),
    StopUserUnit {
        user: String,
        unit: String,
    },
    /// Ends a graphical session. Like [`Step::StopUserUnit`] it may find
    /// the session gone already, once the display manager stopped; the
    /// executor only warns then, and [`Step::ReleaseGpu`] decides.
    TerminateSession(String),
    /// Detaches the text consoles from the framebuffer of the host driver,
    /// which otherwise keeps `nvidia_drm` (with `fbdev`) in use.
    UnbindConsoles,
    /// Attaches the framebuffer consoles again.
    BindConsoles,
    /// Waits for every graphical session to end, so a display manager
    /// started again does not log in beside a session still letting go of
    /// the GPU.
    AwaitSessionsEnd(Duration),
    /// Waits for user processes to let go of the GPU, and fails if any still
    /// hold it at the end: unbinding the driver from a GPU with live buffers
    /// corrupts kernel memory. Leftovers are signalled when
    /// `signal_leftovers` is set; system processes are only waited for. With
    /// `allow_idle_compositor`, `kwin_wayland`'s unused render-node descriptor (see
    /// [`crate::holders::Holder::is_idle_compositor`]) does not count.
    ReleaseGpu {
        wait: Duration,
        signal_leftovers: bool,
        allow_idle_compositor: bool,
    },
    UnloadModules(Vec<String>),
    LoadModules(Vec<String>),
    SetOverride {
        function: PciAddress,
        driver: Option<String>,
    },
    /// Unbinds the function if it is bound, and only from `driver` if given.
    Unbind {
        function: PciAddress,
        driver: Option<String>,
    },
    /// Asks the kernel to bind a driver to the function if none is bound.
    Probe(PciAddress),
    ExpectDriver {
        function: PciAddress,
        driver: Option<String>,
    },
}

impl fmt::Display for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let or_any = |d: &Option<String>| d.clone().unwrap_or_else(|| "any driver".into());
        match self {
            Self::StopUnit(u) => write!(f, "stop {u}"),
            Self::StartUnit(u) => write!(f, "start {u}"),
            Self::StopUserUnit { user, unit } => write!(f, "stop {unit} of {user}"),
            Self::TerminateSession(s) => write!(f, "terminate session {s}"),
            Self::UnbindConsoles => f.write_str("unbind the framebuffer consoles"),
            Self::BindConsoles => f.write_str("bind the framebuffer consoles"),
            Self::AwaitSessionsEnd(wait) => write!(
                f,
                "wait up to {}s for the graphical sessions to end",
                wait.as_secs()
            ),
            Self::ReleaseGpu {
                wait,
                signal_leftovers,
                allow_idle_compositor,
            } => write!(
                f,
                "wait up to {}s for the GPU to be released{}, fail if still held{}",
                wait.as_secs(),
                if *signal_leftovers {
                    ", signalling user leftovers"
                } else {
                    ""
                },
                if *allow_idle_compositor {
                    " by anything but an idle compositor"
                } else {
                    ""
                }
            ),
            Self::UnloadModules(m) => write!(f, "unload {}", m.join(" ")),
            Self::LoadModules(m) => write!(f, "load {}", m.join(" ")),
            Self::SetOverride { function, driver } => {
                write!(
                    f,
                    "{function}: driver_override = {}",
                    driver.as_deref().unwrap_or("(clear)")
                )
            }
            Self::Unbind { function, driver } => {
                write!(f, "{function}: unbind from {}", or_any(driver))
            }
            Self::Probe(function) => write!(f, "{function}: probe if unbound"),
            Self::ExpectDriver { function, driver } => {
                write!(
                    f,
                    "{function}: expect {}",
                    driver.as_deref().unwrap_or("no driver")
                )
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// The GPU's display function; the executor rereads the GPU through it.
    pub gpu: PciAddress,
    pub steps: Vec<Step>,
}

/// Modules to remove before an NVIDIA card can leave its driver, in the order
/// `modprobe -r` accepts them. Other drivers are simply unbound.
fn unload_modules(driver: Option<&str>) -> Vec<String> {
    match driver {
        Some("nvidia") => ["nvidia_drm", "nvidia_modeset", "nvidia_uvm", "nvidia"]
            .map(String::from)
            .to_vec(),
        _ => Vec::new(),
    }
}

fn load_modules(driver: Option<&str>) -> Vec<String> {
    match driver {
        Some("nvidia") => ["nvidia", "nvidia_modeset", "nvidia_drm", "nvidia_uvm"]
            .map(String::from)
            .to_vec(),
        _ => Vec::new(),
    }
}

/// The plan that hands the GPU to `vfio-pci`, and what to restore afterwards.
#[must_use]
pub fn attach(snapshot: &Snapshot) -> (Plan, Handoff) {
    let gpu = &snapshot.gpu;
    let mode = Mode::for_gpu(gpu);
    let host_driver = gpu.display().driver.as_deref();
    let mut steps = Vec::new();

    match mode {
        Mode::Single => {
            if snapshot.display_manager_active {
                steps.push(Step::StopUnit(DISPLAY_MANAGER.into()));
            }
            for session in &snapshot.graphical_sessions {
                steps.push(Step::StopUserUnit {
                    user: session.user.clone(),
                    unit: GRAPHICAL_SESSION.into(),
                });
                steps.push(Step::TerminateSession(session.id.clone()));
            }
            steps.extend(snapshot.gpu_services.iter().cloned().map(Step::StopUnit));
            steps.push(Step::ReleaseGpu {
                wait: Duration::from_secs(10),
                signal_leftovers: true,
                allow_idle_compositor: false,
            });
            steps.push(Step::UnbindConsoles);
            let unload = unload_modules(host_driver);
            if !unload.is_empty() {
                steps.push(Step::UnloadModules(unload));
            }
        }
        Mode::Secondary => {
            // The desktop stays up, so applications keep running: anything
            // rendering on this GPU has to be closed by the user first.
            steps.push(Step::ReleaseGpu {
                wait: Duration::from_secs(2),
                signal_leftovers: false,
                allow_idle_compositor: true,
            });
        }
    }

    steps.push(Step::LoadModules(vec![VFIO_PCI.into()]));
    for function in &gpu.functions {
        let address = function.address;
        steps.push(Step::SetOverride {
            function: address,
            driver: Some(VFIO_PCI.into()),
        });
        steps.push(Step::Unbind {
            function: address,
            driver: None,
        });
        steps.push(Step::Probe(address));
        steps.push(Step::ExpectDriver {
            function: address,
            driver: Some(VFIO_PCI.into()),
        });
    }

    let handoff = Handoff {
        mode,
        display_manager_was_active: mode == Mode::Single && snapshot.display_manager_active,
        gpu_services: if mode == Mode::Single {
            snapshot.gpu_services.clone()
        } else {
            Vec::new()
        },
        drivers: gpu
            .functions
            .iter()
            .map(|f| (f.address, f.driver.clone()))
            .collect(),
    };
    (
        Plan {
            gpu: gpu.display().address,
            steps,
        },
        handoff,
    )
}

/// The plan that returns the GPU from `vfio-pci` to the drivers in `handoff`.
#[must_use]
pub fn detach(handoff: &Handoff) -> Plan {
    let mut steps = Vec::new();
    for (address, _) in &handoff.drivers {
        steps.push(Step::SetOverride {
            function: *address,
            driver: None,
        });
        steps.push(Step::Unbind {
            function: *address,
            driver: Some(VFIO_PCI.into()),
        });
    }
    let load = load_modules(handoff.host_driver());
    if !load.is_empty() {
        steps.push(Step::LoadModules(load));
    }
    for (address, driver) in &handoff.drivers {
        steps.push(Step::Probe(*address));
        if driver.is_some() {
            steps.push(Step::ExpectDriver {
                function: *address,
                driver: driver.clone(),
            });
        }
    }
    if handoff.mode == Mode::Single {
        steps.push(Step::BindConsoles);
    }
    steps.extend(handoff.gpu_services.iter().cloned().map(Step::StartUnit));
    if handoff.display_manager_was_active {
        steps.push(Step::AwaitSessionsEnd(Duration::from_secs(15)));
        steps.push(Step::StartUnit(DISPLAY_MANAGER.into()));
    }
    let gpu = handoff.drivers.first().map_or(
        PciAddress {
            domain: 0,
            bus: 0,
            device: 0,
            function: 0,
        },
        |(address, _)| *address,
    );
    Plan { gpu, steps }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pci::fixture::{self, Function};

    fn rtx_dev_pc() -> (tempfile::TempDir, Snapshot) {
        let (dir, root) = fixture::dev_pc();
        let gpu = Gpu::read(&root, "0e:00".parse().unwrap()).unwrap();
        let snapshot = Snapshot {
            gpu,
            display_manager_active: true,
            graphical_sessions: vec![Session {
                id: "2".into(),
                user: "sovhut".into(),
            }],
            gpu_services: Vec::new(),
        };
        (dir, snapshot)
    }

    /// The target machine: one NVIDIA card on the proprietary driver, with
    /// the monitor on it.
    fn rtx_target() -> (tempfile::TempDir, Snapshot) {
        let dir = tempfile::tempdir().unwrap();
        let root = crate::Root::at(dir.path());
        fixture::add(
            &root,
            &Function {
                address: "0000:09:00.0",
                vendor: 0x10de,
                device: 0x2488,
                class: 0x03_0000,
                driver: Some("nvidia"),
                group: 16,
                boot_vga: true,
            },
        );
        fixture::add(
            &root,
            &Function {
                address: "0000:09:00.1",
                vendor: 0x10de,
                device: 0x228b,
                class: 0x04_0300,
                driver: Some("snd_hda_intel"),
                group: 16,
                boot_vga: false,
            },
        );
        fixture::add_card(
            &root,
            "0000:09:00.0",
            "card1",
            "renderD128",
            &[("HDMI-A-1", true)],
        );
        let gpu = Gpu::read(&root, "09:00".parse().unwrap()).unwrap();
        let snapshot = Snapshot {
            gpu,
            display_manager_active: true,
            graphical_sessions: vec![Session {
                id: "2".into(),
                user: "dreamer".into(),
            }],
            gpu_services: vec!["nvidia-powerd.service".into()],
        };
        (dir, snapshot)
    }

    fn rendered(plan: &Plan) -> Vec<String> {
        plan.steps.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn secondary_mode_leaves_the_desktop_alone() {
        let (_dir, snapshot) = rtx_dev_pc();
        let (plan, handoff) = attach(&snapshot);
        assert_eq!(handoff.mode, Mode::Secondary);
        assert!(
            !plan
                .steps
                .iter()
                .any(|s| matches!(s, Step::StopUnit(_) | Step::TerminateSession(_)))
        );
        assert_eq!(
            plan.steps[0],
            Step::ReleaseGpu {
                wait: Duration::from_secs(2),
                signal_leftovers: false,
                allow_idle_compositor: true
            }
        );
        assert!(rendered(&plan).contains(&"0000:0e:00.1: expect vfio-pci".to_owned()));
        assert!(!handoff.display_manager_was_active);
    }

    #[test]
    fn single_mode_ends_the_session_before_unloading_nvidia() {
        let (_dir, snapshot) = rtx_target();
        let (plan, handoff) = attach(&snapshot);
        assert_eq!(handoff.mode, Mode::Single);
        let steps = rendered(&plan);
        let position = |text: &str| {
            steps
                .iter()
                .position(|s| s == text)
                .unwrap_or_else(|| panic!("missing step '{text}'"))
        };
        assert!(
            position("stop display-manager.service")
                < position("stop graphical-session.target of dreamer")
        );
        assert!(
            position("stop graphical-session.target of dreamer") < position("terminate session 2")
        );
        assert!(position("terminate session 2") < position("stop nvidia-powerd.service"));
        assert!(
            position("unbind the framebuffer consoles")
                < position("unload nvidia_drm nvidia_modeset nvidia_uvm nvidia")
        );
        assert!(
            position("unload nvidia_drm nvidia_modeset nvidia_uvm nvidia")
                < position("load vfio-pci")
        );
        assert!(position("load vfio-pci") < position("0000:09:00.0: driver_override = vfio-pci"));
    }

    #[test]
    fn detach_restores_what_attach_recorded() {
        let (_dir, snapshot) = rtx_target();
        let (_, handoff) = attach(&snapshot);
        let steps = rendered(&detach(&handoff));
        assert_eq!(steps[0], "0000:09:00.0: driver_override = (clear)");
        assert!(steps.contains(&"load nvidia nvidia_modeset nvidia_drm nvidia_uvm".to_owned()));
        assert!(steps.contains(&"0000:09:00.0: expect nvidia".to_owned()));
        assert!(steps.contains(&"0000:09:00.1: expect snd_hda_intel".to_owned()));
        let tail: Vec<&str> = steps[steps.len() - 4..].iter().map(String::as_str).collect();
        assert_eq!(
            tail,
            [
                "bind the framebuffer consoles",
                "start nvidia-powerd.service",
                "wait up to 15s for the graphical sessions to end",
                "start display-manager.service",
            ]
        );
    }

    #[test]
    fn handoff_survives_a_round_trip_through_text() {
        let (_dir, snapshot) = rtx_target();
        let (_, handoff) = attach(&snapshot);
        assert_eq!(Handoff::from_text(&handoff.to_text()).unwrap(), handoff);
    }
}
