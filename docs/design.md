# GUI Sandbox Environment (gse) design

gse runs untrusted Windows and Linux GUI applications, games and pirated
builds included, in a hardened virtual machine that owns the whole GPU. Anything can be put into the
environment; nothing comes out except through one checked channel.

## Target machine

- A single NVIDIA RTX 3070 drives the monitor. There is no integrated GPU.
- The host runs Bazzite, an image-based Fedora Atomic variant, with the
  proprietary NVIDIA driver. `/usr` is read-only and packages come from
  `rpm-ostree` layering or the image itself, which shapes the installer.
- Ryzen 7 3700X on a B450 board (PCIe 3.0), 32 GB RAM. AMD-Vi with interrupt
  remapping is on; the 3070 and its HDMI audio share IOMMU group 16 and nothing
  else. SVM is enabled in the firmware.
- The 3070 is the boot VGA device and drives one 1080p monitor over HDMI. The
  console runs on the `nvidia-drm` framebuffer; KWin, plasmashell, Xwayland and
  most desktop apps hold `/dev/nvidia*`, and `nvidia-powerd` is enabled.

The development PC differs: an Intel Arc A380 drives the monitor and the RTX
3070 has no display attached and runs on nouveau, alone in its IOMMU group
with its HDMI audio function. Everything except the single-GPU switch is
developed and tested there; the single-GPU switch is tested on the target
machine.

The service therefore has two GPU switch modes, chosen from the hardware:

- **single**: the passed-through GPU drives the host desktop. Starting an environment
  stops the display manager, ends the user's graphical sessions, unloads the
  host driver and binds `vfio-pci`; shutting it down reverses that. Stopping
  plasmalogin alone does not end a running Plasma session: KWin keeps the GPU,
  and a KWin that is killed instead comes straight back through its wrapper
  and the plasma user units. KWin and plasmashell are units of the user's
  systemd instance, so the switch stops `graphical-session.target` there and
  then ends the session with `loginctl terminate-session`; nothing has to be
  signalled after that. Measured on the target machine
  (`scripts/target/single-gpu-switch-test.sh`): the desktop is gone and the
  GPU free in about 2 s, unloading the proprietary driver and binding
  `vfio-pci` takes under a second, and reloading the driver (about 4.5 s)
  plus starting the display manager brings the login screen back about 6 s
  after the GPU is released.
- Clearing `driver_override` needs a write that reaches sysfs: a zero-byte
  write leaves the override in place, and the host driver then refuses the
  device.
- **secondary**: another GPU drives the desktop. Starting an environment only rebinds
  the passed-through GPU to `vfio-pci`; the desktop stays up.

### Secondary mode prerequisites

KWin opens every GPU, including one with no monitor, and so does kmscon. On
the development PC the passed-through GPU is kept away from both:

- `KWIN_DRM_DEVICES=/dev/dri/desktop-card` in a drop-in for
  `plasma-kwin_wayland.service`. `/dev/dri/desktop-card` is a udev symlink to
  the desktop GPU by PCI address (`scripts/dev/61-desktop-card.rules`): `cardN`
  numbers follow probe order, and by-path names contain `:`, KWin's list
  separator.
- A Vulkan loader settings file in `/etc/vulkan/loader_settings.d/`
  (`scripts/dev/vk_loader_settings.json`) that gives `/usr/bin/kwin_wayland`
  only the desktop GPU's driver. `kwin_wayland` has `cap_sys_nice`, so the
  loader ignores `VK_*` variables in it; without the file NVK opens the other
  GPU's primary and render nodes inside KWin.
- `gpus=primary` in `/etc/kmscon/kmscon.conf`.

While an environment holds the GPU, host games started with PRIME offload to it fall
back silently to the desktop GPU (Mesa ignores a `DRI_PRIME` that matches no
device). The app shows that the GPU is in an environment.

