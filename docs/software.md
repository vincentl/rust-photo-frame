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

### Choose the application user for setup modules

Most setup modules, including the Wi-Fi watcher build, try to run developer tooling (like `cargo`) as the non-root account that invokes `sudo`. The scripts automatically prefer, in order:

1. A user supplied via `FRAME_USER=<name>` when invoking the script.
1. The user recorded in `SUDO_USER` (i.e., the account that ran `sudo`).
1. The owner of the repository checkout on disk.
1. A `frame` account, if one exists.
1. `root` as a last resort.

This means you can simply clone the repository as your preferred account and run the wrapper script without worrying about the underlying username; it will elevate with `sudo` as needed and export the right account for every module. If you need to override the choice explicitly (for example, when staging a build for another user account), run the module with:

```bash
FRAME_USER=photoframe sudo ./setup/modules/30-wifi-watcher.sh
```

The script will warn and fall back to an available account if the requested name cannot be found.

## Initiate the automated setup

1. The script `./setup/system-setup.sh` calls scripts in `./setup/setup-modules` to configure the boot configuration to support a 4k monitor and install rust.

   ```bash
   ./setup/system-setup.sh
   ```

1. Reboot the pi to enable the boot changes.

   ```bash
   sudo reboot now
   ```

   You will need to ssh back to the frame once it reboots to continue installation.

1. The script `./setup/setup.sh` calls scripts in `./setup/modules` to build the photo frame application, configure cloud syncing, and install a wifi watcher to simplify moving the frame to a new wifi network.
