#!/bin/sh
# SPDX-License-Identifier: MIT
#
# Collects what a Nimbus bug report needs into one archive.
#
#   data/nimbus-bug-report.sh [--stdout] [--since TIME] [--output FILE]
#
# --stdout prints the report instead of writing an archive.
# --since limits journal logs (default: "-2h"; any journalctl --since value).
# Passwords, tokens, keys, and Wi-Fi secrets are redacted; read the report before sharing it.

set -u

since=-2h
output=nimbus-report-$(date +%Y%m%d-%H%M%S).tar.gz
to_stdout=0

usage() {
    sed -n '4,10s/^# \{0,1\}//p' "$0"
}

while [ $# -gt 0 ]; do
    case $1 in
        --stdout) to_stdout=1; shift ;;
        --since) since=${2:?--since needs a time}; shift 2 ;;
        --since=*) since=${1#*=}; shift ;;
        --output) output=${2:?--output needs a file}; shift 2 ;;
        --output=*) output=${1#*=}; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "nimbus-bug-report: unknown option '$1'" >&2; usage >&2; exit 2 ;;
    esac
done

work=$(mktemp -d "${TMPDIR:-/tmp}/nimbus-report.XXXXXX") || exit 1
trap 'rm -rf "$work"' EXIT INT TERM
report=$work/nimbus-report

mkdir -p "$report"

have() {
    command -v "$1" >/dev/null 2>&1
}

# Removes secrets from standard input.
redact() {
    sed -E \
        -e 's/([A-Za-z0-9_.-]*(psk|password|passwd|passphrase|secret|token|api[_-]?key|private[_-]?key|cookie|credentials?)["'"'"']?[[:space:]]*[:=][[:space:]]*)[^[:space:],;}]+/\1<redacted>/Ig' \
        -e 's/(Bearer|Basic)[[:space:]]+[A-Za-z0-9._~+\/=-]+/\1 <redacted>/g' \
        -e 's#(https?://)[^/@[:space:]]+@#\1<redacted>@#g' \
        -e 's/\b[0-9a-fA-F]{32,}\b/<redacted-hex>/g'
}

# Runs a command with a timeout and writes its redacted output to a file in the report.
collect() {
    name=$1
    shift
    {
        echo "\$ $*"
        if have "$1"; then
            timeout 20 "$@" 2>&1
            echo "(exit status $?)"
        else
            echo "($1 not installed)"
        fi
    } | redact >"$report/$name.txt"
}

# Copies a file into the report, redacted.
collect_file() {
    name=$1
    path=$2
    if [ -r "$path" ]; then
        redact <"$path" >"$report/$name"
    else
        echo "($path not readable)" >"$report/$name"
    fi
}

# Versions and system.
{
    echo "date: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo "kernel: $(uname -srvm)"
    [ -r /etc/os-release ] && sed -n 's/^PRETTY_NAME=//p' /etc/os-release | tr -d '"' | sed 's/^/os: /'
    for bin in nimbus-session nimbus-compositor nimbus-shell nimbusctl nimbus-portal nimbus-settings nimbus-files; do
        if have "$bin"; then
            echo "$bin: $(command -v "$bin") $("$bin" --version 2>/dev/null | head -n1)"
        else
            echo "$bin: not found"
        fi
    done
    for bin in xwayland-satellite Xwayland xdg-desktop-portal xdg-desktop-portal-wlr pactl fcitx5 ibus-daemon gnome-keyring-daemon udisksctl nmcli bluetoothctl; do
        if have "$bin"; then echo "$bin: $(command -v "$bin")"; else echo "$bin: not found"; fi
    done
    have xwayland-satellite && echo "xwayland-satellite version: $(xwayland-satellite --version 2>&1 | head -n1)"
    [ -x /usr/lib/polkit-1/polkit-agent-helper-1 ] || [ -x /usr/libexec/polkit-agent-helper-1 ] \
        && echo "polkit-agent-helper-1: found" || echo "polkit-agent-helper-1: not found in the usual paths"
} | redact >"$report/versions.txt"

# Session environment, limited to the variables Nimbus reads or sets.
env | grep -E '^(WAYLAND_DISPLAY|DISPLAY|NIMBUS_[A-Z_]*|XDG_[A-Z_]*|DBUS_SESSION_BUS_ADDRESS|SLINT_[A-Z_]*|RUST_LOG|LANG|LC_[A-Z]*|XMODIFIERS|GTK_IM_MODULE|QT_IM_MODULE|GDK_BACKEND|QT_QPA_PLATFORM|MOZ_ENABLE_WAYLAND)=' \
    | sort | redact >"$report/environment.txt"

# Compositor state.
collect nimbusctl-state nimbusctl state
collect nimbusctl-state-json nimbusctl state --json
collect nimbusctl-status nimbusctl status

# Configuration.
config_dir=${XDG_CONFIG_HOME:-$HOME/.config}
collect_file config.toml "$config_dir/nimbus/config.toml"
collect_file mimeapps.list "$config_dir/mimeapps.list"
# The titlebar font: [appearance] font_family as fc-match resolves it.
font=$(sed -n 's/^[[:space:]]*font_family[[:space:]]*=[[:space:]]*"\(.*\)".*/\1/p' "$config_dir/nimbus/config.toml" 2>/dev/null | head -n1)
collect fonts fc-match "${font:-Inter}"

# Logs.
collect journal-user journalctl --user -b --since "$since" --no-pager -o short-precise
collect journal-nimbus journalctl -b --since "$since" --no-pager -o short-precise _UID="$(id -u)"
collect journal-logind journalctl -b --since "$since" --no-pager -u systemd-logind -u polkit -u udisks2 -u NetworkManager -u bluetooth
collect journal-portal journalctl --user -b --since "$since" --no-pager -u xdg-desktop-portal -u xdg-desktop-portal-wlr
collect coredumps coredumpctl list --no-pager nimbus-compositor nimbus-shell nimbus-session nimbus-settings nimbus-files
for log in "$HOME/nimbus-tty.log" "$HOME/.local/share/sddm/wayland-session.log"; do
    [ -r "$log" ] && collect_file "$(basename "$log")" "$log"
done

# GPU and DRM.
collect lspci-gpu sh -c 'lspci -nnk | grep -A3 -Ei "vga|3d|display"'
collect drm-connectors sh -c 'for c in /sys/class/drm/card*-*; do printf "%s %s %s\n" "${c##*/}" "$(cat "$c/status" 2>/dev/null)" "$(cat "$c/enabled" 2>/dev/null)"; done'
collect drm-devices ls -l /dev/dri
collect glxinfo glxinfo -B
collect eglinfo eglinfo -B
collect vulkaninfo vulkaninfo --summary
collect drm-info drm_info -j
collect libinput libinput list-devices
collect loginctl-session sh -c 'loginctl show-session "${XDG_SESSION_ID:-auto}"; loginctl seat-status seat0 --no-pager'
collect inhibitors systemd-inhibit --list --no-pager
collect logind-sleep busctl get-property org.freedesktop.login1 /org/freedesktop/login1 org.freedesktop.login1.Manager \
    InhibitDelayMaxUSec HandleLidSwitch HandleLidSwitchExternalPower HandleLidSwitchDocked HandleSuspendKey
collect_file mem_sleep /sys/power/mem_sleep
collect battery upower --dump

# Portals and D-Bus services.
collect portal-status systemctl --user status --no-pager xdg-desktop-portal xdg-desktop-portal-wlr
collect portal-config sh -c 'ls -l /usr/share/xdg-desktop-portal /usr/local/share/xdg-desktop-portal /usr/share/xdg-desktop-portal/portals /usr/local/share/xdg-desktop-portal/portals 2>&1'
collect bus-names busctl --user list --no-pager
collect udisks udisksctl status
collect udisks-dump udisksctl dump
collect sound pactl info
collect network nmcli -t -f DEVICE,TYPE,STATE general status
collect network-devices nmcli -t -f DEVICE,TYPE,STATE device
collect bluetooth bluetoothctl show

if [ "$to_stdout" = 1 ]; then
    for file in "$report"/*; do
        echo "===== ${file##*/}"
        cat "$file"
    done
    exit 0
fi

tar -czf "$output" -C "$work" nimbus-report || exit 1
echo "Wrote $output; read it before sharing, since it lists your devices and installed software."