KWin still keeps one render-node descriptor of the other GPU, left by Mesa's
EGL probe and never used. Unbinding the GPU with that descriptor open is
harmless: the kernel marks the device unplugged and KWin carries on. Any other
holder may have buffers on the GPU, and unbinding the driver underneath live
buffers corrupts kernel memory: with nouveau, `nouveau_ttm_fini` warns about
leaked resources, the application then trips a list-corruption `BUG` the next
time it touches them, and the next unbind hangs until a reboot. Secondary mode
therefore tolerates exactly KWin with nothing but a render node open, and any
other holder (an editor or browser rendering on the GPU, a game) makes the
switch fail with its name until it is closed.

### Guest GPU driver

RPM Fusion `akmod-nvidia` builds the NVIDIA open kernel module for the guest
kernel; with it the guest sees the 3070 at PCIe 4.0 x16 with Vulkan.

## Isolation model

- **KVM/QEMU VM with VFIO passthrough of the whole 3070** (GPU and its HDMI
  audio function). Native performance; the guest has its own kernel.
- **On a single-GPU machine host and environment never run on screen at the same
  time.** Starting an environment ends the host graphical session, unbinds the NVIDIA
  driver, binds `vfio-pci` and boots the VM, which then owns the monitor.
  Shutting the environment down reverses this and brings the desktop back. A switch
  takes roughly 20-40 seconds. With a second GPU the environment is shown in a window
  instead (see "Environment screen").
- **Emergency exit hotkey** (for example `Ctrl+Alt+Pause`) is read by the host
  service from evdev, so the guest can neither fake nor block it. It kills the
  VM immediately.
- A suspended VM cannot be resumed after the GPU is reclaimed: consumer GeForce
  cards do not support saving VM state with a passed-through device.
- The environment VM has no emulated display and no network of its own by
  default: the passed-through GPU is its screen. QEMU starts as root for the
  unlimited memlock VFIO needs and drops to an unprivileged account once its
  devices are open.
- The host stops an environment by killing QEMU, never by asking the guest,
  which could ignore or delay a power-down request. The guest loses what it had
  not written yet, as on a power cut; a clean shutdown is started from inside.
  The GPU goes back to the host however QEMU ends; on the development PC it is
  on its host driver about 2.5 s after QEMU exits.
- Several environments can exist; only one runs at a time (a lock in the service).

## Environment screen

The app shows the running environment according to the switch mode:

- **secondary**: the environment's screen is shown in the app window next to the host
  desktop. A "Full screen" button spreads it over the monitor. Keyboard and
  mouse go to the environment while its view has focus.
- **single**: in place of "Full screen" the app has an "Enter" button. It
  performs the switch: the host desktop goes away and the environment owns the
  monitor. Shutting the environment down or the emergency hotkey brings the desktop
  back.

In secondary mode the passed-through GPU usually has no monitor, as on a
laptop whose discrete GPU has no outputs. The guest compositor still runs on
it, with a virtual output, so games and the desktop render on one GPU and the
VM needs no emulated display. Frames reach the host as raw pixels, the way
Looking Glass carries them:

- The host hands the guest the output mode (`--screen-mode`, by default
  `2560x1440@60`) as the QEMU `fw_cfg` item `opt/gse/screen`. Its presence
  makes the guest start labwc with `WLR_BACKENDS=headless,libinput` and set the
  virtual output to that mode; without it labwc drives the GPU's connectors.
- `gse-guest screen` runs as `sandbox` and copies frames with wlr-screencopy,
  which `guest` cannot reach. The compositor blits each frame on the GPU into a
  dmabuf the copier allocated with Vulkan in one of the GPU's own tiled
  layouts; a Vulkan transfer de-tiles it into host-visible memory, and one
  sequential copy puts it into a slot of the shared memory (QEMU
  `ivshmem-plain`, 128 MiB, three slots, layout in `crates/gse-screen`).
  Screencopy into shared memory directly makes labwc read the frame back
  itself, which on NVIDIA goes through a 24-bit format the driver converts on
  the CPU (over 100 ms for a 1440p frame); NVIDIA cannot render into a linear
  buffer. The guest maps the region once, write-combining (`resource2_wc`): a
  second, uncached mapping of the same BAR makes the whole region uncached.
