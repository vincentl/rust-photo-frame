# Quick Reference

Common commands for daily use. Run these from your operator SSH session unless noted.

---

## Control the frame

| What you want | Command |
| --- | --- |
| **Wake** (start cycling photos) | `echo '{"command":"set-state","state":"awake"}' \| sudo -u kiosk socat - UNIX-CONNECT:/run/photoframe/control.sock` |
| **Sleep** (stop cycling, blank display) | `echo '{"command":"set-state","state":"asleep"}' \| sudo -u kiosk socat - UNIX-CONNECT:/run/photoframe/control.sock` |
| **Toggle** wake ↔ sleep | `echo '{"command":"ToggleState"}' \| sudo -u kiosk socat - UNIX-CONNECT:/run/photoframe/control.sock` |
| **Screen on** (DPMS / power) | `sudo -u kiosk /opt/photoframe/bin/powerctl wake` |
| **Screen off** (DPMS / power) | `sudo -u kiosk /opt/photoframe/bin/powerctl sleep` |
| Screen on with explicit output | `sudo -u kiosk /opt/photoframe/bin/powerctl wake HDMI-A-2` |

> `powerctl` must be run as the `kiosk` user — it looks for a Wayland session owned by that UID.

---

## Check status

| What you want | Command |
| --- | --- |
| **Quick health check** | `./setup/tools/verify.sh` |
| **Full status summary** | `/opt/photoframe/bin/print-status.sh` |
| **Live photo logs** | `sudo journalctl -t photoframe -f` |
| **Wi-Fi manager logs** | `sudo journalctl -u photoframe-wifi-manager.service -f` |
| **Button daemon logs** | `sudo journalctl -u buttond.service -f` |
| **All service status** | `sudo systemctl status greetd photoframe-wifi-manager buttond` |
| **Count photos in library** | `find /var/lib/photoframe/photos -type f \| wc -l` |
| **List connected outputs** | `sudo -u kiosk wlr-randr \| grep -E 'connected'` |
| **Check control socket** | `sudo ls -l /run/photoframe/control.sock` |

---

## Manage the frame

| What you want | Command |
| --- | --- |
| **Restart kiosk** (reliable) | `sudo systemctl stop greetd.service && sleep 1 && sudo systemctl start greetd.service` |
| **Edit config** | `sudo nano /etc/photoframe/config.yaml` |
| **Apply config changes** | restart kiosk (command above) |
| **Add photos from Mac/PC** | `scp /path/to/photos/* frame@photoframe.local:/var/lib/photoframe/photos/local/` |
| **Add photos locally** | `sudo cp /path/*.jpg /var/lib/photoframe/photos/local/ && sudo chown kiosk:kiosk /var/lib/photoframe/photos/local/*` |
| **Trigger manual sync** | `sudo systemctl start photoframe-sync.service` |
| **Update software** | `git pull && ./setup/application/deploy.sh` |

---

## Diagnose problems

| What you want | Command |
| --- | --- |
| **Last 50 photo app logs** | `sudo journalctl -t photoframe -n 50 --no-pager` |
| **Logs since boot** | `sudo journalctl -t photoframe -b --no-pager` |
| **Check Wi-Fi state** | `sudo cat /var/lib/photoframe/wifi-state.json` |
| **Check swap** | `swapon --show` |
| **Collect log bundle** | `tests/collect_logs.sh` |
| **Run diagnostics script** | `sudo ./setup/system/tools/diagnostics.sh` |

---

## Notes

- **`sudo -u kiosk`** — required for anything that touches the Wayland session (powerctl, socat to the control socket from a root shell).
- **Restart method** — use stop/sleep/start, not `systemctl restart greetd`, to avoid a race with the seat handoff.
- **Config location** — always edit `/etc/photoframe/config.yaml`, not the template in `/opt/photoframe`.
- **Photo library** — `local/` is for manual imports; `cloud/` is managed by the sync service. Both are scanned.
