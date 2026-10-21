#!/usr/bin/env bash
# The system modules that depend on the display stack, run with apt-get
# faked: the default (kms) installs no compositor packages and skips the
# greetd/sway session files; a wayland choice installs them.
#
#   bash setup/tests/stack-modules.test.sh
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MODULES="${REPO_ROOT}/setup/system/modules"
WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT

mkdir -p "${WORK}/bin"
cat >"${WORK}/bin/apt-get" <<'STUB'
#!/usr/bin/env bash
echo "apt-get $*" >>"${APT_LOG}"
STUB
chmod +x "${WORK}/bin/apt-get"
export APT_LOG="${WORK}/apt.log"
export PATH="${WORK}/bin:${PATH}"
export DISPLAY_ENV_PATH="${WORK}/display.env"
export POLICY_RC_PATH="${WORK}/policy-rc.d"

failures=0
fail() {
    echo "FAIL: $*" >&2
    failures=$((failures + 1))
}

installed() { grep -F -- 'install' "${APT_LOG}" | tr ' ' '\n'; }

# --- default: kms ---------------------------------------------------------
unset PHOTOFRAME_DISPLAY
rm -f "${DISPLAY_ENV_PATH}" "${APT_LOG}"
bash "${MODULES}/10-apt-packages.sh" >/dev/null || fail "apt module failed on the default stack"
for pkg in sway swaybg swayidle swaylock seatd greetd; do
    installed | grep -qx -- "${pkg}" && fail "default stack installed ${pkg}"
done
for pkg in mesa-vulkan-drivers libdrm2 libgl1-mesa-dri; do
    installed | grep -qx -- "${pkg}" || fail "default stack did not install ${pkg}"
done

# the greetd/sway session files are not provisioned (the module would fail
# writing /etc/greetd as a normal user if it tried)
out="$(bash "${MODULES}/50-greetd.sh" 2>&1)" || fail "greetd module failed on the default stack: ${out}"
[[ "${out}" == *"skipping the greetd/sway session files"* ]] || fail "greetd module did not skip on kms: ${out}"

# --- wayland, from the environment ------------------------------------------
rm -f "${APT_LOG}"
PHOTOFRAME_DISPLAY=wayland bash "${MODULES}/10-apt-packages.sh" >/dev/null || fail "apt module failed on wayland"
for pkg in sway swaybg swayidle swaylock seatd greetd wayland-protocols mesa-vulkan-drivers; do
    installed | grep -qx -- "${pkg}" || fail "wayland stack did not install ${pkg}"
done

# greetd must not be started by its postinst while the module runs, and the
# policy file must be gone afterwards (the apt stub sees it mid-install)
cat >"${WORK}/bin/apt-get" <<'STUB'
#!/usr/bin/env bash
echo "apt-get $*" >>"${APT_LOG}"
if [[ -x "${POLICY_RC_PATH}" ]]; then
    "${POLICY_RC_PATH}" greetd start && echo "greetd-allowed" >>"${APT_LOG}"
    "${POLICY_RC_PATH}" cron start || echo "other-denied" >>"${APT_LOG}"
else
    echo "no-policy" >>"${APT_LOG}"
fi
STUB
rm -f "${APT_LOG}"
PHOTOFRAME_DISPLAY=wayland bash "${MODULES}/10-apt-packages.sh" >/dev/null || fail "apt module failed with the policy file"
grep -qx "greetd-allowed" "${APT_LOG}" && fail "greetd was not held back during the wayland install"
grep -qx "other-denied" "${APT_LOG}" && fail "the policy file blocked services other than greetd"
[[ ! -e "${POLICY_RC_PATH}" ]] || fail "the policy file was left behind"
rm -f "${APT_LOG}"
bash "${MODULES}/10-apt-packages.sh" >/dev/null || fail "apt module failed on kms with the policy file"
grep -qx "no-policy" "${APT_LOG}" || fail "kms install wrote a policy file"

