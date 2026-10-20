#!/usr/bin/env bash
# Which stack runs the photo viewer. Exactly one is enabled at a time:
#
#   wayland (default)  greetd.service -> photoframe-session -> sway -> photoframe
#   kms (opt-in)       photoframe-kms.service: the viewer presents straight to
#                      DRM/KMS with no compositor (PHOTOFRAME_SURFACE=drm)
#
# The choice is persisted in /etc/photoframe/display.env so later deploys keep
# it; it changes only when PHOTOFRAME_DISPLAY is given explicitly:
#
#   PHOTOFRAME_DISPLAY=kms ./setup/application/deploy.sh       # opt in
#   PHOTOFRAME_DISPLAY=wayland ./setup/application/deploy.sh   # back to greetd/sway
#
# Until K2 lands, kms gives up the Wi-Fi recovery overlay and powerctl's
# display sleep (both talk to sway); see docs/advanced.md.
#
# Requires setup/lib/systemd.sh to be sourced first (read_env_value).

DISPLAY_ENV_PATH="${DISPLAY_ENV_PATH:-/etc/photoframe/display.env}"
DISPLAY_KMS_UNIT="photoframe-kms.service"
DISPLAY_WAYLAND_UNIT="greetd.service"

_display_stack_sudo() {
    if [[ $(id -u) -eq 0 ]]; then
        "$@"
    else
        sudo "$@"
    fi
}

_display_stack_valid() {
    [[ "$1" == "kms" || "$1" == "wayland" ]]
}

# Print the chosen stack (kms|wayland) without changing anything:
# PHOTOFRAME_DISPLAY if set, else the persisted choice, else wayland.
display_stack_current() {
    local choice="${PHOTOFRAME_DISPLAY:-}"
    if [[ -n "${choice}" ]]; then
        if ! _display_stack_valid "${choice}"; then
            echo "PHOTOFRAME_DISPLAY must be 'kms' or 'wayland' (got '${choice}')" >&2
            return 1
        fi
        printf '%s\n' "${choice}"
        return 0
    fi
    if [[ -f "${DISPLAY_ENV_PATH}" ]]; then
        choice="$(read_env_value PHOTOFRAME_DISPLAY "${DISPLAY_ENV_PATH}")"
        if _display_stack_valid "${choice}"; then
            printf '%s\n' "${choice}"
            return 0
        fi
        echo "ignoring invalid PHOTOFRAME_DISPLAY='${choice}' in ${DISPLAY_ENV_PATH}; using wayland" >&2
    fi
    printf 'wayland\n'
}

# Resolve the stack and, when PHOTOFRAME_DISPLAY was given, persist it.
display_stack_resolve() {
    local choice
    choice="$(display_stack_current)" || return 1
    if [[ -n "${PHOTOFRAME_DISPLAY:-}" ]]; then
        _display_stack_sudo install -d -m 0755 "$(dirname "${DISPLAY_ENV_PATH}")"
        printf '%s\n%s\n' \
            "# Display stack for the photo viewer (setup/lib/display-stack.sh): kms or wayland." \
            "PHOTOFRAME_DISPLAY=${choice}" |
            _display_stack_sudo tee "${DISPLAY_ENV_PATH}" >/dev/null
        _display_stack_sudo chmod 0644 "${DISPLAY_ENV_PATH}"
    fi
    printf '%s\n' "${choice}"
}

# The unit that runs the viewer for a stack.
display_stack_unit() {
    if [[ "$1" == "kms" ]]; then
        printf '%s\n' "${DISPLAY_KMS_UNIT}"
    else
        printf '%s\n' "${DISPLAY_WAYLAND_UNIT}"
    fi
}

# Enable a unit with only its current [Install] wants (stale *.wants links
# from older releases removed first, as 45-activate-services does).
_display_stack_enable_clean() {
    local unit="$1"
    _display_stack_sudo find /etc/systemd/system -type l -name "${unit}" -path '*/*.wants/*' -delete 2>/dev/null || true
    _display_stack_sudo systemctl enable "${unit}" >/dev/null 2>&1
}

