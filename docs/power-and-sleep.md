# Display Power and Sleep Guide

buttond now owns wake/sleep scheduling for the frame. It evaluates the shared `awake-schedule` block, drives the slideshow state via the control socket, and executes DPMS commands to blank or revive the panel. This guide walks through the required packages, configuration snippets, and verification steps.

Command context: run commands as your operator account over SSH and use `sudo` where shown. Commands that touch Wayland session state run as `kiosk`.

## Quick setup

Use this quick sequence for a working schedule + power setup:

1. Install tool:
   - `sudo apt update && sudo apt install wlr-randr`
2. Configure `awake-schedule` and `buttond.screen` in `/etc/photo-frame/config.yaml`.
3. Restart daemon:
   - `sudo systemctl restart buttond.service`
4. Verify:
   - `sudo journalctl -u buttond.service -f`
   - `echo '{"command":"set-state","state":"awake"}' | sudo -u kiosk socat - UNIX-CONNECT:/run/photo-frame/control.sock`

Expected outcome: buttond logs show schedule evaluation and the frame wakes when the control command is sent.

## Setup steps

1. Install the wlroots command-line utilities:
   ```bash
   sudo apt update
   sudo apt install wlr-randr
   ```
2. (Optional) Deploy the application bundle (`./setup/application/deploy.sh`) so `/opt/photo-frame/bin/powerctl` and the systemd units land on the device. The helper mirrors the default inline commands and can be referenced once the staged tree has been installed under `/opt`.
3. Edit `/etc/photo-frame/config.yaml` so it includes an awake schedule and display power commands:
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
       display-name: HDMI-A-2
       on-command:
         program: /opt/photo-frame/bin/powerctl
         args: [wake]
       off-command:
         program: /opt/photo-frame/bin/powerctl
         args: [sleep]
   ```
4. Restart the daemon so it picks up the edits:
   ```bash
   sudo systemctl restart buttond.service
   ```
5. Confirm scheduling works by watching the logs and issuing a manual wake:
   ```bash
   sudo journalctl -u buttond.service -f
   echo '{"command": "set-state", "state": "awake"}' \
     | sudo -u kiosk socat - UNIX-CONNECT:/run/photo-frame/control.sock
   ```
   buttond applies the configured schedule after the greeting delay, then continues driving wake/sleep transitions automatically.

   Update `display-name` in the snippet to match the connector reported by `wlr-randr | grep connected` (for example `HDMI-A-1`).

## Configuration essentials

`awake-schedule` describes when the frame should be awake. The block supports wrap-past-midnight windows, weekday/weekend overrides, and per-day exceptions. Times accept `HH:MM` or `HH:MM:SS` strings and may optionally include a trailing IANA timezone name (for example `"07:30 America/Los_Angeles"`). When a field omits a zone the top-level `timezone` is used.

buttond honours a few additional knobs inside its namespaced section:

- `sleep-grace-ms` — how long to defer scheduled sleep after button activity or a manual wake. This prevents accidental naps immediately after someone interacts with the frame.
- `screen.off-delay-ms` — delay between sending the sleep command and running the configured power-off command so the on-device sleep screen has time to render.
- `screen.on-command` / `screen.off-command` — shell commands executed when buttond transitions the panel. The defaults call `powerctl`, which issues `wlr-randr` DPMS requests with a `vcgencmd` fallback.
- `screen.display-name` — set this to the connector name reported by `wlr-randr` (for example `HDMI-A-2`). buttond appends the value to both power commands so wake/sleep do not rely on runtime auto-detection.

The helper `/opt/photo-frame/bin/powerctl` bootstraps the Wayland environment, auto-detects the first connected output when no argument is supplied, and chains `vcgencmd` as a fallback. buttond now prefers `powerctl` for both wake/sleep transitions and state detection whenever it sees `powerctl` configured for the screen commands. When `display-name` is omitted, the state probe targets any connected output.

Set `buttond.screen.display-name` so buttond always calls it with an explicit connector:
```bash
sudo -u kiosk /opt/photo-frame/bin/powerctl sleep
sudo -u kiosk /opt/photo-frame/bin/powerctl wake HDMI-A-2
```

## Manual overrides

The frame remains asleep after the greeting until it receives a control command. Pipe JSON to the Unix socket (default `/run/photo-frame/control.sock`):

- `{"command":"set-state","state":"awake"}` — force wake mode.
- `{"command":"set-state","state":"asleep"}` — force sleep mode.
- `{"command":"ToggleState"}` — flip between awake ↔ asleep.

Manual toggles persist until another command arrives. When a schedule is configured, buttond resets the state at the next boundary after respecting `sleep-grace-ms`.

## Raspberry Pi 5 + Dell S2725QC notes

- Skip `/sys/class/backlight`. External HDMI panels do not expose a kernel backlight interface; writing there is a no-op.
- Primary method: `wlr-randr --output <NAME> --off|--on`. Ensure `buttond` runs inside the same user Wayland session so the command sees a valid display socket.
- Fallback: `vcgencmd display_power 0|1` still works on Pi 5 KMS. Default commands chain both methods.
- CEC support: Dell monitors (including S2725QC) do not implement HDMI-CEC. `cec-ctl` cannot power them down.
- Connector names: usually `HDMI-A-1` or `HDMI-A-2`. List outputs with:

```bash
wlr-randr | grep -E '^(.* connected|.)'
```

- Wayland session scope: `wlr-randr` must run in the compositor session. If running as a system service, export `WAYLAND_DISPLAY` (for example `WAYLAND_DISPLAY=wayland-1`) and forward the compositor socket via service binding.

Verification checklist:

1. Run sleep command; monitor LED should turn amber and panel should blank.
2. Wait a few seconds, then run wake command; screen should resync to expected mode.
3. Watch `sudo journalctl -u buttond.service` for scheduled transitions and power command execution.

## Troubleshooting matrix

| Symptom | Likely cause | Fix |
| ------- | ------------ | --- |
| `wlr-randr: cannot connect to display` | Command is running outside the compositor Wayland session | Ensure `buttond` runs in the login user session or export the correct `WAYLAND_DISPLAY`. |
| Commands run but monitor stays on | Output name mismatch | Use `wlr-randr | grep connected` and set correct `display-name` in config. |
| Mode changes after wake | External scripts force a mode | Remove explicit `--mode` overrides and let compositor restore output mode. |
| `wlr-randr` not installed | Package missing | `sudo apt install wlr-randr` |
| “display power action failed” in logs | Panel does not support DPMS path used | Keep fallback enabled or remove command block to silence warnings on unsupported hardware. |

## Operational tips

- Use `sudo journalctl -u buttond.service -f` to watch schedule boundaries, DPMS actions, and manual overrides.
- For interactive tests, send `{"command":"set-state","state":"awake"}` over the control socket instead of restarting services.
- Keep custom power scripts under `/opt/photo-frame/bin` and reference absolute paths in `buttond.screen`.
- Debounce physical button wiring before writing to the control socket to avoid accidental double toggles.
