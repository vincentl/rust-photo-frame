#!/usr/bin/env bash
set -euo pipefail

MODULE="system:20-rust"

log() {
    printf '[%s] %s\n' "${MODULE}" "$*"
}

RUSTUP_HOME="/usr/local/rustup"
CARGO_HOME="/usr/local/cargo"
RUSTUP_BIN="${CARGO_HOME}/bin/rustup"

if [[ -x "${RUSTUP_BIN}" ]]; then
    log "Rust toolchain already installed at ${CARGO_HOME}. Updating toolchain."
    env RUSTUP_HOME="${RUSTUP_HOME}" CARGO_HOME="${CARGO_HOME}" \
        "${RUSTUP_BIN}" update stable
else
    temp_dir=$(mktemp -d)
    trap 'rm -rf "${temp_dir}"' EXIT

    log "Downloading rustup-init"
    curl -fsSL https://sh.rustup.rs -o "${temp_dir}/rustup-init.sh"
    chmod +x "${temp_dir}/rustup-init.sh"

    log "Installing Rust toolchain to ${CARGO_HOME}"
    env RUSTUP_HOME="${RUSTUP_HOME}" CARGO_HOME="${CARGO_HOME}" \
        "${temp_dir}/rustup-init.sh" -y --no-modify-path --profile minimal --default-toolchain stable \
        --component clippy --component rustfmt

    rm -rf "${temp_dir}"
fi

env RUSTUP_HOME="${RUSTUP_HOME}" CARGO_HOME="${CARGO_HOME}" \
    "${RUSTUP_BIN}" component add rustfmt clippy

log "Ensuring PATH export for system-wide cargo bin directory"
cat <<'PROFILE' >/etc/profile.d/cargo-bin.sh
SYSTEM_CARGO_BIN="/usr/local/cargo/bin"

if [ "$(id -u)" -eq 0 ] && [ -f "/usr/local/cargo/env" ]; then
    # shellcheck source=/dev/null
    . /usr/local/cargo/env
fi

if [ -d "${SYSTEM_CARGO_BIN}" ]; then
    case ":${PATH}:" in
        *:"${SYSTEM_CARGO_BIN}":*) ;;
        *) PATH="${SYSTEM_CARGO_BIN}:${PATH}" ;;
    esac
fi

: "${CARGO_HOME:=${HOME}/.cargo}"
export PATH CARGO_HOME
PROFILE

chmod 0644 /etc/profile.d/cargo-bin.sh

log "Rust toolchain installation complete"

