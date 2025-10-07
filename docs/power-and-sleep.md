# Display Power and Sleep Guide

This guide focuses on powering down HDMI monitors from the Raspberry Pi 5 running Raspberry Pi OS (Trixie) under Wayfire/wlroots. It explains how the sleep schedule interacts with `wlr-randr`, the default `powerctl` helper, and what to check when the Dell S2725QC refuses to cooperate.

## Quick start

1. Install the wlroots command-line utilities:
   ```bash
   sudo apt update
   sudo apt install wlr-randr
   ```
2. (Optional) Install the app (`./setup/app/run.sh`) so `/opt/photo-frame/bin/powerctl` lands on the device. The helper mirrors the default inline commands and can be referenced once the staged tree is deployed to `/opt`.
3. Enable sleep in `config.yaml` using the built-in `@OUTPUT@` placeholder:
   ```yaml
   sleep-mode:
     timezone: America/New_York
     on-hours:
       start: "08:00"
       end:   "22:00"
     dim-brightness: 0.05
     display-power:
       sleep-command: "wlr-randr --output @OUTPUT@ --off || vcgencmd display_power 0"
       wake-command: "wlr-randr --output @OUTPUT@ --on  || vcgencmd display_power 1"
   ```
4. Start the service. The frame wakes the moment `on-hours.start` arrives and sleeps when `on-hours.end` hits, even if a manual override was active. Writing `{ "command": "ToggleSleep" }` to `/run/photo-frame/control.sock` (or pressing a mapped GPIO button) still flips sleep ↔ wake immediately; the next boundary restores the scheduled state. Run a quick validation with `rust-photo-frame config.yaml --sleep-test 10`.

## Configuration essentials

The `sleep-mode` block accepts wrap-past-midnight windows, per-day overrides, and optional display-power commands. Times are interpreted using the configured `timezone`; when a field omits a zone the top-level `sleep.timezone` applies. Day overrides take precedence over weekend/weekday overrides, which in turn override the default `on-hours` window.

Manual overrides can be triggered by writing `{ "command": "ToggleSleep" }` to `/run/photo-frame/control.sock`. Each press flips the current state (wake ↔ sleep) immediately, regardless of the schedule. Upcoming boundaries still win: `on-hours.start` forces wake, `on-hours.end` forces sleep, and either transition clears any active override. Overrides can also be seeded at startup by setting `PHOTO_FRAME_SLEEP_OVERRIDE=sleep|wake` or pointing `PHOTO_FRAME_SLEEP_OVERRIDE_FILE` at a file containing `sleep` or `wake`.

The default `display-power` commands issue Wayland DPMS requests via:
```
wlr-randr --output @OUTPUT@ --off || vcgencmd display_power 0
wlr-randr --output @OUTPUT@ --on  || vcgencmd display_power 1
```
`@OUTPUT@` is replaced with the first connected output reported by `wlr-randr`. If detection fails the code falls back to `HDMI-A-1` and logs a warning.

`/opt/photo-frame/bin/powerctl` wraps the same logic. It auto-detects the first connected output, falls back to `HDMI-A-1`, and chains `vcgencmd` so you do not need to duplicate shell logic in your configuration. The helper is safe to call from other scripts:
```bash
powerctl sleep        # auto-detect output
powerctl wake HDMI-A-1 # override the connector
```

### CLI support

- `--verbose-sleep` logs the parsed schedule and the next 24 hours of transitions during startup.
- `--sleep-test <SECONDS>` forces the configured commands to sleep, waits the requested duration, retries wake once after two seconds if necessary, and exits non-interactively. Use this at night to confirm DPMS works before relying on the automation.

## Raspberry Pi 5 + Dell S2725QC notes

- **Skip `/sys/class/backlight`.** External HDMI panels do not expose a kernel backlight interface—writing to `/sys/class/backlight/*` is a no-op.
- **Primary method:** `wlr-randr --output <NAME> --off|--on`. Install `wlr-randr` and ensure the app runs inside the same user Wayland session as the compositor so the command inherits a valid `WAYLAND_DISPLAY`.
- **Fallback:** `vcgencmd display_power 0|1` still works on the Pi 5’s KMS stack. The default commands chain both approaches.
- **CEC support:** Dell monitors (including the S2725QC) do not implement HDMI-CEC. Tools such as `cec-ctl` will not power them down.
- **Connector names:** Expect `HDMI-A-1` or `HDMI-A-2`. List outputs with:
  ```bash
  wlr-randr | grep -E '^(.* connected|.)'
  ```
- **Wayland session scope:** `wlr-randr` must run under the same user session as Wayfire. When the app runs as a user service (`systemd --user`), no extra environment tweaks are needed. When running as a system service, export `WAYLAND_DISPLAY` (for example `WAYLAND_DISPLAY=wayland-1`) and forward the compositor’s socket via `BindPaths=`.
- **Verification checklist:**
  1. Run the sleep command; the Dell’s LED should turn amber and the panel should blank.
  2. Wait a few seconds, then run the wake command; the screen should resync at 3840×2160 @ 60 Hz.
  3. Use `--sleep-test 10` from an SSH session to confirm the automation handles both directions.

## Troubleshooting

| Symptom | Likely cause | Fix |
| ------- | ------------ | --- |
| `wlr-randr: cannot connect to display` | Command running outside the compositor’s Wayland session | Ensure the service runs as the login user or export `WAYLAND_DISPLAY` to match the compositor. |
| Commands run but the monitor stays on | Output name mismatch | Use `wlr-randr | grep connected` to find the connector, or rely on the default `@OUTPUT@` placeholder/powerctl helper. |
| Mode changes after wake | External scripts forcing a resolution | Remove any explicit `--mode` flags; rely on the compositor to remember the previous mode. |
| `wlr-randr` not installed | Package missing | `sudo apt install wlr-randr`. |
| Commands fail and the log reports “display power action failed” | DPMS not supported by the panel | The viewer falls back to dimming only; leave `display-power` configured for future hardware or remove the block to silence warnings. |

## Compatibility matrix

- **Wayfire/wlroots (default image):** `wlr-randr` on/off ✅, `vcgencmd` fallback ✅
- **Sway:** `swaymsg output NAME dpms off|on` ✅ (replace the default commands or adapt `powerctl`)
- **Hyprland:** `hyprctl dispatch dpms off|on` ✅
- **X11 sessions:** `xset dpms force off` ⚠️ (not used by this project but relevant if you repurpose the code)

## Additional tips

- Wrap long-running GPIO button handlers with a debouncer before writing to the control socket to avoid accidental double toggles.
- When experimenting interactively, run the viewer with `--verbose-sleep` so you can see upcoming transitions and the detected output name in the logs.
- Store custom power scripts alongside `powerctl` in `/opt/photo-frame/bin` and reference them via absolute paths inside `sleep-mode.display-power`.
