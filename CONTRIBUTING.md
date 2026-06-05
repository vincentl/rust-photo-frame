# Contributing

This document is for makers who want to modify the code, debug the kiosk stack, or validate changes before merging. End-user setup lives in [README.md](README.md) and [docs/](docs/).

---

## Coding standards & design principles

### Mission

- Deliver a rock-solid, Raspberry Pi-friendly photo frame that feels effortless to own.
- Treat elegance and efficiency as non-negotiable: every commit should shrink complexity or earn its keep.
- Keep the loop tight between hardware realities, user experience, and the rendering pipeline.

### Operating principles

- **Bias to clarity:** prefer straightforward control flow, narrow public APIs, explicit data lifecycles.
- **Win with smart algorithms:** choose data structures and scheduling strategies that minimize CPU, heap, and I/O churn.
- **Guard performance:** profile early, watch hot paths (decode, upload, draw), gate merges on measurable wins.
- **Design for resilience:** fail fast, surface structured context in logs, recover without manual babysitting.
- **Automate proof:** back critical behavior with unit, async, and integration tests; refuse TODO-driven development.

### Collaboration norms

- **Communicate intent:** describe the *why* alongside the diff; document config and ops-facing surfaces as you add them.
- **Respect context:** understand existing modules (PhotoFiles/Manager/Loader/Viewer) before reshaping them.
- **Simplify incrementally:** remove dead branches, collapse redundant types, lean on composition over inheritance-style layering.
- **Leave breadcrumbs:** note invariants, tricky math, or concurrency guarantees directly where they matter.

### Coding rules

- **Favor clarity over terseness:** complete, descriptive names; skip abbreviations unless canonical (e.g. `GPU`).
- **Configuration in kebab case:** include units in keys (`fade-ms`, `cache-capacity-count`) so consumers grasp scale at a glance.
- **Model data explicitly:** structs and enums over loosely typed maps; describe invariants in doc comments when types must uphold them.
- **Guard concurrency:** default to `Send`/`Sync` safe primitives, document ownership boundaries, prefer channels/async streams over shared mutable state.
- **Keep modules cohesive:** tight public surface; hide implementation machinery behind private helpers.
- **Test with intent:** name tests after the behavior they prove; colocate async/integration tests near the systems they exercise.
- **Respect formatting tools:** keep `rustfmt` and `clippy -- -D warnings` green; add `allow` annotations only with a comment that justifies the exception.

> We ship only when the code is easy to reason about, pleasant to maintain, and fast enough to disappear behind the photos.

---

## Local development

The four crates (`photoframe`, `buttond`, `wifi-manager`, `config-model`) build with stable Rust on macOS or Linux. You don't need a Pi to run unit tests; you do need one to validate the kiosk stack and Wi-Fi recovery end-to-end.

```bash
cargo check --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt
```

Run the photo app from source against a local config:

```bash
cargo run -p photoframe -- path/to/config.yaml
```

Validate a config without opening the render window:

```bash
cargo run -p photoframe -- --playlist-dry-run 1 path/to/config.yaml
```

---

## Setup script architecture

The provisioning pipeline lives under `setup/` and is fully idempotent — every stage can be re-run safely after OS updates or image refreshes.

### Standard deployment flow

```bash
./setup/install-all.sh                # all-in-one
./setup/tools/verify.sh               # post-install health check
sudo ./setup/system/tools/diagnostics.sh
```

`install-all.sh` provisions the OS (sudo), builds and deploys the app as your unprivileged user, and activates kiosk services. Set `CARGO_BUILD_JOBS` to cap build parallelism on low-memory devices.

### System provisioning (Trixie only)

```bash
sudo ./setup/system/install.sh
```

Run before building so toolchain and kiosk dependencies are ready. `install.sh` executes numbered modules under `setup/system/modules/`:

