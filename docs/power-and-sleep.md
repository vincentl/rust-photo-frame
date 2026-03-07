# Display Power and Sleep Guide

`buttond` owns wake/sleep scheduling for the frame. It evaluates the `awake-schedule` block, drives the slideshow state via the control socket, and executes DPMS commands to blank or revive the panel.

**Command context:** run commands as your operator account over SSH and use `sudo` where shown. Commands that touch the Wayland session must run as `kiosk` — use `sudo -u kiosk` as shown in the examples below. Running `powerctl` or `wlr-randr` as any other user produces "no sway process found for uid N" because those tools look for a Wayland session owned by the running user.

---

## Always-on vs. scheduled operation

**Default behavior (no awake-schedule):** `buttond` keeps the frame awake at all times. Manual sleep/wake commands still work, but no automatic transitions happen.

**Scheduled behavior:** add an `awake-schedule` block to `config.yaml` and buttond drives the frame between awake and asleep states at each boundary. See [Configuration essentials](#configuration-essentials).

---

## Quick setup

For a working schedule + display power setup:

1. Install display tool:
   ```bash
   sudo apt update && sudo apt install wlr-randr
   ```
2. Find your connector name:
   ```bash
   sudo -u kiosk wlr-randr | grep connected
   ```
3. Configure `awake-schedule` and `buttond.screen` in `/etc/photoframe/config.yaml`.
4. Restart daemon:
   ```bash
   sudo systemctl restart buttond.service
   ```
5. Verify:
   ```bash
   sudo journalctl -u buttond.service -f
   echo '{"command":"set-state","state":"awake"}' \
     | sudo -u kiosk socat - UNIX-CONNECT:/run/photoframe/control.sock
   ```

Expected outcome: buttond logs show schedule evaluation and the frame wakes when the control command is sent.

---

## Setup steps

### 1. Install wlr-randr

```bash
sudo apt update
sudo apt install wlr-randr
```

### 2. Find your display connector name

The connector name must be run inside the kiosk Wayland session:

```bash
sudo -u kiosk wlr-randr | grep connected
```

Common values: `HDMI-A-1`, `HDMI-A-2`. Use whatever your hardware reports.

### 3. Edit the config

```bash
sudo nano /etc/photoframe/config.yaml
```

Add or update these blocks:

```yaml
awake-schedule:
  timezone: America/New_York
  awake-scheduled:
    daily:
      - ["07:30", "22:00"]
    weekend:
      - ["09:00", "23:30"]

buttond:
  sleep-grace-ms: 300000
  screen:
    off-delay-ms: 3500
    display-name: HDMI-A-2   # replace with output from step 2
    on-command:
      program: /opt/photoframe/bin/powerctl
      args: [wake]
    off-command:
      program: /opt/photoframe/bin/powerctl
      args: [sleep]
```

### 4. Restart buttond

```bash
sudo systemctl restart buttond.service
```

### 5. Confirm it works

```bash
sudo journalctl -u buttond.service -f
echo '{"command":"set-state","state":"awake"}' \
  | sudo -u kiosk socat - UNIX-CONNECT:/run/photoframe/control.sock
```

buttond applies the configured schedule after the greeting delay, then continues driving wake/sleep transitions automatically.

---

## Configuration essentials

`awake-schedule` describes when the frame should be awake. Supports wrap-past-midnight windows, weekday/weekend overrides, and per-day exceptions. Times use `HH:MM` or `HH:MM:SS`. An empty list for a day key (e.g. `friday: []`) means "sleep all day on that day" — remove the key to fall back to the `daily` window.

`buttond` knobs:

| Key | What it does |
| --- | --- |
| `sleep-grace-ms` | Delay before scheduled sleep after manual interaction, preventing accidental naps |
| `screen.off-delay-ms` | Delay between sleep command and screen power-off, giving the sleep card time to render |
| `screen.on-command` / `screen.off-command` | Shell commands for wake/sleep — defaults call `powerctl` |
| `screen.display-name` | Connector name passed to both power commands; set explicitly to avoid auto-detection |

### powerctl

`/opt/photoframe/bin/powerctl` bootstraps the Wayland environment, issues `wlr-randr` DPMS requests, and falls back to `vcgencmd display_power`.

> **powerctl must run as the `kiosk` user.** It searches for a Wayland session owned by the running UID. Any other user produces "no sway process found for uid N".

```bash
sudo -u kiosk /opt/photoframe/bin/powerctl wake
sudo -u kiosk /opt/photoframe/bin/powerctl sleep
sudo -u kiosk /opt/photoframe/bin/powerctl wake HDMI-A-2   # explicit connector
```

When `display-name` is set in config, buttond always passes it to `powerctl` automatically. When omitted, `powerctl` auto-detects the first connected output.

---

## Manual overrides

Pipe JSON to the control socket to override the schedule temporarily:

- Wake: `echo '{"command":"set-state","state":"awake"}' | sudo -u kiosk socat - UNIX-CONNECT:/run/photoframe/control.sock`
- Sleep: `echo '{"command":"set-state","state":"asleep"}' | sudo -u kiosk socat - UNIX-CONNECT:/run/photoframe/control.sock`
- Toggle: `echo '{"command":"ToggleState"}' | sudo -u kiosk socat - UNIX-CONNECT:/run/photoframe/control.sock`

Manual overrides persist until the next schedule boundary (after `sleep-grace-ms` elapses).

---

## Raspberry Pi 5 + Dell S2725QC notes

- **Skip `/sys/class/backlight`** — external HDMI panels don't expose a kernel backlight interface; writing there is a no-op.
- **Primary method:** `wlr-randr --output <NAME> --off|--on` via `powerctl`.
- **Fallback:** `vcgencmd display_power 0|1` still works on Pi 5 KMS. Default `powerctl` chains both.
- **CEC:** the Dell S2725QC does not implement HDMI-CEC. `cec-ctl` cannot power it down.
- **Connector names:** list outputs with:

  ```bash
  sudo -u kiosk wlr-randr | grep -E '^[A-Z]'
  ```

---

## Troubleshooting

| Symptom | Likely cause | Fix |
| --- | --- | --- |
| `no sway process found for uid N` | Running as wrong user | `sudo -u kiosk /opt/photoframe/bin/powerctl ...` |
| `wlr-randr: cannot connect to display` | Running outside compositor session | Use `sudo -u kiosk` |
| Commands run but monitor stays on | Output name mismatch | Run `sudo -u kiosk wlr-randr | grep connected` and update `display-name` in config |
| Mode changes after wake | External scripts forcing a mode | Remove explicit `--mode` overrides |
| `wlr-randr` not installed | Package missing | `sudo apt install wlr-randr` |
| "display power action failed" in logs | Panel doesn't support this DPMS path | Keep `vcgencmd` fallback enabled or remove the command block |

---

## Operational tips

- Watch schedule transitions live: `sudo journalctl -u buttond.service -f`
- For testing, use control socket commands rather than restarting services — they take effect immediately.
- Keep custom power scripts in `/opt/photoframe/bin/` and use absolute paths in `buttond.screen`.
- Debounce physical button wiring before connecting — spurious events cause rapid wake/sleep toggling.

---

## Verification checklist

1. Send sleep command → monitor LED goes amber, panel blanks.
2. Wait a few seconds → send wake command → screen resyncs to normal mode.
3. Watch `sudo journalctl -u buttond.service` for scheduled transitions and power command output.
