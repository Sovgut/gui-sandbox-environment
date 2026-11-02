#!/bin/bash
# Builds the base layer every environment image shares, inside a fresh Fedora Cloud
# guest that cloud-init boots once: the NVIDIA driver, a light Wayland desktop
# that logs `sandbox` in on its own, the confined `guest` account for untrusted programs,
# PipeWire and Flatpak. The build VM has no GPU, so the driver module is built
# for the newest installed kernel rather than the running one.
#
# The last line on the serial console is the verdict the host reads:
# "GSE-BUILD: OK" or "GSE-BUILD: FAILED <line>". The guest powers off
# either way.
#
# GSE_DEV=1 keeps SSH and the `dev` account cloud-init created; without it
# the image has no SSH server and no account that can become root.

set -euo pipefail

verdict() {
    echo "GSE-BUILD: $*" > /dev/ttyS0
    systemctl --no-block poweroff
}
trap 'verdict "FAILED $LINENO"' ERR

step() { echo "=== $*" > /dev/ttyS0; }

step "update"
dnf -y upgrade --refresh

step "RPM Fusion"
fedora="$(rpm -E %fedora)"
dnf -y install \
    "https://mirrors.rpmfusion.org/free/fedora/rpmfusion-free-release-$fedora.noarch.rpm" \
    "https://mirrors.rpmfusion.org/nonfree/fedora/rpmfusion-nonfree-release-$fedora.noarch.rpm"

step "NVIDIA driver"
# Cloud images carry kernel-core without the `kernel` meta package, so the
# installed kernels are read from their module directories.
kernel="$(find /lib/modules -mindepth 1 -maxdepth 1 -printf '%f\n' | sort -V | tail -1)"
# kernel-modules carries the sound drivers (HDMI audio of the GPU, the sound
# card the host provides in window mode); cloud images leave it out.
dnf -y install akmod-nvidia xorg-x11-drv-nvidia-cuda "kernel-devel-$kernel" \
    "kernel-modules-$kernel"
akmods --force --kernels "$kernel"
# A missing module would only show at the first environment boot as a black screen.
modinfo -k "$kernel" nvidia > /dev/null

step "desktop"
# Budgie on labwc: a desktop with a panel at the bottom, a start menu and
# open windows beside it, laid out like Windows. labwc is also what keeps
# `guest`'s programs apart (wp_security_context_v1). Nemo shows the Inbox
# and Outbox.
dnf -y install \
    budgie-desktop budgie-desktop-defaults budgie-desktop-view budgie-backgrounds \
    python3-systemd papirus-icon-theme nemo gnome-text-editor xdg-desktop-portal-gtk \
    labwc xdg-desktop-portal-wlr \
    xorg-x11-server-Xwayland xwayland-satellite xrandr greetd greetd-selinux wlr-randr \
    pipewire wireplumber pipewire-pulseaudio \
    flatpak policycoreutils-python-utils google-noto-sans-fonts

# One panel at the bottom: the menu and the open windows at the left, the
# tray, sound and clock at the right. An applet's ID is the Name in its
# .plugin file; the panel skips an applet it does not know.
install -d /usr/share/budgie-desktop/layouts
cat > /usr/share/budgie-desktop/layouts/gse.layout <<'EOF'
[Panels]
Panels=Taskbar

[Taskbar]
Position=Bottom
Size=44
Spacing=4
Autohide=never
Dock=false
Shadow=true
Theme=true
Transparency=none
Children=Menu;Tasks;Tray;Status;Clock;Notifications

[Menu]
ID=Budgie Menu
Alignment=start

[Tasks]
ID=Icon Task List
Alignment=start

[Tray]
ID=System Tray
Alignment=end

[Status]
ID=Status Indicator
Alignment=end

[Clock]
ID=Clock
Alignment=end

[Notifications]
ID=Notifications
Alignment=end
EOF
cat > /usr/share/glib-2.0/schemas/30_gse.gschema.override <<'EOF'
[com.solus-project.budgie-panel]
layout='gse'

