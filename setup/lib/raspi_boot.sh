#!/usr/bin/env bash
set -euo pipefail

RASPI_CMDLINE=${RASPI_CMDLINE:-/boot/firmware/cmdline.txt}

ensure_cmdline_without_console_tty1() {
    local path="${1:-${RASPI_CMDLINE}}" changed_var="${2:-_cmdline_changed}" line="" new_line="" changed=0
    if [[ ! -f "${path}" ]]; then
        echo "cmdline.txt not found at ${path}" >&2
        exit 1
    fi

    if [[ -r "${path}" ]]; then
        IFS= read -r line <"${path}" || line=""
    fi

    if [[ -z "${line}" ]]; then
        printf -v "${changed_var}" '%d' 0
        return
    fi

    local -a args=()
    read -r -a args <<<"${line}"
    local -a filtered=()
    local arg
    for arg in "${args[@]}"; do
        if [[ "${arg}" == "console=tty1" ]]; then
            changed=1
            continue
        fi
        filtered+=("${arg}")
    done

    new_line="${filtered[*]}"
    if [[ "${new_line}" != "${line}" ]]; then
        local tmp
        tmp="$(mktemp)"
        printf '%s\n' "${new_line}" >"${tmp}"
        install -m 0644 "${tmp}" "${path}"
        rm -f "${tmp}"
        changed=1
    fi

    printf -v "${changed_var}" '%d' "${changed}"
}

update_initramfs_if_available() {
    if command -v update-initramfs >/dev/null 2>&1; then
        update-initramfs -u
    fi
}

ensure_boot_config_4k() {
    local module="raspi_boot:4k" dry_run="${DRY_RUN:-0}" enable_4k="${ENABLE_4K_BOOT:-1}" config_path="${1:-}" backup_taken=0
    local changes_needed=0 backup_path=""

    if [[ "${enable_4k}" != "1" ]]; then
        printf '[%s] INFO: 4K boot configuration disabled via ENABLE_4K_BOOT=%s. Skipping.\n' "${module}" "${enable_4k}"
        return 0
    fi

    if [[ -z "${config_path}" ]]; then
        if [[ -f /boot/firmware/config.txt ]]; then
            config_path="/boot/firmware/config.txt"
        elif [[ -f /boot/config.txt ]]; then
            config_path="/boot/config.txt"
        else
            printf '[%s] WARN: Unable to locate config.txt (looked in /boot/firmware and /boot).\n' "${module}"
            return 0
        fi
    fi

    printf '[%s] INFO: Using configuration file %s\n' "${module}" "${config_path}"

    declare -A settings=(
        ["hdmi_force_hotplug:1"]="1"
        ["hdmi_group:1"]="2"
        ["hdmi_mode:1"]="97"
        ["hdmi_drive:1"]="2"
    )
    local overlays=("pi5-fan,temp0=55000,level0=50,temp1=65000,level1=150,temp2=75000,level2=255")

    local key value current overlay overlay_line
    for key in "${!settings[@]}"; do
        value="${settings[${key}]}"
        current=$(sudo awk -F'=' -v key="${key}" '$1==key {print $2}' "${config_path}" 2>/dev/null || true)
        if [[ "${current}" == "${value}" ]]; then
            printf '[%s] INFO: %s already set to %s\n' "${module}" "${key}" "${value}"
            continue
        fi

        changes_needed=1
        if [[ "${dry_run}" == "1" ]]; then
            if [[ -n "${current}" ]]; then
                printf '[%s] INFO: DRY_RUN: would update %s=%s\n' "${module}" "${key}" "${value}"
            else
                printf '[%s] INFO: DRY_RUN: would append %s=%s\n' "${module}" "${key}" "${value}"
            fi
            continue
        fi

        if [[ ${backup_taken} -eq 0 ]]; then
            backup_path="${config_path}.bak.$(date +%Y%m%d-%H%M%S)"
            sudo cp -a "${config_path}" "${backup_path}"
            printf '[%s] INFO: Backup written to %s\n' "${module}" "${backup_path}"
            backup_taken=1
        fi

        if [[ -n "${current}" ]]; then
            sudo sed -i "s|^${key}=.*$|${key}=${value}|" "${config_path}"
        else
            printf '\n%s=%s\n' "${key}" "${value}" | sudo tee -a "${config_path}" >/dev/null
        fi
        printf '[%s] INFO: Set %s=%s\n' "${module}" "${key}" "${value}"
    done

    for overlay in "${overlays[@]}"; do
        overlay_line="dtoverlay=${overlay}"
        if sudo grep -qxF "${overlay_line}" "${config_path}" 2>/dev/null; then
            printf '[%s] INFO: %s already present\n' "${module}" "${overlay_line}"
            continue
        fi

        changes_needed=1
        if [[ "${dry_run}" == "1" ]]; then
            printf '[%s] INFO: DRY_RUN: would append %s\n' "${module}" "${overlay_line}"
            continue
        fi

        if [[ ${backup_taken} -eq 0 ]]; then
            backup_path="${config_path}.bak.$(date +%Y%m%d-%H%M%S)"
            sudo cp -a "${config_path}" "${backup_path}"
            printf '[%s] INFO: Backup written to %s\n' "${module}" "${backup_path}"
            backup_taken=1
        fi

        printf '\n%s\n' "${overlay_line}" | sudo tee -a "${config_path}" >/dev/null
        printf '[%s] INFO: Added %s\n' "${module}" "${overlay_line}"
    done

    if [[ "${dry_run}" == "1" ]]; then
        if [[ ${changes_needed} -eq 0 ]]; then
            printf '[%s] INFO: DRY_RUN: configuration already matches requirements\n' "${module}"
        else
            printf '[%s] INFO: DRY_RUN: configuration changes summarized above\n' "${module}"
        fi
        return 0
    fi

    if [[ ${backup_taken} -eq 0 ]]; then
        printf '[%s] INFO: Boot configuration already satisfied\n' "${module}"
        return 0
    fi

    sudo sync
    printf '[%s] INFO: 4K boot configuration ensured\n' "${module}"
}
