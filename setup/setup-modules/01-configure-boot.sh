#!/usr/bin/env bash
set -euo pipefail

CONFIG_TXT="/boot/firmware/config.txt"
[[ -f "$CONFIG_TXT" ]] || CONFIG_TXT="/boot/config.txt"

echo "[01-configure-boot] Using configuration file: ${CONFIG_TXT}"

# Require root
if [[ $EUID -ne 0 ]]; then
  echo "[01-configure-boot] Please run as root (e.g., sudo $0)" >&2
  exit 1
fi

# Backup once per run
cp -a "${CONFIG_TXT}" "${CONFIG_TXT}.bak.$(date +%Y%m%d-%H%M%S)"

# Replace an existing key=... (anchored at start of line), else append.
set_kv() {
  local key="$1"
  local value="$2"
  if grep -qE "^${key}=" "${CONFIG_TXT}"; then
    echo "[01-configure-boot] Updating ${key}=${value}"
    # Use | as sed delimiter to avoid escaping /
    sed -i "s|^${key}=.*$|${key}=${value}|" "${CONFIG_TXT}"
  else
    echo "[01-configure-boot] Adding ${key}=${value}"
    printf '\n%s=%s\n' "${key}" "${value}" >> "${CONFIG_TXT}"
  fi
}

# Ensure an exact dtoverlay line exists; if not, append it.
ensure_overlay() {
  local overlay_line="dtoverlay=$1"
  if grep -qxF "${overlay_line}" "${CONFIG_TXT}"; then
    echo "[01-configure-boot] ${overlay_line} already present"
  else
    echo "[01-configure-boot] Adding ${overlay_line}"
    printf '\n%s\n' "${overlay_line}" >> "${CONFIG_TXT}"
  fi
}

# ---- 4K on HDMI-1 (top port) ----
# DMT group; mode 97 = 3840x2160 @ 60 Hz
set_kv "hdmi_force_hotplug:1" "1"
set_kv "hdmi_group:1"        "2"
set_kv "hdmi_mode:1"         "97"
set_kv "hdmi_drive:1"        "2"

# Pi 5 fan curve (temps in millideg C; levels 0-255)
ensure_overlay "pi5-fan,temp0=55000,level0=50,temp1=65000,level1=150,temp2=75000,level2=255"

sync
echo "[01-configure-boot] Boot configuration updated. Reboot to apply."
