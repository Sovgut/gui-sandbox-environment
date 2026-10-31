#!/usr/bin/env bash
# Moves a GPU and every function in its slot between the host drivers and
# vfio-pci on the development PC, where another GPU drives the desktop.
# Runs as root.
#
# Usage:
#   gpu-vfio.sh check  [slot]   list processes holding the GPU's DRM nodes
#   gpu-vfio.sh attach [slot]   bind to vfio-pci, hand /dev/vfio/<group> to the caller
#                               (refuses while anything holds the GPU; FORCE=1 unbinds
#                               anyway, which is safe only for KWin's unused render-node
#                               descriptor: unbinding under an application's live
#                               buffers corrupts kernel memory)
#   gpu-vfio.sh detach [slot]   return to the host drivers
#
# slot defaults to 0000:0e:00 (the RTX 3070 on the development PC).

set -euo pipefail

action="${1:?usage: $0 check|attach|detach [slot]}"
slot="${2:-0000:0e:00}"
owner="${PKEXEC_UID:-${SUDO_UID:-$(id -u)}}"

functions=(/sys/bus/pci/devices/"$slot".*)
[ -e "${functions[0]}" ] || { echo "no PCI device at $slot" >&2; exit 1; }

# DRM nodes of the slot: /dev/dri/cardN and renderDN.
gpu_nodes() {
    local f
    for f in "${functions[@]}"; do
        [ -d "$f/drm" ] || continue
        for n in "$f"/drm/card* "$f"/drm/renderD*; do
            [ -e "$n" ] && echo "/dev/dri/$(basename "$n")"
        done
    done
}

holders() {
    local nodes p fd target
    mapfile -t nodes < <(gpu_nodes)
    [ "${#nodes[@]}" -gt 0 ] || return 0
    for p in /proc/[0-9]*; do
        for fd in "$p"/fd/*; do
            target="$(readlink "$fd" 2>/dev/null)" || continue
            for n in "${nodes[@]}"; do
                if [ "$target" = "$n" ]; then
                    echo "$(basename "$p") $(cat "$p/comm" 2>/dev/null) $n"
                fi
            done
        done
    done | sort -u
}

case "$action" in
check)
    echo "nodes: $(gpu_nodes | tr '\n' ' ')"
    h="$(holders)"
    if [ -n "$h" ]; then echo "held by:"; echo "$h"; else echo "held by: nobody"; fi
    ;;
attach)
    h="$(holders)"
    if [ -n "$h" ]; then
        if [ "${FORCE:-0}" = 1 ]; then
            echo "FORCE=1: unbinding while still held by" >&2
            echo "$h" >&2
        else
            echo "refusing: the GPU is still in use by" >&2
            echo "$h" >&2
            exit 1
        fi
    fi
    modprobe vfio-pci
    for f in "${functions[@]}"; do
        dev="$(basename "$f")"
        echo vfio-pci > "$f/driver_override"
        [ -e "$f/driver" ] && echo "$dev" > "$f/driver/unbind"
        echo "$dev" > /sys/bus/pci/drivers_probe
        echo "$dev -> $(basename "$(readlink "$f/driver")")"
    done
    group="$(basename "$(readlink "${functions[0]}/iommu_group")")"
    chown "$owner" "/dev/vfio/$group"
    echo "/dev/vfio/$group owned by uid $owner"
    ;;
detach)
    for f in "${functions[@]}"; do
        dev="$(basename "$f")"
        echo > "$f/driver_override"
        [ -e "$f/driver" ] && echo "$dev" > "$f/driver/unbind"
        echo "$dev" > /sys/bus/pci/drivers_probe
        echo "$dev -> $(basename "$(readlink "$f/driver" 2>/dev/null)")"
    done
    ;;
*)
    echo "unknown action: $action" >&2
    exit 1
    ;;
esac
