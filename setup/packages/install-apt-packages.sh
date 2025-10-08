#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'

if [[ $(id -u) -ne 0 ]]; then
    echo "install-packages.sh must be run as root" >&2
    exit 1
fi

PACKAGES=(
    at
    cage
    libinput10
    libwayland-client0
    libgbm1
    libdrm2
    mesa-vulkan-drivers
    seatd
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
    vulkan-tools
    kmscube
)

echo "Updating apt package index"
apt-get update

echo "Installing required packages: ${PACKAGES[*]}"
DEBIAN_FRONTEND=noninteractive apt-get -y --no-install-recommends install "${PACKAGES[@]}"

echo "Removing unused packages"
apt-get -y autoremove
apt-get -y autoclean

echo "Package installation complete"
