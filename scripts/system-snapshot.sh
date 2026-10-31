#!/usr/bin/env bash
# Collects what gamejail needs to know about a machine: OS and image-based
# deployment, boot, CPU virtualization, IOMMU groups, GPUs and their drivers,
# what a single-GPU switch has to stop (consoles, framebuffers, GPU users and
# services), displays, session, memory, storage, network, audio, input devices
# and installed tooling.
#
# Read-only and runs without root. Writes one text report to the current
# directory and prints each section to the terminal as it starts, so a stall
# shows where it is. Hostname and user name are replaced, and no MAC addresses,
# serial numbers or public IP addresses are collected.
#
# Usage: bash system-snapshot.sh

set -u
export LC_ALL=C

out="gamejail-snapshot-$(date +%Y%m%d-%H%M%S).txt"
host_name="$(cat /proc/sys/kernel/hostname 2>/dev/null || echo unknown)"
user_name="$(id -un)"

# The terminal, kept apart from the report stream.
exec 3>&2

have() { command -v "$1" >/dev/null 2>&1; }

section() {
    printf '\n===== %s =====\n' "$1"
    printf '[gamejail-snapshot] %s\n' "$1" >&3
}

# Every external command gets 20 seconds: GPU tools hang on a wedged driver and
# package tools can stall on metadata.
t() { timeout 20 "$@"; }

# Runs a command if it exists and labels its output; a missing tool is reported
# rather than silently skipped, since absence is itself information.
run() {
    printf -- '--- $ %s\n' "$*"
    if have "$1"; then
        t "$@" 2>&1 || echo "(exit $?)"
    else
        echo "(not installed: $1)"
    fi
}

show() {
    printf -- '--- %s\n' "$1"
    if [ -r "$1" ]; then cat "$1" 2>&1; else echo "(not readable)"; fi
}

# Distribution default hostnames are left alone: replacing "fedora" would
# mangle os-release and package names.
case "$host_name" in
    fedora|localhost|localhost-live|ubuntu|debian|archlinux|nobara|bazzite) host_name='' ;;
esac

# Unbuffered, so an interrupted run keeps everything collected up to that point.
redact() {
    sed -u -E \
        ${host_name:+-e "s/\\b${host_name//./\\.}\\b/<host>/g"} \
        -e "s/\\b${user_name}\\b/<user>/g" \
        -e 's/([0-9a-fA-F]{2}[:_]){5}[0-9a-fA-F]{2}/<mac>/g' \
        -e 's/wwn-0x[0-9a-fA-F]+/wwn-<id>/g' \
        -e 's/[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}/<uuid>/g'
}

pci_line() { lspci -nns "$1" 2>/dev/null | cut -d' ' -f2-; }

