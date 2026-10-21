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
UNIT_WANTED_BY="graphical.target"  # [Install] WantedBy= that `systemctl cat` prints
KMS_ENABLED=0       # 1: photoframe-kms.service is enabled
GREETD_RUNNING=0     # 1: greetd is enabled and active (a Sway frame)
GREETD_INSTALLED=1   # 0: `systemctl list-unit-files greetd.service` finds nothing
NRESTARTS="${WORK}/nrestarts" # successive NRestarts values, one per line
# (a file, because the library reads it from command substitutions)
JOURNAL_LINE=""      # what journalctl prints for the current invocation
sudo() { "$@"; }
find() { echo "find $*" >>"${CALLS}"; }
sleep() { :; }
id() { if [[ "${1:-}" == "-u" ]]; then echo 1000; else command id "$@"; fi; }
journalctl() { [[ -n "${JOURNAL_LINE}" ]] && printf '%s\n' "${JOURNAL_LINE}"; return 0; }
systemctl() {
    echo "systemctl $*" >>"${CALLS}"
    case "$1" in
        enable) [[ "$2" != "${FAIL_ENABLE}" ]] ;;
        start | restart) [[ "$2" != "${FAIL_START}" ]] ;;
        cat) [[ -z "${UNIT_WANTED_BY}" ]] || printf '[Install]\nWantedBy=%s\n' "${UNIT_WANTED_BY}" ;;
        is-active) [[ "$3" != "greetd.service" || "${GREETD_RUNNING}" -eq 1 ]] ;;
        is-enabled) if [[ "$3" == "greetd.service" ]]; then [[ "${GREETD_RUNNING}" -eq 1 ]]; else [[ "${KMS_ENABLED}" -eq 1 ]]; fi ;;
        list-unit-files) [[ "$2" != "greetd.service" || "${GREETD_INSTALLED}" -eq 1 ]] ;;
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
    GREETD_INSTALLED=1
    GREETD_RUNNING=0
    KMS_ENABLED=0
    UNIT_WANTED_BY="graphical.target"
    : >"${NRESTARTS}"
    JOURNAL_LINE=""
}
# line number of the first call matching $1 (0 if absent)
call_line() { grep -n -m1 -F -- "$1" "${CALLS}" | cut -d: -f1 || true; }
called() { grep -q -F -- "$1" "${CALLS}"; }

# --- choice resolution -------------------------------------------------
# kms is the default; wayland is an explicit, persisted opt-in.
reset_stubs
unset PHOTOFRAME_DISPLAY
rm -f "${DISPLAY_ENV_PATH}"
[[ "$(display_stack_current)" == "kms" ]] || fail "default stack is not kms"
[[ "$(display_stack_source)" == "default" ]] || fail "default stack not reported as default"
[[ "$(display_stack_describe)" == "kms (photoframe-kms.service, default)" ]] ||
    fail "describe line for the default: $(display_stack_describe)"
[[ "$(display_stack_resolve)" == "kms" ]] || fail "resolve with nothing chosen is not kms"
[[ ! -e "${DISPLAY_ENV_PATH}" ]] || fail "an implicit default was persisted"
if PHOTOFRAME_DISPLAY=bogus display_stack_current >/dev/null 2>&1; then
    fail "invalid PHOTOFRAME_DISPLAY accepted"
fi
[[ "$(display_stack_unit kms)" == "photoframe-kms.service" ]] || fail "kms unit name"
[[ "$(display_stack_unit wayland)" == "greetd.service" ]] || fail "wayland unit name"

# A wayland choice is persisted and survives a re-run with nothing given
# (what `./setup/application/deploy.sh` or a later system install does).
PHOTOFRAME_DISPLAY=wayland display_stack_resolve >/dev/null
[[ "$(display_stack_current)" == "wayland" ]] || fail "wayland choice was not persisted"
[[ "$(display_stack_source)" == "persisted" ]] || fail "persisted choice not reported as persisted"
[[ "$(display_stack_describe)" == "wayland (greetd.service, persisted)" ]] ||
    fail "describe line for the persisted choice: $(display_stack_describe)"
