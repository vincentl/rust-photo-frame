#!/usr/bin/env bash
set -euo pipefail

MODULE="system:60-systemd"

log() {
    printf '[%s] %s\n' "${MODULE}" "$*"
}

install_auxiliary_units() {
    log "Installing photoframe systemd units"
    local unit
    for unit in "${REPO_ROOT}"/assets/systemd/photoframe-*; do
        [ -f "${unit}" ] || continue
        install -D -m 0644 "${unit}" "/etc/systemd/system/$(basename "${unit}")"
    done
}

ensure_persistent_journald() {
    local module="journald"
    local config="/etc/systemd/journald.conf"
    local journal_dir="/var/log/journal"

    log "[${module}] Enabling persistent systemd-journald storage"
    install -d -m 2755 -o root -g systemd-journal "${journal_dir}"

    if [[ ! -f "${config}" ]]; then
        printf '[%s] ERROR: [%s] %s not found\n' "${MODULE}" "${module}" "${config}" >&2
        exit 1
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
    systemctl restart systemd-journald
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

    local session_bin="/opt/photo-frame/bin/rust-photo-frame"
    local greetd_started=0

    local seatd_units=(seatd.service seatd.socket)
    local seatd_unit
    for seatd_unit in "${seatd_units[@]}"; do
        if systemctl list-unit-files "${seatd_unit}" >/dev/null 2>&1; then
            log "Enabling ${seatd_unit}"
            systemctl enable "${seatd_unit}" >/dev/null 2>&1 || true
            log "Starting ${seatd_unit}"
            systemctl start "${seatd_unit}" >/dev/null 2>&1 || true
        fi
    done

    log "Setting greetd as the system display manager"
    if [[ -x "${session_bin}" ]]; then
        systemctl enable --now greetd.service >/dev/null 2>&1 || true
        greetd_started=1
    else
        log "rust-photo-frame binary missing at ${session_bin}; enabling greetd without starting"
        systemctl enable greetd.service >/dev/null 2>&1 || true
    fi

    if (( greetd_started )); then
        log "Verifying display-manager alias"
        systemctl status display-manager.service --no-pager || true
    else
        log "Skipping display-manager status until application binaries are installed"
    fi

    local wifi_bin="/opt/photo-frame/bin/wifi-manager"
    local button_bin="/opt/photo-frame/bin/photo-buttond"
    local unit
    for unit in photoframe-wifi-manager.service photoframe-buttond.service; do
        if systemctl list-unit-files "${unit}" >/dev/null 2>&1; then
            systemctl enable "${unit}"
            case "${unit}" in
                photoframe-wifi-manager.service)
                    if [[ -x "${wifi_bin}" ]]; then
                        systemctl start "${unit}" || true
                    else
                        log "Deferring start of ${unit}; ${wifi_bin} missing"
                    fi
                    ;;
                photoframe-buttond.service)
                    if [[ -x "${button_bin}" ]]; then
                        systemctl start "${unit}" || true
                    else
                        log "Deferring start of ${unit}; ${button_bin} missing"
                    fi
                    ;;
            esac
        fi
    done

    if systemctl list-unit-files photoframe-sync.timer >/dev/null 2>&1; then
        systemctl enable photoframe-sync.timer
        systemctl start photoframe-sync.timer || true
    fi
}

install_auxiliary_units
ensure_persistent_journald
enable_systemd_units

log "systemd provisioning complete"

