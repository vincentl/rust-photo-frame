# Installation Guide

This guide walks you from a blank microSD card to a running slideshow. Follow the steps in order; each one builds on the last.

**Time estimate:** about 20 minutes of active work + 20–40 minutes of unattended build time on a Pi 5 (longer on a Pi 4).

**Command context:** run commands as your operator account over SSH unless the step says otherwise. Use `sudo` where shown.

> **Before you start:** Have your SSH public key ready — you'll need it during SD card imaging. If you don't have one yet, the first section below covers that.

---

## Prepare an SSH key

Raspberry Pi Imager asks for a public key during setup. Confirm one exists before you start flashing:

```bash
ls ~/.ssh/id_*.pub
```

If nothing appears, generate a key. This example creates one named `photoframe`:

```bash
ssh-keygen -t ed25519 -f ~/.ssh/photoframe -C "frame@photoframe.local"
```

**Optional:** add a shortcut to `~/.ssh/config` so you can type `ssh photoframe` later:

```
Host photoframe
    HostName photoframe.local
    User frame
    IdentityFile ~/.ssh/photoframe
    IdentitiesOnly yes
```

---

## Step 1 — Flash the SD card

1. Download and install [Raspberry Pi Imager](https://www.raspberrypi.com/software/).
2. Insert your microSD card and launch the imager.
3. **Choose Device:** Raspberry Pi 5
4. **Choose OS:** Raspberry Pi OS (64-bit) — select the **Trixie** (Debian 13) build.
5. **Choose Storage:** your microSD card.
6. Click **Next**, then **Edit Settings** when asked about customization.
7. In **General**:
   - Hostname: `photoframe`
   - Username / Password: create a dedicated user (e.g. `frame`) with a strong password
   - Configure Wireless LAN: enter your Wi-Fi SSID, passphrase, and country code
   - Set locale, timezone, and keyboard layout
8. In **Services**: enable SSH → **Allow public-key authentication only** → paste your public key (contents of `~/.ssh/photoframe.pub`)
9. Click **Save → Yes → Write** and wait for verification to finish.

---

## Step 2 — First boot and OS check

1. Insert the SD card into the Pi, connect the display, and power on.
2. Give it about a minute to join Wi-Fi, then SSH in:

   ```bash
   ssh frame@photoframe.local
   # or, if you set up the config shortcut:
   ssh photoframe
   ```

3. Confirm you're on the right OS before installing anything:

   ```bash
   grep VERSION_CODENAME /etc/os-release
   ```

   The output must say `VERSION_CODENAME=trixie`. The setup scripts require Debian 13.

4. Update packages (recommended before the automated install):

   ```bash
   sudo apt update && sudo apt upgrade -y
   ```

---

## Step 3 — Clone and install

1. Install git and clone the repository:

   ```bash
   sudo apt install -y git
   git clone https://github.com/vincentl/rust-photo-frame.git photoframe
   cd photoframe
   ```

2. Run the all-in-one installer:

   ```bash
   ./setup/install-all.sh
   ```

   This script:
   - Installs system packages and a Rust toolchain (requires `sudo` — you'll be prompted)
   - Logs out and back in automatically to pick up environment changes
   - Builds all four crates (`photoframe`, `buttond`, `wifi-manager`, `config-model`)
   - Installs binaries, unit files, and a starter config to `/opt/photoframe`
   - Starts the kiosk session, Wi-Fi watcher, and button daemon

   **What you'll see on the Pi's display during the build:** nothing; the kiosk session starts after the build finishes, not during.

   **If the build is killed with `signal: 9`** (out of memory), cap the parallelism:

   ```bash
   CARGO_BUILD_JOBS=2 ./setup/install-all.sh
   ```

3. After the installer returns, verify everything came up:

   ```bash
   ./setup/tools/verify.sh
   ```

   You should see green / OK for all critical checks. If anything is red, check the message — most issues are a missing config file or a service that needs a restart.

---

## Step 4 — Add your photos

The frame scans `/var/lib/photoframe/photos` recursively. Drop photos in the `local/` subdirectory for manual imports:

1. Make sure the directory exists with the right ownership (the installer creates it, but just in case):

   ```bash
   sudo install -d -m 2775 -o kiosk -g kiosk /var/lib/photoframe/photos/local
   ```

2. Copy photos to the Pi. From your Mac or Linux machine:

   ```bash
   scp /path/to/photos/* frame@photoframe.local:/var/lib/photoframe/photos/local/
   ```

   Or locally on the Pi:

   ```bash
   sudo cp /path/to/photos/* /var/lib/photoframe/photos/local/
   sudo chown kiosk:kiosk /var/lib/photoframe/photos/local/*
   ```

3. If you get a permission error, log out and SSH back in. The installer adds your operator account to the `kiosk` group, but your current session doesn't pick that up until you reconnect.

4. Confirm the files landed:

   ```bash
   find /var/lib/photoframe/photos -type f | wc -l
   ```

---

## Step 5 — Wake the frame

> **Why does the screen go dark?** After the greeting ("Warming up your photo memories…"), the frame enters sleep state. This is the default behavior. The frame is running normally — it's just waiting for a wake command or a schedule window before cycling photos. See [docs/first-boot.md](first-boot.md) for a full explanation.

Send a wake command over the control socket:

```bash
echo '{"command":"set-state","state":"awake"}' \
  | sudo -u kiosk socat - UNIX-CONNECT:/run/photoframe/control.sock
```

Within a few seconds the greeting (or sleep screen) should clear and photos should begin cycling.

**To make the frame always-on** (never sleep automatically), leave `awake-schedule` commented out in `/etc/photoframe/config.yaml` — that's the default shipped config. The frame will stay awake until you explicitly send a sleep command or configure a schedule.

---

## Verify

Check live logs to confirm photos are cycling:

```bash
sudo journalctl -t photoframe -f
```

Healthy output looks like:

```
photoframe: loaded /var/lib/photoframe/photos/local/sunset.jpg
photoframe: displaying photo 1 of 42
photoframe: transition fade 450ms
```

Service status check:

```bash
sudo systemctl status greetd photoframe-wifi-manager buttond
```

All three should say `active (running)`.

---

## Optional: configure a wake/sleep schedule

By default the frame is always-on. To enable timed operation, edit the config:

```bash
sudo nano /etc/photoframe/config.yaml
```

Uncomment the `awake-schedule` block and adjust times and timezone. Then restart the kiosk:

```bash
sudo systemctl stop greetd.service && sleep 1 && sudo systemctl start greetd.service
```

Full schedule options are documented in [docs/configuration.md](configuration.md#awake-schedule).

> **Watch out:** a day-of-week key with an empty list (e.g. `friday: []`) means "sleep all day on that day." Remove the key entirely to fall back to the `daily` window for that day.

---

## Optional: cloud photo sync

The installer ships a disabled `photoframe-sync` service that runs `rclone` or `rsync` on a timer. To enable it:

1. Create `/etc/photoframe/sync.env`:

   ```bash
   sudo tee /etc/photoframe/sync.env >/dev/null <<'EOF'
   # Choose one:
   # RCLONE_REMOTE=remote-name:path/to/album
   # RSYNC_SOURCE=/path/to/source/
   EOF
   sudo nano /etc/photoframe/sync.env
   ```

2. Enable the timer:

   ```bash
   sudo systemctl enable --now photoframe-sync.timer
   ```

3. Synced photos land in `/var/lib/photoframe/photos/cloud`.

---

## Optional: remote administration

SSH key auth over LAN is the recommended baseline. For remote access outside your network:

- **Tailscale** — private mesh with stable hostnames across NAT
- **Raspberry Pi Connect** — browser-based remote access

Keep direct LAN SSH as a fallback even when using a mesh VPN, so you can recover if the remote agent goes down.

---

## Troubleshooting first install

| Symptom | Likely cause | Quick fix |
| --- | --- | --- |
| Build killed with `signal: 9` | Out of memory | `CARGO_BUILD_JOBS=2 ./setup/install-all.sh` |
| `VERSION_CODENAME` is not `trixie` | Wrong OS build | Re-flash with Trixie 64-bit |
| `./setup/tools/verify.sh` fails | Service not running | Check `sudo systemctl status greetd` |
| Screen dark after greeting | Frame in sleep state | Send wake command (Step 5) |
| Permission denied writing photos | Group not picked up | Log out and SSH back in |

For anything else, see [docs/troubleshooting.md](troubleshooting.md).

---

## What's installed where

```
/opt/photoframe/       Read-only runtime: binaries, unit files, config templates
/var/lib/photoframe/   Writable state: photos, logs, hotspot artifacts
/etc/photoframe/       Active configuration (edit this with sudo)
```

Rerunning the installer updates `/opt/photoframe` without touching `/etc/photoframe/config.yaml` or the photo library.