[com.solus-project.icon-tasklist]
pinned-launchers=['com.google.Chrome.desktop', 'org.mozilla.firefox.desktop', 'nemo.desktop']

[org.gnome.desktop.interface:Budgie]
color-scheme='prefer-dark'
icon-theme='Papirus-Dark'
font-name='Noto Sans 10'

[org.gnome.desktop.background:Budgie]
picture-uri='file:///usr/share/backgrounds/budgie/high-trestle-trail.jpg'
picture-uri-dark='file:///usr/share/backgrounds/budgie/high-trestle-trail.jpg'

[org.buddiesofbudgie.budgie-desktop-view]
click-policy='double'
EOF
glib-compile-schemas /usr/share/glib-2.0/schemas

step "accounts"
# `sandbox` owns the desktop and trusted apps; `guest` runs every launcher and
# untrusted program as an SELinux confined user, which forbids su, sudo and SUID
# transitions. Neither has a password: the desktop starts without a login.
useradd --create-home --comment "Desktop owner" sandbox
useradd --create-home --comment "Untrusted programs" guest
semanage login -a -s user_u guest
chmod 0700 /home/sandbox /home/guest
# Everything below creates files under ~sandbox/.config as root, so the
# directories come first and belong to `sandbox`: the panel, Nemo and
# dconf write there, and the session copies labwc's config there before
# labwc starts. Budgie's dark window frames are its themerc-dark, which
# its settings apply as this override; Pocillo has no labwc theme.
labwc_config=/home/sandbox/.config/budgie-desktop/labwc
install -d -o sandbox -g sandbox /home/sandbox/.config \
    /home/sandbox/.config/budgie-desktop "$labwc_config"
ln -s /usr/share/budgie-desktop/labwc/themerc-dark "$labwc_config/themerc-override"
chown -h sandbox:sandbox "$labwc_config/themerc-override"

step "login"
# The host tells the guest how its screen is shown through QEMU's fw_cfg:
# opt/gse/screen holds a mode such as 2560x1440@60 when the screen goes to
# the host through shared memory, and is absent when the GPU drives a monitor
# of its own. In the first case labwc runs with a virtual output on the GPU
# instead of the GPU's connectors. The fw_cfg file is readable by root only,
# so a service copies the mode to /run/gse/screen-mode before the login.
# opt/gse/build marks an image build, which has no GPU: the service then
# leaves /run/gse/build, and greetd starts no desktop.
cat > /etc/systemd/system/gse-screen-mode.service <<'EOF'
[Unit]
Description=Screen mode the host chose for this environment
After=systemd-modules-load.service
Before=greetd.service

[Service]
Type=oneshot
ExecStartPre=-/usr/sbin/modprobe qemu_fw_cfg
ExecStart=/bin/sh -c 'install -d /run/gse; f=/sys/firmware/qemu_fw_cfg/by_name/opt/gse/screen/raw; rm -f /run/gse/screen-mode; [ -r $f ] && install -m 0644 $f /run/gse/screen-mode; true'
ExecStart=/bin/sh -c 'rm -f /run/gse/build; [ -r /sys/firmware/qemu_fw_cfg/by_name/opt/gse/build/raw ] && touch /run/gse/build; true'
ExecStart=/usr/libexec/gse/keyboard

