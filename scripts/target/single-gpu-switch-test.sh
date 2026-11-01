#!/usr/bin/env bash
# Exercises the single-GPU switch on a machine whose only GPU drives the desktop,
# without starting a VM: stop the desktop, unload the NVIDIA driver, bind the
# GPU and its audio function to vfio-pci, hold, then reverse every step and
# bring the desktop back. Every step is timed and logged.
#
# The screen stays black for about 20 seconds. Open windows are closed, so save
# your work first. A failsafe timer reboots the machine unless the desktop is
# back within FAILSAFE_SECONDS (default 60) of the test starting.
#
# Usage:
#   sudo bash single-gpu-switch-test.sh             run the test
#   sudo DRY_RUN=1 bash single-gpu-switch-test.sh   log the plan, change nothing
#
# Optional: SLOT=0000:09:00 picks the GPU slot; by default it is the NVIDIA
# card that is the boot VGA device. HOLD_SECONDS (default 5) is the time spent
# on vfio-pci.
#
# The log is /var/tmp/gse-switch-test-<time>.log, readable by everyone.

set -u
export LC_ALL=C

DRY_RUN="${DRY_RUN:-0}"
HOLD_SECONDS="${HOLD_SECONDS:-5}"
FAILSAFE_SECONDS="${FAILSAFE_SECONDS:-60}"
UNIT=gse-switch-test
FAILSAFE_UNIT=gse-switch-failsafe

if [ "$(id -u)" -ne 0 ]; then
    echo "run it with sudo" >&2
    exit 1
fi

# Stopping the display manager ends the terminal this was started from, so the
# test itself runs as a transient system service outside the session.
if [ -z "${GJ_DETACHED:-}" ]; then
    self="${BASH_SOURCE[0]}"
    if [ ! -f "$self" ] || ! grep -q 'gse single-GPU switch test' "$self"; then
        echo "run it from a saved file: sudo bash single-gpu-switch-test.sh" >&2
        exit 1
    fi
    stamp="$(date +%Y%m%d-%H%M%S)"
    log="/var/tmp/gse-switch-test-$stamp.log"
    # Stays owned by root and world-readable: with fs.protected_regular, root
    # cannot append to a file in /var/tmp that another user owns.
    install -m 644 /dev/null "$log"
    {
        echo "launcher: $(date -Is), dry run: $DRY_RUN, script: $self"
        echo "launcher: $(sha256sum < "$self" | cut -c1-16), $(grep -c $'\r' "$self") CR characters"
    } >> "$log"
    # Line endings from a messenger or browser would break every line.
    tr -d '\r' < "$self" > /run/gse-switch-test.sh
    chmod 700 /run/gse-switch-test.sh
    systemctl reset-failed "$UNIT.service" "$FAILSAFE_UNIT.timer" 2>/dev/null
    if ! systemd-run --unit "$UNIT" --collect --property=Type=exec \
        --setenv=GJ_DETACHED=1 --setenv=GJ_LOG="$log" \
        --setenv=DRY_RUN="$DRY_RUN" --setenv=HOLD_SECONDS="$HOLD_SECONDS" \
        --setenv=FAILSAFE_SECONDS="$FAILSAFE_SECONDS" --setenv=SLOT="${SLOT:-}" \
        /bin/bash /run/gse-switch-test.sh >> "$log" 2>&1; then
        echo "launcher: systemd-run failed" >> "$log"
        cat "$log"
        exit 1
    fi
    echo "Test started as $UNIT.service, log: $log"
    if [ "$DRY_RUN" = 1 ]; then
        # Nothing stops the terminal in a dry run, so show the result here.
        for _ in $(seq 1 30); do
            systemctl is-active --quiet "$UNIT.service" || break
            sleep 1
        done
        cat "$log"
    else
        echo "The screen goes black in a few seconds and comes back in about 20 seconds."
        echo "If it does not, the machine reboots by itself after $FAILSAFE_SECONDS seconds."
    fi
    exit 0
