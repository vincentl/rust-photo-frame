#!/usr/bin/env bash
set -euo pipefail

MODULE="app:10-build"
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
# Constrain parallelism on lower-memory devices to reduce OOM risk.
# Honor operator override via CARGO_BUILD_JOBS when set.
cargo_jobs_args=()
if [[ -n "${CARGO_BUILD_JOBS:-}" ]]; then
    cargo_jobs_args=(--jobs "${CARGO_BUILD_JOBS}")
else
    jobs=""
    if [[ -r /proc/meminfo ]]; then
        mem_kb=$(awk '/MemTotal:/ {print $2}' /proc/meminfo 2>/dev/null || echo 0)
        if (( mem_kb > 0 )); then
            if (( mem_kb <= 2097152 )); then       # <= 2 GiB
                jobs=1
            elif (( mem_kb <= 4194304 )); then     # <= 4 GiB
                jobs=2
            elif (( mem_kb <= 6291456 )); then     # <= 6 GiB
                jobs=3
            fi
        fi
    fi
    if [[ -n "${jobs}" ]]; then
        if command -v nproc >/dev/null 2>&1; then
            cpus=$(nproc)
            if (( jobs > cpus )); then jobs="${cpus}"; fi
        fi
        cargo_jobs_args=(--jobs "${jobs}")
        log INFO "Constraining cargo parallelism to ${jobs} jobs based on memory"
    fi
fi

cargo build --workspace --bins "${profile_flag[@]}" "${cargo_jobs_args[@]}"

if [[ -d "${REPO_ROOT}/target" ]]; then
    if find "${REPO_ROOT}/target" -maxdepth 2 -user root -print -quit | grep -q .; then
        log ERROR "Detected root-owned files under ${REPO_ROOT}/target; clean them before continuing."
        exit 1
    fi
fi

log INFO "Cargo build step complete"