[Install]
WantedBy=multi-user.target
EOF
systemctl enable gse-screen-mode.service
# The host hands over its user's keyboard layouts at each start (fw_cfg
# opt/gse/keyboard: `layouts=us,ru`, `options=grp:win_space_toggle`). They
# become the desktop's system-wide settings, locked, so the panel and
# labwc (through Budgie's bridge) use them whatever was set inside.
install -d /usr/libexec/gse /etc/dconf/profile /etc/dconf/db/local.d/locks
printf 'user-db:user\nsystem-db:local\n' > /etc/dconf/profile/user
cat > /usr/libexec/gse/keyboard <<'EOF'
#!/bin/sh
raw=/sys/firmware/qemu_fw_cfg/by_name/opt/gse/keyboard/raw
db=/etc/dconf/db/local.d
rm -f "$db/00-gse-keyboard" "$db/locks/gse-keyboard"
field() { [ -r "$raw" ] && sed -n "s/^$1=//p" "$raw" | head -1; }
layouts=$(field layouts)
options=$(field options)
# XKB names only; anything else is left out rather than written into dconf.
case "$layouts$options" in *[!a-z0-9_:,\(\)+-]*) layouts= ;; esac
if [ -n "$layouts" ]; then
    sources=$(echo "$layouts" | tr ',' '\n' | sed 's/(\(.*\))/+\1/' \
        | sed "s/.*/('xkb', '&')/" | paste -sd, - | sed 's/),(/), (/g')
    xkb=$(echo "$options" | tr ',' '\n' | sed '/^$/d' | sed "s/.*/'&'/" \
        | paste -sd, - | sed 's/,/, /g')
    cat > "$db/00-gse-keyboard" <<KEYS
[org/gnome/desktop/input-sources]
sources=[$sources]
xkb-options=[$xkb]
KEYS
    printf '%s\n' /org/gnome/desktop/input-sources/sources \
        /org/gnome/desktop/input-sources/xkb-options > "$db/locks/gse-keyboard"
fi
dconf update
EOF
chmod 0755 /usr/libexec/gse/keyboard
mkdir -p /etc/systemd/system/greetd.service.d
cat > /etc/systemd/system/greetd.service.d/gse.conf <<'EOF'
[Unit]
After=gse-screen-mode.service
ConditionPathExists=!/run/gse/build
EOF
install -d /usr/libexec/gse
cat > /usr/libexec/gse/session <<'EOF'
#!/bin/sh
# Starts the desktop of `sandbox` for the screen the host chose.
if [ -s /run/gse/screen-mode ]; then
    export WLR_BACKENDS=headless,libinput WLR_HEADLESS_OUTPUTS=1
fi
exec startbudgielabwc
EOF
chmod 0755 /usr/libexec/gse/session
# greetd starts the desktop as `sandbox` on boot as an ordinary user session.
# After a logout it falls back to the greeter slot, which starts the same
# desktop again; logind then classes that session as a greeter.
cat > /etc/greetd/config.toml <<'EOF'
[terminal]
vt = 1

[initial_session]
command = "/usr/libexec/gse/session"
user = "sandbox"

[default_session]
command = "/usr/libexec/gse/session"
user = "sandbox"
EOF
systemctl enable greetd.service
# The host waits for this line on the serial console to know the environment
# is up: it goes out once the desktop of `sandbox` has its Wayland socket.
cat > /etc/systemd/system/gse-ready.service <<'EOF'
[Unit]
Description=Tell the host the environment is up
After=greetd.service

[Service]
Type=oneshot
ExecStart=/bin/sh -c 'socket=/run/user/$(id -u sandbox)/wayland-0; for i in $(seq 120); do [ -S $socket ] && exec sh -c "echo GSE-READY > /dev/ttyS0"; sleep 1; done'

[Install]
WantedBy=graphical.target
EOF
systemctl enable gse-ready.service
systemctl set-default graphical.target

step "guest runtime"
# gse-guest travels on the seed volume next to the cloud-init files.
mkdir -p /run/gse-seed
mount -o ro LABEL=CIDATA /run/gse-seed
install -D -m 0755 /run/gse-seed/gse-guest /usr/libexec/gse/gse-guest
umount /run/gse-seed
restorecon -R /usr/libexec/gse

# The sockets `guest` connects to live in a directory owned by `sandbox` with
# setgid group `guest`, so only those two accounts reach them. Every connection
# between them is opened by `guest`: SELinux cannot label one opened by the
# unconfined `sandbox` towards a socket the confined `guest` serves.
semanage fcontext -a -t user_tmp_t '/run/gse(/.*)?'
cat > /etc/tmpfiles.d/gse.conf <<'EOF'
d /run/gse 0755 root root -
d /run/gse/display 2750 sandbox guest -
EOF

