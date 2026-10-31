# gamejail design

gamejail runs untrusted Windows and Linux games (pirated builds included) in a
hardened virtual machine that owns the whole GPU. Anything can be put into the
jail; nothing comes out except through one checked channel.

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

- **single**: the passed-through GPU drives the host desktop. Starting a jail
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
- **secondary**: another GPU drives the desktop. Starting a jail only rebinds
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

While a jail holds the GPU, host games started with PRIME offload to it fall
back silently to the desktop GPU (Mesa ignores a `DRI_PRIME` that matches no
device). The app shows that the GPU is in a jail.

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
- **Host and jail never run at the same time on screen.** Starting a jail ends
  the host graphical session, unbinds the NVIDIA driver, binds `vfio-pci` and
  boots the VM, which then owns the monitor. Shutting the jail down reverses
  this and brings the desktop back. A switch takes roughly 20-40 seconds.
- **Emergency exit hotkey** (for example `Ctrl+Alt+Pause`) is read by the host
  service from evdev, so the guest can neither fake nor block it. It kills the
  VM immediately.
- A suspended VM cannot be resumed after the GPU is reclaimed: consumer GeForce
  cards do not support saving VM state with a passed-through device.
- The jail VM has no emulated display and no network of its own by default:
  the passed-through GPU is its screen. QEMU starts as root for the unlimited
  memlock VFIO needs and drops to an unprivileged account once its devices are
  open. A stop request reaches the guest as an ACPI power-down; the GPU goes
  back to the host however QEMU ends. On the development PC a guest powers off
  in about 3 s and the GPU is back on its host driver 2.5 s later.
- Several jails can exist; only one runs at a time (a lock in the service).

## Network

- The guest's network card is backed by passt, a user-space network stack
  that turns the guest's traffic into ordinary sockets of the passt process.
  The host needs no bridge, no forwarding and no NAT, so nothing depends on
  firewalld, which is active on both machines and drops forwarded traffic.
- passt runs as the system account `gamejail-net`
  (`dist/sysusers.d/gamejail.conf`), one instance per jail, IPv4 only, with
  `--one-off` (it exits with its guest), `--no-map-gw` (the gateway does not
  lead to the host) and no inbound ports. The guest gets the host's own
  address from passt, so to the jail the host's address is itself.
- Host nftables (`table inet gamejail`, output hook) filter only that
  account's traffic: every address of this host (`fib daddr type local`),
  RFC1918, `100.64.0.0/10`, loopback, link-local, multicast, reserved ranges
  and their IPv6 counterparts are dropped; the rest of the internet is
  allowed. Replies on connections the host itself opened into passt (the
  development SSH forward) pass; the jail cannot open such a connection.
- DNS goes to public resolvers (1.1.1.1, 9.9.9.9), never to the host's
  resolver on loopback.
- The internet toggle is one drop rule, applied live
  (`gamejail-service net internet on|off`).
- passt's socket lives in `/tmp/gamejail-net`, a root directory with mode
  0700: SELinux lets `passt_t` create sockets in `tmp_t` but not in `/run`,
  and passt binds it as root before dropping privileges, without
  `dac_override`.
- Measured from inside the prototype jail: DNS and HTTPS work; the router,
  other LAN hosts and link-local addresses are unreachable; turning the
  internet off cuts HTTPS and DNS at once and turning it on restores them.

## Guest system

- Fedora built from a declarative recipe (kickstart or mkosi), lightweight
  Wayland session, `akmod-nvidia` from RPM Fusion.
- Two users:
  - `friend` owns the login session, the compositor, PipeWire and trusted apps
    such as Discord. Home is `0700`.
  - `jail` runs every launcher and game. It is an SELinux confined user
    (`user_u`): no `sudo`, `su` or SUID transitions, and no access to
    `friend`'s files through either Unix permissions or SELinux labels.
- Trusted apps run as native Wayland clients; games run through XWayland, so a
  game cannot capture a trusted window or its input.
- PipeWire belongs to `friend`. `jail` gets a separate socket that allows
  playback only, so a game cannot record the microphone or voice chat.
- Nobody inside the guest has `sudo`. A root exploit inside the guest still
  defeats the `friend`/`jail` split; the VM boundary is the hard one.

## Host background apps

Host GUI apps die with the compositor (measured: killing `kwin_wayland` closes
every windowed app, Qt and Electron alike; only windowless services survive).
Apps that should outlive a switch to the jail run under xpra with CPU
rendering, with GPU device nodes hidden (`flatpak override --nodevice=dri` or
bubblewrap), because any process holding `/dev/nvidia*` blocks the driver
unload. They are invisible while the jail runs and reattach when the desktop
returns. Push-to-talk does not reach them (the keyboard belongs to the VM);
voice activation does.

## Presets and reset

- A preset is a declarative list of what a jail contains: Fedora/RPM Fusion
  packages, Flathub apps, local files (AppImages), trusted apps for `friend`,
  and VM resources.
- Each preset is built into a read-only golden image in a temporary VM without
  a GPU, before any untrusted code runs. Building does not require switching
  away from the host desktop.
- A jail is a qcow2 overlay on its preset's golden image. **Reset deletes the
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
version. A preset installs it as a local-file entry, extracted at image build
time so the guest needs no FUSE, for user `jail`.

## File bridge

- Transport is **AF_VSOCK**, not TCP/IP: the guest has no IP route to the host,
  and vsock exposes exactly one port with no access to the host network stack.
- **The host always initiates**; the guest agent only answers.
- The agent runs as `jail` and only sees `~/Inbox` and `~/Outbox` of `jail`.
- Inbound: files queued in the app before start are pushed into `~/Inbox` after
  boot.
- Outbound: on a normal shutdown the host pulls `~/Outbox` before power-off.
  After an emergency exit, a disposable extractor VM (no network, no GPU, jail
  disk read-only) pulls `~/Outbox` from the stopped disk. The host never mounts
  a jail filesystem itself.
- Protocol: length-prefixed frames with hard caps on frame size, file count and
  total size, per-step timeouts, version handshake; `List`, `Pull`, `Push`
  with a SHA-256 per file. Paths are relative to `Inbox`/`Outbox` only.
- The host treats every byte from the agent as hostile, since malware can
  replace the agent in the overlay. Receiving runs in a separate process under
  bubblewrap and seccomp, without network, into a `noexec` quarantine.

### Checks on outbound files

Saves only travel jail -> vault -> new jail and are never opened on the host.
The checks exist so that a reset really cleans: malware dropped into a save
folder must not ride into the next jail.

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

1. **gamejail-service** (Rust, root, systemd): VM lifecycle, GPU driver
   switching, nftables, emergency hotkey, check that nothing holds the GPU
   before a switch, file bridge host side, image builds.
2. **gamejail** (Rust, gpui, AppImage): jail list, start/stop/reset, presets
   editor, file queue, internet toggle, background apps list, transfer reports.
   Talks to the service over a local socket guarded by polkit.
3. **Host background layer**: xpra, GPU-less launch wrappers, PipeWire with
   lingering.
4. **Guest image recipe** and **guest agent** (Rust, vsock).
5. **One-time installer**: service, polkit rule, IOMMU kernel parameters,
   QEMU/libvirt, OVMF, ClamAV. An AppImage alone cannot do this part.

## Open questions

- How the one-time installer gets QEMU, OVMF, ClamAV and the service onto
  Bazzite: layered packages, a system extension, or a container.

- A game library disk that survives reset (per jail, off by default) or full
  re-download after each reset.
- Where the Epic launcher stores its GE-Proton runtime, to pre-fetch it at
  image build time.
