#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd "$SCRIPT_DIR/../../.." && pwd)
FILES_DIR="$SCRIPT_DIR/files"

BIN_DIR=/opt/photo-frame/bin
SYSTEMD_UNIT=/etc/systemd/system/photo-buttond.service
LOGIND_DIR=/etc/systemd/logind.conf.d
LOGIND_DROPIN="$LOGIND_DIR/10-photo-ignore-powerkey.conf"
UDEV_RULE=/etc/udev/rules.d/99-input-perms.rules
SHUTDOWN_HELPER="$BIN_DIR/photo-safe-shutdown"
SERVICE_USER=frame

pushd "$REPO_ROOT" >/dev/null
cargo build --release -p photo-buttond
popd >/dev/null

install -d -m 0755 -o root -g root "$BIN_DIR"
install -m 0755 -o root -g root "$REPO_ROOT/target/release/photo-buttond" "$BIN_DIR/photo-buttond"
install -m 0750 -o root -g "$SERVICE_USER" "$FILES_DIR/photo-safe-shutdown" "$SHUTDOWN_HELPER"
install -m 0644 -o root -g root "$FILES_DIR/photo-buttond.service" "$SYSTEMD_UNIT"
install -d -m 0755 -o root -g root "$LOGIND_DIR"
install -m 0644 -o root -g root "$FILES_DIR/logind.conf.d/10-photo-ignore-powerkey.conf" "$LOGIND_DROPIN"
install -m 0644 -o root -g root "$FILES_DIR/99-input-perms.rules" "$UDEV_RULE"

if ! id -nG "$SERVICE_USER" | tr ' ' '\n' | grep -qx "input"; then
  usermod -a -G input "$SERVICE_USER"
fi

udevadm control --reload
udevadm trigger
systemctl daemon-reload
systemctl enable --now photo-buttond.service
systemctl restart systemd-logind

echo "Photo button candidates (override with --device if needed):"
ls /dev/input/by-path/*power* 2>/dev/null || echo "  (none found)"
