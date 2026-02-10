# Operations SOP

This runbook covers day-2 operations for deployed frames.

- Fresh install and first-time Wi-Fi recovery test: [`software.md`](software.md)
- Full release validation matrix: [`../developer/test-plan.md`](../developer/test-plan.md)
- Advanced Sway/kiosk debugging workflows: [`../developer/kiosk-debug.md`](../developer/kiosk-debug.md)
- Deep Wi-Fi service operations and troubleshooting: [`wifi-manager.md`](wifi-manager.md#service-management)

## Daily health snapshot

Run these first before deeper debugging:

```bash
./setup/tools/verify.sh
/opt/photo-frame/bin/print-status.sh
sudo systemctl status greetd.service
sudo systemctl status photoframe-wifi-manager.service
```

## Viewing runtime logs

The kiosk session launches the photo frame through Sway and pipes stdout/stderr into journald with `systemd-cat`.

```bash
sudo journalctl -t photo-frame -f
```

For Wi-Fi watcher logs:

```bash
sudo journalctl -u photoframe-wifi-manager.service -f
```

## Starting, stopping, and restarting the viewer

- Stop: `sudo systemctl stop greetd.service`
- Start: `sudo systemctl start greetd.service`
- Restart (preferred): `sudo systemctl stop greetd.service && sleep 1 && sudo systemctl start greetd.service`

`systemctl restart greetd` can race with logind seat handoff on tty1. The stop/sleep/start sequence is more reliable.

## Wi-Fi recovery validation

Use one of these canonical procedures:

- Fresh image acceptance flow: [`software.md#fresh-install-wi-fi-recovery-test`](software.md#fresh-install-wi-fi-recovery-test)
- Full QA matrix (including WAN-down and AP outage scenarios): [`../developer/test-plan.md#phase-7--wi-fi-provisioning--watcher`](../developer/test-plan.md#phase-7--wi-fi-provisioning--watcher)

## Wi-Fi failure triage

When recovery is stuck, gather these artifacts before changing config:

1. Snapshot operational state:
   - `/opt/photo-frame/bin/print-status.sh`
2. Inspect watcher state files:
   - `sudo cat /var/lib/photo-frame/wifi-state.json`
   - `sudo cat /var/lib/photo-frame/wifi-last.json`
   - `sudo ls -l /var/lib/photo-frame/wifi-request.json` (if present)
3. Inspect NetworkManager:
   - `nmcli dev status`
   - `nmcli connection show --active`
4. Verify kiosk session/Sway reachability:
   - `systemctl status greetd.service`
   - `sudo sh -lc 'uid=$(id -u kiosk); ls "/run/user/$uid"/sway-ipc.*.sock'`
5. Validate credential apply path manually:
   - `sudo -u kiosk /opt/photo-frame/bin/wifi-manager nm add --ssid "<ssid>" --psk "<password>"`

If triage still fails, collect bundle + journals and attach to issue triage:

```bash
tests/collect_logs.sh
sudo journalctl -u photoframe-wifi-manager.service --since "15 min ago"
```
