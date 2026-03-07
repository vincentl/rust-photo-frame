# Operations

Day-to-day tasks for maintaining a running frame. Run commands as your operator account over SSH; use `sudo` where shown.

For a one-page command cheat sheet, see [docs/quick-reference.md](quick-reference.md).
For incident triage and error scenarios, see [docs/troubleshooting.md](troubleshooting.md).

---

## Daily health check

These commands give you a quick picture of frame state. Run them first when something feels off:

```bash
./setup/tools/verify.sh
/opt/photoframe/bin/print-status.sh
sudo systemctl status greetd photoframe-wifi-manager buttond
```

Expected outcome: all three services show `active (running)`. `print-status.sh` exits without errors.

---

## Viewing logs

**Photo app logs** (rendering, library scans, transitions):

```bash
sudo journalctl -t photoframe -f
```

**Wi-Fi manager logs** (connectivity state, hotspot events):

```bash
sudo journalctl -u photoframe-wifi-manager.service -f
```

**Button daemon logs** (schedule evaluation, wake/sleep transitions, power commands):

```bash
sudo journalctl -u buttond.service -f
```

**Browsing logs from the last boot:**

```bash
sudo journalctl -t photoframe -b --no-pager | less
```

**Log interpretation notes:**
- `loaded <path>` — a photo was decoded and queued; normal
- `displaying photo N of M` — active cycling; normal
- `invalid photo <path>` — decode failed; that file is skipped until restart
- `transition <kind> <duration>ms` — GPU transition started; normal
- `state change: asleep → awake` — frame received a wake command
- `schedule: sleeping until HH:MM` — buttond put the frame to sleep based on schedule

---

## Start, stop, and restart the kiosk

**Restart** (preferred — avoids a seat-handoff race):

```bash
sudo systemctl stop greetd.service && sleep 1 && sudo systemctl start greetd.service
```

> Do not use `systemctl restart greetd` — it can race with logind seat handoff on tty1 and leave the session in a bad state.

**Stop** (frame goes dark):

```bash
sudo systemctl stop greetd.service
```

**Start:**

```bash
sudo systemctl start greetd.service
```

---

## Adding photos

**From another machine over the network:**

```bash
scp /path/to/photos/*.jpg frame@photoframe.local:/var/lib/photoframe/photos/local/
# With a non-default SSH key:
scp -i ~/.ssh/photoframe /path/to/photos/*.jpg frame@photoframe.local:/var/lib/photoframe/photos/local/
```

**Locally on the Pi:**

```bash
sudo cp /path/to/photos/*.jpg /var/lib/photoframe/photos/local/
sudo chown kiosk:kiosk /var/lib/photoframe/photos/local/*.jpg
```

**If you get permission denied:** your SSH session predates the install, which added your account to the `kiosk` group. Log out and reconnect:

```bash
exit
ssh frame@photoframe.local
```

**Photo library layout:**
- `local/` — manual imports; safe from sync operations
- `cloud/` — managed by the sync service; may be overwritten on sync

Both subdirectories are scanned recursively. Supported formats: JPEG, PNG.

---

## Editing configuration

The active config lives at `/etc/photoframe/config.yaml`:

```bash
sudo nano /etc/photoframe/config.yaml
```

After editing, restart the kiosk to apply:

```bash
sudo systemctl stop greetd.service && sleep 1 && sudo systemctl start greetd.service
```

> Do not edit the copy at `/opt/photoframe/etc/photoframe/config.yaml` — that's a template and gets overwritten on redeploy. Always edit `/etc/photoframe/config.yaml`.

Common things to configure: [docs/configuration.md](configuration.md)

---

## Running a manual sync

To set up cloud sync for the first time, see [docs/cloud-sync.md](cloud-sync.md).

If the sync timer is enabled, it runs automatically on its schedule. To trigger it immediately:

```bash
sudo systemctl start photoframe-sync.service
sudo journalctl -u photoframe-sync.service -f
```

To check when the next automatic sync is scheduled:

```bash
systemctl list-timers photoframe-sync.timer
```

To enable the sync timer (after configuring `/etc/photoframe/sync.env`):

```bash
sudo systemctl enable --now photoframe-sync.timer
```

---

## Updating the software

Pull the latest code and redeploy:

```bash
cd ~/photoframe
git pull
./setup/application/deploy.sh
```

The deploy step is safe to run on a running frame — it builds in the background and restarts services at the end. Your config at `/etc/photoframe/config.yaml` and photos at `/var/lib/photoframe` are not touched.

If the update includes system-level changes (new packages, systemd unit changes), run the full installer instead:

```bash
./setup/install-all.sh
```

---

## Wi-Fi recovery validation

After a fresh install, confirm Wi-Fi recovery works before mounting the frame:

```bash
make -f tests/Makefile wifi-recovery
```

If your SSH session is over `wlan0`, run inside `tmux` so the session survives the test disconnecting Wi-Fi:

```bash
tmux new -s wifi-recovery
ALLOW_WIFI_SSH_DROP=1 make -f tests/Makefile wifi-recovery
# After reconnect:
tmux attach -t wifi-recovery
```

For full Wi-Fi triage procedures, see [docs/wifi-manager.md](wifi-manager.md#service-management).

---

## Wi-Fi failure triage

When Wi-Fi recovery is stuck, gather these artifacts before changing anything:

1. Print the status summary: `/opt/photoframe/bin/print-status.sh`
2. Check watcher state files:

   ```bash
   sudo cat /var/lib/photoframe/wifi-state.json
   sudo cat /var/lib/photoframe/wifi-last.json
   ```

3. Check NetworkManager:

   ```bash
   nmcli dev status
   nmcli connection show --active
   ```

4. Validate credential apply path manually:

   ```bash
   sudo -u kiosk /opt/photoframe/bin/wifi-manager nm add --ssid "<ssid>" --psk "<password>"
   ```

5. Collect a log bundle:

   ```bash
   tests/collect_logs.sh
   sudo journalctl -u photoframe-wifi-manager.service --since "15 min ago" --no-pager
   ```

For deeper Wi-Fi service documentation, see [docs/wifi-manager.md](wifi-manager.md).
