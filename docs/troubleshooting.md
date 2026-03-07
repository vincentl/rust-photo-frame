# Troubleshooting

Find your symptom below. Each entry gives the most likely cause and the fastest fix.

For daily commands and quick status checks, see [docs/quick-reference.md](quick-reference.md).

---

## Screen shows greeting ("Warming up…") then goes black

**This is the most common first-boot surprise.** It is not a crash.

**What's happening:** After the greeting, the frame enters sleep state. Sleep means the GPU is idle and the display goes dark. The frame is running normally — it's waiting for a wake command or a scheduled wake window.

**Fix A — Wake the frame manually:**

```bash
echo '{"command":"set-state","state":"awake"}' \
  | sudo -u kiosk socat - UNIX-CONNECT:/run/photoframe/control.sock
```

Photos should begin cycling within a few seconds.

**Fix B — Check your awake-schedule:**

If you have an `awake-schedule` configured in `/etc/photoframe/config.yaml`, the frame may be sleeping because the current time falls outside a wake window.

Open the config:

```bash
sudo nano /etc/photoframe/config.yaml
```

Look for the `awake-schedule` block. Check:
- Is `timezone` set correctly for your location?
- Does the current day/time fall inside a declared window?
- Is there a `day: []` entry (e.g. `friday: []`) for today? An empty list means "sleep all day" on that day.

To disable the schedule and run always-on, comment out the entire `awake-schedule` block, then restart:

```bash
sudo systemctl stop greetd.service && sleep 1 && sudo systemctl start greetd.service
```

---

## `powerctl wake` returns "no sway process found for uid N"

**Cause:** `powerctl` looks for a Wayland (Sway) session owned by the user running the command. The kiosk session belongs to the `kiosk` user — not your operator account.

**Fix:** Run `powerctl` as the `kiosk` user:

```bash
sudo -u kiosk /opt/photoframe/bin/powerctl wake
sudo -u kiosk /opt/photoframe/bin/powerctl sleep
```

The `sudo -u kiosk` prefix is always required when running `powerctl` from an operator shell.

---

## No photos cycling after the frame wakes

**Check 1 — Is the library empty?**

```bash
find /var/lib/photoframe/photos -type f | head -20
```