# --- wayland, persisted ------------------------------------------------------
printf 'PHOTOFRAME_DISPLAY=wayland\n' >"${DISPLAY_ENV_PATH}"
rm -f "${APT_LOG}"
bash "${MODULES}/10-apt-packages.sh" >/dev/null || fail "apt module failed on a persisted wayland choice"
installed | grep -qx -- sway || fail "a persisted wayland choice did not install sway"

# --- 45-activate-services (application stage) --------------------------------
# sudo, systemctl, sleep stubbed on PATH; the module must not start seatd on
# kms, must start greetd (not restart it) on wayland, and must stay on kms when
# an explicit wayland choice cannot be applied.
cat >"${WORK}/bin/sudo" <<'STUB'
#!/usr/bin/env bash
exec "$@"
STUB
cat >"${WORK}/bin/sleep" <<'STUB'
#!/usr/bin/env bash
exit 0
STUB
cat >"${WORK}/bin/systemctl" <<'STUB'
#!/usr/bin/env bash
echo "systemctl $*" >>"${SYSTEMCTL_LOG}"
case "$1" in
    list-unit-files) [[ "$2" != "greetd.service" || "${GREETD_MISSING:-0}" != 1 ]] ;;
    show) ;;
    *) ;;
esac
STUB
chmod +x "${WORK}/bin/sudo" "${WORK}/bin/sleep" "${WORK}/bin/systemctl"
export SYSTEMCTL_LOG="${WORK}/systemctl.log"
export INSTALL_ROOT="${WORK}/opt" SYNC_ENV_PATH="${WORK}/sync.env"
activate() { : >"${SYSTEMCTL_LOG}"; bash "${REPO_ROOT}/setup/application/modules/45-activate-services.sh" 2>&1; }
logged() { grep -qF -- "$1" "${SYSTEMCTL_LOG}"; }

unset PHOTOFRAME_DISPLAY GREETD_MISSING
rm -f "${DISPLAY_ENV_PATH}"
out="$(activate)" || fail "45-activate failed on the default stack: ${out}"
logged "seatd" && fail "45-activate touched seatd on kms"
logged "restart photoframe-kms.service" || fail "45-activate did not restart the kms unit"
[[ "${out}" == *"Display stack: kms (photoframe-kms.service, default)"* ]] || fail "45-activate did not name the stack: ${out}"

printf 'PHOTOFRAME_DISPLAY=wayland\n' >"${DISPLAY_ENV_PATH}"
out="$(activate)" || fail "45-activate failed on a persisted wayland choice: ${out}"
logged "start seatd" || fail "45-activate did not start seatd on wayland"
logged "start greetd.service" || fail "45-activate did not start greetd on wayland"
logged "restart greetd.service" && fail "45-activate restarted greetd (must stop/start)"

rm -f "${DISPLAY_ENV_PATH}"
out="$(GREETD_MISSING=1 PHOTOFRAME_DISPLAY=wayland activate)" || fail "45-activate failed on a refused wayland choice: ${out}"
[[ ! -e "${DISPLAY_ENV_PATH}" ]] || fail "a refused wayland choice was persisted"
logged "greetd.service" && { logged "start greetd.service" && fail "45-activate started a greetd that is not installed"; }
logged "restart photoframe-kms.service" || fail "a refused wayland choice left the kms stack unrestarted"
[[ "${out}" == *"Display stack: kms"* ]] || fail "45-activate named a stack that is not running: ${out}"
unset INSTALL_ROOT SYNC_ENV_PATH

# --- the launcher is stack-independent ---------------------------------------
[[ -x "${MODULES}/45-launcher.sh" ]] || fail "45-launcher.sh missing or not executable"
grep -q '/usr/local/bin/photoframe' "${MODULES}/45-launcher.sh" || fail "45-launcher.sh does not install the launcher"
grep -q 'install_photo_launcher' "${MODULES}/50-greetd.sh" && fail "50-greetd.sh still installs the launcher (wayland-only module)"

if ((failures)); then
    echo "stack-modules tests: ${failures} failure(s)" >&2
    exit 1
fi
echo "stack-modules tests: all passed"
