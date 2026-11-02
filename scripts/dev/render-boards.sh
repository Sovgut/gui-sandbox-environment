#!/bin/sh
# Renders the design boards (.working/design/project/NAME.dc.html) to
# OUT_DIR/NAME.png at 1280x800 with a headless Chromium. It renders offline:
# every host name fails to resolve, so the boards' web font does not load
# and Inter comes from the system, the same font files the app draws with.
#
# usage: render-boards.sh OUT_DIR [BOARD...]
#
# BOARD is a name such as Main, or a board's file; without boards it renders
# every one. It prints each PNG it writes, reports a board that fails on
# stderr and exits 1 when any did. The browser is Fedora's chromium-headless
# or chromium when installed, else the newest Playwright headless shell in
# ~/.cache/ms-playwright. Run it on the host.
set -eu

usage() {
    echo "usage: $0 OUT_DIR [BOARD...]" >&2
    exit 2
}

[ $# -ge 1 ] || usage
out=$1
shift
boards=$(cd "$(dirname "$0")/../.." && pwd)/.working/design/project
[ -d "$boards" ] || { echo "no boards in $boards" >&2; exit 1; }

# The browser, and whether it needs --headless (a headless shell does not).
browser=/usr/lib64/chromium-browser/headless_shell
headless=
if [ ! -x "$browser" ]; then
    browser=
    for name in chromium-browser chromium; do
        if command -v "$name" >/dev/null 2>&1; then
            browser=$(command -v "$name")
            headless=--headless
            break
        fi
    done
fi
if [ -z "$browser" ]; then
    browser=$(printf '%s\n' "$HOME"/.cache/ms-playwright/chromium_headless_shell-*/chrome-headless-shell-linux64/chrome-headless-shell |
        sort -V | tail -n 1)
    [ -x "$browser" ] || browser=
fi
if [ -z "$browser" ]; then
    echo "no headless Chromium: sudo dnf install chromium-headless" >&2
    exit 1
fi

mkdir -p "$out"
out=$(cd "$out" && pwd)
if [ $# -eq 0 ]; then
    set -- "$boards"/*.dc.html
fi

failed=
for board in "$@"; do
    name=$(basename "$board" .html)
    name=${name%.dc}
    file=$boards/$name.dc.html
    png=$out/$name.png
    rm -f "$png"
    # shellcheck disable=SC2086 # $headless is one flag or nothing
    if [ -f "$file" ] && "$browser" $headless --hide-scrollbars \
        --window-size=1280,800 --host-resolver-rules='MAP * ~NOTFOUND' \
        --virtual-time-budget=3000 --screenshot="$png" "file://$file" \
        >/dev/null 2>&1 && [ -s "$png" ]; then
        echo "$png"
    else
        echo "render-boards.sh: $name failed" >&2
        failed="$failed $name"
    fi
done
if [ -n "$failed" ]; then
    echo "render-boards.sh: failed:$failed" >&2
    exit 1
fi
