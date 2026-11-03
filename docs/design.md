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
  signalled after that. Both may find the session already ending once the
  display manager stopped, and refuse: the switch goes on, and whether the
  GPU is free decides. On Bazzite the user's systemd refused to stop the
  target and outlived the ended session, starting every killed KWin again,
  so the switch then stops the user's whole instance (`user@<uid>.service`)
  as well. The text consoles are unbound from the framebuffer
  first, since with `nvidia-drm.fbdev` they keep `nvidia_drm` in use, and
  bound again on the way back. Returning the GPU waits for the old sessions
  to end before the display manager starts, so a new login does not meet a
  session still letting go of the GPU. Measured on the target machine
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
- **Emergency exit**: both Ctrl keys pressed together. On a single-GPU machine
  QEMU grabs the host's keyboards and mice (`input-linux`), so nothing on
  the host sees them, and this pair makes it let them go once the keys are
  lifted. Only the keyboards carry `grab_all`: QEMU's toggle on such a
  device lets go of every device without it and skips the others, so the
  mice come back with the keyboard. QEMU says nothing when it lets go, so
  the service reads every device as well and gets nothing while they are
  grabbed: the first key, button or motion it sees means the user asked to
  leave. The guest then shows that the screen goes dark, the service takes
  the Outbox once more and stops the VM; before the desktop is up, or
  after two minutes at most, it stops it at once. The guest never sees the
  devices, so it can neither fake the exit nor block it.
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
  desktop. A "Full screen" button spreads it over the monitor; full screen
  hides the bar, and Ctrl+Alt+F leaves it. Keyboard and mouse go to the
  environment while its view has focus.
- **single**: the environment owns the monitor and the input. The guest gets
  no screen mode, so labwc drives the GPU's connectors; QEMU gets the host's
  keyboards and mice (see the emergency exit above) and no host sound card,
  since the host session is gone: sound plays through the GPU's own HDMI
  function. The app warns that the session closes as at a logout and goes
  with it. Shutting the environment down from its menu, or the emergency
  exit, brings the login screen back.

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
  (`gse-service net internet on|off`). Whether a sandbox reaches the
  internet is one of its settings: it starts that way, and a change to a
  running sandbox applies at once.
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
  network. The service builds this base image into `/var/lib/gse/base.qcow2`
  at the app's request (`build-base`) the way it builds a preset's image
  (see "Presets and reset"), under the name `base`, which no preset may
  take; `gse-service image build-base` builds one from a terminal. The seed
  is a FAT volume written by the service itself, so the host needs no mkfs
  or mtools. The guest reports its verdict on the serial console; a passing
  build is flattened into one compressed qcow2 with no backing file, so
  images built on an older base image keep working. On the
  development PC a build takes about 5 minutes, two of them for the driver.
- The service fetches the Fedora Cloud Base Generic image itself. The
  release is `fedora_release` in `/etc/gse/gse.toml`, by default the one
  `provision.sh` is made for (`RELEASE` in `crates/gse-service/src/fedora.rs`).
  The image's name, size and SHA-256 come from the release's CHECKSUM file,
  read with the listing of `releases/<N>/Cloud/x86_64/images/` from
  Fedora's own server (`dl.fedoraproject.org`) over HTTPS, which stands in
  for the file's signature; the image comes from a mirror through
  `download.fedoraproject.org` and counts only once it matches. The app
  sees the steps `download` and `download <percent>%`, the latter with the
  bytes received and in all, while it downloads.
  A verified image stays in `/var/lib/gse/cache` and serves the next build
  while it still matches.
- `akmod-nvidia` from RPM Fusion, built for the newest installed kernel since
  the build VM runs an older one; the build fails if the module is missing.
- Budgie on labwc (Wayland) with Xwayland for games: one panel at the
  bottom with the menu, the open windows, the tray and the clock
  (`/usr/share/budgie-desktop/layouts/gse.layout`), Papirus icons, a dark
  theme, and Nemo for the Inbox and Outbox. greetd starts it as `sandbox`
  without a password, on boot and after a logout.
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
- `gse-guest` (`crates/gse-guest`) is gse's program inside the guest. Every
  start hands the host's copy over on a read-only disk of its own (serial
  `gse-guest`), with its size and SHA-256 as the `fw_cfg` items
  `opt/gse/guest-size` and `opt/gse/guest-sha256`; `fw_cfg` suits only a
  few bytes, since the guest reads an item from its start for every page.
  `gse-guest-update.service` puts a copy that matches in place before the
  agent, the journal stream and the desktop start, so a new gse brings its guest side
  without a rebuilt image. Only a change to the provisioning script makes a
  base image out of date. Programs of `guest` show their windows on the
  desktop of `sandbox` through it:
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

