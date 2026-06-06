# Testing & release

The single home for verifying the photo frame: automated checks you can run on
any dev machine, the on-device test plan, and the release gate.

- Day-to-day operator commands → [docs/operate.md](../docs/operate.md)
- How the kiosk stack is built and debugged → [architecture.md](architecture.md)

---

## Automated checks (no Pi required)

Run on any macOS/Linux dev machine before pushing:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo audit                          # dependency CVE scan (cargo install cargo-audit)
make -f tests/Makefile docs-links    # markdown link check
```

`cargo audit` should report **zero vulnerabilities**. A lone `paste`
*unmaintained* warning is expected and benign — it is a build-time proc-macro
pulled in only by wgpu's macOS Metal backend, which never compiles on the Pi.

---

## On-device test harness

The `tests/Makefile` targets run on the device after an install:

```bash
make -f tests/Makefile smoke           # 10–15 min full sanity
make -f tests/Makefile daily           # ≤3 min quick check
make -f tests/Makefile wifi-recovery   # captive-portal recovery
make -f tests/Makefile collect         # bundle logs for a report
```

Plus the post-install health checks:

```bash
./setup/tools/verify.sh
/opt/photoframe/bin/print-status.sh
sudo ./setup/system/tools/diagnostics.sh
```

### Pre-flight (on the Pi)

```bash
vcgencmd measure_temp
grep -H . /sys/class/hwmon/hwmon*/fan1_input 2>/dev/null   # fan RPM (non-zero under load)
sudo -u kiosk wlr-randr
tests/collect_logs.sh        # baseline log capture
```

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

---

## Manual phase checklist

- **Blank-Pi install** — Flash latest Trixie 64-bit, boot, network up, `XDG_SESSION_TYPE=wayland` after first graphical login.
- **Project setup** — clone repo; `sudo ./setup/system/install.sh`; `./setup/application/deploy.sh`; customize `/etc/photoframe/config.yaml`; populate library.
- **Kiosk autostart** — `systemctl status greetd photoframe-wifi-manager photoframe-sync.timer`; reboot; confirm greetd claims tty1 and the app launches full-screen. Confirm the `kiosk` user can reach seatd: `id -nG kiosk` must include the group that owns `/run/seatd.sock` (`stat -c '%G' /run/seatd.sock`, normally `seat`). A missing membership fails every session at the wrapper's seatd check.
- **Display & frame rate** — `wlr-randr` shows target mode (4K@60 preferred); short phone video documents smoothness; note tearing/flicker.
- **Button & power** — `sudo evtest` to identify input device; single press toggles **both** directions — test the round trip (awake → sleep, then sleep → wake); double press clean shutdown; long hold is bypassed for the Pi 5 firmware force-off (don't actually trigger it if there's risk of corruption).
- **Sleep schedule** — set a near-future window, restart kiosk, observe transitions; manual `set-state`/`toggle-state` over the control socket works. Exercise a wrap-past-midnight window (start later than end, e.g. `["21:00","07:00"]`).
- **Wi-Fi provisioning** — `make -f tests/Makefile wifi-recovery`; phone joins `PhotoFrame-Setup`, submits credentials, frame reconnects. Also exercise LAN-up/Internet-down (slideshow keeps advancing) and full Wi-Fi outage with later restoration (auto-reconnect).
- **Library ingest** — tiny library validates EXIF/mat/transitions; medium library run for 10–15 min with `top -H -p $(pidof photoframe)` and `vcgencmd measure_temp`. During the burn-in confirm the fan ramps under load (`grep -H . /sys/class/hwmon/hwmon*/fan1_input`) and the temperature holds under ~80 °C.
- **Updates & rollback** — `git checkout <release-tag>`, rebuild, restart greetd; rollback to previous tag; confirm unit files unchanged via `systemctl cat greetd.service`.
- **Power loss** — momentarily cut power; confirm auto-boot, kiosk service startup, slideshow resume; check `journalctl -b | grep -i fsck` and `dmesg | grep -i error`.
- **Diagnostics bundle** — `tests/collect_logs.sh`; verify artifact at `artifacts/FRAME-logs-*.tar.gz`.

### Acceptance criteria

- Cold boot to slideshow ready in ≤ 45 s after greetd starts.
- Display locked to desired mode with no sustained tearing.
- Button behaviors reliable (single-press sleep toggle, double-press shutdown).
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

---

## Dependency upgrade runbook

Use this before merging changes that also touch host dependencies.

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
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo audit
```

If `cargo update -w` introduces regressions, pin only the problematic crates and document why in the commit message.

**5. Re-evaluate git patches** in `Cargo.toml` (`cosmic-text`, `glyphon`). Keep pins only when required for reproducible runtime or compatibility on Pi.

**6. Recovery validation on device** — run the wrong-password recovery, AP outage auto-reconnect, and `WAN down / Wi-Fi up` (no false trigger) checks via `make -f tests/Makefile wifi-recovery`.

---

## Release checklist

Before tagging a release:

1. **Automated checks green** — `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
2. **`cargo audit` clean** — zero vulnerabilities (the `paste` unmaintained warning is acceptable).
3. **Fresh-image smoke on real hardware** — run the [phase checklist](#manual-phase-checklist) on a clean Trixie image, paying special attention to:
   - the kiosk user's `seat` group membership / `/run/seatd.sock` access,
   - power button (single = toggle, double = shutdown),
   - Wi-Fi recovery end-to-end.
4. **Versions & lockfile** — crate `version`s bumped, `Cargo.lock` committed.
5. **Upgrade note** — if unit files, the kiosk user's groups, or tmpfiles changed, existing installs must re-run system provisioning and reboot; call it out in the release notes.
6. **Tag and push.**
