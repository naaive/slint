#!/bin/sh
# SPDX-License-Identifier: MIT
#
# Builds and installs the Nimbus desktop.
#
#   data/install.sh [--prefix DIR] [--destdir DIR] [--session-dir DIR] [--uninstall]
#
# PREFIX (default /usr/local), DESTDIR, SESSION_DIR, and PAM_DIR (default /etc/pam.d)
# can also come from the environment.
# Some display managers, such as SDDM, only read /usr/share/wayland-sessions;
# pass --session-dir /usr/share/wayland-sessions for them.

set -eu

PREFIX=${PREFIX:-/usr/local}
DESTDIR=${DESTDIR:-}
SESSION_DIR=${SESSION_DIR:-}
PAM_DIR=${PAM_DIR:-/etc/pam.d}
action=install

usage() {
    sed -n '4,11s/^# \{0,1\}//p' "$0"
}

while [ $# -gt 0 ]; do
    case $1 in
        --prefix) PREFIX=${2:?--prefix needs a directory}; shift 2 ;;
        --prefix=*) PREFIX=${1#*=}; shift ;;
        --destdir) DESTDIR=${2:?--destdir needs a directory}; shift 2 ;;
        --destdir=*) DESTDIR=${1#*=}; shift ;;
        --session-dir) SESSION_DIR=${2:?--session-dir needs a directory}; shift 2 ;;
        --session-dir=*) SESSION_DIR=${1#*=}; shift ;;
        --uninstall) action=uninstall; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "install.sh: unknown option '$1'" >&2; usage >&2; exit 2 ;;
    esac
done

data_dir=$(cd "$(dirname "$0")" && pwd)
workspace=$(dirname "$data_dir")
SESSION_DIR=${SESSION_DIR:-$PREFIX/share/wayland-sessions}
root=$DESTDIR$PREFIX

packages="crates/nimbus-compositor crates/nimbus-shell-host crates/nimbus-session crates/nimbus-portal apps/nimbus-settings apps/nimbus-files apps/nimbus-terminal apps/nimbus-monitor"
binaries="nimbus-compositor nimbus-shell nimbus-session nimbusctl nimbus-portal nimbus-settings nimbus-files nimbus-terminal nimbus-monitor"
apps="org.nimbus.Settings org.nimbus.Files org.nimbus.Terminal org.nimbus.Monitor"
portal_service=org.freedesktop.impl.portal.desktop.nimbus.service

if [ "$action" = uninstall ]; then
    for bin in $binaries; do rm -f "$root/bin/$bin"; done
    for app in $apps; do rm -f "$root/share/applications/$app.desktop"; done
    rm -f "$DESTDIR$SESSION_DIR/nimbus.desktop" \
        "$root/share/xdg-desktop-portal/nimbus-portals.conf" \
        "$root/share/xdg-desktop-portal/portals/nimbus.portal" \
        "$root/share/dbus-1/services/$portal_service" \
        "$root/lib/systemd/user/nimbus-session.target"
    if cmp -s "$data_dir/pam.d/nimbus" "$DESTDIR$PAM_DIR/nimbus"; then
        rm -f "$DESTDIR$PAM_DIR/nimbus"
    fi
    rm -rf "$root/share/nimbus"
    echo "Removed Nimbus from $root."
    exit 0
fi

command -v cargo >/dev/null 2>&1 || { echo "install.sh: cargo isn't installed; see https://rustup.rs" >&2; exit 1; }

# Reuse the workspace's build directory so reinstalling doesn't rebuild from scratch.
target_dir=$(cargo metadata --manifest-path "$workspace/Cargo.toml" --format-version 1 --no-deps \
    | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')

mkdir -p "$root"
for package in $packages; do
    echo "Installing $package"
    cargo install --locked --force --path "$workspace/$package" --root "$root" ${target_dir:+--target-dir "$target_dir"}
done
# cargo install records what it installed next to bin/; package managers and uninstall don't need it.
rm -f "$root/.crates.toml" "$root/.crates2.json"

install -Dm644 "$data_dir/nimbus.desktop" "$DESTDIR$SESSION_DIR/nimbus.desktop"
install -Dm644 "$data_dir/nimbus-portals.conf" "$root/share/xdg-desktop-portal/nimbus-portals.conf"
install -Dm644 "$data_dir/nimbus.portal" "$root/share/xdg-desktop-portal/portals/nimbus.portal"
mkdir -p "$root/share/dbus-1/services"
sed "s|^Exec=@bindir@|Exec=$PREFIX/bin|" "$data_dir/dbus-1/$portal_service" > "$root/share/dbus-1/services/$portal_service"
install -Dm644 "$data_dir/systemd/nimbus-session.target" "$root/lib/systemd/user/nimbus-session.target"
install -Dm644 "$data_dir/config.toml" "$root/share/nimbus/config.toml"
for app in $apps; do
    install -Dm644 "$data_dir/applications/$app.desktop" "$root/share/applications/$app.desktop"
done
# The lock screen's PAM service; a distribution's own file takes precedence.
if [ ! -e "$DESTDIR$PAM_DIR/nimbus" ]; then
    install -Dm644 "$data_dir/pam.d/nimbus" "$DESTDIR$PAM_DIR/nimbus"
    # The shipped file includes the "login" service.
    if [ -z "$DESTDIR" ] && [ ! -e "$PAM_DIR/login" ]; then
        echo "install.sh: $PAM_DIR/nimbus includes 'login', which doesn't exist; edit it so the lock screen can authenticate" >&2
    fi
fi

if [ -z "$DESTDIR" ] && command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database -q "$PREFIX/share/applications" || true
fi

echo "Installed Nimbus into $root."
echo "Select \"Nimbus\" in your display manager, or run nimbus-session from a TTY."
echo "An example configuration is in $PREFIX/share/nimbus/config.toml."
