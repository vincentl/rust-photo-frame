#!/usr/bin/env bash
set -euo pipefail

MODULE="system:10-apt-packages"

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
    logrotate
    mesa-vulkan-drivers
    network-manager
    fonts-dejavu-core
    fonts-noto-core
    pkg-config
    python3
    rclone
    rsync
    seatd
    sway
    swaybg
    swayidle
    swaylock
    tmux
    vulkan-tools
)

log "Updating apt package index"
apt-get update

log "Installing required packages: ${PACKAGES[*]}"
DEBIAN_FRONTEND=noninteractive apt-get -y --no-install-recommends install "${PACKAGES[@]}"

log "Removing unused packages"
apt-get -y autoremove
apt-get -y autoclean

log "Package installation complete"