before="$(cat "${DISPLAY_ENV_PATH}")"
for rerun in 1 2; do
    [[ "$(display_stack_resolve)" == "wayland" ]] || fail "re-run ${rerun} lost the persisted wayland choice"
done
[[ "$(cat "${DISPLAY_ENV_PATH}")" == "${before}" ]] || fail "a re-run rewrote the persisted choice"
[[ "$(display_stack_current)" == "wayland" ]] || fail "persisted wayland choice did not survive re-runs"
# ... and the apply that follows it keeps the wayland stack
reset_stubs
display_stack_apply "$(display_stack_resolve)" start >/dev/null || fail "re-run apply reported failure"
called "systemctl start greetd.service" || fail "re-run with a persisted wayland choice did not start greetd"
called "systemctl restart photoframe-kms.service" && fail "re-run with a persisted wayland choice started the kms unit"
# an explicit choice still overrides the persisted one, and is persisted in turn
PHOTOFRAME_DISPLAY=kms display_stack_resolve >/dev/null
[[ "$(display_stack_current)" == "kms" ]] || fail "switch back to kms not persisted"
[[ "$(display_stack_source)" == "persisted" ]] || fail "persisted kms choice not reported as persisted"
# an unreadable persisted value falls back to the default, with a warning
printf 'PHOTOFRAME_DISPLAY=bogus\n' >"${DISPLAY_ENV_PATH}"
[[ "$(display_stack_current 2>/dev/null)" == "kms" ]] || fail "invalid persisted value did not fall back to kms"
[[ "$(display_stack_source)" == "default" ]] || fail "invalid persisted value reported as persisted"
rm -f "${DISPLAY_ENV_PATH}"

# What the rest of a deploy sees after the refusal: one shell, resolve then
# every later lookup (describe, unit, current) must name the running stack.
reset_stubs
GREETD_INSTALLED=0
rm -f "${DISPLAY_ENV_PATH}"
PHOTOFRAME_DISPLAY=wayland
display_stack_refuse_unavailable 2>/dev/null
[[ -z "${PHOTOFRAME_DISPLAY+x}" ]] || fail "refusal left PHOTOFRAME_DISPLAY set"
[[ "$(display_stack_current)" == "kms" ]] || fail "after the refusal current is not kms"
[[ "$(display_stack_describe)" == "kms (photoframe-kms.service, default)" ]] || fail "after the refusal describe names another stack: $(display_stack_describe)"
[[ "$(display_stack_unit "$(display_stack_current)")" == "photoframe-kms.service" ]] || fail "after the refusal the unit is not the kms one"
# an explicit choice that can be applied is kept
GREETD_INSTALLED=1
PHOTOFRAME_DISPLAY=wayland
display_stack_refuse_unavailable
[[ "${PHOTOFRAME_DISPLAY:-}" == "wayland" ]] || fail "an applicable wayland choice was dropped"
unset PHOTOFRAME_DISPLAY
reset_stubs

# On a frame set up on kms (no greetd installed) an explicit wayland choice is
# refused and not persisted: the deploy stage cannot install the packages.
reset_stubs
GREETD_INSTALLED=0
rm -f "${DISPLAY_ENV_PATH}"
out="$(PHOTOFRAME_DISPLAY=wayland display_stack_resolve 2>/dev/null)" || fail "refused wayland choice made resolve fail"
[[ "${out}" == "kms" ]] || fail "refused wayland choice did not resolve to the current stack: ${out}"
[[ ! -e "${DISPLAY_ENV_PATH}" ]] || fail "a wayland choice without greetd was persisted"
err="$(PHOTOFRAME_DISPLAY=wayland display_stack_resolve 2>&1 >/dev/null)" || true
[[ "${err}" == *"PHOTOFRAME_DISPLAY=wayland ./setup/system/install.sh"* ]] || fail "refusal does not give the command: ${err}"
# ... and keeps whatever was persisted
printf 'PHOTOFRAME_DISPLAY=kms\n' >"${DISPLAY_ENV_PATH}"
[[ "$(PHOTOFRAME_DISPLAY=wayland display_stack_resolve 2>/dev/null)" == "kms" ]] || fail "refusal changed the persisted stack"
# a kms choice never needs greetd
PHOTOFRAME_DISPLAY=kms display_stack_resolve >/dev/null 2>&1 || fail "kms choice refused without greetd"
rm -f "${DISPLAY_ENV_PATH}"
reset_stubs

