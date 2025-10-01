# Raspberry Pi Photo Frame – Developer Test Plan

## Overview & Philosophy
- [ ] **Purpose:** Validate the end-to-end lifecycle of the Raspberry Pi 5 photo frame from clean install through daily operation and recovery.
- [ ] **Scope:** Cover the main-line workflows (install → configure → operate → update/recover). Edge-case permutations, GPU micro-benchmarks, and full security audits are **out of scope**.
- [ ] **Operator:** A developer working directly on the Pi (keyboard/mouse locally or via SSH with physical access when prompted).
- [ ] **Approach:** Prefer observable evidence (logs, screenshots, journal captures). Automate where practical via `tests/run_smoke.sh`, `tests/run_daily.sh`, and `tests/collect_logs.sh`.

## Test Environments & Matrix
Exercise each axis at least once per release cycle.

| Matrix ID | Display Mode | Network | Library Size | Internet Availability | Notes |
|-----------|--------------|---------|--------------|-----------------------|-------|
| M1        | 3840×2160 @ 60 Hz | Ethernet | Tiny (≤50) | Available | Baseline sanity after clean install. |
| M2        | 3840×2160 @ 30 Hz | Wi-Fi | Medium (1–3k) | Available | Stress decode/render pipeline. |
| M3        | 3840×2160 @ 60 Hz | Wi-Fi | Tiny (≤50) | ISP outage (LAN up) | Exercise watcher without WAN. |
| M4        | 3840×2160 @ 60 Hz | Wi-Fi | Medium (1–3k) | Available | Overnight burn-in with sleep schedule. |
| M5        | 3840×2160 @ 60 Hz | Ethernet | Medium (1–3k) | Available | Update + rollback rehearsal. |

## Phase 0 – Pre-flight
- [ ] Confirm hardware power and cooling are connected (Pi 5, active fan, 52Pi PD board, Dell S2725QC on HDMI-1).
- [ ] Run quick thermal check:
  ```sh
  vcgencmd measure_temp
  ```
- [ ] (Optional) Stress-check for 2–3 minutes using `stress-ng --cpu 4` if installed; watch fan response.
- [ ] Verify display handshake via Wayland:
  ```sh
  wlr-randr
  ```
- [ ] If Wayland tooling missing, fallback to DRM:
  ```sh
  cat /sys/class/drm/card*/card*/modes
  sudo modetest -c    # optional, needs kmscube package
  ```
- [ ] Capture evidence via `tests/collect_logs.sh` (provides modes, EDID, journals).

## Phase 1 – Blank-Pi Install
- [ ] Flash the latest Raspberry Pi OS (Bookworm, 64-bit) onto a reliable microSD.
- [ ] On first boot, create user `pi` (or desired kiosk user) with password and enable autologin.
- [ ] Apply OS updates:
  ```sh
  sudo apt update && sudo apt full-upgrade
  ```
- [ ] Install required packages:
  ```sh
  sudo apt install -y git build-essential pkg-config libssl-dev cmake libwayland-client0 wayland-protocols wayland-utils wlr-randr
  ```
- [ ] Confirm Wayland session active (`echo $XDG_SESSION_TYPE` should print `wayland`).
- [ ] Enable kiosk autologin (raspi-config → System Options → Boot / Auto Login → Console Autologin) and set desktop to launch Wayland session.

## Phase 2 – Project Install
- [ ] Clone repo:
  ```sh
  git clone https://github.com/<org>/rust-photo-frame.git
  cd rust-photo-frame
  ```
- [ ] Build release binary (expect ~5–7 minutes on Pi 5):
  ```sh
  cargo build --release
  ```
- [ ] Install optional Google font system-wide (example: Roboto Slab):
  ```sh
  sudo mkdir -p /usr/local/share/fonts/truetype/google
  sudo cp assets/fonts/RobotoSlab-Regular.ttf /usr/local/share/fonts/truetype/google/
  sudo fc-cache -f
  ```
