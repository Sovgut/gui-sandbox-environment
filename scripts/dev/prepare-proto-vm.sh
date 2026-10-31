#!/usr/bin/env bash
# Prepares the development prototype VM: an overlay disk on the Fedora Cloud
# base image, a private OVMF variable store, an SSH key and a cloud-init NoCloud
# seed. Runs without root. Re-running keeps the existing disk.
#
# Usage: scripts/dev/prepare-proto-vm.sh <base-image.qcow2>

set -euo pipefail

base="$(realpath "${1:?usage: $0 <base-image.qcow2>}")"
root="$(cd "$(dirname "$0")/../.." && pwd)"
vm="$root/.working/vm/proto"
mkdir -p "$vm"

if [ ! -e "$vm/disk.qcow2" ]; then
    qemu-img create -q -f qcow2 -F qcow2 -b "$base" "$vm/disk.qcow2" 60G
fi
[ -e "$vm/OVMF_VARS.qcow2" ] || cp /usr/share/edk2/ovmf/OVMF_VARS_4M.qcow2 "$vm/OVMF_VARS.qcow2"
[ -e "$vm/id_ed25519" ] || ssh-keygen -q -t ed25519 -N '' -C gamejail-proto -f "$vm/id_ed25519"

seed="$vm/seed"
mkdir -p "$seed"
cat > "$seed/meta-data" <<EOF
instance-id: gamejail-proto
local-hostname: gamejail-proto
EOF
# The prototype user gets passwordless sudo; the real jail image has no sudo.
cat > "$seed/user-data" <<EOF
#cloud-config
users:
  - name: dev
    groups: [wheel]
    sudo: "ALL=(ALL) NOPASSWD:ALL"
    shell: /bin/bash
    ssh_authorized_keys:
      - $(cat "$vm/id_ed25519.pub")
ssh_pwauth: false
growpart: {mode: auto, devices: ["/"]}
EOF

rm -f "$vm/seed.img"
mkfs.vfat -n CIDATA -C "$vm/seed.img" 1024 >/dev/null
mcopy -i "$vm/seed.img" "$seed/meta-data" "$seed/user-data" ::

echo "Prototype VM prepared in $vm"
