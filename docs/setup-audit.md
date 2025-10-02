# Setup Process Audit

The deployment pipeline was reviewed end-to-end to ensure a Raspberry Pi can provision the photo frame and Wi-Fi recovery flow without manual tweaks. The changes in this branch address the discrepancies found during the audit.

## Key findings

1. **NetworkManager permissions were implicit.** `wifi-manager` runs as the unprivileged service user, but nothing granted it permission to modify system connections. The new `setup/system/configure-networkmanager.sh` script installs a dedicated polkit rule so NetworkManager accepts those requests without extra group membership.
2. **Legacy systemd units lingered in the install tree.** Staging still shipped `photo-app.service`/`photo-app.target`, which referenced outdated binaries and paths. They are now removed so the compositor boots through the maintained `cage@tty1.service` template.
3. **Status tooling referenced obsolete unit names.** `print-status.sh` assumed the legacy `photo-app` and `sync-photos` units. The script now reports on the canonical `cage@tty1`/`photoframe-sync` services directly so drift is immediately obvious during development.
4. **Optional sync automation was never activated.** The systemd installer looked for `photo-sync.*` units even though the repository still packages `sync-photos.*`. We standardized on the `photoframe-sync.*` naming and removed the auto-detection shim so the installer only enables the new units.

## Operator impact

- Running the new system scripts (`install-packages.sh`, `create-users-and-perms.sh`, `configure-networkmanager.sh`) may prompt for a sudo password. Reconnect SSH after `create-users-and-perms.sh` so the `kiosk` group assignments propagate.
- `print-status.sh` reports on the new service names and clearly calls out when optional sync units are not installed.
- Old deployments that still use the retired `photo-app.*` or `sync-photos.*` units need to migrate to the new names; the tooling now surfaces missing expected units instead of hiding the mismatch.

## Recommended verification

1. Re-run the system and app setup stages on a Pi that previously required manual Wi-Fi provisioning. Confirm the Wi-Fi recovery hotspot comes up and accepts new credentials without invoking `sudo`.
2. Execute `/opt/photo-frame/bin/print-status.sh` and confirm the output lists `cage@tty1.service`, `photoframe-wifi-manager.service`, and, if configured, the sync timer with the correct names.
3. Use `journalctl -u photoframe-wifi-manager.service` to ensure NetworkManager operations succeed without authorization errors.
