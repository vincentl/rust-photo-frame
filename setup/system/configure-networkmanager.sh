#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'

if [[ $(id -u) -ne 0 ]]; then
    echo "configure-networkmanager.sh must be run as root" >&2
    exit 1
fi

SERVICE_USER="${SERVICE_USER:-kiosk}"
POLKIT_RULE="/etc/polkit-1/rules.d/45-photoframe-network.rules"

if ! id -u "${SERVICE_USER}" >/dev/null 2>&1; then
    echo "Service user ${SERVICE_USER} does not exist" >&2
    exit 1
fi

read -r -d '' RULE_CONTENT <<EOF_RULE || true
polkit.addRule(function(action, subject) {
    if (action.id.indexOf("org.freedesktop.NetworkManager.") === 0 && subject.user == "${SERVICE_USER}") {
        return polkit.Result.YES;
    }
});
EOF_RULE

tmpfile="$(mktemp)"
trap 'rm -f "${tmpfile}"' EXIT
printf '%s\n' "${RULE_CONTENT}" > "${tmpfile}"
install -D -m 0644 "${tmpfile}" "${POLKIT_RULE}"
trap - EXIT
rm -f "${tmpfile}"
echo "NetworkManager polkit rule installed for ${SERVICE_USER}"
