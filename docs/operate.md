# Operate

Day-to-day commands, status checks, and troubleshooting for a frame you own and maintain.

> Run commands from your operator SSH session unless noted. `sudo -u kiosk` is required for anything that talks to the viewer's control socket (`powerctl`, `socat`). The commands below are for the default KMS stack; where the Wayland fallback differs, the difference is called out. See [Advanced › Display stacks](advanced.md#display-stacks) for how to tell which stack you have.

---

## Command cheat sheet

### Control the frame

| What you want | Command |
| --- | --- |
| Wake (start cycling) | `echo '{"command":"set-state","state":"awake"}' \| sudo -u kiosk socat - UNIX-CONNECT:/run/photoframe/control.sock` |
| Sleep (stop cycling, blank) | `echo '{"command":"set-state","state":"asleep"}' \| sudo -u kiosk socat - UNIX-CONNECT:/run/photoframe/control.sock` |
| Toggle wake ↔ sleep | `echo '{"command":"toggle-state"}' \| sudo -u kiosk socat - UNIX-CONNECT:/run/photoframe/control.sock` |
| Screen on (DPMS) | `sudo -u kiosk /opt/photoframe/bin/powerctl wake` |
| Screen off (DPMS) | `sudo -u kiosk /opt/photoframe/bin/powerctl sleep` |
| Screen on, explicit output | `sudo -u kiosk /opt/photoframe/bin/powerctl wake HDMI-A-2` |
| Is the screen on? | `sudo -u kiosk /opt/photoframe/bin/powerctl state` |
| Screen power, straight to the viewer | `echo '{"command":"display-power","state":"off"}' \| sudo -u kiosk socat -t 6 - UNIX-CONNECT:/run/photoframe/control.sock` (`"on"`; omit `state` to query; answers `unsupported` on the Wayland fallback) |
| Take the Wi-Fi overlay down | `echo '{"command":"wifi-overlay-hide"}' \| sudo -u kiosk socat -t 3 - UNIX-CONNECT:/run/photoframe/control.sock` (wifi-manager shows it again within 10 s while recovery lasts) |