# The `guest` account's systemd instance runs from boot, so its launcher and X
# server wait for the desktop rather than the other way round.
loginctl enable-linger guest
units=/home/guest/.config/systemd/user
install -d -o guest -g guest /home/guest/.config /home/guest/.config/systemd \
    "$units" "$units/default.target.wants" /home/guest/.config/environment.d
cat > /home/guest/.config/environment.d/gse.conf <<'EOF'
WAYLAND_DISPLAY=/run/gse/display/wayland-0
DISPLAY=:1
PULSE_SERVER=unix:/run/gse/display/pulse
# The desktop's dark look is set for Budgie sessions only.
XDG_CURRENT_DESKTOP=Budgie:GNOME
# AppImages unpack themselves and run: `guest` has no FUSE to mount one.
APPIMAGE_EXTRACT_AND_RUN=1
EOF
cat > "$units/gse-xwayland.service" <<'EOF'
[Unit]
Description=X server for guest programs, on the restricted display
# Retries until the desktop has opened the display.
StartLimitIntervalSec=0

[Service]
Type=notify
NotifyAccess=all
ExecStart=/usr/bin/xwayland-satellite :1
Restart=always
RestartSec=2
EOF
cat > "$units/gse-launch.service" <<'EOF'
[Unit]
Description=Starts programs as guest when the desktop asks
StartLimitIntervalSec=0

[Service]
ExecStart=/usr/libexec/gse/gse-guest launcher
Restart=always
RestartSec=1
EOF
ln -s ../gse-xwayland.service ../gse-launch.service "$units/default.target.wants/"
# The portals let `guest`'s programs ask where to save a file and show it in
# its folder. They wait for a graphical session, which `guest` never has: it
# draws on the desktop of `sandbox`. Without them Firefox and Chrome save
# into ~/Downloads unasked and cannot open it.
for portal in xdg-desktop-portal xdg-desktop-portal-gtk; do
    install -d "$units/$portal.service.d"
    cat > "$units/$portal.service.d/gse.conf" <<'EOF'
[Unit]
Requisite=
PartOf=
After=
EOF
done

# The agent answers the host about `guest`'s programs and its Inbox and
# Outbox, as root, on a virtio-serial port only root opens
# (crates/gse-guest/src/agent.rs). An image build has no such port; the
# agent then waits for it.
cat > /etc/systemd/system/gse-agent.service <<'EOF'
[Unit]
Description=Answers the host about guest's programs and files
StartLimitIntervalSec=0

[Service]
ExecStart=/usr/libexec/gse/gse-guest agent
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF
systemctl enable gse-agent.service
# The guest's journal goes to the host as it is written, over a port of its
# own, so a sandbox that misbehaves leaves a record outside it. Only an
# environment has the port; without it after a minute (an image build) the
# unit ends.
cat > /etc/systemd/system/gse-journal.service <<'EOF'
[Unit]
Description=Stream the journal to the host

[Service]
ExecStart=/bin/sh -c 'port=/dev/virtio-ports/io.github.sovgut.gse.journal; for i in $(seq 60); do [ -e $port ] && exec journalctl -b -f --no-tail -o short-iso-precise > $port; sleep 1; done'
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF
systemctl enable gse-journal.service
install -d -o guest -g guest -m 0755 /home/guest/Inbox /home/guest/Outbox \
    /home/guest/Downloads
# `sandbox` cannot look into `guest`'s home, so the desktop's Inbox and
# Outbox open a file manager as `guest`: files reach the Outbox from
# wherever `guest`'s programs keep them, and what arrives in the Inbox is
# there to take.
install -d -o sandbox -g sandbox /home/sandbox/Desktop
for box in Inbox Outbox Downloads; do
    case $box in
        Downloads) about="where guest's browsers save what they download" ;;
        *) about="guest's ~/$box, which the host fills or empties" ;;
    esac
    cat > "/home/sandbox/Desktop/gse-$box.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=$box
