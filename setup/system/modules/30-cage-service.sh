#!/usr/bin/env bash
set -euo pipefail

MODULE="system:30-cage-service"
DRY_RUN="${DRY_RUN:-0}"
INSTALL_ROOT="${INSTALL_ROOT:-/opt/photo-frame}"
KIOSK_USER="${SERVICE_USER:-kiosk}"
TTY_DEVICE="${CAGE_TTY:-tty1}"
SYSTEMD_TARGET="/etc/systemd/system/cage@.service"
PAM_TARGET="/etc/pam.d/cage"
APP_BIN="${INSTALL_ROOT}/bin/rust-photo-frame"
CONFIG_PATH="${INSTALL_ROOT}/var/config.yaml"

log() {
    local level="$1"; shift
    printf '[%s] %s\n' "${MODULE}" "$level: $*"
}

run_sudo() {
    if [[ "${DRY_RUN}" == "1" ]]; then
        log INFO "DRY_RUN: sudo $*"
    else
        sudo "$@"
    fi
}

install_file() {
    local path="$1" mode="$2" owner="$3" group="$4" content="$5"
    if [[ "${DRY_RUN}" == "1" ]]; then
        log INFO "DRY_RUN: would install ${path} (${mode} ${owner}:${group})"
        printf '%s\n' "${content}"
        return
    fi
    local tmp
    tmp="$(mktemp)"
    printf '%s\n' "${content}" > "${tmp}"
    run_sudo install -Dm"${mode}" -o "${owner}" -g "${group}" "${tmp}" "${path}"
    rm -f "${tmp}"
}

log INFO "Writing cage systemd template to ${SYSTEMD_TARGET}"
service_content=$(cat <<EOF_SERVICE
# Managed by rust-photo-frame setup
# Launch Cage as a system unit with auto-login to a TTY.
[Unit]
Description=Cage Wayland compositor on %I
After=systemd-user-sessions.service plymouth-quit-wait.service
Before=graphical.target
ConditionPathExists=/dev/tty0
Wants=dbus.socket systemd-logind.service
After=dbus.socket systemd-logind.service
Conflicts=getty@%i.service
After=getty@%i.service

[Service]
Type=simple
ExecStart=/usr/bin/cage -- ${APP_BIN} ${CONFIG_PATH}
ExecStartPost=+sh -c "tty_name='%i'; exec chvt $${tty_name#tty}"
Restart=always
User=${KIOSK_USER}
UtmpIdentifier=%I
UtmpMode=user
TTYPath=/dev/%I
TTYReset=yes
TTYVHangup=yes
TTYVTDisallocate=yes
StandardInput=tty-fail
WorkingDirectory=${INSTALL_ROOT}/var
PAMName=cage

[Install]
WantedBy=graphical.target
Alias=display-manager.service
DefaultInstance=${TTY_DEVICE}
EOF_SERVICE
)
install_file "${SYSTEMD_TARGET}" 644 root root "${service_content}"

log INFO "Writing PAM stack to ${PAM_TARGET}"
pam_content=$(cat <<'EOF_PAM'
auth           required        pam_unix.so nullok
account        required        pam_unix.so
session        required        pam_unix.so
session        required        pam_systemd.so
EOF_PAM
)
install_file "${PAM_TARGET}" 644 root root "${pam_content}"

if [[ "${DRY_RUN}" != "1" ]]; then
    log INFO "Reloading systemd units"
    run_sudo systemctl daemon-reload
fi

log INFO "Cage unit installed"