Every control-socket command answers with one JSON line (`{"ok":true,…}`); the full list is in [Advanced › Control socket protocol](advanced.md#control-socket-protocol).

### Check status

| What you want | Command |
| --- | --- |
| Quick health check | `./setup/tools/verify.sh` |
| Full status summary | `/opt/photoframe/bin/print-status.sh` |
| Live photo logs | `sudo journalctl -t photoframe -f` |
| Wi-Fi manager logs | `sudo journalctl -u photoframe-wifi-manager.service -f` |
| Button daemon logs | `sudo journalctl -u buttond.service -f` |
| All service status | `sudo systemctl status photoframe-kms photoframe-wifi-manager buttond` |
| Which display stack | `./setup/tools/verify.sh \| grep 'display stack'` (or the `Display stack:` line of `print-status.sh`) |
| Count photos in library | `find /var/lib/photoframe/photos -type f \| wc -l` |
| Which display the viewer drives (connector, mode, power) | `echo '{"command":"display-power"}' \| sudo -u kiosk socat -t 3 - UNIX-CONNECT:/run/photoframe/control.sock` |
| Check control socket | `sudo ls -l /run/photoframe/control.sock` |

### Manage

| What you want | Command |
| --- | --- |
| Restart kiosk | `sudo systemctl restart photoframe-kms.service` (if the panel was asleep it comes back on with the greeting until the next schedule boundary or button press) |
| Edit config | `sudo nano /etc/photoframe/config.yaml` |
| Apply config changes | restart kiosk (above) |
| Add photos from laptop | `rsync -a -e ssh /path/to/photos/ frame@photoframe.local:/var/lib/photoframe/photos/local/` |
| Add photos locally | `sudo cp /path/*.jpg /var/lib/photoframe/photos/local/ && sudo chown kiosk:kiosk /var/lib/photoframe/photos/local/*` |
| Trigger manual cloud sync | `sudo systemctl start photoframe-sync.service` |
| Update software | `git pull && ./setup/application/deploy.sh` (a frame installed before v1.2 and still on Sway moves to KMS; to stay on Sway run `PHOTOFRAME_DISPLAY=wayland ./setup/application/deploy.sh` once, see below) |

> **On the Wayland fallback, don't use `systemctl restart greetd`.** It can race with logind seat handoff on tty1 and leave the session in a bad state. Always stop, sleep, then start: `sudo systemctl stop greetd.service && sleep 1 && sudo systemctl start greetd.service`.

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

### Running the showcase

The showcase is a labeled tour of every transition and mat so you can choose which to configure. See [showcase/README.md](../showcase/README.md) for the full guide and photo setup.

```bash
# 1. Stage media (once)
sudo install -d -o kiosk -g kiosk -m 0755 \
  /var/lib/photoframe/showcase/photos /var/lib/photoframe/showcase/backgrounds
sudo cp /path/to/*.jpg /var/lib/photoframe/showcase/photos/
sudo cp /path/to/backdrop.jpg /var/lib/photoframe/showcase/backgrounds/background.jpg
sudo chown -R kiosk:kiosk /var/lib/photoframe/showcase

# 2. Activate (backs up config, swaps in showcase.yaml, restarts the kiosk)
./showcase/activate.sh
journalctl -t photoframe -f          # watch the tour

# 3. Restore your normal slideshow
./showcase/deactivate.sh
```

### Daily health check

```bash
./setup/tools/verify.sh
/opt/photoframe/bin/print-status.sh
sudo systemctl status photoframe-kms photoframe-wifi-manager buttond
```

Expected: all three services `active (running)`, `print-status.sh` exits cleanly.

### Reading the photo logs

`sudo journalctl -t photoframe -f` shows the slideshow loop:

| Line pattern | Meaning |
| --- | --- |
| `viewer presenting straight to KMS` | The viewer owns the display (`width`, `height` fields). Also how long each startup step took, in ms: `instance_surface_ms` (Vulkan instance and DRM surface), `device_ms` (adapter and device), `greeting_ms` (the greeting's text renderer and font scan), `pipelines_ms` (the slideshow's render pipelines), `overlays_ms` (the sleep and Wi-Fi overlays, plus the caption overlay when showcase captions are on) |
| `viewer: greeting on screen` | The greeting's first frame was presented; its `duration-seconds` count from here |
| `viewer: greeting on; request held until it is over request=...` | A `set-state` or `toggle-state` arrived during the greeting (the startup state, a button press); it is applied when the greeting ends (`viewer: entering wake` or `viewer: entering sleep`) |
| `playlist_scheduler order=...` | Logged once at startup: the playlist scheme and its parameters |
| `photo_display_metric seq=... path=...` | Active cycling: one line per photo shown (needs `metrics: true`) |
| `transition_frame_metric transition=<kind> frames=...` | A transition finished, with its frame statistics (needs `metrics: true`) |
| `photo failed to decode; skipping (left on disk) path=...` | Decode failed; that file is skipped |

`buttond` logs to its own unit (`sudo journalctl -u buttond.service -f`):

| Line pattern | Meaning |
| --- | --- |
| `single press → toggle frame state` | The power button was pressed once |
| `frame sleep request completed reason="manual"` | The panel was put to sleep (`reason="scheduled"` for an `awake-schedule` boundary) |
| `frame wake request completed reason="manual"` | The panel was woken (`reason="scheduled"` at the wake boundary) |

### Adding photos

Both `local/` (manual imports, never overwritten by sync) and `cloud/` (managed by the sync service) under `/var/lib/photoframe/photos` are scanned recursively. Supported formats: JPEG, PNG.

From your laptop, use `rsync` (not `scp` — the sftp subsystem doesn't load supplementary groups, so `scp` writes silently fail):

```bash
rsync -a -e "ssh" /path/to/photos/ frame@photoframe.local:/var/lib/photoframe/photos/local/
# With a non-default key:
rsync -a -e "ssh -i ~/.ssh/photoframe" /path/to/photos/ frame@photoframe.local:/var/lib/photoframe/photos/local/
```

Or locally on the Pi:

```bash
sudo cp /path/to/photos/*.jpg /var/lib/photoframe/photos/local/
sudo chown kiosk:kiosk /var/lib/photoframe/photos/local/*.jpg
```

If you get **permission denied** during `rsync`, your SSH session predates the install (which added your account to the `kiosk` group). Log out and reconnect.

### Editing configuration

Edit `/etc/photoframe/config.yaml` (not the template at `/opt/photoframe/etc/photoframe/config.yaml` — that's overwritten on redeploy). Restart the kiosk to apply changes. See [Configure](configure.md) for the full reference.

### Updating the software

```bash
cd ~/photoframe
git pull
./setup/application/deploy.sh
```

> **If the new stack does not come up after an update**, go back to Sway by hand: `sudo systemctl enable greetd.service && sudo systemctl disable photoframe-kms.service && sudo systemctl stop photoframe-kms.service && sudo systemctl start greetd.service`. The system stage (`install-all.sh`, `setup/system/install.sh`) does not switch a running Sway frame to KMS while the installed viewer predates KMS; the application deploy does, after it installs the new viewer.

> **Frames installed before v1.2 run Sway.** They have no `/etc/photoframe/display.env`, so the first v1.2 deploy switches them to the KMS stack (and stops greetd). Their checkout is also dirty: the v1.1 installers ran `chmod +x` on two setup modules that git tracked as non-executable, so `git pull` refuses with "would be overwritten". Drop those two changes first: `git checkout -- setup/application/modules/45-activate-services.sh setup/system/modules/35-firmware.sh`, then pull (v1.2 tracks them as executable, so this does not recur). To keep Sway, run `PHOTOFRAME_DISPLAY=wayland ./setup/application/deploy.sh` for that update; the choice is remembered. If the new stack does not come up, `PHOTOFRAME_DISPLAY=wayland ./setup/application/deploy.sh` also takes you back. See [Advanced › Display stacks](advanced.md#display-stacks).

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

This only happens on the Wayland fallback, where `powerctl` falls back to sway and looks for a sway session owned by the running UID. The kiosk session belongs to `kiosk`, not your operator account. Always run with `sudo -u kiosk` (on KMS it is also what reaches the control socket):

```bash
sudo -u kiosk /opt/photoframe/bin/powerctl wake
```

### No photos cycling after the frame wakes

1. **Library empty?** `find /var/lib/photoframe/photos -type f | head -20` — add photos if it returns nothing.
2. **Permission error?** If you copied as root, `sudo chown -R kiosk:kiosk /var/lib/photoframe/photos/`.
3. **Unsupported format?** Only JPEG and PNG decode; check `sudo journalctl -t photoframe -n 50 --no-pager` for `invalid photo` lines.

### Black screen from the start — no greeting ever appears

The viewer didn't reach the display.

```bash
sudo systemctl status photoframe-kms.service
sudo journalctl -t photoframe -b --no-pager | tail -40
```

If the viewer is waiting for a display the journal says `kms_waiting_for_display`: confirm the panel is connected and powered on; the slideshow starts when it appears. If the journal says "not DRM master", something else holds the display (a greetd/Sway session left from the Wayland fallback, or plymouth): `sudo systemctl stop greetd.service; sudo systemctl reset-failed photoframe-kms.service; sudo systemctl start photoframe-kms.service`. Re-run the deploy if the unit is missing: `./setup/application/deploy.sh`. See [Advanced › KMS stack](advanced.md#when-it-stops).

On the Wayland fallback the kiosk session (greetd + Sway) didn't start: `sudo systemctl status greetd.service` and `sudo journalctl -u greetd.service -b --no-pager | tail -40`. greetd may not fully initialize without an active HDMI signal.

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
4. Confirm the viewer answers on its control socket (the overlay is drawn by the viewer on KMS):
   ```bash
   echo '{"command":"display-power"}' | sudo -u kiosk socat -t 3 - UNIX-CONNECT:/run/photoframe/control.sock
   ```
   On the Wayland fallback the viewer draws the overlay too; wifi-manager uses a Sway window only when the viewer does not answer. To check for that window: `sudo sh -lc 'uid=$(id -u kiosk); ls "/run/user/$uid"/sway-ipc.*.sock'`.
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

Restart the kiosk after editing (`sudo systemctl restart photoframe-kms.service`). See [Advanced › Memory tuning](advanced.md#memory-tuning) for a full budget breakdown.

What is normal: on a 4K frame with the default preload and portrait pairing on, the viewer settles at about 1.2 to 1.3 GB resident after a few hours awake, with brief peaks near 1.6 GB while photos decode, and holds flat while the frame is asleep. That leaves more than 2 GB free on a 4 GB Pi 5. A resident size that keeps climbing hour after hour without a plateau is the thing to report.

### Display power commands run but the monitor stays on

First see which path `powerctl` took:

```bash
echo '{"command":"display-power"}' | sudo -u kiosk socat -t 3 - UNIX-CONNECT:/run/photoframe/control.sock
```

- **KMS stack, the default** (the answer has `"ok":true` and a `connector`): the viewer switches the panel. After `powerctl sleep` the journal shows `kms_power_off: panel off`. A refusal is printed by powerctl itself (`powerctl: the viewer refused: …`):

  ```bash
  sudo journalctl -t photoframe -b | grep -E "kms_power_off|kms_power_on"
  ```

  If `buttond.screen.display-name` names a different connector than the answer's `connector`, the viewer refuses with `not-driven`. Fix the name, or remove the key so powerctl means whatever the viewer drives.
- **Wayland fallback** (the answer is `"error":"unsupported"`): sway switches the panel, and the usual cause is an output name mismatch. Find the connector:

  ```bash
  # wlr-randr must run in the kiosk Wayland session (display is usually wayland-1;
  # find it with: sudo ls /run/user/$(id -u kiosk)/wayland-*)
  sudo -u kiosk env XDG_RUNTIME_DIR=/run/user/$(id -u kiosk) WAYLAND_DISPLAY=wayland-1 wlr-randr
  ```

Update `buttond.screen.display-name` in `/etc/photoframe/config.yaml` to match (e.g. `HDMI-A-2`), then `sudo systemctl restart buttond.service`. See [Advanced › Power and sleep](advanced.md#power-and-sleep).

### The panel stays black after a wake on the KMS stack

The viewer re-lights the panel with a full modeset when it gets `display-power on`. The journal should show `kms_power_on: modeset committed crtc_active=Ok(true) vblank=Ok(())`, then `kms_power_on: connector reads connected`.

```bash
sudo journalctl -t photoframe -b | grep -E "kms_power_on|kms_surface_lost"
systemctl show -p NRestarts photoframe-kms.service
```

- `kms_power_on_failed: retrying once`, followed by `exiting for a restart`: the wake commit failed twice, or the TV no longer offers the mode in use. The viewer restarted to light the panel. Report the lines.
- `kms_waiting_for_display … no display reads connected even after a forced probe`: the viewer restarted while the panel was off, and the TV dropped hot-plug detect in standby. Wake the TV with its remote or input button; the slideshow starts within a second.
- `crtc_active=Ok(false)` or a `vblank=Err(…)`: the commit returned but the CRTC is not running. Report it.
- `NRestarts` went up at the wake, or `kms_surface_lost` appears: a frame reached Mesa while the panel was off, which the design rules out. Report it, with the journal.
- All lines are right but the TV still says "no signal": that is the vc4 link failure the full modeset is meant to prevent. Note the TV model and report it.

### `socat` command not found

```bash
sudo apt install -y socat
```

### `verify.sh` reports warnings about the display stack

On a first deploy, `verify.sh` warns that `photoframe-kms.service` (or `greetd` on the Wayland fallback) is missing, or reports an error and exits 1 when it is installed but not active, if system provisioning was not yet complete. A frame still held on Sway by a pre-v1.2 viewer gets a warning and a check of `greetd.service` instead. Re-run both stages:

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
