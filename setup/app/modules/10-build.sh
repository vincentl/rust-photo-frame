#!/usr/bin/env bash
set -euo pipefail

MODULE="app:10-build"
DRY_RUN="${DRY_RUN:-0}"
CARGO_PROFILE="${CARGO_PROFILE:-release}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "${SCRIPT_DIR}/../../.." && pwd)}"

log() {
    local level="$1"; shift
    printf '[%s] %s\n' "${MODULE}" "$level: $*"
}

if [[ $(id -u) -eq 0 ]]; then
    log ERROR "Refusing to run cargo as root. Execute the app stage as the deployment user."
    exit 1
fi

if ! command -v cargo >/dev/null 2>&1; then
    log ERROR "cargo command not found in PATH. Run system setup stage first."
    exit 1
fi

profile_flag=(--profile "${CARGO_PROFILE}")
if [[ "${CARGO_PROFILE}" == "release" ]]; then
    profile_flag=(--release)
fi

log INFO "Building workspace binaries with cargo ${profile_flag[*]}"
cd "${REPO_ROOT}"
if [[ "${DRY_RUN}" == "1" ]]; then
    log INFO "DRY_RUN: would run cargo build --workspace --bins ${profile_flag[*]}"
else
    cargo build --workspace --bins "${profile_flag[@]}"
fi

if [[ "${DRY_RUN}" != "1" && -d "${REPO_ROOT}/target" ]]; then
    if find "${REPO_ROOT}/target" -maxdepth 2 -user root -print -quit | grep -q .; then
        log ERROR "Detected root-owned files under ${REPO_ROOT}/target; clean them before continuing."
        exit 1
    fi
fi

log INFO "Cargo build step complete"
