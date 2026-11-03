//! What each host driver needs for a GPU to leave it and come back.
//!
//! Unloading modules is needed only for a driver that cannot let go of one
//! device while it keeps others, which is NVIDIA's; `nouveau`, `amdgpu`,
//! `i915` and `xe` unbind per device, which also keeps another GPU on the
//! same driver (an integrated one) working on the host.

/// A host kernel driver of GPUs.
#[derive(Debug, PartialEq, Eq)]
pub struct HostDriver {
    pub name: &'static str,
    /// Modules removed before the GPU can leave, in the order `modprobe -r`
    /// accepts them.
    pub unload: &'static [&'static str],
    /// Modules loaded again before the GPU comes back.
    pub load: &'static [&'static str],
    /// Units that keep the driver busy without holding a device node:
    /// stopped before the GPU leaves a single-GPU host, started after.
    pub services: &'static [&'static str],
    /// Device nodes in `/dev`, by name prefix, through which the GPU is held
    /// besides its DRM nodes. They are shared by every GPU on the driver,
    /// so on a machine with several such GPUs a holder of another counts.
    pub control_nodes: &'static [&'static str],
}

const DRIVERS: [HostDriver; 5] = [
    HostDriver {
        name: "nvidia",
        unload: &["nvidia_drm", "nvidia_modeset", "nvidia_uvm", "nvidia"],
        load: &["nvidia", "nvidia_modeset", "nvidia_drm", "nvidia_uvm"],
        services: &["nvidia-powerd.service", "nvidia-persistenced.service"],
        control_nodes: &["nvidia"],
    },
    HostDriver {
        name: "nouveau",
        unload: &[],
        load: &[],
        services: &[],
        control_nodes: &[],
    },
    HostDriver {
        name: "amdgpu",
        unload: &[],
        load: &[],
        services: &[],
        // ROCm and OpenCL programs hold the GPU through the compute node.
        control_nodes: &["kfd"],
    },
    HostDriver {
        name: "i915",
        unload: &[],
        load: &[],
        services: &[],
        control_nodes: &[],
    },
    HostDriver {
        name: "xe",
        unload: &[],
        load: &[],
        services: &[],
        control_nodes: &[],
    },
];

/// A driver gse knows nothing special about: the GPU is unbound and bound
/// again, and only its DRM nodes count.
const PLAIN: HostDriver = HostDriver {
    name: "",
    unload: &[],
    load: &[],
    services: &[],
    control_nodes: &[],
};

/// What `name` needs; a driver not in the table, or none, needs nothing
/// beyond unbinding.
#[must_use]
pub fn host_driver(name: Option<&str>) -> &'static HostDriver {
    name.and_then(|name| DRIVERS.iter().find(|d| d.name == name))
        .unwrap_or(&PLAIN)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_nvidia_unloads_modules() {
        assert_eq!(host_driver(Some("nvidia")).unload.len(), 4);
        for name in ["nouveau", "amdgpu", "i915", "xe", "radeon"] {
            assert!(host_driver(Some(name)).unload.is_empty(), "{name}");
        }
        assert_eq!(host_driver(None), &PLAIN);
        assert_eq!(host_driver(Some("amdgpu")).control_nodes, ["kfd"]);
    }
}
