#!/bin/sh
# Runs the app in one preview state (`GSE_PREVIEW`, see
# crates/gse-app/src/preview.rs) in a hidden nested KWin and feeds it the
# actions of preview-drive.py: pointer, keys, window frame, screenshots.
# The nested KWin has its own Wayland socket and its own DBus session, so
# nothing of it shows on the desktop or reaches the desktop's apps.
#
# usage: preview-session.sh [--size WxH] [--look light|dark] [--wait S]
#                           STATE [ACTION...]
#
#   --size WxH   the nested screen and the window frame (default 1280x800,
#                the design's size); the frame fills the screen from 0,0, so
#                a `shot` is exactly the window
#   --look       GSE_PREVIEW_LOOK for the app (default dark)
#   --wait S     seconds between the window taking its size and the first
#                action (default 1.5)
#
# Example: preview-session.sh main click 1152 534 sleep 0.5 shot /tmp/menu.png
#
# Exits non-zero, with the app's output on stderr, when the app does not
# start or dies, its window does not show within 10 s, or an action fails.
# Run it on the host, after `cargo build --release -p gse-app`.
set -eu

usage() {
    echo "usage: $0 [--size WxH] [--look light|dark] [--wait S] STATE [ACTION...]" >&2
    exit 2
}

here=$(cd "$(dirname "$0")" && pwd)

# The part that runs inside the nested session, started by KWin itself.
if [ "${1-}" = --nested ]; then
    work=$GSE_SESSION_WORK
    fail() {
        echo "$1" > "$work/status"
        [ -z "${pid-}" ] || kill "$pid" 2>/dev/null || true
        exit 0
    }
    # Services the bus starts from here on (the portals the app asks for)
    # draw on the nested KWin, and the outer part can find them by the bus.
    echo "$DBUS_SESSION_BUS_ADDRESS" > "$work/bus"
    dbus-update-activation-environment WAYLAND_DISPLAY >/dev/null 2>&1 ||
        fail "the nested bus did not take the nested WAYLAND_DISPLAY"
    if ! qdbus-qt6 org.kde.KWin /Scripting org.kde.kwin.Scripting.loadScript \
        "$work/fit.js" gse-preview-fit >/dev/null ||
        ! qdbus-qt6 org.kde.KWin /Scripting org.kde.kwin.Scripting.start >/dev/null; then
        fail "the window-size script did not load into KWin"
    fi
    NO_COLOR=1 XDG_CONFIG_HOME=$work/config GSE_PREVIEW=$GSE_SESSION_STATE "$GSE_SESSION_APP" \
        >"$work/app.log" 2>&1 &
    pid=$!
    tries=0
    until grep -q '^js: gse-preview fitted' "$work/kwin.log" 2>/dev/null; do
        kill -0 "$pid" 2>/dev/null || { pid=; fail "the app exited before its window showed"; }
        tries=$((tries + 1))
        [ "$tries" -lt 100 ] || fail "the app's window did not show within 10 s"
        sleep 0.1
    done
    sleep "$GSE_SESSION_WAIT"
    # shellcheck disable=SC1091 # written by the outer part
    . "$work/actions"
    GSE_SESSION_BUS=$DBUS_SESSION_BUS_ADDRESS \
        python3 "$here/preview-drive.py" "$@" 2>"$work/drive.log" ||
        fail "an action failed"
    kill -0 "$pid" 2>/dev/null || { pid=; fail "the app died during the actions"; }
    kill "$pid"
    echo ok > "$work/status"
    exit 0
fi

size=1280x800
look=dark
wait=1.5
while [ $# -gt 0 ]; do
    case $1 in
        --size) [ $# -ge 2 ] || usage; size=$2; shift 2 ;;
        --look) [ $# -ge 2 ] || usage; look=$2; shift 2 ;;
        --wait) [ $# -ge 2 ] || usage; wait=$2; shift 2 ;;
        --) shift; break ;;
        -*) usage ;;
        *) break ;;
    esac
done
[ $# -ge 1 ] || usage
state=$1
shift
case $size in *x*) ;; *) usage ;; esac
width=${size%x*}
height=${size#*x}
case $width in '' | *[!0-9]*) usage ;; esac
case $height in '' | *[!0-9]*) usage ;; esac
case $look in light | dark) ;; *) usage ;; esac

app=$(cd "$here/../.." && pwd)/target/release/gse
if [ ! -x "$app" ]; then
    echo "$app is missing: cargo build --release -p gse-app" >&2
    exit 1
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
# A settings folder of its own: the welcome tour and the user's settings
# must not show.
mkdir -p "$work/config/gse"
printf 'tour_done = true\n' > "$work/config/gse/app.toml"

# Gives the app's window its frame once it shows; the print is what the
# nested part waits for.
cat > "$work/fit.js" <<EOF
function fit(window) {
    if (window.resourceClass != "io.github.sovgut.gse" || !window.normalWindow)
        return;
    window.frameGeometry = {x: 0, y: 0, width: $width, height: $height};
    print("gse-preview fitted");
}
workspace.windowAdded.connect(fit);
workspace.windowList().forEach(fit);
EOF

# The actions, quoted for the nested part to read back as its arguments.
{
    printf 'set --'
    for action in "$@"; do
        printf " '%s'" "$(printf '%s' "$action" | sed "s/'/'\\\\''/g")"
    done
    echo
} > "$work/actions"

# The nested bus starts its services with this environment: without the
# desktop's display they cannot reach the desktop. KWin's output goes to a
# file, never a pipe: services started on the nested bus keep a pipe open
# after KWin exits.
unset WAYLAND_DISPLAY DISPLAY
GSE_SESSION_WORK=$work GSE_SESSION_STATE=$state GSE_PREVIEW_LOOK=$look \
    GSE_SESSION_WAIT=$wait GSE_SESSION_APP=$app \
    QT_FORCE_STDERR_LOGGING=1 QT_LOGGING_RULES='js.debug=true' \
    dbus-run-session -- kwin_wayland --virtual --no-lockscreen \
    --width "$width" --height "$height" \
    --exit-with-session "$here/$(basename "$0") --nested" \
    >"$work/kwin.log" 2>&1 || true

# Services the nested bus started outlive it; stop every process still
# holding its address.
bus=$(cat "$work/bus" 2>/dev/null || true)
if [ -n "$bus" ]; then
    for environ in /proc/[0-9]*/environ; do
        [ -r "$environ" ] || continue
        if tr '\0' '\n' 2>/dev/null <"$environ" |
            grep -qxF "DBUS_SESSION_BUS_ADDRESS=$bus"; then
            process=${environ#/proc/}
            kill "${process%/environ}" 2>/dev/null || true
        fi
    done
fi

status=$(cat "$work/status" 2>/dev/null || echo "the nested KWin did not start")
[ -s "$work/drive.log" ] && cat "$work/drive.log" >&2
if [ "$status" != ok ]; then
    echo "preview-session.sh: $state: $status" >&2
    if [ "$status" != "an action failed" ] && [ -s "$work/app.log" ]; then
        echo "--- app output" >&2
        tail -n 20 "$work/app.log" >&2
    fi
    if [ ! -e "$work/status" ]; then
        echo "--- KWin output" >&2
        tail -n 20 "$work/kwin.log" >&2
    fi
    exit 1
fi
