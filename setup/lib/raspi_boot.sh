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
