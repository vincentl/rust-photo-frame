# Operations SOP

This runbook covers day-2 operations for deployed frames.

Command context: run commands as your operator account over SSH and use `sudo` where shown.

- Fresh install and first-time Wi-Fi recovery test: [`software.md`](software.md)
- Full validation matrix: [`../developer/test-plan.md`](../developer/test-plan.md)
- Advanced Sway/kiosk debugging workflows: [`../developer/kiosk-debug.md`](../developer/kiosk-debug.md)
- Deep Wi-Fi service operations and troubleshooting: [`wifi-manager.md`](wifi-manager.md#service-management)

## Daily health snapshot

Run these first before deeper debugging:

```bash
./setup/tools/verify.sh
/opt/photoframe/bin/print-status.sh
sudo systemctl status greetd.service
sudo systemctl status photoframe-wifi-manager.service
```

Expected outcome: both services report `active (running)` and `print-status.sh` completes without errors.

## Viewing runtime logs

The kiosk session launches the photo frame through Sway and pipes stdout/stderr into journald with `systemd-cat`.

```bash
sudo journalctl -t photoframe -f
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
- Full QA matrix (including WAN-down and AP outage scenarios): [`../developer/test-plan.md#phase-7-wi-fi-provisioning-watcher`](../developer/test-plan.md#phase-7-wi-fi-provisioning-watcher)

If fault injection will disrupt your current SSH transport (for example, SSH over `wlan0` while testing `wlan0`), run recovery tests inside `tmux` so the test process survives disconnects.

## Wi-Fi failure triage

When recovery is stuck, gather these artifacts before changing config:

1. Snapshot operational state:
   - `/opt/photoframe/bin/print-status.sh`
2. Inspect watcher state files:
   - `sudo cat /var/lib/photoframe/wifi-state.json`
   - `sudo cat /var/lib/photoframe/wifi-last.json`
   - `sudo ls -l /var/lib/photoframe/wifi-request.json` (if present)
3. Inspect NetworkManager:
   - `nmcli dev status`
   - `nmcli connection show --active`
4. Verify kiosk session/Sway reachability:
   - `sudo systemctl status greetd.service`
   - `sudo sh -lc 'uid=$(id -u kiosk); ls "/run/user/$uid"/sway-ipc.*.sock'`
5. Validate credential apply path manually:
   - `sudo -u kiosk /opt/photoframe/bin/wifi-manager nm add --ssid "<ssid>" --psk "<password>"`

If triage still fails, collect bundle + journals and attach to issue triage:

```bash
tests/collect_logs.sh
sudo journalctl -u photoframe-wifi-manager.service --since "15 min ago"
```
