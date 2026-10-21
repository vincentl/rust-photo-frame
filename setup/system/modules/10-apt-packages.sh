#!/usr/bin/env bash
set -euo pipefail

MODULE="system:10-apt-packages"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib/systemd.sh
source "${SCRIPT_DIR}/../lib/systemd.sh"
# shellcheck source=../../lib/display-stack.sh
source "${SCRIPT_DIR}/../../lib/display-stack.sh"

log() {
    printf '[%s] %s\n' "${MODULE}" "$*"
}

PACKAGES=(
    acl
    at
    build-essential
    clang
    cmake
    curl
    jq
    dbus
    dbus-user-session
    git
    kmscube
    libclang-dev
    libdrm2
    libgbm1
    libinput10
    libssl-dev
    libudev-dev
    libwayland-client0
    # Mesa GL/GLES drivers (v3d gallium). Without these sway cannot create a
    # GPU renderer and silently composites every 4K frame on the CPU with
    # pixman, which also breaks direct scanout, and kmscube (the smoke test
    # for the display) has no GL either. --no-install-recommends below means
    # nothing pulls this in implicitly; mesa-vulkan-drivers only covers the
    # app's Vulkan path. Installed on both stacks.
    libgl1-mesa-dri
    logrotate
    mesa-vulkan-drivers
    network-manager
    fonts-dejavu-core
    fonts-noto-core
    pkg-config
    python3
    rclone
    rsync
    tmux
    vulkan-tools
)

# The compositor stack (greetd, seatd, sway, ...) is only for the wayland
# fallback; the default KMS stack runs the viewer without any of it.
# PHOTOFRAME_DISPLAY=wayland (or a persisted wayland choice) adds it; a frame
# set up on KMS gets it by rerunning this stage with that choice.
DISPLAY_CHOICE="$(display_stack_current)"
STACK_PACKAGE_COUNT=0
while IFS= read -r pkg; do
    PACKAGES+=("${pkg}")
    STACK_PACKAGE_COUNT=$((STACK_PACKAGE_COUNT + 1))
done < <(display_stack_packages "${DISPLAY_CHOICE}")
log "Display stack ${DISPLAY_CHOICE}: ${STACK_PACKAGE_COUNT} stack-specific package(s)"

log "Updating apt package index"
apt-get update

# Debian's greetd package enables and starts itself on install. On a frame
# being switched from KMS to the wayland fallback that would stop the running
# viewer (photoframe-kms Conflicts=greetd.service) and put greetd's default
# greeter on tty1 until 50-greetd.sh and 60-systemd.sh have run, minutes
# later. Ask invoke-rc.d not to start greetd during this install; the stack is
# started once at the end of install.sh. Skipped when a policy-rc.d already
# exists, and removed again on exit.
POLICY_RC="${POLICY_RC_PATH:-/usr/sbin/policy-rc.d}"
if [[ "${DISPLAY_CHOICE}" == "wayland" && ! -e "${POLICY_RC}" && -d "$(dirname "${POLICY_RC}")" ]]; then
    if printf '#!/bin/sh\n[ "$1" = greetd ] && exit 101\nexit 0\n' >"${POLICY_RC}" && chmod 0755 "${POLICY_RC}"; then
        trap 'rm -f "${POLICY_RC}"' EXIT
    else
        log "Could not write ${POLICY_RC}; greetd may start before it is configured"
    fi
fi

log "Installing required packages: ${PACKAGES[*]}"
DEBIAN_FRONTEND=noninteractive apt-get -y --no-install-recommends install "${PACKAGES[@]}"

log "Removing unused packages"
apt-get -y autoremove
apt-get -y autoclean

log "Package installation complete"
