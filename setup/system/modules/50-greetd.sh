#!/usr/bin/env bash
set -euo pipefail

MODULE="system:50-greetd"

log() {
    printf '[%s] %s\n' "${MODULE}" "$*"
}

write_greetd_config() {
    local config_dir="/etc/greetd"
    local config_file="${config_dir}/config.toml"
    log "Writing ${config_file}"
    install -d -m 0755 "${config_dir}"
    cat <<'CONFIG' >"${config_file}"
[terminal]
vt = 1

[default_session]
command = "/usr/local/bin/photoframe-session"
user = "kiosk"
CONFIG
    chmod 0644 "${config_file}"
}

install_session_wrapper() {
    local wrapper="/usr/local/bin/photoframe-session"
    log "Installing ${wrapper}"
    install -d -m 0755 "$(dirname "${wrapper}")"
    cat <<'WRAPPER' >"${wrapper}"
#!/usr/bin/env bash
# Strict kiosk launcher: requires proper system setup (seatd + logind) and DRM backend.
set -euo pipefail

die() { echo "[photoframe-session] ERROR: $*" >&2; exit 1; }

# 0) Must be running as the kiosk user (or whatever greetd uses)
#    Not strictly required, but catches accidental manual invocations.
: "${USER:=}"
[[ -n "$USER" ]] || die "USER not set"
# echo "[photoframe-session] running as $USER (uid=$(id -u))"

# 1) Refuse nested Wayland/X sessions — we want DRM backend on VT1.
[[ -z "${WAYLAND_DISPLAY:-}" ]] || die "WAYLAND_DISPLAY is set (${WAYLAND_DISPLAY}); refusing to run nested. Clear this and launch from VT1 via greetd."
[[ -z "${DISPLAY:-}" ]]        || die "DISPLAY is set (${DISPLAY}); refusing to run under X11. Launch from VT1 via greetd."

# 2) XDG_RUNTIME_DIR must come from logind (/run/user/<uid>) and already exist.
[[ -n "${XDG_RUNTIME_DIR:-}" ]] || die "XDG_RUNTIME_DIR is unset. This should be provided by PAM/logind."
case "$XDG_RUNTIME_DIR" in
  /run/user/*) ;;
  *) die "XDG_RUNTIME_DIR must be under /run/user/<uid>, got: ${XDG_RUNTIME_DIR}";;
esac
[[ -d "$XDG_RUNTIME_DIR" ]] || die "XDG_RUNTIME_DIR does not exist: $XDG_RUNTIME_DIR"
# Ownership  and perms (0700) are recommended; warn if off.
owner_uid="$(stat -c '%u' "$XDG_RUNTIME_DIR")"
[[ "$owner_uid" -eq "$(id -u)" ]] || die "XDG_RUNTIME_DIR owner uid=$owner_uid does not match current uid=$(id -u)"
perm="$(stat -c '%a' "$XDG_RUNTIME_DIR")"
[[ "$perm" == "700" || "$perm" == "700" ]] || echo "[photoframe-session] WARN: XDG_RUNTIME_DIR perms are $perm (expected 700)"

# 3) seatd must be running and its socket accessible by this user.
[[ -S /run/seatd.sock ]] || die "seatd socket missing at /run/seatd.sock. Is seatd.service active?"
socket_group="$(stat -c '%G' /run/seatd.sock || true)"
[[ -n "$socket_group" ]] || die "Failed to read group of /run/seatd.sock"
id -nG | tr ' ' '\n' | grep -qx "$socket_group" \
  || die "current user is not in the '$socket_group' group required to access /run/seatd.sock"

# 4) Required binaries
command -v dbus-run-session >/dev/null 2>&1 || die "dbus-run-session not found. Install dbus-user-session."
command -v sway >/dev/null 2>&1 || die "sway not found. Install sway."

# 5) Sway config must exist
CONFIG_PATH="/usr/local/share/photoframe/sway/config"
[[ -f "$CONFIG_PATH" ]] || die "sway config missing at ${CONFIG_PATH}"

# 6) Launch compositor on DRM; logs to journald under photoframe
exec systemd-cat -t photoframe -- dbus-run-session \
  sway -c "$CONFIG_PATH"
WRAPPER
    chmod 0755 "${wrapper}"
}

install_photo_launcher() {
    local launcher="/usr/local/bin/photoframe"
    log "Installing ${launcher}"
    install -d -m 0755 "$(dirname "${launcher}")"
    cat <<'LAUNCHER' >"${launcher}"
#!/usr/bin/env bash
set -euo pipefail

APP="/opt/photoframe/bin/photoframe"

if [[ ! -x "${APP}" ]]; then
  echo "[photoframe] binary not found at ${APP}" >&2
  exit 127
fi

# Provide a stable Wayland app_id for Sway rules and focus control.
# Allow override via environment; default matches configuration defaults.
export WINIT_APP_ID="${WINIT_APP_ID:-photoframe}"
# Control where logs go via PHOTOFRAME_LOG (journal|stdout|file:/path)
# Defaults to journald for kiosk stability.
case "${PHOTOFRAME_LOG:-journal}" in
  journal)
    exec systemd-cat -t photoframe -- "${APP}" "$@"
    ;;
  stdout)
    exec "${APP}" "$@"
    ;;
  file:*)
    logfile="${PHOTOFRAME_LOG#file:}"
    # Ensure parent dir exists; ignore failure
    mkdir -p -- "$(dirname -- "$logfile")" 2>/dev/null || true
    # Append and flush; caller can tail the file
    exec "${APP}" "$@" >>"$logfile" 2>&1
    ;;
  *)
    exec systemd-cat -t photoframe -- "${APP}" "$@"
    ;;
esac
LAUNCHER
    chmod 0755 "${launcher}"
}

install_sway_config() {
    local config_dir="/usr/local/share/photoframe/sway"
    local config_file="${config_dir}/config"
    log "Writing ${config_file}"
    install -d -m 0755 "${config_dir}"
    cat <<'CONFIG' >"${config_file}"
# Photo frame sway configuration

set $photo_app_id photoframe
set $overlay_app_id wifi-overlay
set $config_path /etc/photoframe/config.yaml

focus_follows_mouse no
mouse_warping none
default_border none
smart_borders off
floating_modifier Mod4

seat seat0 hide_cursor 2000

output * bg #000000 solid_color
output HDMI-A-1 mode 3840x2160@60Hz
output HDMI-A-1 scale 1.0

workspace number 1 output HDMI-A-1
assign [app_id="$photo_app_id"] workspace number 1

for_window [app_id="$photo_app_id"] floating enable, fullscreen enable, move position 0 0, inhibit_idle fullscreen
for_window [app_id="$overlay_app_id"] fullscreen enable, focus, border none

bar {
    mode invisible
}

exec_always swaybg -m solid_color -c '#000000'
# Launch the app fullscreen
exec_always /usr/local/bin/photoframe "$config_path"
CONFIG
    chmod 0644 "${config_file}"
}

write_greetd_config
install_sway_config
install_photo_launcher
install_session_wrapper

log "greetd session provisioning complete"
