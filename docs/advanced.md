# Advanced

Optional and deeper-dive topics: cloud sync, Wi-Fi recovery internals, power/sleep model, memory tuning, and the display stacks (KMS by default, Wayland as a fallback).

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
- Shows the hotspot instructions full screen whenever Wi-Fi needs attention. The viewer draws them itself when it answers on its control socket, on both display stacks (see [The recovery overlay](#the-recovery-overlay)). Otherwise wifi-manager falls back to its own fullscreen window through Sway IPC, and in `app-handoff` mode stops the photo app while that window is up and relaunches it afterwards.
- Emits structured logs for deterministic states (`Online`, `OfflineGrace`, `RecoveryHotspotActive`, `ProvisioningAttempt`, `RecoveryBackoff`).

### Subcommands

The deployed binary lives at `/opt/photoframe/bin/wifi-manager`:

| Subcommand | Purpose |
| --- | --- |
| `watch`   | Default daemon. Monitors connectivity, raises hotspot/UI when offline, reconnects when provisioning succeeds. |
| `ui`      | Runs only the HTTP UI server (auto-spawned by `watch`; useful for debugging). |
| `qr`      | Generates `/var/lib/photoframe/wifi-qr.png`. |
| `nm`      | Thin wrapper around `nmcli` operations. Safe to run manually for diagnostics. |
| `overlay` | Renders the recovery overlay as a window (the fallback when the viewer does not draw it). Auto-invoked by the watcher. |

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
  # control-socket: /run/photoframe/control.sock
  # viewer-config: /etc/photoframe/config.yaml
```

| Key | Description |
| --- | --- |
| `interface` | Wireless device monitored (default `wlan0`). |
| `check-interval-sec` | Base delay between connectivity probes; small jitter added internally. |
| `offline-grace-sec` | Seconds offline before the hotspot activates. |
| `recovery-mode` | Applies only when the fallback window draws the overlay: `app-handoff` (default) stops the photo app while the window is up and relaunches it afterwards; `overlay` keeps it running under the window. When the viewer draws the overlay, nothing is stopped. |
| `recovery-reconnect-probe-sec` | Seconds between auto-reconnect probes while hotspot mode is active. |
| `recovery-connect-timeout-sec` | Maximum wait for infrastructure association when applying credentials. |
| `wordlist-path` | Source of the random three-word passphrase. |
| `var-dir` | Runtime artifact directory. |
| `hotspot.connection-id` | NetworkManager profile name. |
| `hotspot.ssid` | Recovery hotspot SSID. |
| `hotspot.ipv4-addr` | Hotspot interface address. |
| `ui.bind-address`, `ui.port` | HTTP UI bind. |
| `photo-app.launch-command`, `photo-app.app-id` | Used in `app-handoff` mode. |
| `overlay.command`, `overlay.photo-app-id`, `overlay.overlay-app-id` | Sway IPC wiring for the fallback window. |
| `overlay.sway-socket` | Explicit Sway IPC socket for the fallback window; auto-discovered when unset. |
| `overlay.control-socket` | The viewer's control socket. Leave it unset to follow the viewer's `control-socket-path` in `overlay.viewer-config` (default `/etc/photoframe/config.yaml`), or `/run/photoframe/control.sock` if that file sets none. |

After editing: `sudo systemctl restart photoframe-wifi-manager.service`.

### The recovery overlay

When wifi-manager enters recovery it sends the overlay's content to the viewer (`wifi-overlay-show`, see [Control socket protocol](#control-socket-protocol)): the hotspot SSID and password, the two QR payloads (join the hotspot, open the setup page), and the text. The viewer draws it full screen, in the greeting screen's colours, font and frame, over the greeting or the slideshow.

- **The slideshow pauses while the overlay covers it.** A transition in progress is undone, so its incoming photo is shown later instead, and no photo is reported as shown while covered. When the overlay goes away, the photo on screen gets its full dwell again.
- **Sleep wins.** While the frame is asleep (or the panel is off), the overlay is held and appears when the frame wakes.
- **It is a lease.** wifi-manager renews it every 10 s and the viewer drops it 30 s after the last renewal. The overlay therefore comes back within 10 s of a viewer restart, and disappears on its own if wifi-manager stops. The journal shows `wifi_overlay_shown`, `wifi_overlay_hidden` and `wifi_overlay_expired`.
- **Fallback.** A viewer that does not answer (an older build, or none running) gets the window as before. On the KMS stack there is no window to fall back to, so wifi-manager keeps offering the overlay to the viewer until it answers.

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

`buttond` owns wake/sleep scheduling and decides when the panel goes off and on; `powerctl` carries that out. Configuration is documented in [Configure › `buttond`](configure.md#buttond-power-button-daemon) — this section explains the model, the `powerctl` tool and the control socket both talk to.

### Always-on vs. scheduled

- **No `awake-schedule`:** `buttond` keeps the frame awake at all times. Manual sleep/wake commands still work.
- **With `awake-schedule`:** `buttond` drives the frame between awake and asleep at each boundary, applying the schedule's current state after the greeting delay.

### powerctl

`/opt/photoframe/bin/powerctl {sleep|wake|state} [OUTPUT]` switches the panel off (`sleep`), on (`wake`), or prints `on`/`off` (`state`).

It asks the viewer first, with `display-power` on the control socket. On the default KMS stack the viewer holds the display and is the only process that can switch it (see [Display power and the Wi-Fi overlay on KMS](#display-power-and-the-wi-fi-overlay-on-kms)). powerctl falls back to sway (`swaymsg output <OUTPUT> power off|on`) only when the viewer cannot help: on the Wayland fallback, where the viewer answers "unsupported" because sway owns the output; when no viewer is running; or when the viewer is too old to answer. Any other refusal is the answer. powerctl prints it (for example `powerctl: the viewer refused: not-driven: this viewer drives HDMI-A-2, not HDMI-A-1`) and exits 1. A `timeout` refusal means the viewer was slow, not that it ignored the request: it may still carry it out. `PHOTOFRAME_CONTROL_SOCKET` overrides the socket path; buttond sets it to its own `control-socket-path`.

> Run `powerctl` as the `kiosk` user. The control socket's directory is `kiosk:kiosk` mode `0770`, and the sway fallback looks for a sway session owned by the running UID; any other user gets "no sway process found for uid N".

```bash
sudo -u kiosk /opt/photoframe/bin/powerctl wake
sudo -u kiosk /opt/photoframe/bin/powerctl sleep
sudo -u kiosk /opt/photoframe/bin/powerctl wake HDMI-A-2   # explicit connector
sudo -u kiosk /opt/photoframe/bin/powerctl state
```

When `buttond.screen.display-name` is set in config, `buttond` always passes it to `powerctl`. On KMS it must name the connector the viewer drives, or the viewer refuses (`not-driven`). When omitted, `powerctl` means whatever the viewer drives (KMS) or every output (sway).

### Manual overrides

Pipe JSON to the control socket to override the schedule temporarily:

```bash
echo '{"command":"set-state","state":"awake"}'  | sudo -u kiosk socat - UNIX-CONNECT:/run/photoframe/control.sock
echo '{"command":"set-state","state":"asleep"}' | sudo -u kiosk socat - UNIX-CONNECT:/run/photoframe/control.sock
echo '{"command":"toggle-state"}'               | sudo -u kiosk socat - UNIX-CONNECT:/run/photoframe/control.sock
```

Manual overrides persist until the next schedule boundary — the override clears the moment the schedule's own desired state matches it, at which point the frame resumes following the schedule. Pressing again toward the opposite state agrees with the schedule and clears the override immediately (a natural "undo"). Overrides are in-memory, so a `buttond` restart resets to schedule-following.

### Control socket protocol

The viewer listens on `control-socket-path` (default `/run/photoframe/control.sock`, directory `kiosk:kiosk` mode `0770`). Each connection carries one request: the client writes one JSON object and closes its sending side, and the viewer answers with one JSON line and closes. Reading the answer is optional: a client that writes and disconnects (as buttond does) still has its command applied. `socat` does the half-close and waits for the answer:

```bash
echo '{"command":"display-power"}' | sudo -u kiosk socat -t 3 - UNIX-CONNECT:/run/photoframe/control.sock
```

| Request | Answer |
| --- | --- |
| `{"command":"set-state","state":"awake"}` or `"asleep"` | `{"ok":true}` |
| `{"command":"toggle-state"}` | `{"ok":true}` |
| `{"command":"display-power","state":"off"}` or `"on"`; optional `"output":"HDMI-A-2"` | `{"ok":true,"state":"off","connector":"HDMI-A-2","mode":"3840x2160@60"}` |
| `{"command":"display-power"}` (query) | same as above |
| `{"command":"wifi-overlay-show","ttl-sec":30,"headline":"…","body":"…","ssid":"…","password":"…","join-qr":"WIFI:T:WPA;S:…;P:…;;","portal-url":"http://192.168.4.1:8080/","footer":"…"}` | `{"ok":true}` |
| `{"command":"wifi-overlay-hide"}` | `{"ok":true}` |
| `{"command":"ping"}` | `{"ok":true}`; a client uses it to check the viewer speaks this protocol before sending anything sensitive |

A refusal is `{"ok":false,"error":KIND,"detail":"…"}`, KIND being:

- `unsupported`: `display-power` on the Wayland fallback; sway owns the output, use `swaymsg`.
- `not-driven`: `output` names a connector this viewer does not drive.
- `invalid`: the request did not parse or is out of limits (texts up to 256 characters, QR payloads up to 512 bytes and encodable).
- `timeout`: the viewer did not answer within 3 s.
- `failed`: the viewer tried and the operation failed (for example, the kernel refused the atomic commit).

`display-power` reports the viewer's own state, not a value read back from the kernel, and only exists on the KMS stack. `wifi-overlay-show` holds the overlay for `ttl-sec` (5–300, default 30); repeat it to keep the overlay up. The viewer never logs the overlay's content, because it carries the hotspot password. The types are defined once, in `crates/config-model/src/control.rs`.

### Pi 5 + Dell S2725QC notes

- **Skip `/sys/class/backlight`** — external HDMI panels don't expose a kernel backlight; writing there is a no-op.
- **Method:** the viewer's atomic CRTC switch on the KMS stack, `swaymsg output <NAME> power off|on` on the Wayland stack, both via `powerctl`.
- **Manual last resort:** `vcgencmd display_power 0|1` still works on Pi 5; `powerctl` does not use it.
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

The Linux OOM killer terminates processes. `photoframe` is not always the first target — `wireplumber` and others may go first. Signs of OOM: photos stop updating; `photoframe` exits and systemd restarts it (greeting reappears); `dmesg` shows "Out of memory: Kill process".

```bash
sudo dmesg | grep -i "oom\|killed" | tail -20
sudo journalctl -t photoframe -n 100 | grep -i "killed\|error"
```

---

## Display stacks

The frame runs the viewer on one of two display stacks. Exactly one is enabled at a time.

| | **KMS** (default) | **Wayland** (fallback) |
| --- | --- | --- |
| Unit | `photoframe-kms.service` | `greetd.service` |
| What runs | the viewer alone, presenting to DRM/KMS | `greetd → photoframe-session → sway → viewer` |
| Extra packages | none | `greetd`, `seatd`, `sway`, `swaybg`, `swayidle`, `swaylock`, `wayland-protocols` |
| 4K frame rate | vblank-locked 60 fps | about 32 fps (the compositor's ceiling on Mesa 26) |
| Display power | the viewer, over the control socket | `swaymsg` |
| Wi-Fi recovery overlay | drawn by the viewer | drawn by the viewer; a separate fullscreen window only if the viewer does not answer |

KMS is the default because the compositor is the frame-rate ceiling: on Mesa 26.2 with sway 1.10 every present mode is capped near 32 fps at 4K, while the viewer on KMS holds 60. See [performance.md](performance.md#the-viewer-on-kms) for the measurements. The Wayland stack stays for one release (v1.2) as a fallback; expect it to be removed after that.

Which stack is running:

```bash
./setup/tools/verify.sh | grep 'display stack'   # "display stack: kms (photoframe-kms.service, default)"
cat /etc/photoframe/display.env              # the persisted choice, if any
```

### Switching

`PHOTOFRAME_DISPLAY` (`kms` or `wayland`) chooses the stack at setup. With nothing given and nothing persisted, the stack is `kms`. An explicit choice is written to `/etc/photoframe/display.env`, and every later `install-all.sh`, `system/install.sh` or `deploy.sh` keeps it until you give another.

To switch to the Wayland fallback, run the system stage with the choice. It installs the compositor packages and the greetd and sway files, enables `greetd.service`, disables `photoframe-kms.service` and starts the new stack:

```bash
PHOTOFRAME_DISPLAY=wayland ./setup/system/install.sh
```

To switch back to the default:

```bash
PHOTOFRAME_DISPLAY=kms ./setup/system/install.sh
```

Run it without `sudo` in front, as shown; the script elevates itself and keeps the variable. `PHOTOFRAME_DISPLAY=wayland sudo ./setup/system/install.sh` loses the variable to sudo's environment reset and silently stays on KMS. The running viewer keeps the display until the stage's last step, when the new stack starts.

The compositor packages stay installed after a switch back, so a later switch to Wayland is quick. `PHOTOFRAME_DISPLAY=… ./setup/application/deploy.sh` also persists the choice and switches, but it cannot install packages: on a frame set up on KMS, `PHOTOFRAME_DISPLAY=wayland` is refused with a message that names the command above, nothing is persisted and the frame stays on KMS.

Each switch:

- enables the chosen unit before it disables the other, so a failed enable never leaves the frame without a stack at the next boot;
- restarts the chosen unit (the two units conflict, so starting one stops the other and releases the display);
- makes `postcheck`, `verify.sh` and `print-status` check the chosen unit (`verify.sh` checks `greetd.service` instead while a pre-v1.2 viewer holds the frame on Sway).

> **Updating a frame installed before v1.2.** It has no `/etc/photoframe/display.env`, so the first install or deploy of v1.2 moves it to KMS. To stay on Sway through the update, run `PHOTOFRAME_DISPLAY=wayland ./setup/application/deploy.sh` (greetd is already installed there) and the choice is remembered.

To try the other stack once without changing the persisted choice, start its unit; the next boot returns to the enabled stack:

```bash
sudo systemctl start photoframe-kms.service
sudo systemctl start greetd.service            # only where the Wayland stack is installed
```

### The launcher and its environment

Both stacks start the viewer through one script, `/usr/local/bin/photoframe` (installed by the system stage, `setup/system/modules/45-launcher.sh`). It checks that `/opt/photoframe/bin/photoframe` exists, exports `WINIT_APP_ID`, sends the viewer's output to the journal under the tag `photoframe` (or elsewhere, per `PHOTOFRAME_LOG`) and passes its arguments (the config path) through. `photoframe-kms.service` runs it directly and sets `PHOTOFRAME_SURFACE=drm`; on the Wayland stack sway's config `exec`s it.

The viewer reads these variables. They are environment, not `config.yaml` keys, because they describe how the process is launched:

| Variable | Values | Default |
| --- | --- | --- |
| `PHOTOFRAME_SURFACE` | `drm` or `wayland` | `wayland` when unset; the KMS unit sets `drm` |
| `PHOTOFRAME_PRESENT_MODE` | `fifo`, `mailbox`, `immediate` | `fifo` on KMS; `mailbox` (else the driver's vsync mode) on Wayland |
| `PHOTOFRAME_TRANSITION_SCALE` | `1`–`4`; the linear downscale of the buffer transitions render into, `1` is native resolution | `2` |
| `PHOTOFRAME_IRIS_LAYER_SCALE` | `1`–`16`; overrides every iris transition's `layer-scale` (the petal shading resolution; the outline stays crisp at any scale), for tuning. A warning is logged when it does | unset: each iris transition's `layer-scale` (default `4`) |
| `PHOTOFRAME_MIP_SHARPEN` | `0`–`2`; how strongly the image transitions read is sharpened when a photo is loaded, to offset the upscale's softness (`0` = none; edges stay halo-free at any value). Applies to photos loaded after a restart | `1` |
| `PHOTOFRAME_UPSCALE` | `bilinear`, `bicubic`, `bicubic-9`; how transition frames are upscaled from the intermediate. The cubics are sharper but cost 18 / 25.5 ms a 4K frame on the Pi 5, too slow for 60 Hz (see [performance.md](performance.md)) | `bilinear` |
| `PHOTOFRAME_PROBE` | `single-sample` (diagnostic: transitions read one photo per pixel and look wrong on purpose; see [performance.md](performance.md)); remove after measuring | unset |
| `PHOTOFRAME_DRM_CONNECTOR` | e.g. `HDMI-A-1` (KMS only; a name no card has is an error) | first connected connector |
| `PHOTOFRAME_DRM_MODE` | `WxH[@Hz]` (KMS only; must exist) | `3840x2160@60` if offered, else the panel's preferred mode |
| `PHOTOFRAME_DEBUG_BEZIER` | `1`, `true`, `yes` or `on`; a developer diagnostic: draws a debug Bézier stroke over the current photo instead of the transitions. Not for a running frame | unset |
| `PHOTOFRAME_LOG` | `journal`, `stdout`, `file:/path` (read by the launcher) | `journal` |
| `RUST_LOG` | a `tracing` filter, e.g. `photoframe=debug` | `info` |
| `WGPU_BACKEND` | `vulkan` or `gl` (wgpu's own override) | Vulkan |
| `PHOTOFRAME_CONTROL_SOCKET` | path (read by `powerctl`, not the viewer) | `/run/photoframe/control.sock` |

A value the viewer cannot parse is logged and ignored, except `PHOTOFRAME_SURFACE`, `PHOTOFRAME_DRM_MODE` and `PHOTOFRAME_DRM_CONNECTOR`, where it is an error and the viewer exits. A `PHOTOFRAME_MIP_SHARPEN` outside `0`–`2` is clamped (with a warning), not ignored. Setup also reads `PHOTOFRAME_DISPLAY` (above), which is a setup choice and never reaches the viewer.

**Setting them on KMS.** Add a drop-in; it survives deploys, which rewrite the unit file:

```bash
sudo systemctl edit photoframe-kms.service
```

```ini
[Service]
Environment=PHOTOFRAME_TRANSITION_SCALE=1
Environment=PHOTOFRAME_DRM_CONNECTOR=HDMI-A-2
```

Then `sudo systemctl restart photoframe-kms.service`. The journal shows `viewer_scale_override` and the other overrides at startup.

**Setting them on Wayland.** Export the variable in `/usr/local/bin/photoframe` (the file has a commented block for it) or prefix the `exec` line in `/usr/local/share/photoframe/sway/config`. Both files are rewritten by the system stage, so reapply the edit after rerunning it.

---

## KMS stack (default)

With the KMS stack the viewer presents straight to the display (DRM/KMS through Vulkan's `VK_KHR_display`), with no greetd, sway or compositor in between. Greeting, slideshow, sleep/wake through the control socket, transitions, mats, the showcase captions and the `metrics` log lines all behave as on the Wayland stack. So do the Wi-Fi recovery overlay and display power, which the viewer handles itself on this stack (next section).

### Provisioning

`sudo ./setup/system/install.sh` (called from `install-all.sh`) performs the kiosk-related actions:

- Verifies `/etc/os-release` reports `VERSION_CODENAME=trixie`.
- Applies Pi 5 boot tweaks (set `ENABLE_4K_BOOT=0` to skip the 4K60 profile).
- Creates the locked `kiosk` user with `video`, `render` and `input` group membership. The unit opens `/dev/dri` through those groups; there is no login session and no seat manager.
- Provisions `/run/photoframe` as `kiosk:kiosk` mode `0770` plus a tmpfiles entry for boot-time creation.
- Installs the launcher `/usr/local/bin/photoframe`.
- Disables competing display managers (`gdm3`, `sddm`, `lightdm`) and masks `getty@tty1.service`.
- Deploys the `photoframe-wifi-manager.service`, `buttond.service`, `photoframe-kms.service` and `photoframe-sync.timer` units. The application deploy enables `photoframe-kms.service` and restarts it.

### Verification

```bash
grep VERSION_CODENAME /etc/os-release
sudo systemctl status photoframe-kms
sudo journalctl -t photoframe -b | grep -E "kms_|viewer presenting"
./setup/tools/verify.sh
```

Expected: `photoframe-kms` reports `active (running)` and the journal has `viewer presenting straight to KMS`. `verify.sh` reports `KMS display: card1 HDMI-A-2 3840x2160@60 power=on` (card, connector and mode), cross-checked between sysfs and the viewer.

### Idempotent reruns

Re-running provisioning after OS or app updates is supported. Idempotent status lines like `getty@tty1.service already masked; skipping disable/mask` are normal.

### Display power and the Wi-Fi overlay on KMS

Nothing on this stack talks to sway. The viewer draws the [Wi-Fi recovery overlay](#the-recovery-overlay) itself, and `powerctl` (so buttond's button and schedule) switches the panel through the viewer's `display-power` command.

The viewer switches the panel itself with atomic modesetting, which is how the kernel expects power to be switched on drivers like the Pi's vc4: it turns the display pipe (the CRTC) off and on. It does not use the older DPMS property, which on these drivers is only a stand-in for that switch and whose read-back the kernel calls unreliable.

**Off.** The viewer first lets the frames it already submitted reach the screen, then turns the CRTC off. The journal shows `kms_power_off: panel off crtc_active=Ok(false)`. While the panel is off, the viewer:
- draws nothing and waits for no vblank;
- pauses the slideshow, so no photo goes by unseen;
- skips its display-change check, because a TV in standby often drops hot-plug detect, which would otherwise look like an unplugged panel.

**On.** The viewer turns the CRTC back on with a full modeset: the same commit Mesa makes when it first takes the display at startup, which drives the HDMI link up from nothing. Simply re-enabling the output is not trusted to re-drive vc4's HDMI link; the picframe3 project saw TVs stay on "no signal" that way. The journal shows `kms_power_on: modeset committed crtc_active=Ok(true) vblank=Ok(())`, where the vblank proves the CRTC is running. It then shows `kms_power_on: connector reads connected after_ms=…` once the TV has re-asserted hot-plug detect. The display-change check resumes 15 s after the modeset.

**Why the viewer does the modeset itself.** Mesa's display code does not know the viewer switched the CRTC off.
- From Mesa 25.3 on, a frame presented onto the dark CRTC is refused by the kernel, and Mesa then loses the swapchain, which would restart the viewer.
- Up to 25.2, Mesa falls back to its own modeset and lights the panel behind the viewer's back.

So nothing is presented while the panel is off, and the wake commit always comes before the next frame.

**A wake never leaves the panel dark with nothing left to try.** Before the wake commit, if the TV reads connected, the viewer checks that it still offers the mode in use (a different TV, or a changed EDID, while the panel was off). A failed commit is retried once. If the mode is gone or the retry fails too, `display-power on` answers `failed … the viewer restarts to light the panel`, and the viewer exits. The restart picks a mode again and lights the panel with the startup modeset.

**Restarts while the panel is off.** The viewer starts by waiting for a connected display. A TV in standby may have dropped hot-plug detect and will only raise it when it sees a signal. So while waiting, the viewer still answers `display-power`:
- **on** forces a probe of the connectors (a fresh read of hot-plug detect and the EDID) and starts on the display if that finds one;
- otherwise the answer says no display is connected, and the TV has to be woken with its own remote or input button.

Commands that arrive while waiting (sleep or wake, the Wi-Fi overlay) are applied as soon as the viewer runs. A `display-power off` received while waiting is remembered: if the display appears later on its own, the viewer starts with the panel off.

**What only your eyes can confirm.** The journal shows the display pipe running after a wake. Whether the TV shows a picture at 3840x2160@60 also depends on the HDMI link (at 4K60, the HDMI 2.0 scrambling the TV must accept). If a wake ever leaves the TV on "no signal", report it with the TV model.

**Known limits:**
- If the viewer restarts while the panel is off and the TV keeps hot-plug detect up, the panel comes back on with the greeting, while buttond still believes the frame is asleep. A sway restart does the same on the Wayland stack. The next schedule boundary or button press puts it right.
- If a wake fails twice, the viewer restarts to light the panel and comes back in the schedule's state. buttond retries a failed screen power-on with backoff, as it does a failed power-off, and repeats its wake once the panel is on, so a manual wake in sleep hours ends on the slideshow within a few seconds rather than on the sleep banner.

### How it runs

`photoframe-kms.service` runs as `kiosk` (groups `video render input`), with `PHOTOFRAME_SURFACE=drm`. It starts the [launcher](#the-launcher-and-its-environment) and logs keep the `photoframe` tag:

```bash
sudo journalctl -t photoframe -b | grep -E "kms_|viewer presenting"
```

At startup the viewer logs `kms_probe` lines (cards, connectors, modes, the Vulkan plane) and `kms_target`, then `viewer presenting straight to KMS` with the time each startup step took (`instance_surface_ms`, `device_ms`, `greeting_ms`, `pipelines_ms`, `overlays_ms`; see [Operate](operate.md#reading-the-photo-logs)), then `viewer: greeting on screen` when the greeting's first frame is presented. The startup state (`set-state`) is held while the greeting is on (`viewer: greeting on; request held until it is over`) and applied once it has been on screen for `greeting-screen.duration-seconds`. On every stop it logs the teardown steps in order: `kms_teardown: releasing swapchain`, then `destroying the wgpu instance`, then `closing the card`. The unit's `TimeoutStopSec=10s` bounds that teardown.

The variables the unit honours (`PHOTOFRAME_DRM_CONNECTOR`, `PHOTOFRAME_DRM_MODE`, `PHOTOFRAME_PRESENT_MODE` and the rest) are listed under [The launcher and its environment](#the-launcher-and-its-environment).

### When it stops

**No display at startup.** If no display is connected when the unit starts, the viewer waits for one and checks once a second. This covers a power cut where the Pi boots before the TV, or a receiver in standby. The journal shows `kms_waiting_for_display` once. When the panel appears, the slideshow starts.

**Display changes.** v1.2 does not adapt to a display change in place. Every time the viewer wakes from idle, it checks the connector. Once per photo, at the end of the dwell, it also forces a fresh read of the panel's mode list, so a panel swapped between two checks is caught. In each of these cases the viewer exits with an error and `Restart=always` brings it back:

- the panel was unplugged;
- the panel no longer offers the mode in use;
- the swapchain is lost;
- frames repeatedly time out.

The restart waits for a display and picks its mode again. Because the check runs on idle wakes, an unplug is noticed at the next photo change, command or matting result.

**Startup failures.** If the viewer cannot start, systemd retries five times in two minutes. The usual cause is that greetd/sway (a Wayland stack that is still running) or plymouth still holds the display; the journal reports this as "not DRM master". It then leaves the unit failed and the screen dark. There is deliberately no automatic fallback to greetd. To recover, fix the cause, then:

```bash
sudo systemctl reset-failed photoframe-kms.service && sudo systemctl start photoframe-kms.service
```

You can also switch to the Wayland stack, as described under [Switching](#switching).

---

## Wayland stack (fallback)

The Wayland stack boots straight into the Wayland app via greetd on Debian 13 (Trixie). greetd launches a dedicated Sway session on tty1 and runs the photo frame as the `kiosk` user, with no display manager shims or PAM templates. It was the only stack before v1.2 and is kept for one release in case a display or Mesa combination does not work on KMS. Choose it with `PHOTOFRAME_DISPLAY=wayland` ([Switching](#switching)).

### Provisioning

With the Wayland choice, `sudo ./setup/system/install.sh` additionally:

- Installs the compositor packages: `greetd`, `seatd`, `sway`, `swaybg`, `swayidle`, `swaylock`, `wayland-protocols` (the base set already has `dbus` and `dbus-user-session`).
- Adds the `kiosk` user to the `seat` group (required to access `/run/seatd.sock`).
- Installs `/usr/local/bin/photoframe-session` and the sway config `/usr/local/share/photoframe/sway/config`.
- Writes `/etc/greetd/config.toml` so tty1 runs `photoframe-session` as `kiosk`.
- Enables `greetd` as `display-manager` and `seatd`, and disables `photoframe-kms.service`.

### Expected state

`/etc/greetd/config.toml`:

```toml
[terminal]
vt = 1

[default_session]
command = "/usr/local/bin/photoframe-session"
user = "kiosk"
```

- The wrapper launches Sway via `dbus-run-session`; `seatd.service` provides seat management (Sway connects to `/run/seatd.sock`).
- Sway config: `/usr/local/share/photoframe/sway/config`.
- `greetd` owns tty1; no autologin hacks.
- Launch chain: `greetd → photoframe-session → dbus-run-session → sway → /usr/local/bin/photoframe`.

### Verification

```bash
sudo systemctl status greetd
sudo systemctl status display-manager
sudo journalctl -u greetd -b
```

Expected: `greetd` reports `active (running)`; the unit command line includes `/usr/local/bin/photoframe-session`; the journal shows the kiosk session and slideshow startup.

For deeper debug workflows (manual Sway launch, overlay tests, kiosk-shell environment export), see [developer/architecture.md › Debugging the kiosk stack](../developer/architecture.md#debugging-the-kiosk-stack).
