#!/usr/bin/env bash
set -euo pipefail

MODULE="system:30-rustup"
DRY_RUN="${DRY_RUN:-0}"

log() {
    local level="$1"; shift
    printf '[%s] %s\n' "${MODULE}" "$level: $*"
}

if [[ $(id -u) -eq 0 ]]; then
    log ERROR "Do not run ${MODULE} as root. Re-run the stage as the target user."
    exit 1
fi

CARGO_HOME="${CARGO_HOME:-${HOME}/.cargo}"
RUSTUP_BIN="${CARGO_HOME}/bin/rustup"
CARGO_BIN="${CARGO_HOME}/bin/cargo"

ensure_profile_path() {
    local profile_file="$1"
    local line='export PATH="$HOME/.cargo/bin:$PATH"'
    if [[ -f "${profile_file}" ]] && grep -Fqx "${line}" "${profile_file}"; then
        log INFO "${profile_file} already exports cargo bin path"
        return
    fi
    if [[ "${DRY_RUN}" == "1" ]]; then
        log INFO "DRY_RUN: would append cargo PATH export to ${profile_file}"
        return
    fi
    mkdir -p "$(dirname "${profile_file}")"
    touch "${profile_file}"
    printf '\n%s\n' "${line}" >> "${profile_file}"
    log INFO "Added cargo PATH export to ${profile_file}"
}

if [[ "${DRY_RUN}" == "1" ]]; then
    if [[ -x "${RUSTUP_BIN}" ]]; then
        log INFO "DRY_RUN: would update existing rustup toolchain"
    else
        log INFO "DRY_RUN: would install rustup for user $(id -un)"
    fi
else
    if [[ -x "${RUSTUP_BIN}" ]]; then
        log INFO "Updating existing rustup installation"
        "${RUSTUP_BIN}" self update
        "${RUSTUP_BIN}" update stable
    else
        log INFO "Installing rustup for user $(id -un)"
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile default --no-modify-path
    fi
    log INFO "Ensuring stable toolchain is default"
    "${RUSTUP_BIN}" default stable
fi

ensure_profile_path "${HOME}/.bash_profile"
ensure_profile_path "${HOME}/.zprofile"
ensure_profile_path "${HOME}/.profile"

if [[ -x "${CARGO_BIN}" ]]; then
    log INFO "cargo binary located at ${CARGO_BIN}"
else
    log WARN "cargo was not found at ${CARGO_BIN}. Verify rustup installation."
fi

log INFO "Rust toolchain ready for $(id -un)"
