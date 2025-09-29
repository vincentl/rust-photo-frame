#!/usr/bin/env bash
set -euo pipefail

MODULE="system:10-packages"
DRY_RUN="${DRY_RUN:-0}"

log() {
    local level="$1"; shift
    printf '[%s] %s\n' "${MODULE}" "$level: $*"
}

run_sudo() {
    if [[ "${DRY_RUN}" == "1" ]]; then
        log INFO "DRY_RUN: sudo $*"
    else
        sudo "$@"
    fi
}

log INFO "Ensuring apt package index is fresh"
run_sudo apt-get update

log INFO "Upgrading base operating system packages"
run_sudo env DEBIAN_FRONTEND=noninteractive apt-get -y dist-upgrade

PACKAGES=(
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
    network-manager
    dnsmasq-base
    iptables
)

log INFO "Installing required packages: ${PACKAGES[*]}"
run_sudo env DEBIAN_FRONTEND=noninteractive apt-get -y --no-install-recommends install "${PACKAGES[@]}"

log INFO "Cleaning up unused packages"
run_sudo apt-get -y autoremove
run_sudo apt-get -y autoclean

log INFO "System packages ready"
