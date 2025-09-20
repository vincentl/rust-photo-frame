#!/usr/bin/env bash
set -euo pipefail

echo "[00-update-os] Updating package lists..."
apt-get update

echo "[00-update-os] Upgrading installed packages..."
DEBIAN_FRONTEND=noninteractive apt-get dist-upgrade -y

echo "[00-update-os] Autoremoving unused packages..."
apt-get autoremove -y
apt-get autoclean -y

echo "[00-update-os] OS update completed."
