#!/usr/bin/env bash
# Setup smoke test, run INSIDE the Lima VM (see developer/lima.md):
#
#   limactl shell photoframe -- developer/lima/smoke-setup.sh
#
# Runs the real system installer and the real application deploy against the
# VM, then checks what they left behind: the kiosk user and groups, the
# /opt/photoframe tree, every systemd unit under assets/systemd and setup/
# (systemd-analyze verify), enable/active state, setup/tools/verify.sh and the
# journal. It changes the VM substantially (kiosk user, greetd, zram,
# journald, masked getty@tty1), so run it in a clone: `limactl clone`.
#
# Expected in a VM: greetd cannot bring up a compositor without the Pi's GPU
# and display, so greetd.service ends up inactive/failed, 50-postcheck.sh fails
# on it, and verify.sh reports it. Those are tolerated and listed separately.
# Anything else that fails is a real finding.
set -euo pipefail

# The expectations above and below are for the Wayland fallback stack (greetd
# is what fails in the VM). KMS is the default install since v1.2, so pin the
# stack unless the caller chose one; the system installer and the deploy both
# read the variable. PHOTOFRAME_DISPLAY=kms makes the other expectations apply
# (photoframe-kms.service, whose postcheck now warns, not fails, while the
# viewer waits for a display); they are not asserted here.
export PHOTOFRAME_DISPLAY="${PHOTOFRAME_DISPLAY:-wayland}"

# shellcheck source=./common.sh
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

LOG_DIR="${BUILD_ROOT}/smoke"
hard_failures=0
expected_failures=0

section() { printf '\n==== %s ====\n' "$*"; }
fail()    { printf '[smoke] FAIL: %s\n' "$*"; hard_failures=$((hard_failures + 1)); }
pass()    { printf '[smoke] ok:   %s\n' "$*"; }
expected() { printf '[smoke] EXPECTED IN VM: %s\n' "$*"; expected_failures=$((expected_failures + 1)); }

lima_require_vm
# The smoke test reconfigures the machine (greetd, zram, journald, masked getty);
# keep the base VM clean for tests and builds.
if [[ "$(hostname)" == "lima-photoframe" && "${SMOKE_ALLOW_MAIN_VM:-0}" != 1 ]]; then
    lima_die "this is the base VM; run the smoke test in a clone (limactl clone --start photoframe photoframe-smoke) or set SMOKE_ALLOW_MAIN_VM=1"
fi
sudo -n true 2>/dev/null || lima_die "passwordless sudo is required"
systemctl is-system-running --wait >/dev/null 2>&1 || true
[[ -d /run/systemd/system ]] || lima_die "systemd is not PID 1 in this guest"
mkdir -p "${LOG_DIR}"

section "1. Sync working tree into the VM"
lima_sync_src

section "2. System stage: sudo setup/system/install.sh"
# NetworkManager gets installed by the system stage. Keep it off the VM's own
# (cloud-init) network interface, or it can drop the ssh connection.
sudo install -d -m 0755 /etc/NetworkManager/conf.d
printf '[keyfile]\nunmanaged-devices=*\n' | sudo tee /etc/NetworkManager/conf.d/10-lima-unmanaged.conf >/dev/null
if (cd "${SRC_COPY}" && sudo --preserve-env=PHOTOFRAME_DISPLAY ./setup/system/install.sh) 2>&1 | tee "${LOG_DIR}/system-install.log"; then
    pass "setup/system/install.sh"
else
    fail "setup/system/install.sh (log: ${LOG_DIR}/system-install.log)"
fi

section "3. Application stage: setup/application/deploy.sh"
# deploy.sh stops at the first failing module. Everything before 50-postcheck
# must complete; postcheck is allowed to fail on greetd (see header).
deploy_rc=0
(cd "${SRC_COPY}" && ./setup/application/deploy.sh) 2>&1 | tee "${LOG_DIR}/deploy.log" || deploy_rc=${PIPESTATUS[0]}
for module in 05-service-user 10-build 20-stage 30-install 35-fonts 45-activate-services; do
    if grep -q "Completed ${module}.sh" "${LOG_DIR}/deploy.log"; then
        pass "app:${module}"
    else
        fail "app:${module} did not complete (log: ${LOG_DIR}/deploy.log)"
    fi
