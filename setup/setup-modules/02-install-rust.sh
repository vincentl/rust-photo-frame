#!/usr/bin/env bash
set -euo pipefail

TARGET_USER="${FRAME_USER:-${SUDO_USER:-$USER}}"
TARGET_HOME="$(eval echo ~"${TARGET_USER}")"
CARGO_BIN="${TARGET_HOME}/.cargo/bin"

run_as_target() {
    if [[ "${TARGET_USER}" == "root" ]]; then
        bash -lc "$1"
    else
        sudo -u "${TARGET_USER}" bash -lc "$1"
    fi
}

echo "[02-install-rust] Installing Rust toolchain for user ${TARGET_USER}..."
if [[ -x "${CARGO_BIN}/rustup" ]]; then
    echo "[02-install-rust] rustup already installed. Updating toolchain."
    run_as_target "${CARGO_BIN}/rustup update"
else
    run_as_target "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y"
fi

echo "[02-install-rust] Ensuring stable toolchain is default..."
run_as_target "${CARGO_BIN}/rustup default stable"

echo "[02-install-rust] Installing build dependencies..."
apt-get install -y build-essential pkg-config libssl-dev libclang-dev clang cmake

echo "[02-install-rust] Rust installation complete."
