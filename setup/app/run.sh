#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
MODULE_DIR="${SCRIPT_DIR}/modules"
SCRIPT_NAME="$(basename "$0")"

INSTALL_ROOT="${INSTALL_ROOT:-/opt/photo-frame}"
DEFAULT_SERVICE_USER="kiosk"
if id -u "${DEFAULT_SERVICE_USER}" >/dev/null 2>&1; then
    DEFAULT_SERVICE_GROUP="$(id -gn "${DEFAULT_SERVICE_USER}")"
else
    DEFAULT_SERVICE_GROUP="${DEFAULT_SERVICE_USER}"
fi
SERVICE_USER="${SERVICE_USER:-${DEFAULT_SERVICE_USER}}"
if id -u "${SERVICE_USER}" >/dev/null 2>&1; then
    SERVICE_GROUP="${SERVICE_GROUP:-$(id -gn "${SERVICE_USER}")}"
else
    SERVICE_GROUP="${SERVICE_GROUP:-${DEFAULT_SERVICE_GROUP}}"
fi
CARGO_PROFILE="${CARGO_PROFILE:-release}"
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

SYSTEM_CARGO_HOME="/usr/local/cargo"
SYSTEM_RUSTUP_HOME="/usr/local/rustup"
SYSTEM_CARGO_BIN="${SYSTEM_CARGO_HOME}/bin"

if [[ -d "${SYSTEM_CARGO_BIN}" ]]; then
    if [[ -z "${CARGO_HOME+x}" ]]; then
        CARGO_HOME="${SYSTEM_CARGO_HOME}"
    fi
    if [[ -z "${RUSTUP_HOME+x}" ]]; then
        RUSTUP_HOME="${SYSTEM_RUSTUP_HOME}"
    fi
    case ":${PATH}:" in
        *:"${SYSTEM_CARGO_BIN}":*) ;;
        *)
            export PATH="${SYSTEM_CARGO_BIN}:${PATH}"
            ;;
    esac
fi

CARGO_HOME="${CARGO_HOME:-${HOME}/.cargo}"
RUSTUP_HOME="${RUSTUP_HOME:-${HOME}/.rustup}"

STAGE_ROOT="${STAGE_ROOT:-${SCRIPT_DIR}/build}"
export STAGE_ROOT

# The app stage expects the system-level Rust toolchain installed by
# setup/packages/install-rust.sh, but developers may override the toolchain
# location by setting CARGO_HOME and RUSTUP_HOME before invoking this script.
export INSTALL_ROOT SERVICE_USER SERVICE_GROUP CARGO_PROFILE REPO_ROOT
export CARGO_HOME RUSTUP_HOME
export PATH

shopt -s nullglob
modules=("${MODULE_DIR}"/[0-9][0-9]-*.sh)
shopt -u nullglob

if [[ ${#modules[@]} -eq 0 ]]; then
    log INFO "No app setup modules found in ${MODULE_DIR}."
    exit 0
fi

log INFO "Executing app setup modules as user $(id -un)"
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

log INFO "App setup stage complete."
