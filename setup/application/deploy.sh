#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
MODULE_DIR="${SCRIPT_DIR}/modules"
SCRIPT_NAME="$(basename "$0")"

INSTALL_ROOT="${INSTALL_ROOT:-/opt/photoframe}"
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

# RaspberryPi OS ships /etc/systemd/journald.conf.d/40-rpi-volatile-storage.conf
# which forces Storage=volatile and discards the persistent logs the system
# setup configured (Storage=persistent in /etc/systemd/journald.conf). Mask the
# RaspberryPi drop-in by symlinking it to /dev/null so systemd-journald ignores
# it and our persistent journal survives reboots. Idempotent: re-running only
# re-asserts the symlink and skips the journald restart when already masked.
ensure_journald_persistence() {
    local dropin_dir="/etc/systemd/journald.conf.d"
    local override="${dropin_dir}/40-rpi-volatile-storage.conf"

    if [[ "$(readlink -f "${override}" 2>/dev/null)" == "/dev/null" ]]; then
        log INFO "journald volatile-storage override already masked at ${override}"
        return 0
    fi

    log INFO "Masking RaspberryPi journald volatile-storage override at ${override}"
    sudo mkdir -p "${dropin_dir}"
    sudo ln -sf /dev/null "${override}"

    if command -v systemctl >/dev/null 2>&1; then
        log INFO "Restarting systemd-journald to apply persistent storage"
        sudo systemctl restart systemd-journald >/dev/null 2>&1 || \
            log WARN "Failed to restart systemd-journald; persistent logging applies after next boot"
    fi
}

# --skip-build: install a tree that was already built and staged elsewhere
# (see developer/lima.md). Skips 10-build and 20-stage; every other module runs
# as usual against ${STAGE_ROOT}/stage. Without the flag nothing changes.
SKIP_BUILD=0
while [[ $# -gt 0 ]]; do
    case "$1" in
        --skip-build) SKIP_BUILD=1 ;;
        -h|--help)
            echo "Usage: ${SCRIPT_NAME} [--skip-build]"
            echo "  --skip-build  skip the build and stage modules; install the tree already at \${STAGE_ROOT}/stage"
            exit 0
            ;;
        *)
            log ERROR "Unknown argument: $1"
            exit 2
            ;;
    esac
    shift
done

trap 'log ERROR "${SCRIPT_NAME} failed on line ${LINENO}"' ERR

# The deploy cannot install the compositor packages. An explicit
# PHOTOFRAME_DISPLAY=wayland on a frame without greetd is dropped here, once,
# so no module (they inherit the environment) works from a stack that is not
# installed.
# shellcheck source=../lib/systemd.sh
source "${SCRIPT_DIR}/../lib/systemd.sh"
# shellcheck source=../lib/display-stack.sh
source "${SCRIPT_DIR}/../lib/display-stack.sh"
display_stack_refuse_unavailable

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

# Prefer the system-installed toolchain binaries if present, but keep
# per-user Cargo/Rustup homes so the unprivileged user can write registries,
# git checkouts, and build caches without requiring root.
if [[ -d "${SYSTEM_CARGO_BIN}" ]]; then
    case ":${PATH}:" in
        *:"${SYSTEM_CARGO_BIN}":*) ;;
        *) export PATH="${SYSTEM_CARGO_BIN}:${PATH}" ;;
    esac
fi

# Default to a per-user Cargo home so registries and git checkouts are
# writable without sudo, but keep rustup state pointing at the system
# toolchain installed by the system stage when present. This avoids the
# "no default toolchain configured" error when using the system proxy
# binaries from /usr/local/cargo/bin.
CARGO_HOME="${CARGO_HOME:-${HOME}/.cargo}"
if [[ -d "${SYSTEM_RUSTUP_HOME}" ]]; then
    RUSTUP_HOME="${RUSTUP_HOME:-${SYSTEM_RUSTUP_HOME}}"
