#!/usr/bin/env bash
# Tests for setup/assets/app/bin/powerctl with socat, swaymsg and pgrep
# stubbed on PATH and real Unix sockets (python3) standing in for the
# viewer's control socket and sway's IPC socket. Runs anywhere bash, jq and
# python3 run.
#
#   bash setup/tests/powerctl.test.sh
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
POWERCTL="${REPO_ROOT}/setup/assets/app/bin/powerctl"
WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT

for tool in jq python3; do
    command -v "${tool}" >/dev/null || { echo "powerctl tests need ${tool}" >&2; exit 1; }
done

make_socket() { python3 -c 'import socket, sys; socket.socket(socket.AF_UNIX).bind(sys.argv[1])' "$1"; }

BIN="${WORK}/bin"
mkdir -p "${BIN}" "${WORK}/run"
CONTROL="${WORK}/control.sock"
SWAY="${WORK}/run/sway-ipc.sock"
make_socket "${CONTROL}"
make_socket "${SWAY}"

# socat: records the request, prints ${WORK}/socat.reply.
cat >"${BIN}/socat" <<EOF
#!/usr/bin/env bash
cat >"${WORK}/socat.in"
echo "socat \$*" >>"${WORK}/calls"
cat "${WORK}/socat.reply" 2>/dev/null || true
EOF
# swaymsg: records its arguments; answers get_outputs.
cat >"${BIN}/swaymsg" <<EOF
#!/usr/bin/env bash
echo "swaymsg \$*" >>"${WORK}/calls"
if [[ " \$* " == *" get_outputs"* ]]; then
  echo '[{"name":"HDMI-A-1","power":true}]'
fi
EOF
cat >"${BIN}/pgrep" <<EOF
#!/usr/bin/env bash
echo 4242
EOF
chmod +x "${BIN}"/*

failures=0
fail() {
    echo "FAIL: $*" >&2
    failures=$((failures + 1))
}
reset() {
    : >"${WORK}/calls"
    rm -f "${WORK}/socat.in" "${WORK}/socat.reply"
}
reply() { printf '%s\n' "$1" >"${WORK}/socat.reply"; }
called() { grep -q -F -- "$1" "${WORK}/calls"; }
run() {
    PATH="${BIN}:${PATH}" \
        PHOTOFRAME_CONTROL_SOCKET="${CONTROL_SOCKET_UNDER_TEST:-${CONTROL}}" \
        XDG_RUNTIME_DIR="${RUNTIME_UNDER_TEST:-${WORK}/run}" \
        SWAYSOCK="${SWAY}" \
        bash "${POWERCTL}" "$@"
}

# --- the viewer takes it --------------------------------------------------
reset
reply '{"ok":true,"state":"off","connector":"HDMI-A-2","mode":"3840x2160@60"}'
run sleep || fail "sleep via the viewer failed"
[[ "$(cat "${WORK}/socat.in")" == '{"command":"display-power","state":"off"}' ]] ||
    fail "sleep sent $(cat "${WORK}/socat.in")"
called swaymsg && fail "sleep ran swaymsg although the viewer took it"

reset
reply '{"ok":true,"state":"on","connector":"HDMI-A-2","mode":"3840x2160@60"}'
run wake HDMI-A-2 || fail "wake via the viewer failed"
[[ "$(cat "${WORK}/socat.in")" == '{"command":"display-power","state":"on","output":"HDMI-A-2"}' ]] ||
    fail "wake with an output sent $(cat "${WORK}/socat.in")"
called swaymsg && fail "wake ran swaymsg although the viewer took it"

reset
reply '{"ok":true,"state":"off","connector":"HDMI-A-2","mode":"3840x2160@60"}'
[[ "$(run state)" == "off" ]] || fail "state did not print the viewer's state"
[[ "$(cat "${WORK}/socat.in")" == '{"command":"display-power"}' ]] ||
    fail "state sent $(cat "${WORK}/socat.in")"
called swaymsg && fail "state ran swaymsg although the viewer answered"

# On KMS there is no /run/user/<uid>: the viewer path must not need it.
reset
reply '{"ok":true,"state":"off","connector":"HDMI-A-2","mode":"3840x2160@60"}'
RUNTIME_UNDER_TEST="${WORK}/no-such-dir" run sleep || fail "sleep needed a runtime dir on the viewer path"

# --- fallback to sway -----------------------------------------------------
reset
reply '{"ok":false,"error":"unsupported","detail":"the compositor owns display power on this stack"}'
run sleep || fail "sleep fallback failed"
called "swaymsg -s ${SWAY} output * power off" || fail "unsupported did not fall back to swaymsg"

reset
reply '{"ok":false,"error":"not-driven","detail":"this viewer drives HDMI-A-2"}'
run wake HDMI-A-1 || fail "wake fallback failed"
called "swaymsg -s ${SWAY} output HDMI-A-1 power on" || fail "not-driven did not fall back to swaymsg"

reset
CONTROL_SOCKET_UNDER_TEST="${WORK}/missing.sock" run sleep || fail "sleep without a viewer failed"
called socat && fail "socat ran without a control socket"
called "output * power off" || fail "no viewer did not fall back to swaymsg"

reset
: >"${WORK}/socat.reply" # an old viewer reads and closes without a reply
[[ "$(run state)" == "on" ]] || fail "state did not fall back to sway's get_outputs"
called "get_outputs" || fail "state fallback did not query sway"

# Neither the viewer nor sway: a clear error, not a hang.
reset
if CONTROL_SOCKET_UNDER_TEST="${WORK}/missing.sock" RUNTIME_UNDER_TEST="${WORK}/no-such-dir" \
    run sleep 2>"${WORK}/stderr"; then
    fail "sleep succeeded with neither the viewer nor sway"
fi
grep -q "did not take the request" "${WORK}/stderr" || fail "missing explanation: $(cat "${WORK}/stderr")"

# --- usage ----------------------------------------------------------------
status=0
run reboot 2>/dev/null || status=$?
[[ ${status} -eq 2 ]] || fail "bad mode exited ${status}, expected 2"

if ((failures)); then
    echo "powerctl tests: ${failures} failure(s)" >&2
    exit 1
fi
echo "powerctl tests: all passed"
