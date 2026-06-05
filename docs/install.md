# Install

Blank microSD → running slideshow. Follow the steps in order.

**Time:** ~20 min active + 20–40 min unattended build on a Pi 5.

**Command context:** run as your operator account (e.g. `frame`) over SSH; use `sudo` where shown.

> **sudo requires your password.** Raspberry Pi OS no longer configures passwordless sudo. Every `sudo` command in this guide will prompt for the password you set during imaging. Enter it once — your session caches it for ~15 minutes, so subsequent `sudo` calls won't re-prompt.

---

## Two accounts you'll meet

- **Operator account** (e.g. `frame`) — your SSH login. You use this for maintenance, `sudo`, and `scp`.
- **Service account** (`kiosk`) — runs greetd, Sway, the photo app, the Wi-Fi manager, and `buttond`. You'll need `sudo -u kiosk` for anything that touches the Wayland session (`powerctl`, `socat` to the control socket).

---

## Prepare an SSH key

Raspberry Pi Imager wants a public key during setup:

```bash
ls ~/.ssh/id_*.pub
# or generate one:
ssh-keygen -t ed25519 -f ~/.ssh/photoframe -C "frame@photoframe.local"
```

Optional shortcut in `~/.ssh/config`:

```
Host photoframe
    HostName photoframe.local
    User frame
    IdentityFile ~/.ssh/photoframe
    IdentitiesOnly yes
```

---

## Step 1 — Flash the SD card

