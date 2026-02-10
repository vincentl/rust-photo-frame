# Display Power and Sleep Notes

This appendix contains platform-specific notes, troubleshooting references, and operational tips for `buttond` power/sleep behavior.

For primary setup steps, use [`power-and-sleep.md`](power-and-sleep.md).

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
3. Watch `journalctl -u buttond.service` for scheduled transitions and power command execution.

## Troubleshooting matrix

| Symptom | Likely cause | Fix |
| ------- | ------------ | --- |
| `wlr-randr: cannot connect to display` | Command is running outside the compositor Wayland session | Ensure `buttond` runs in the login user session or export the correct `WAYLAND_DISPLAY`. |
| Commands run but monitor stays on | Output name mismatch | Use `wlr-randr | grep connected` and set correct `display-name` in config. |
| Mode changes after wake | External scripts force a mode | Remove explicit `--mode` overrides and let compositor restore output mode. |
| `wlr-randr` not installed | Package missing | `sudo apt install wlr-randr` |
| “display power action failed” in logs | Panel does not support DPMS path used | Keep fallback enabled or remove command block to silence warnings on unsupported hardware. |

## Additional operational tips

- Use `journalctl -u buttond.service -f` to watch schedule boundaries, DPMS actions, and manual overrides.
- For interactive tests, send `{"command":"set-state","state":"awake"}` over the control socket instead of restarting services.
- Keep custom power scripts under `/opt/photo-frame/bin` and reference absolute paths in `buttond.screen`.
- Debounce physical button wiring before writing to the control socket to avoid accidental double toggles.