- The host app maps the memory read-only and copies pixels out of it, checking
  every size the guest wrote against the slot before touching a pixel. The
  host runs no video decoder and parses no format of the guest's choosing,
  which keeps the surface a hostile guest can reach as small as raw pixels
  allow. The memory is `/dev/shm/gse-<name>.screen`, readable by the user who
  started the environment only.
- Input goes the other way: the app sends QEMU input events over a QMP
  socket given to that user (`/run/gse/<name>.input`), to a virtual keyboard,
  a tablet for the desktop pointer and a relative mouse. A capture key
  (Ctrl+Alt+G, present on compact keyboards too) locks the host pointer and
  sends relative motion, which games that lock the pointer need. The guest
  gets no other channel back to the host.
- Sound goes to the host user's sound server on a playback-only
  `virtio-sound` card, over the PulseAudio protocol with the socket named
  explicitly: QEMU opens it as root, and both its PipeWire backend and
  libpulse's runtime-directory lookup misbehave for a root client of another
  user's server. An emulated Intel HDA made the guest's PipeWire play at a
  fraction of the rate, with dropouts.
- Measured on the development PC: 2560x1440 at 60 frames per second (the
  virtual output's rate), about 5% of one guest CPU for the copier, and every
  frame shown by the development viewer (`crates/gse-viewer`).

Streaming compressed video (NVENC, as Moonlight does) would cost less
bandwidth, but it puts a video decoder on the host to parse data produced by
the untrusted guest, so it is not used.

Development runs can add an emulated `virtio-vga` with VNC (`--dev-vnc`) to
look into an environment without the app.

## Network

- The guest's network card is backed by passt, a user-space network stack
  that turns the guest's traffic into ordinary sockets of the passt process.
  The host needs no bridge, no forwarding and no NAT, so nothing depends on
  firewalld, which is active on both machines and drops forwarded traffic.
- passt runs as the system account `gse-net`
  (`dist/sysusers.d/gse.conf`), one instance per environment, IPv4 only, with
  `--one-off` (it exits with its guest), `--no-map-gw` (the gateway does not
  lead to the host) and no inbound ports. The guest gets the host's own
  address from passt, so to the environment the host's address is itself.
- Host nftables (`table inet gse`, output hook) filter only that
  account's traffic: every address of this host (`fib daddr type local`),
  RFC1918, `100.64.0.0/10`, loopback, link-local, multicast, reserved ranges
  and their IPv6 counterparts are dropped; the rest of the internet is
  allowed. Replies on connections the host itself opened into passt (the
  development SSH forward) pass; the environment cannot open such a connection.
- DNS goes to public resolvers (1.1.1.1, 9.9.9.9), never to the host's
  resolver on loopback.
- The internet toggle is one drop rule, applied live
  (`gse-service net internet on|off`).
- passt's socket lives in `/tmp/gse-net`, a root directory with mode
  0700: SELinux lets `passt_t` create sockets in `tmp_t` but not in `/run`,
  and passt binds it as root before dropping privileges, without
  `dac_override`.
- Measured from inside the prototype environment: DNS and HTTPS work; the router,
  other LAN hosts and link-local addresses are unreachable; turning the
  internet off cuts HTTPS and DNS at once and turning it on restores them.

## Guest system

- Fedora, built from the Fedora Cloud image by `guest/base/provision.sh`,
  which cloud-init runs once in a build VM without a GPU and with the environment
  network (`gse-service image build-base`). The seed is a FAT volume
  written by the service itself, so the host needs no mkfs or mtools. The
  guest reports its verdict on the serial console; a passing build is
  flattened into one compressed qcow2 with no backing file. On the
  development PC a build takes about 5 minutes, two of them for the driver.
- `akmod-nvidia` from RPM Fusion, built for the newest installed kernel since
  the build VM runs an older one; the build fails if the module is missing.
- LXQt on labwc (Wayland) with Xwayland for games. greetd starts it as
  `sandbox` without a password, on boot and after a logout.
- `kernel-modules` is installed for the guest kernel: cloud images leave it
  out, and it carries every sound driver.
- The guest writes `GSE-READY` to its serial console once the desktop is up;
  the service logs "environment is ready" when it sees the line.
- Release images have no SSH server and no account that can become root;
  development builds keep a `dev` account with an SSH key.
- Two users:
  - `sandbox` owns the login session, the compositor, PipeWire and trusted apps
    such as Discord. Home is `0700`.
  - `guest` runs every launcher and untrusted program. It is an SELinux
    confined user (`user_u`): no `sudo`, `su` or SUID transitions, and no
    access to `sandbox`'s files through either Unix permissions or SELinux
    labels.
- Programs of `guest` show their windows on the desktop of `sandbox` through
  `gse-guest` (`crates/gse-guest`):
  - `desktop`, autostarted as `sandbox`, opens a Wayland socket for `guest`
    through the compositor's security-context protocol. labwc withholds every
    privileged protocol from its clients: no screen capture, no clipboard
    monitoring, no virtual input, no list of other windows.
  - `guest` has its own X server (`xwayland-satellite` on `:1`, a client of that
    socket), so its X programs never share an X server with `sandbox`'s.
  - `launcher`, a service of the `guest` account's lingering systemd instance,
    starts what `sandbox` asks for (`gse-guest run <command>`) as transient
    units of that instance. Only the command line goes in; one line of status
    comes back.
  - Every connection between the two accounts is opened by `guest`, through
    sockets in `/run/gse/display` (owner `sandbox`, setgid group `guest`,
    label `user_tmp_t`). `sandbox` is `unconfined_u` with the full MCS range
    and `guest` is `user_u` at `s0`; SELinux cannot label a connection opened by
    the former to a socket the latter serves, and the connect fails with
    `EINVAL` without an audit record.
  - Measured in an environment booted from the base image: a program started as
    `guest` runs as `user_u:user_r:user_t:s0`, opens windows, gets no
    privileged protocol (`grim` and `wl-paste` fail), reaches `:1`, and cannot
    list `/home/sandbox`.
- PipeWire belongs to `sandbox`. `guest` reaches it only through a second
  PulseAudio socket of `sandbox`'s `pipewire-pulse`,
  `/run/gse/display/pulse` (`PULSE_SERVER` in the `guest` account's
  environment, configured in `/etc/pipewire/pipewire-pulse.conf.d/`). Its
  clients carry `pipewire.client.access = "restricted"`, which WirePlumber
  turns into read-only access, and a `pulse.rules` entry matching that
  property applies `block-record-stream`, `block-source-volume` and
  `block-sink-volume`. A game can play sound but cannot record the
  microphone or a monitor of what others play (voice chat), nor change any
  volume. Measured from a booted image: playback from `guest` reaches the
  desktop's sound server; recording from the default source and from a sink
  monitor, and setting a volume, fail with "Access denied"; `sandbox` keeps
  both.
- `guest` has no sound server of its own (its PipeWire user units are
  masked). One would hold no sound card, and Wine's PipeWire driver would
  pick it and play into nothing instead of using `PULSE_SERVER`.
- Flatpak and Steam's pressure-vessel sandbox their programs with bwrap, which
  `guest` runs in a user namespace of its own. The base image grants `user_t`
  what bwrap needs there and nothing that reaches past it: writing to a
  route netlink socket (loopback in its network namespace), opening sysctls
  for writing (`user.max_user_namespaces` of its namespace), and mounting on
  its own home directories. Each operation also needs a capability that
  `guest` holds only over namespaces it created; the host-wide sysctls stay
  root-owned and closed. `/dev/ntsync`, which Proton uses and the sandboxes
  bind in, gets a type of its own that `guest` may read, write and `ioctl`;
  the policy otherwise leaves it the generic `device_t`. System Flatpaks under
  `/var/lib/flatpak/{app,repo,runtime}` are labelled `bin_t`, since Fedora
  leaves them `var_lib_t`, which a confined user may not execute. Deployed
  files are hardlinks to repository objects, so they take no label from their
  directory; the preset build relabels after installing.
- Measured in an environment booted from the launchers preset: Steam (with its webhelper
  under pressure-vessel), Lutris, the Epic Games Launcher, Heroic, ProtonPlus
  and Faugus run as `user_t`; Vulkan sees the RTX 3070 both natively and inside
  Flatpak, GL renders on it, and `vkcube` presents through `:1`. With the
  compositor on the 3070, GTK 4 apps such as ProtonPlus and Faugus present
  their windows; on an emulated display they cannot, since that compositor
  cannot import the 3070's buffers.
- Nobody inside the guest has `sudo`. A root exploit inside the guest still
  defeats the `sandbox`/`guest` split; the VM boundary is the hard one.

## Host background apps

Host GUI apps die with the compositor (measured: killing `kwin_wayland` closes
every windowed app, Qt and Electron alike; only windowless services survive).
Apps that should outlive a switch to the environment run under xpra with CPU
rendering, with GPU device nodes hidden (`flatpak override --nodevice=dri` or
bubblewrap), because any process holding `/dev/nvidia*` blocks the driver
unload. They are invisible while the environment runs and reattach when the desktop
returns. Push-to-talk does not reach them (the keyboard belongs to the VM);
voice activation does.

## Presets and reset

- A preset is a TOML file listing applications (`presets/launchers.toml`,
  format in `crates/gse-host/src/preset.rs`). Each comes from exactly one
  source: Fedora/RPM Fusion packages (`dnf`, with a `command`), a Flathub ID
  (`flatpak`) or a local AppImage (`appimage`). Applications run as `guest`
  unless marked `trusted`, which makes them run as `sandbox`.
- Each preset is built into a read-only golden image on top of the base image
  in a temporary VM without a GPU, before any untrusted code runs
  (`gse-service image build-preset`). The base image keeps cloud-init
  with only the NoCloud datasource: an environment boots without a seed volume and
  cloud-init stays idle, a preset build attaches one carrying the generated
  script and the AppImages. Building does not require switching away from the
  host desktop.
- The generated script installs the packages and Flatpaks, the Flatpak NVIDIA
  GL extension matching the image's driver package (Flatpak would otherwise
  pick it by the loaded module, which the build VM lacks), and extracts each
  AppImage into `/opt/gse/apps/<name>` so the guest needs no FUSE. It
  then writes a menu entry for every application into `sandbox`'s
  `~/.local/share/applications`, overriding the package's own entry: `guest`
  applications start through `gse-guest run` and carry "(guest)" in their
  name, so clicking one never starts it as `sandbox`.
