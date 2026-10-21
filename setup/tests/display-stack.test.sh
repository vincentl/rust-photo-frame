#!/usr/bin/env bash
# Tests for setup/lib/display-stack.sh with systemctl, journalctl, sudo,
# find and sleep stubbed out: runs anywhere bash runs (no systemd needed).
#
#   bash setup/tests/display-stack.test.sh
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT

# shellcheck source=../lib/systemd.sh
source "${REPO_ROOT}/setup/lib/systemd.sh"
# shellcheck source=../lib/display-stack.sh
source "${REPO_ROOT}/setup/lib/display-stack.sh"

# Read by the library.
export DISPLAY_ENV_PATH="${WORK}/display.env"
CALLS="${WORK}/calls"

# --- stubs -------------------------------------------------------------
FAIL_ENABLE=""       # unit whose `systemctl enable` fails
FAIL_START=""        # unit whose start/restart fails
GREETD_STATE="enabled"
NRESTARTS="${WORK}/nrestarts" # successive NRestarts values, one per line
# (a file, because the library reads it from command substitutions)
JOURNAL_LINE=""      # what journalctl prints for the current invocation
sudo() { "$@"; }
find() { :; }
sleep() { :; }
id() { if [[ "${1:-}" == "-u" ]]; then echo 1000; else command id "$@"; fi; }
journalctl() { [[ -n "${JOURNAL_LINE}" ]] && printf '%s\n' "${JOURNAL_LINE}"; return 0; }
systemctl() {
    echo "systemctl $*" >>"${CALLS}"
    case "$1" in
        enable) [[ "$2" != "${FAIL_ENABLE}" ]] ;;
        start | restart) [[ "$2" != "${FAIL_START}" ]] ;;
        show)
            case "$3" in
                UnitFileState) echo "${GREETD_STATE}" ;;
                InvocationID) echo "abc123" ;;
                NRestarts)
                    head -n1 "${NRESTARTS}" 2>/dev/null || echo 0
                    tail -n +2 "${NRESTARTS}" >"${NRESTARTS}.next" 2>/dev/null || true
                    mv "${NRESTARTS}.next" "${NRESTARTS}"
                    ;;
            esac
            ;;
        *) return 0 ;;
    esac
}

# --- harness -----------------------------------------------------------
failures=0
fail() {
    echo "FAIL: $*" >&2
    failures=$((failures + 1))
}
reset_stubs() {
    : >"${CALLS}"
    FAIL_ENABLE=""
    FAIL_START=""
    GREETD_STATE="enabled"
    : >"${NRESTARTS}"
    JOURNAL_LINE=""
}
# line number of the first call matching $1 (0 if absent)
call_line() { grep -n -m1 -F -- "$1" "${CALLS}" | cut -d: -f1 || true; }
called() { grep -q -F -- "$1" "${CALLS}"; }

# --- choice resolution -------------------------------------------------
reset_stubs
unset PHOTOFRAME_DISPLAY
[[ "$(display_stack_current)" == "wayland" ]] || fail "default stack is not wayland"
PHOTOFRAME_DISPLAY=kms display_stack_resolve >/dev/null
[[ "$(display_stack_current)" == "kms" ]] || fail "kms choice was not persisted"
if PHOTOFRAME_DISPLAY=bogus display_stack_current >/dev/null 2>&1; then
    fail "invalid PHOTOFRAME_DISPLAY accepted"
fi
PHOTOFRAME_DISPLAY=wayland display_stack_resolve >/dev/null
[[ "$(display_stack_current)" == "wayland" ]] || fail "switch back to wayland not persisted"
[[ "$(display_stack_unit kms)" == "photoframe-kms.service" ]] || fail "kms unit name"

# --- kms: enable the new stack before disabling the old one ------------
reset_stubs
display_stack_apply kms start >/dev/null || fail "kms apply reported failure"
enable_kms="$(call_line "systemctl enable photoframe-kms.service")"
disable_greetd="$(call_line "systemctl disable greetd.service")"
[[ -n "${enable_kms}" && -n "${disable_greetd}" && "${enable_kms}" -lt "${disable_greetd}" ]] ||
    fail "kms: greetd disabled before photoframe-kms was enabled"
called "systemctl restart photoframe-kms.service" || fail "kms: unit not (re)started"

# kms enable fails: greetd stays enabled, start still happens, failure reported
reset_stubs
FAIL_ENABLE="photoframe-kms.service"
if display_stack_apply kms start >/dev/null 2>&1; then fail "kms: enable failure not reported"; fi
called "systemctl disable greetd.service" && fail "kms: greetd disabled although kms enable failed"
called "systemctl restart photoframe-kms.service" || fail "kms: start skipped after enable failure"

# --- wayland: same order, same failure handling ------------------------
reset_stubs
display_stack_apply wayland start >/dev/null || fail "wayland apply reported failure"
enable_greetd="$(call_line "systemctl enable greetd.service")"
disable_kms="$(call_line "systemctl disable photoframe-kms.service")"
[[ -n "${enable_greetd}" && -n "${disable_kms}" && "${enable_greetd}" -lt "${disable_kms}" ]] ||
    fail "wayland: photoframe-kms disabled before greetd was enabled"