fi

log="$GJ_LOG"
# A failure here lands in the journal: journalctl -u gse-switch-test
if ! exec >>"$log" 2>&1; then
    echo "cannot write $log" >&2
    exit 1
fi
t0="$(date +%s.%N)"
# Kernel messages are picked by journal cursor, not by time: with the RTC in
# local time (a Windows dual boot), boot messages carry future timestamps.
kcursor="$(journalctl -k -n 0 --show-cursor --no-pager 2>/dev/null | sed -n 's/^-- cursor: //p')"

say() { printf '[%7.2fs] %s\n' "$(awk -v a="$(date +%s.%N)" -v b="$t0" 'BEGIN { print a - b }')" "$*"; }

# Runs a state-changing command, or only logs it in a dry run.
act() {
    if [ "$DRY_RUN" = 1 ]; then
        say "dry-run: $*"
        return 0
    fi
    say "run: $*"
    "$@"
    local rc=$?
    [ "$rc" -eq 0 ] || say "  -> exit $rc"
    return "$rc"
}

write() {
    if [ "$DRY_RUN" = 1 ]; then
        say "dry-run: echo '$1' > $2"
        return 0
    fi
    say "write: '$1' > $2"
    # Always one write with a newline: an empty value written as zero bytes
    # never reaches sysfs, so driver_override would silently stay set.
    printf '%s\n' "$1" > "$2" || { say "  -> write failed"; return 1; }
}

say "gse single-GPU switch test, dry run: $DRY_RUN"
uname -r

# --- target -----------------------------------------------------------------

slot="${SLOT:-}"
if [ -z "$slot" ]; then
    for dev in /sys/bus/pci/devices/*; do
        [ "$(cat "$dev/vendor" 2>/dev/null)" = 0x10de ] || continue
        [ "$(cat "$dev/boot_vga" 2>/dev/null)" = 1 ] || continue
        slot="$(basename "$dev")"
        slot="${slot%.*}"
    done
fi
[ -n "$slot" ] || { say "no NVIDIA boot VGA device found; set SLOT"; exit 1; }
functions=(/sys/bus/pci/devices/"$slot".*)
[ -e "${functions[0]}" ] || { say "no PCI device at $slot"; exit 1; }
group="$(basename "$(readlink "${functions[0]}/iommu_group")")"
say "GPU slot $slot, functions: ${functions[*]##*/}, IOMMU group $group"

nodes() {
    local f n
    for n in /dev/nvidia*; do
        [ -c "$n" ] && echo "$n"
    done
    for f in "${functions[@]}"; do
        for n in "$f"/drm/card* "$f"/drm/renderD*; do
            [ -e "$n" ] && echo "/dev/dri/$(basename "$n")"
        done
    done
}

# One find over every process's descriptors; a per-descriptor loop in bash takes
# seconds, which the failsafe budget cannot afford.
# Prints "pid uid comm" per holder.
holders() {
    local args=() n pid
    while read -r n; do args+=(-o -lname "$n"); done < <(nodes)
    [ "${#args[@]}" -gt 0 ] || return 0
    find /proc/[0-9]*/fd -maxdepth 1 -type l \( "${args[@]:1}" \) 2>/dev/null \
        | cut -d/ -f3 | sort -un | while read -r pid; do
            echo "$pid $(stat -c %u "/proc/$pid" 2>/dev/null) $(cat "/proc/$pid/comm" 2>/dev/null)"
        done
}

# Only processes of regular users are ever signalled. System holders such as
# systemd and systemd-logind keep a session's DRM descriptors and release them
# when the session ends; signalling them would take the whole system down.
user_holders() { holders | awk '$2 >= 1000'; }
system_holders() { holders | awk '$2 < 1000'; }

