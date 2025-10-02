#!/usr/bin/env bash
set -euo pipefail

MODULE="system:50-networkmanager"
DRY_RUN="${DRY_RUN:-0}"
INSTALL_ROOT="${INSTALL_ROOT:-/opt/photo-frame}"
SERVICE_USER="${SERVICE_USER:-$(id -un)}"
NETDEV_GROUP="netdev"
POLKIT_RULE="/etc/polkit-1/rules.d/90-photo-frame-network.rules"

log() {
    local level="$1"; shift
    printf '[%s] %s\n' "${MODULE}" "$level: $*"
}

run_sudo() {
    if [[ "${DRY_RUN}" == "1" ]]; then
        log INFO "DRY_RUN: sudo $*"
    else
        sudo "$@"
    fi
}

user_in_group() {
    local user="$1" group="$2"
    id -nG "${user}" 2>/dev/null | tr ' ' '\n' | grep -Fxq "${group}"
}

if ! id -u "${SERVICE_USER}" >/dev/null 2>&1; then
    log ERROR "Service user ${SERVICE_USER} does not exist."
    exit 1
fi

if ! getent group "${NETDEV_GROUP}" >/dev/null 2>&1; then
    log ERROR "Required group ${NETDEV_GROUP} is missing. Install NetworkManager before rerunning."
    exit 1
fi

if user_in_group "${SERVICE_USER}" "${NETDEV_GROUP}"; then
    log INFO "${SERVICE_USER} already in ${NETDEV_GROUP} group"
else
    if [[ "${DRY_RUN}" == "1" ]]; then
        log INFO "DRY_RUN: would add ${SERVICE_USER} to ${NETDEV_GROUP} group"
    else
        run_sudo usermod -a -G "${NETDEV_GROUP}" "${SERVICE_USER}"
        log INFO "Added ${SERVICE_USER} to ${NETDEV_GROUP}; re-login required for new group membership"
    fi
fi

write_config_if_changed() {
    local path="$1" mode="$2" owner="$3" group="$4" content="$5"

    if [[ "${DRY_RUN}" == "1" ]]; then
        log INFO "DRY_RUN: would install ${path} (${mode} ${owner}:${group}) with contents:"
        printf '%s\n' "${content}"
        return 2
    fi

    local tmp
    tmp="$(mktemp)"
    printf '%s\n' "${content}" > "${tmp}"

    if run_sudo test -f "${path}"; then
        if run_sudo cmp -s "${tmp}" "${path}"; then
            rm -f "${tmp}"
            return 1
        fi
    fi

    run_sudo install -m "${mode}" -o "${owner}" -g "${group}" "${tmp}" "${path}"
    rm -f "${tmp}"
    return 0
}

rule_content=$(cat <<EOF_INNER
polkit.addRule(function(action, subject) {
    if (action.id.indexOf("org.freedesktop.NetworkManager.") === 0 &&
        (subject.user == "${SERVICE_USER}" || subject.isInGroup("${NETDEV_GROUP}"))) {
        return polkit.Result.YES;
    }
});
EOF_INNER
)

log INFO "Ensuring NetworkManager polkit rule at ${POLKIT_RULE}"
rule_status=$(write_config_if_changed "${POLKIT_RULE}" 644 root root "${rule_content}") || true
case "${rule_status}" in
    0)
        log INFO "Polkit rule updated"
        ;;
    1)
        log INFO "Polkit rule already up to date"
        ;;
    2)
        log INFO "DRY_RUN: skipped writing ${POLKIT_RULE}"
        ;;
    *)
        log WARN "Unexpected status ${rule_status} while writing ${POLKIT_RULE}"
        ;;
esac

log INFO "NetworkManager permissions configured"
