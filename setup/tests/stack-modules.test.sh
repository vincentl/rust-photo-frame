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

# --- wayland, persisted ------------------------------------------------------
printf 'PHOTOFRAME_DISPLAY=wayland\n' >"${DISPLAY_ENV_PATH}"
rm -f "${APT_LOG}"
bash "${MODULES}/10-apt-packages.sh" >/dev/null || fail "apt module failed on a persisted wayland choice"
installed | grep -qx -- sway || fail "a persisted wayland choice did not install sway"

# --- the launcher is stack-independent ---------------------------------------
[[ -x "${MODULES}/45-launcher.sh" ]] || fail "45-launcher.sh missing or not executable"
grep -q '/usr/local/bin/photoframe' "${MODULES}/45-launcher.sh" || fail "45-launcher.sh does not install the launcher"
grep -q 'install_photo_launcher' "${MODULES}/50-greetd.sh" && fail "50-greetd.sh still installs the launcher (wayland-only module)"

if ((failures)); then
    echo "stack-modules tests: ${failures} failure(s)" >&2
    exit 1
fi
echo "stack-modules tests: all passed"