state() {
    say "state: $1"
    lspci -nnk -s "$slot" | sed 's/^/    /'
    lsmod | grep -E '^(nvidia|vfio_pci|snd_hda_intel) ' | sed 's/^/    /'
    local h
    h="$(holders)"
    if [ -n "$h" ]; then echo "    holders:"; echo "$h" | sed 's/^/      /'; else echo "    holders: none"; fi
}

dm_was_active=0
systemctl is-active --quiet display-manager && dm_was_active=1
gpu_services=()
for s in nvidia-powerd nvidia-persistenced; do
    systemctl is-active --quiet "$s" && gpu_services+=("$s")
done
say "display-manager active: $dm_was_active ($(systemctl show -p Id --value display-manager)); GPU services running: ${gpu_services[*]:-none}"
driver_of() { basename "$(readlink "$1/driver" 2>/dev/null)" 2>/dev/null; }
host_driver="$(driver_of "${functions[0]}")"
say "host driver of the GPU: $host_driver"
state "before"

# --- failsafe ---------------------------------------------------------------

if [ "$DRY_RUN" != 1 ]; then
    systemd-run --quiet --unit "$FAILSAFE_UNIT" --collect --on-active="$FAILSAFE_SECONDS" \
        --timer-property=AccuracySec=1s /usr/bin/systemctl reboot
    say "failsafe: reboot in $FAILSAFE_SECONDS s unless the test finishes"
fi

# --- restore, run on success and on any failure ------------------------------

restored=0
unbound_vtcons=()
restore() {
    [ "$restored" = 1 ] && return
    restored=1
    say "=== restore"
    local f
    for f in "${functions[@]}"; do
        write "" "$f/driver_override"
        if [ -e "$f/driver" ] && [ "$(basename "$(readlink "$f/driver")")" = vfio-pci ]; then
            write "$(basename "$f")" "$f/driver/unbind"
        fi
    done
    act modprobe nvidia
    act modprobe nvidia_modeset
    act modprobe nvidia_drm
    act modprobe nvidia_uvm
    for f in "${functions[@]}"; do
        [ -e "$f/driver" ] || write "$(basename "$f")" /sys/bus/pci/drivers_probe
    done
    local v
    for v in "${unbound_vtcons[@]}"; do write 1 "$v/bind"; done
    state "after rebind"
    local s
    for s in "${gpu_services[@]}"; do act systemctl start "$s"; done
    if [ "$dm_was_active" = 1 ]; then
        act systemctl start display-manager
        local _
        for _ in $(seq 1 30); do
            systemctl is-active --quiet display-manager && break
            sleep 1
        done
        say "display-manager active: $(systemctl is-active display-manager)"
    fi
    # The failsafe stays armed unless the desktop is really back: the GPU on its
    # host driver and the display manager running. A display manager without
    # the GPU is a black screen.
    if [ "$DRY_RUN" != 1 ]; then
        local back=1
        [ "$(driver_of "${functions[0]}")" = "$host_driver" ] || back=0
        [ "$dm_was_active" = 0 ] || systemctl is-active --quiet display-manager || back=0
        if [ "$back" = 1 ]; then
            systemctl stop "$FAILSAFE_UNIT.timer" 2>/dev/null
            say "failsafe cancelled"
        else
            say "GPU on '$(driver_of "${functions[0]}")', not '$host_driver', or display-manager down; leaving the failsafe reboot armed"
        fi
    fi
    say "=== kernel log during the test"
    journalctl -k --no-pager -o short-monotonic ${kcursor:+--after-cursor="$kcursor"} 2>/dev/null \
        | grep -iE 'nvidia|nvrm|vfio|vgaarb|fbcon|drm|snd_hda|pci 0000:'"${slot#0000:}" | tail -80
    say "done"
}
trap restore EXIT

fail() { say "FAILED: $*"; state "at failure"; exit 1; }

# --- stop the desktop ---------------------------------------------------------

wait_until_empty() {
    local what="$1" tries="$2"
    [ "$DRY_RUN" = 1 ] && return
    for _ in $(seq 1 "$tries"); do
        [ -z "$("$what")" ] && return
        sleep 0.5
    done
}