# --- the system stage never displaces a running Sway stack for an old viewer
# (a v1.1 frame updated with install-all.sh or system/install.sh) ---------------
export DISPLAY_VIEWER_BIN="${WORK}/photoframe"
old_viewer() { printf 'ELF v1.1 viewer: WAYLAND_DISPLAY\n' >"${DISPLAY_VIEWER_BIN}"; chmod +x "${DISPLAY_VIEWER_BIN}"; }
new_viewer() { printf 'ELF v1.2 viewer: PHOTOFRAME_SURFACE wayland drm\n' >"${DISPLAY_VIEWER_BIN}"; chmod +x "${DISPLAY_VIEWER_BIN}"; }
no_viewer() { rm -f "${DISPLAY_VIEWER_BIN}"; }
touched_greetd() { called "systemctl disable greetd.service" || called "systemctl stop greetd.service"; }

# v1.1 Sway frame, system stage on its own (both update paths run it)
reset_stubs; GREETD_RUNNING=1; old_viewer
display_stack_system_apply kms start 2>/dev/null || fail "held stage reported failure"
called "systemctl enable photoframe-kms.service" && fail "system stage enabled kms over a running Sway stack with an old viewer"
touched_greetd && fail "system stage disabled or stopped greetd for an old viewer"
called "photoframe-kms.service" && fail "system stage touched the kms unit for an old viewer"
reset_stubs; GREETD_RUNNING=1; old_viewer
display_stack_system_apply kms enable-only 2>/dev/null
called "systemctl enable photoframe-kms.service" && fail "60-systemd path enabled kms over a running Sway stack"
# ... the hold message says who switches
reset_stubs; GREETD_RUNNING=1; old_viewer
out="$(display_stack_system_apply kms start 2>&1)"
[[ "${out}" == *"deploy.sh"* ]] || fail "hold message does not name the deploy: ${out}"
# the same frame with no viewer installed at all is held too
reset_stubs; GREETD_RUNNING=1; no_viewer
display_stack_system_apply kms start >/dev/null 2>&1
touched_greetd && fail "system stage displaced greetd with no viewer installed"
# the deploy-only path (45-activate) switches once the new viewer is in place
reset_stubs; GREETD_RUNNING=1; new_viewer
display_stack_apply kms start >/dev/null || fail "deploy-time switch failed"
called "systemctl disable greetd.service" || fail "deploy-time switch did not retire greetd"
called "systemctl restart photoframe-kms.service" || fail "deploy-time switch did not start kms"
out="$(display_stack_apply kms start 2>&1)"
[[ "${out}" == *"to go back to Sway by hand"* ]] || fail "no manual way back printed: ${out}"
# a system stage that finds a KMS-capable viewer may switch
reset_stubs; GREETD_RUNNING=1; new_viewer
display_stack_system_apply kms start >/dev/null 2>&1 || fail "capable viewer: switch failed"
called "systemctl restart photoframe-kms.service" || fail "capable viewer: system stage did not start kms"
# fresh install: enable kms, but do not start a unit with no viewer
reset_stubs; no_viewer
display_stack_system_apply kms start >/dev/null 2>&1 || fail "fresh install failed"
called "systemctl enable photoframe-kms.service" || fail "fresh install did not enable kms"
called "systemctl restart photoframe-kms.service" && fail "fresh install started kms without a viewer"
# an explicit wayland choice is never held
reset_stubs; GREETD_RUNNING=0; old_viewer
display_stack_system_apply wayland start >/dev/null 2>&1 || fail "wayland system apply failed"
called "systemctl start greetd.service" || fail "wayland choice was held"
no_viewer
unset DISPLAY_VIEWER_BIN

