#!/usr/bin/env bash
# Which stack runs the photo viewer. Exactly one is enabled at a time:
#
#   kms (default)   photoframe-kms.service: the viewer presents straight to
#                   DRM/KMS with no compositor (PHOTOFRAME_SURFACE=drm)
#   wayland         greetd.service -> photoframe-session -> sway -> photoframe
#                   (the fallback, kept for one release after KMS became the
#                   default)
#
# The choice is persisted in /etc/photoframe/display.env so later deploys keep
# it. With nothing given and nothing persisted the stack is kms. The
# persisted file changes only when PHOTOFRAME_DISPLAY is given explicitly:
#
#   PHOTOFRAME_DISPLAY=wayland ./setup/system/install.sh       # opt in to the fallback
#   PHOTOFRAME_DISPLAY=kms ./setup/application/deploy.sh       # back to the default
#
# A frame installed on the KMS stack does not have the compositor packages
# (greetd, sway, ...); switching to wayland therefore goes through the
# system stage, which installs them. See display_stack_packages and
# docs/advanced.md.
#
# On kms the viewer draws the Wi-Fi recovery overlay itself and switches the
# panel's power (powerctl asks it over the control socket); see
# docs/advanced.md.
#
# Requires setup/lib/systemd.sh to be sourced first (read_env_value).

DISPLAY_ENV_PATH="${DISPLAY_ENV_PATH:-/etc/photoframe/display.env}"
DISPLAY_KMS_UNIT="photoframe-kms.service"
DISPLAY_WAYLAND_UNIT="greetd.service"
# What the wayland stack needs beyond the base package set. Installed by the
# system stage only when that stack is chosen.
DISPLAY_WAYLAND_PACKAGES=(greetd seatd sway swaybg swayidle swaylock wayland-protocols)

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
# PHOTOFRAME_DISPLAY if set, else the persisted choice, else kms.
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
        echo "ignoring invalid PHOTOFRAME_DISPLAY='${choice}' in ${DISPLAY_ENV_PATH}; using kms" >&2
    fi
    printf 'kms\n'
}

# Where the choice came from, for status output: env (PHOTOFRAME_DISPLAY),
# persisted (${DISPLAY_ENV_PATH}) or default.
display_stack_source() {
    if [[ -n "${PHOTOFRAME_DISPLAY:-}" ]]; then
        printf 'env\n'
    elif [[ -f "${DISPLAY_ENV_PATH}" ]] &&
        _display_stack_valid "$(read_env_value PHOTOFRAME_DISPLAY "${DISPLAY_ENV_PATH}")"; then
        printf 'persisted\n'
    else
        printf 'default\n'
    fi
}

# One line naming the active stack, its unit and where the choice came from:
# "kms (photoframe-kms.service, default)".
display_stack_describe() {
    local choice
    choice="$(display_stack_current)" || return 1
    printf '%s (%s, %s)\n' "${choice}" "$(display_stack_unit "${choice}")" "$(display_stack_source)"
}

# The extra apt packages a stack needs, one per line: the compositor set for
# wayland, nothing for kms.
display_stack_packages() {
    if [[ "$1" == "wayland" ]]; then
        printf '%s\n' "${DISPLAY_WAYLAND_PACKAGES[@]}"
    fi
}

# True when PHOTOFRAME_DISPLAY=wayland was given but greetd is not installed.
_display_stack_wayland_unavailable() {
    [[ "${PHOTOFRAME_DISPLAY:-}" == "wayland" ]] &&
        ! _display_stack_sudo systemctl list-unit-files "${DISPLAY_WAYLAND_UNIT}" >/dev/null 2>&1
}

_display_stack_wayland_unavailable_message() {
    echo "display stack: ${DISPLAY_WAYLAND_UNIT} is not installed (this frame was set up on the kms stack), so PHOTOFRAME_DISPLAY=wayland is not applied; run 'PHOTOFRAME_DISPLAY=wayland ./setup/system/install.sh' to install the compositor packages and switch" >&2
}

