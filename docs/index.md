# Documentation Index

Find what you need by task below. If you're not sure where to start, use the **"Setting up for the first time"** path.

---

## Setting up for the first time

Read these in order:

1. **[hardware.md](hardware.md)** — bill of materials, button wiring, planning tips
2. **[installation.md](installation.md)** — SD card, install, add photos, wake the frame
3. **[first-boot.md](first-boot.md)** — what to expect on screen; what's normal vs. broken
4. **[quick-reference.md](quick-reference.md)** — keep this open during setup

---

## Something isn't working

Start with **[troubleshooting.md](troubleshooting.md)**. Covers:
- Screen goes black after greeting
- `powerctl wake` returns "no sway process found"
- Photos don't appear after waking
- Frame wakes/sleeps at wrong times
- Build OOM, permission errors, Wi-Fi hotspot issues

For deeper Wi-Fi triage: [wifi-manager.md](wifi-manager.md)
For display / sleep issues: [power-and-sleep.md](power-and-sleep.md)

---

## Customizing the behavior

- **[configuration.md](configuration.md)** — full YAML reference: transitions, matting, playlist weighting, greeting screen, schedule
- **[power-and-sleep.md](power-and-sleep.md)** — wake/sleep schedule, DPMS, screen commands

---

## Day-to-day operations

- **[quick-reference.md](quick-reference.md)** — command cheat sheet
- **[operations.md](operations.md)** — adding photos, editing config, restarting, updating, sync
- **[cloud-sync.md](cloud-sync.md)** — automatic cloud sync setup (Google Drive, Dropbox, S3, and more)

---

## Building the physical frame

- **[hardware.md](hardware.md)** — BOM and button wiring
- **[../maker/fabrication.md](../maker/fabrication.md)** — French cleat mount, shadow box, 3D-printed brackets

---

## Understanding the internals

- **[kiosk.md](kiosk.md)** — greetd + Sway session architecture, provisioning details
- **[wifi-manager.md](wifi-manager.md)** — Wi-Fi recovery state machine, captive portal, configuration
- **[memory.md](memory.md)** — memory pipeline, budget by Pi model, tuning levers
- **[../setup/README.md](../setup/README.md)** — setup script internals

---

## Account glossary

Two accounts matter on a running frame:

- **Operator account** (e.g. `frame`) — your SSH login; used for maintenance, `sudo` escalation, `scp` file transfers.
- **Service account** (`kiosk`) — runs greetd, Sway, the photo app, and Wi-Fi manager. Use `sudo -u kiosk` when a command needs access to the Wayland session (e.g. `powerctl`, `socat` to the control socket from a root shell).

---

## Complete document list

| Document | What it covers |
| --- | --- |
| [hardware.md](hardware.md) | BOM, button wiring, planning |
| [installation.md](installation.md) | Full install guide |
| [first-boot.md](first-boot.md) | Normal first-boot sequence, wake command |
| [troubleshooting.md](troubleshooting.md) | Symptom → cause → fix |
| [quick-reference.md](quick-reference.md) | Daily command cheat sheet |
| [operations.md](operations.md) | Adding photos, config, updates, sync |
| [cloud-sync.md](cloud-sync.md) | Cloud sync setup and management |
| [configuration.md](configuration.md) | YAML reference: all settings |
| [power-and-sleep.md](power-and-sleep.md) | Schedule, DPMS, powerctl |
| [wifi-manager.md](wifi-manager.md) | Wi-Fi recovery architecture and ops |
| [kiosk.md](kiosk.md) | greetd + Sway stack internals |
| [memory.md](memory.md) | Memory budget and tuning |
| [../maker/fabrication.md](../maker/fabrication.md) | Physical build guide |
| [../setup/README.md](../setup/README.md) | Setup script internals |
| [../developer/test-plan.md](../developer/test-plan.md) | Full QA validation matrix |
