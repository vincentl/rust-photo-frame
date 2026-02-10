#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"

# shellcheck source=../lib/systemd.sh
source "${SCRIPT_DIR}/../lib/systemd.sh"

INSTALL_ROOT="${INSTALL_ROOT:-/opt/photoframe}"
SERVICE_USER="${SERVICE_USER:-kiosk}"

log() {
  local level="$1"; shift
  printf '[verify] %s\n' "$level: $*"
}

ok()   { log "OK"    "$*"; }
warn() { log "WARN"  "$*"; }
err()  { log "ERROR" "$*"; }

failures=0
warnings=0

have() { command -v "$1" >/dev/null 2>&1; }

# Binaries and config presence
BIN_APP="${INSTALL_ROOT}/bin/photoframe"
BIN_WIFI="${INSTALL_ROOT}/bin/wifi-manager"
BIN_BUTTOND="${INSTALL_ROOT}/bin/buttond"
BIN_POWERCTL="${INSTALL_ROOT}/bin/powerctl"
CONF_TEMPLATE="${INSTALL_ROOT}/etc/photoframe/config.yaml"
CONF_ACTIVE="/etc/photoframe/config.yaml"
WORDLIST_PATH="${INSTALL_ROOT}/share/wordlist.txt"

if [[ -x "${BIN_APP}" ]]; then
  ok "photoframe binary present: ${BIN_APP}"
else
  err "photoframe binary missing: ${BIN_APP}"
  failures=$((failures+1))
fi

if [[ -x "${BIN_WIFI}" ]]; then
  ok "wifi-manager binary present: ${BIN_WIFI}"
else
  err "wifi-manager binary missing: ${BIN_WIFI}"
  failures=$((failures+1))
fi

if [[ -x "${BIN_BUTTOND}" ]]; then
  ok "buttond binary present: ${BIN_BUTTOND}"
else
  warn "buttond binary missing (optional): ${BIN_BUTTOND}"
  warnings=$((warnings+1))
fi

if [[ -x "${BIN_POWERCTL}" ]]; then
  ok "powerctl helper present: ${BIN_POWERCTL}"
else
  warn "powerctl helper missing (optional): ${BIN_POWERCTL}"
  warnings=$((warnings+1))
fi

if [[ -f "${CONF_TEMPLATE}" ]]; then
  ok "config template present: ${CONF_TEMPLATE}"
else
  err "config template missing: ${CONF_TEMPLATE}"
  failures=$((failures+1))
fi

if [[ -f "${CONF_ACTIVE}" ]]; then
  ok "active system config present: ${CONF_ACTIVE}"
else
  warn "active system config missing: ${CONF_ACTIVE} (use template to seed)"
  warnings=$((warnings+1))
fi

if [[ -f "${WORDLIST_PATH}" ]]; then
  ok "wordlist present: ${WORDLIST_PATH}"
else
  err "wordlist missing: ${WORDLIST_PATH}"
  failures=$((failures+1))
fi

# Var tree and ownership
VAR_DIR="/var/lib/photoframe"
if [[ -d "${VAR_DIR}" ]]; then
  owner="$(stat -c %U "${VAR_DIR}")"
  group="$(stat -c %G "${VAR_DIR}")"
  if [[ "${owner}:${group}" == "${SERVICE_USER}:${SERVICE_USER}" ]]; then
    ok "${VAR_DIR} owned by ${SERVICE_USER}:${SERVICE_USER}"
  else
    err "${VAR_DIR} owner ${owner}:${group}, expected ${SERVICE_USER}:${SERVICE_USER}"
    failures=$((failures+1))
  fi
else
  err "var directory missing: ${VAR_DIR}"
  failures=$((failures+1))
fi

# Toolchain versions (informational)
rustc_v=$(rustc --version 2>/dev/null || echo "rustc unavailable")
cargo_v=$(cargo --version 2>/dev/null || echo "cargo unavailable")
ok "toolchain: ${rustc_v}; ${cargo_v}"

# Swap/zram check
if have swapon; then
  if swapon --show=NAME --noheadings | grep -q '^/dev/zram'; then
    ok "zram swap active: $(swapon --show=NAME,SIZE --noheadings | tr -s ' ')"
  else
    warn "zram swap inactive; run 'sudo ./setup/system/install.sh' or reboot"
    warnings=$((warnings+1))
  fi
else
  warn "swapon not available; skipping zram check"
  warnings=$((warnings+1))
fi

# Power helper deps
if [[ -x "${BIN_POWERCTL}" ]]; then
  have jq && have swaymsg || {
    warn "powerctl dependencies missing (jq and/or sway); install with system stage"
    warnings=$((warnings+1))
  }
fi

# systemd service health
if systemd_available; then
  check_unit() {
    local unit="$1" level_on_fail="$2" desc="$3"
    if ! systemd_unit_exists "${unit}"; then
      [[ -n "${desc}" ]] && warn "${unit} not installed (${desc})" || warn "${unit} not installed"
      warnings=$((warnings+1))
      return 0
    fi
    if systemd_is_active "${unit}"; then
      ok "${unit} active"
      return 0
    fi
    systemd_status "${unit}" || true
    case "${level_on_fail}" in
      ERROR) err "${unit} not active"; failures=$((failures+1)); ;;
      WARN|*) warn "${unit} not active"; warnings=$((warnings+1)); ;;
    esac
  }

  check_seatd() {
    local socket_exists=0
    local socket_active=0
    local service_exists=0
    local service_active=0

    if systemd_unit_exists seatd.socket; then
      socket_exists=1
      if systemd_is_active seatd.socket; then
        socket_active=1
      fi
    fi

    if systemd_unit_exists seatd.service; then
      service_exists=1
      if systemd_is_active seatd.service; then
        service_active=1
      fi
    fi

    if (( socket_exists == 0 && service_exists == 0 )); then
      err "seatd not installed (seatd.socket/seatd.service missing)"
      failures=$((failures+1))
      return
    fi

    if (( socket_active == 1 || service_active == 1 )); then
      if (( socket_active == 1 && service_active == 1 )); then
        ok "seatd.socket and seatd.service active"
      elif (( socket_active == 1 )); then
        ok "seatd.socket active"
      else
        ok "seatd.service active"
      fi
      return
    fi

    if (( socket_exists == 1 )); then
      systemd_status seatd.socket || true
    fi
    if (( service_exists == 1 )); then
      systemd_status seatd.service || true
    fi
    err "seatd installed but not active (expected seatd.socket or seatd.service)"
    failures=$((failures+1))
  }

  check_unit greetd.service ERROR "set via system install"
  check_seatd
  check_unit photoframe-wifi-manager.service ERROR "installed by app deploy"
  check_unit buttond.service WARN "optional, installed by app deploy"
  check_unit photoframe-sync.timer WARN "optional periodic sync"

  # display-manager alias (informational)
  if systemctl status display-manager.service --no-pager >/dev/null 2>&1; then
    ok "display-manager alias present"
  else
    warn "display-manager alias missing or inactive"
    warnings=$((warnings+1))
  fi
else
  warn "systemctl unavailable; skipping service checks"
  warnings=$((warnings+1))
fi

echo
log INFO "Summary: ${failures} error(s), ${warnings} warning(s)"

exit $(( failures > 0 ? 1 : 0 ))
