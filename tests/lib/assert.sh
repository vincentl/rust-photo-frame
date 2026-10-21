#!/usr/bin/env bash
set -Eeuo pipefail

# The unit that runs the viewer on this frame: photoframe-kms.service (the
# default) or greetd.service when the Wayland fallback is persisted in
# /etc/photoframe/display.env (setup/lib/display-stack.sh). Scripts use it as
# the default for PHOTO_SERVICE. Needs REPO_ROOT.
default_photo_service() {
  (
    # shellcheck disable=SC1091
    source "$REPO_ROOT/setup/lib/systemd.sh"
    # shellcheck disable=SC1091
    source "$REPO_ROOT/setup/lib/display-stack.sh"
    display_stack_unit "$(display_stack_current)"
  )
}

section() {
  printf '\n==== %s ====\n' "$1"
}

info() {
  printf '[INFO] %s\n' "$1"
}

warn() {
  printf 'WARN: %s\n' "$1" >&2
}

pass() {
  printf 'PASS: %s\n' "$1"
}

skip() {
  printf 'SKIP: %s\n' "$1"
}

fail() {
  printf 'FAIL: %s\n' "$1" >&2
  exit 1
}

log_cmd() {
  printf '  $ %s\n' "$*"
}

run_cmd() {
  local desc="$1"
  shift
  info "$desc"
  log_cmd "$*"
  if "$@"; then
    pass "$desc"
  else
    fail "$desc"
  fi
}

require_cmd() {
  local cmd="$1"
  if command -v "$cmd" >/dev/null 2>&1; then
    pass "Command available: $cmd"
  else
    fail "Missing required command: $cmd"
  fi
}

confirm() {
  local prompt="$1"
  local reply
  read -rp "$prompt [y/N]: " reply
  case "${reply:-}" in
    [yY]|[yY][eE][sS])
      pass "$prompt"
      return 0
      ;;
    *)
      fail "$prompt"
      ;;
  esac
}

confirm_or_skip() {
  local prompt="$1"
  local reply
  read -rp "$prompt [y/N/s to skip]: " reply
  case "${reply:-}" in
    [yY]|[yY][eE][sS])
      pass "$prompt"
      return 0
      ;;
    [sS])
      skip "$prompt"
      return 1
      ;;
    *)
      fail "$prompt"
      ;;
  esac
}

prompt_continue() {
  local prompt="$1"
  read -rp "$prompt Press Enter to continue..." _
}

with_timeout() {
  local seconds="$1"
  shift
  if command -v timeout >/dev/null 2>&1; then
    timeout "$seconds" "$@"
  else
    warn "timeout command unavailable; running without limit"
    "$@"
  fi
}