1. Install [Raspberry Pi Imager](https://www.raspberrypi.com/software/).
2. Insert your microSD; launch the imager.
3. **Choose Device:** Raspberry Pi 5
4. **Choose OS:** Raspberry Pi OS (64-bit) — the **Trixie** (Debian 13) build.
5. **Choose Storage:** your microSD.
6. **Edit Settings**:
   - Hostname: `photoframe`
   - Username/password: create a dedicated user (e.g. `frame`) with a strong password — **this password is required even with key-only SSH** because `sudo` authenticates against it
   - Configure Wireless LAN: SSID, passphrase, country code
   - Set locale, timezone, keyboard
7. **Services**: enable SSH → **Allow public-key authentication only** → paste your public key.
8. **Save → Yes → Write** and wait for verification.

---

## Step 2 — First boot and OS check

1. Insert the SD card, connect the display, power on.
2. Wait ~1 minute for Wi-Fi association, then SSH:

   ```bash
   ssh frame@photoframe.local
   ```

3. Confirm the OS — the setup scripts require Trixie:

   ```bash
   grep VERSION_CODENAME /etc/os-release    # must say trixie
   ```

4. Update packages:

   ```bash
   sudo apt update && sudo apt upgrade -y
   ```

---

## Step 3 — Clone and install

```bash
sudo apt install -y git
git clone https://github.com/vincentl/rust-photo-frame.git photoframe
cd photoframe
./setup/install-all.sh
```

The script uses `sudo` internally for system-level steps (your credential is still cached from Step 2 — no extra prompt). It:
- Installs system packages (graphics stack, NetworkManager, Sway, greetd, build tools) and a system-wide Rust toolchain.
- Builds the four crates (`photoframe`, `buttond`, `wifi-manager`, `config-model`).
- Provisions the `kiosk` user, polkit rules, runtime directories, zram swap, and Pi 5 boot tweaks.
- Installs binaries, unit files, and a starter config to `/opt/photoframe`.
- Wires up greetd on tty1 and starts the kiosk session, Wi-Fi watcher, and `buttond`.

> **Build killed with `signal: 9`?** The Rust build ran out of memory. Cap parallelism: `CARGO_BUILD_JOBS=2 ./setup/install-all.sh`.

When the script returns, **log out and SSH back in** so your shell picks up the `kiosk` group membership added during setup. Then verify:

```bash
exit
ssh frame@photoframe.local
cd photoframe
./setup/tools/verify.sh
```

You should see all checks green. Warnings appear yellow; errors red.

### What got installed where

```
/opt/photoframe/       Read-only runtime: binaries, unit files, config templates
/var/lib/photoframe/   Writable state: photos, hotspot artifacts, runtime files
/etc/photoframe/       Active configuration (edit this with sudo)
```

Re-running the installer updates `/opt/photoframe` without touching `/etc/photoframe/config.yaml` or your photo library.

---

## Step 4 — Add your photos

Photos live under `/var/lib/photoframe/photos`, scanned recursively. Use the `local/` subdirectory for manual imports (the `cloud/` subdirectory is reserved for the optional sync service).

From your laptop, use `rsync` (not `scp` — the sftp subsystem doesn't load supplementary groups, so `scp` writes silently fail):

```bash
rsync -a -e "ssh -i ~/.ssh/photoframe" /path/to/photos/ frame@photoframe.local:/var/lib/photoframe/photos/local/
```

Or locally on the Pi:

```bash
cp /path/to/photos/* /var/lib/photoframe/photos/local/
```

Confirm:

```bash
find /var/lib/photoframe/photos -type f | wc -l
```

**Supported formats:** JPEG, PNG. Other formats are silently skipped.

**Custom mat backgrounds:** drop JPEG/PNG files into `/var/lib/photoframe/backgrounds/`, then uncomment the `fixed-image` block in `/etc/photoframe/config.yaml`. See [Configure](configure.md).

---

## Step 5 — First boot: what to expect

When everything is working, the display goes through these states in order:

1. **Blank** (a few seconds) — greetd is launching Sway and handing off the compositor.
2. **Greeting card** ("Warming up your photo memories…", a few seconds) — the app is scanning the library and pre-decoding the first images.
3. **Display goes dark — this is normal.** After the greeting the frame enters sleep state. The process is running, the GPU is idle, and it's waiting for either a wake command or a scheduled wake window.
4. **Photos cycle** — once awake, the slideshow begins.

### Wake the frame

Send a JSON command over the Unix control socket:

```bash
echo '{"command":"set-state","state":"awake"}' \
  | sudo -u kiosk socat - UNIX-CONNECT:/run/photoframe/control.sock
```

Photos should begin cycling within a few seconds.

### Confirm it's healthy

```bash
sudo systemctl status greetd photoframe-wifi-manager buttond
```

All three should report `active (running)`. Live photo logs:

```bash
sudo journalctl -t photoframe -f
```

Healthy output:

```
photoframe: scanning /var/lib/photoframe/photos
photoframe: found 42 photos
photoframe: loaded sunset.jpg (3840x2160)
photoframe: displaying photo 3 of 42
photoframe: transition fade 450ms
```

If the greeting never appeared at all, see [Operate › Troubleshooting](operate.md#black-screen-from-the-start--no-greeting-ever-appears).

### Always-on operation

Leave `awake-schedule` commented out in `/etc/photoframe/config.yaml` (the default) and the frame stays awake until you explicitly send a sleep command. To run on a schedule, see [Configure › Wake/sleep control](configure.md#wakesleep-control).

---

## Step 6 (optional) — Cloud photo sync

The installer ships a disabled `photoframe-sync` service that pulls photos from a cloud provider or NAS into `/var/lib/photoframe/photos/cloud/` on a timer. See [Advanced › Cloud sync](advanced.md#cloud-sync) for the full guide.

---

## Step 7 (optional) — Remote administration

SSH key auth over LAN is the recommended baseline. For access outside your network:

- **Tailscale** — private mesh with stable hostnames across NAT
- **Raspberry Pi Connect** — browser-based remote access

Keep direct LAN SSH as a fallback so you can recover if the remote agent goes down.

---

## First-install troubleshooting

| Symptom | Likely cause | Quick fix |
| --- | --- | --- |
| Build killed with `signal: 9` | OOM during Rust build | `CARGO_BUILD_JOBS=2 ./setup/install-all.sh` |
| `VERSION_CODENAME` is not `trixie` | Wrong OS image | Re-flash with Raspberry Pi OS (64-bit) Trixie |
| `verify.sh` reports failures | Service didn't start | `sudo systemctl status greetd` and check `journalctl -u greetd -b` |
| Screen dark after greeting | Frame in sleep state — normal | Send the wake command from Step 5 |
| Permission denied writing photos | New `kiosk` group not picked up by SSH session | `exit`, then SSH back in |

For anything else, see [Operate › Troubleshooting](operate.md#troubleshooting).
