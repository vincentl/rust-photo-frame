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
command = "cage -s -- /usr/local/bin/photoframe-session"
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

if command -v wlr-randr >/dev/null 2>&1; then
    if ! wlr-randr --output HDMI-A-1 --mode 3840x2160@60; then
        log "WARN: Failed to apply HDMI-A-1 3840x2160@60 mode via wlr-randr"
    else
        log "Applied HDMI-A-1 3840x2160@60 via wlr-randr"
    fi
else
    log "WARN: wlr-randr not found; skipping output configuration"
fi

exec systemd-cat --identifier=rust-photo-frame env RUST_LOG=info /opt/photo-frame/bin/rust-photo-frame /var/lib/photo-frame/config/config.yaml
WRAPPER
    chmod 0755 "${wrapper}"
}

write_greetd_config
install_session_wrapper

log "greetd session provisioning complete"