# Call once, at top level (not in a command substitution), before anything
# reads the choice, in every stage that cannot install packages (the
# application deploy): an explicit wayland choice that cannot be applied is
# dropped from the environment, so every later display_stack_current,
# describe and unit lookup in this process and its children agrees with the
# stack that is actually running instead of naming one that is not installed.
display_stack_refuse_unavailable() {
    if _display_stack_wayland_unavailable; then
        _display_stack_wayland_unavailable_message
        unset PHOTOFRAME_DISPLAY
    fi
    return 0
}

# Resolve the stack and, when PHOTOFRAME_DISPLAY was given, persist it.
#
# An explicit wayland choice is refused (nothing persisted, the current
# persisted or default stack returned) while greetd is not installed: a frame
# set up on kms gets the compositor packages only from the system stage, and
# persisting the choice without them would leave display.env naming a stack
# that is not there. The system stage installs the packages before it
# resolves, so it is never refused.
display_stack_resolve() {
    local choice
    choice="$(display_stack_current)" || return 1
    if _display_stack_wayland_unavailable; then
        _display_stack_wayland_unavailable_message
        PHOTOFRAME_DISPLAY="" display_stack_current
        return
    fi
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
# from older releases removed, but only after the enable succeeded).
_display_stack_enable_clean() {
    local unit="$1" targets t
    local -a keep=()
    # Enable first: a failed enable must leave whatever wants the unit
    # already has, or a rerun of the stack that is already the enabled one
    # could leave the next boot with no display unit at all.
    _display_stack_sudo systemctl enable "${unit}" >/dev/null 2>&1 || return 1
    # Then prune wants left in targets the unit no longer names, read from
    # its [Install] WantedBy= (when that cannot be read, prune nothing).
    targets="$(_display_stack_sudo systemctl cat "${unit}" 2>/dev/null | sed -n 's/^WantedBy=//p' | tr '\n' ' ')"
    for t in ${targets}; do
        keep+=(! -path "*/${t}.wants/*")
    done
    if ((${#keep[@]})); then
        _display_stack_sudo find /etc/systemd/system -type l -name "${unit}" -path '*/*.wants/*' "${keep[@]}" -delete 2>/dev/null || true
    fi
    return 0
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
    # A frame set up on the kms stack has no greetd or sway; only the system
    # stage installs them. Leave the working stack alone and say how.
    if ! _display_stack_sudo systemctl list-unit-files "${DISPLAY_WAYLAND_UNIT}" >/dev/null 2>&1; then
        echo "display stack: ${DISPLAY_WAYLAND_UNIT} is not installed (this frame was set up on the kms stack); run 'PHOTOFRAME_DISPLAY=wayland ./setup/system/install.sh' to install the compositor packages and switch. Leaving ${DISPLAY_KMS_UNIT} as it is" >&2
        return 1
    fi
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
#
# Returns 0 healthy, 1 unhealthy, and 2 when the viewer is running but
# waiting for a display (no panel connected, or one in standby that has not
# asserted hot-plug; the journal has kms_waiting_for_display and no
# "presenting" line). That is the viewer working as designed, e.g. a frame
# provisioned over ssh before the panel is on, so callers warn, not fail.
# Usage: display_stack_kms_healthy [timeout_sec] [settle_sec]
display_stack_kms_healthy() {
    local timeout_sec="${1:-30}" settle_sec="${2:-6}"
    local waited=0 invocation="" journal="" waiting=0 restarts_before restarts_after
    while ((waited < timeout_sec)); do
        invocation="$(_display_stack_sudo systemctl show -p InvocationID --value "${DISPLAY_KMS_UNIT}" 2>/dev/null || true)"
        if [[ -n "${invocation}" ]]; then
            journal="$(_display_stack_sudo journalctl -q -o cat "_SYSTEMD_INVOCATION_ID=${invocation}" 2>/dev/null || true)"
            if grep -q "viewer presenting straight to KMS" <<<"${journal}"; then
                break
            fi
            if grep -q "kms_waiting_for_display" <<<"${journal}"; then
                waiting=1
            fi
        fi
        sleep 1
        waited=$((waited + 1))
    done
    if ((waited >= timeout_sec)); then
        if ((waiting)); then
            echo "${DISPLAY_KMS_UNIT}: the viewer is waiting for a display; connect and power on the panel and the slideshow starts (journalctl -t photoframe -b shows kms_waiting_for_display)" >&2
            return 2
        fi
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