- An environment is a qcow2 overlay on its preset's golden image. **Reset deletes the
  overlay**: everything, including Discord login and downloaded games, is gone,
  and the preset's software is there again in seconds.
- A golden image ages; the app offers a rebuild with updates and reminds when an
  image is older than about two weeks.
- The base layer of every preset (NVIDIA driver, session, users, SELinux, audio
  split, network config) is fixed and not editable in the UI.

### Epic Games Launcher

Shipped as a linuxdeploy type-2 AppImage (Tauri, WebKitGTK 4.1, EOS SDK, own
libraries). Data lives in `~/.local/share/com.epicgames.eglv2` and
`~/.cache/com.epicgames.eglv2`. For Windows builds it downloads a pinned
GE-Proton from GitHub and checks its SHA-512; `EGL_PROTON_PIN` overrides the
version. A preset installs it as an `appimage` entry.

## File bridge

- Transport is **AF_VSOCK**, not TCP/IP: the guest has no IP route to the host,
  and vsock exposes exactly one port with no access to the host network stack.
- **The host always initiates**; the guest agent only answers.
- The agent runs as `guest` and only sees `~/Inbox` and `~/Outbox` of `guest`.
- Inbound: files queued in the app before start are pushed into `~/Inbox` after
  boot.
- Outbound: on a normal shutdown the host pulls `~/Outbox` before power-off.
  After an emergency exit, a disposable extractor VM (no network, no GPU, environment
  disk read-only) pulls `~/Outbox` from the stopped disk. The host never mounts
  an environment filesystem itself.
