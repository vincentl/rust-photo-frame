#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SETUP_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"
FILES_DIR="${SETUP_DIR}/files"
WORDLIST_SRC="${FILES_DIR}/wordlist.txt"
WORDLIST_DEST="/opt/photo-frame/share/wordlist.txt"

if [[ $EUID -ne 0 ]]; then
    echo "[20-networking] This module must be run as root." >&2
    exit 1
fi

if ! command -v nmcli >/dev/null 2>&1; then
    echo "[20-networking] nmcli not found. Installing NetworkManager..."
    apt-get update
    apt-get install -y network-manager
fi

echo "[20-networking] Enabling NetworkManager service"
systemctl enable NetworkManager.service >/dev/null
systemctl restart NetworkManager.service

install -d -m 0755 /opt/photo-frame/share
if [[ -f "${WORDLIST_SRC}" ]]; then
    install -m 0644 "${WORDLIST_SRC}" "${WORDLIST_DEST}"
    echo "[20-networking] Installed hotspot wordlist to ${WORDLIST_DEST}"
else
    echo "[20-networking] ERROR: wordlist source not found at ${WORDLIST_SRC}" >&2
    exit 1
fi

# Simple sanity check for nmcli functionality
if ! nmcli -t -f WIFI g >/tmp/nmcli_check 2>&1; then
    echo "[20-networking] nmcli sanity check failed:" >&2
    cat /tmp/nmcli_check >&2
    exit 1
fi
rm -f /tmp/nmcli_check

echo "[20-networking] Networking prerequisites ready."