done
if grep -q "Completed 50-postcheck.sh" "${LOG_DIR}/deploy.log"; then
    pass "app:50-postcheck (greetd and the control socket came up)"
elif grep -q "greetd.service is not active\|Control socket not found" "${LOG_DIR}/deploy.log"; then
    expected "app:50-postcheck failed on greetd / control socket (no GPU, no display)"
else
    fail "app:50-postcheck failed for another reason (deploy.sh exit ${deploy_rc})"
fi

section "4. Kiosk user, groups, tree"
if id kiosk >/dev/null 2>&1; then
    pass "kiosk user: $(id kiosk)"
    for group in render video input seat; do
        if id -nG kiosk | tr ' ' '\n' | grep -Fxq "${group}"; then pass "kiosk in ${group}"; else fail "kiosk not in ${group}"; fi
    done
else
    fail "kiosk user missing"
fi
for path in /opt/photoframe/bin/photoframe /opt/photoframe/bin/wifi-manager /opt/photoframe/bin/buttond \
            /opt/photoframe/etc/photoframe/config.yaml /etc/photoframe/config.yaml; do
    if [[ -e "${path}" ]]; then pass "present: ${path}"; else fail "missing: ${path}"; fi
done
sudo ls -ld /var/lib/photoframe /var/lib/photoframe/photos /run/photoframe 2>&1 || true
if file /opt/photoframe/bin/photoframe | grep -q "ARM aarch64"; then pass "photoframe is an aarch64 ELF"; else fail "photoframe is not an aarch64 ELF"; fi

section "5. systemd-analyze verify (assets/systemd and setup/)"
mapfile -d '' units < <(cd "${SRC_COPY}" && find assets/systemd setup \( -name '*.service' -o -name '*.timer' \) -type f -print0 | sort -z)
[[ ${#units[@]} -gt 0 ]] || fail "no unit files found under assets/systemd or setup/"
for unit in "${units[@]}"; do
    if out="$(sudo systemd-analyze verify "${SRC_COPY}/${unit}" 2>&1)"; then
        pass "verify ${unit}"
    else
        fail "verify ${unit}"
    fi
    if [[ -n "${out}" ]]; then printf '%s\n' "${out}" | sed 's/^/        /'; fi
done

section "6. Unit state"
for unit in photoframe-wifi-manager.service buttond.service greetd.service seatd.service photoframe-sync.timer; do
    printf '%-36s enabled=%-10s active=%s\n' "${unit}" \
        "$(systemctl is-enabled "${unit}" 2>&1 || true)" "$(systemctl is-active "${unit}" 2>&1 || true)"
done
systemctl --failed --no-pager || true

section "7. setup/tools/verify.sh"
verify_rc=0
# As root: /var/lib/photoframe is 0750 kiosk:kiosk and sync.env is 0660, so as the
# VM user verify.sh cannot see them and reports phantom "library directory
# missing" warnings and awk "Permission denied".
sudo "${SRC_COPY}/setup/tools/verify.sh" 2>&1 | tee "${LOG_DIR}/verify.log" || verify_rc=${PIPESTATUS[0]}
# The ERROR lines verify.sh prints for greetd (and the status dump before them)
# are the VM's missing display. Anything else is real.
unexpected="$(grep 'ERROR' "${LOG_DIR}/verify.log" | grep -v 'greetd.service not active' || true)"
if [[ -z "${unexpected}" ]]; then
    if (( verify_rc != 0 )); then
        expected "verify.sh exit ${verify_rc}: only greetd.service not active"
    else
        pass "verify.sh clean"
    fi
else
    fail "verify.sh reported errors other than greetd:"
    printf '%s\n' "${unexpected}" | sed 's/^/        /'
fi

section "8. Journal (this boot)"
sudo journalctl -b --no-pager -n 40 \
    -u photoframe-wifi-manager.service -u buttond.service -u greetd.service -u seatd.service || true
echo "--- tag photoframe (the kiosk session wrapper) ---"
sudo journalctl -b --no-pager -n 40 -t photoframe || true

section "Summary"
printf '%d hard failure(s), %d expected-in-VM item(s). Logs: %s\n' "${hard_failures}" "${expected_failures}" "${LOG_DIR}"
(( hard_failures == 0 ))
