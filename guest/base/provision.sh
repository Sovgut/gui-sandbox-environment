#!/bin/bash
# Builds the base layer every jail image shares, inside a fresh Fedora Cloud
# guest that cloud-init boots once: the NVIDIA driver, a light Wayland desktop
# that logs `friend` in on its own, the confined `jail` account for games,
# PipeWire and Flatpak. The build VM has no GPU, so the driver module is built
# for the newest installed kernel rather than the running one.
#
# The last line on the serial console is the verdict the host reads:
# "GAMEJAIL-BUILD: OK" or "GAMEJAIL-BUILD: FAILED <line>". The guest powers off
# either way.
#
# GAMEJAIL_DEV=1 keeps SSH and the `dev` account cloud-init created; without it
# the image has no SSH server and no account that can become root.

set -euo pipefail

verdict() {
    echo "GAMEJAIL-BUILD: $*" > /dev/ttyS0
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
dnf -y install akmod-nvidia xorg-x11-drv-nvidia-cuda "kernel-devel-$kernel"
akmods --force --kernels "$kernel"
# A missing module would only show at the first jail boot as a black screen.
modinfo -k "$kernel" nvidia > /dev/null

step "desktop"
dnf -y install \
    labwc lxqt-wayland-session lxqt-session lxqt-panel lxqt-config \
    pcmanfm-qt qterminal featherpad xdg-desktop-portal-lxqt xdg-desktop-portal-wlr \
    xorg-x11-server-Xwayland xwayland-satellite greetd greetd-selinux \
    pipewire wireplumber pipewire-pulseaudio \
    flatpak policycoreutils-python-utils google-noto-sans-fonts

step "accounts"
# `friend` owns the desktop and trusted apps; `jail` runs every launcher and
# game as an SELinux confined user, which forbids su, sudo and SUID
# transitions. Neither has a password: the desktop starts without a login.
useradd --create-home --comment "Desktop owner" friend
useradd --create-home --comment "Games" jail
semanage login -a -s user_u jail
chmod 0700 /home/friend /home/jail

install -d -o friend -g friend /home/friend/.config/lxqt
cat > /home/friend/.config/lxqt/session.conf <<'EOF'
[General]
compositor=labwc
EOF
chown friend:friend /home/friend/.config/lxqt/session.conf

step "login"
# greetd starts the desktop as `friend` on boot as an ordinary user session.
# After a logout it falls back to the greeter slot, which starts the same
# desktop again; logind then classes that session as a greeter.
cat > /etc/greetd/config.toml <<'EOF'
[terminal]
vt = 1

[initial_session]
command = "startlxqtwayland"
user = "friend"

[default_session]
command = "startlxqtwayland"
user = "friend"
EOF
systemctl enable greetd.service
systemctl set-default graphical.target

step "jail runtime"
# gamejail-guest travels on the seed volume next to the cloud-init files.
mkdir -p /run/gamejail-seed
mount -o ro LABEL=CIDATA /run/gamejail-seed
install -D -m 0755 /run/gamejail-seed/gamejail-guest /usr/libexec/gamejail/gamejail-guest
umount /run/gamejail-seed
restorecon -R /usr/libexec/gamejail

# The sockets `jail` connects to live in a directory owned by `friend` with
# setgid group `jail`, so only those two accounts reach them. Every connection
# between them is opened by `jail`: SELinux cannot label one opened by the
# unconfined `friend` towards a socket the confined `jail` serves.
semanage fcontext -a -t user_tmp_t '/run/gamejail(/.*)?'
cat > /etc/tmpfiles.d/gamejail.conf <<'EOF'
d /run/gamejail 0755 root root -
d /run/gamejail/display 2750 friend jail -
EOF

# The jail account's systemd instance runs from boot, so its launcher and X
# server wait for the desktop rather than the other way round.
loginctl enable-linger jail
units=/home/jail/.config/systemd/user
install -d -o jail -g jail /home/jail/.config /home/jail/.config/systemd \
    "$units" "$units/default.target.wants" /home/jail/.config/environment.d
cat > /home/jail/.config/environment.d/gamejail.conf <<'EOF'
WAYLAND_DISPLAY=/run/gamejail/display/wayland-0
DISPLAY=:1
PULSE_SERVER=unix:/run/gamejail/display/pulse
EOF
cat > "$units/gamejail-xwayland.service" <<'EOF'
[Unit]
Description=X server for jail programs, on the restricted display
# Retries until the desktop has opened the display.
StartLimitIntervalSec=0

[Service]
Type=notify
NotifyAccess=all
ExecStart=/usr/bin/xwayland-satellite :1
Restart=always
RestartSec=2
EOF
cat > "$units/gamejail-launch.service" <<'EOF'
[Unit]
Description=Starts programs as jail when the desktop asks
StartLimitIntervalSec=0

[Service]
ExecStart=/usr/libexec/gamejail/gamejail-guest launcher
Restart=always
RestartSec=1
EOF
ln -s ../gamejail-xwayland.service ../gamejail-launch.service "$units/default.target.wants/"
chown -R jail:jail /home/jail/.config

# Sound for `jail` comes from the desktop's sound server through a second
# PulseAudio socket beside the display. Its clients are "restricted", which
# WirePlumber turns into read-only access, and they may not record (no
# microphone, no monitor of what others play) or change any volume. `jail`
# has no other way to PipeWire.
mkdir -p /etc/pipewire/pipewire-pulse.conf.d
cat > /etc/pipewire/pipewire-pulse.conf.d/gamejail.conf <<'EOF'
pulse.properties = {
    server.address = [
        "unix:native"
        { address = "unix:/run/gamejail/display/pulse" client.access = "restricted" }
    ]
}

pulse.rules = [
    {
        matches = [ { pipewire.client.access = "restricted" } ]
        actions = {
            quirks = [ block-record-stream block-source-volume block-sink-volume ]
        }
    }
]
EOF

# The desktop opens the restricted display and the launch relay when
# `friend` logs in.
install -d -o friend -g friend /home/friend/.config/autostart
cat > /home/friend/.config/autostart/gamejail-desktop.desktop <<'EOF'
[Desktop Entry]
Type=Application
Name=gamejail desktop
Exec=/usr/libexec/gamejail/gamejail-guest desktop
EOF
chown friend:friend /home/friend/.config/autostart/gamejail-desktop.desktop

if [ "${GAMEJAIL_DEV:-0}" = 1 ]; then
    dnf -y install grim wl-clipboard wayland-utils xdpyinfo pulseaudio-utils
fi

step "flatpak"
flatpak remote-add --system --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo

step "cleanup"
if [ "${GAMEJAIL_DEV:-0}" != 1 ]; then
    systemctl disable sshd.service
    userdel --remove dev 2>/dev/null || true
    rm -f /etc/sudoers.d/90-cloud-init-users
fi
dnf clean all
# cloud-init only looks for a NoCloud seed volume. A jail boots without one,
# and cloud-init then turns itself off for that boot without waiting; a preset
# build attaches one, and cloud-init runs the preset's script.
cat > /etc/cloud/cloud.cfg.d/90-gamejail.cfg <<'EOF'
datasource_list: [ NoCloud ]
EOF
truncate -s 0 /etc/machine-id

trap - ERR
verdict "OK"
