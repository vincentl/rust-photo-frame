#!/usr/bin/env bash
set -euo pipefail

MODULE="system:35-firmware"

log() {
    printf '[%s] %s\n' "${MODULE}" "$*"
}

# Pi 5 bootloader EEPROMs from v2025.01.22 through v2025.10.x shipped a memory
# configuration change (fake NUMA + SDRAM_BANKLOW) that cuts GPU memory
# bandwidth by roughly a third, directly lowering the 4K render ceiling.
# Fixed bootloaders (>= 2025.11.05) apply iommu_dma_numa_policy=interleave
# automatically. The bootloader lives on the board, not the SD card, so a
# fresh install must stage the update explicitly; it applies on next reboot.
if ! command -v rpi-eeprom-update >/dev/null 2>&1; then
    log "rpi-eeprom-update not available; skipping bootloader check (non-Pi host?)"
    exit 0
fi

status_output="$(rpi-eeprom-update 2>&1 || true)"
printf '%s\n' "${status_output}"

if grep -q "UPDATE AVAILABLE" <<<"${status_output}"; then
    log "Staging bootloader EEPROM update"
    rpi-eeprom-update -a
    log "NOTE: a reboot is required for the bootloader update to take effect"
else
    log "Bootloader EEPROM is up to date"
fi