- [ ] Stage a minimal `config.yaml`:
  ```yaml
  fonts:
    primary: "/usr/local/share/fonts/truetype/google/RobotoSlab-Regular.ttf"
  mats:
    default-hex: "#101214"
  sleep-windows:
    - start: "23:30"
      end: "06:30"
      timezone: "America/Los_Angeles"
  library-paths:
    - "/home/pi/photos"
  wifi-watcher:
    check-interval-secs: 15
    retry-limit: 0   # unlimited
  ```
- [ ] Populate photo library folder(s) per matrix scenario.

## Phase 3 – Kiosk Autostart & Services
- [ ] Install systemd unit (example at `setup/photo-app.service` or generated script).
- [ ] Enable + start:
  ```sh
  sudo cp setup/photo-app.service /etc/systemd/system/
  sudo systemctl daemon-reload
  sudo systemctl enable --now photo-app.service
  ```
- [ ] Verify service:
  ```sh
  systemctl status photo-app.service
  journalctl -u photo-app.service -n 200 --no-pager
  ```
- [ ] Reboot (`sudo reboot`) and confirm the app launches full-screen without manual login.

## Phase 4 – Display Mode & Frame Rate
- [ ] Confirm target mode (4K@60 preferred):
  ```sh
  wlr-randr
  ```
- [ ] If forced to fallback, confirm DRM mode lines:
  ```sh
  cat /sys/class/drm/card*/card*/modes
  ```
- [ ] Capture short (≤10 s) phone video to document refresh smoothness.
- [ ] Note any tearing/flicker and add to `docs/test-plan.md` observations section (Appendix B).

## Phase 5 – Button & Power Behavior
- [ ] Identify button input device:
  ```sh
  sudo evtest    # inspect for gpio-keys or power-button device
  ```
- [ ] Short press → expect app sleep toggle (log should show SIGUSR1 or equivalent).
- [ ] Double-click → expect clean shutdown (`shutdown -h now` path confirmed in journal).
- [ ] Long press → document expected behavior (hard-off). **Do not perform if risk of corruption.**
- [ ] Evidence:
  ```sh
  journalctl -u photo-app.service -n 100 --no-pager | grep -i "sleep"
  sudo journalctl -b | grep -i "systemd-logind" | tail -20
  ```

## Phase 6 – Sleep/Dimming Schedule
- [ ] Ensure config sleep window matches local timezone.
- [ ] Manual toggle:
  ```sh
  kill -USR1 $(pidof rust-photo-frame)
  ```
  Confirm screen dims / slideshow pauses; capture journal snippet.
- [ ] Scheduled test: temporarily set sleep window to begin in ~5 minutes, reload config (restart service), observe dim-on schedule and subsequent wake.
- [ ] Evidence: screen photo with timestamp, journal entries referencing scheduler actions.

## Phase 7 – Wi-Fi Provisioning & Watcher
- [ ] Trigger first-run provisioning portal or local page (follow project-specific instructions). Confirm phone/laptop can join hotspot and submit credentials.
- [ ] After provisioning, ensure Pi joins target Wi-Fi (`nmcli dev status`).
- [ ] LAN-up/Internet-down: block WAN (e.g., override DNS `/etc/hosts` or router rule) while keeping Wi-Fi up. Confirm slideshow keeps advancing and `wifi-watcher` logs retries without pausing playback.
- [ ] Wi-Fi down: power off AP or change SSID. Observe watcher retry cadence and any UI hints. Restore AP and ensure automatic reconnection.
- [ ] Evidence:
  ```sh
  nmcli dev status
  journalctl -u wifi-watcher.service -n 200 --no-pager  # if separate unit
  journalctl -u photo-app.service -n 200 --no-pager | grep -i network
  ```

## Phase 8 – Library Ingest & Rendering
- [ ] Point to tiny library (≤50). Validate EXIF orientation, mats, transitions.
- [ ] Swap to medium library (1–3k). Allow 10–15 minutes playback, monitoring CPU/GPU/IO:
  ```sh
  top -H -p $(pidof rust-photo-frame)
  vcgencmd measure_temp
  ```
- [ ] Watch for stutters or decode warnings in `journalctl -u photo-app.service`.
- [ ] Note performance metrics in appendix.

## Phase 9 – Updates & Rollback
- [ ] Simulate release update:
  ```sh
  git fetch origin
  git checkout <release-tag>
  cargo build --release
  sudo systemctl restart photo-app.service
  ```
