#!/usr/bin/env bash
set -euo pipefail

MODULE_NAME="[10-power-button-override]"

echo "${MODULE_NAME} Configuring power button override..."

TARGET_USER="${SUDO_USER:-$USER}"
TARGET_HOME="$(eval echo ~"${TARGET_USER}")"
TARGET_GROUP="$(id -gn "${TARGET_USER}")"

run_as_target() {
    local command="$1"
    if [[ "${TARGET_USER}" == "root" ]]; then
        bash -lc "${command}"
    else
        sudo -u "${TARGET_USER}" bash -lc "${command}"
    fi
}

require_file_copy() {
    local source="$1"
    local destination="$2"
    local mode="$3"
    local owner="$4"
    local group="$5"

    if [[ -f "${source}" ]]; then
        install -o "${owner}" -g "${group}" -m "${mode}" "${source}" "${destination}"
        return 0
    fi
    return 1
}

CONFIG_DIR="${TARGET_HOME}/.config"
AUTOSTART_DIR="${CONFIG_DIR}/autostart"
AUTOSTART_FILE="${AUTOSTART_DIR}/pwrkey.desktop"
LABWC_DIR="${CONFIG_DIR}/labwc"
LABWC_RC="${LABWC_DIR}/rc.xml"

install -d -o "${TARGET_USER}" -g "${TARGET_GROUP}" "${AUTOSTART_DIR}"

if ! require_file_copy "/etc/xdg/autostart/pwrkey.desktop" "${AUTOSTART_FILE}" 644 "${TARGET_USER}" "${TARGET_GROUP}"; then
    if [[ ! -f "${AUTOSTART_FILE}" ]]; then
        cat <<'DESKTOP' > "${AUTOSTART_FILE}"
[Desktop Entry]
Type=Application
Name=pwrkey
Exec=pwrkey
DESKTOP
        chown "${TARGET_USER}:${TARGET_GROUP}" "${AUTOSTART_FILE}"
        chmod 644 "${AUTOSTART_FILE}"
    fi
fi

echo "${MODULE_NAME} Ensuring pwrkey autostart entry is hidden."
if [[ -f "${AUTOSTART_FILE}" ]]; then
    if grep -q '^Hidden=' "${AUTOSTART_FILE}"; then
        sed -i 's/^Hidden=.*/Hidden=true/' "${AUTOSTART_FILE}"
    else
        printf '\nHidden=true\n' >> "${AUTOSTART_FILE}"
    fi
    chown "${TARGET_USER}:${TARGET_GROUP}" "${AUTOSTART_FILE}"
fi

install -d -o "${TARGET_USER}" -g "${TARGET_GROUP}" "${LABWC_DIR}"
if require_file_copy "/etc/xdg/labwc/rc.xml" "${LABWC_RC}" 644 "${TARGET_USER}" "${TARGET_GROUP}"; then
    echo "${MODULE_NAME} Overriding Labwc power button command."
    sed -i 's|<command>pwrkey</command>|<command>/usr/bin/false</command>|g' "${LABWC_RC}"
else
    echo "${MODULE_NAME} [WARN] /etc/xdg/labwc/rc.xml not found. Skipping Labwc override." >&2
fi

if command -v systemctl >/dev/null 2>&1; then
    echo "${MODULE_NAME} Masking user-level pwrkey autostart service (if present)."
    run_as_target "systemctl --user mask app-pwrkey@autostart.service" || true
    run_as_target "systemctl --user daemon-reload" || true
else
    echo "${MODULE_NAME} [WARN] systemctl not available. Skipping user service mask." >&2
fi

UDEV_RULE="/etc/udev/rules.d/99-pwr-button.rules"
echo "${MODULE_NAME} Installing udev rule at ${UDEV_RULE}."
cat <<'RULE' > "${UDEV_RULE}"
SUBSYSTEM=="input", KERNEL=="event*", ATTRS{name}=="pwr_button", SYMLINK+="input/pwr_button", GROUP="input", MODE="0660"
RULE
chmod 644 "${UDEV_RULE}"

if command -v udevadm >/dev/null 2>&1; then
    echo "${MODULE_NAME} Reloading udev rules and triggering input subsystem."
    udevadm control --reload-rules
    udevadm trigger -s input
else
    echo "${MODULE_NAME} [WARN] udevadm not available. Please reload rules manually." >&2
fi

if [[ -e /dev/input/pwr_button ]]; then
    ls -l /dev/input/pwr_button
else
    echo "${MODULE_NAME} /dev/input/pwr_button not present (will appear after compatible hardware initializes)."
fi

echo "${MODULE_NAME} Adding ${TARGET_USER} to input group."
usermod -aG input "${TARGET_USER}"

echo "${MODULE_NAME} Power button override configuration complete."