# --- verify.sh: the other stack must not stay enabled -------------------------
reset_stubs; GREETD_RUNNING=1; KMS_ENABLED=1
[[ "$(display_stack_other_enabled kms)" == "greetd.service" ]] || fail "enabled greetd not reported on kms"
GREETD_RUNNING=0
[[ -z "$(display_stack_other_enabled kms)" ]] || fail "kms with greetd disabled reported something"
[[ -z "$(display_stack_other_enabled wayland)" ]] || fail "wayland with kms disabled reported something"
KMS_ENABLED=1; GREETD_RUNNING=1
[[ "$(display_stack_other_enabled wayland)" == "photoframe-kms.service" ]] || fail "enabled kms not reported on wayland"

# a v1.1 frame held on Sway after a failed build: kms chosen but not enabled,
# greetd running, old viewer. Nothing may tell the operator to disable greetd.
export DISPLAY_VIEWER_BIN="${WORK}/photoframe"
reset_stubs; GREETD_RUNNING=1; KMS_ENABLED=0; old_viewer
[[ -z "$(display_stack_other_enabled kms)" ]] || fail "held frame: greetd reported as the leftover stack"
display_stack_hold_for_deploy kms || fail "held frame not recognised as held"
# once the deploy installed the viewer and enabled kms, greetd left enabled is the conflict
new_viewer; KMS_ENABLED=1
[[ "$(display_stack_other_enabled kms)" == "greetd.service" ]] || fail "both enabled not reported"
display_stack_hold_for_deploy kms && fail "a KMS-capable viewer still reads as held"
no_viewer
unset DISPLAY_VIEWER_BIN

# --- system stage guard (deploy.sh --skip-build) -----------------------------
# getent/id stubbed: the user exists, and only the groups in ${GROUPS_PRESENT}.
GROUPS_PRESENT="video render input"
getent() { [[ "$1" == group ]] && [[ " ${GROUPS_PRESENT} " == *" $2 "* ]]; }
id() { if [[ "${1:-}" == "-u" && "${2:-}" == "kiosk" ]]; then echo 1001; elif [[ "${1:-}" == "-u" && -n "${2:-}" ]]; then return 1; elif [[ "${1:-}" == "-u" ]]; then echo 1000; else command id "$@"; fi; }
# kms needs no seat group (seatd is not installed there)
[[ -z "$(display_stack_system_stage_missing kms kiosk)" ]] || fail "kms guard demands more than video/render/input: $(display_stack_system_stage_missing kms kiosk)"
# wayland does
[[ "$(display_stack_system_stage_missing wayland kiosk)" == "group seat" ]] || fail "wayland guard does not demand the seat group"
GROUPS_PRESENT="video render input seat"
[[ -z "$(display_stack_system_stage_missing wayland kiosk)" ]] || fail "wayland guard fails with every group present"
# a stage that never ran fails on both stacks
GROUPS_PRESENT="seat"
[[ "$(display_stack_system_stage_missing kms kiosk | paste -sd, -)" == "group video,group render,group input" ]] || fail "kms guard misses absent groups"
[[ "$(display_stack_system_stage_missing kms nobody)" == "user nobody" ]] || fail "guard does not report a missing user"
unset -f getent
id() { if [[ "${1:-}" == "-u" ]]; then echo 1000; else command id "$@"; fi; }