- A preset is a TOML file listing applications (format in
  `crates/gse-host/src/preset.rs`). Each comes from exactly one
  source: Fedora/RPM Fusion packages (`dnf`, with a `command`), a Flathub ID
  (`flatpak`) or a local AppImage (`appimage`). Applications run as `guest`
  unless marked `trusted`, which makes them run as `sandbox`. A preset has
  a name, which its file, its image and the environments made from it go
  by, and a title, what people call it (up to 64 characters).
- The service keeps the presets in `/var/lib/gse/presets/<name>.toml` and
  puts the built-in presets there when the directory is empty: `launchers`
  (`presets/launchers.toml`, "Game launchers": the game launchers with a
  browser) and `desktop` (`presets/desktop.toml`, "Empty desktop": the
  desktop with only a browser). A file of a built-in preset's name without a
  title goes by the built-in title. The Presets part of the app's settings lists them
  with their applications, sources and trusted marks and whether an image
  built from each exists, and creates, edits, deletes and builds them; presets
  are edited nowhere else. The service checks
  every name, title, package, Flathub ID, desktop file and command it is
  sent before writing a preset, since they end up in a script that runs as
  root in the build VM.
- Each preset is built into a read-only golden image on top of the base image
  in a temporary VM without a GPU, before any untrusted code runs
  (`gse-service image build-preset`, or the service at the app's request).
  The service builds from `/var/lib/gse/base.qcow2` in `/var/lib/gse/builds`
  and moves the result to `/var/lib/gse/images/<preset>.qcow2` only once the
  build passes; it reports each provisioning step to the app as it reaches
  the console, can stop a build, and builds only while no environment runs.
  The service marks `/var/lib/gse/images`, `environments` and `builds`
  no-copy-on-write where the file system has the mark (btrfs), so the
  images and disks made there from then on do not fragment with every
  write, and runs the disks of a VM past the host's page cache
  (`cache=none`) when they carry the mark; direct I/O into a btrfs file with
  checksums can store a block whose checksum no longer matches.
  It tells the app when the build in progress started, and keeps the last
  failed build of each image, the base image's included, with the step it
  reached and its console in `/var/log/gse/builds`, as
  `/var/lib/gse/images/<image>.failed` until the image builds again; a
  build that was stopped did not fail.
  An overlay cannot outlive a change of the image under it, so a rebuilt
  image gives the environments created from it a new system; the app asks
  first. Their /home stays: each environment has its own `home.qcow2`
  (sparse, up to 512 GiB) beside its system overlay, which the guest finds
  by its serial `gse-home` and mounts over /home at boot
  (`gse-home.service`). A new one starts with the /home the system had.
  What the system itself puts in /home (shortcuts, autostart entries,
  `guest`'s units) is taken from the image at each boot: every image build
  ends by keeping /home as `/usr/share/gse/home-template`, which is copied
  over the disk, and the files an older template had and the new one
  dropped go away. Only a reset empties the home disk. The
  service reads a preset's AppImages as root, and only those the user who
  asked for the build could read. The base image keeps cloud-init
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
- An environment is a qcow2 overlay on its preset's golden image. It can be
  created before that image exists, while it builds or before it was ever
  built; it then waits, the app says why, and it starts once the image is
  there. The overlay is made at its first start. **Reset deletes the
  overlay**: everything, including Discord login and downloaded games, is gone,
  and the next start begins from the preset's image again.
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

## Guest agent and file bridge

- The host talks to an agent inside the environment over one
  **virtio-serial port** (`io.github.sovgut.gse.agent`), not TCP/IP: the
  guest has no IP route to the host, the port reaches nothing but the
  service, and it needs no host kernel module or address the way vsock
  does. QEMU serves the host end on a socket only root reaches
  (`/run/gse/<name>.agent`).
- **Files outside a run** (`crates/gse-service/src/mailbox.rs`): on a
  single GPU the app is gone while the environment owns the screen, so the
  service carries files itself. Sent in while the environment is stopped,
  files wait in its folder (`inbox-waiting/`) and go into the Inbox once it
  is up; the agent then shows them arriving on the guest's desktop (a
  window with a bar, the file and the time left, from `/run/gse/incoming`
  the agent writes) and a notification once they are in. A file that
  cannot go in stays waiting for the next start while the others go; the
  window then ends short of it, the notification says how many arrived,
  and the sandbox's journal says why the rest stayed. A send copies at
  the lowest best-effort I/O priority and nice 10, and waits for every
  32 MiB it wrote to be on disk, so it never leaves gigabytes for the
  kernel to flush at once; on btrfs such a flush stalls every program that
  touches the disk. `CancelTransfer` stops a send at its next part: whole
  files stay, the one under way goes. While a single-GPU environment runs, the service takes what is new
  in its Outbox every 5 s, past the same checks as a take, into a spool of
  the lines a take sends (`outbox-taken`); the app's Files panel lists and
  takes it after the run like a live Outbox. The guest shows each round
  (agent version 4): a window with a bar while files leave, then a
  notification of what left for the host and what stays, with why. A
  power-off from the guest's own menu waits up to a minute until the host
  took or turned down every file of the Outbox: the agent puts a unit in
  /run whose stop waits, so no image has to be rebuilt for it.
- **The host always initiates**; the agent only answers. The protocol is
  JSON lines (`gse_protocol::agent`): every call carries an id its answers
  repeat, lines are capped, a file travels in base64 chunks of 32 KiB with
  its size and SHA-256, and listings and files have hard limits. The
  host speaks to agents from version 1 on; `Incoming`, which announces the
  files kept for a start, goes only to version 2 and later, so
  environments made from an older image keep working without the window.
- The agent runs as **root** (`gse-agent.service`), since the port's device
  node belongs to root: a program of `guest` cannot open it and pose as the
  agent. It lists the processes of `guest` by the unit that started them
  (each menu entry starts a transient unit titled after the program, and a
  Flatpak moves its app into a scope of its own), with the program that
  started each one: the nearest other program up the line of parents of
  its first process, none when that line reaches `guest`'s systemd, which
  starts every menu entry. It asks one to quit or kills it (through a
  pidfd, after checking its owner), or kills a program with everything it
  started (through its cgroup).
- Files go into `~/Inbox` and come out of `~/Outbox` of `guest`. `guest` owns
  both folders and can swap anything in them at any moment, so every path
  below them is opened with `openat2` refusing symbolic links and leaving
  the folder, new files and folders are handed to `guest`, an existing name
  gets a number instead of being replaced, and a file only leaves when it
  is a regular file with a single link that `guest` owns.
- The sandbox's desktop has **Inbox** and **Outbox** shortcuts. `sandbox`
  cannot look into `guest`'s home, so they open a file manager as `guest`,
  where files of `guest`'s programs can be moved into the Outbox.
- Inbound: files and folders of the host, chosen in the app, which the
  service reads only when the user asking could read them; they arrive as
  they are, a folder with what it holds. They are not packed into an
  archive: unpacking one would be the agent's work, as root, on data from
  outside.
- Outbound: the Outbox comes out file by file past the checks below. The
  service hands a file to the app only once its start passed, and the app,
  as the user, puts what came whole into one zip archive in the received
  files folder (`~/GSE` unless app settings name another). The host never
  mounts an environment's filesystem.
- The host treats every byte from the agent as hostile, since malware can
  replace the agent in the overlay. Still to come: receiving in a separate
  process under bubblewrap and seccomp, ClamAV, per-game allowlists, and
  an extractor VM (no network, no GPU, the disk read-only) that empties the
  Outbox of an environment that was stopped in an emergency.

### Checks on outbound files

Outbound files are mostly saves on their way to the next environment. The
checks exist so that a reset really cleans: malware dropped into a save
folder must not ride into the next environment or onto the host.

1. Paths: no `..`, absolute paths, symlinks, hardlinks or device files.
2. Limits: 4 GiB a file, 8 GiB and 1000 files a take, 16 levels deep. A
   file sent in may be up to 64 GiB (4 GiB for an image older than agent
   version 2).
3. Type by content and name: programs (ELF, PE), scripts, Windows and
   desktop shortcuts, Office documents and installers that can carry
   macros, and archives are refused (`crates/gse-service/src/checks.rs`).
4. Antivirus: every file that passed the checks above waits in
   `/var/lib/gse/scan/<name>` while ClamAV scans the take, and one it finds
   infected is refused with what it found. The service keeps ClamAV itself
   in `/var/lib/gse/clamav`, installed nowhere else: once a day it asks
   GitHub for the latest Cisco Talos release, takes the Linux x86_64
   package only with a valid signature of the Talos key it pins
   (`dist/clamav/cisco-talos.asc`), unpacks it with `rpm2cpio` and `cpio`,
   and updates the signatures with the engine's own freshclam; neither
   waits for a gse release or touches the sandboxes' images. The scanner and
   freshclam run as `gse-scan` in transient units (system read-only, no
   privileges, the scan without network), their output in files the
   manager opens rather than pipes handed over D-Bus, which Bazzite's
   manager refuses; where no transient unit starts, they run as
   `gse-scan` directly. freshclam gets the system's bundle of trusted
   certificates through `CURL_CA_BUNDLE`, since the engine looks where the
   system it was built on keeps it. The last run's output stays readable
   in `/var/lib/gse/clamav/runs` for the app's diagnostics. A take fails, and a spool waits,
   while there is no engine or its signatures are older than a week, so
   nothing leaves unscanned. The updates run in the background with no one
   asking; About and updates shows the engine and the age of its
   signatures (`Antivirus`).
5. To come: a per-game allowlist of paths and formats from the preset.

What passes lands in one zip archive per take, which the app never opens;
the app shows what passed and what was refused and why.

## Components

1. **gse-service** (Rust, root, systemd): VM lifecycle, GPU driver
   switching, nftables, emergency hotkey, check that nothing holds the GPU
   before a switch, file bridge host side, image builds.
2. **gse** (Rust, gpui, AppImage): the app, described under "The app". It
   talks to the service over a local socket guarded by polkit.
3. **Host background layer**: xpra, GPU-less launch wrappers, PipeWire with
   lingering.
4. **Guest image recipe** and **guest agent** (Rust, virtio-serial).
5. **Setup from the app**: gse ships as one AppImage carrying the app and
   the `gse-service` and `gse-guest` programs; `gse-service` has the
   systemd unit, the polkit action, the sysusers entry and the default
   settings of `dist/` built in. No step needs a terminal:
   - On first start the app sees that the machine is not prepared and shows
     what will be installed and why. It copies the AppImage's
     `usr/libexec/gse` to a directory root can read, since root cannot read
     the AppImage's FUSE mount, and one polkit prompt runs
     `gse-service install` from there. Install:
     - installs the Fedora packages the service runs when they are missing
       (`qemu-system-x86-core`, `qemu-img`, `qemu-audio-pa`, `edk2-ovmf`,
       `passt`, `passt-selinux`, `nftables`): with dnf, or layered live
       with `rpm-ostree install --apply-live` on Fedora Atomic systems,
       which ask for a restart when that fails. Elsewhere it names what
       to install;
     - checks the machine: `/dev/kvm`, IOMMU groups, `vfio-pci`, QEMU,
       `qemu-img`, passt, nft and the UEFI firmware. What is missing is a
       warning, not a failure; install changes no firmware setting or
       kernel parameter;
     - puts both programs into `/usr/local/libexec/gse` (on Fedora Atomic
       systems such as Bazzite `/usr` is read-only and `/usr/local` is the
       writable `/var/usrlocal`) and gives them their SELinux labels;
     - writes the unit to `/etc/systemd/system/gse.service`, running the
       service from there, the polkit action to
       `/usr/share/polkit-1/actions` or, where `/usr` is read-only,
       `/etc/polkit-1/actions` (read by polkit 126 and later), the
       sysusers entry to `/etc/sysusers.d/gse.conf`, and
       `/etc/gse/gse.toml` unless there are settings already; drop-ins of
       the unit stay, except the development one (`dev.conf`), which would
       start another build. polkit rereads its actions when a policy file
       appears in a directory it watches, and finds a directory created
       after it started only seconds later, so install puts the action's
       file in place again until `pkaction` knows the action;
     - creates `gse-net` with `systemd-sysusers`;
     - enables the service and restarts it once any running environment
       has stopped, then waits until it answers with the protocol version
       and build of the program just installed.

     It reports on stdout, one line each: `step <id>` as a step begins,
     `warning <id> <text>` for what it found that does not stop it, and
     `done <id>` or `failed <id> <reason>` as the step ends. The ids are
     `packages`, `check`, `programs`, `files`, `account` and `service`; a failed step
     ends the install with a non-zero exit status. `--dry-run` reports
     the same steps and changes nothing.
   - The app then has the service build the base image (see "Guest
     system") and the image of the `launchers` preset; the `desktop` preset
     builds when a sandbox is first made from it.
   - A newer AppImage finds an older installed service through the
     protocol handshake and updates it the same way: install replaces the
     programs and gse's files, keeps the settings, and restarts the
     service. The app's own update does both in one go: it puts the new
     AppImage in place and starts it with `GSE_FINISH_UPDATE`, and that app
     updates the service as soon as it finds it older, which brings the
     guest's side too (every start hands the service's gse-guest over).
     Neither runs while a sandbox runs: the app asks to stop it first.
   - App settings, General, has "Remove everything": it runs
     `gse-service uninstall` through polkit, which stops any environment,
     the service, and removes its unit, the polkit action, `gse-net`,
     `/etc/gse`, `/var/lib/gse` (images, environments and the downloaded
     Fedora image), the nftables table, the state and sockets and the
     programs; the app then deletes its own settings and, if asked, the
     received files folder, and closes. Nothing of gse stays on the system
     but the AppImage file.
   The development machine runs the same unit with a drop-in
   (`scripts/dev/systemd/`) that starts a development build from
   `/usr/local/sbin`. `scripts/dev/install-dev-service.sh` sets that up once;
   after it, a new build gets there only through `gse-dev-install`, which
   asks for an administrator password (the app's "Install service build" in
   About and updates runs it). The service never takes a build out of the
   working tree by itself: anything running as the developer can write
   there.

## The app

The app calls environments sandboxes and presets "sets of apps". It is
built on GPUI Kit (`gpui-kit`) with the patched `gpui-pre`, `gpui-pre-linux`
and `gpui-component` in `vendor/`. It draws its own title bar like a KDE
window: round corners, the Breeze window buttons in the order of
`ButtonsOnRight` in `~/.config/kwinrc` (in full screen only leaving full
screen and close), a double click to maximize and a right click for the
window menu. A light and a dark palette lie over the
Kit's theme (`ui::Theme::apply`), following the system unless settings pick
one. One window:

- **First start**: while the service socket or the base image is missing,
  the window shows only "Getting this computer ready". "Prepare this
  computer" copies `gse-service` and `gse-guest` out of the AppImage to a
  temporary folder, runs `pkexec <that folder>/gse-service install` and
  follows its `step`/`warning`/`done`/`failed` lines, then asks the service
  to build the base image (`BuildBase`) and the `launchers` set. The step
  in progress says what it does now: the install's step, the download of
  Fedora with the gigabytes done of all (its percentage while the service
  reports no sizes) and the time left at its pace so far, then the build's
  step, the last two over a bar of how far they got. "Show
  details" opens the latest lines of the step's log under the steps: the
  install's lines, or the build's steps. Once the computer is checked, its
  step names the graphics card a sandbox gets and whether virtualization
  is on. A failure says why and offers "Try again" from the step it stopped
  at.
- **Welcome tour**, after preparing, until it is skipped or done (kept in
  `~/.config/gse/app.toml`, restarted from settings): what a sandbox is;
  making the first sandbox from a set; then, on its page, the window dims
  around Play, which gets a ring, and a box under it says what Play does,
  with "Play now", which plays like Play. That step waits while Play
  cannot be pressed, as while the sandbox's set builds. "Skip the tour"
  ends it on any step.
- **Left**: the sidebar, 272 px wide: a search over the sandboxes' names
  and sets with a button beside it that steps the order through newest
  first, by name and by set (names with numbers in natural order); each
  sandbox with its small cover and its state under its name (running, a
  start's stage, stopping, "Preparing" with a percentage while its set or
  the sandbox system builds, when it last played or that it never has);
  "New sandbox" and the gear of settings. A cover shows the sandbox's
  colours once it has played, a plain tile while its apps are missing
  (with rays while they are being prepared) and a tint before its first
  play. The sidebar collapses to a 64 px rail of covers, with a dot on the
  running one, by itself while a sandbox's screen is open and by its own
  button anywhere.
- **Right**, for the selected sandbox: its cover with its set by its
  title, the set's first apps and its name, and what can be done now:
  Play; Show screen and Stop while it runs; the start's steps as a line
  (prepare, hand over the graphics card, boot, desktop) with Cancel; the
  build of its set as a line whenever its set builds, a rebuild included,
  since nothing starts then, with a note under the cover that the build
  holds every other sandbox too; "Build its apps" when nothing builds them.
  A sandbox never played has a plain cover with its set's title alone over
  its name. Play is greyed out while any image builds, another sandbox's
  set included, and while the service updates or restarts; while files are
  sent into a sandbox its Play gives way to the send's progress and Cancel,
  since with one GPU nothing would say whether they arrived.
  Under it the facts: internet as a switch that applies at once, graphics
  (leaving the host while it starts), screen, and the way into its
  settings. Under them the Activity, Programs and Files tabs. A tab's list
  scrolled away from its top shrinks the cover to a strip, which grows
  back at the top.
- **Play** while another sandbox runs asks to stop that one first. On a GPU
  that drives the monitor it says the desktop goes away and how to come
  back, unless told not to again.
- **Activity** is the journal. Its filter is a regular expression over each
  line with its date and time, and `@` words keep a time or a span of it
  (`@14:05`, `@14:00..14:30`, `@2026-11-01`); a menu beside it keeps the
  last hour, today or this week. The line under the filter says how many
  lines it shows and what its `@` words and the rest do, or what is wrong
  with them. What the expression finds is marked in each line. A click on
  a line copies it with its full date and time; a button copies every line
  shown.
- **Programs**, while it runs, lists the programs of `guest` by the menu
  entry that started them, or "started by" the program that did, with each
  process's memory and CPU time, refreshed every few seconds, with a
  search. A process can be asked to quit or killed, and a program killed
  with everything it started.
- **Files**, while it runs, is a panel at the right: "Take N files out"
  puts what of the Outbox passes the checks into a zip archive in the
  received files folder, and the panel names each sandbox's last archive
  with a way to its folder. The list marks the files whose name keeps them
  inside and N leaves them out; the checks of how a file starts show only
  in the take. "Choose files", "Choose a folder" or files dropped on the
  panel go into the Inbox (see "Guest agent and file bridge"). A send or a
  take under way shows how many bytes of how many went, the time left and a
  bar (a send from the service's `Sending` reports every quarter second, a
  take from the chunks it receives against the Outbox's listed sizes), with
  Cancel: a take writes nothing, a send keeps the files that went in whole.
- **The screen** of a running sandbox replaces the page: a bar with a way
  back to the sandbox's page ("Show screen" there returns), its name, how
  long it has run since its desktop came up, the mouse hint, the internet
  switch, Files (a panel at the right),
  full screen and Stop; the screen; and its programs as cards beside it,
  where × asks a program to quit and, held, kills it with everything it
  started.
- **New sandbox** opens a dialog with a set already picked (one that is
  built first): pick another (each with its apps and whether it is built;
  a long list scrolls), name the sandbox (a name is suggested), or go to
  the sets editor with "Make your own set of apps". With no set at all it
  opens the editor straight away.
- **Sandbox settings** is a panel at the right with that sandbox's own
  settings as rows that save as they change: its name, renamed in place
  while it is stopped; the graphics card, screen size and frame rate as
  menus (on a GPU that drives the monitor, the monitor's own mode; the card
  menu checks the current card, marks the one that drives the monitor and
  adds the PCI slot to cards of one model, with no "None", since a sandbox
  always has a card); and the internet as a switch. The GPU and the screen
  apply from the next start, so they wait while the sandbox runs; the
  internet applies at once. Reset and Delete sit in a danger zone below,
  each asking a second time, and wait until the sandbox stops.
- **Settings** replace the sidebar with their own menu:
  - General: the look (system, light or dark), the received files folder
    and the welcome tour, kept per user in `~/.config/gse/app.toml`, and
    "Remove everything", which asks in a dialog that says what goes and how
    much it takes, offers to keep the taken files, and waits for the word
    `remove`.
  - Sets of apps: each set as a row with its apps, how many sandboxes use
    it (they wait while it is not built), its state and Build, Rebuild or
    Stop, its build's steps as a line while it builds, with the step it is
    at and the time it has taken since the service started it; and the
    sandbox system every set is built on, with Rebuild (`BuildBase`). Sets
    go by their titles everywhere the app names them ("Game launchers",
    "Empty desktop"), a set without one by its name. A failed build, as
    the service keeps it, marks its set with the step it stopped at,
    "Show log" (the build's console in `/var/log/gse/builds`, when the
    build got that far) and "Try again", until it builds again. While the
    service is older than the app, the page offers to
    update it (an AppImage) or restart it (a development build) in place
    of the sets. Edit opens a page with the set's apps (each run
    as `guest` or `sandbox`) beside where apps come from: a Flathub search
    through Flathub's API, a Fedora package, or an AppImage file. "Save and
    build" saves and builds at once. The page's name field holds the set's
    title, which a save changes; the set's name stays what it was made
    with: a new set takes the name of the recommended set it starts from
    while that is free, otherwise one made from its title (`wow-addons` for
    "WoW addons"). No two sets share a title. The page deletes a set no
    sandbox is made from, and the service refuses to delete one a sandbox
    is made from; its image goes with it. At its start the service removes
    images that no set names and no sandbox stands on.
  - New sandboxes: the set the new sandbox dialog picks first (kept by the
    app) and the settings new sandboxes start with (`/etc/gse/gse.toml`),
    as the same rows as a sandbox's.
  - About and updates: a newer release with its notes when there is one,
    in place of the notice over other pages; the version, whether the app
    checks by itself once a day (only the AppImage looks), the service's
    version as its handshake names it and how it compares with the app's,
    diagnostics, and when the app last checked and whether it is up to
    date. Each sandbox keeps a
  copy of the start settings of its own in
  `/var/lib/gse/environments/<name>/settings.toml`. The app keeps its own
  Vulkan drawing off every GPU these settings name and picks those GPUs
  when it starts, so a new one needs an app restart, which the app offers.
- Banners above the page stand for what needs doing or waiting for: a
  newer release (Later puts it off until the app starts again), a build,
  what is out of date, a service that does not match the app, an app
  restart. Results and errors are toasts at the bottom right: a result
  goes by itself after a few seconds, and one that wrote a file names it
  with "Show in folder"; an error waits for its ×, and one about a sandbox
  offers "Open activity", its journal.
- Every long operation reports its progress as it goes: an image build
  its provisioning steps (the `=== <step>` lines and the `GSE-BUILD`
  verdict on the build VM's serial console), a start the GPU switch, the
  boot and the `GSE-READY` line, so the app shows where it is instead of a
  bare wait.
- The **journal** of a sandbox is kept by the service in
  `/var/lib/gse/environments/<name>/journal`: its creation, starts with
  their settings, each stage, settings and internet changes, stops and
  why, resets and new images. Lines from the guest agent will be marked
  as the guest's, since malware can write them.

### Previews

`GSE_PREVIEW=<state> gse` shows the app in one state on a made-up machine:
the sandboxes, sets, journal, programs and files of the design's boards
(`crates/gse-app/src/preview/`; `GSE_PREVIEW=list` names the states, one per
widget variant). `GSE_PREVIEW_LOOK=light` shows it in the light look; dark is
the default, as on the boards. Every run of a state looks the same:

- Nothing reaches the service. A button that would send a request says
  "Preview: ... is not sent" instead, and other work that leaves the app
  (an update, a polkit prompt, opening a folder) says "is not done".
- Nothing is read from or written to the machine. What the app would learn
  from it (an AppImage, the service it carries, the development helper,
  the GPUs it started for, the sizes Remove everything measures, Flathub
  search results) is made up, and a state switches the variants it needs.
- The clock stands at 1 November 2026, 19:30, and animations stand still.

`scripts/dev/preview-shots.sh` screenshots states in a hidden nested KWin
at an exact window size, `scripts/dev/preview-session.sh` drives one state
with clicks and keys, and `scripts/dev/render-boards.sh` renders the boards
for side-by-side comparison (`.working/design/README.md`).

### What outlives the window

The service runs sandboxes and builds, not the app, so closing the window
stops neither.

- Closing the window while a sandbox runs asks: stop it and close, close
  and leave it running, or don't close. A kept sandbox goes on holding its GPU, sound and
  network; the app shows it as running when it opens again.
- An image build goes on without the window. The app shows its steps so
  far and the time it has taken when it opens again, from the service's
  record of the build, and a build that failed meanwhile, which the
  service keeps until the image builds again.
- When the service is older than the app (its protocol, or its release on
  one protocol), an AppImage offers to update it: it installs the service
  it carries, through the same password prompt as the first start, and the
  install restarts the service. A development build offers a restart
  instead. Either stops a running sandbox and an image build first. A build the service
  did not live to finish leaves nothing behind: the service clears
  `/var/lib/gse/builds` and an unfinished download in
  `/var/lib/gse/cache` when it starts, and the preset is shown as not
  built.
- When the service is newer than the app, the app says it is out of date;
  an AppImage looks for the current release there and installs it like any
  update.
- A host shutdown stops the service, which kills QEMU like any stop.
- Still to come: stopping a user's sandbox when their session ends (its
  sound goes to that session), and releasing keys the guest still holds
  pressed when the app goes away without letting them go.

## Logs and diagnostics

Everything gse does is written down in plain files a user can read without
the service or the app, so a failure of either still leaves a record:

| What | Where |
| :--- | :--- |
| The app: start, requests and their outcomes, events, first-start steps, updates, the screen, and a crash with its backtrace | `~/.cache/gse/app.log`, the previous start's in `app.previous.log` |
| The service install: what it ran and what dnf or rpm-ostree printed | `~/.cache/gse/install.log` |
| The service, each line synced to disk so a switch that freezes the machine still leaves its last steps | `/var/log/gse/service.log` and `service.previous.log`, besides the systemd journal |
| Each image build: the guest's console, ok or failed | `/var/log/gse/builds/<time>-<image>-<ok or failed>.log`, the last 12 |
| Each sandbox: its activity, its guest console, and the guest's journal streamed out as it is written (`gse-journal.service` on the `io.github.sovgut.gse.journal` port) | `/var/lib/gse/environments/<name>/`: `journal`, `console.log`, `guest.log`, and the previous run's `*.previous.log`; the guest logs belong to the sandbox's owner alone |

QEMU appends to a running sandbox's logs; past 64 MiB one starts over, since
the guest writes them and a hostile one could fill the disk. The host keeps
what the guest's journal says and never parses it.

"Save diagnostics" (About and updates, and a stopped first start) puts all
of it, the settings and what the computer is (system, kernel, packages,
GPUs and drivers, IOMMU, the service's journal) into one zip archive in the
received files folder. `gse --diagnostics`, or the AppImage with that flag,
makes the same archive from a terminal when the window does not open.

## Releases and updates

- gse supports two Fedora releases at once, the current one and the one
  before (now 44 and 45), on the host and in the guest: users stay on a
  release for months, and Fedora Atomic systems such as Bazzite follow the
  older one. Where their tools differ, the service asks the tool what it
  has rather than assuming one release: QEMU 11 has `-object monitor-qmp`,
  QEMU 10 only `-mon`, and the service picks by `qemu-system-x86_64 -object
  help`. `scripts/dev/qemu-compat.sh` runs the exact passt and QEMU command
  lines the service builds (`crates/gse-host/examples/qemu_lines.rs`) and
  its nftables ruleset on each release's tools in a container; run it
  before a release that changes how QEMU, passt or nftables are called.
- A release is a GitHub release of the project, made by the workflow
  `.github/workflows/release.yml` when a tag `v<version>` matching the
  workspace version is pushed. It builds the AppImage
  (`scripts/release/build-appimage.sh`: the app, the service, the guest
  binary the service puts into images, and the files setup installs) on
  Ubuntu 24.04, so it runs on any newer glibc.
- The workflow signs the AppImage with an ed25519 key, the secret
  `GSE_SIGNING_KEY`, and attaches `gse-x86_64.AppImage` and its raw
  signature `gse-x86_64.AppImage.sig`. The private key lives only there and
  with its owner; the app holds the public half.
- While the app runs as an AppImage it looks for a newer release when it
  starts and then once a day, and says so in a banner above the page,
  which Later puts off until the app starts again. Install (on About and
  updates, "Install and restart") downloads it, checks the signature,
  replaces the AppImage file
  and starts the new one; an update that does not check out is never
  installed, since the AppImage carries the service that runs as root. A
  running sandbox keeps running. About and updates in app settings shows
  the version and looks on demand.
- Still to come: the new AppImage updating the installed service it
  carries, through the install it runs on first start.

## Open questions

- How setup gets QEMU, OVMF, passt and ClamAV onto Bazzite: inside the
  AppImage, as layered packages (which need a reboot), or a container.

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
