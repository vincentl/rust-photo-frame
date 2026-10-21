#!/usr/bin/env bash
set -euo pipefail

MODULE="system:45-launcher"

log() {
    printf '[%s] %s\n' "${MODULE}" "$*"
}

# /usr/local/bin/photoframe starts the viewer on either display stack:
# photoframe-kms.service runs it (and sets PHOTOFRAME_SURFACE=drm), and on the
# wayland fallback sway execs it. docs/advanced.md ("The launcher and its
# environment") lists every knob it honours.
install_photo_launcher() {
    local launcher="/usr/local/bin/photoframe"
    log "Installing ${launcher}"
    install -d -m 0755 "$(dirname "${launcher}")"
    cat <<'LAUNCHER' >"${launcher}"
#!/usr/bin/env bash
set -euo pipefail

APP="/opt/photoframe/bin/photoframe"

if [[ ! -x "${APP}" ]]; then
  echo "[photoframe] binary not found at ${APP}" >&2
  exit 127
fi

# Keep WINIT_APP_ID for compatibility with older builds.
# Current binaries set Wayland app_id via winit window attributes.
export WINIT_APP_ID="${WINIT_APP_ID:-photoframe}"
# Environment the viewer reads (full list: docs/advanced.md, "The launcher
# and its environment"). Set them in the systemd unit (a drop-in from
# `systemctl edit photoframe-kms.service`) or uncomment here; this file is
# rewritten by the system stage.
#   PHOTOFRAME_SURFACE         drm | wayland. The KMS unit sets drm; unset
#                              means a Wayland client (the sway fallback).
#   PHOTOFRAME_PRESENT_MODE    fifo | mailbox | immediate. Default: fifo on
#                              KMS (vblank-locked), mailbox on Wayland.
#   PHOTOFRAME_TRANSITION_SCALE   1-4, default 2. 1 renders transitions at
#                              native resolution.
# Uncomment to render via the GL/EGL backend instead of Vulkan:
# export WGPU_BACKEND=gl
# Control where logs go via PHOTOFRAME_LOG (journal|stdout|file:/path)
# Defaults to journald for kiosk stability.
case "${PHOTOFRAME_LOG:-journal}" in
  journal)
    exec systemd-cat -t photoframe -- "${APP}" "$@"
    ;;
  stdout)
    exec "${APP}" "$@"
    ;;
  file:*)
    logfile="${PHOTOFRAME_LOG#file:}"
    # Ensure parent dir exists; ignore failure
    mkdir -p -- "$(dirname -- "$logfile")" 2>/dev/null || true
    # Append and flush; caller can tail the file
    exec "${APP}" "$@" >>"$logfile" 2>&1
    ;;
  *)
    exec systemd-cat -t photoframe -- "${APP}" "$@"
    ;;
esac
LAUNCHER
    chmod 0755 "${launcher}"
}

install_photo_launcher

log "launcher provisioning complete"
