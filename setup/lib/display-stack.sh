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
# On kms the viewer draws the Wi-Fi recovery overlay itself and switches the
# panel's power (powerctl asks it over the control socket); see
# docs/advanced.md.
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
# Enables the chosen stack's unit, and only once that succeeded disables the
# other one, so a failed enable never leaves the frame with no stack enabled
# for the next boot. With "start", the chosen unit is (re)started even if
# enabling it failed (as the old greetd-only code did); photoframe-kms has
# Conflicts=greetd.service, so starting either stops the other and systemd
# never runs both. Prints one line per action; returns non-zero if any step
# failed, after doing every step it could.
display_stack_apply() {
    local choice="$1" mode="${2:-start}"
    local status=0
    if ! _display_stack_valid "${choice}"; then
        echo "display_stack_apply: unknown stack '${choice}'" >&2
        return 1
    fi

    if [[ "${choice}" == "kms" ]]; then
        echo "display stack: kms (${DISPLAY_KMS_UNIT})"
        if _display_stack_enable_clean "${DISPLAY_KMS_UNIT}"; then
            echo "display stack: enabled ${DISPLAY_KMS_UNIT}; disabling ${DISPLAY_WAYLAND_UNIT}"
            _display_stack_sudo systemctl disable "${DISPLAY_WAYLAND_UNIT}" >/dev/null 2>&1 || true
        else
            echo "display stack: failed to enable ${DISPLAY_KMS_UNIT}; leaving ${DISPLAY_WAYLAND_UNIT} enabled for the next boot" >&2
            status=1
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

    echo "display stack: wayland (${DISPLAY_WAYLAND_UNIT})"
    local greetd_state greetd_enabled=0
    greetd_state="$(_display_stack_sudo systemctl show -p UnitFileState --value "${DISPLAY_WAYLAND_UNIT}" 2>/dev/null || true)"
    case "${greetd_state}" in
        masked | masked-runtime)
            echo "display stack: ${DISPLAY_WAYLAND_UNIT} is masked; unmask it, then rerun. Leaving ${DISPLAY_KMS_UNIT} as it is" >&2
            return 1
            ;;
        static)
            greetd_enabled=1
            ;;
        *)
            if _display_stack_enable_clean "${DISPLAY_WAYLAND_UNIT}"; then
                greetd_enabled=1
            else
                echo "display stack: failed to enable ${DISPLAY_WAYLAND_UNIT}; leaving ${DISPLAY_KMS_UNIT} enabled for the next boot" >&2
                status=1
            fi
            ;;
    esac
    if ((greetd_enabled)) && _display_stack_sudo systemctl list-unit-files "${DISPLAY_KMS_UNIT}" >/dev/null 2>&1; then
        echo "display stack: enabled ${DISPLAY_WAYLAND_UNIT}; disabling ${DISPLAY_KMS_UNIT}"
        _display_stack_sudo systemctl disable "${DISPLAY_KMS_UNIT}" >/dev/null 2>&1 || true
    fi
    if [[ "${mode}" == "start" ]]; then
        # Release DRM master before sway needs it (Conflicts= would also stop
        # it when greetd starts), then stop / sleep / start greetd, never
        # restart: see docs/operate.md.
        _display_stack_sudo systemctl stop "${DISPLAY_KMS_UNIT}" >/dev/null 2>&1 || true
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

# Postcheck for the KMS stack: the current run of photoframe-kms.service
# reached "viewer presenting straight to KMS" and then stayed up without a
# restart for a few seconds. "active" alone is not enough: with
# Restart=always a crash-looping unit is active about a third of the time,
# and the control socket is bound before the viewer opens the card.
# Usage: display_stack_kms_healthy [timeout_sec] [settle_sec]
display_stack_kms_healthy() {
    local timeout_sec="${1:-30}" settle_sec="${2:-6}"
    local waited=0 invocation="" restarts_before restarts_after
    while ((waited < timeout_sec)); do
        invocation="$(_display_stack_sudo systemctl show -p InvocationID --value "${DISPLAY_KMS_UNIT}" 2>/dev/null || true)"
        if [[ -n "${invocation}" ]] &&
            _display_stack_sudo journalctl -q -o cat "_SYSTEMD_INVOCATION_ID=${invocation}" 2>/dev/null |
            grep -q "viewer presenting straight to KMS"; then
            break
        fi
        sleep 1
        waited=$((waited + 1))
    done
    if ((waited >= timeout_sec)); then
        echo "${DISPLAY_KMS_UNIT}: the viewer did not reach the display within ${timeout_sec}s; see 'journalctl -t photoframe -b'" >&2
        return 1
    fi
    restarts_before="$(_display_stack_sudo systemctl show -p NRestarts --value "${DISPLAY_KMS_UNIT}" 2>/dev/null || echo "?")"
    sleep "${settle_sec}"
    restarts_after="$(_display_stack_sudo systemctl show -p NRestarts --value "${DISPLAY_KMS_UNIT}" 2>/dev/null || echo "?")"
    if [[ "${restarts_before}" != "${restarts_after}" ]] ||
        ! _display_stack_sudo systemctl is-active --quiet "${DISPLAY_KMS_UNIT}"; then
        echo "${DISPLAY_KMS_UNIT}: restarted or stopped while settling (NRestarts ${restarts_before} -> ${restarts_after}); see 'journalctl -t photoframe -b'" >&2
        return 1
    fi
    echo "${DISPLAY_KMS_UNIT}: presenting and stable for ${settle_sec}s (NRestarts ${restarts_after})"
}

