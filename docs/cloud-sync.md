# Cloud Sync

The photo frame can automatically keep a `cloud/` directory in your photo library in sync with any cloud storage service. Photos in `cloud/` are treated exactly like photos in `local/` — they appear in the rotation alongside anything you've copied directly to the Pi.

This is optional. If you only add photos manually, you can skip this entirely.

---

## How it works

- The sync service downloads photos from your cloud storage into `/var/lib/photoframe/photos/cloud/`.
- Your manually-managed photos in `/var/lib/photoframe/photos/local/` are never touched.
- A systemd timer triggers the sync on a schedule (hourly by default).
- The timer is **disabled by default** — it only activates after you configure a source.

**Sync is safe to run while the frame is displaying photos.** The service syncs to a staging directory first, then promotes the result to `cloud/` in a single step. Files that haven't changed are shared between staging and `cloud/` as hard links, so the sync uses no extra disk space.

---

## Supported providers

The frame uses [rclone](https://rclone.org) for cloud sync. rclone supports over 40 providers including:

| Provider | Remote type |
|----------|------------|
| Google Drive | `drive` |
| Dropbox | `dropbox` |
| Amazon S3 (and compatible) | `s3` |
| Microsoft OneDrive | `onedrive` |
| Backblaze B2 | `b2` |
| iCloud Drive | `iclouddrive` |
| SFTP / NAS | `sftp` |
| WebDAV | `webdav` |

For a full list: `rclone help backends`

Alternatively, if you have a NAS on your local network, you can use rsync directly — see [Using rsync instead](#using-rsync-instead).

---

## Setup

### Step 1: Configure an rclone remote

Run the interactive setup wizard on the Pi:

```bash
rclone config
```

The wizard walks you through selecting your provider and authenticating. For providers that use OAuth (Google Drive, Dropbox, OneDrive), rclone will print a URL. Open that URL in a browser on any device, authorize the app, and paste the confirmation code back into the terminal.

After finishing, verify it worked:

```bash
rclone listremotes
# Example output: gdrive:
rclone ls gdrive:My\ Photos/Frame
```

Use whatever path in your cloud storage holds the photos you want to display.

> **Tip:** rclone stores its config at `~/.config/rclone/rclone.conf`. The config runs as your operator user (`frame`), but the sync service runs as `kiosk`. Copy the config after setup:
> ```bash
> sudo mkdir -p /home/kiosk/.config/rclone
> sudo cp ~/.config/rclone/rclone.conf /home/kiosk/.config/rclone/rclone.conf
> sudo chown -R kiosk:kiosk /home/kiosk/.config/rclone
> ```

### Step 2: Configure the sync source

Edit the sync config file:

```bash
sudo nano /etc/photoframe/sync.env
```

Uncomment and set `RCLONE_REMOTE` to your remote name and path:

```bash
RCLONE_REMOTE=gdrive:My Photos/Frame
```

The format is `<remote-name>:<path>`. Use the exact remote name shown by `rclone listremotes`, and the path to the folder containing your photos.

Save the file (`Ctrl+O`, `Enter`, `Ctrl+X` in nano).

### Step 3: Test the sync

Trigger a sync manually and watch the output:

```bash
sudo systemctl start photoframe-sync.service
sudo journalctl -u photoframe-sync.service -f
```

You should see rclone downloading files. When it finishes, check that photos landed in the cloud directory:

```bash
ls /var/lib/photoframe/photos/cloud/
```

If photos appear here, the sync is working. The frame will start showing them in the next rotation cycle.

### Step 4: Enable automatic sync

Once you've confirmed the sync works, enable the timer:

```bash
sudo systemctl enable --now photoframe-sync.timer
```

Check when the next run is scheduled:

```bash
systemctl list-timers photoframe-sync.timer
```

---

## Changing the sync schedule

The default schedule is hourly. To change it, create a drop-in override:

```bash
sudo systemctl edit photoframe-sync.timer
```

This opens an editor. Add the following (replacing `OnCalendar=` with your schedule):

```ini
[Timer]
OnCalendar=
OnCalendar=*-*-* 02:00:00
```

The empty `OnCalendar=` clears the default before setting the new one. Save and close — systemd applies the change immediately.

Common schedule values:

| Schedule | `OnCalendar=` |
|----------|--------------|
| Every hour (default) | `hourly` |
| Every 15 minutes | `*:0/15` |
| Once a day at 2 AM | `*-*-* 02:00:00` |
| Twice a day | `*-*-* 06,18:00:00` |

---

## Monitoring

**Check the last sync result:**

```bash
sudo systemctl status photoframe-sync.service
```

**Watch a sync in progress:**

```bash
sudo journalctl -u photoframe-sync.service -f
```

**Check when the next sync runs:**

```bash
systemctl list-timers photoframe-sync.timer
```

**Count synced photos:**

```bash
find /var/lib/photoframe/photos/cloud -type f | wc -l
```

---

## Using rsync instead

If your photos are on a NAS or another machine on your local network, you can use rsync instead of rclone:

```bash
sudo nano /etc/photoframe/sync.env
```

Set:

```bash
SYNC_TOOL=rsync
RSYNC_SOURCE=user@nas.local:/photos/frame/
# RSYNC_FLAGS=-av --delete  # uncomment to override defaults
```

The Pi must be able to SSH to the source without a password. Set up an SSH key for the `kiosk` user:

```bash
sudo -u kiosk ssh-keygen -t ed25519 -N "" -f /home/kiosk/.ssh/id_ed25519
sudo -u kiosk ssh-copy-id user@nas.local
```

---

## Troubleshooting

| Symptom | Likely cause | Fix |
|---------|-------------|-----|
| `RCLONE_REMOTE must be set` in logs | `sync.env` not configured | Edit `/etc/photoframe/sync.env`, set `RCLONE_REMOTE=` |
| `Failed to create file system for ... not found` | Remote name wrong | Run `rclone listremotes`; check spelling in `sync.env` |
| `directory not found` | Path in remote doesn't exist | Run `rclone lsd gdrive:` to list top-level folders |
| Auth errors / token expired | OAuth token needs refresh | Run `rclone config reconnect gdrive:` as operator user, then copy config to kiosk |
| Photos don't appear after sync | Wrong file format | Only JPEG and PNG are supported; run `rclone ls` to verify file types |
| `permission denied` writing to `cloud/` | Permissions issue | Run `sudo chown -R kiosk:kiosk /var/lib/photoframe/photos/cloud` |
| Timer stays disabled after configuring sync.env | Timer not yet enabled | Run `sudo systemctl enable --now photoframe-sync.timer` |

If you configured rclone as your operator user but the service runs as `kiosk`, the most common issue is the rclone config not being present for the `kiosk` user. See the tip in [Step 1](#step-1-configure-an-rclone-remote).