Comment=$about
Icon=folder
Exec=/usr/libexec/gse/gse-guest run --title $box --icon folder nemo /home/guest/$box
EOF
    chmod 0755 "/home/sandbox/Desktop/gse-$box.desktop"
    chown sandbox:sandbox "/home/sandbox/Desktop/gse-$box.desktop"
done
# The desktop runs shortcuts it is told to trust without asking.
runuser -u sandbox -- dbus-run-session -- sh -c \
    'for f in /home/sandbox/Desktop/gse-*.desktop; do gio set -t string "$f" metadata::trust true; done' || true
# `guest` has no sound server of its own: one would hold no sound card, and
# Wine's PipeWire driver would pick it and play into nothing instead of
# reaching the desktop's server through PULSE_SERVER.
for unit in pipewire.socket pipewire.service pipewire-pulse.socket \
    pipewire-pulse.service wireplumber.service filter-chain.service; do
    ln -s /dev/null "$units/$unit"
done
chown -R guest:guest /home/guest/.config

# Sound for `guest` comes from the desktop's sound server through a second
# PulseAudio socket beside the display. Its clients are "restricted", which
# WirePlumber turns into read-only access, and they may not change any
# volume. They may record, so voice chat works: the host's microphone
# reaches the guest. `guest` has no other way to PipeWire.
mkdir -p /etc/pipewire/pipewire-pulse.conf.d
cat > /etc/pipewire/pipewire-pulse.conf.d/gse.conf <<'EOF'
pulse.properties = {
    server.address = [
        "unix:native"
        { address = "unix:/run/gse/display/pulse" client.access = "restricted" }
    ]
}

pulse.rules = [
    {
        matches = [ { pipewire.client.access = "restricted" } ]
        actions = {
            quirks = [ block-source-volume block-sink-volume ]
        }
    }
]
EOF

# The desktop opens the restricted display and the launch relay when
# `sandbox` logs in.
install -d -o sandbox -g sandbox /home/sandbox/.config/autostart
cat > /home/sandbox/.config/autostart/gse-desktop.desktop <<'EOF'
[Desktop Entry]
Type=Application
Name=gse desktop
Exec=/usr/libexec/gse/gse-guest desktop
EOF
chown sandbox:sandbox /home/sandbox/.config/autostart/gse-desktop.desktop

# The screen the host shows: `sandbox` sets the virtual output to the mode the
# host asked for and copies it into the shared memory (QEMU ivshmem), which
# udev hands to `sandbox` alone. Without that mode the copy does not start.
# The copy runs under the user systemd of `sandbox`, which has the user's
# SELinux context: the desktop greetd starts stays in the display manager's
# domain, which may not map the shared memory.
cat > /etc/udev/rules.d/70-gse-screen.rules <<'EOF'
ACTION=="add", SUBSYSTEM=="pci", ATTR{vendor}=="0x1af4", ATTR{device}=="0x1110", ATTR{enable}="1", RUN+="/bin/sh -c 'chown sandbox:sandbox /sys%p/resource2 /sys%p/resource2_wc'"
EOF
cat > /usr/libexec/gse/screen <<'EOF'
#!/bin/sh
# Copies the desktop to the host when the host asked for it.
[ -s /run/gse/screen-mode ] || exit 0
wlr-randr --output HEADLESS-1 --on --custom-mode "$(cat /run/gse/screen-mode)"
exec systemd-run --user --collect --quiet --unit=gse-screen \
    --setenv=WAYLAND_DISPLAY="$WAYLAND_DISPLAY" /usr/libexec/gse/gse-guest screen
EOF
chmod 0755 /usr/libexec/gse/screen
restorecon -R /usr/libexec/gse
cat > /home/sandbox/.config/autostart/gse-screen.desktop <<'EOF'
[Desktop Entry]
Type=Application
Name=gse screen
Exec=/usr/libexec/gse/screen
EOF
chown sandbox:sandbox /home/sandbox/.config/autostart/gse-screen.desktop

if [ "${GSE_DEV:-0}" = 1 ]; then
    dnf -y install grim wl-clipboard wayland-utils xdpyinfo pulseaudio-utils
