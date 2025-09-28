# Raspberry Pi Provisioning Instructions

These instructions describe how to prepare a Raspberry Pi for the rust-photo-frame project using the automation scripts in this directory.

## Create or identify SSH keys (do this before imaging)
Recent releases of Raspberry Pi Imager (v1.8 and newer) prompt for customization *after* you choose the OS and storage. Because you will need an SSH public key at that point, confirm that one is available before you begin flashing the card.

1. On macOS, check for existing keys and generate one if necessary:
   ```bash
   ls ~/.ssh/id_*.pub || ssh-keygen -t ed25519 -f ~/.ssh/photoframe -C "frame@photoframe.local"
   ```
2. If you create a new key specifically for this frame, note the private key path (e.g., `~/.ssh/photoframe`) and the public key path (e.g., `~/.ssh/photoframe.pub`).
3. Optional: add an entry to `~/.ssh/config` so you can connect with `ssh photoframe` later:
   ```
   Host photoframe
       HostName photoframe.local
       User frame
       IdentityFile ~/.ssh/photoframe
       IdentitiesOnly yes
   ```

## Headless setup with Raspberry Pi Imager (macOS)
This workflow prepares a Raspberry Pi OS (Bookworm, 64-bit) image that boots directly into a network-connected, SSH-ready system.

1. Download and install the latest [Raspberry Pi Imager](https://www.raspberrypi.com/software/) for macOS.
2. Insert the target microSD card into your Mac and launch Raspberry Pi Imager.
3. **Choose OS:** select *Raspberry Pi OS (64-bit)* (Bookworm).
4. **Choose Storage:** pick the microSD card.
5. Click **Next**. When prompted to apply OS customization, choose **Edit Settings**. The older gear icon has been replaced with this dialog in recent releases.
6. In **General** settings:
   - **Hostname:** `photoframe`
   - **Username / Password:** create a dedicated user (e.g., `frame`) with a strong password.
   - **Configure Wireless LAN:** enter your Wi-Fi SSID, passphrase, and country code (for example, `US`).
   - **Locale / Timezone / Keyboard:** adjust to your environment.
7. Switch to the **Services** tab and enable:
   - **Enable SSH** → choose **Allow public-key authentication only** and paste the contents of your SSH public key (from `~/.ssh/photoframe.pub` or an existing key).
8. (Optional) Review the **Options** tab for any additional tweaks (e.g., persistent settings or telemetry choices).
9. Click **Save**, then **Yes** to apply the settings, and finally **Write** the image. Wait for verification to finish, then eject the card safely.

## First boot and SSH access
1. Insert the prepared microSD card into the Raspberry Pi, connect the display, and power it on.
2. Give the device a minute to join Wi-Fi. From your Mac, connect via SSH using the host alias configured earlier:
   ```bash
   ssh photoframe
   ```
   If you did not preload the key, log in with the username/password you configured, then add the key to `~/.ssh/authorized_keys` on the Pi.
3. (Recommended) Update the package cache and upgrade packages before running the automation:
   ```bash
   sudo apt update && sudo apt upgrade -y
   ```

## Clone the repository on the Raspberry Pi
1. Ensure Git is installed:
   ```bash
   sudo apt install -y git
   ```
2. Choose a workspace (e.g., `~/projects`) and clone this repository:
   ```bash
   mkdir -p ~/projects
   cd ~/projects
   git clone https://github.com/your-org/rust-photo-frame.git
   cd rust-photo-frame
   ```
3. Check out the branch or tag that corresponds to the release you plan to deploy.

## Choose the application user for setup modules
Most setup modules, including the Wi-Fi watcher build, try to run developer tooling (like `cargo`) as the non-root account that
invokes `sudo`. The scripts automatically prefer, in order:

1. A user supplied via `FRAME_USER=<name>` when invoking the script.
2. The user recorded in `SUDO_USER` (i.e., the account that ran `sudo`).
3. The owner of the repository checkout on disk.
4. A `frame` account, if one exists.
5. `root` as a last resort.

This means you can simply clone the repository as your preferred account and run the wrapper script without worrying about the
underlying username; it will elevate with `sudo` as needed and export the right account for every module. If you need to
override the choice explicitly (for example, when staging a build for another user account), run the module with:

```bash
FRAME_USER=photoframe sudo ./setup/modules/30-wifi-watcher.sh
```

The script will warn and fall back to an available account if the requested name cannot be found.

## Initiate the automated setup
1. Review the scripts in `setup/setup-modules/` to understand each configuration stage. Scripts are prefixed with two-digit numbers to show execution order.
2. Ensure the scripts are executable:
   ```bash
   chmod +x setup/system-setup.sh setup/setup-modules/*.sh
   ```
3. Run the system configuration wrapper. When launched as a regular user it re-executes itself with `sudo`, preserving the
   invoking account. When run directly as `root`, it prompts for the target username before continuing. The wrapper executes
   each module in ascending numeric order:
   ```bash
   ./setup/system-setup.sh
   ```
4. Watch the console output for prompts or errors. You can rerun an individual module directly (e.g., `sudo ./setup/setup-modules/00-update-os.sh`).
5. After the base setup completes, continue following the roadmap to implement kiosk mode, background synchronization, button monitoring, Tailscale, Wi-Fi recovery mode, and the configuration web UI.