- `10-apt-packages.sh` — graphics stack, NetworkManager, Sway, build tools.
- `20-rust.sh` — system-wide Rust toolchain under `/usr/local/cargo`.
- `40-kiosk-user.sh` — locked `kiosk` user, runtime directories, polkit rule.
- `50-greetd.sh` — Sway session wrapper greetd launches on tty1.
- `60-systemd.sh` — `photoframe-wifi-manager.service`, `buttond.service`, `photoframe-sync.timer`.

Plus `dtoverlay=vc4-kms-v3d-pi5,cma-512` boot config for GPU CMA, swapfile replaced with `systemd-zram-generator` (half-RAM zram), Pi 5 firmware tweaks (`ENABLE_4K_BOOT=0` to skip the 4K60 profile).

### Application deployment

```bash
./setup/application/deploy.sh
```

Compiles the workspace, stages binaries and documentation under `setup/application/build/stage`, ensures the kiosk service user exists, and installs artifacts into `/opt/photoframe`. Modules:

- `10-build.sh` — compiles `wifi-manager`, `photoframe`, `buttond` in release mode (never as root).
- `20-stage.sh` — stages binaries, config templates, wordlist, and `docs/` content.
- `30-install.sh` — installs into `/opt/photoframe`; seeds `/etc/photoframe/config.yaml` if missing.

### Diagnostics

```bash
sudo ./setup/system/tools/diagnostics.sh
```

Inspects the greetd session, kiosk user, and display-manager wiring. Run from the device when display login fails or the kiosk session won't start.

### Shared systemd helper library

Provisioning and diagnostics scripts use `setup/lib/systemd.sh`. Source pattern:

```bash
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib/systemd.sh
source "${SCRIPT_DIR}/../lib/systemd.sh"
```

Helper groups:

- Availability and reload: `systemd_available`, `systemd_daemon_reload`
- Unit lifecycle: `systemd_enable_unit`, `systemd_start_unit`, `systemd_restart_unit`, `systemd_stop_unit`
- Enable/disable/mask: `systemd_enable_now_unit`, `systemd_disable_unit`, `systemd_disable_now_unit`, `systemd_mask_unit`, `systemd_unmask_unit`, `systemd_set_default_target`
- State checks: `systemd_unit_exists`, `systemd_is_active`, `systemd_is_enabled`, `systemd_status`, `systemd_unit_property`
- Unit/drop-in management: `systemd_install_unit_file`, `systemd_install_dropin`, `systemd_remove_dropins`

---

## Pre-merge test matrix

Exercise each row at least once per release cycle.

| ID | Display | Network | Library | Internet | Notes |
|----|---------|---------|---------|----------|-------|
| M1 | 3840×2160 @60 Hz | Ethernet | Tiny (≤50) | Available | Baseline sanity after clean install |
| M2 | 3840×2160 @30 Hz | Wi-Fi | Medium (1–3k) | Available | Stress decode/render pipeline |
| M3 | 3840×2160 @60 Hz | Wi-Fi | Tiny | ISP outage (LAN up) | Watcher without WAN |
| M4 | 3840×2160 @60 Hz | Wi-Fi | Medium | Available | Overnight burn-in with sleep schedule |
| M5 | 3840×2160 @60 Hz | Ethernet | Medium | Available | Update + rollback rehearsal |

### Pre-flight

```bash
vcgencmd measure_temp
sudo -u kiosk wlr-randr
cargo check --workspace
cargo test --workspace
tests/collect_logs.sh        # baseline log capture
```

### Phase checklist

