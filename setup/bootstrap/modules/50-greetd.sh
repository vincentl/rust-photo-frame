#!/usr/bin/env bash
set -euo pipefail

MODULE="bootstrap:50-greetd"

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

log() {
    printf '[photoframe-session] %s\n' "$*" >&2
}

CONFIG_PATH="/usr/local/share/photoframe/sway/config"

ensure_runtime_dir() {
    if [[ -z "${XDG_RUNTIME_DIR:-}" ]]; then
        export XDG_RUNTIME_DIR="/run/user/$(id -u)"
    fi
    if [[ ! -d "${XDG_RUNTIME_DIR}" ]]; then
        install -d -m 0700 "${XDG_RUNTIME_DIR}"
    fi
}

ensure_runtime_dir

if [[ ! -f "${CONFIG_PATH}" ]]; then
    log "ERROR: sway config missing at ${CONFIG_PATH}"
    exit 1
fi

export XDG_SESSION_TYPE="wayland"
export XDG_CURRENT_DESKTOP="sway"
export QT_QPA_PLATFORM="wayland"
export CLUTTER_BACKEND="wayland"
export MOZ_ENABLE_WAYLAND="1"

exec systemd-cat --identifier=photoframe-session dbus-run-session seatd-launch -- sway --config "${CONFIG_PATH}"
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

set $photo_app_id rust-photo-frame
set $overlay_app_id wifi-overlay
set $config_path /var/lib/photo-frame/config/config.yaml

focus_follows_mouse no
mouse_warping none
default_border none
smart_borders off
floating_modifier Mod4

seat seat0 hide_cursor 0

output * bg #000000 solid_color
output HDMI-A-1 mode 3840x2160@60000Hz
output HDMI-A-1 scale 1.0

workspace number 1 output HDMI-A-1
assign [app_id="$photo_app_id"] workspace number 1

for_window [app_id="$photo_app_id"] fullscreen enable, inhibit_idle fullscreen
for_window [app_id="$overlay_app_id"] floating enable, border none

bar {
    mode invisible
}

exec --no-startup-id swaybg -c '#000000'
exec --no-startup-id env WINIT_APP_ID=$photo_app_id RUST_LOG=info systemd-cat --identifier=rust-photo-frame /opt/photo-frame/bin/rust-photo-frame $config_path
CONFIG
    chmod 0644 "${config_file}"
}

write_greetd_config
install_sway_config
install_session_wrapper

log "greetd session provisioning complete"