called "systemctl start greetd.service" || fail "wayland: greetd not started"
called "systemctl restart greetd.service" && fail "wayland: greetd restarted (must be stop/start)"

reset_stubs
FAIL_ENABLE="greetd.service"
if display_stack_apply wayland start >/dev/null 2>&1; then fail "wayland: enable failure not reported"; fi
called "systemctl disable photoframe-kms.service" && fail "wayland: kms disabled although greetd enable failed"
called "systemctl start greetd.service" || fail "wayland: greetd start skipped after enable failure"

reset_stubs
GREETD_STATE="masked"
if display_stack_apply wayland start >/dev/null 2>&1; then fail "wayland: masked greetd not reported"; fi
called "systemctl disable photoframe-kms.service" && fail "wayland: kms disabled although greetd is masked"
called "systemctl stop photoframe-kms.service" && fail "wayland: kms stopped although greetd is masked"

reset_stubs
display_stack_apply kms enable-only >/dev/null || fail "enable-only reported failure"
called "systemctl restart" && fail "enable-only started a unit"

# --- KMS postcheck -------------------------------------------------------
reset_stubs
JOURNAL_LINE="INFO viewer presenting straight to KMS width=3840 height=2160"
printf '2\n2\n' >"${NRESTARTS}"
display_stack_kms_healthy 3 1 >/dev/null || fail "healthy KMS viewer judged unhealthy"

reset_stubs
JOURNAL_LINE="INFO viewer presenting straight to KMS width=3840 height=2160"
printf '2\n3\n' >"${NRESTARTS}"
display_stack_kms_healthy 3 1 >/dev/null 2>&1 && fail "crash-looping KMS unit judged healthy"

reset_stubs
JOURNAL_LINE="WARN kms_viewer_exiting_with_error: not DRM master"
display_stack_kms_healthy 3 1 >/dev/null 2>&1 && fail "KMS unit that never presented judged healthy"

# --- KMS display check (verify.sh) ---------------------------------------
# A fake /sys/class/drm and a real Unix socket for the viewer; socat is a
# function that answers with ${VIEWER_REPLY}.
SYSFS="${WORK}/drm"
mkdir -p "${SYSFS}/card1-HDMI-A-1" "${SYSFS}/card1-HDMI-A-2" "${SYSFS}/card0-Writeback-1"
echo disconnected >"${SYSFS}/card1-HDMI-A-1/status"
echo connected >"${SYSFS}/card1-HDMI-A-2/status"
printf '3840x2160\n1920x1080\n' >"${SYSFS}/card1-HDMI-A-2/modes"
echo unknown >"${SYSFS}/card0-Writeback-1/status"
python3 -c 'import socket, sys; socket.socket(socket.AF_UNIX).bind(sys.argv[1])' "${WORK}/control.sock"
export DISPLAY_SYSFS_ROOT="${SYSFS}" PHOTOFRAME_CONTROL_SOCKET="${WORK}/control.sock"
VIEWER_REPLY=""
socat() { cat >/dev/null; printf '%s\n' "${VIEWER_REPLY}"; }
kms_check() { display_stack_kms_check 2>&1; }

VIEWER_REPLY='{"ok":true,"state":"on","connector":"HDMI-A-2","mode":"3840x2160@60"}'
out="$(kms_check)" || fail "healthy KMS display judged failing: ${out}"
[[ "${out}" == $'OK\tcard1 HDMI-A-2 3840x2160@60 power=on' ]] || fail "KMS check line: ${out}"

VIEWER_REPLY='{"ok":true,"state":"on","connector":"HDMI-A-1","mode":"3840x2160@60"}'
out="$(kms_check)" && fail "a disconnected driven connector passed: ${out}"
[[ "${out}" == ERROR* ]] || fail "disconnected connector not an ERROR: ${out}"

VIEWER_REPLY='{"ok":true,"state":"off","connector":"HDMI-A-1","mode":"3840x2160@60"}'
out="$(kms_check)" || fail "a switched-off panel that dropped HPD failed: ${out}"
[[ "${out}" == WARN* ]] || fail "switched-off panel not a WARN: ${out}"

VIEWER_REPLY='{"ok":true,"state":"on","connector":"HDMI-A-2","mode":"2560x1440@60"}'
out="$(kms_check)" && fail "a mode the panel no longer lists passed: ${out}"
[[ "${out}" == *"no longer lists"* ]] || fail "missing mode message: ${out}"

VIEWER_REPLY='{"ok":true,"state":"on","connector":"DP-1","mode":"3840x2160@60"}'
out="$(kms_check)" && fail "an unknown connector passed: ${out}"

VIEWER_REPLY='{"ok":false,"error":"unsupported","detail":"the compositor owns display power on this stack"}'
out="$(kms_check)" || fail "an unanswered query is not an error: ${out}"
[[ "${out}" == WARN*"HDMI-A-2"* ]] || fail "unanswered query should warn and list the connected connector: ${out}"

echo disconnected >"${SYSFS}/card1-HDMI-A-2/status"
out="$(kms_check)" && fail "no connected connector and no viewer passed: ${out}"
[[ "${out}" == ERROR* ]] || fail "nothing connected not an ERROR: ${out}"
unset -f socat

if ((failures)); then
    echo "display-stack tests: ${failures} failure(s)" >&2
    exit 1
fi
echo "display-stack tests: all passed"