- Protocol: length-prefixed frames with hard caps on frame size, file count and
  total size, per-step timeouts, version handshake; `List`, `Pull`, `Push`
  with a SHA-256 per file. Paths are relative to `Inbox`/`Outbox` only.
- The host treats every byte from the agent as hostile, since malware can
  replace the agent in the overlay. Receiving runs in a separate process under
  bubblewrap and seccomp, without network, into a `noexec` quarantine.

### Checks on outbound files

Saves only travel environment -> vault -> new environment and are never opened on the host.
The checks exist so that a reset really cleans: malware dropped into a save
folder must not ride into the next environment.

1. Paths: no `..`, absolute paths, symlinks, hardlinks or device files.
2. Limits: size, file count, depth.
3. Type by content: executables (PE, ELF, scripts, `.lnk`, `.desktop`),
   macro documents and nested archives are rejected.
4. Per-game allowlist of paths and formats from the preset.
5. ClamAV with signatures refreshed before the scan.

Accepted files are stored as one archive per export in a vault outside `$HOME`,
mounted `noexec`, so file managers and indexers have nothing to parse. A report
lists what passed and what was rejected and why.

## Components

1. **gse-service** (Rust, root, systemd): VM lifecycle, GPU driver
   switching, nftables, emergency hotkey, check that nothing holds the GPU
   before a switch, file bridge host side, image builds.
