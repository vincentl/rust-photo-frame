#!/usr/bin/env bash
set -euo pipefail

MODULE="app:45-kiosk"
DRY_RUN="${DRY_RUN:-0}"
INSTALL_ROOT="${INSTALL_ROOT:-/opt/photo-frame}"
SERVICE_USER="${SERVICE_USER:-$(id -un)}"
SERVICE_GROUP="${SERVICE_GROUP:-$(id -gn)}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "${SCRIPT_DIR}/../../.." && pwd)}"
KIOSK_DIR="${REPO_ROOT}/setup/app/kiosk"

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

write_root_config_if_changed() {
    local path="$1" mode="$2" owner="$3" group="$4" content="$5"

    if [[ "${DRY_RUN}" == "1" ]]; then
        log INFO "DRY_RUN: would install ${path} (${mode} ${owner}:${group}) with contents:"
        printf '%s\n' "${content}"
        return 2
    fi

    local tmp
    tmp="$(mktemp)"
    printf '%s\n' "${content}" > "${tmp}"

    if run_sudo test -f "${path}"; then
        if run_sudo cmp -s "${tmp}" "${path}"; then
            rm -f "${tmp}"
            return 1
        fi
    fi

    run_sudo install -m "${mode}" -o "${owner}" -g "${group}" "${tmp}" "${path}"
    rm -f "${tmp}"
    return 0
}

write_user_file_if_changed() {
    local path="$1" content="$2"

    if [[ "${DRY_RUN}" == "1" ]]; then
        log INFO "DRY_RUN: would update ${path} with contents:"
        printf '%s\n' "${content}"
        return 2
    fi

    local tmp
    tmp="$(mktemp)"
    printf '%s\n' "${content}" > "${tmp}"

    if [[ -f "${path}" ]]; then
        if cmp -s "${tmp}" "${path}"; then
            rm -f "${tmp}"
            return 1
        fi
    fi

    install -Dm644 "${tmp}" "${path}"
    rm -f "${tmp}"
    return 0
}

get_user_home() {
    local user="$1"
    local entry
    entry="$(getent passwd "${user}" || true)"
    if [[ -z "${entry}" ]]; then
        log ERROR "Unable to determine home directory for ${user}"
        exit 1
    fi
    printf '%s' "${entry}" | cut -d: -f6
}

ensure_root_dir() {
    local dir="$1" mode="$2" owner="$3" group="$4"
    if [[ "${DRY_RUN}" == "1" ]]; then
        log INFO "DRY_RUN: would create directory ${dir} (${mode} ${owner}:${group})"
    else
        run_sudo install -d -m "${mode}" -o "${owner}" -g "${group}" "${dir}"
    fi
}

ensure_user_dir() {
    local dir="$1"
    if [[ "${DRY_RUN}" == "1" ]]; then
        log INFO "DRY_RUN: would create directory ${dir}"
    else
        install -d -m 755 "${dir}"
    fi
}

USER_HOME="$(get_user_home "${SERVICE_USER}")"
KIOSK_SNIPPET_SRC="${KIOSK_DIR}/bash-profile-snippet"
if [[ ! -f "${KIOSK_SNIPPET_SRC}" ]]; then
    log ERROR "Missing kiosk profile snippet at ${KIOSK_SNIPPET_SRC}"
    exit 1
fi

if [[ ! -x "${INSTALL_ROOT}/bin/photo-frame-kiosk" ]]; then
    log ERROR "Expected kiosk launcher at ${INSTALL_ROOT}/bin/photo-frame-kiosk. Re-run stage/install modules."
    exit 1
fi

AUTOLOGIN_DIR="/etc/systemd/system/getty@tty1.service.d"
AUTOLOGIN_DROPIN="${AUTOLOGIN_DIR}/autologin.conf"
ensure_root_dir "${AUTOLOGIN_DIR}" 755 root root
AUTOLOGIN_CONTENT=$(cat <<EOF_AUTO
[Service]
ExecStart=
ExecStart=-/sbin/agetty --autologin ${SERVICE_USER} --noclear %I \$TERM
EOF_AUTO
)

