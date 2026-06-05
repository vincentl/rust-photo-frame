# Advanced

Optional and deeper-dive topics: cloud sync, Wi-Fi recovery internals, power/sleep model, memory tuning, and the kiosk stack.

---

## Cloud sync

The frame can keep `/var/lib/photoframe/photos/cloud/` in sync with cloud storage on a timer. Photos in `cloud/` appear in rotation alongside anything in `local/`. Everything below is optional — skip it if you only add photos manually.

### How it works

- The sync service downloads photos into `/var/lib/photoframe/photos/cloud/`.
- `/var/lib/photoframe/photos/local/` is never touched.
- `photoframe-sync.timer` triggers the sync (hourly default).
- The timer is **disabled by default**; it activates only after you configure a source.

Sync is safe while the frame is displaying photos: it stages downloads, then promotes them to `cloud/` in a single step. Unchanged files are hard-linked across staging and `cloud/`, so sync uses no extra disk space.

### Supported providers

The frame uses [rclone](https://rclone.org), which supports 40+ providers including Google Drive (`drive`), Dropbox (`dropbox`), S3 (`s3`), OneDrive (`onedrive`), Backblaze B2 (`b2`), iCloud Drive (`iclouddrive`), SFTP, WebDAV. Full list: `rclone help backends`.

For a NAS on your local network, you can use rsync directly — see [Using rsync instead](#using-rsync-instead).

### Setup

**1. Configure an rclone remote** on the Pi:

```bash
rclone config
```

The wizard walks through provider selection and OAuth (it prints a URL to authorize in a browser, then asks for the confirmation code). Verify when done:

```bash
rclone listremotes        # e.g. "gdrive:"
rclone ls gdrive:My\ Photos/Frame
```

**2. Copy the rclone config to the kiosk user.** rclone stores config at `~/.config/rclone/rclone.conf`. The wizard ran as your operator user, but the sync service runs as `kiosk`:

```bash
sudo mkdir -p /home/kiosk/.config/rclone
sudo cp ~/.config/rclone/rclone.conf /home/kiosk/.config/rclone/rclone.conf
sudo chown -R kiosk:kiosk /home/kiosk/.config/rclone
```

**3. Set the sync source.** Edit `/etc/photoframe/sync.env`:

```bash
sudo nano /etc/photoframe/sync.env
```

Uncomment and set:

```bash
RCLONE_REMOTE=gdrive:My Photos/Frame
```

Format is `remote-name:path` — the remote name must exactly match `rclone listremotes`.

**4. Test a manual sync:**

```bash
sudo systemctl start photoframe-sync.service
sudo journalctl -u photoframe-sync.service -f
ls /var/lib/photoframe/photos/cloud/
```

**5. Enable the hourly timer:**

```bash
sudo systemctl enable --now photoframe-sync.timer
systemctl list-timers photoframe-sync.timer
```

### Changing the sync schedule

```bash
sudo systemctl edit photoframe-sync.timer
```

Add (replacing `OnCalendar=` with your schedule):

```ini
[Timer]
OnCalendar=
OnCalendar=*-*-* 02:00:00
```

The empty `OnCalendar=` clears the default before applying yours. Common values:

| Schedule | `OnCalendar=` |
| --- | --- |
| Every hour (default) | `hourly` |
| Every 15 minutes | `*:0/15` |
| Once a day at 2 AM | `*-*-* 02:00:00` |
| Twice a day | `*-*-* 06,18:00:00` |

### Using rsync instead

```bash
sudo nano /etc/photoframe/sync.env
```

Set:

```bash
SYNC_TOOL=rsync
RSYNC_SOURCE=user@nas.local:/photos/frame/
# RSYNC_FLAGS=-av --delete  # optional override
```

The Pi must be able to SSH to the source without a password. Set up a key for the `kiosk` user:

```bash
sudo -u kiosk ssh-keygen -t ed25519 -N "" -f /home/kiosk/.ssh/id_ed25519
sudo -u kiosk ssh-copy-id user@nas.local
```

### Sync troubleshooting

| Symptom | Likely cause | Fix |
| --- | --- | --- |
| `RCLONE_REMOTE must be set` in logs | `sync.env` not configured | Edit `/etc/photoframe/sync.env` |
| `Failed to create file system for ... not found` | Remote name wrong | `rclone listremotes`; check spelling |
| `directory not found` | Path doesn't exist | `rclone lsd gdrive:` to list folders |
| Auth errors / token expired | OAuth token needs refresh | `rclone config reconnect gdrive:` as operator, then copy config to kiosk |
| Photos don't appear after sync | Wrong file format | Only JPEG and PNG; `rclone ls` to verify types |
| `permission denied` writing to `cloud/` | Permissions issue | `sudo chown -R kiosk:kiosk /var/lib/photoframe/photos/cloud` |

If you configured rclone as your operator user but the service runs as `kiosk`, the most common issue is the rclone config not being present for `kiosk` — copy it as in step 2.

---

## Wi-Fi recovery

`wifi-manager` is the frame's single entry point for Wi-Fi monitoring, hotspot recovery, and captive-portal provisioning. It wraps NetworkManager's `nmcli`, spawns the recovery web UI, and persists operational breadcrumbs under `/var/lib/photoframe`.

### What it does

- Polls NetworkManager for the interface's connection state.
- Treats Wi-Fi as online when the interface is associated to an infrastructure SSID (link-level only — no internet reachability requirement).
- Creates/updates the `pf-hotspot` NetworkManager profile and brings it online with a random three-word passphrase.
- Serves an HTTP UI for SSID/password entry on `192.168.4.1:8080`, plus a QR code (`/var/lib/photoframe/wifi-qr.png`) phones can scan to jump to the portal.
- Uses Sway IPC to present a fullscreen overlay with hotspot instructions whenever Wi-Fi needs attention. Can also stop/relaunch the photo app (`app-handoff` mode).
- Emits structured logs for deterministic states (`Online`, `OfflineGrace`, `RecoveryHotspotActive`, `ProvisioningAttempt`, `RecoveryBackoff`).

### Subcommands

The deployed binary lives at `/opt/photoframe/bin/wifi-manager`:

| Subcommand | Purpose |
| --- | --- |
| `watch`   | Default daemon. Monitors connectivity, raises hotspot/UI when offline, reconnects when provisioning succeeds. |
| `ui`      | Runs only the HTTP UI server (auto-spawned by `watch`; useful for debugging). |
| `qr`      | Generates `/var/lib/photoframe/wifi-qr.png`. |
| `nm`      | Thin wrapper around `nmcli` operations. Safe to run manually for diagnostics. |
| `overlay` | Renders the on-device recovery overlay window. Auto-invoked by the watcher. |

`--help` and `--version` are permitted as root; all other modes refuse `UID==0`.

### Configuration

Template at `/opt/photoframe/etc/wifi-manager.yaml` (staged from `setup/assets/app/etc/wifi-manager.yaml`):

```yaml
interface: wlan0
check-interval-sec: 5
offline-grace-sec: 30
recovery-mode: app-handoff
recovery-reconnect-probe-sec: 300
recovery-connect-timeout-sec: 20
wordlist-path: /opt/photoframe/share/wordlist.txt
var-dir: /var/lib/photoframe
hotspot:
  connection-id: pf-hotspot
  ssid: PhotoFrame-Setup
  ipv4-addr: 192.168.4.1
ui:
  # Bind the recovery portal to the hotspot address so it is reachable only on
  # the recovery AP, never on the home LAN.  Leave unset to follow hotspot
  # ipv4-addr automatically; set 0.0.0.0 only for local testing.
  bind-address: 192.168.4.1
  port: 8080
photo-app:
  launch-command:
    - /usr/local/bin/photoframe
    - /etc/photoframe/config.yaml
  app-id: photoframe
overlay:
  command:
    - swaymsg
  photo-app-id: photoframe
  overlay-app-id: wifi-overlay
```

| Key | Description |
| --- | --- |
| `interface` | Wireless device monitored (default `wlan0`). |
| `check-interval-sec` | Base delay between connectivity probes; small jitter added internally. |
| `offline-grace-sec` | Seconds offline before the hotspot activates. |
| `recovery-mode` | `app-handoff` (default) stops/relaunches photo app; `overlay` keeps slideshow running under overlay. |
| `recovery-reconnect-probe-sec` | Seconds between auto-reconnect probes while hotspot mode is active. |
| `recovery-connect-timeout-sec` | Maximum wait for infrastructure association when applying credentials. |
| `wordlist-path` | Source of the random three-word passphrase. |
| `var-dir` | Runtime artifact directory. |
| `hotspot.connection-id` | NetworkManager profile name. |
| `hotspot.ssid` | Recovery hotspot SSID. |
| `hotspot.ipv4-addr` | Hotspot interface address. |
| `ui.bind-address`, `ui.port` | HTTP UI bind. |
| `photo-app.launch-command`, `photo-app.app-id` | Used in `app-handoff` mode. |
| `overlay.command`, `overlay.photo-app-id`, `overlay.overlay-app-id` | Sway IPC wiring for the overlay. |

After editing: `sudo systemctl restart photoframe-wifi-manager.service`.

### Runtime files

All under `/var/lib/photoframe`, owned by `kiosk`:

- `hotspot-password.txt` — current random passphrase for `PhotoFrame-Setup`
- `wifi-qr.png` — QR pointing to `http://<hotspot-ip>:<port>/`
- `wifi-request.json` — ephemeral credential request from `POST /submit` (mode `0600`)
- `wifi-last.json` — latest provisioning attempt record (inputs masked, result + timestamps)
- `wifi-state.json` — watcher state (`state`, `reason`, optional `attempt_id`)

### NetworkManager permissions

`wifi-manager` runs as `kiosk`. The setup pipeline installs `/etc/polkit-1/rules.d/90-photoframe-nm.rules` granting the kiosk group the NetworkManager actions needed to add, modify, and activate Wi-Fi profiles. Without this rule, manual `nm` subcommands fail with `Insufficient privileges`.

### Service management

```bash
sudo journalctl -u photoframe-wifi-manager.service -f          # tail logs
sudo systemctl restart photoframe-wifi-manager.service          # after config edit
/opt/photoframe/bin/print-status.sh                             # status summary
sudo -u kiosk /opt/photoframe/bin/wifi-manager nm add --ssid "HomeWiFi" --psk "secret"
sudo nmcli connection up pf-hotspot                             # force recovery hotspot for testing
```

For day-2 triage steps, see [Operate › Wi-Fi failure triage](operate.md#wi-fi-failure-triage).

### Disable wifi-manager

```bash
sudo systemctl stop photoframe-wifi-manager.service
sudo systemctl disable photoframe-wifi-manager.service
sudo systemctl mask photoframe-wifi-manager.service       # prevents setup scripts from re-enabling
sudo nmcli connection delete pf-hotspot || true            # optional: drop the hotspot profile
```

Re-enable: `sudo systemctl unmask photoframe-wifi-manager.service && sudo systemctl enable --now photoframe-wifi-manager.service`.

---

## Power and sleep

`buttond` owns wake/sleep scheduling and DPMS commands. Configuration is documented in [Configure › `buttond`](configure.md#buttond-power-button-daemon) — this section explains the model and the `powerctl` tool.

### Always-on vs. scheduled

- **No `awake-schedule`:** `buttond` keeps the frame awake at all times. Manual sleep/wake commands still work.
- **With `awake-schedule`:** `buttond` drives the frame between awake and asleep at each boundary, applying the schedule's current state after the greeting delay.

### powerctl

`/opt/photoframe/bin/powerctl` bootstraps the Wayland environment, issues `wlr-randr` DPMS requests, and falls back to `vcgencmd display_power`.

> `powerctl` must run as the `kiosk` user. It searches for a Wayland session owned by the running UID. Any other user produces "no sway process found for uid N".

```bash
sudo -u kiosk /opt/photoframe/bin/powerctl wake
sudo -u kiosk /opt/photoframe/bin/powerctl sleep
sudo -u kiosk /opt/photoframe/bin/powerctl wake HDMI-A-2   # explicit connector
```

When `buttond.screen.display-name` is set in config, `buttond` always passes it to `powerctl`. When omitted, `powerctl` auto-detects the first connected output.

### Manual overrides

Pipe JSON to the control socket to override the schedule temporarily:

```bash
echo '{"command":"set-state","state":"awake"}'  | sudo -u kiosk socat - UNIX-CONNECT:/run/photoframe/control.sock
echo '{"command":"set-state","state":"asleep"}' | sudo -u kiosk socat - UNIX-CONNECT:/run/photoframe/control.sock
echo '{"command":"ToggleState"}'                | sudo -u kiosk socat - UNIX-CONNECT:/run/photoframe/control.sock
```

Manual overrides persist until the next schedule boundary — the override clears the moment the schedule's own desired state matches it, at which point the frame resumes following the schedule. Pressing again toward the opposite state agrees with the schedule and clears the override immediately (a natural "undo"). Overrides are in-memory, so a `buttond` restart resets to schedule-following.

### Pi 5 + Dell S2725QC notes

- **Skip `/sys/class/backlight`** — external HDMI panels don't expose a kernel backlight; writing there is a no-op.
- **Primary method:** `wlr-randr --output <NAME> --off|--on` via `powerctl`.
- **Fallback:** `vcgencmd display_power 0|1` still works on Pi 5 KMS. Default `powerctl` chains both.
- **CEC:** the Dell S2725QC does not implement HDMI-CEC. `cec-ctl` cannot power it down.

---

## Memory tuning

How `photoframe` uses RAM at runtime, and how to tune it down.

### Budget by Pi model

The frame keeps several decoded frames in memory simultaneously (configurable via `viewer-preload-count`). On a 3840×2160 display with `oversample: 1.0`, a single RGBA image is ~33 MiB. With the default preload of 3 plus intermediate matting copies, steady-state can exceed 400 MiB before OS overhead.

| Pi RAM | OS + system | Available to frame | Default preload | Recommended oversample |
| --- | --- | --- | --- | --- |
| 2 GiB | ~500 MiB | ~1.5 GiB | reduce to 1 | 0.75 |
| 4 GiB | ~500 MiB | ~3.5 GiB | 3 (default) | 1.0 |
| 8 GiB | ~500 MiB | ~7.5 GiB | 4–5 | 1.0–1.5 |

Heavy matting (studio, blur, fixed-image) and large backgrounds increase usage. Monitor with:

```bash
ps -o pid,rss,vsz,comm -p $(pgrep photoframe)
# or in htop: F4 to filter by 'photoframe'
```

### Pipeline: where memory goes

1. **Loader decode buffer** — source decoded to raw RGBA. Channel between loader and viewer holds `viewer-preload-count` of these.
2. **Matting worker input** — clone of the decoded frame sent to the CPU matting pipeline.
3. **Matting output canvas** — full-screen RGBA canvas at display resolution × `oversample`. ~33 MiB per 4K frame.
4. **GPU upload staging** — padded staging buffer aligned for WGPU row requirements, held until upload completes.
5. **Fixed-image backgrounds** — each configured background is decoded once and cached at full canvas resolution indefinitely.

With three frames in flight, copies 1–4 stack across all three.

### Mitigation levers

Apply in order — each has diminishing returns:

**1. Reduce `viewer-preload-count`** (highest impact)

```yaml
viewer-preload-count: 1   # default 3
```

Cutting from 3 to 1 trims ~200 MiB on a 4K display. 1–2 still hides most decode latency on fast SD cards.

**2. Dial back `oversample`**

```yaml
global-photo-settings:
  oversample: 0.75   # default 1.0
```

Reduces every matting canvas and GPU texture by 44% (0.75² = 0.56).

**3. Cull large backgrounds.** Each `fixed-image` background is cached at full canvas resolution forever. A list of five 4K backgrounds adds ~165 MiB permanently. Keep the list short or pre-scale backgrounds.

**4. Switch to lighter matting styles.** `fixed-color` requires no intermediate copies. `blur` and `studio` are heavier. Disabling matting (`active: []`) is the most aggressive option.

**5. Constrain source resolution.** Pre-scale very large source photos (50 MP RAW exports) to display resolution before adding them.

### Profiling

```bash
watch -n 2 'ps -o pid,rss,vsz,comm -p $(pgrep photoframe)'
```

The footprint stabilizes after the first few transitions. Take a steady-state reading with the frame awake and cycling, then compare before and after each tuning change.

### What happens when memory runs out

The Linux OOM killer terminates processes. `photoframe` is not always the first target — `wireplumber` and others may go first. Signs of OOM: photos stop updating; `photoframe` exits and greetd restarts the session (greeting reappears); `dmesg` shows "Out of memory: Kill process".

```bash
sudo dmesg | grep -i "oom\|killed" | tail -20
sudo journalctl -t photoframe -n 100 | grep -i "killed\|error"
```

---

## Kiosk stack

The frame boots straight into the Wayland app via greetd on Debian 13 (Trixie). greetd launches a dedicated Sway session on tty1 and runs the photo frame as the `kiosk` user — no display manager shims or PAM templates.

### Provisioning

`sudo ./setup/system/install.sh` (called from `install-all.sh`) performs the kiosk-related actions:

- Verifies `/etc/os-release` reports `VERSION_CODENAME=trixie`.
- Applies Pi 5 boot tweaks (set `ENABLE_4K_BOOT=0` to skip the 4K60 profile).
- Installs Wayland/kiosk dependencies: `greetd`, `sway`, `swaybg`, `swayidle`, `swaylock`, `wayland-protocols`, `dbus`, `dbus-user-session`.
- Creates the locked `kiosk` user with `video`, `render`, and `input` group membership.
- Provisions `/run/photoframe` as `kiosk:kiosk` mode `0770` plus a tmpfiles entry for boot-time creation.
- Installs `/usr/local/bin/photoframe-session`.
- Writes `/etc/greetd/config.toml` so tty1 runs `photoframe-session` as `kiosk`.
- Disables competing display managers (`gdm3`, `sddm`, `lightdm`), enables `greetd` as `display-manager`, and masks `getty@tty1.service`.
- Deploys the `photoframe-wifi-manager.service`, `buttond.service`, and `photoframe-sync.timer` units.

### Expected state

`/etc/greetd/config.toml`:

```toml
[terminal]
vt = 1

[default_session]
command = "/usr/local/bin/photoframe-session"
user = "kiosk"
```

- The wrapper launches Sway via `dbus-run-session`/`seatd-launch`.
- Sway config: `/usr/local/share/photoframe/sway/config`.
- `greetd` owns tty1; no autologin hacks.
- Launch chain: `greetd → photoframe-session → dbus-run-session → seatd-launch → sway`.

### Verification

```bash
grep VERSION_CODENAME /etc/os-release
sudo systemctl status greetd
sudo systemctl status display-manager
sudo journalctl -u greetd -b
```

Expected: `greetd` reports `active (running)`; the unit command line includes `/usr/local/bin/photoframe-session`; the journal shows the kiosk session and slideshow startup.

### Idempotent reruns

Re-running provisioning after OS or app updates is supported. Idempotent status lines like `getty@tty1.service already masked; skipping disable/mask` are normal.

For deeper debug workflows (manual Sway launch, overlay tests, kiosk-shell environment export), see [CONTRIBUTING.md › Debugging the kiosk stack](../CONTRIBUTING.md#debugging-the-kiosk-stack).
