#!/usr/bin/env bash
set -euo pipefail

MODULE="system:50-greetd"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib/systemd.sh
source "${SCRIPT_DIR}/../lib/systemd.sh"
# shellcheck source=../../lib/display-stack.sh
source "${SCRIPT_DIR}/../../lib/display-stack.sh"

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
[[ "$perm" == "700" || "$perm" == "0700" ]] || echo "[photoframe-session] WARN: XDG_RUNTIME_DIR perms are $perm (expected 700)"

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
# The panel may be on either HDMI port; configure both.
output HDMI-A-1 mode 3840x2160@60Hz
output HDMI-A-1 scale 1.0
output HDMI-A-2 mode 3840x2160@60Hz
output HDMI-A-2 scale 1.0

workspace number 1 output HDMI-A-1
workspace number 1 output HDMI-A-2
assign [app_id="$photo_app_id"] workspace number 1

for_window [app_id="$photo_app_id"] floating enable, fullscreen enable, move position 0 0, inhibit_idle fullscreen
for_window [app_id="$overlay_app_id"] fullscreen enable, focus, border none

bar {
    mode invisible
}

exec swaybg -m solid_color -c '#000000'
# Launch the app fullscreen. Use `exec` (not `exec_always`) so a `swaymsg reload`
# doesn't spawn a second photoframe alongside the running one.
exec /usr/local/bin/photoframe "$config_path"
CONFIG
    chmod 0644 "${config_file}"
}

# The greetd -> sway session is the wayland fallback. On the default KMS
# stack none of this is installed (the launcher the KMS unit runs comes from
# 45-launcher.sh); PHOTOFRAME_DISPLAY=wayland provisions it.
display_choice="$(display_stack_current)"
if [[ "${display_choice}" != "wayland" ]]; then
    log "Display stack is ${display_choice}; skipping the greetd/sway session files (PHOTOFRAME_DISPLAY=wayland installs them)"
    exit 0
fi

write_greetd_config
install_sway_config
install_session_wrapper

log "greetd session provisioning complete"
