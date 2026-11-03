#!/bin/bash
# Copies what gse reads about a machine's CPU and GPUs into a tarball, laid
# out like the real /proc and /sys, so it can become a machine fixture for
# crates/gse-host (see crates/gse-host/src/machines.rs). Nothing personal
# goes in: no hostname, serial numbers, user names, disks or network.
#
# Usage: capture-machine.sh [OUT.tar.gz]   (no root needed)

set -euo pipefail

out="${1:-machine-$(date +%Y%m%d-%H%M%S).tar.gz}"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
tree="$work/machine"

# Copies the file `$1` into the tree at its own path, if it is readable.
copy() {
    [ -r "$1" ] || return 0
    mkdir -p "$tree$(dirname "$1")"
    cat "$1" > "$tree$1" 2>/dev/null || rm -f "$tree$1"
}

# Recreates the symlink `$1` with its target as it is.
link() {
    [ -L "$1" ] || return 0
    mkdir -p "$tree$(dirname "$1")"
    ln -sfn "$(readlink "$1")" "$tree$1"
}

copy /proc/cpuinfo
# The kernel arguments without the ones that name disks by their UUIDs.
mkdir -p "$tree/proc"
tr ' ' '\n' < /proc/cmdline | grep -iv uuid | paste -sd' ' - > "$tree/proc/cmdline"
for topology in /sys/devices/system/cpu/cpu[0-9]*/topology; do
    copy "$topology/core_id"
    copy "$topology/physical_package_id"
done
copy /sys/devices/cpu_core/cpus
copy /sys/devices/cpu_atom/cpus

# Every PCI function at its place behind its bridges, as the kernel lays
# them out, with the link from /sys/bus/pci/devices.
for entry in /sys/bus/pci/devices/*; do
    real="$(readlink -f "$entry")"
    for attribute in vendor device class boot_vga reset_method driver_override; do
        copy "$real/$attribute"
    done
    link "$real/driver"
    link "$real/iommu_group"
    for node in "$real"/drm/card* "$real"/drm/renderD*; do
        [ -e "$node" ] || continue
        mkdir -p "$tree$node"
        for connector in "$node"/card*-*; do
            if [ -e "$connector/status" ]; then copy "$connector/status"; fi
        done
    done
    link "$entry"
done
for group in /sys/kernel/iommu_groups/*; do
    mkdir -p "$tree$group"
done

tar -czf "$out" -C "$work" machine
echo "$out"
