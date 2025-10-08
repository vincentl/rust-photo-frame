#!/usr/bin/env bash
set -euo pipefail

MODULE="bootstrap:40-kiosk-user"

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
    local src_dir="${REPO_ROOT}/setup/files/etc/polkit-1/rules.d"
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

require_trixie
ensure_boot_config_pi5
ensure_kiosk_packages
ensure_kiosk_user
ensure_runtime_dirs
install_polkit_rules

log "Kiosk base provisioning complete"

