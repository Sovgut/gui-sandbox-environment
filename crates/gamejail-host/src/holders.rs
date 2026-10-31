//! Processes that hold a GPU's device nodes open.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::Root;
use crate::pci::Gpu;

/// UIDs below this belong to the system. Such processes are never signalled:
/// systemd and systemd-logind keep a session's DRM descriptors and release
/// them when the session ends, and signalling them takes the system down.
pub const FIRST_USER_UID: u32 = 1000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holder {
    pub pid: u32,
    pub uid: u32,
    pub comm: String,
    /// The process's cgroup, which names the unit that would restart it.
    pub cgroup: String,
}

impl Holder {
    #[must_use]
    pub fn is_user(&self) -> bool {
        self.uid >= FIRST_USER_UID
    }
}

/// Device nodes through which `gpu` can be held: its DRM nodes and, for an
/// NVIDIA card, the driver's control nodes. The NVIDIA nodes are shared by all
/// NVIDIA cards, so on a machine with several of them this over-reports.
#[must_use]
pub fn device_nodes(root: &Root, gpu: &Gpu) -> Vec<PathBuf> {
    let mut nodes: Vec<PathBuf> = gpu
        .drm_nodes
        .iter()
        .map(|n| PathBuf::from("/dev/dri").join(n))
        .collect();
    if gpu.is_nvidia() && gpu.display().driver.as_deref() == Some("nvidia") {
        nodes.extend(
            root.list("/dev")
                .into_iter()
                .filter(|n| n.starts_with("nvidia") && n != "nvidia-caps")
                .map(|n| PathBuf::from("/dev").join(n)),
        );
    }
    nodes
}

/// Every process with a descriptor open on one of `nodes`, by PID.
#[must_use]
pub fn scan(root: &Root, nodes: &[PathBuf]) -> Vec<Holder> {
    let mut holders = Vec::new();
    if nodes.is_empty() {
        return holders;
    }
    for pid in root
        .list("/proc")
        .iter()
        .filter_map(|p| p.parse::<u32>().ok())
    {
        let proc_dir = PathBuf::from("/proc").join(pid.to_string());
        if holds_any(root, &proc_dir, nodes) {
            let uid = std::fs::metadata(root.path(&proc_dir)).map_or(0, |m| m.uid());
            holders.push(Holder {
                pid,
                uid,
                comm: root.read_opt(proc_dir.join("comm")).unwrap_or_default(),
                cgroup: root
                    .read_opt(proc_dir.join("cgroup"))
                    .map(|c| c.trim_start_matches("0::").to_owned())
                    .unwrap_or_default(),
            });
        }
    }
    holders.sort_by_key(|h| h.pid);
    holders
}

fn holds_any(root: &Root, proc_dir: &Path, nodes: &[PathBuf]) -> bool {
    let Ok(fds) = std::fs::read_dir(root.path(proc_dir.join("fd"))) else {
        return false;
    };
    fds.filter_map(std::result::Result::ok)
        .filter_map(|fd| std::fs::read_link(fd.path()).ok())
        .any(|target| nodes.contains(&target))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::symlink;

    use super::*;
    use crate::pci::fixture;

    fn fake_process(root: &Root, pid: u32, comm: &str, fds: &[&str]) {
        let dir = root.path(format!("/proc/{pid}"));
        fs::create_dir_all(dir.join("fd")).unwrap();
        fs::write(dir.join("comm"), format!("{comm}\n")).unwrap();
        fs::write(dir.join("cgroup"), "0::/user.slice/app.slice/test.scope\n").unwrap();
        for (n, target) in fds.iter().enumerate() {
            symlink(target, dir.join("fd").join(n.to_string())).unwrap();
        }
    }

    #[test]
    fn finds_processes_holding_the_gpu_nodes_only() {
        let (_dir, root) = fixture::dev_pc();
        fake_process(
            &root,
            100,
            "kwin_wayland",
            &["/dev/dri/renderD128", "/dev/dri/renderD129"],
        );
        fake_process(&root, 200, "Discord", &["/dev/dri/renderD128"]);
        fake_process(&root, 300, "game", &["/dev/null", "/dev/dri/card0"]);

        let rtx = crate::pci::Gpu::read(&root, "0e:00".parse().unwrap()).unwrap();
        let holders = scan(&root, &device_nodes(&root, &rtx));

        let pids: Vec<u32> = holders.iter().map(|h| h.pid).collect();
        assert_eq!(pids, [100, 300]);
        assert_eq!(holders[0].comm, "kwin_wayland");
        assert_eq!(holders[0].cgroup, "/user.slice/app.slice/test.scope");
    }

    #[test]
    fn nouveau_card_has_no_nvidia_control_nodes() {
        let (_dir, root) = fixture::dev_pc();
        fs::create_dir_all(root.path("/dev")).unwrap();
        fs::write(root.path("/dev/nvidiactl"), "").unwrap();
        let rtx = crate::pci::Gpu::read(&root, "0e:00".parse().unwrap()).unwrap();
        assert_eq!(
            device_nodes(&root, &rtx),
            [
                PathBuf::from("/dev/dri/card0"),
                PathBuf::from("/dev/dri/renderD129")
            ]
        );
    }
}
