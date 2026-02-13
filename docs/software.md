# Raspberry Pi Provisioning and Installation

These instructions cover the full workflow for preparing a Raspberry Pi to run the Photo Frame project, from creating the SD card image to verifying the kiosk session and showing your first slideshow.

Audience: operator/installer. For setup module internals see `setup/README.md`.

Command context: run commands as your operator account over SSH unless noted otherwise. Use `sudo` for system changes and service inspection.

## Quick install checklist

Use this if you want a first slideshow quickly:

1. Flash Raspberry Pi OS (Trixie, 64-bit) with SSH + Wi-Fi enabled.
2. SSH into the Pi as your operator account (example `frame`).
3. Verify OS codename:
   - `grep VERSION_CODENAME /etc/os-release` (must be `trixie`)
4. Clone and enter repo:
   - `git clone https://github.com/vincentl/rust-photo-frame.git photoframe && cd photoframe`
5. Run installer:
   - `./setup/install-all.sh`
6. Verify services:
   - `./setup/tools/verify.sh`
   - No reboot should be required; the installer restarts kiosk services and validates control socket readiness.
7. Add photos to the default library path:
   - Copy images into `/var/lib/photoframe/photos/local` (manual imports) or `/var/lib/photoframe/photos/cloud` (sync-managed imports)
   - If those directories are missing, create them: `sudo install -d -m 2775 -o kiosk -g kiosk /var/lib/photoframe/photos/local /var/lib/photoframe/photos/cloud`
   - If writes to `/var/lib/photoframe/photos` fail with permissions, log out and SSH back in once to pick up new group membership.
8. Wake the frame if it is sleeping:
   - `echo '{"command":"set-state","state":"awake"}' | sudo -u kiosk socat - UNIX-CONNECT:/run/photoframe/control.sock`
9. (Recommended) Run Wi-Fi recovery acceptance test:
   - `make -f tests/Makefile wifi-recovery`

Expected outcome: the frame boots into the kiosk session, accepts a wake command, and begins cycling through photos from `/var/lib/photoframe/photos`.

## Before you image: prepare SSH keys

Current versions of Raspberry Pi Imager (v1.8 and newer) prompt for customization _after_ you choose the OS and storage. Because you will need an SSH public key at that point, confirm that one is available before you begin flashing the card. Commands in this section are examples that should work on macOS or Linux. Consult documentation about your computer's OS for details about how to carry out these steps. See `ssh-keygen` documentation to choose appropriate parameters if you generate a new ssh key.

1. Check for an existing public SSH key.

   ```bash
   ls ~/.ssh/id_*.pub
   ```

   If you see `no matches found` or `No such file or directory`, then you must generate an ssh key. If there is an existing public ssh key, decide if you want to use that key or create a new key just for the photo frame.

1. **To generate a new ssh key**, use `ssh-keygen`. This example creates a key with the basename `photoframe` to distinguish the key.

   ```bash
   ssh-keygen -t ed25519 -f ~/.ssh/photoframe -C "frame@photoframe.local"
   ```

1. Note the key paths. If you create a new key specifically for this frame with the above command, the private key path is `~/.ssh/photoframe` and the public key path is `~/.ssh/photoframe.pub`.
1. **Optional:** add an entry to `~/.ssh/config` so you can connect with `ssh photoframe` later:

   ```config
   Host photoframe
       HostName photoframe.local
       User frame
       IdentityFile ~/.ssh/photoframe
       IdentitiesOnly yes
   ```

## Flash Raspberry Pi OS with Raspberry Pi Imager

This workflow prepares a Raspberry Pi OS (Trixie, 64-bit) image that boots directly into a network-connected, SSH-ready system.