- **Blank-Pi install** — Flash latest Trixie 64-bit, boot, network up, `XDG_SESSION_TYPE=wayland` after first graphical login.
- **Project setup** — clone repo; `sudo ./setup/system/install.sh`; `./setup/application/deploy.sh`; customize `/etc/photoframe/config.yaml`; populate library.
- **Kiosk autostart** — `systemctl status greetd photoframe-wifi-manager photoframe-sync.timer`; reboot; confirm greetd claims tty1 and the app launches full-screen.
- **Display & frame rate** — `wlr-randr` shows target mode (4K@60 preferred); short phone video documents smoothness; note tearing/flicker.
- **Button & power** — `sudo evtest` to identify input device; short press toggles sleep; double-click clean shutdown; long press hard-off documented (don't actually do it if there's risk of corruption).
- **Sleep schedule** — set a near-future window, restart kiosk, observe transitions; manual `set-state`/`toggle-state` over the control socket works.
- **Wi-Fi provisioning** — `make -f tests/Makefile wifi-recovery`; phone joins `PhotoFrame-Setup`, submits credentials, frame reconnects. Also exercise LAN-up/Internet-down (slideshow keeps advancing) and full Wi-Fi outage with later restoration (auto-reconnect).
- **Library ingest** — tiny library validates EXIF/mat/transitions; medium library run for 10–15 min with `top -H -p $(pidof photoframe)` and `vcgencmd measure_temp`.
- **Updates & rollback** — `git checkout <release-tag>`, rebuild, restart greetd; rollback to previous tag; confirm unit files unchanged via `systemctl cat greetd.service`.
- **Power loss** — momentarily cut power; confirm auto-boot, kiosk service startup, slideshow resume; check `journalctl -b | grep -i fsck` and `dmesg | grep -i error`.
- **Diagnostics bundle** — `tests/collect_logs.sh`; verify artifact at `artifacts/FRAME-logs-*.tar.gz`.

### Acceptance criteria

- Cold boot to slideshow ready in ≤ 45 s after greetd starts.
- Display locked to desired mode with no sustained tearing.
- Button behaviors reliable (short-press sleep toggle, double-click shutdown).
- Sleep schedule respects configured windows; reacts to manual `toggle-state` immediately.
- Wi-Fi provisioning, outage resilience, and watcher recovery all succeed without slideshow halt.
- Medium-library playback smooth (CPU spikes <150% overall, no OOM or IO errors).
- Update deploys cleanly; rollback path verified.
- Power-loss recovery returns to slideshow without manual intervention.
- Diagnostics bundle captures all required evidence.

### Recovery & rollback notes

- **Bad config.yaml:** restore from a known-good backup (keep one before editing); validate YAML (`python3 -c "import yaml,sys; yaml.safe_load(open('/etc/photoframe/config.yaml'))"`); restart kiosk.
- **Service won't start:** `journalctl -u greetd.service -b` and `journalctl -u photoframe-wifi-manager.service -b`; rebuild binary; `sudo systemctl daemon-reload`.
- **Failed update:** `git checkout <previous-good>`; rebuild + restart; if binary corrupted, `rm -rf target/` and rebuild.
- **Network stuck offline:** `nmcli connection show`; `sudo systemctl restart NetworkManager`; collect logs.

### Test harness quick reference

```bash
make -f tests/Makefile smoke           # 10–15 min
make -f tests/Makefile daily           # ≤3 min
make -f tests/Makefile wifi-recovery
make -f tests/Makefile collect
```

---

## Dependency upgrade runbook

Use this before merging Wi-Fi recovery changes that also touch host dependencies.

**1. Maintenance branch:**

```bash
git checkout -b maintenance/upgrade-$(date +%Y%m%d)
```

**2. Capture baseline:**

```bash
./developer/capture-system-baseline.sh
```

Writes version snapshots under `artifacts/upgrade-baseline-<timestamp>/`.

**3. Refresh system packages on Pi:**

```bash
sudo apt update
sudo apt full-upgrade -y
sudo reboot
# After reboot:
sudo ./setup/system/install.sh
./setup/application/deploy.sh
./setup/tools/verify.sh
```

**4. Refresh Rust dependencies:**

```bash
cargo update -w
cargo check --workspace
cargo test -p photoframe -- --nocapture
cargo test -p wifi-manager -- --nocapture
cargo clippy -p wifi-manager -- -D warnings
```

If `cargo update -w` introduces regressions, pin only the problematic crates and document why in the commit message.

**5. Re-evaluate git patches** in `Cargo.toml` (`cosmic-text`, `glyphon`). Keep pins only when required for reproducible runtime or compatibility on Pi.

