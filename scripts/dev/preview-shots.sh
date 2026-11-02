#!/bin/sh
# Screenshots preview states of the app (`GSE_PREVIEW`, see
# crates/gse-app/src/preview.rs) into OUT_DIR/STATE.png, each exactly the
# window, through preview-session.sh: a hidden nested KWin per state, so
# nothing shows on the desktop and nothing of it needs the service.
#
# usage: preview-shots.sh OUT_DIR [--size WxH] [--look light|dark] [STATE...]
#
#   --size WxH   window size (default 1280x800, the design's size); a
#                height below 760 gives the compact pages
#   --look       GSE_PREVIEW_LOOK for the app (default dark)
#
# Without states it shoots every one `GSE_PREVIEW=list` names. It prints
# each PNG it writes; a state that fails is reported on stderr, the rest
# still run, and the script exits 1. Run it on the host, after
# `cargo build --release -p gse-app`.
set -eu

usage() {
    echo "usage: $0 OUT_DIR [--size WxH] [--look light|dark] [STATE...]" >&2
    exit 2
}

[ $# -ge 1 ] || usage
out=$1
shift
size=1280x800
look=dark
while [ $# -gt 0 ]; do
    case $1 in
        --size) [ $# -ge 2 ] || usage; size=$2; shift 2 ;;
        --look) [ $# -ge 2 ] || usage; look=$2; shift 2 ;;
        --) shift; break ;;
        -*) usage ;;
        *) break ;;
    esac
done
case $size in *x*) ;; *) usage ;; esac
case ${size%x*} in '' | *[!0-9]*) usage ;; esac
case ${size#*x} in '' | *[!0-9]*) usage ;; esac
case $look in light | dark) ;; *) usage ;; esac

here=$(cd "$(dirname "$0")" && pwd)
app=$(cd "$here/../.." && pwd)/target/release/gse
if [ ! -x "$app" ]; then
    echo "$app is missing: cargo build --release -p gse-app" >&2
    exit 1
fi
mkdir -p "$out"
out=$(cd "$out" && pwd)
# shellcheck disable=SC2046 # one state per word
[ $# -gt 0 ] || set -- $(GSE_PREVIEW=list "$app" | cut -f1)

log=$(mktemp)
trap 'rm -f "$log"' EXIT
failed=
for state in "$@"; do
    png=$out/$state.png
    rm -f "$png"
    if "$here/preview-session.sh" --size "$size" --look "$look" "$state" \
        shot "$png" 2>"$log" && [ -s "$png" ]; then
        echo "$png"
    else
        echo "preview-shots.sh: $state failed:" >&2
        cat "$log" >&2
        failed="$failed $state"
    fi
done
if [ -n "$failed" ]; then
    echo "preview-shots.sh: failed:$failed" >&2
    exit 1
fi
