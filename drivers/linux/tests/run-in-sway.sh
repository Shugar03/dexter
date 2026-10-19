#!/usr/bin/env bash
set -euo pipefail

export PATH="/usr/libexec:$PATH"

if [[ ${DEXTER_SWAY_TEST_SESSION:-0} != 1 ]]; then
    export DEXTER_SWAY_TEST_SESSION=1
    exec dbus-run-session -- "$0" "$@"
fi

if (($# < 3)); then
    printf 'usage: %s <output-scale> -- <cmd...>\n' "$0" >&2
    exit 2
fi

output_scale=$1
shift
if [[ $1 != -- ]]; then
    printf 'expected -- before command\n' >&2
    exit 2
fi
shift
if (($# == 0)); then
    printf 'command is required\n' >&2
    exit 2
fi
if [[ ! $output_scale =~ ^[0-9]+([.][0-9]+)?$ ]] || ! awk "BEGIN { exit !($output_scale > 0) }"; then
    printf 'invalid output scale: %s\n' "$output_scale" >&2
    exit 2
fi
for component in pipewire sway xdg-desktop-portal xdg-desktop-portal-wlr; do
    command -v "$component" >/dev/null || {
        printf 'required command is missing: %s\n' "$component" >&2
        exit 1
    }
done
for component in awk dbus-update-activation-environment find fusermount3 gdbus pgrep python3 rg swaymsg xdpyinfo; do
    command -v "$component" >/dev/null || {
        printf 'required command is missing: %s\n' "$component" >&2
        exit 1
    }
done

portal_dir=/usr/share/xdg-desktop-portal/portals
wlr_portal=$portal_dir/wlr.portal
gtk_portal=$portal_dir/gtk.portal
if [[ ! -r $wlr_portal ]] || ! rg -q 'UseIn=.*(sway|wlroots)' "$wlr_portal"; then
    printf 'WLR portal metadata does not advertise Sway: %s\n' "$wlr_portal" >&2
    exit 1
fi
if [[ -r $gtk_portal ]] && rg -q 'UseIn=.*sway' "$gtk_portal"; then
    printf 'GTK portal metadata also matches Sway; backend selection is ambiguous\n' >&2
    exit 1
fi

runtime_dir=$(mktemp -d "${TMPDIR:-/tmp}/dexter-sway-runtime.XXXXXX")
chmod 700 "$runtime_dir"
export XDG_RUNTIME_DIR=$runtime_dir
export XDG_CONFIG_HOME=$runtime_dir/config
export XDG_DATA_HOME=$runtime_dir/data
export XDG_CURRENT_DESKTOP=sway
export XDG_SESSION_TYPE=wayland
export XDG_SESSION_DESKTOP=sway
export WLR_BACKENDS=headless
export WLR_LIBINPUT_NO_DEVICES=1
unset WAYLAND_DISPLAY SWAYSOCK DISPLAY XDG_DESKTOP_PORTAL_DIR
unset XAUTHORITY
mkdir -p "$XDG_CONFIG_HOME" "$XDG_DATA_HOME"
mkdir -p "$XDG_CONFIG_HOME/xdg-desktop-portal-wlr"
printf '[screencast]\nchooser_type=none\n' \
    >"$XDG_CONFIG_HOME/xdg-desktop-portal-wlr/sway"

pipewire_pid=
sway_pid=
xdpw_pid=
portal_pid=
stop_activated_portal_services() {
    local pid process_env process_runtime
    for pid in $(pgrep -f '^/usr/libexec/xdg-(desktop-portal|document-portal)' || true); do
        process_env=$(tr '\0' '\n' <"/proc/$pid/environ" 2>/dev/null) || continue
        process_runtime=$(printf '%s\n' "$process_env" | sed -n 's/^XDG_RUNTIME_DIR=//p')
        if [[ $process_runtime == "$runtime_dir" ]]; then
            kill "$pid" 2>/dev/null || true
        fi
    done
}
cleanup() {
    local status=$?
    local pid
    for pid in "$portal_pid" "$xdpw_pid" "$sway_pid"; do
        if [[ -n $pid ]]; then
            kill "$pid" 2>/dev/null || true
            wait "$pid" 2>/dev/null || true
        fi
    done
    stop_activated_portal_services
    if [[ -n $pipewire_pid ]]; then
        kill "$pipewire_pid" 2>/dev/null || true
        wait "$pipewire_pid" 2>/dev/null || true
    fi
    if [[ -d $runtime_dir/doc ]]; then
        fusermount3 -u "$runtime_dir/doc" 2>/dev/null \
            || fusermount3 -uz "$runtime_dir/doc" 2>/dev/null \
            || true
    fi
    if ((status != 0)); then
        local log_directory="${DEXTER_SWAY_LOG_DIR:-/tmp/dexter-sway-logs}/$(basename "$runtime_dir")"
        mkdir -p "$log_directory" 2>/dev/null || true
        cp "$runtime_dir"/*.log "$log_directory/" 2>/dev/null || true
        printf 'Sway test logs (if available): %s\n' "$log_directory" >&2
        for log in "$runtime_dir"/*.log; do
            if [[ -f $log ]]; then
                printf '\n--- %s ---\n' "$(basename "$log")" >&2
                tail -n 100 "$log" >&2
            fi
        done
    fi
    rm -rf "$runtime_dir"
    return "$status"
}
trap cleanup EXIT

pipewire >"$runtime_dir/pipewire.log" 2>&1 &
pipewire_pid=$!
for _ in $(seq 1 100); do
    if ! kill -0 "$pipewire_pid" 2>/dev/null; then
        printf 'PipeWire exited during startup\n' >&2
        cat "$runtime_dir/pipewire.log" >&2
        exit 1
    fi
    [[ -S $runtime_dir/pipewire-0 ]] && break
    sleep 0.1
done
if [[ ! -S $runtime_dir/pipewire-0 ]]; then
    printf 'PipeWire did not create %s/pipewire-0\n' "$runtime_dir" >&2
    exit 1
fi

printf 'output HEADLESS-1 mode 3200x1800\nfor_window [all] floating enable\nxwayland force\n' \
    >"$runtime_dir/sway.conf"
sway -d -c "$runtime_dir/sway.conf" >"$runtime_dir/sway.log" 2>&1 &
sway_pid=$!
for _ in $(seq 1 100); do
    wayland_socket=$(find "$runtime_dir" -maxdepth 1 -type s -name 'wayland-*' -print -quit)
    sway_socket="$runtime_dir/sway-ipc.$UID.$sway_pid.sock"
    if [[ -n $wayland_socket && -S $sway_socket ]]; then
        break
    fi
    if ! kill -0 "$sway_pid" 2>/dev/null; then
        printf 'Sway exited during startup\n' >&2
        exit 1
    fi
    sleep 0.1
done
if [[ -z ${wayland_socket:-} || ! -S $sway_socket ]]; then
    printf 'Sway did not create its Wayland and IPC sockets\n' >&2
    exit 1
fi
export WAYLAND_DISPLAY
WAYLAND_DISPLAY=$(basename "$wayland_socket")
export SWAYSOCK=$sway_socket

swaymsg output HEADLESS-1 mode 3200x1800 >/dev/null
swaymsg output HEADLESS-1 scale "$output_scale" >/dev/null
sway_outputs=$(swaymsg -t get_outputs -r)
if ! printf '%s\n' "$sway_outputs" | python3 -c '
import json
import math
import sys

expected_scale = float(sys.argv[1])
outputs = json.load(sys.stdin)
headless = next((output for output in outputs if output.get("name") == "HEADLESS-1"), None)
if headless is None:
    raise SystemExit("HEADLESS-1 is missing")
mode = headless.get("current_mode") or {}
if (mode.get("width"), mode.get("height")) != (3200, 1800):
    raise SystemExit(f"unexpected mode: {mode}")
if not math.isclose(headless.get("scale", math.nan), expected_scale):
    raise SystemExit(f"unexpected scale: {headless.get('scale')}")
rect = headless.get("rect") or {}
if not (
    math.isclose(rect.get("width", math.nan), 3200 / expected_scale)
    and math.isclose(rect.get("height", math.nan), 1800 / expected_scale)
):
    raise SystemExit(f"unexpected logical output rect: {rect}")
' "$output_scale"; then
    printf 'HEADLESS-1 did not apply mode 3200x1800 at scale %s:\n%s\n' \
        "$output_scale" "$sway_outputs" >&2
    exit 1
fi
for _ in $(seq 1 100); do
    for pid in $(pgrep -f 'Xwayland :[0-9]+' || true); do
        process_env=$(tr '\0' '\n' <"/proc/$pid/environ" 2>/dev/null) || continue
        if [[ $(printf '%s\n' "$process_env" | sed -n 's/^XDG_RUNTIME_DIR=//p') != "$runtime_dir" \
            || $(printf '%s\n' "$process_env" | sed -n 's/^WAYLAND_DISPLAY=//p') != "$WAYLAND_DISPLAY" ]]; then
            continue
        fi
        DISPLAY=$(printf '%s\n' "$process_env" | sed -n 's/^DISPLAY=//p')
        if [[ -z $DISPLAY ]]; then
            process_cmd=$(tr '\0' ' ' <"/proc/$pid/cmdline")
            if [[ $process_cmd =~ Xwayland[[:space:]](:[0-9]+) ]]; then
                DISPLAY=${BASH_REMATCH[1]}
            fi
        fi
        if [[ -n $DISPLAY ]]; then
            break
        fi
    done
    if [[ -n $DISPLAY ]] && xdpyinfo -display "$DISPLAY" >/dev/null 2>&1; then
        break
    fi
    sleep 0.1
done
if [[ -z $DISPLAY ]] || ! xdpyinfo -display "$DISPLAY" >/dev/null 2>&1; then
    printf 'XWayland for this Sway session did not become available\n' >&2
    cat "$runtime_dir/sway.log" >&2 || true
    pgrep -af 'Xwayland|sway' >&2 || true
    exit 1
fi
export DISPLAY
dbus-update-activation-environment --all

xdg-desktop-portal-wlr >"$runtime_dir/portal-wlr.log" 2>&1 &
xdpw_pid=$!
xdg-desktop-portal >"$runtime_dir/portal.log" 2>&1 &
portal_pid=$!
dbus-update-activation-environment --all
portal_ready=0
for _ in $(seq 1 100); do
    if ! kill -0 "$xdpw_pid" 2>/dev/null; then
        printf 'xdg-desktop-portal-wlr exited during startup\n' >&2
        exit 1
    fi
    if gdbus call --session --dest org.freedesktop.portal.Desktop \
        --object-path /org/freedesktop/portal/desktop \
        --method org.freedesktop.DBus.Properties.Get \
        org.freedesktop.portal.Screenshot version >/dev/null 2>&1; then
        portal_ready=1
        break
    fi
    sleep 0.1
done
if [[ $portal_ready != 1 ]]; then
    printf 'Screenshot portal did not become available\n' >&2
    cat "$runtime_dir/portal.log" "$runtime_dir/portal-wlr.log" >&2
    exit 1
fi
if ! gdbus call --session --dest org.freedesktop.DBus \
    --object-path /org/freedesktop/DBus \
    --method org.freedesktop.DBus.GetNameOwner \
    org.freedesktop.portal.Desktop >/dev/null; then
    printf 'xdg-desktop-portal did not own its D-Bus name after startup\n' >&2
    cat "$runtime_dir/portal.log" "$runtime_dir/portal-wlr.log" >&2
    exit 1
fi
if ! gdbus call --session --dest org.freedesktop.DBus \
    --object-path /org/freedesktop/DBus \
    --method org.freedesktop.DBus.GetNameOwner \
    org.freedesktop.impl.portal.desktop.wlr >/dev/null; then
    printf 'xdg-desktop-portal-wlr did not own its D-Bus name after startup\n' >&2
    cat "$runtime_dir/portal.log" "$runtime_dir/portal-wlr.log" >&2
    exit 1
fi

"$@"
