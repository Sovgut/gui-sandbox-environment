#!/bin/sh
# Tries the command lines gse runs (crates/gse-host/examples/qemu_lines.rs) on
# the tools of each Fedora release gse supports, in a container per release:
# passt and QEMU as the service starts them, and its nftables ruleset. A QEMU
# line passes when QEMU starts it paused and keeps running; an option that
# release does not know makes it exit at once with the reason.
#
# Run on the host: scripts/dev/qemu-compat.sh [release...]   (default: 44 45)
#
# What a container cannot have is checked by name instead of by use: the
# PulseAudio backend (no sound server) and input-linux (no input devices).
# The monitor-qmp line runs only where QEMU lists monitor-qmp, as the
# service decides the same way.
set -eu

tree=$(cd "$(dirname "$0")/../.." && pwd)
[ $# -gt 0 ] || set -- 44 45
lines=$(mktemp)
trap 'rm -f "$lines"' EXIT
(cd "$tree" && cargo run -q -p gse-host --example qemu_lines) > "$lines"

status=0
for release in "$@"; do
    echo "=== Fedora $release"
    podman run --rm -i --device /dev/kvm --cap-add NET_ADMIN -v "$lines:/lines:ro,z" \
        "registry.fedoraproject.org/fedora:$release" sh -s <<'EOF' || status=1
set -u
dnf -y -q install qemu-system-x86-core qemu-img qemu-audio-pa edk2-ovmf passt nftables \
    >/dev/null 2>&1 || { echo "installing the tools failed"; exit 1; }
qemu-system-x86_64 --version | head -1
passt --version | head -1
nft --version
failed=0
check() {
    if [ "$1" = ok ]; then echo "ok   $2"; else echo "FAIL $2"; failed=1; fi
}
qemu-system-x86_64 -audiodev help | grep -qx pa && r=ok || r=fail
check $r "QEMU has the PulseAudio backend"
props=$(qemu-system-x86_64 -object input-linux,help)
for prop in evdev grab_all repeat grab-toggle; do
    echo "$props" | grep -q "$prop=" && r=ok || r=fail
    check $r "input-linux has $prop"
done
qmp_object=$(qemu-system-x86_64 -object help | grep -cx '  *monitor-qmp')

passt_line=$(sed -n 's/^passt //p' /lines)
sed -n 's/^nft //p' /lines > /tmp/ruleset.escaped
printf '%b\n' "$(cat /tmp/ruleset.escaped)" > /tmp/ruleset
nft -c -f /tmp/ruleset > /tmp/nft.out 2>&1 && r=ok || r=fail
check $r "nftables accepts the ruleset"
[ $r = ok ] || sed 's/^/     /' /tmp/nft.out

grep -vE '^(passt|nft) ' /lines | while read -r kind line; do
    if [ "$kind" = window-qmp-object ] && [ "$qmp_object" = 0 ]; then
        echo "skip $kind (this QEMU has no monitor-qmp; the service uses -mon)"
        continue
    fi
    dir=$(mktemp -d)
    chmod 755 "$dir"
    qemu-img create -q -f qcow2 "$dir/disk.qcow2" 1G
    cp /usr/share/edk2/ovmf/OVMF_VARS_4M.qcow2 "$dir/OVMF_VARS.qcow2"
    truncate -s 1M "$dir/seed.img"
    truncate -s 128M "$dir/screen"
    # passt drops to nobody, who must reach the socket's folder.
    $(echo "$passt_line" | sed "s|@PASST@|$dir/passt|") > "$dir/passt.out" 2>&1 &
    passt=$!
    sleep 1
    if ! kill -0 $passt 2>/dev/null; then
        echo "FAIL passt:"
        sed 's/^/     /' "$dir/passt.out"
        exit 1
    fi
    # Fill the placeholders; sound and host input are checked above.
    args=$(echo "$line" \
        | sed "s|@PASST@|$dir/passt|g; s|@DIR@|$dir|g" \
        | sed -E "s|pa,id=audio,[^ ]*|none,id=audio|; s|-object input-linux,[^ ]*||g")
    # shellcheck disable=SC2086
    $args -S > "$dir/out" 2>&1 &
    qemu=$!
    sleep 3
    if kill -0 $qemu 2>/dev/null; then
        echo "ok   $kind"
        kill $qemu
    else
        echo "FAIL $kind:"
        sed 's/^/     /' "$dir/out"
        echo 1 > /tmp/failed
    fi
    wait $qemu 2>/dev/null
    kill $passt 2>/dev/null
    rm -rf "$dir"
done
[ -e /tmp/failed ] && failed=1
exit $failed
EOF
done
exit $status
