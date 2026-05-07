# Operate

Day-to-day commands, status checks, and troubleshooting for a frame you own and maintain.

> Run commands from your operator SSH session unless noted. `sudo -u kiosk` is required for anything that touches the Wayland session (`powerctl`, `socat` to the control socket from a root shell).

---

## Command cheat sheet

### Control the frame

| What you want | Command |
| --- | --- |
| Wake (start cycling) | `echo '{"command":"set-state","state":"awake"}' \| sudo -u kiosk socat - UNIX-CONNECT:/run/photoframe/control.sock` |
| Sleep (stop cycling, blank) | `echo '{"command":"set-state","state":"asleep"}' \| sudo -u kiosk socat - UNIX-CONNECT:/run/photoframe/control.sock` |
| Toggle wake ↔ sleep | `echo '{"command":"ToggleState"}' \| sudo -u kiosk socat - UNIX-CONNECT:/run/photoframe/control.sock` |
| Screen on (DPMS) | `sudo -u kiosk /opt/photoframe/bin/powerctl wake` |
| Screen off (DPMS) | `sudo -u kiosk /opt/photoframe/bin/powerctl sleep` |
| Screen on, explicit output | `sudo -u kiosk /opt/photoframe/bin/powerctl wake HDMI-A-2` |

### Check status

| What you want | Command |
| --- | --- |
| Quick health check | `./setup/tools/verify.sh` |
| Full status summary | `/opt/photoframe/bin/print-status.sh` |
| Live photo logs | `sudo journalctl -t photoframe -f` |
| Wi-Fi manager logs | `sudo journalctl -u photoframe-wifi-manager.service -f` |
| Button daemon logs | `sudo journalctl -u buttond.service -f` |
| All service status | `sudo systemctl status greetd photoframe-wifi-manager buttond` |
| Count photos in library | `find /var/lib/photoframe/photos -type f \| wc -l` |
| List connected outputs | `sudo -u kiosk wlr-randr \| grep connected` |
| Check control socket | `sudo ls -l /run/photoframe/control.sock` |

### Manage

| What you want | Command |
| --- | --- |
| Restart kiosk (reliable) | `sudo systemctl stop greetd.service && sleep 1 && sudo systemctl start greetd.service` |
| Edit config | `sudo nano /etc/photoframe/config.yaml` |
| Apply config changes | restart kiosk (above) |
| Add photos from laptop | `scp /path/to/photos/* frame@photoframe.local:/var/lib/photoframe/photos/local/` |
| Add photos locally | `sudo cp /path/*.jpg /var/lib/photoframe/photos/local/ && sudo chown kiosk:kiosk /var/lib/photoframe/photos/local/*` |
| Trigger manual cloud sync | `sudo systemctl start photoframe-sync.service` |
| Update software | `git pull && ./setup/application/deploy.sh` |

> **Don't use `systemctl restart greetd`** — it can race with logind seat handoff on tty1 and leave the session in a bad state. Always stop, sleep, then start.

### Diagnose

| What you want | Command |
| --- | --- |
| Last 50 photo logs | `sudo journalctl -t photoframe -n 50 --no-pager` |
| Logs since boot | `sudo journalctl -t photoframe -b --no-pager` |
| Wi-Fi state | `sudo cat /var/lib/photoframe/wifi-state.json` |
| Check swap | `swapon --show` |
| Collect log bundle | `tests/collect_logs.sh` |
| Run diagnostics script | `sudo ./setup/system/tools/diagnostics.sh` |

---

## Daily operations

### Daily health check

```bash
./setup/tools/verify.sh
/opt/photoframe/bin/print-status.sh
sudo systemctl status greetd photoframe-wifi-manager buttond
```

Expected: all three services `active (running)`, `print-status.sh` exits cleanly.

### Reading the photo logs

`sudo journalctl -t photoframe -f` shows the slideshow loop:

| Line pattern | Meaning |
| --- | --- |
| `loaded <path>` | A photo was decoded and queued |
| `displaying photo N of M` | Active cycling |
| `transition <kind> <duration>ms` | GPU transition started |
| `invalid photo <path>` | Decode failed; that file is skipped until restart |
| `state change: asleep → awake` | Frame received a wake command |
| `schedule: sleeping until HH:MM` | `buttond` put the frame to sleep based on schedule |

### Adding photos

Both `local/` (manual imports, never overwritten by sync) and `cloud/` (managed by the sync service) under `/var/lib/photoframe/photos` are scanned recursively. Supported formats: JPEG, PNG.

From your laptop:

```bash
scp /path/to/photos/*.jpg frame@photoframe.local:/var/lib/photoframe/photos/local/
# With a non-default key:
scp -i ~/.ssh/photoframe /path/to/photos/*.jpg frame@photoframe.local:/var/lib/photoframe/photos/local/
```

Or locally on the Pi:

```bash
sudo cp /path/to/photos/*.jpg /var/lib/photoframe/photos/local/
sudo chown kiosk:kiosk /var/lib/photoframe/photos/local/*.jpg
```

If you get **permission denied** during `scp`, your SSH session predates the install (which added your account to the `kiosk` group). Log out and reconnect.

### Editing configuration

Edit `/etc/photoframe/config.yaml` (not the template at `/opt/photoframe/etc/photoframe/config.yaml` — that's overwritten on redeploy). Restart the kiosk to apply changes. See [Configure](configure.md) for the full reference.

### Updating the software

```bash
cd ~/photoframe
git pull
./setup/application/deploy.sh
```

`deploy.sh` builds in the background and restarts services at the end. `/etc/photoframe/config.yaml` and `/var/lib/photoframe/` are untouched. If the update includes system-level changes (new packages, unit changes), run the full installer instead:

```bash
./setup/install-all.sh
```

### Triggering a manual cloud sync

If `photoframe-sync.timer` is enabled, it runs automatically. To trigger now:

```bash
sudo systemctl start photoframe-sync.service
sudo journalctl -u photoframe-sync.service -f
```

Check the next scheduled run: `systemctl list-timers photoframe-sync.timer`. To set up cloud sync for the first time, see [Advanced › Cloud sync](advanced.md#cloud-sync).

### Wi-Fi recovery validation

After a fresh install, confirm Wi-Fi recovery works *before* mounting the frame:

```bash
make -f tests/Makefile wifi-recovery
```

If your SSH session is over `wlan0`, run inside `tmux` so it survives the test disconnecting Wi-Fi:

```bash
tmux new -s wifi-recovery
ALLOW_WIFI_SSH_DROP=1 make -f tests/Makefile wifi-recovery
# After reconnect:
tmux attach -t wifi-recovery
```

For deeper Wi-Fi behavior, see [Advanced › Wi-Fi recovery](advanced.md#wi-fi-recovery).

---

## Troubleshooting

### Screen shows greeting then goes black

**This is the most common first-boot surprise — it's not a crash.** After the greeting the frame enters sleep state. The GPU is idle and the display blanks. The frame is waiting for a wake command or a schedule window.

```bash
echo '{"command":"set-state","state":"awake"}' \
  | sudo -u kiosk socat - UNIX-CONNECT:/run/photoframe/control.sock
```

If you have an `awake-schedule` configured, also check that the current local time falls inside a wake window and that the timezone is correct. An empty list for a day key (e.g. `friday: []`) means **sleep all day on that day** — delete the key to fall back to `daily`.

### `powerctl wake` returns "no sway process found for uid N"

`powerctl` looks for a Wayland session owned by the running UID. The kiosk session belongs to `kiosk`, not your operator account. Always run with `sudo -u kiosk`:

```bash
sudo -u kiosk /opt/photoframe/bin/powerctl wake
```

### No photos cycling after the frame wakes

1. **Library empty?** `find /var/lib/photoframe/photos -type f | head -20` — add photos if it returns nothing.
2. **Permission error?** If you copied as root, `sudo chown -R kiosk:kiosk /var/lib/photoframe/photos/`.
3. **Unsupported format?** Only JPEG and PNG decode; check `sudo journalctl -t photoframe -n 50 --no-pager` for `invalid photo` lines.

### Black screen from the start — no greeting ever appears

The kiosk session (greetd + Sway) didn't start.

```bash
sudo systemctl status greetd.service
sudo journalctl -u greetd.service -b --no-pager | tail -40
```

Re-run the deploy: `./setup/application/deploy.sh`. greetd may not fully initialize without an active HDMI signal — confirm the display is connected and powered on before booting.

### Frame wakes or sleeps at the wrong time

Most often a misconfigured `awake-schedule`. Check:

- `timezone` is a valid IANA name (`America/New_York`, `Europe/London`, etc.). Wrong timezones fire boundaries at unexpected local times.
- A `day-of-week: []` entry (e.g. `friday: []`) means **sleep all day**. Delete the key to fall back to `daily`.
- The `daily` window is the default when no specific day key matches.

Restart `buttond` after editing: `sudo systemctl restart buttond.service`. `buttond` logs each evaluation — `sudo journalctl -u buttond.service -f` shows the next boundary.

### Build fails with "signal: 9" / "killed"

OOM during the Rust build. Common on Pi 4, rare on Pi 5.

```bash
CARGO_BUILD_JOBS=2 ./setup/install-all.sh
# or, deploy only:
CARGO_BUILD_JOBS=2 ./setup/application/deploy.sh
```

Verify swap: `swapon --show` should list `zram0`. If missing, re-run `sudo ./setup/system/install.sh`.

### Install fails: "OS codename must be trixie"

Setup requires Debian 13. Re-flash with the Trixie 64-bit Raspberry Pi OS image. See [Install › Step 1](install.md#step-1--flash-the-sd-card).

### Wi-Fi provisioning hotspot never appears

When the frame loses Wi-Fi, it should raise the `PhotoFrame-Setup` hotspot after about 30 seconds.

```bash
sudo systemctl status photoframe-wifi-manager.service
sudo journalctl -u photoframe-wifi-manager.service -f
```

If the service is stopped: `sudo systemctl start photoframe-wifi-manager.service`. If it's running but no hotspot appears, confirm the configured interface matches your hardware:

```bash
grep '^interface:' /opt/photoframe/etc/wifi-manager.yaml
nmcli -t -f DEVICE,TYPE,STATE device status
```

Wait the full grace period (30 s default) after disconnecting. For deeper Wi-Fi triage, see [Advanced › Wi-Fi recovery](advanced.md#wi-fi-recovery).

### Wi-Fi failure triage

When recovery is stuck, gather artifacts before changing anything:

1. `/opt/photoframe/bin/print-status.sh`
2. ```bash
   sudo cat /var/lib/photoframe/wifi-state.json
   sudo cat /var/lib/photoframe/wifi-last.json
   sudo ls -l /var/lib/photoframe/wifi-request.json
   ```
3. ```bash
   nmcli dev status
   nmcli connection show --active
   ```
4. Confirm Sway socket exists for kiosk:
   ```bash
   sudo sh -lc 'uid=$(id -u kiosk); ls "/run/user/$uid"/sway-ipc.*.sock'
   ```
5. Try the manual credential apply path:
   ```bash
   sudo -u kiosk /opt/photoframe/bin/wifi-manager nm add --ssid "<ssid>" --psk "<password>"
   ```
6. Collect a log bundle:
   ```bash
   tests/collect_logs.sh
   sudo journalctl -u photoframe-wifi-manager.service --since "15 min ago" --no-pager
   ```

If step 5 reports `Insufficient privileges`, re-run provisioning to reinstall the polkit rule.

### Cannot write photos to `/var/lib/photoframe/photos`

Your operator account was added to the `kiosk` group during install, but your current SSH session predates that change.

```bash
exit
ssh frame@photoframe.local
groups   # should now include "kiosk"
```

### Monitor stays on but photos freeze or stop updating

Likely an OOM kill. Check:

```bash
sudo journalctl -t photoframe -n 100 --no-pager | grep -i "killed\|oom\|memory"
sudo dmesg | grep -i "oom\|killed" | tail -20
```

To reduce memory use, edit `/etc/photoframe/config.yaml`:

```yaml
viewer-preload-count: 1    # was 3
global-photo-settings:
  oversample: 0.75         # was 1.0
```

Restart the kiosk after editing. See [Advanced › Memory tuning](advanced.md#memory-tuning) for a full budget breakdown.

### Display power commands run but the monitor stays on

Output name mismatch. Find the connector:

```bash
sudo -u kiosk wlr-randr | grep connected
```

Update `buttond.screen.display-name` in `/etc/photoframe/config.yaml` to match (e.g. `HDMI-A-2`), then `sudo systemctl restart buttond.service`. See [Advanced › Power and sleep](advanced.md#power-and-sleep).

### `socat` command not found

```bash
sudo apt install -y socat
```

### `verify.sh` reports warnings about greetd

On a first deploy, `verify.sh` may warn about greetd if system provisioning was not yet complete. Re-run both stages:

```bash
sudo ./setup/system/install.sh
./setup/application/deploy.sh
./setup/tools/verify.sh
```

### Collect logs for a bug report

```bash
tests/collect_logs.sh
sudo journalctl -t photoframe --since "30 min ago" --no-pager > /tmp/photoframe.log
sudo journalctl -u photoframe-wifi-manager.service --since "30 min ago" --no-pager >> /tmp/photoframe.log
```

Attach both outputs when filing an issue.
