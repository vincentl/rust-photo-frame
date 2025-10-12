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
set -euo pipefail

export RUST_LOG="${RUST_LOG:-info}"
if id -u kiosk >/dev/null 2>&1; then
    export XDG_RUNTIME_DIR="/run/user/$(id -u kiosk)"
else
    export XDG_RUNTIME_DIR="/run/user/$(id -u)"
fi
export WAYLAND_DISPLAY="wayland-1"
export XDG_SESSION_TYPE="wayland"
export XDG_CURRENT_DESKTOP="sway"
export QT_QPA_PLATFORM="wayland"
export CLUTTER_BACKEND="wayland"
export MOZ_ENABLE_WAYLAND="1"

if [[ ! -d "${XDG_RUNTIME_DIR}" ]]; then
    mkdir -p "${XDG_RUNTIME_DIR}"
    chmod 0700 "${XDG_RUNTIME_DIR}"
fi

CONFIG_PATH="/usr/local/share/photoframe/sway/config"
if [[ ! -f "${CONFIG_PATH}" ]]; then
    echo "[photoframe-session] ERROR: sway config missing at ${CONFIG_PATH}" >&2
    exit 1
fi

CMD=(sway -c "${CONFIG_PATH}")
runner=()

if command -v dbus-run-session >/dev/null 2>&1; then
    runner+=(dbus-run-session)
fi

# seatd.service already provides the compositor with a socket, so avoid
# wrapping sway in seatd-launch (which would fail when seatd is active).

if [[ ${#runner[@]} -eq 0 ]]; then
    exec systemd-cat -t rust-photo-frame -- "${CMD[@]}"
else
    exec systemd-cat -t rust-photo-frame -- "${runner[@]}" "${CMD[@]}"
fi
WRAPPER
    chmod 0755 "${wrapper}"
}

install_photo_launcher() {
    local launcher="/usr/local/bin/photo-frame"
    log "Installing ${launcher}"
    install -d -m 0755 "$(dirname "${launcher}")"
    cat <<'LAUNCHER' >"${launcher}"
#!/usr/bin/env bash
set -euo pipefail

APP="/opt/photo-frame/bin/photo-frame"

if [[ ! -x "${APP}" ]]; then
    echo "[photo-frame] binary not found at ${APP}" >&2
    exit 127
fi

exec systemd-cat -t rust-photo-frame -- "${APP}" "$@"
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

set $photo_app_id rust-photo-frame
set $overlay_app_id wifi-overlay
set $config_path /var/lib/photo-frame/config/config.yaml

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
exec_always /usr/local/bin/photo-frame
CONFIG
    chmod 0644 "${config_file}"
}

write_greetd_config
install_sway_config
install_photo_launcher
install_session_wrapper

log "greetd session provisioning complete"

