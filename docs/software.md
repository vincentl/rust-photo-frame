# Raspberry Pi Provisioning Instructions

These instructions describe how to prepare a Raspberry Pi the Photo Frame project.

## Create or identify SSH keys (do this before imaging)

Recent releases of Raspberry Pi Imager (v1.8 and newer) prompt for customization _after_ you choose the OS and storage. Because you will need an SSH public key at that point, confirm that one is available before you begin flashing the card. Commands in this section are examples that should work on macOS or Linux. Consult documentation about your computer's OS for details about how to carryout these steps. See `ssh-keygen` documentation to choose appropriate parameters if you generate a new ssh key.

1. Check for existing public ssky key.

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

## Setup with Raspberry Pi Imager

This workflow prepares a Raspberry Pi OS (Bookworm, 64-bit) image that boots directly into a network-connected, SSH-ready system.

1. Download and install the latest [Raspberry Pi Imager](https://www.raspberrypi.com/software/).
1. Insert the target microSD card into your computer and launch Raspberry Pi Imager.
1. **Choose Device:** Raspberry Pi 5
1. **Choose OS:** select _Raspberry Pi OS (64-bit)_ (Bookworm).
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

## First boot and SSH access

1. Insert the prepared microSD card into the Raspberry Pi, connect the display, and power it on.
1. Give the device a minute to join Wi-Fi. From your Mac, connect via SSH using the host alias configured earlier:

   ```bash
   ssh frame@photoframe.local
   ```

   If you did not preload the key, log in with the username/password you configured, then add the key to `~/.ssh/authorized_keys` on the Pi.

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

Run the automation in three stages. Each script is idempotent, so you can safely re-run it if the connection drops or you need to retry after a reboot.

1. Provision OS packages and the Rust toolchain:

   ```bash
   sudo ./setup/packages/run.sh
   ```

   This script installs the apt dependencies and a system-wide Rust toolchain under `/usr/local/cargo`. Log out and back in (or reconnect your SSH session) afterwards so your shell picks up the updated `PATH`.

1. Build and install the application:

   ```bash
   ./setup/app/run.sh
   ```

   Run this command as the unprivileged operator account. It compiles the photo frame, stages the release artifacts, and installs them into `/opt/photo-frame`.

1. Configure system services and permissions:

   ```bash
   sudo ./setup/system/run.sh
   ```

   When the script finishes, reconnect your SSH session so new group memberships take effect.

Use the following environment variables to customize an installation:

| Variable        | Default            | Notes |
| --------------- | ------------------ | ----- |
| `INSTALL_ROOT`  | `/opt/photo-frame` | Target installation prefix. |
| `SERVICE_USER`  | `kiosk`            | The systemd account that owns `/var/lib/photo-frame`. The system stage creates it when missing. |
| `SERVICE_GROUP` | `kiosk` (or the primary group for `SERVICE_USER`) | Group that owns `/var/lib/photo-frame` alongside `SERVICE_USER`. |
| `CARGO_PROFILE` | `release`          | Cargo profile passed to `cargo build`. |

### Kiosk session configuration

When both setup stages complete successfully the Raspberry Pi is ready to boot directly into a kiosk session:

- The templated systemd unit `cage@tty1.service` binds to `/dev/tty1`, logs in the `kiosk` user through PAM, and `exec`s the Wayland compositor as `cage -- /opt/photo-frame/bin/rust-photo-frame --config /opt/photo-frame/etc/config.yaml`. PAM/logind create a real session so `XDG_RUNTIME_DIR` points at `/run/user/<uid>` and DRM permissions flow automatically while the writable config lives under `/var/lib/photo-frame/config`.
- Device access comes from the `kiosk` user belonging to the `render`, `video`, and `input` groups. The system stage wires this up, so Vulkan/GL stacks can open `/dev/dri/renderD128` without any extra udev hacks.
- `seatd` remains optional on Bookworm but harmless to have installed; the compositor happily runs without manual socket management when launched via logind. We force the `LIBSEAT_BACKEND=logind` environment for `cage@tty1.service` so the compositor always binds through logind even when the seatd daemon happens to be present.
- Disable any other display manager or compositor on the target TTY so Cage can claim DRM master and input devices without contention.

For smoke testing, temporarily adjust the unit to run `kmscube` instead of the photo frame binary. A spinning cube on HDMI verifies DRM, GBM, and input permissions before deploying the full app.

To pause the slideshow for maintenance, SSH into the Pi and run `sudo systemctl stop cage@tty1.service`. The kiosk remains down until you start it again with `sudo systemctl start cage@tty1.service`.
