#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"

log() {
    printf '[kiosk-setup] %s\n' "$*"
}

die() {
    printf '[kiosk-setup] ERROR: %s\n' "$*" >&2
    exit 1
}

require_root() {
    if [[ ${EUID} -ne 0 ]]; then
        exec sudo -- "$0" "$@"
    fi
}

require_trixie() {
    if [[ ! -r /etc/os-release ]]; then
        die "/etc/os-release not found; cannot detect OS"
    fi
    # shellcheck disable=SC1091
    . /etc/os-release
    if [[ "${VERSION_CODENAME:-}" != "trixie" ]]; then
        die "Debian 13 (trixie) is required"
    fi
}

require_commands() {
    local missing=()
    local cmd
    for cmd in apt-get install getent groupadd id install mkdir systemctl useradd usermod; do
        if ! command -v "${cmd}" >/dev/null 2>&1; then
            missing+=("${cmd}")
        fi
    done
    if (( ${#missing[@]} > 0 )); then
        die "Missing required commands: ${missing[*]}"
    fi
}

backup_boot_config() {
    local config_path="$1" backup_var="$2"
    if [[ "${!backup_var}" -eq 0 ]]; then
        local backup
        backup="${config_path}.bak.$(date +%Y%m%d-%H%M%S)"
        cp -a "${config_path}" "${backup}"
        printf -v "${backup_var}" '%d' 1
        log "Backup of ${config_path} written to ${backup}"
    fi
}

ensure_boot_config_pi5() {
    local module="pi5-boot-config" enable_4k="${ENABLE_4K_BOOT:-1}" config_path="${1:-}"
    local backup_taken=0

    if [[ "${enable_4k}" != "1" ]]; then
        log "[${module}] 4K boot configuration disabled via ENABLE_4K_BOOT=${enable_4k}. Skipping."
        return 0
    fi

    if [[ -z "${config_path}" ]]; then
        if [[ -f /boot/firmware/config.txt ]]; then
            config_path="/boot/firmware/config.txt"
        elif [[ -f /boot/config.txt ]]; then
            config_path="/boot/config.txt"
        else
            log "[${module}] WARN: Unable to locate config.txt (looked in /boot/firmware and /boot)."
            return 0
        fi
    fi

    log "[${module}] Using configuration file ${config_path}"

    declare -A kv_settings=(
        ["hdmi_force_hotplug:1"]="1"
        ["hdmi_group:1"]="2"
        ["hdmi_mode:1"]="97"
        ["hdmi_drive:1"]="2"
    )

    declare -A dtparams=(
        ["dtparam=fan_temp0"]="dtparam=fan_temp0=50000"
        ["dtparam=fan_temp1"]="dtparam=fan_temp1=60000"
        ["dtparam=fan_temp2"]="dtparam=fan_temp2=70000"
        ["dtparam=fan_temp3"]="dtparam=fan_temp3=80000"
    )

    local key value line
    for key in "${!kv_settings[@]}"; do
        value="${kv_settings[${key}]}"
        if grep -qx "${key}=${value}" "${config_path}" 2>/dev/null; then
            log "[${module}] ${key} already set to ${value}"
            continue
        fi

        if grep -q "^${key}=" "${config_path}" 2>/dev/null; then
            backup_boot_config "${config_path}" backup_taken
            sed -i "s|^${key}=.*$|${key}=${value}|" "${config_path}"
        else
            backup_boot_config "${config_path}" backup_taken
            printf '\n%s=%s\n' "${key}" "${value}" >>"${config_path}"
        fi
        log "[${module}] Set ${key}=${value}"
    done

    for line in "${!dtparams[@]}"; do
        value="${dtparams[${line}]}"
        if grep -qxF "${value}" "${config_path}" 2>/dev/null; then
            log "[${module}] ${value} already present"
            continue
        fi

        if grep -q "^${line}=" "${config_path}" 2>/dev/null; then
            backup_boot_config "${config_path}" backup_taken
            sed -i "s|^${line}=.*$|${value}|" "${config_path}"
        else
            backup_boot_config "${config_path}" backup_taken
            printf '\n%s\n' "${value}" >>"${config_path}"
        fi
        log "[${module}] Ensured ${value}"
    done

    if grep -q '^dtoverlay=pi5-fan' "${config_path}" 2>/dev/null; then
        backup_boot_config "${config_path}" backup_taken
        sed -i '/^dtoverlay=pi5-fan/d' "${config_path}"
        log "[${module}] Removed deprecated dtoverlay=pi5-fan entry"
    fi

    if [[ ${backup_taken} -eq 0 ]]; then
        log "[${module}] Boot configuration already satisfied"
    else
        sync
        log "[${module}] Boot configuration updated"
    fi
}

ensure_packages() {
    local packages=(
        cage
        greetd
        mesa-vulkan-drivers
        vulkan-tools
        wayland-protocols
        wlr-randr
        socat
    )
    log "Installing packages: ${packages[*]}"
    export DEBIAN_FRONTEND=noninteractive
    apt-get update
    apt-get install -y --no-install-recommends "${packages[@]}"
}

ensure_kiosk_user() {
    local user="kiosk"
    if ! id -u "${user}" >/dev/null 2>&1; then
        log "Creating kiosk user ${user}"
        useradd --create-home --shell /usr/sbin/nologin "${user}"
    else
        log "User ${user} already exists"
        usermod --shell /usr/sbin/nologin "${user}" >/dev/null 2>&1 || true
        if [[ ! -d "/home/${user}" ]]; then
            log "Ensuring home directory for ${user}"
            install -d -m 0750 "/home/${user}"
            chown "${user}:${user}" "/home/${user}"
        fi
    fi

    local group
    for group in render video input; do
        if ! getent group "${group}" >/dev/null 2>&1; then
            log "Creating group ${group}"
            groupadd "${group}"
        fi
        if ! id -nG "${user}" | tr ' ' '\n' | grep -Fxq "${group}"; then
            log "Adding ${user} to ${group}"
            usermod -aG "${group}" "${user}"
        fi
    done

    if [[ ! -d "/home/${user}" ]]; then
        log "Creating home directory for ${user}"
        install -d -m 0750 "/home/${user}"
        chown "${user}:${user}" "/home/${user}"
    fi
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

ensure_runtime_dirs() {
    local runtime_dir="/run/photo-frame"
    local tmpfiles_conf="/etc/tmpfiles.d/photo-frame.conf"

    log "Ensuring runtime control socket directory ${runtime_dir}"
    install -d -m 0770 -o kiosk -g kiosk "${runtime_dir}"

    log "Writing tmpfiles.d entry ${tmpfiles_conf}"
    install -d -m 0755 "$(dirname "${tmpfiles_conf}")"
    cat <<'TMPFILES' >"${tmpfiles_conf}"
# photo-frame runtime directories
d /run/photo-frame 0770 kiosk kiosk -
TMPFILES
}

install_auxiliary_units() {
    log "Installing photoframe systemd units"
    local unit
    for unit in "${REPO_ROOT}"/assets/systemd/photoframe-*; do
        [ -f "${unit}" ] || continue
        install -D -m 0644 "${unit}" "/etc/systemd/system/$(basename "${unit}")"
    done
}

enable_systemd_units() {
    log "Enabling kiosk services"
    systemctl daemon-reload

    local dm
    for dm in gdm3.service sddm.service lightdm.service; do
        if systemctl list-unit-files "${dm}" >/dev/null 2>&1; then
            log "Disabling conflicting display manager ${dm}"
            systemctl disable --now "${dm}" >/dev/null 2>&1 || true
        fi
    done

    log "Setting default boot target to graphical.target"
    systemctl set-default graphical.target

    if systemctl list-unit-files getty@tty1.service >/dev/null 2>&1; then
        log "Disabling and masking getty@tty1.service to avoid VT contention"
        systemctl disable --now getty@tty1.service >/dev/null 2>&1 || true
        systemctl mask getty@tty1.service >/dev/null 2>&1 || true
    fi

    log "Setting greetd as the system display manager"
    systemctl enable --now greetd.service >/dev/null 2>&1 || true

    log "Verifying display-manager alias"
    systemctl status display-manager.service --no-pager || true

    local unit
    for unit in photoframe-wifi-manager.service photoframe-buttond.service; do
        if systemctl list-unit-files "${unit}" >/dev/null 2>&1; then
            systemctl enable --now "${unit}"
        fi
    done

    if systemctl list-unit-files photoframe-sync.timer >/dev/null 2>&1; then
        systemctl enable photoframe-sync.timer
        systemctl start photoframe-sync.timer || true
    fi
}

main() {
    require_root "$@"
    require_trixie
    require_commands

    ensure_boot_config_pi5

    ensure_packages
    ensure_kiosk_user
    ensure_runtime_dirs
    install_session_wrapper
    write_greetd_config
    install_auxiliary_units
    enable_systemd_units

    log "Kiosk provisioning complete. greetd will launch cage on tty1 as kiosk."
}

main "$@"
