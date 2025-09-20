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
ensure_setting "hdmi_group" "2"
ensure_setting "hdmi_mode" "82"  # 1080p @ 60Hz
ensure_setting "hdmi_drive" "2"
ensure_setting "display_lcd_rotate" "0"

if ! grep -q '^dtoverlay=vc4-kms-v3d$' "${CONFIG_TXT}"; then
    echo "[01-configure-boot] Enabling KMS graphics overlay"
    printf '\ndtoverlay=vc4-kms-v3d\n' >> "${CONFIG_TXT}"
fi

echo "[01-configure-boot] Boot configuration updated. A reboot is recommended to apply changes."
