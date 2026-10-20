#!/usr/bin/env bash
# Build the release binaries in the Lima VM, stage them exactly as
# setup/application/modules/20-stage.sh does, and rsync the staged tree to a
# Pi. Run INSIDE the VM (see developer/lima.md):
#
#   limactl shell photoframe -- developer/lima/stage-to-pi.sh [--deploy] USER@HOST
#   limactl shell photoframe -- developer/lima/stage-to-pi.sh --stage-only
#
# --stage-only builds and stages, runs the checks below and stops: no Pi, no
# ssh agent, no rsync (USER@HOST is not needed). The result is in
# ${STAGE_ROOT}/stage inside the VM.
#
# Without --deploy it stops after the rsync and prints the command to finish.
# With --deploy it also runs, on the Pi:
#
#   cd $PI_REPO && STAGE_ROOT=$HOME/$PI_STAGE ./setup/application/deploy.sh --skip-build
#
# i.e. the Pi's own 05/30/35/45/50 modules install the tree into /opt/photoframe
# and restart services; only the build and stage modules are skipped.
#
# Environment:
#   PI_REPO   Pi checkout, relative to its home   (default: photoframe)
#   PI_STAGE  Pi staging dir, relative to its home (default: photoframe-stage)
#
# The Pi needs the same commit checked out as this working tree: the staged
# tree (config.yaml, docs, systemd units) comes from here, the install
# scripts that consume it run from the Pi's checkout.
set -euo pipefail

# shellcheck source=./common.sh
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

PI_REPO="${PI_REPO:-photoframe}"
PI_STAGE="${PI_STAGE:-photoframe-stage}"
deploy=0
stage_only=0
target=""
for arg in "$@"; do
    case "${arg}" in
        --deploy) deploy=1 ;;
        --stage-only) stage_only=1 ;;
        -h|--help) sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed '$d; s/^# \{0,1\}//'; exit 0 ;;
        -*) lima_die "unknown option ${arg}" ;;
        *) [[ -z "${target}" ]] || lima_die "only one USER@HOST"; target="${arg}" ;;
    esac
done
(( stage_only )) && (( deploy )) && lima_die "--stage-only and --deploy are mutually exclusive"
(( stage_only )) || [[ -n "${target}" ]] || lima_die "usage: stage-to-pi.sh [--deploy] USER@HOST | --stage-only"

lima_require_vm

# Binaries built here must run on a Cortex-A76. The template pins the flag;
# refuse anything that would reintroduce target-cpu=native on the VM's CPU.
export RUSTFLAGS="${RUSTFLAGS:--Ctarget-cpu=cortex-a76}"
case "${RUSTFLAGS}" in
    *native*) lima_die "RUSTFLAGS=${RUSTFLAGS} would target the VM's CPU, not the Pi's (SIGILL risk)" ;;
esac
export CARGO_PROFILE=release
export STAGE_ROOT="${STAGE_ROOT:-${BUILD_ROOT}/stage-root}"

(( stage_only )) || ssh-add -l >/dev/null 2>&1 || lima_die "no ssh-agent in the VM; on the Mac run 'ssh-add' and restart the shell ('limactl shell' forwards the agent)"

lima_sync_src

lima_log "Building (10-build.sh, RUSTFLAGS=${RUSTFLAGS})"
bash "${SRC_COPY}/setup/application/modules/10-build.sh"
lima_log "Staging (20-stage.sh) -> ${STAGE_ROOT}/stage"
bash "${SRC_COPY}/setup/application/modules/20-stage.sh"

stage="${STAGE_ROOT}/stage"
# frametest is staged only when it was built; check it like the others if so.
checked_bins=(photoframe wifi-manager buttond)
[[ -e "${stage}/bin/frametest" ]] && checked_bins+=(frametest)
for bin in "${checked_bins[@]}"; do
    [[ -x "${stage}/bin/${bin}" ]] || lima_die "${stage}/bin/${bin} missing after staging"
    file "${stage}/bin/${bin}" | grep -q "ARM aarch64" || lima_die "${bin} is not an aarch64 ELF"
    if ldd "${stage}/bin/${bin}" | grep -q "not found"; then
        ldd "${stage}/bin/${bin}" | grep "not found" >&2
        lima_die "${bin} has unresolved shared libraries"
    fi
done

if (( stage_only )); then
    lima_log "Staged at ${stage} (--stage-only: nothing sent anywhere)"
    exit 0
fi

lima_log "rsync ${stage}/ -> ${target}:${PI_STAGE}/stage/"
rsync -rlptD --delete --mkpath --no-owner --no-group \
    "${stage}/" "${target}:${PI_STAGE}/stage/"

deploy_cmd="cd ${PI_REPO} && STAGE_ROOT=\"\$HOME/${PI_STAGE}\" ./setup/application/deploy.sh --skip-build"
if (( deploy )); then
    lima_log "Running deploy on ${target}"
    ssh -t "${target}" "${deploy_cmd}"
else
    lima_log "Staged. To install on the Pi:"
    printf '    ssh -t %s %q\n' "${target}" "${deploy_cmd}"
fi
