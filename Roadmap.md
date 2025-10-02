# Photo Frame Project Roadmap

## Software Subproject

- [ ] **Remote administration & observability**
  - [ ] Bundle Tailscale install + login flow into the setup script.
  - [ ] Harden SSH: authorized_keys provisioning, disable password auth, document recovery.
  - [ ] Capture baseline diagnostics (journalctl, `tail -f` scripts) for remote troubleshooting.
  - [ ] Document day-two ops playbook: reboot, service restart, system updates.
- [ ] **Power & button control**
  - [ ] Implement GPIO daemon translating short/long press into viewer events.
  - [ ] Wire short press to screen sleep/wake handling in the app.
  - [ ] Wire long press to an ordered shutdown (viewer → manager → OS `poweroff`).
  - [ ] Add async test coverage for button → event propagation and debounce timing.
- [ ] **Content synchronization**
  - [ ] Choose cloud storage target + auth strategy (fileshare vs API).
  - [ ] Design sync cadence: periodic pull, manual trigger, conflict handling policy.
  - [ ] Implement sync worker (hashing, temp staging, graceful failover when offline).
  - [ ] Surface manual sync trigger (button event + future web UI hook).
- [ ] **Configuration & UX**
  - [ ] Expand config schema to cover matting, schedules, sync, admin settings.
  - [ ] Define UX flows for first-run wifi setup and device naming.
- [ ] **Deployment automation**
  - [ ] Systemd unit files for manager, viewer, and background sync services.
  - [ ] End-to-end setup validation script (lint config, check dependencies, smoke test viewer).

## Setup Roadmap

This roadmap tracks the tasks required to automate the provisioning of a Raspberry Pi for the rust-photo-frame project. Checkboxes are used to indicate completion status.

### Completed in this iteration

- [x] Establish initial `setup/` directory structure and module loader scaffolding.
- [x] Draft initial instructions covering Raspberry Pi imaging, SSH setup, repository cloning, and invoking automation scripts.
- [x] Create base setup modules for OS updates, display boot configuration, and Rust installation.

### TODO

- [ ] Document kiosk mode configuration and automation of the `cage@tty1.service` flow.
- [ ] Implement background sync service configuration from cloud storage providers.
- [ ] Add setup module for button monitor service deployment and systemd integration.
- [ ] Add setup module for Wi-Fi connectivity checks and captive portal configuration workflow.
- [ ] Create module for Tailscale installation and registration guidance.
- [ ] Provide lightweight web GUI for local configuration editing with Git-based versioning.
- [ ] Automate configuration-driven restarts of the rust-photo-frame application.
- [ ] Expand testing and validation steps for the full provisioning pipeline.

## Tier 1 – Must-do (MVP)

- [ ] **Frame hardware**
  - [x] Assemble Pi board with cooler + power hat (always-on switch engaged).
  - [x] Wire GPIO momentary button to Pi (for screen/shutdown).
  - [x] Connect Pi to Dell monitor (USB-C power, HDMI video).
  - [ ] Design + cut acrylic plates for open case.
  - [ ] Mount Pi assembly behind monitor.
- [ ] **Pi OS**
  - [x] Flash Raspberry Pi OS to microSD (macOS laptop).
  - [ ] Write setup script: update OS, install required packages.
  - [ ] Install Rust, build Rust project, enable auto-start (systemd service).
  - [ ] Configure button → key events with `gpio-shutdown` overlay.
  - [ ] Add SSH authorized_keys for remote admin via Tailscale.
- [ ] **Rust project core**
  - [x] Scan photo directories + display scaled images full screen.
  - [x] Handle startup without crashing if photo list is empty/missing.
  - [x] Simple circular buffer of photos (no weighting yet).

## Intermediate Milestone – Cross-platform Image Display

- [x] **GPU viewer shows decoded photo**
  - [x] Upload `PreparedImageCpu` to a wgpu texture and render a full-screen quad.
  - [x] Unit test verifies EXIF orientation is applied during load.
- [x] **macOS demo**
  - [x] Build & run on macOS, confirming a window renders the first photo.
  - [x] Document minimal run steps and dependencies.
- [x] **Raspberry Pi demo**
  - [x] Build & run the same viewer code on Raspberry Pi.
  - [x] Document any Pi-specific configuration.
- [x] **Quality gates**
  - [x] Keep `cargo build`, `cargo clippy -- -D warnings`, and `cargo test` clean.

## Tier 2 – Should-do (reliability & usability)

- [ ] **Frame hardware**
  - [ ] Design + build wooden frame around monitor.
  - [ ] Design + 3D print French cleat wall mount.
  - [ ] Acquire + paint wall channel to hide power cord.
- [ ] **Pi OS**
  - [ ] Automate Tailscale install + login during setup.
  - [ ] Add wifi configuration utility (form → wpa_supplicant update).
- [ ] **Rust project features**
  - [x] Circular buffer weighting (half-life replication for new photos).
    - [x] Exponential half-life weighting keeps recent additions repeating while ensuring every photo appears each cycle.
  - [x] Graceful removal of deleted photos from list.
  - [x] Randomized list at boot with configurable seed.
  - [ ] Event system:
    - [ ] Short button press → toggle screen.
    - [ ] Long button press → shutdown.
    - [ ] Screen sleep/wake at set times.
    - [ ] Manual cloud sync trigger.

## Tier 3 – Nice-to-have (polish & extras)

- [ ] **Photo rendering**
  - [ ] Matting options:
    - [x] Fixed color mat (configurable).
    - [x] Studio mat (average color + textured bevel).
    - [x] Blur mat (scaled background fill).
    - [x] Configurable minimum mat size.
    - [x] Fixed background image that is scaled to fit screen and images are overlayed
- [ ] **User web interface**
  - [ ] Local web server for configuration (cloud, mats, screen schedule, photo timing).
  - [ ] Access limited to local network.
  - [ ] Display QR code sticker with config URL.
  - [ ] Photo delay options (fixed seconds or Poisson distribution).
- [ ] **Wifi setup polish**
  - [ ] Host AP if no wifi on boot.
  - [ ] Serve SSID/password form over local http.
  - [ ] mDNS for `frame.local`.
  - [ ] Display QR code with access URL.
