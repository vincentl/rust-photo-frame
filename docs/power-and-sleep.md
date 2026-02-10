# Display Power and Sleep Guide

buttond now owns wake/sleep scheduling for the frame. It evaluates the shared `awake-schedule` block, drives the slideshow state via the control socket, and executes DPMS commands to blank or revive the panel. This guide walks through the required packages, configuration snippets, and verification steps.

## Fast path

Use this quick sequence for a working schedule + power setup:

1. Install tool:
   - `sudo apt update && sudo apt install wlr-randr`
2. Configure `awake-schedule` and `buttond.screen` in `/etc/photo-frame/config.yaml`.
3. Restart daemon:
   - `sudo systemctl restart buttond.service`
4. Verify:
   - `journalctl -u buttond.service -f`
   - `echo '{"command":"set-state","state":"awake"}' | sudo -u kiosk socat - UNIX-CONNECT:/run/photo-frame/control.sock`

For platform-specific notes, deeper troubleshooting, and operator tips, use [`power-and-sleep-notes.md`](power-and-sleep-notes.md).

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
   journalctl -u buttond.service -f
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
powerctl sleep          # auto-detect output (used for manual testing only)
powerctl wake HDMI-A-2  # explicit connector provided by configuration
```

## Manual overrides

The frame remains asleep after the greeting until it receives a control command. Pipe JSON to the Unix socket (default `/run/photo-frame/control.sock`):

- `{"command":"set-state","state":"awake"}` — force wake mode.
- `{"command":"set-state","state":"asleep"}` — force sleep mode.
- `{"command":"ToggleState"}` — flip between awake ↔ asleep.

Manual toggles persist until another command arrives. When a schedule is configured, buttond resets the state at the next boundary after respecting `sleep-grace-ms`.

## Advanced notes

Use [`power-and-sleep-notes.md`](power-and-sleep-notes.md) for:

- Raspberry Pi 5 + Dell S2725QC specifics
- troubleshooting matrix
- operational tips and caveats
