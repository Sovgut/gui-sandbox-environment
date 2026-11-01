#!/bin/bash
# Waits for a development environment to come up and prints the outcome:
# "ready" once the guest's GSE-READY line is on its serial console (or, for
# images built before the marker existed, once the desktop answers over the
# development SSH), "exited" when QEMU goes away first, "timeout" after the
# limit. Meant to run in the background while someone starts the environment.
#
# usage: wait-environment.sh <environment dir> [ssh port] [limit seconds]

set -u
dir=${1:?environment directory}
port=${2:-2222}
limit=${3:-900}
here=$(cd "$(dirname "$0")" && pwd)
key=$here/../../.working/vm/proto/id_ed25519
console=$dir/console.log

ssh_ready() {
    ssh -i "$key" -o UserKnownHostsFile=/dev/null -o StrictHostKeyChecking=no \
        -o LogLevel=ERROR -o ConnectTimeout=3 -p "$port" dev@127.0.0.1 \
        'sudo test -S /run/user/$(id -u sandbox)/wayland-0' 2>/dev/null
}

# QEMU truncates the console when it starts, so a marker from the last run
# does not count until a new QEMU is up.
start=$SECONDS
until pgrep -f '[q]emu-system-x86_64 -name' >/dev/null; do
    [ $((SECONDS - start)) -ge "$limit" ] && { echo timeout; exit 1; }
    sleep 2
done
sleep 3
while pgrep -f '[q]emu-system-x86_64 -name' >/dev/null; do
    if grep -aq GSE-READY "$console" 2>/dev/null || ssh_ready; then
        echo ready
        exit 0
    fi
    [ $((SECONDS - start)) -ge "$limit" ] && { echo timeout; exit 1; }
    sleep 3
done
echo exited
tail -5 "$console" 2>/dev/null
exit 1
