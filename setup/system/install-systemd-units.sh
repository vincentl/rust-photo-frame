#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'

if [[ $(id -u) -ne 0 ]]; then
    echo "install-systemd-units.sh must be run as root" >&2
    exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
UNIT_SRC="${SCRIPT_DIR}/units"
PAM_SRC="${SCRIPT_DIR}/pam.d"
SYSTEMD_DIR=/etc/systemd/system
PAM_DIR=/etc/pam.d

install_unit() {
    local src="$1" name
    name="$(basename "${src}")"
    if [[ ! -f "${src}" ]]; then
        echo "Unit template missing: ${src}" >&2
        exit 1
    fi
    install -D -m 0644 "${src}" "${SYSTEMD_DIR}/${name}"
}

install_unit "${SCRIPT_DIR}/cage@.service"

if [[ -d "${UNIT_SRC}" ]]; then
    while IFS= read -r -d '' unit; do
        install_unit "${unit}"
    done < <(find "${UNIT_SRC}" -maxdepth 1 -type f -print0)
fi

if [[ -d "${PAM_SRC}" ]]; then
    while IFS= read -r -d '' pam; do
        name="$(basename "${pam}")"
        install -D -m 0644 "${pam}" "${PAM_DIR}/${name}"
    done < <(find "${PAM_SRC}" -maxdepth 1 -type f -print0)
fi

systemctl daemon-reload

LEGACY_UNITS=(
    sync-photos.service
    sync-photos.timer
    wifi-manager.service
    photo-buttond.service
    photo-sync.service
    photo-sync.timer
)

for unit in "${LEGACY_UNITS[@]}"; do
    if systemctl list-unit-files "${unit}" >/dev/null 2>&1; then
        systemctl disable "${unit}" >/dev/null 2>&1 || true
    fi
    if systemctl is-active --quiet "${unit}"; then
        systemctl stop "${unit}" || true
    fi
    rm -f "${SYSTEMD_DIR}/${unit}"
    rm -rf "${SYSTEMD_DIR}/${unit}.d" 2>/dev/null || true
    rm -f "/etc/systemd/system/multi-user.target.wants/${unit}" 2>/dev/null || true
    rm -f "/etc/systemd/system/graphical.target.wants/${unit}" 2>/dev/null || true
    rm -f "/etc/systemd/system/timers.target.wants/${unit}" 2>/dev/null || true
done

systemctl daemon-reload

ENABLE_UNITS=(
    cage@tty1.service
    photoframe-wifi-manager.service
    photoframe-buttond.service
    photoframe-sync.timer
)

for unit in "${ENABLE_UNITS[@]}"; do
    if systemctl list-unit-files "${unit}" >/dev/null 2>&1; then
        systemctl enable "${unit}"
    fi
    if [[ "${unit}" == *.timer ]]; then
        systemctl start "${unit}"
    fi
    if [[ "${unit}" != *.timer ]] && systemctl list-unit-files "${unit}" >/dev/null 2>&1; then
        if systemctl is-active --quiet "${unit}"; then
            systemctl reload-or-restart "${unit}"
        else
            systemctl start "${unit}"
        fi
    fi
done

printf 'Systemd units installed. Active services: %s\n' "${ENABLE_UNITS[*]}"