log INFO "Configuring console autologin for ${SERVICE_USER}"
autologin_status=$(write_root_config_if_changed "${AUTOLOGIN_DROPIN}" 644 root root "${AUTOLOGIN_CONTENT}") || true
case "${autologin_status}" in
    0)
        log INFO "Autologin drop-in updated"
        ;;
    1)
        log INFO "Autologin drop-in already up to date"
        ;;
    2)
        ;;
    *)
        log WARN "Unexpected status ${autologin_status} while writing ${AUTOLOGIN_DROPIN}"
        ;;
esac

if [[ "${DRY_RUN}" != "1" ]]; then
    run_sudo systemctl daemon-reload
    run_sudo systemctl try-restart getty@tty1.service || true
fi

log INFO "Ensuring seatd service is enabled"
if [[ "${DRY_RUN}" == "1" ]]; then
    log INFO "DRY_RUN: would enable and start seatd.service if available"
else
    if run_sudo systemctl list-unit-files seatd.service >/dev/null 2>&1; then
        if ! run_sudo systemctl is-enabled --quiet seatd.service; then
            run_sudo systemctl enable seatd.service
        fi
        if ! run_sudo systemctl is-active --quiet seatd.service; then
            run_sudo systemctl start seatd.service
        fi
    else
        log WARN "seatd.service not found; ensure seatd package is installed."
    fi
fi

ensure_user_dir "${USER_HOME}/.config/photo-frame"
PROFILE_SNIPPET_TARGET="${USER_HOME}/.config/photo-frame/kiosk-login.sh"
INSTALL_ROOT_ESC="$(printf '%s' "${INSTALL_ROOT}" | sed 's/[\\&/]/\\\\&/g')"
PROFILE_SNIPPET_CONTENT="$(sed "s|@INSTALL_ROOT@|${INSTALL_ROOT_ESC}|g" "${KIOSK_SNIPPET_SRC}")"

snippet_status=$(write_user_file_if_changed "${PROFILE_SNIPPET_TARGET}" "${PROFILE_SNIPPET_CONTENT}") || true
case "${snippet_status}" in
    0)
        log INFO "Installed kiosk login snippet"
        ;;
    1)
        log INFO "Kiosk login snippet already up to date"
        ;;
    2)
        ;;
    *)
        log WARN "Unexpected status ${snippet_status} while writing ${PROFILE_SNIPPET_TARGET}"
        ;;
esac

BASH_PROFILE="${USER_HOME}/.bash_profile"
PROFILE_SOURCE_LINE='[ -f "$HOME/.config/photo-frame/kiosk-login.sh" ] && source "$HOME/.config/photo-frame/kiosk-login.sh"'

if [[ "${DRY_RUN}" == "1" ]]; then
    log INFO "DRY_RUN: would ensure ${BASH_PROFILE} sources kiosk snippet"
else
    if [[ ! -f "${BASH_PROFILE}" ]]; then
        {
            printf '#!/usr/bin/env bash\n'
            printf '# Generated by rust-photo-frame setup to launch the kiosk session.\n'
            printf 'if [ -f "$HOME/.bashrc" ]; then\n'
            printf '  . "$HOME/.bashrc"\n'
            printf 'fi\n'
        } > "${BASH_PROFILE}"
        chmod 644 "${BASH_PROFILE}"
    fi
    if ! grep -Fqx "${PROFILE_SOURCE_LINE}" "${BASH_PROFILE}"; then
        {
            printf '\n# Auto-start rust-photo-frame when logging in on the console\n'
            printf '%s\n' "${PROFILE_SOURCE_LINE}"
        } >> "${BASH_PROFILE}"
    fi
fi

log INFO "Kiosk mode configuration applied"
