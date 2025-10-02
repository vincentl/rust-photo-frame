#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'

if [[ $(id -u) -ne 0 ]]; then
    echo "create-users-and-perms.sh must be run as root" >&2
    exit 1
fi

KIOSK_USER="${KIOSK_USER:-kiosk}"
FRAME_USER="${FRAME_USER:-frame}"
KIOSK_GROUPS=(render video input)

OPT_ROOT=/opt/photo-frame
OPT_SUBDIRS=(bin etc share)
VAR_ROOT=/var/lib/photo-frame
VAR_SUBDIRS=(photos config)
CACHE_ROOT=/var/cache/photo-frame
LOG_ROOT=/var/log/photo-frame

require_command() {
    local cmd="$1"
    if ! command -v "${cmd}" >/dev/null 2>&1; then
        echo "Missing required command: ${cmd}" >&2
        exit 1
    fi
}

require_command install
require_command setfacl

ensure_user() {
    local user="$1" shell="$2" home="$3" system_flag="$4"
    if id -u "${user}" >/dev/null 2>&1; then
        return
    fi

    if [[ "${system_flag}" == "system" ]]; then
        useradd --system --home "${home}" --create-home --shell "${shell}" "${user}"
    else
        useradd --create-home --home-dir "${home}" --shell "${shell}" "${user}"
    fi
}

ensure_home_owner() {
    local user="$1" path="$2"
    if [[ -d "${path}" ]]; then
        chown "${user}:${user}" "${path}"
        chmod 0750 "${path}"
    fi
}

ensure_group_membership() {
    local user="$1" group="$2"
    if ! getent group "${group}" >/dev/null 2>&1; then
        echo "Required group ${group} is missing. Install GPU/input packages first." >&2
        exit 1
    fi
    if id -nG "${user}" | tr ' ' '\n' | grep -Fxq "${group}"; then
        return
    fi
    usermod -aG "${group}" "${user}"
}

ensure_directory() {
    local path="$1" mode="$2" owner="$3" group="$4"
    install -d -m "${mode}" -o "${owner}" -g "${group}" "${path}"
}

ensure_user "${KIOSK_USER}" /usr/sbin/nologin "${VAR_ROOT}" system
ensure_home_owner "${KIOSK_USER}" "${VAR_ROOT}"
ensure_user "${FRAME_USER}" /bin/bash "/home/${FRAME_USER}" regular

for group in "${KIOSK_GROUPS[@]}"; do
    ensure_group_membership "${KIOSK_USER}" "${group}"
done

ensure_directory "${OPT_ROOT}" 0755 root root
for dir in "${OPT_SUBDIRS[@]}"; do
    ensure_directory "${OPT_ROOT}/${dir}" 0755 root root
done

ensure_directory "${VAR_ROOT}" 0750 "${KIOSK_USER}" "${KIOSK_USER}"
setfacl -m "u:${FRAME_USER}:rwx,g:${KIOSK_USER}:rwx,mask::rwx" "${VAR_ROOT}"
setfacl -d -m "u:${KIOSK_USER}:rwx,u:${FRAME_USER}:rwx,g:${KIOSK_USER}:rwx,mask::rwx,other::---" "${VAR_ROOT}"

for dir in "${VAR_SUBDIRS[@]}"; do
    ensure_directory "${VAR_ROOT}/${dir}" 0770 "${KIOSK_USER}" "${KIOSK_USER}"
    setfacl -m "u:${FRAME_USER}:rwx,u:${KIOSK_USER}:rwx,g:${KIOSK_USER}:rwx,mask::rwx,other::---" "${VAR_ROOT}/${dir}"
    setfacl -d -m "u:${FRAME_USER}:rwx,u:${KIOSK_USER}:rwx,g:${KIOSK_USER}:rwx,mask::rwx,other::---" "${VAR_ROOT}/${dir}"
done

ensure_directory "${CACHE_ROOT}" 0750 "${KIOSK_USER}" "${KIOSK_USER}"
ensure_directory "${LOG_ROOT}" 0750 "${KIOSK_USER}" "${KIOSK_USER}"
setfacl -m "u:${FRAME_USER}:rx,mask::rwx" "${LOG_ROOT}"
setfacl -d -m "u:${FRAME_USER}:rx,u:${KIOSK_USER}:rwx,g:${KIOSK_USER}:rwx,mask::rwx,other::---" "${LOG_ROOT}"

printf 'Users and permissions ready for kiosk=%s frame=%s\n' "${KIOSK_USER}" "${FRAME_USER}"
