# Photo Frame Project Roadmap

## Tier 1 – Must-do (MVP)

- [ ] **Frame hardware**
  - [ ] Assemble Pi board with cooler + power hat (always-on switch engaged).
  - [ ] Wire GPIO momentary button to Pi (for screen/shutdown).
  - [ ] Connect Pi to Dell monitor (USB-C power, HDMI video).
  - [ ] Design + cut acrylic plates for open case.
  - [ ] Mount Pi assembly behind monitor.
- [ ] **Pi OS**
  - [ ] Flash Raspberry Pi OS to microSD (macOS laptop).
  - [ ] Write setup script: update OS, install required packages.
  - [ ] Install Rust, build Rust project, enable auto-start (systemd service).
  - [ ] Configure button → key events with `gpio-shutdown` overlay.
  - [ ] Add SSH authorized_keys for remote admin via Tailscale.
- [ ] **Rust project core**
  - [ ] Scan photo directories + display scaled images full screen.
  - [x] Handle startup without crashing if photo list is empty/missing.
  - [x] Simple circular buffer of photos (no weighting yet).

## Intermediate Milestone – Cross-platform Image Display

- [ ] **macOS support**
  - [ ] Build and run the application, opening a window and showing a test image.
- [ ] **Raspberry Pi support**
  - [ ] Build and run on Pi with the same viewer path as macOS.
- [ ] **Testing & QA**
  - [ ] Ensure `cargo test` and `cargo clippy -- -D warnings` pass on both platforms.
  - [ ] Document minimal steps to run the demo on each platform.

## Tier 2 – Should-do (reliability & usability)

- [ ] **Frame hardware**
  - [ ] Design + build wooden frame around monitor.
  - [ ] Design + 3D print French cleat wall mount.
  - [ ] Acquire + paint wall channel to hide power cord.
- [ ] **Pi OS**
  - [ ] Automate Tailscale install + login during setup.
  - [ ] Add wifi configuration utility (form → wpa_supplicant update).
- [ ] **Rust project features**
  - [ ] Circular buffer weighting (half-life replication for new photos).
  - [ ] Graceful removal of deleted photos from list.
  - [ ] Randomized list at boot with configurable seed.
  - [ ] Event system:
    - [ ] Short button press → toggle screen.
    - [ ] Long button press → shutdown.
    - [ ] Screen sleep/wake at set times.
    - [ ] Manual cloud sync trigger.

## Tier 3 – Nice-to-have (polish & extras)

- [ ] **Photo rendering**
  - [ ] Matting options:
    - [ ] Fixed color mat (configurable).
    - [ ] Studio mat (average color + textured bevel).
    - [ ] Blur mat (scaled background fill).
    - [ ] Configurable minimum mat size.
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
