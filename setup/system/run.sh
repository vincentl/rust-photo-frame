#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
MODULE_DIR="${SCRIPT_DIR}/modules"
SCRIPT_NAME="$(basename "$0")"

INSTALL_ROOT="${INSTALL_ROOT:-/opt/photo-frame}"
SERVICE_USER="${SERVICE_USER:-photo-frame}"
CARGO_PROFILE="${CARGO_PROFILE:-release}"
DRY_RUN="${DRY_RUN:-0}"

log() {
    local level="$1"; shift
    printf '[%s] %s\n' "${SCRIPT_NAME}" "$level: $*"
}

trap 'log ERROR "${SCRIPT_NAME} failed on line ${LINENO}"' ERR

if [[ ! -d "${MODULE_DIR}" ]]; then
    log ERROR "Module directory not found: ${MODULE_DIR}"
    exit 1
fi

if [[ $(id -u) -eq 0 ]]; then
    log ERROR "Run ${SCRIPT_NAME} as an unprivileged user; modules will request sudo when necessary."
    exit 1
fi

CARGO_HOME="${CARGO_HOME:-${HOME}/.cargo}"
if [[ -d "${CARGO_HOME}/bin" ]]; then
    case ":${PATH}:" in
        *:"${CARGO_HOME}/bin":*) ;;
        *)
            export PATH="${CARGO_HOME}/bin:${PATH}"
            ;;
    esac
fi

export INSTALL_ROOT SERVICE_USER CARGO_PROFILE DRY_RUN REPO_ROOT
export CARGO_HOME
export PATH

shopt -s nullglob
modules=("${MODULE_DIR}"/[0-9][0-9]-*.sh)
shopt -u nullglob

if [[ ${#modules[@]} -eq 0 ]]; then
    log INFO "No system setup modules found in ${MODULE_DIR}."
    exit 0
fi

log INFO "Executing system setup modules as user $(id -un)"
for module in "${modules[@]}"; do
    module_name="$(basename "${module}")"
    log INFO "Starting ${module_name}"
    if [[ ! -x "${module}" ]]; then
        chmod +x "${module}"
    fi
    "${module}"
    log INFO "Completed ${module_name}"
    echo
done

log INFO "System setup stage complete."