gpu_devices() {
    local dev cls
    for dev in /sys/bus/pci/devices/*; do
        cls="$(cat "$dev/class" 2>/dev/null)"
        case "$cls" in 0x0300*|0x0302*) echo "$dev" ;; esac
    done
}

collect() {
    section "snapshot"
    date -u +%Y-%m-%dT%H:%M:%SZ
    echo "running as root: $([ "$(id -u)" -eq 0 ] && echo yes || echo no)"
    echo "inside container: $([ -e /run/.containerenv ] || [ -e /.dockerenv ] && echo yes || echo no)"

    section "os"
    show /etc/os-release
    run uname -srvmo
    show /proc/cmdline
    run systemctl --version
    run ldd --version
    echo "--- secure boot"
    run mokutil --sb-state
    show /sys/kernel/security/lockdown
    echo "--- selinux"
    run getenforce
    echo "--- bootloader"
    [ -d /sys/firmware/efi ] && echo "firmware: UEFI" || echo "firmware: BIOS"
    run bootctl is-installed
    have grubby && echo "grubby present (kernel args via grubby)"
    [ -e /etc/kernel/cmdline ] && show /etc/kernel/cmdline

    section "os image"
    if [ -e /run/ostree-booted ]; then
        echo "image-based (ostree): yes"
        run rpm-ostree status --booted
        run rpm-ostree kargs
        [ -r /usr/share/ublue-os/image-info.json ] && show /usr/share/ublue-os/image-info.json
    else
        echo "image-based (ostree): no"
    fi
    echo "/usr mount: $(findmnt -no OPTIONS /usr 2>/dev/null || echo 'part of /')"
    echo "/etc writable: $([ -w /etc ] && echo yes || echo 'no (as this user)')"

    section "motherboard"
    for f in sys_vendor product_name board_vendor board_name bios_vendor bios_version bios_date; do
        printf '%s: %s\n' "$f" "$(cat /sys/class/dmi/id/$f 2>/dev/null)"
    done

    section "cpu"
    run lscpu
    echo "--- virtualization flags"
    grep -oEm1 '\b(vmx|svm)\b' /proc/cpuinfo || echo "(no vmx/svm flag: virtualization disabled in firmware or unsupported)"
    echo "--- /dev/kvm"
    ls -l /dev/kvm 2>&1
    echo "--- relevant modules"
    lsmod 2>/dev/null | grep -E '^(kvm|vfio|nvidia|nouveau|i915|xe|amdgpu|vhost)' || echo "(none loaded)"
    for m in kvm_intel kvm_amd; do
        [ -r /sys/module/$m/parameters/nested ] && echo "$m nested: $(cat /sys/module/$m/parameters/nested)"
    done

    section "iommu"
    echo "iommu devices: $(ls /sys/class/iommu 2>/dev/null | tr '\n' ' ')"
    groups=/sys/kernel/iommu_groups
    if [ -d "$groups" ] && [ -n "$(ls -A $groups 2>/dev/null)" ]; then
        for g in $(ls $groups | sort -n); do
            echo "group $g:"
            for d in "$groups/$g"/devices/*; do
                echo "    $(pci_line "$(basename "$d")")"
            done
        done
    else
        echo "(no IOMMU groups: IOMMU is off in firmware or on the kernel command line)"
    fi
    echo "--- kernel log, iommu and vfio"
    if have journalctl; then
        t journalctl -k -b --no-pager 2>/dev/null | grep -iE 'iommu|dmar|amd-vi|vfio|interrupt remapping' \
            | grep -v 'Adding to iommu group' | head -40
    fi

    section "gpu"
    run lspci -nnk -d ::0300
    run lspci -nnk -d ::0302
    run lspci -nnk -d ::0403
    for dev in $(gpu_devices); do
        a="$(basename "$dev")"
        echo "--- $a $(pci_line "$a")"
        echo "boot_vga: $(cat "$dev/boot_vga" 2>/dev/null)"
        echo "driver: $(basename "$(readlink "$dev/driver" 2>/dev/null)" 2>/dev/null)"
        echo "iommu_group: $(basename "$(readlink "$dev/iommu_group" 2>/dev/null)" 2>/dev/null)"
        echo "reset_method: $(cat "$dev/reset_method" 2>/dev/null)"
        echo "link: $(cat "$dev/current_link_speed" 2>/dev/null) x$(cat "$dev/current_link_width" 2>/dev/null)"
        echo "rom exposed: $([ -e "$dev/rom" ] && echo yes || echo no)"
        printf 'functions in slot:'
        for f in /sys/bus/pci/devices/"${a%.*}".*; do printf ' %s' "$(basename "$f")"; done
        echo
        printf 'drm nodes:'
        for n in "$dev"/drm/card* "$dev"/drm/renderD*; do [ -e "$n" ] && printf ' %s' "$(basename "$n")"; done
        echo
    done
    echo "--- nvidia driver"
    show /proc/driver/nvidia/version
    have modinfo && t modinfo -F license nvidia 2>/dev/null | sed 's/^/nvidia module license: /'
    for p in modeset fbdev; do
        [ -r /sys/module/nvidia_drm/parameters/$p ] && echo "nvidia_drm $p: $(cat /sys/module/nvidia_drm/parameters/$p)"
    done
    for m in nvidia nvidia_drm nvidia_modeset nvidia_uvm; do
        [ -r /sys/module/$m/refcnt ] && echo "$m refcnt: $(cat /sys/module/$m/refcnt)"
    done
    echo "--- /dev/nvidia*"
    ls -l /dev/nvidia* 2>/dev/null || echo "(none)"
    run nvidia-smi --query-gpu=name,driver_version,vbios_version,memory.total,pcie.link.gen.max,pcie.link.width.max --format=csv
    echo "--- nvidia-smi process table"
    have nvidia-smi && t nvidia-smi 2>&1 | sed -n '/Processes:/,$p'
    echo "--- vulkan"
    if have vulkaninfo; then
        t vulkaninfo --summary 2>/dev/null | grep -E 'deviceName|driverName|driverInfo|apiVersion'
    else
        echo "(vulkaninfo not installed)"
    fi

    section "gpu switch"
    echo "--- virtual consoles"
    for v in /sys/class/vtconsole/vtcon*; do
        [ -e "$v" ] && echo "$(basename "$v"): bind=$(cat "$v/bind" 2>/dev/null) $(cat "$v/name" 2>/dev/null)"
    done
    echo "--- framebuffers"
    for fb in /sys/class/graphics/fb[0-9]*; do
        [ -e "$fb" ] && echo "$(basename "$fb"): $(cat "$fb/name" 2>/dev/null) device=$(basename "$(readlink -f "$fb/device" 2>/dev/null)")"
    done
    echo "--- kernel log, framebuffer and display drivers"
    if have journalctl; then
        t journalctl -k -b --no-pager -o short-monotonic 2>/dev/null \
            | grep -iE 'simpledrm|efifb|vesafb|fbcon|nvidia-drm|nvidia-modeset|vgaarb' | head -30
    fi
    echo "--- processes of this user holding GPU device nodes"
    for p in /proc/[0-9]*; do
        hits="$(for fd in "$p"/fd/*; do readlink "$fd"; done 2>/dev/null | grep -E '^/dev/(nvidia|dri/)' | sort -u | tr '\n' ' ')"
        [ -n "$hits" ] && echo "$(basename "$p") $(cat "$p/comm" 2>/dev/null): $hits"
    done
    echo "--- running system services that may use the GPU"
    have systemctl && t systemctl list-units --type=service --state=running --no-legend --plain 2>/dev/null \
        | awk '{print $1}' | grep -iE 'nvidia|display|sddm|gdm|lightdm|plasmalogin|greetd|gamescope|kmscon|sunshine|coolercontrol|ollama|lact|switcheroo' \
        || echo "(none matched)"
    echo "--- running user services that may use the GPU"
    have systemctl && t systemctl --user list-units --type=service --state=running --no-legend --plain 2>/dev/null \
        | awk '{print $1}' | grep -iE 'plasma|kwin|gnome-shell|gamescope|steam|sunshine|xpra|wayland' \
        || echo "(none matched)"
    echo "--- enabled nvidia system services"
    have systemctl && t systemctl list-unit-files --no-legend --plain 'nvidia*' 2>/dev/null || echo "(none)"

    section "displays"
    for c in /sys/class/drm/card*-*; do
        [ -r "$c/status" ] || continue
        echo "$(basename "$c"): $(cat "$c/status") $(head -1 "$c/modes" 2>/dev/null)"
    done

    section "session"
    echo "XDG_SESSION_TYPE=${XDG_SESSION_TYPE:-}"
    echo "XDG_CURRENT_DESKTOP=${XDG_CURRENT_DESKTOP:-}"
    echo "display-manager: $(basename "$(readlink /etc/systemd/system/display-manager.service 2>/dev/null)" 2>/dev/null)"
    printf 'available sessions:'
    for s in /usr/share/wayland-sessions/*.desktop /usr/share/xsessions/*.desktop; do
        [ -e "$s" ] && printf ' %s' "$(basename "$s")"
    done
    echo
    run loginctl list-sessions --no-legend
    run loginctl show-user "$user_name" -p Linger
    if have systemd-analyze; then
        t systemd-analyze cat-config systemd/logind.conf 2>/dev/null | grep -E '^\s*KillUserProcesses' || echo "KillUserProcesses: default"
    fi

    section "memory"
    run free -h
    grep -E 'MemTotal|HugePages_Total|Hugepagesize' /proc/meminfo

    section "storage"
    run lsblk -o NAME,SIZE,TYPE,FSTYPE,ROTA,TRAN,MOUNTPOINTS
    run df -hT -x tmpfs -x devtmpfs -x squashfs -x overlay

    section "network"
    if have ip; then
        ip -br link 2>&1
        echo "--- private and link-local addresses"
        ip -o addr show 2>/dev/null | awk '{print $2, $3, $4}' \
            | grep -E ' (10\.|172\.(1[6-9]|2[0-9]|3[01])\.|192\.168\.|100\.(6[4-9]|[7-9][0-9]|1[01][0-9]|12[0-7])\.|169\.254\.|127\.|fe80:|fd|fc|::1)'
        echo "--- ipv6 global addresses (count only)"
        ip -6 -o addr show scope global 2>/dev/null | wc -l
        echo "--- routes"
        ip route 2>&1
    fi
    run nmcli -t -f STATE,CONNECTIVITY general status
    run nmcli -t -f DEVICE,TYPE,STATE device
    echo "--- firewall"
    have systemctl && systemctl is-active firewalld nftables iptables 2>&1 | paste -sd' ' | sed 's/^/firewalld nftables iptables: /'
    have nft && echo "nft: $(nft --version 2>&1)"

    section "audio"
    have pactl && t pactl info 2>&1 | grep -E '^(Server Name|Default Sink|Default Source)'
    run pactl list short cards
    run pactl list short sinks
    run pactl list short sources

    section "input devices"
    ls -1 /dev/input/by-id/ 2>/dev/null | sed -E 's/_[0-9A-Fa-f]{8,}//g' || echo "(none)"

    section "usb controllers"
    run lspci -nn -d ::0c03

    section "virtualization and tooling"
    for t in qemu-system-x86_64 qemu-kvm qemu-img virsh virt-install swtpm \
             nft bwrap flatpak podman distrobox toolbox xpra gamescope clamscan freshclam \
             fusermount fusermount3 cargo rustc; do
        printf '%-20s %s\n' "$t" "$(command -v "$t" 2>/dev/null || echo -)"
    done
    for f in /usr/share/edk2/ovmf/OVMF_CODE.fd /usr/share/edk2/ovmf/OVMF_CODE_4M.qcow2 /usr/share/OVMF/OVMF_CODE.fd; do
        [ -e "$f" ] && echo "ovmf: $f"
    done
    ls /usr/lib64/libfuse.so.2* /usr/lib/x86_64-linux-gnu/libfuse.so.2* 2>/dev/null | sed 's/^/libfuse2: /'
    if have rpm; then
        echo "--- rpm packages"
        t rpm -q qemu-kvm qemu-img libvirt edk2-ovmf akmod-nvidia kmod-nvidia xorg-x11-drv-nvidia nvidia-driver \
               xpra clamav pipewire wireplumber kernel 2>&1
        if have dnf && [ ! -e /run/ostree-booted ]; then
            echo "--- enabled repositories"
            t dnf repolist --enabled 2>/dev/null
        fi
    fi
    if have flatpak; then
        echo "--- flatpak apps"
        t flatpak list --app --columns=application,branch,installation 2>&1
    fi
    echo "--- groups of this user"
    id -Gn

    section "resources in use"
    run uptime
    run nproc

    section "done"
}

collect 2>&1 | redact > "$out"
echo "Report written to: $PWD/$out"
echo "Look it over before sending it; it lists hardware, installed apps and local network addresses."