else
    RUSTUP_HOME="${RUSTUP_HOME:-${HOME}/.rustup}"
fi

STAGE_ROOT_FROM_ENV="${STAGE_ROOT:-}"
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
    log INFO "No application modules found in ${MODULE_DIR}."
    exit 0
fi

if (( SKIP_BUILD )); then
    # Installing a tree nobody built from this checkout is the failure mode of
    # this flag, so refuse early (before anything is modified) and say what is
    # about to be installed.
    if [[ -z "${STAGE_ROOT_FROM_ENV}" ]]; then
        log ERROR "--skip-build needs STAGE_ROOT set explicitly (the default is the stage of the last on-device build, which is stale after a git pull)."
        exit 1
    fi
    stage_dir="${STAGE_ROOT}/stage"
    for required in photoframe wifi-manager; do
        if [[ ! -x "${stage_dir}/bin/${required}" ]]; then
            log ERROR "--skip-build: ${stage_dir}/bin/${required} is missing; the staged tree is incomplete."
            exit 1
        fi
    done
    if [[ ! -f "${stage_dir}/.stamp" ]]; then
        log ERROR "--skip-build: ${stage_dir}/.stamp is missing; the tree was not staged by developer/lima/stage-to-pi.sh or the transfer did not finish."
        exit 1
    fi
    log INFO "--skip-build will install the tree at ${stage_dir}, staged as:"
    sed 's/^/    /' "${stage_dir}/.stamp"
    for bin in "${stage_dir}"/bin/*; do
        [[ -x "${bin}" ]] && printf '    %s  %s\n' "$(stat -c '%y' "${bin}" | cut -d. -f1)" "$(basename "${bin}")"
    done
    stamp_rev="$(sed -n 's/^rev=//p' "${stage_dir}/.stamp")"
    if head_rev="$(git -C "${REPO_ROOT}" rev-parse HEAD 2>/dev/null)"; then
        if [[ "${stamp_rev}" != "${head_rev}" && "${ALLOW_STAGE_REV_MISMATCH:-0}" != 1 ]]; then
            log ERROR "--skip-build: staged tree is from ${stamp_rev:-unknown} but this checkout is at ${head_rev}. git pull / check out that commit, or set ALLOW_STAGE_REV_MISMATCH=1."
            exit 1
        fi
    else
        log WARN "--skip-build: ${REPO_ROOT} is not a git checkout; cannot compare it with the staged revision."
    fi
    # What the system stage leaves behind. Without it 05/30/45 would half-provision the device.
    # What it must leave depends on the display stack: seat (seatd) exists
    # only for wayland; the default KMS stack needs video/render/input.
    display_stack_name="$(display_stack_current)"
    missing_items="$(display_stack_system_stage_missing "${display_stack_name}" "${SERVICE_USER}" | paste -sd, -)"
    if [[ -n "${missing_items}" ]]; then
        log ERROR "--skip-build: the system stage has not run for the ${display_stack_name} display stack (missing: ${missing_items//,/, }). Run sudo ./setup/system/install.sh first."
        exit 1
    fi
fi

# Re-assert persistent journald logging before running modules so their output
# (and any failure diagnostics) survive a reboot even after RaspberryPi OS
# updates restore the volatile-storage drop-in.
ensure_journald_persistence

log INFO "Executing application deployment modules as user $(id -un)"
for module in "${modules[@]}"; do
    module_name="$(basename "${module}")"
    if (( SKIP_BUILD )) && [[ "${module_name}" == 10-build.sh || "${module_name}" == 20-stage.sh ]]; then
        log INFO "Skipping ${module_name} (--skip-build)"
        continue
    fi
    log INFO "Starting ${module_name}"
    if [[ ! -x "${module}" ]]; then
        chmod +x "${module}"
    fi
    "${module}"
    log INFO "Completed ${module_name}"
    echo
done

log INFO "Application deployment complete."
