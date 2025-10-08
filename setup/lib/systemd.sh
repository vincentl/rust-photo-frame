#!/usr/bin/env bash
set -euo pipefail

if [[ -z "${SYSTEMDCTL:-}" ]]; then
    if command -v systemctl >/dev/null 2>&1; then
        SYSTEMDCTL="$(command -v systemctl)"
    else
        SYSTEMDCTL=/bin/systemctl
    fi
fi
SYSTEMD_DIR=${SYSTEMD_DIR:-/etc/systemd/system}

systemd_available() {
    if [[ -x "${SYSTEMDCTL}" ]]; then
        return 0
    fi
    if command -v systemctl >/dev/null 2>&1; then
        SYSTEMDCTL="$(command -v systemctl)"
        return 0
    fi
    return 1
}

_systemd_require() {
    if ! systemd_available; then
        echo "systemctl not found at ${SYSTEMDCTL}" >&2
        exit 1
    fi
}

systemd_daemon_reload() {
    _systemd_require
    "${SYSTEMDCTL}" daemon-reload
}

systemd_enable_unit() {
    _systemd_require
    local unit="$1"
    "${SYSTEMDCTL}" enable "${unit}"
}

systemd_disable_unit() {
    _systemd_require
    local unit="$1"
    "${SYSTEMDCTL}" disable "${unit}" >/dev/null 2>&1 || true
}

systemd_stop_unit() {
    _systemd_require
    local unit="$1"
    "${SYSTEMDCTL}" stop "${unit}" >/dev/null 2>&1 || true
}

systemd_restart_unit() {
    _systemd_require
    local unit="$1"
    if "${SYSTEMDCTL}" is-active --quiet "${unit}"; then
        "${SYSTEMDCTL}" restart "${unit}"
    else
        "${SYSTEMDCTL}" start "${unit}"
    fi
}

systemd_start_unit() {
    _systemd_require
    local unit="$1"
    "${SYSTEMDCTL}" start "${unit}"
}

systemd_enable_now_unit() {
    _systemd_require
    local unit="$1"
    "${SYSTEMDCTL}" enable --now "${unit}"
}

systemd_disable_now_unit() {
    _systemd_require
    local unit="$1"
    "${SYSTEMDCTL}" disable --now "${unit}"
}

systemd_is_active() {
    _systemd_require
    local unit="$1"
    "${SYSTEMDCTL}" is-active --quiet "${unit}"
}

systemd_is_enabled() {
    _systemd_require
    local unit="$1"
    "${SYSTEMDCTL}" is-enabled --quiet "${unit}"
}

systemd_status() {
    _systemd_require
    local unit="$1"
    "${SYSTEMDCTL}" status "${unit}" --no-pager
}

systemd_set_default_target() {
    _systemd_require
    local target="$1"
    "${SYSTEMDCTL}" set-default "${target}"
}

systemd_mask_unit() {
    _systemd_require
    local unit="$1"
    "${SYSTEMDCTL}" mask "${unit}"
}

systemd_unmask_unit() {
    _systemd_require
    local unit="$1"
    "${SYSTEMDCTL}" unmask "${unit}"
}

systemd_unit_property() {
    _systemd_require
    local unit="$1" property="$2"
    "${SYSTEMDCTL}" show -p "${property}" --value "${unit}"
}

systemd_unit_exists() {
    _systemd_require
    local unit="$1"
    if "${SYSTEMDCTL}" list-unit-files "${unit}" >/dev/null 2>&1; then
        return 0
    fi
    return 1
}

systemd_install_unit_file() {
    local src="$1" dest_name="$2"
    if [[ ! -f "${src}" ]]; then
        echo "systemd unit template missing: ${src}" >&2
        exit 1
    fi
    install -D -m 0644 "${src}" "${SYSTEMD_DIR}/${dest_name}"
}

systemd_install_dropin() {
    local unit="$1" name="$2" src="$3"
    local dropin_dir="${SYSTEMD_DIR}/${unit}.d"
    if [[ ! -f "${src}" ]]; then
        echo "systemd drop-in template missing: ${src}" >&2
        exit 1
    fi
    install -d -m 0755 "${dropin_dir}"
    install -m 0644 "${src}" "${dropin_dir}/${name}"
}

systemd_remove_dropins() {
    local unit="$1"
    rm -rf "${SYSTEMD_DIR}/${unit}.d"
}
