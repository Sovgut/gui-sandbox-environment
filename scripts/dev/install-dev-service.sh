#!/bin/sh
# Sets a development machine up to run the working tree's gse-service as the
# gse service. Run once as root:
#
#   pkexec /path/to/tree/scripts/dev/install-dev-service.sh
#
# It installs the development drop-in, gse-dev-install with its polkit
# action (the app's "Install service build" runs it, behind an administrator
# password), installs the current build and restarts the service.
set -eu

tree=$(cd "$(dirname "$0")/../.." && pwd)

install -d -m 0755 /usr/local/libexec
sed "s|@TREE@|$tree|" "$tree/scripts/dev/gse-dev-install" > /usr/local/libexec/gse-dev-install.new
chown root:root /usr/local/libexec/gse-dev-install.new
chmod 0755 /usr/local/libexec/gse-dev-install.new
mv -f /usr/local/libexec/gse-dev-install.new /usr/local/libexec/gse-dev-install
restorecon /usr/local/libexec/gse-dev-install 2>/dev/null || true

install -m 0644 -o root -g root \
    "$tree/scripts/dev/polkit/io.github.sovgut.gse.dev-install.policy" \
    /usr/share/polkit-1/actions/io.github.sovgut.gse.dev-install.policy
install -D -m 0644 -o root -g root \
    "$tree/scripts/dev/systemd/gse.service.d-dev.conf" \
    /etc/systemd/system/gse.service.d/dev.conf
# A guest program beside the service would win over the working tree's.
rm -f /usr/local/sbin/gse-guest

/usr/local/libexec/gse-dev-install
systemctl daemon-reload
systemctl restart gse.service
