#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"

FRAME_USER_REQUESTED="${FRAME_USER:-}"

choose_frame_user() {
    local requested_user="$1"
    local repo_owner
    repo_owner="$(stat -c %U "${REPO_ROOT}")"

    local -a candidates=()
    local -A seen=()

    if [[ -n "${requested_user}" ]]; then
        candidates+=("${requested_user}")
    fi
    if [[ -n "${SUDO_USER:-}" ]]; then
        candidates+=("${SUDO_USER}")
    fi
    if [[ -n "${repo_owner}" ]]; then
        candidates+=("${repo_owner}")
    fi
    candidates+=("frame" "root")

    local candidate
    for candidate in "${candidates[@]}"; do
        if [[ -z "${candidate}" ]]; then
            continue
        fi
        if [[ -n "${seen[${candidate}]:-}" ]]; then
            continue
        fi
        seen["${candidate}"]=1
        if id -u "${candidate}" >/dev/null 2>&1; then
            echo "${candidate}"
            return 0
        fi
    done

    echo "root"
}

FRAME_USER="$(choose_frame_user "${FRAME_USER_REQUESTED}")"

if [[ -n "${FRAME_USER_REQUESTED}" && "${FRAME_USER}" != "${FRAME_USER_REQUESTED}" ]]; then
    echo "[30-wifi-watcher] Requested user '${FRAME_USER_REQUESTED}' was not found; using '${FRAME_USER}' instead."
fi

FRAME_HOME="$(getent passwd "${FRAME_USER}" | cut -d: -f6)"
if [[ -z "${FRAME_HOME}" ]]; then
    echo "[30-wifi-watcher] Unable to determine home directory for user '${FRAME_USER}'." >&2
    exit 1
fi

# shellcheck disable=SC1090
if [[ -f "${FRAME_HOME}/.cargo/env" ]]; then
    source "${FRAME_HOME}/.cargo/env"
elif [[ -d "${FRAME_HOME}/.cargo/bin" ]]; then
    export PATH="${FRAME_HOME}/.cargo/bin:${PATH}"
fi

if ! command -v cargo >/dev/null 2>&1; then
    echo "[30-wifi-watcher] cargo is required but was not found in PATH." >&2
    exit 1
fi

run_as_frame() {
    local cmd="$1"
    if [[ "${FRAME_USER}" == "root" ]]; then
        bash -lc "${cmd}"
    else
        sudo -u "${FRAME_USER}" -H bash -lc "${cmd}"
    fi
}

echo "[30-wifi-watcher] Building Wi-Fi watcher binaries with cargo (user: ${FRAME_USER})..."
run_as_frame "cd '${REPO_ROOT}' && cargo build --release"
echo "[30-wifi-watcher] Build completed. Artifacts available in target/release."
