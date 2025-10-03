#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'

if [[ $(id -u) -ne 0 ]]; then
    echo "install-rust.sh must be run as root" >&2
    exit 1
fi

RUSTUP_HOME="/usr/local/rustup"
CARGO_HOME="/usr/local/cargo"
RUSTUP_BIN="${CARGO_HOME}/bin/rustup"

if [[ -x "${RUSTUP_BIN}" ]]; then
    echo "Rust toolchain already installed at ${CARGO_HOME}. Updating toolchain instead."
    env RUSTUP_HOME="${RUSTUP_HOME}" CARGO_HOME="${CARGO_HOME}" \
        "${RUSTUP_BIN}" update stable
else
    temp_dir=$(mktemp -d)
    trap 'rm -rf "${temp_dir}"' EXIT

    echo "Downloading rustup-init"
    curl -fsSL https://sh.rustup.rs -o "${temp_dir}/rustup-init.sh"
    chmod +x "${temp_dir}/rustup-init.sh"

    echo "Installing Rust toolchain to ${CARGO_HOME}"
    env RUSTUP_HOME="${RUSTUP_HOME}" CARGO_HOME="${CARGO_HOME}" \
        "${temp_dir}/rustup-init.sh" -y --no-modify-path --profile minimal --default-toolchain stable \
        --component clippy --component rustfmt

    rm -rf "${temp_dir}"
fi

env RUSTUP_HOME="${RUSTUP_HOME}" CARGO_HOME="${CARGO_HOME}" \
    "${RUSTUP_BIN}" component add rustfmt clippy

echo "Ensuring PATH export for system-wide cargo bin directory"
cat <<'PROFILE' >/etc/profile.d/cargo-bin.sh
export CARGO_HOME="${CARGO_HOME:-/usr/local/cargo}"
if [ -d "${CARGO_HOME}/bin" ]; then
    case ":${PATH}:" in
        *:"${CARGO_HOME}/bin":*) ;;
        *) PATH="${CARGO_HOME}/bin:${PATH}" ;;
    esac
fi
export PATH
PROFILE

chmod 0644 /etc/profile.d/cargo-bin.sh

if [[ -f /root/.profile ]] && ! grep -q 'cargo/bin' /root/.profile; then
    printf '\n# Added by Photo Frame setup: ensure cargo is on PATH\nexport PATH="%s/bin:${PATH}"\n' "${CARGO_HOME}" >> /root/.profile
fi

echo "Rust toolchain installation complete"