# --- packages ------------------------------------------------------------
[[ -z "$(display_stack_packages kms)" ]] || fail "the kms stack installs compositor packages"
wayland_packages="$(display_stack_packages wayland)"
for pkg in greetd seatd sway; do
    grep -qx -- "${pkg}" <<<"${wayland_packages}" || fail "wayland packages lack ${pkg}"
done

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

# a rerun of the stack that is already enabled: enable first, prune stale
# wants only after it succeeded and only outside the unit's own target
reset_stubs
display_stack_apply kms enable-only >/dev/null || fail "kms rerun reported failure"
enable_kms="$(call_line "systemctl enable photoframe-kms.service")"
prune="$(call_line "find /etc/systemd/system -type l -name photoframe-kms.service")"
[[ -n "${enable_kms}" && -n "${prune}" && "${enable_kms}" -lt "${prune}" ]] || fail "kms: stale wants pruned before the enable"
grep -F "find " "${CALLS}" | grep -qF -- '! -path */graphical.target.wants/*' || fail "kms: prune would delete the wants in the unit's own target"
reset_stubs
FAIL_ENABLE="photoframe-kms.service"
display_stack_apply kms enable-only >/dev/null 2>&1 && fail "kms rerun: enable failure not reported"
called "find " && fail "kms rerun: wants pruned although the enable failed (could leave no stack at boot)"
reset_stubs
UNIT_WANTED_BY=""
display_stack_apply kms enable-only >/dev/null || fail "kms rerun without a readable WantedBy reported failure"
called "find " && fail "kms: pruned wants without knowing the unit's target"

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

# a frame set up on kms has no greetd: switching is refused with the command
# to run, and the working stack is left alone
reset_stubs
GREETD_INSTALLED=0
out="$(display_stack_apply wayland start 2>&1)" && fail "wayland: missing greetd not reported"
[[ "${out}" == *"PHOTOFRAME_DISPLAY=wayland ./setup/system/install.sh"* ]] || fail "wayland: no switch hint: ${out}"
called "systemctl disable photoframe-kms.service" && fail "wayland: kms disabled although greetd is not installed"
called "systemctl stop photoframe-kms.service" && fail "wayland: kms stopped although greetd is not installed"
called "systemctl start greetd.service" && fail "wayland: greetd start attempted although it is not installed"

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

# the viewer is waiting for a panel: not a failure (status 2), and a viewer
# that never logged anything still is one (status 1, above)
reset_stubs
JOURNAL_LINE="WARN kms_waiting_for_display"
printf '2\n2\n' >"${NRESTARTS}"
rc=0
out="$(display_stack_kms_healthy 3 1 2>&1)" || rc=$?
[[ "${rc}" == 2 ]] || fail "waiting-for-display viewer returned ${rc}, want 2: ${out}"
[[ "${out}" == *"waiting for a display"* ]] || fail "waiting-for-display message: ${out}"

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

# --- the viewer's control socket path (review L2) ----------------------------
cat >"${WORK}/viewer.yaml" <<'YAML'
photo-library-path: /var/lib/photoframe/photos
control-socket-path: "/run/custom/control.sock"   # an override
YAML
[[ "$(PHOTOFRAME_CONFIG="${WORK}/viewer.yaml" display_stack_control_socket)" == "/run/custom/control.sock" ]] ||
    fail "control-socket-path override not read"
printf 'photo-library-path: /x\n' >"${WORK}/viewer.yaml"
[[ "$(PHOTOFRAME_CONFIG="${WORK}/viewer.yaml" display_stack_control_socket)" == "/run/photoframe/control.sock" ]] ||
    fail "default control socket not used when the config sets none"
[[ "$(PHOTOFRAME_CONFIG="${WORK}/missing.yaml" display_stack_control_socket)" == "/run/photoframe/control.sock" ]] ||
    fail "default control socket not used without a config"

if ((failures)); then
    echo "display-stack tests: ${failures} failure(s)" >&2
    exit 1
fi
echo "display-stack tests: all passed"