- [ ] Validate new version via `rust-photo-frame --version` in logs or manual run.
- [ ] Rollback rehearsal: checkout previous known-good commit/tag, rebuild, restart service.
- [ ] Ensure unit file remains untouched (compare with `systemctl cat photo-app.service`).

## Phase 10 – Power Loss & Recovery
- [ ] Use 52Pi PD board (if equipped) to momentarily cut power (per manufacturer safe window).
- [ ] Confirm auto-boot, kiosk autologin, and slideshow resume within expected timeframe.
- [ ] Check for filesystem warnings:
  ```sh
  journalctl -b | grep -i fsck
  dmesg | grep -i error
  ```

## Phase 11 – Diagnostics & Log Bundle
- [ ] Run collector:
  ```sh
  tests/collect_logs.sh
  ```
- [ ] Verify artifact present: `ls artifacts/FRAME-logs-*.tar.gz`.
- [ ] Attach bundle to issue tracker entry with notes on observed behavior.

## Acceptance Criteria
- [ ] Cold boot to slideshow ready in ≤ 45 seconds after login prompt appears.
- [ ] Display locked to desired mode (4K@60 preferred) with no sustained tearing.
- [ ] Button behaviors (short press sleep toggle, double-click shutdown) reliable.
- [ ] Sleep schedule respects configured windows and reacts to manual SIGUSR1 trigger.
- [ ] Wi-Fi provisioning, outage resilience, and watcher recovery all succeed without slideshow halt.
- [ ] Library playback smooth for medium set (≤5% CPU spikes >150% overall, no OOM/IO errors).
- [ ] Updates deploy cleanly and rollback path verified.
- [ ] Power loss recovery returns to slideshow without manual intervention.
- [ ] Diagnostics bundle captures all required evidence.

## Recovery & Rollback Notes
- **Bad config.yaml:**
  - [ ] Restore last-known-good from backup (`cp config.yaml.bak config.yaml`).
  - [ ] Validate YAML syntax with `yamllint` (if installed) or `python3 -c "import yaml,sys; yaml.safe_load(open('config.yaml'))"`.
  - [ ] Restart service: `sudo systemctl restart photo-app.service`.
- **Broken service (fails to start):**
  - [ ] Inspect logs: `journalctl -u photo-app.service -b`.
  - [ ] Rebuild binary: `cargo build --release`.
  - [ ] Validate unit file dependencies (Wayland, env vars) and run `sudo systemctl daemon-reload`.
- **Failed update:**
  - [ ] `git checkout <previous-good>`.
  - [ ] Rebuild + restart service.
  - [ ] If binary corrupted, delete `target/` and rebuild.
- **Network stuck offline:**
  - [ ] Verify Wi-Fi credentials (`nmcli connection show`).
  - [ ] Restart NetworkManager: `sudo systemctl restart NetworkManager`.
  - [ ] Run `tests/collect_logs.sh` and attach bundle to bug report.

## Appendices
### Appendix A – Helper Commands
- Temperature: `vcgencmd measure_temp`
- Display info: `wlr-randr`, `modetest -c`, `cat /sys/class/drm/card*/card*/modes`
- Services: `systemctl status photo-app.service`, `journalctl -u photo-app.service -n 200 --no-pager`
- Network: `nmcli dev status`, `nmcli connection show --active`
- Signals: `kill -USR1 $(pidof rust-photo-frame)`
- Button events: `sudo evtest`

### Appendix B – Observations Log
Use this space to jot down anomalies, thermal readings, or TODOs discovered during testing. Copy into issue tracker alongside log bundle.

- [ ] Date / Tester:
- [ ] Matrix scenario:
- [ ] Findings:

## Test Harness Quick Reference
- [ ] **Smoke (10–15 min):** `tests/run_smoke.sh`
- [ ] **Daily (≤3 min):** `tests/run_daily.sh`
- [ ] **Log bundle:** `tests/collect_logs.sh`
- [ ] **Make targets:**
  ```sh
  make -f tests/Makefile smoke
  make -f tests/Makefile daily
  make -f tests/Makefile collect
  ```
