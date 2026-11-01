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
    xorg-x11-server-Xwayland greetd greetd-selinux \
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

step "flatpak"
flatpak remote-add --system --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo

step "cleanup"
if [ "${GAMEJAIL_DEV:-0}" != 1 ]; then
    systemctl disable sshd.service
    userdel --remove dev 2>/dev/null || true
    rm -f /etc/sudoers.d/90-cloud-init-users
fi
dnf clean all
# The image boots without a seed from now on; cloud-init would only wait for
# one.
touch /etc/cloud/cloud-init.disabled
truncate -s 0 /etc/machine-id

trap - ERR
verdict "OK"
