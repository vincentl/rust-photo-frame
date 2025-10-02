#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'

if [[ $(id -u) -ne 0 ]]; then
    echo "install-sudoers.sh must be run as root" >&2
    exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SOURCE_FILE="${SCRIPT_DIR}/sudoers/photoframe"
TARGET_FILE=/etc/sudoers.d/photoframe

if [[ ! -f "${SOURCE_FILE}" ]]; then
    echo "Expected sudoers template missing at ${SOURCE_FILE}" >&2
    exit 1
fi

install -D -m 0440 "${SOURCE_FILE}" "${TARGET_FILE}"
visudo -cf "${TARGET_FILE}" >/dev/null
printf 'Installed sudoers rules at %s\n' "${TARGET_FILE}"
