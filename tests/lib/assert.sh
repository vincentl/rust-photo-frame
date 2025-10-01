#!/usr/bin/env bash
set -Eeuo pipefail

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
