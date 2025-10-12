#!/usr/bin/env bash
set -euo pipefail

MODULE="system:10-apt-packages"

log() {
    printf '[%s] %s\n' "${MODULE}" "$*"
}

PACKAGES=(
    at
    libinput10
    libwayland-client0
    libgbm1
    libdrm2
    mesa-vulkan-drivers
    seatd
    dbus
    dbus-user-session
    network-manager
    build-essential
    pkg-config
    libudev-dev
    libssl-dev
    libclang-dev
    clang
    cmake
    python3
    curl
    git
    rsync
    logrotate
    acl
    rclone
    sway
    swaybg
    swayidle
    swaylock
    vulkan-tools
    kmscube
)

log "Updating apt package index"
apt-get update

log "Installing required packages: ${PACKAGES[*]}"
DEBIAN_FRONTEND=noninteractive apt-get -y --no-install-recommends install "${PACKAGES[@]}"

log "Removing unused packages"
apt-get -y autoremove
apt-get -y autoclean

log "Package installation complete"

