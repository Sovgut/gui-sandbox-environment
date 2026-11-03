# Hardware support design

**Status: proposed, not implemented.** gse runs today on one NVIDIA card
(tested on an RTX 3070) with an AMD CPU; `docs/design.md` describes that. This
document is the design for every other GPU and CPU, written before the
hardware is at hand: testing on AMD and Intel machines will be rare, so the
structure has to be right first and the first runs on new hardware should only
find bugs in it, not missing places for it. Once a part is implemented, its
description moves into `docs/design.md` and leaves this file.

## What is tied to one vendor today

| Place | What it assumes |
| :--- | :--- |
| `gse-host/src/vm.rs` (`-cpu host,topoext=on`) | `topoext` is an AMD CPUID flag |
| `gse-host/src/vm.rs` (`-smp ...threads=2`) | every core has two threads |
| `gse-host/src/plan.rs` (`unload_modules`, `GPU_SERVICES`) | only `nvidia` needs modules unloaded and services stopped |
| `gse-host/src/pci.rs` (`Gpu::read`) | a GPU's companions (HDMI audio) share its PCI slot |
| `gse-host/src/holders.rs` | NVIDIA's control nodes are the only extra device nodes |
| `gse-service/src/gpu.rs` (`default_gpu`) | discrete means vendor `10de` or `1002` |
| `gse-service/src/setup.rs` (`check`) | one warning text for both CPU vendors |
| `gse-app/src/gpu.rs` | the desktop GPU and the sandbox GPU have different vendors |
| `guest/base/provision.sh` | the guest needs `akmod-nvidia` and nothing else |
| `gse-service/src/image.rs` (`base_fingerprint`) | the base image depends on the script alone |

Everything below replaces these assumptions with data read from the machine.
For the RTX 3070 target and the development PC the result must be exactly
what runs today; their fixtures (see "Testing without the hardware") pin that.

## Terms

- **Host driver**: the kernel driver the GPU has on the host before it is
  handed over (`nvidia`, `nouveau`, `amdgpu`, `i915`, `xe`). It decides how
  the GPU leaves the host and comes back.
- **Guest driver**: the driver stack the sandbox runs the GPU with. It is
  independent of the host driver: the development PC runs the 3070 on
  `nouveau` and the sandbox runs it on the proprietary driver.
- **Hardware profile**: what the service reads about the CPU and every GPU.
- **Driver pack**: a named set of guest packages and settings that
  `provision.sh` knows how to install.
- **Support level**: Tested, Expected or Unsupported, with a reason, shown to
  the user before anything is built.

## Hardware profile

A new module `gse-host/src/hardware.rs` reads the profile through `Root`, like
`pci.rs`, so every test can point it at a fixture tree:

```rust
pub struct Profile {
    pub cpu: Cpu,
    pub gpus: Vec<GpuInfo>,
    pub kernel: String,           // uname -r, for kernel-dependent traps
}

pub struct Cpu {
    pub vendor: CpuVendor,        // Amd | Intel | Other(String), from /proc/cpuinfo vendor_id
    pub model_name: String,
    pub family: u32, pub model: u32,
    pub flags: BTreeSet<String>,  // svm, vmx, topoext, ...
    pub threads: Vec<Thread>,     // per logical CPU: core id, package, kind
    pub iommu: Iommu,             // groups present, driver (AMD-Vi / DMAR)
}

pub enum CoreKind { Uniform, Performance, Efficient }

pub struct GpuInfo {
    pub address: PciAddress,
    pub vendor: u16, pub device: u16,
    pub subsystem: (u16, u16),
    pub family: Family,           // from the device tables below
    pub integrated: bool,
    pub host_driver: Option<String>,
    pub boot_vga: bool,
    pub drives_display: bool,
    pub iommu_group: Option<u32>,
    pub group_members: Vec<GroupMember>, // every other function in its group
    pub reset_methods: Vec<String>,      // sysfs reset_method
    pub link: Option<(String, u8)>,      // current_link_speed, width
    pub rebar: Option<u64>,              // largest BAR, for Resizable BAR
}
```

Sources: `/proc/cpuinfo` and `/sys/devices/system/cpu/cpu*/topology` for the
CPU; `/sys/devices/cpu_core/cpus` and `/sys/devices/cpu_atom/cpus` for Intel
hybrid core kinds; `/sys/bus/pci/devices/*` for the GPUs as `pci.rs` reads
them now, plus `reset_method`, `current_link_*` and `resource` sizes;
`/sys/kernel/iommu_groups` for the groups.