# display_stack_apply <kms|wayland> <start|enable-only>
#
# Enables the chosen stack's unit and disables the other one. With "start",
# also stops the other stack and (re)starts the chosen one; photoframe-kms has
# Conflicts=greetd.service, so systemd never runs both. Prints one line per
# action on stdout; returns non-zero if the chosen unit could not be enabled
# or started.
display_stack_apply() {
    local choice="$1" mode="${2:-start}"
    local status=0
    if ! _display_stack_valid "${choice}"; then
        echo "display_stack_apply: unknown stack '${choice}'" >&2
        return 1
    fi

    if [[ "${choice}" == "kms" ]]; then
        echo "display stack: kms (${DISPLAY_KMS_UNIT}); disabling ${DISPLAY_WAYLAND_UNIT}"
        _display_stack_sudo systemctl disable "${DISPLAY_WAYLAND_UNIT}" >/dev/null 2>&1 || true
        if ! _display_stack_enable_clean "${DISPLAY_KMS_UNIT}"; then
            echo "display stack: failed to enable ${DISPLAY_KMS_UNIT}" >&2
            return 1
        fi
        if [[ "${mode}" == "start" ]]; then
            # Clear a StartLimit failure from an earlier attempt, then
            # (re)start; starting it stops greetd through Conflicts=.
            _display_stack_sudo systemctl reset-failed "${DISPLAY_KMS_UNIT}" >/dev/null 2>&1 || true
            _display_stack_sudo systemctl stop "${DISPLAY_WAYLAND_UNIT}" >/dev/null 2>&1 || true
            if _display_stack_sudo systemctl restart "${DISPLAY_KMS_UNIT}" >/dev/null 2>&1; then
                echo "display stack: started ${DISPLAY_KMS_UNIT}"
            else
                echo "display stack: failed to start ${DISPLAY_KMS_UNIT}; check 'journalctl -t photoframe -b'" >&2
                status=1
            fi
        fi
        return "${status}"
    fi

    echo "display stack: wayland (${DISPLAY_WAYLAND_UNIT}); disabling ${DISPLAY_KMS_UNIT}"
    if _display_stack_sudo systemctl list-unit-files "${DISPLAY_KMS_UNIT}" >/dev/null 2>&1; then
        _display_stack_sudo systemctl disable "${DISPLAY_KMS_UNIT}" >/dev/null 2>&1 || true
        # Release DRM master before sway needs it.
        _display_stack_sudo systemctl stop "${DISPLAY_KMS_UNIT}" >/dev/null 2>&1 || true
    fi
    local greetd_state
    greetd_state="$(_display_stack_sudo systemctl show -p UnitFileState --value "${DISPLAY_WAYLAND_UNIT}" 2>/dev/null || true)"
    case "${greetd_state}" in
        masked | masked-runtime)
            echo "display stack: ${DISPLAY_WAYLAND_UNIT} is masked; enable it manually once unmasked" >&2
            return 1
            ;;
        static) ;;
        *)
            if ! _display_stack_enable_clean "${DISPLAY_WAYLAND_UNIT}"; then
                echo "display stack: failed to enable ${DISPLAY_WAYLAND_UNIT}" >&2
                return 1
            fi
            ;;
    esac
    if [[ "${mode}" == "start" ]]; then
        # stop / sleep / start, never restart: see docs/operate.md.
        _display_stack_sudo systemctl reset-failed "${DISPLAY_WAYLAND_UNIT}" >/dev/null 2>&1 || true
        _display_stack_sudo systemctl stop "${DISPLAY_WAYLAND_UNIT}" >/dev/null 2>&1 || true
        sleep 1
        if _display_stack_sudo systemctl start "${DISPLAY_WAYLAND_UNIT}" >/dev/null 2>&1; then
            echo "display stack: started ${DISPLAY_WAYLAND_UNIT}"
        else
            echo "display stack: failed to start ${DISPLAY_WAYLAND_UNIT}; check 'systemctl status ${DISPLAY_WAYLAND_UNIT}'" >&2
            status=1
        fi
    fi
    return "${status}"
}
