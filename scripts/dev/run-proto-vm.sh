#!/usr/bin/env bash
# Boots the development prototype VM with the RTX 3070 passed through. Runs as
# root because VFIO pins all guest memory, which needs an unlimited memlock
# limit; QEMU drops to the calling user once its devices are open.
#
# The GPU must already be on vfio-pci (gpu-vfio.sh attach). The guest is
# reachable over SSH on 127.0.0.1:2222, its emulated display over VNC on
# 127.0.0.1:5901 and its serial console in console.log. This prototype uses
# QEMU user networking; the jail network isolation is not part of it.
#
# Usage: run-proto-vm.sh [slot]

set -euo pipefail

slot="${1:-0000:0e:00}"
root="$(cd "$(dirname "$0")/../.." && pwd)"
vm="$root/.working/vm/proto"
user="$(id -un "${PKEXEC_UID:-${SUDO_UID:-$(id -u)}}")"

gpu_args=()
for f in /sys/bus/pci/devices/"$slot".*; do
    drv="$(basename "$(readlink "$f/driver" 2>/dev/null)")"
    [ "$drv" = vfio-pci ] || { echo "$(basename "$f") is on '$drv', run gpu-vfio.sh attach first" >&2; exit 1; }
    gpu_args+=(-device "vfio-pci,host=$(basename "$f")")
done

ulimit -l unlimited

exec qemu-system-x86_64 \
    -name gamejail-proto \
    -machine q35,accel=kvm \
    -cpu host,topoext=on \
    -smp 8,sockets=1,cores=4,threads=2 \
    -m 12G \
    -drive if=pflash,format=qcow2,readonly=on,file=/usr/share/edk2/ovmf/OVMF_CODE_4M.qcow2 \
    -drive if=pflash,format=qcow2,file="$vm/OVMF_VARS.qcow2" \
    -drive file="$vm/disk.qcow2",if=virtio,format=qcow2 \
    -drive file="$vm/seed.img",if=virtio,format=raw,readonly=on \
    "${gpu_args[@]}" \
    -nic user,model=virtio-net-pci,hostfwd=tcp:127.0.0.1:2222-:22 \
    -device virtio-vga \
    -display vnc=127.0.0.1:1 \
    -serial file:"$vm/console.log" \
    -monitor unix:"$vm/monitor.sock",server,nowait \
    -pidfile "$vm/qemu.pid" \
    -run-with user="$user"