If nothing is listed, add photos. See [installation.md Step 4](installation.md#step-4--add-your-photos).

**Check 2 — Permission error during copy?**

If you copied files as root, the frame (running as `kiosk`) may not be able to read them:

```bash
sudo chown -R kiosk:kiosk /var/lib/photoframe/photos/
```

**Check 3 — Are your files in a supported format?**

The frame decodes JPEG and PNG only. Other formats are silently skipped. Check the logs:

```bash
sudo journalctl -t photoframe -n 50 --no-pager
```

Look for `invalid photo` or `decode error` lines — those file paths won't be shown.

**Check 4 — Permission error writing to the library?**

If you're getting "permission denied" when trying to `scp` or `cp` files, your operator account probably hasn't picked up group membership yet. Log out and SSH back in:

```bash
exit
ssh frame@photoframe.local
```

---

## Black screen from the start — no greeting ever appears

The kiosk session (greetd + Sway) did not start.

**Step 1 — Check the service:**

```bash
sudo systemctl status greetd.service
```

If it says `failed` or `inactive`, look at the journal:

```bash
sudo journalctl -u greetd.service -b --no-pager | tail -40
```

**Step 2 — Re-run the deploy:**

```bash
./setup/application/deploy.sh
```

This is safe to run repeatedly. It will re-provision any missing config or unit files.

**Step 3 — Check display connection:**

greetd may not fully initialize without an active HDMI signal. Make sure the display is powered on and connected before booting.

---

## Frame wakes or sleeps at the wrong time

**Most likely cause:** a misconfigured `awake-schedule` — wrong timezone, day-of-week override, or an unexpected empty-list day.

**Step 1 — Check the schedule:**

```bash
sudo nano /etc/photoframe/config.yaml
```

Things to verify:
- `timezone` must be a valid IANA name (e.g. `America/New_York`, `Europe/London`, `America/Los_Angeles`). A wrong timezone causes all boundaries to fire at incorrect local times.
- A `day-of-week: []` entry (e.g. `friday: []`) means "sleep all day." Delete the key to let that day fall back to the `daily` window.
- The `daily` window is the default when no more specific key matches.

**Step 2 — Restart buttond after config changes:**

```bash
sudo systemctl restart buttond.service
sudo journalctl -u buttond.service -f
```

buttond logs each schedule evaluation — you'll see the next boundary and whether the frame is awake or asleep.

---

## Build fails with "signal: 9" or "killed"

The Rust build ran out of memory. This is common on Pi 4 (2–4 GiB) and rare on Pi 5 (4–8 GiB).

**Fix — cap build parallelism:**

```bash
CARGO_BUILD_JOBS=2 ./setup/install-all.sh
# or for the deploy step alone:
CARGO_BUILD_JOBS=2 ./setup/application/deploy.sh
```

**Verify swap is present:**

```bash
swapon --show
```

You should see a `zram0` entry. If it's missing, re-run the system provisioning step:

```bash
sudo ./setup/system/install.sh
```

---

## Install fails: "OS codename must be trixie"

The setup scripts require Debian 13 (Trixie). Earlier Raspberry Pi OS builds use Bullseye or Bookworm.

**Check your OS:**

```bash
grep VERSION_CODENAME /etc/os-release
```

If it's not `trixie`, re-flash the SD card with the Trixie 64-bit Raspberry Pi OS image. See [installation.md Step 1](installation.md#step-1--flash-the-sd-card).

---

## Wi-Fi provisioning hotspot never appears

When the frame loses Wi-Fi, it should raise a hotspot named `PhotoFrame-Setup` after about 30 seconds.

**Quick checks:**

```bash
sudo systemctl status photoframe-wifi-manager.service
sudo journalctl -u photoframe-wifi-manager.service -f
```

If the service is stopped, start it:

```bash
sudo systemctl start photoframe-wifi-manager.service
```

If it's running but no hotspot appears:
- Confirm the configured interface matches your actual Wi-Fi adapter: `grep '^interface:' /opt/photoframe/etc/wifi-manager.yaml`
- Compare against: `nmcli -t -f DEVICE,TYPE,STATE device status`
- Wait the full grace period (default 30 seconds) after disconnecting

For full Wi-Fi triage, see [docs/wifi-manager.md](wifi-manager.md#troubleshooting).

---

## Cannot write photos to `/var/lib/photoframe/photos`

**Cause:** Your operator account was added to the `kiosk` group during install, but your current SSH session predates that change.

**Fix:** Log out and reconnect:

```bash
exit
ssh frame@photoframe.local
```

Then verify group membership:

```bash
groups
```

You should see `kiosk` in the list.

---

## Monitor stays on but photos freeze or stop updating

**Likely cause:** the `photoframe` process was killed by the OOM killer (out of memory).

**Check:**

```bash
sudo journalctl -t photoframe -n 100 --no-pager | grep -i "killed\|oom\|memory"
sudo dmesg | grep -i "oom\|killed" | tail -20
```

**Fix — reduce memory use:**

Edit `/etc/photoframe/config.yaml`:

```yaml
viewer-preload-count: 1    # was 3
global-photo-settings:
  oversample: 0.75         # was 1.0
```

Restart the kiosk after editing. See [docs/memory.md](memory.md) for a full budget breakdown.

---

## `socat` command not found

The wake/sleep control commands use `socat`. If it's missing:

```bash
sudo apt install -y socat
```

---

## `./setup/tools/verify.sh` reports warnings about greetd

On a first deploy, the verify script may warn about greetd if system provisioning was not yet complete. Re-run both stages in order:

```bash
sudo ./setup/system/install.sh
./setup/application/deploy.sh
./setup/tools/verify.sh
```

---

## Collect logs for a bug report

If you're stuck and need to report an issue:

```bash
tests/collect_logs.sh
sudo journalctl -t photoframe --since "30 min ago" --no-pager > /tmp/photoframe.log
sudo journalctl -u photoframe-wifi-manager.service --since "30 min ago" --no-pager >> /tmp/photoframe.log
```

Attach both outputs when filing an issue.
