#!/bin/sh
# Screenshots every preview state of the app (`GSE_PREVIEW`, see
# crates/gse-app/src/preview.rs) into a folder, each in a hidden nested KWin
# of the design's size, so nothing shows on the desktop and nothing of it
# needs the service.
#
# usage: preview-shots.sh OUT_DIR [STATE...]
# Without states it shoots every one `GSE_PREVIEW=list` names. Run it on
# the host, after `cargo build --release -p gse-app`.
set -eu
out=$1
shift
app=$(cd "$(dirname "$0")/../.." && pwd)/target/release/gse
mkdir -p "$out"
[ $# -gt 0 ] || set -- $(GSE_PREVIEW=list "$app" | cut -f1)
# A settings folder of its own: the welcome tour and a light look of the
# user's must not show.
config=$(mktemp -d)
mkdir -p "$config/gse"
printf 'tour_done = true\nlook = "dark"\n' > "$config/gse/app.toml"
for state in "$@"; do
    dbus-run-session -- kwin_wayland --virtual --no-lockscreen --width 1300 --height 820 \
        --exit-with-session "sh -c 'XDG_CONFIG_HOME=$config GSE_PREVIEW=$state $app \
        >/dev/null 2>&1 & sleep 5; spectacle -b -n -f -o $out/$state.png; sleep 1; kill \$!'" \
        >/dev/null 2>&1 || true
    echo "$out/$state.png"
done
rm -rf "$config"