The service serves the profile as a protocol reply (`Request::Hardware`), the
app shows it on the setup screen and in GPU settings, and diagnostics write it
as `service/hardware.toml`. The profile is the only input to everything
below: a decision never reads sysfs on its own.

### GPU families

`Family` comes from per-vendor device ID tables in
`gse-host/src/hardware/families.rs`, not from ID ranges or names: ranges
break across generations and names come from `pci.ids`, which may be stale.
An ID missing from the tables gives `Family::Unknown { vendor }`, which is
never Tested.

- **NVIDIA**: generated from `supported-gpus.json`, which every NVIDIA driver
  release ships, by `scripts/dev/nvidia-families.py`. It records per device
  whether the open kernel module supports it (Turing and newer) and the
  newest driver branch that does (older cards end on a legacy branch). The
  table is regenerated when the guest's driver branch changes.
- **AMD**: hand-written: GCN 1-2 (Southern and Sea Islands), Polaris, Vega,
  Navi 1x, Navi 2x, Navi 3x, Navi 4x, and the APU families as integrated.
- **Intel**: hand-written: Alchemist (DG2, `56xx`), Battlemage (`e20x`), and
  every integrated generation as integrated.

## Support levels

Each GPU gets one level with a reason the app shows word for word, and the
CPU gets one too. The level of a sandbox is the lower of its GPU's and the
CPU's.

| Hardware | Level | Reason or trap |
| :--- | :--- | :--- |
| NVIDIA Turing and newer, discrete | Tested (RTX 3070) | - |
| NVIDIA Maxwell to Volta | Expected | needs the legacy proprietary branch, closed module |
| NVIDIA Kepler and older | Unsupported | no current proprietary branch; nouveau cannot reclock them |
| AMD Navi 2x, 3x, 4x, discrete | Expected | - |
| AMD Polaris, Vega, Navi 1x | Expected, with a warning | reset bug: the card may not come back to the host without `vendor-reset` or a reboot |
| AMD GCN 1-2 | Unsupported | `radeon` by default, `amdgpu` only with module options, no reset |
| Intel Arc Alchemist, Battlemage | Expected | needs Resizable BAR on the host for usable speed |
| Any integrated GPU (Intel, AMD APU) | Unsupported | cannot be handed over whole; GVT-g and SR-IOV are out of scope |
| Unknown device of a known vendor | Expected, with a warning | not in the tables |
| AMD CPU | Tested (Ryzen 7 3700X) | - |
| Intel CPU, uniform cores | Expected | - |
| Intel CPU, hybrid (12th gen and newer) | Expected | vCPUs go on P-cores only |

Unsupported never blocks a user who insists: the GPU stays selectable with
the reason under it, and setup does not refuse to install. It does block the
default: `default_gpu` picks only GPUs that are not Unsupported and not
integrated.

## Host side: a strategy per host driver

`plan.rs` stops matching on driver names inline. Each host driver gets a
`HostDriver` entry in `gse-host/src/drivers.rs`:

```rust
pub struct HostDriver {
    pub name: &'static str,
    pub modules_to_unload: &'static [&'static str], // in modprobe -r order
    pub modules_to_load: &'static [&'static str],
    pub services: &'static [&'static str],          // stop before, start after
    pub control_nodes: &'static [&'static str],     // extra /dev nodes holders hold
    pub unbind_consoles: bool,
    pub after_return: AfterReturn,                  // what proves it came back
}
```

| Host driver | Unload | Services | Control nodes | Notes |
| :--- | :--- | :--- | :--- | :--- |
| `nvidia` | `nvidia_drm nvidia_modeset nvidia_uvm nvidia` | `nvidia-powerd`, `nvidia-persistenced` | `/dev/nvidiactl`, `/dev/nvidia-uvm*`, `/dev/nvidia-modeset` | as today |
| `nouveau` | none, unbind only | none | none | as today on the development PC |
| `amdgpu` | none, unbind only | none | `/dev/kfd` (ROCm) | reset bug families need the warning; `/dev/kfd` holders block like render nodes |
| `i915` | none, unbind only | none | none | an Arc on `i915` |
| `xe` | none, unbind only | none | none | an Arc on `xe` |

Unloading a module is only needed when it cannot let go of one device while
it keeps others, which is NVIDIA's case. `amdgpu`, `i915` and `xe` unbind per
device, which also keeps an integrated GPU of the same driver working on the
host.

### Companion functions