1. Download and install the latest [Raspberry Pi Imager](https://www.raspberrypi.com/software/).
1. Insert the target microSD card into your computer and launch Raspberry Pi Imager.
1. **Choose Device:** Raspberry Pi 5
1. **Choose OS:** select _Raspberry Pi OS (64-bit)_ (Trixie).
1. **Choose Storage:** pick the microSD card.
1. Click **Next**. When prompted to apply OS customization, choose **Edit Settings**.
1. In **General** settings:
   - **Hostname:** `photoframe`
   - **Username / Password:** create a dedicated user (e.g., `frame`) with a strong password.
   - **Configure Wireless LAN:** enter your Wi-Fi SSID, passphrase, and country code (for example, `US`).
   - **Locale / Timezone / Keyboard:** adjust to your environment.
1. Switch to the **Services** tab and enable:
   - **Enable SSH** → choose **Allow public-key authentication only** and paste the contents of your SSH public key (from `~/.ssh/photoframe.pub` or an existing key).
1. (Optional) Review the **Options** tab for any additional tweaks (e.g., persistent settings or telemetry choices).
1. Click **Save**, then **Yes** to apply the settings, and finally **Write** the image. Wait for verification to finish, then eject the card safely.

## First boot and base OS checks

1. Insert the prepared microSD card into the Raspberry Pi, connect the display, and power it on.
1. Give the device a minute to join Wi-Fi. From your Mac, connect via SSH using the host alias configured earlier:

   ```bash
   ssh frame@photoframe.local
   ```

   If you did not preload the key, log in with the username/password you configured, then add the key to `~/.ssh/authorized_keys` on the Pi.

1. Confirm the operating system reports the expected codename before installing anything:

   ```bash
   grep VERSION_CODENAME /etc/os-release
   ```

   The output must include `VERSION_CODENAME=trixie`. The setup scripts target Debian 13 (Trixie).

1. (Recommended) Update the package cache and upgrade packages before running the automation. Although the setup scripts will also check for OS updates, the first update after a fresh install can be more involved and may include user prompts.

   ```bash
   sudo apt update && sudo apt upgrade -y
   ```

## Clone the repository on the Raspberry Pi

1. Ensure Git is installed:

   ```bash
   sudo apt install -y git
   ```

1. Clone this repository:

   ```bash
   git clone https://github.com/vincentl/rust-photo-frame.git photoframe
   cd photoframe
   ```

1. **Optional:** Check out a specific branch or tag.

## Run the automated setup

Run the automation in two steps. Each script is idempotent, so you can safely re-run it if the connection drops or you need to retry after a reboot. Alternatively, use the single-command installer below.

### 0. All-in-one installer (recommended)

```bash
./setup/install-all.sh
```

This script provisions the OS (with sudo) and immediately builds and deploys the application as your unprivileged user. It also installs/updates the systemd units and starts the kiosk stack.

### 1. Provision the operating system

```bash
sudo ./setup/system/install.sh
```

   This pipeline installs the apt dependencies, configures zram swap, and installs a system-wide Rust toolchain under `/usr/local/cargo`. It also sets `dtoverlay=vc4-kms-v3d-pi5,cma-512` in `/boot/firmware/config.txt` (or `/boot/config.txt`) so the GPU has sufficient contiguous memory for the renderer, and provisions the kiosk user, greetd configuration, and supporting systemd units that launch the photo frame at boot and reserve room for the Wi‑Fi overlay. Run it before building so toolchains and packages are ready.

### 2. Logout and Login

The provision step modifies shell configuration files. To pick up environment changes, log out and then SSH back in.

### 3. Deploy the application

```bash
./setup/application/deploy.sh
```

   Run this command as the unprivileged operator account. It compiles the photo frame, stages the runtime artifacts, and installs them into `/opt/photoframe`. The stage verifies the kiosk service account exists and will prompt for sudo to create it (along with its primary group) when missing. After install, it installs/updates the app’s systemd unit files and starts the kiosk services (greetd, seatd, wifi-manager, buttond) so the session comes up without re-running the system stage. The postcheck confirms binaries and templates are in place and will warn if the system config at `/etc/photoframe/config.yaml` is missing; re-running the command recreates it from the staged template.

## Validate the kiosk stack

Quick check:

```bash
./setup/tools/verify.sh
```

The verifier inspects installed binaries, configuration templates, var tree ownership, swap/zram, and the health of core services (greetd, seatd, wifi-manager, buttond, sync timer). It exits non‑zero on critical failures and prints hints for warnings.

Manual checks (optional):

```bash
sudo systemctl status greetd
sudo systemctl status display-manager
sudo journalctl -u greetd -b
sudo ls -l /run/photoframe/control.sock
```

`systemctl status` should report `active (running)` and show `/usr/local/bin/photoframe-session` in the command line. The journal should contain the photo frame application logs for the current boot, and the socket check should show `/run/photoframe/control.sock` as a Unix socket owned by `kiosk`.

## Load photos and show the first slideshow

After the kiosk stack is healthy, copy at least a few photos into the default library and wake the frame.

1. Ensure the local library directory exists with kiosk ownership:

   ```bash
   sudo install -d -m 2775 -o kiosk -g kiosk /var/lib/photoframe/photos/local
   sudo install -d -m 2775 -o kiosk -g kiosk /var/lib/photoframe/photos/cloud
   ```

2. Copy photos into the local library:

   ```bash
   sudo cp /path/to/photos/* /var/lib/photoframe/photos/local/
   sudo chown kiosk:kiosk /var/lib/photoframe/photos/local/*
   ```

   From another machine over SSH:

   ```bash
   scp /path/to/photos/* frame@photoframe.local:/var/lib/photoframe/photos/local/
   # For non-default SSH keys:
   scp -i ~/.ssh/photoframe /path/to/photos/* frame@photoframe.local:/var/lib/photoframe/photos/local/
   ```

3. If you cannot write into `/var/lib/photoframe/photos`, log out and SSH back in once to pick up group membership changes from install.

4. Confirm files are present:

   ```bash
   find /var/lib/photoframe/photos -type f | head
   ```

5. Wake the frame if it is currently asleep:

   ```bash
   echo '{"command":"set-state","state":"awake"}' \
     | sudo -u kiosk socat - UNIX-CONNECT:/run/photoframe/control.sock
   ```

6. Watch recent app logs:

   ```bash
   sudo journalctl -t photoframe -n 50 --no-pager
   ```

Expected outcome: within a few seconds the display leaves the greeting/sleep card and begins cycling through your photos.

## Fresh Install Wi-Fi Recovery Test

After completing a fresh microSD install and successful deployment, run this end-to-end validation to confirm recovery works before mounting the frame.

1. Confirm core services are healthy:

   ```bash
   ./setup/tools/verify.sh
   /opt/photoframe/bin/print-status.sh
   ```

2. Run the automated Wi-Fi recovery acceptance script:

   ```bash
   make -f tests/Makefile wifi-recovery
   ```

   Or directly:

   ```bash
   tests/run_wifi_recovery.sh
   ```

   Important: if your SSH session is currently routed over the same Wi-Fi interface under test (for example `wlan0`), fault injection will drop SSH. Run from a local console or an alternate management path (Ethernet, second NIC, or remote agent). The script now fails early in this case unless you explicitly set `ALLOW_WIFI_SSH_DROP=1`.

   Recommended for Wi-Fi-only remote sessions: run the test in `tmux` so the process survives SSH disconnects.
   The setup pipeline installs `tmux`; if it is missing on an older image, install it with `sudo apt install -y tmux`.

   ```bash
   tmux new -s wifi-recovery
   ALLOW_WIFI_SSH_DROP=1 make -f tests/Makefile wifi-recovery
   # After reconnecting over SSH:
   tmux attach -t wifi-recovery
   ```

   The script will:

   - deliberately inject a wrong Wi-Fi password (`developer/suspend-wifi.sh`),
   - wait for watcher transition into `RecoveryHotspotActive`,
   - prompt you to join the hotspot and submit real credentials,
   - verify transition back to online,
   - ensure hotspot is torn down and `wifi-request.json` is cleared.

3. Optional second scenario: temporary AP outage with unchanged credentials.

   - power off or disconnect the Wi-Fi access point for longer than `offline-grace-sec`,
   - wait for hotspot mode to appear,
   - restore the AP without submitting new credentials,
   - verify watcher returns to `Online` from reconnect probe.

4. Capture logs/artifacts if anything fails:

   ```bash
   tests/collect_logs.sh
   sudo journalctl -u photoframe-wifi-manager.service --since "10 min ago"
   ```

For the full validation matrix, use [`../developer/test-plan.md`](../developer/test-plan.md) (Phase 7). For day-2 incident triage, use [`sop.md`](sop.md).

### If `wifi-recovery` hangs at "Wait for hotspot transition"

1. Confirm the watcher interface matches your actual active Wi-Fi NIC:

   ```bash
   grep '^interface:' /opt/photoframe/etc/wifi-manager.yaml
   nmcli -t -f DEVICE,TYPE,STATE device status
   nmcli -g GENERAL.CONNECTION device show <interface-from-config>
   ```

2. Confirm the interface has an active infrastructure connection before fault injection (`GENERAL.CONNECTION` must not be `--` and must not be `pf-hotspot`).
3. Re-run with logs visible:

   ```bash
   make -f tests/Makefile wifi-recovery
   sudo journalctl -u photoframe-wifi-manager.service -f
   ```

4. If it still hangs, collect artifacts:

   ```bash
   tests/collect_logs.sh
   ```

## Kiosk session reference

When both setup stages complete successfully the Pi boots into a greetd-managed Sway session that launches the slideshow as `kiosk`. The full kiosk architecture reference (unit wiring, expected config, and compositor assumptions) lives in [`kiosk.md`](kiosk.md).

Operational commands (restart sequence, logs, and control socket checks) are maintained in [`sop.md`](sop.md).

## Optional Remote Administration

Remote administration is optional and intentionally not bundled into the installer.

Options:

1. **SSH-only baseline** (recommended starting point): key-only auth, strong operator password, and clear recovery process.
2. **Tailscale**: private mesh networking with stable device access over NAT.
3. **Raspberry Pi Connect**: browser-mediated remote access for managed fleets.

Recovery path recommendation: keep direct LAN SSH access as fallback even when using Tailscale or Raspberry Pi Connect so you can recover from remote-agent outages.

Document whichever approach you choose in your site runbook so future maintenance follows one consistent path.

## Optional Library Sync Service

The repository ships `photoframe-sync.service` + `photoframe-sync.timer`.
The timer is disabled by default and should remain disabled until a sync source is configured.

1. Create `/etc/photoframe/sync.env`:

   ```bash
   sudo install -d -m 755 /etc/photoframe
   sudo tee /etc/photoframe/sync.env >/dev/null <<'EOF'
   # Choose one source model:
   # RCLONE_REMOTE=remote-name:path
   # or
   # RSYNC_SOURCE=/path/to/source/
   EOF
   ```

2. Enable sync timer after configuration:

   ```bash
   sudo systemctl enable --now photoframe-sync.timer
   ```

3. Confirm timer/service status:

   ```bash
   sudo systemctl status photoframe-sync.timer --no-pager
   sudo systemctl status photoframe-sync.service --no-pager
   ```

4. Validate that synced files land in `/var/lib/photoframe/photos/cloud`.

## Rust toolchain behavior

- The system stage installs a minimal Rust toolchain under `/usr/local/cargo` with rustup state in `/usr/local/rustup`.
- The app deploy step prefers those system proxies and defaults `RUSTUP_HOME` to `/usr/local/rustup` so a default toolchain is available without per-user initialization.
- `CARGO_HOME` remains per-user for writable registries and caches.
- If you encounter `rustup could not choose a version of cargo to run` during build:
  - Ensure system stage has been run: `sudo ./setup/system/install.sh`
  - Or export: `RUSTUP_HOME=/usr/local/rustup`
  - Avoid overriding `RUSTUP_HOME` to `~/.rustup` unless you initialize a per-user toolchain (`rustup default stable`).

## Build memory and OOM mitigation

The installer auto-tunes Cargo job count on lower-memory Pis. If build workers are killed (`signal: 9`), cap jobs explicitly:

```bash
CARGO_BUILD_JOBS=2 ./setup/install-all.sh
# or
CARGO_BUILD_JOBS=2 ./setup/application/deploy.sh
```

Also verify swap after system provisioning:

```bash
swapon --show
```

Expect a `zram0` entry.

## Filesystem roles

- `/opt/photoframe`: read-only runtime artifacts (binaries, unit templates, stock config files).
- `/var/lib/photoframe`: writable runtime state (logs, hotspot artifacts, synced media, operational state files).
- `/etc/photoframe/config.yaml`: active system configuration.

This separation allows redeploys to refresh `/opt` without clobbering operator-managed runtime data under `/var/lib/photoframe`.

## Deployment postcheck notes

The postcheck defers some systemd validation until kiosk provisioning exists. If this is the first application deploy, warnings about `greetd.service` and helper units can appear until provisioning is complete.

## Installer environment variables

Use these to customize installation behavior:

| Variable        | Default            | Notes |
| --------------- | ------------------ | ----- |
| `INSTALL_ROOT`  | `/opt/photoframe` | Target installation prefix. |
| `SERVICE_USER`  | `kiosk`            | Service account that owns `/var/lib/photoframe`. |
| `SERVICE_GROUP` | `kiosk` (or primary group for `SERVICE_USER`) | Group ownership paired with `SERVICE_USER`. |
| `CARGO_PROFILE` | `release`          | Cargo profile passed to `cargo build`. |
