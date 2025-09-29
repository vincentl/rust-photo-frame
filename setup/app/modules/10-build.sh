#!/usr/bin/env bash
set -euo pipefail

MODULE="app:10-build"
DRY_RUN="${DRY_RUN:-0}"
CARGO_PROFILE="${CARGO_PROFILE:-release}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "${SCRIPT_DIR}/../../.." && pwd)}"
export CARGO_TARGET_DIR="${REPO_ROOT}/target"

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

cd "${REPO_ROOT}"

build_manifest() {
    local manifest="$1"
    local name="$2"
    if [[ "${DRY_RUN}" == "1" ]]; then
        log INFO "DRY_RUN: would build ${name} (cargo build ${profile_flag[*]} --manifest-path ${manifest})"
    else
        log INFO "Building ${name} with cargo ${profile_flag[*]}"
        cargo build "${profile_flag[@]}" --manifest-path "${manifest}"
    fi
}

manifests=(
    "${REPO_ROOT}/Cargo.toml"
    "${REPO_ROOT}/crates/wifi-watcher/Cargo.toml"
    "${REPO_ROOT}/crates/wifi-setter/Cargo.toml"
)
names=(
    "rust-photo-frame"
    "wifi-watcher"
    "wifi-setter"
)

for idx in "${!manifests[@]}"; do
    build_manifest "${manifests[idx]}" "${names[idx]}"
done

if [[ "${DRY_RUN}" != "1" && -d "${REPO_ROOT}/target" ]]; then
    if find "${REPO_ROOT}/target" -maxdepth 2 -user root -print -quit | grep -q .; then
        log ERROR "Detected root-owned files under ${REPO_ROOT}/target; clean them before continuing."
        exit 1
    fi
fi

log INFO "Cargo build step complete"