**6. Recovery validation on device** — run the wrong-password recovery, AP outage auto-reconnect, and `WAN down / Wi-Fi up` (no false trigger) checks via `make -f tests/Makefile wifi-recovery`.

---

## Debugging the kiosk stack

For developer-only debugging on a live Pi when normal operator runbooks aren't enough.

### Manual debug launch (advanced)

In normal operation `photoframe` is started by greetd via `/usr/local/bin/photoframe-session`. For low-level debugging you can run a standalone Sway session as `kiosk`.

Prerequisite (one-time):

```bash
sudo loginctl enable-linger kiosk
```

Launch:

```bash
sudo -u kiosk \
  XDG_RUNTIME_DIR="/run/user/$(id -u kiosk)" \
  dbus-run-session \
  sway -c /usr/local/share/photoframe/sway/config
```

### Debugging without journald capture

Logs to stdout:

```bash
sudo -u kiosk \
  XDG_RUNTIME_DIR="/run/user/$(id -u kiosk)" \
  dbus-run-session \
  env PHOTOFRAME_LOG=stdout \
  env RUST_LOG='photoframe::tasks::viewer=debug,info' \
  sway -c /usr/local/share/photoframe/sway/config
```

Logs to file:

```bash
sudo -u kiosk \
  XDG_RUNTIME_DIR="/run/user/$(id -u kiosk)" \
  dbus-run-session \
  env PHOTOFRAME_LOG='file:/var/tmp/photoframe.log' \
  env RUST_LOG='photoframe::tasks::viewer=debug,info' \
  sway -c /usr/local/share/photoframe/sway/config

sudo tail -f /var/tmp/photoframe.log
```

### Overlay takeover test

Verify overlay focus/fullscreen behavior before Wi-Fi recovery testing.

One-time install:

```bash
sudo install -D -m 0755 developer/overlay-test.sh /usr/local/bin/overlay-test
sudo install -D -m 0644 developer/systemd/wifi-overlay-test.service /etc/systemd/system/wifi-overlay-test.service
sudo systemctl daemon-reload
```

Run:

```bash
sudo systemctl start wifi-overlay-test.service
sudo -u kiosk bash developer/overlay-test.sh status
sudo -u kiosk bash developer/overlay-test.sh hide
```

Troubleshooting:

- `systemctl status greetd` — Sway session up?
- `sudo sh -lc 'uid=$(id -u kiosk); ls "/run/user/$uid"/sway-ipc.*.sock'` — IPC socket present?
- `journalctl -u wifi-overlay-test.service -n 50 --no-pager`

### Kiosk shell with live Sway environment

Open an interactive shell as `kiosk` with compositor environment exported:

```bash
sudo sh -lc '
  uid=$(id -u kiosk)
  RUNDIR="/run/user/$uid"
  SWAYSOCK="$(ls "$RUNDIR"/sway-ipc.*.sock | head -1)"
  WAYLAND_DISPLAY="$(basename "$(ls "$RUNDIR"/wayland-* | head -1)")"
  exec sudo --preserve-env=RUNDIR,SWAYSOCK,WAYLAND_DISPLAY \
    -u kiosk env XDG_RUNTIME_DIR="$RUNDIR" SWAYSOCK="$SWAYSOCK" WAYLAND_DISPLAY="$WAYLAND_DISPLAY" bash -l
'
```

Inside that shell:

- Verify env: `echo "$XDG_RUNTIME_DIR" "$SWAYSOCK" "$WAYLAND_DISPLAY"`
- Test IPC: `swaymsg -s "$SWAYSOCK" -t get_tree >/dev/null && echo ok || echo fail`

### Manually simulate a Wi-Fi failure

`developer/suspend-wifi.sh` stashes the active profile keyfile, swaps in a Wi-Fi connection with a deliberately wrong PSK, and tries to activate it. Run from a multiplexer or with `nohup` so it survives SSH drops:

```bash
sudo nohup bash developer/suspend-wifi.sh wlan0 >/tmp/wifi-test.log 2>&1 & disown
```