# Graphical sessions on any seat, as "session user". Stopping the display
# manager leaves them running: with plasmalogin the user's KWin keeps going on
# its VT. KWin and plasmashell are units of the user's systemd instance, not of
# the session scope, so ending the session alone leaves them up for seconds,
# and a KWin that is killed instead comes back through its wrapper.
graphical_sessions() {
    local s type
    for s in $(loginctl list-sessions --no-legend | awk '{print $1}'); do
        type="$(loginctl show-session "$s" -p Type --value 2>/dev/null)"
        case "$type" in
            wayland|x11|mir) echo "$s $(loginctl show-session "$s" -p Name --value 2>/dev/null)" ;;
        esac
    done
}

say "=== stop"
sessions="$(graphical_sessions)"
say "graphical sessions: $(echo "${sessions:-none}" | tr '\n' ' ')"
if [ "$dm_was_active" = 1 ]; then
    act systemctl stop display-manager || fail "display-manager did not stop"
fi
while read -r s user; do
    [ -n "$s" ] || continue
    act systemctl --user --machine="$user@" stop graphical-session.target
    act loginctl terminate-session "$s"
done <<< "$sessions"
for s in "${gpu_services[@]}"; do act systemctl stop "$s"; done

# Ending the session stops the compositor and everything drawn through it.
# Whatever is left after that is signalled, and its unit is logged so a
# respawning one can be found.
wait_until_empty user_holders 20
for sig in TERM KILL; do
    h="$(user_holders)"
    [ -n "$h" ] || break
    say "user processes still holding the GPU, sending SIG$sig:"
    echo "$h" | while read -r pid _ comm; do
        echo "    $pid $comm $(sed 's/^0:://' "/proc/$pid/cgroup" 2>/dev/null)"
    done
    for pid in $(echo "$h" | awk '{print $1}'); do act kill -s "$sig" "$pid"; done
    wait_until_empty user_holders 6
done
wait_until_empty system_holders 10
h="$(system_holders)"
if [ -n "$h" ]; then
    say "system processes still holding the GPU (not signalled):"
    echo "$h" | sed 's/^/    /'
fi

# A text console on the NVIDIA framebuffer pins nvidia_drm.
for v in /sys/class/vtconsole/vtcon*; do
    [ -e "$v" ] || continue
    say "$(basename "$v"): bind=$(cat "$v/bind") $(cat "$v/name")"
    if [ "$(cat "$v/bind")" = 1 ] && grep -qi 'frame buffer' "$v/name"; then
        write 0 "$v/bind" && unbound_vtcons+=("$v")
    fi
done
for m in nvidia_drm nvidia_modeset nvidia_uvm nvidia; do
    say "$m refcnt: $(cat /sys/module/$m/refcnt 2>/dev/null)"
done
state "desktop stopped"

# --- unload the host driver and bind vfio-pci ---------------------------------

act modprobe -r nvidia_drm nvidia_modeset nvidia_uvm nvidia || fail "NVIDIA modules did not unload"
act modprobe vfio-pci || fail "vfio-pci did not load"
for f in "${functions[@]}"; do
    write vfio-pci "$f/driver_override" || fail "driver_override on $f"
    if [ -e "$f/driver" ]; then
        write "$(basename "$f")" "$f/driver/unbind" || fail "unbind $f"
    fi
    write "$(basename "$f")" /sys/bus/pci/drivers_probe || fail "probe $f"
done
state "on vfio-pci"
[ "$DRY_RUN" = 1 ] || [ -e "/dev/vfio/$group" ] || fail "/dev/vfio/$group missing"
say "/dev/vfio/$group present; holding for $HOLD_SECONDS s"
[ "$DRY_RUN" = 1 ] || sleep "$HOLD_SECONDS"

say "=== switch succeeded, returning the GPU"
exit 0