A GPU's companions are every endpoint in its IOMMU group, not the functions
in its slot: an Arc card's HDMI audio is a separate device on another bus in
the same group, behind the card's own PCIe switch. Bridges in the group stay
on the host (`vfio-pci` does not need them). A group that holds an endpoint
that is not part of the card (a USB controller, a NIC) makes the GPU
Unsupported on that board, with the member named: handing it over would take
that device from the host, and leaving it behind fails VFIO.

### Reset

`reset_methods` decides what the plan can expect:

- An AMD family with the reset bug and no `device_specific` method (which
  `vendor-reset` provides) gets the warning at selection time and an extra
  `AfterReturn` check: if the card does not answer on its driver after
  detach, the app says that a reboot is needed, instead of a generic error.
- No reset method at all makes the GPU Unsupported.

### Single mode on other vendors

The single-GPU switch stays as it is, with the strategy filling in the
modules and services. The traps it already handles (KWin restarted by the
user's systemd, consoles on the framebuffer) are not NVIDIA-specific. What is
vendor-specific and unknown until tested: whether `amdgpu` and `xe` let go of
the boot framebuffer (`simpledrm` handover) on unbind, and how long they take
to come back. Both are logged with timings so the first run on such a machine
answers them.

## Guest side: driver packs

The base image is shared by every set, so it carries the union of what every
GPU named in the settings needs (the default in `/etc/gse/gse.toml` and each
sandbox's own). A pack that is installed but unused costs disk space and
nothing else.

| Pack | Installs | Needed for |
| :--- | :--- | :--- |
| `mesa` | Mesa GL and Vulkan (RADV, ANV, NVK), `linux-firmware` GPU parts, `mesa-va-drivers-freeworld` | always |
| `nvidia` | `akmod-nvidia` (open module), CUDA libraries, the Flatpak GL extension | NVIDIA Turing and newer with the proprietary choice |
| `nvidia-legacy` | the RPM Fusion legacy branch package, closed module | NVIDIA Maxwell to Volta with the proprietary choice |
| `intel-media` | `intel-media-driver` (VA-API) | any Intel GPU |

`mesa` covers `amdgpu`, `i915`, `xe` and `nouveau` in the guest: their kernel
modules are in `kernel-modules`, which the image installs already.

The service writes the pack list into the seed as `/gse/drivers`, one name a
line. `provision.sh` has one function per pack and fails the build on a name
it does not know or on a pack whose module does not end up built (the
`modinfo` check `nvidia` has today). The base fingerprint hashes the script
and the sorted pack list, so a change of GPU or of driver choice that needs a
pack the image lacks marks the base image out of date, and the app offers
the rebuild it offers today.

`nvidia` and `nvidia-legacy` cannot share an image (one kernel module name).
Settings that would need both are refused with the reason, which takes two
NVIDIA cards of different generations in one machine.

## The guest driver choice

Some GPUs can run on more than one guest driver. The choice is per sandbox,
in its settings right under the GPU, shown only when the GPU has more than
one option:

| GPU family | Options (default first) |
| :--- | :--- |
| NVIDIA Turing and newer | Proprietary (NVIDIA), Open (nouveau and NVK) |
| NVIDIA Maxwell to Volta | Proprietary (NVIDIA legacy), Open (nouveau) |
| Intel Alchemist | `i915`, `xe` |
| Intel Battlemage | `xe` only |
| AMD | `amdgpu` only |

The option list comes from the family tables, so a new case (an AMD family
that gains a second driver) is a table entry. It is stored in the sandbox's
`settings.toml` as `gpu_driver` (`nvidia`, `nouveau`, `i915`, `xe`,
`amdgpu`); a missing key means the default. The host driver plays no part:
whatever the host runs, the default is the proprietary driver for NVIDIA.

### Choosing at boot

One image boots with any of the installed drivers, chosen at each start:

- The service puts the choice in fw_cfg as `opt/gse/gpu-driver` (a few
  bytes, which fw_cfg reads cheaply) next to the items it passes now.
- The image blacklists every GPU kernel module from automatic loading
  (`/etc/modprobe.d/gse-gpu.conf`) and keeps them out of the initramfs
  (`omit_drivers` in dracut), so udev loads none of them on its own.
- `gse-gpu-driver.service` runs before the display manager, reads the item
  and loads the chosen module explicitly: `nvidia_drm` (which pulls in the
  rest), `nouveau`, `amdgpu`, `i915` or `xe`. Intel's choice also needs the
  device ID: `xe force_probe=<id>` for an Alchemist card, since `xe` does not
  take DG2 by default. With no item (a build VM, an older service) it loads
  nothing and the build goes on without a GPU, as now.
- The agent reports the module in use and the Vulkan device name in its
  hello, and the service logs both; a sandbox whose GPU came up on no driver
  says so in the app instead of showing a black screen.

## CPU

- `-cpu host` with `topoext=on` only on AMD; on Intel plain `-cpu host`.
- `-smp` threads come from the profile: `threads=2` only when the cores the
  sandbox gets have two threads each.
- On a hybrid Intel CPU the sandbox's vCPUs are limited to P-cores: QEMU's
  vCPU thread IDs come from QMP `query-cpus-fast` after start, and the
  service sets their affinity with `sched_setaffinity` (`rustix`, no unsafe
  code). The CPU count a user can pick is capped at the P-core threads.
- Setup's warnings name the vendor's own terms: "SVM" and "AMD-Vi" on AMD,
  "VT-x" and "VT-d" on Intel. On Intel with VT-d on but no IOMMU groups, the
  warning names the kernel argument `intel_iommu=on`.

## Desktop GPU of the same vendor

Two cases need the host desktop and the sandbox GPU apart while they share a
vendor: an Intel iGPU with an Arc card, and an AMD APU with a Radeon card.

- The app keeps itself off the sandbox GPU by leaving out a vendor's Vulkan
  driver (`gse-app/src/gpu.rs`), which cannot work here. It needs to stay off
  one device instead. The candidate: the app restarts itself in a mount
  namespace (bubblewrap, already on every Flatpak system) where `/dev/dri`
  holds only the desktop GPU's nodes. To be checked on the development PC
  with a forced same-vendor pair before it is relied on.
- KWin and kmscon open every GPU. On the development PC that is solved by
  hand (`KWIN_DRM_DEVICES`, a Vulkan loader settings file, kmscon's
  `gpus=primary`; see "Secondary mode prerequisites" in `docs/design.md`).
  With the profile, setup can write the same files for the desktop GPU it
  finds, as a separate setup step the user can see and undo.

## Testing without the hardware

- **Machine fixtures.** `crates/gse-host/tests/machines/<name>/` holds a
  trimmed sysfs and procfs tree (the files `hardware.rs` reads, nothing
  else) and an `expected.toml` with the profile, the support levels, the
  attach and detach plans, the driver packs, the guest driver options and
  the QEMU CPU arguments. One test walks every machine and compares.
- **Capturing a machine.** `scripts/target/capture-machine.sh` copies those
  files from a real machine into a tarball with no personal data (no
  hostnames, serials or user names); diagnostics include the same tarball,
  so every friend's archive becomes a fixture.
- **Machines to start with.** `target` (RTX 3070 single, Ryzen 7 3700X) and
  `dev-pc` (Arc A380 desktop, RTX 3070 on nouveau) are real and pin today's
  behaviour. Synthetic ones, marked `synthetic = true` until a real capture
  replaces them: an RX 6800 alone on an Intel hybrid CPU, an RX 580 (reset
  bug), an Arc A770 beside an Intel iGPU, a Radeon beside an AMD APU, a GTX
  1080 (legacy branch), and a board whose GPU group holds a USB controller.
- **Guest packs.** Each pack function in `provision.sh` ends with its own
  check (a module that `modinfo` finds, a Vulkan ICD file in place), so a
  build with a pack list nobody has run before fails at the pack, with its
  name, rather than at boot. A unit test checks the manifest the service
  writes for each fixture machine.

## Order of work

Each step leaves the RTX 3070 path unchanged, which the `target` and `dev-pc`
fixtures check.

1. Profile, families, fixtures, capture script and diagnostics. No behaviour
   changes; it starts collecting data from friends at once.
2. CPU: vendor flags, threads from the topology, vendor-specific setup
   warnings, P-core affinity.
3. Host strategies, companions by IOMMU group, reset handling, support levels
   in the app.
4. Driver packs, the seed manifest and the fingerprint.
5. The guest driver choice: settings, fw_cfg item, the guest unit, the
   agent's report.
6. Same-vendor desktop GPU.

## To learn on the first real machines

- Whether `amdgpu` and `xe` release a boot GPU cleanly in single mode, and
  how long they take to come back.
- Whether akmod-nvidia's kernel arguments (`rd.driver.blacklist=nouveau`,
  `modprobe.blacklist=nouveau`) stop an explicit `modprobe nouveau`; if they
  do, `provision.sh` removes them and the blacklist file takes their place.
- The exact RPM Fusion package name of the legacy NVIDIA branch for the
  Fedora release the guest is on.
- Whether an Arc card in the guest gets its full BAR when the host has
  Resizable BAR, and how slow it is without it.
- Whether RDNA 3 and 4 cards come back to the host after a sandbox without a
  reboot.