fi

step "flatpak"
flatpak remote-add --system --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
# Fedora leaves system Flatpak deployments as var_lib_t, which a confined user
# may not execute. Deployed files are hardlinks to objects in the repository
# and share their inode, so the repository gets the same label, and whatever
# installs Flatpaks relabels afterwards.
mkdir -p /var/lib/flatpak/app /var/lib/flatpak/repo /var/lib/flatpak/runtime
semanage fcontext -a -t bin_t '/var/lib/flatpak/(app|repo|runtime)(/.*)?'
restorecon -R /var/lib/flatpak

step "sandboxes"
# Flatpak and Steam's pressure-vessel run programs under bwrap, which `guest`
# may use as it creates its own user namespace. Inside that namespace bwrap
# brings up loopback, caps nested user namespaces and builds a new root from
# bind mounts, which it then remounts read-only one by one: every filesystem
# type under / (proc, sysfs, cgroup, efivarfs...) and any directory or file
# it mounts onto. GTK does the same for each image it loads (glycin runs its
# loaders under bwrap), so without this even Nemo for the Inbox aborts on its
# first icon. Each of these needs CAP_NET_ADMIN, CAP_SYS_RESOURCE or
# CAP_SYS_ADMIN, which `guest` holds only over namespaces it created, and the
# kernel lets such a namespace mount only what is safe there and never lift a
# flag the mount was given outside, so the rules below reach nothing outside
# them. A Flatpak sub-sandbox (Electron apps such as Heroic start their
# renderers in one) joins the namespaces of its parent app through
# /proc/<pid>/ns, which the kernel opens only for the caller's own
# processes. The policy refuses `user_t` those files without logging it, so
# such an app hangs with no window. Heroic's Electron also uses io_uring.
cat > /tmp/gse_sandboxes.cil <<'EOF'
(allow user_t file_type (dir (mounton)))
(allow user_t file_type (file (mounton)))
(allow user_t filesystem_type (filesystem (mount remount unmount)))
(allow user_t io_uring_t (anon_inode (create getattr map read write)))
(allow user_t nsfs_t (file (open read)))
(allow user_t self (netlink_route_socket (nlmsg_write)))
(allow user_t sysctl_t (file (open write)))
EOF
semodule -i /tmp/gse_sandboxes.cil
rm /tmp/gse_sandboxes.cil

# Proton synchronises Windows threads through /dev/ntsync, and the sandboxes
# bind it in. The policy leaves it the generic device_t, which `guest` may not
# touch, so it gets a type of its own that `guest` may use. The device is meant
# for unprivileged users (mode 0666). tmpfiles restores the label at boot in
# case the module loads before udev labels devices.
cat > /tmp/gse_ntsync.cil <<'EOF'
(type gse_ntsync_device_t)
(roletype object_r gse_ntsync_device_t)
(typeattributeset device_node (gse_ntsync_device_t))
(allow user_t gse_ntsync_device_t (chr_file (getattr open read write ioctl)))
EOF
semodule -i /tmp/gse_ntsync.cil
rm /tmp/gse_ntsync.cil
semanage fcontext -a -t gse_ntsync_device_t /dev/ntsync
echo 'z /dev/ntsync - - - -' > /etc/tmpfiles.d/gse-ntsync.conf

step "cleanup"
if [ "${GSE_DEV:-0}" != 1 ]; then
    systemctl disable sshd.service
    userdel --remove dev 2>/dev/null || true
    rm -f /etc/sudoers.d/90-cloud-init-users
fi
dnf clean all
# cloud-init only looks for a NoCloud seed volume. An environment boots without one,
# and cloud-init then turns itself off for that boot without waiting; a preset
# build attaches one, and cloud-init runs the preset's script.
cat > /etc/cloud/cloud.cfg.d/90-gse.cfg <<'EOF'
datasource_list: [ NoCloud ]
EOF
truncate -s 0 /etc/machine-id

trap - ERR
verdict "OK"