# KMS display check for verify.sh: the card, connector and mode, from two
# sources that must agree. sysfs says which connectors are connected and
# which modes they list; the viewer (`display-power` query on its control
# socket) says which connector and mode it actually drives and whether the
# panel is on. Prints one finding per line as LEVEL<TAB>message, LEVEL being
# OK, WARN or ERROR, and returns 1 when it printed an ERROR.
#
# A panel switched off by display-power may drop hot-plug detect, so a
# disconnected connector is only a warning while the viewer reports it off.
#
# DISPLAY_SYSFS_ROOT (default /sys/class/drm) and PHOTOFRAME_CONTROL_SOCKET
# (default /run/photoframe/control.sock) are overridable for tests.
display_stack_kms_check() {
    local sysfs="${DISPLAY_SYSFS_ROOT:-/sys/class/drm}"
    local socket="${PHOTOFRAME_CONTROL_SOCKET:-/run/photoframe/control.sock}"
    local status_file connected=()
    for status_file in "${sysfs}"/card*-*/status; do
        [[ -f "${status_file}" ]] || continue
        if [[ "$(cat "${status_file}")" == "connected" ]]; then
            connected+=("$(basename "$(dirname "${status_file}")")")
        fi
    done

    local reply=""
    if [[ -S "${socket}" ]] && command -v socat >/dev/null 2>&1 && command -v jq >/dev/null 2>&1; then
        reply="$(printf '%s' '{"command":"display-power"}' |
            socat -t 3 - "UNIX-CONNECT:${socket}" 2>/dev/null || true)"
    fi
    if ! jq -e '.ok == true and (.connector | type) == "string"' >/dev/null 2>&1 <<<"${reply}"; then
        if ((${#connected[@]} == 0)); then
            printf 'ERROR\tno connected display connector under %s, and the viewer did not report one\n' "${sysfs}"
            return 1
        fi
        printf 'WARN\tconnected: %s; the viewer did not report the display it drives (is %s running?)\n' \
            "${connected[*]}" "${DISPLAY_KMS_UNIT}"
        return 0
    fi

    local connector mode power entry="" size card
    connector="$(jq -r '.connector' <<<"${reply}")"
    mode="$(jq -r '.mode // "?"' <<<"${reply}")"
    power="$(jq -r '.state // "?"' <<<"${reply}")"
    for status_file in "${sysfs}"/card*-"${connector}"/status; do
        [[ -f "${status_file}" ]] && entry="$(dirname "${status_file}")"
    done
    if [[ -z "${entry}" ]]; then
        printf 'ERROR\tthe viewer drives %s, which no card under %s has\n' "${connector}" "${sysfs}"
        return 1
    fi
    card="$(basename "${entry}")"
    card="${card%%-*}"
    if [[ "$(cat "${entry}/status")" != "connected" ]]; then
        if [[ "${power}" == "off" ]]; then
            printf 'WARN\t%s %s reads %s while the panel is switched off (display-power off); expected for sinks that drop hot-plug detect\n' \
                "${card}" "${connector}" "$(cat "${entry}/status")"
            return 0
        fi
        printf 'ERROR\tthe viewer drives %s %s, which reads %s\n' "${card}" "${connector}" "$(cat "${entry}/status")"
        return 1
    fi
    size="${mode%@*}"
    if ! grep -qx -- "${size}" "${entry}/modes" 2>/dev/null; then
        printf 'ERROR\t%s %s no longer lists the mode in use (%s)\n' "${card}" "${connector}" "${mode}"
        return 1
    fi
    printf 'OK\t%s %s %s power=%s\n' "${card}" "${connector}" "${mode}" "${power}"
}
