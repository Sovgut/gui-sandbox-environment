#!/bin/sh
# Builds target/appimage/gse-x86_64.AppImage: the app, the service and the
# guest binary the service puts into images, with the files setup installs.
# APPIMAGETOOL names the appimagetool program to pack it with.
set -eu
cd "$(dirname "$0")/../.."
: "${APPIMAGETOOL:?set APPIMAGETOOL to the appimagetool program}"

cargo build --release --locked -p gse-app -p gse-service -p gse-guest

out=target/appimage
dir=$out/AppDir
rm -rf "$out"
install -D -m 0755 target/release/gse "$dir/usr/bin/gse"
install -D -m 0755 target/release/gse-service "$dir/usr/libexec/gse/gse-service"
install -D -m 0755 target/release/gse-guest "$dir/usr/libexec/gse/gse-guest"
install -D -m 0644 dist/systemd/gse.service "$dir/usr/share/gse/systemd/gse.service"
install -D -m 0644 dist/polkit-1/actions/io.github.sovgut.gse.policy \
    "$dir/usr/share/gse/polkit-1/actions/io.github.sovgut.gse.policy"
install -D -m 0644 dist/sysusers.d/gse.conf "$dir/usr/share/gse/sysusers.d/gse.conf"
install -D -m 0644 dist/gse/gse.toml "$dir/usr/share/gse/gse.toml"
install -m 0755 dist/appimage/AppRun "$dir/AppRun"
install -m 0644 dist/appimage/gse.desktop "$dir/gse.desktop"
install -m 0644 dist/appimage/gse.svg "$dir/gse.svg"
ln -s gse.svg "$dir/.DirIcon"

ARCH=x86_64 "$APPIMAGETOOL" --no-appstream "$dir" "$out/gse-x86_64.AppImage"
echo "built $out/gse-x86_64.AppImage"
