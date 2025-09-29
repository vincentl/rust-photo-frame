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

Both setup stages should be launched as the deployment user. They call `sudo` internally for the few operations that need elevated privileges (apt packages, `/boot` updates, and copying into `/opt/photo-frame`). The stages are safe to re-run; unchanged modules will detect that no work is required.

1. The script `./setup/system/run.sh` provisions operating-system dependencies, enables the 4K HDMI boot profile, and installs a user-scoped Rust toolchain.

   ```bash
   ./setup/system/run.sh
   ```

1. Reboot the pi to enable the boot changes.

   ```bash
   sudo reboot now
   ```

   You will need to ssh back to the frame once it reboots to continue installation.

1. The script `./setup/app/run.sh` builds the photo frame application, stages the release artifacts, installs them into `/opt/photo-frame`, and enables the `photo-frame.service` systemd unit.

   ```bash
   ./setup/app/run.sh
   ```

Use the following environment variables to customise an installation:

| Variable | Default | Notes |
|----------|---------|-------|
| `INSTALL_ROOT` | `/opt/photo-frame` | Target installation prefix. |
| `SERVICE_USER` | invoking user | The systemd account that owns `/opt/photo-frame/var`. Must already exist. |
| `SERVICE_GROUP` | invoking user's primary group | Group that owns `/opt/photo-frame/var` alongside `SERVICE_USER`. |
| `CARGO_PROFILE` | `release` | Cargo profile passed to `cargo build`. |
| `DRY_RUN` | unset | Set to `1` to see the actions that would be taken without modifying the system. |

After both stages finish, the installer enables `wifi-watcher.service`, stages `wifi-setter.service`,
and copies the hotspot template. The provisioning workflow—including how to use the temporary
hotspot and web UI—is documented in detail in [Wi-Fi Provisioning and Hotspot Workflow](wifi-provisioning.md).
