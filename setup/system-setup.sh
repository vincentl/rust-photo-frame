#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MODULE_DIR="${SCRIPT_DIR}/setup-modules"

if [[ $EUID -ne 0 ]]; then
    echo "[ERROR] This script must be run with sudo or as root." >&2
    exit 1
fi

if [[ ! -d "${MODULE_DIR}" ]]; then
    echo "[ERROR] Module directory not found: ${MODULE_DIR}" >&2
    exit 1
fi

shopt -s nullglob
MODULES=("${MODULE_DIR}"/[0-9][0-9]-*.sh)
shopt -u nullglob

if [[ ${#MODULES[@]} -eq 0 ]]; then
    echo "[WARN] No setup modules found in ${MODULE_DIR}. Nothing to do."
    exit 0
fi

prompt_yes_no() {
    local prompt="$1"
    local default_choice="$2"
    local suffix="[y/N]"

    if [[ "${default_choice}" == "Y" ]]; then
        suffix="[Y/n]"
    fi

    while true; do
        local response
        if read -r -p "${prompt} ${suffix} " response </dev/tty; then
            if [[ -z "${response}" ]]; then
                if [[ "${default_choice}" == "Y" ]]; then
                    return 0
                else
                    return 1
                fi
            fi

            case "${response}" in
                [Yy]*)
                    return 0
                    ;;
                [Nn]*)
                    return 1
                    ;;
            esac
        fi
        echo "Please answer yes or no (y/n)."
    done
}

RUN_MODULES=()

for module in "${MODULES[@]}"; do
    case "$(basename "${module}")" in
        10-power-button-override.optional.sh)
            if prompt_yes_no "[OPTIONAL] Enable the power button override module?" "N"; then
                RUN_MODULES+=("${module}")
            else
                echo "[INFO] Skipping optional module $(basename "${module}")."
            fi
            ;;
        *)
            RUN_MODULES+=("${module}")
            ;;
    esac
done

if [[ ${#RUN_MODULES[@]} -eq 0 ]]; then
    echo "[WARN] No setup modules selected. Nothing to do."
    exit 0
fi

echo "[INFO] Executing setup modules from ${MODULE_DIR}"
for module in "${RUN_MODULES[@]}"; do
    echo "[INFO] Running $(basename "${module}")"
    if [[ ! -x "${module}" ]]; then
        echo "[INFO] Making module executable"
        chmod +x "${module}"
    fi
    "${module}"
    echo "[INFO] Completed $(basename "${module}")"
    echo

done

echo "[INFO] All setup modules completed successfully."
