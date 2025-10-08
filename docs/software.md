# Raspberry Pi Provisioning and Installation

These instructions cover the full workflow for preparing a Raspberry Pi to run the Photo Frame project, from creating the SD card image to verifying the kiosk session.

## Before you image: prepare SSH keys

Recent releases of Raspberry Pi Imager (v1.8 and newer) prompt for customization _after_ you choose the OS and storage. Because you will need an SSH public key at that point, confirm that one is available before you begin flashing the card. Commands in this section are examples that should work on macOS or Linux. Consult documentation about your computer's OS for details about how to carry out these steps. See `ssh-keygen` documentation to choose appropriate parameters if you generate a new ssh key.

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
1. Click **Next**. When prompted to apply OS customization, choose **Edit Settings**. The older gear icon has been replaced with this dialog in recent releases.
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

   The output must include `VERSION_CODENAME=trixie`. Earlier Debian releases are no longer supported by the setup scripts.

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
   git clone https://github.com/vincentl/rust-photo-frame.git
   cd rust-photo-frame
   ```

1. **Optional:** Check out a specific branch or tag.

## Run the automated setup

Run the automation in two passes. Each script is idempotent, so you can safely re-run it if the connection drops or you need to retry after a reboot.

### 1. Bootstrap the operating system

```bash
sudo ./setup/bootstrap/run.sh
```

   This pipeline installs the apt dependencies, configures zram swap, and installs a system-wide Rust toolchain under `/usr/local/cargo`. It also provisions the kiosk user, greetd configuration, and supporting systemd units. Run it before building so toolchains and packages are ready; re-run it after the application install so the kiosk services start once the binaries exist in `/opt/photo-frame`.

### 2. Build and stage the application

```bash
./setup/app/run.sh
```

   Run this command as the unprivileged operator account. It compiles the photo frame, stages the release artifacts, and installs them into `/opt/photo-frame`. The stage verifies the kiosk service account exists and will prompt for sudo to create it (along with its primary group) when missing. The closing postcheck confirms binaries and templates are in place and will warn if the runtime config at `/var/lib/photo-frame/config/config.yaml` is missing; re-running the command recreates it from the staged template.

> **Filesystem roles**
>
> - `/opt/photo-frame` is treated as read-only at runtime. It contains the versioned binaries, systemd unit templates, and stock configuration files delivered by the setup scripts.
> - `/var/lib/photo-frame` is writable and owned by the service account. Configuration overrides, logs, hotspot artifacts, and synchronized media all live here. Systemd services expect to mutate this tree, so backups and troubleshooting should start in `/var`.
>
> Keeping code and mutable state separate allows updates to replace the staged artifacts in `/opt` without disturbing operator-managed data in `/var/lib/photo-frame`.

   The postcheck defers systemd validation until the kiosk environment is provisioned. Expect warnings about `greetd.service` and related helper units until you rerun the bootstrap pipeline after the application install.

Use the following environment variables to customize an installation:

| Variable        | Default            | Notes |
| --------------- | ------------------ | ----- |
| `INSTALL_ROOT`  | `/opt/photo-frame` | Target installation prefix. |
| `SERVICE_USER`  | `kiosk`            | The systemd account that owns `/var/lib/photo-frame`. The app stage creates it on demand before installing artifacts. |
| `SERVICE_GROUP` | `kiosk` (or the primary group for `SERVICE_USER`) | Group that owns `/var/lib/photo-frame` alongside `SERVICE_USER`. |
| `CARGO_PROFILE` | `release`          | Cargo profile passed to `cargo build`. |

## Validate the kiosk stack

Use the following checks to confirm the kiosk environment is live:

```bash
systemctl status greetd
systemctl status display-manager
journalctl -u greetd -b
```

`systemctl status` should report `active (running)` and show `cage -s -- /usr/local/bin/photoframe-session` in the command line. The journal should contain the photo frame application logs for the current boot. Once these checks pass, reboot the device to land directly in the fullscreen photo frame experience.

## Kiosk session reference

When both setup stages complete successfully the Raspberry Pi is ready to boot directly into a kiosk session:

- `/etc/greetd/config.toml` binds greetd to virtual terminal 1 and runs `cage -s -- /usr/local/bin/photoframe-session` as the `kiosk` user. The wrapper applies the HDMI 4K60 layout via `wlr-randr` before launching the photo frame binary through `systemd-cat` so logs land in the journal. greetd creates the login session so `XDG_RUNTIME_DIR` points at `/run/user/<uid>` while `/var/lib/photo-frame` remains writable by the kiosk account.
- Device access comes from the `kiosk` user belonging to the `render`, `video`, and `input` groups. The setup stage wires this up so Vulkan/GL stacks can open `/dev/dri/renderD128` without any extra udev hacks.
- The kiosk stack relies on `greetd` + `cage`; no display-manager compatibility targets or tty autologin services are installed.

For smoke testing, temporarily modify `/etc/greetd/config.toml` to run `kmscube` instead of the photo frame binary. A spinning cube on HDMI verifies DRM, GBM, and input permissions before deploying the full app.

To pause the slideshow for maintenance, SSH into the Pi and run `sudo systemctl stop greetd`. Start it again with `sudo systemctl start greetd` when you are ready to resume playback.