2. **gse** (Rust, gpui, AppImage): environment list, start/stop/reset, the environment
   screen (a window with "Full screen", or "Enter" on one GPU), presets
   editor, file queue, internet toggle, background apps list, transfer reports.
   Talks to the service over a local socket guarded by polkit. Every long
   operation reports its progress to the app as it goes: an image build its
   provisioning steps (the `=== <step>` lines and the `GSE-BUILD` verdict on
   the build VM's serial console), an environment start the GPU switch, the
   boot and the `GSE-READY` line, so the app shows where the preparation is
   instead of a bare wait.
3. **Host background layer**: xpra, GPU-less launch wrappers, PipeWire with
   lingering.
4. **Guest image recipe** and **guest agent** (Rust, vsock).
5. **One-time installer**: service, polkit rule, IOMMU kernel parameters,
   QEMU/libvirt, OVMF, ClamAV. An AppImage alone cannot do this part.

## Open questions

- How the one-time installer gets QEMU, OVMF, ClamAV and the service onto
  Bazzite: layered packages, a system extension, or a container.

- A game library disk that survives reset (per environment, off by default) or full
  re-download after each reset.
- Where the Epic launcher stores its GE-Proton runtime, to pre-fetch it at
  image build time.
- The desktop greetd starts for `sandbox` runs in the display manager's domain
  (`xdm_t`), not `unconfined_t`: greetd's `pam_selinux` does not change the
  context of the session it starts. Programs that need the user's context,
  such as the screen copier, run under the user systemd of `sandbox` instead.
- Whether the screen path keeps up above 60 Hz and at 4K, and how the app
  draws frames on the host GPU.
