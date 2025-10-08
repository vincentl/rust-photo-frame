#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
# shellcheck source=../lib/systemd.sh
source "${SCRIPT_DIR}/../lib/systemd.sh"

log() {
    printf '[%s] %s\n' "${MODULE}" "$*"
}

die() {
    printf '[%s] ERROR: %s\n' "${MODULE}" "$*" >&2
    exit 1
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

    declare -A kv_settings=()

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

ensure_kiosk_packages() {
    local packages=(
        cage
        greetd
        mesa-vulkan-drivers
        vulkan-tools
        wayland-protocols
        wlr-randr
        socat
    )
    local missing=()
    local pkg
    for pkg in "${packages[@]}"; do
        if ! dpkg-query -W -f='${Status}' "${pkg}" 2>/dev/null | grep -q 'install ok installed'; then
            missing+=("${pkg}")
        fi
    done

    if (( ${#missing[@]} > 0 )); then
        log "Installing packages: ${missing[*]}"
        export DEBIAN_FRONTEND=noninteractive
        apt-get update
        apt-get install -y --no-install-recommends "${missing[@]}"
    else
        log "Kiosk-specific packages already installed"
    fi
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

install_polkit_rules() {
    local src_dir="${REPO_ROOT}/setup/assets/kiosk/polkit-1/rules.d"
    local dest_dir="/etc/polkit-1/rules.d"

    if [[ ! -d "${src_dir}" ]]; then
        log "No polkit rules to install"
        return
    fi

    log "Installing polkit rules for NetworkManager access"
    install -d -m 0755 "${dest_dir}"

    local rule
    for rule in "${src_dir}"/*.rules; do
        [ -f "${rule}" ] || continue
        install -m 0644 "${rule}" "${dest_dir}/$(basename "${rule}")"
    done
}

enable_systemd_units() {
    log "Enabling kiosk services"
    if ! systemd_available; then
        die "systemctl not available; cannot configure kiosk services"
    fi

    systemd_daemon_reload

    local dm
    for dm in gdm3.service sddm.service lightdm.service; do
        if systemd_unit_exists "${dm}"; then
            log "Disabling conflicting display manager ${dm}"
            systemd_disable_now_unit "${dm}" >/dev/null 2>&1 || true
        fi
    done

    log "Setting default boot target to graphical.target"
    systemd_set_default_target graphical.target

    if systemd_unit_exists getty@tty1.service; then
        log "Disabling and masking getty@tty1.service to avoid VT contention"
        systemd_disable_now_unit getty@tty1.service >/dev/null 2>&1 || true
        systemd_mask_unit getty@tty1.service >/dev/null 2>&1 || true
    fi

    log "Setting greetd as the system display manager"
    systemd_enable_now_unit greetd.service >/dev/null 2>&1 || true

    log "Verifying display-manager alias"
    systemd_status display-manager.service || true

    local unit
    for unit in photoframe-wifi-manager.service photoframe-buttond.service; do
        if systemd_unit_exists "${unit}"; then
            systemd_enable_now_unit "${unit}" || true
        fi
    done

    if systemd_unit_exists photoframe-sync.timer; then
        systemd_enable_unit photoframe-sync.timer
        systemd_start_unit photoframe-sync.timer || true
    fi
}

ensure_persistent_journald() {
    local module="journald"
    local config="/etc/systemd/journald.conf"
    local journal_dir="/var/log/journal"

    log "[${module}] Enabling persistent systemd-journald storage"
    install -d -m 2755 -o root -g systemd-journal "${journal_dir}"

    if [[ ! -f "${config}" ]]; then
        die "[${module}] ${config} not found"
    fi

    if grep -Eq '^[#[:space:]]*Storage=persistent' "${config}"; then
        log "[${module}] Storage already set to persistent"
    elif grep -Eq '^[#[:space:]]*Storage=' "${config}"; then
        sed -i 's/^[#[:space:]]*Storage=.*/Storage=persistent/' "${config}"
        log "[${module}] Set Storage=persistent"
    else
        printf '\nStorage=persistent\n' >>"${config}"
        log "[${module}] Appended Storage=persistent"
    fi

    if grep -Eq '^[#[:space:]]*SystemMaxUse=200M' "${config}"; then
        log "[${module}] SystemMaxUse already set to 200M"
    elif grep -Eq '^[#[:space:]]*SystemMaxUse=' "${config}"; then
        sed -i 's/^[#[:space:]]*SystemMaxUse=.*/SystemMaxUse=200M/' "${config}"
        log "[${module}] Set SystemMaxUse=200M"
    else
        printf 'SystemMaxUse=200M\n' >>"${config}"
        log "[${module}] Appended SystemMaxUse=200M"
    fi

    log "[${module}] Restarting systemd-journald to apply configuration"
    systemd_restart_unit systemd-journald
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
    install_polkit_rules
    ensure_persistent_journald
    enable_systemd_units

    log "Kiosk provisioning complete. greetd will launch cage on tty1 as kiosk."
}

main "$@"
