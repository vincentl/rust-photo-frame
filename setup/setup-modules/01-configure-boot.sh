#!/usr/bin/env bash
set -euo pipefail

CONFIG_TXT="/boot/firmware/config.txt"
if [[ ! -f "${CONFIG_TXT}" ]]; then
    CONFIG_TXT="/boot/config.txt"
fi

echo "[01-configure-boot] Using configuration file: ${CONFIG_TXT}"

ensure_setting() {
    local key="$1"
    local value="$2"
    if grep -q "^${key}=${value}$" "${CONFIG_TXT}"; then
        echo "[01-configure-boot] ${key} already set to ${value}"
    else
        echo "[01-configure-boot] Setting ${key}=${value}"
        printf '\n%s=%s\n' "${key}" "${value}" >> "${CONFIG_TXT}"
    fi
}

ensure_setting "disable_splash" "1"
ensure_setting "hdmi_force_hotplug" "1"
ensure_setting "hdmi_enable_4kp60" "1"
ensure_setting "dtoverlay" "vc4-kms-v3d"
ensure_setting "dtoverlay" "pi5-fan,temp0=55000,level0=50,temp1=65000,level1=150,temp2=75000,level2=255"

echo "[01-configure-boot] Boot configuration updated. A reboot is recommended to apply changes."
