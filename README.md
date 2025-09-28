# Photo Frame

A digital photo frame driver implemented in Rust with a pipeline tuned for Raspberry Pi hardware. Watches a photo library, weights the playlist so new images appear more frequently, and renders each slide with configurable matting, transitions, and photo effects.

**Built for:** Raspberry Pi hobbyists, makers or photographers who want a bespoke display, and Rust developers interested in embedded graphics pipelines.

**Highlights:**

- Runs entirely on-device with a configurable playlist weighting system.
- Supports rich visual treatments (mats, transitions, print simulation) without requiring graphics expertise.

## Table of Contents

1. [Hardware](#hardware)
2. [Software Setup](#software-setup)
3. [Wi-Fi Recovery & Provisioning](#wi-fi-recovery--provisioning)
4. [Features](#features)
5. [Architecture Overview](#architecture-overview)
6. [Configuration](#configuration)
7. [Fabrication](#fabrication)
8. [References](#references)
9. [License](#license)

## Hardware

Plan your build around a Raspberry Pi 5, a portrait-capable 4K monitor, and mounting hardware that hides cables while keeping airflow open. The dedicated hardware guide covers the recommended bill of materials plus optional accessories and planning tips. [Full details →](docs/hardware.md)

## Software Setup

From flashing Raspberry Pi OS to deploying the watcher, hotspot, and sync services, the setup guide walks through every command you need to bring the slideshow online. It also documents CLI flags for local debugging and a quickstart checklist for provisioning. [Full details →](docs/software.md)

## Wi-Fi Recovery & Provisioning

The frame includes a self-contained Wi-Fi manager that watches connectivity, launches a recovery hotspot, and writes fresh credentials into NetworkManager without manual SSH access.

- **Automatic hotspot.** If the frame has been offline for more than 30 seconds, `wifi-manager` brings up an access point named **PhotoFrame-Setup** secured with a random three-word passphrase. The active passphrase is saved to `/opt/photo-frame/var/hotspot-password.txt` and mirrored in the QR code at `/opt/photo-frame/var/wifi-qr.png`.
- **Guided web UI.** Join the hotspot (the frame shows the QR code) and browse to [http://192.168.4.1:8080/](http://192.168.4.1:8080/) to reach a single-page setup form. Submit the target SSID and password; the page polls status until the frame rejoins your network.
- **Seamless NetworkManager updates.** Credentials are written via `nmcli` and the hotspot is torn down only after the frame obtains an IP address on the new network. Last-attempt diagnostics live in `/opt/photo-frame/var/wifi-last.json` for remote support.
- **Service management.** The systemd unit (`wifi-manager.service`) runs as the `photo-frame` user on boot. Tail logs with `journalctl -u wifi-manager -f` to watch state transitions such as `ONLINE → OFFLINE` or provisioning progress.
- **Configuration.** Tune behaviour in `/opt/photo-frame/etc/wifi-manager.yaml` (grace period, hotspot SSID, UI port). Update the template, then restart with `sudo systemctl restart wifi-manager`.
- **Manual overrides.** When direct shell access is available, you can run `sudo -u photo-frame /opt/photo-frame/bin/wifi-manager nm add --ssid "MyNetwork" --psk "password123"` to seed a connection, or `nmcli connection up pf-hotspot` to force the recovery hotspot.

## Features

- Recursively scans a configurable photo library directory
  - Detects changes from external synchronization processes
    - Automatically adds new photos to the playlist
    - Removes deleted photos from the playlist
  - Prioritizes newer photos with user-configurable display rates
- Configurable matting, transitions, and photo effects
- Supports multiple image formats: JPG, PNG, GIF, WebP, BMP, TIFF
- Robust error handling with structured logging

## Architecture Overview

Curious how the frame stays responsive? This optional deep dive outlines the async tasks and their communication patterns. Skip ahead to [Configuration](#configuration) if you just want to tune the experience.

The runtime is composed of five asynchronous tasks orchestrated by `main.rs`. They communicate over bounded channels to keep memory predictable and to respect GPU/CPU parallelism limits.

```mermaid
flowchart LR
  MAIN[Main] --> FILES[PhotoFiles]
  MAIN --> MAN[PhotoManager]
  MAIN --> LOAD[PhotoLoader]
  MAIN --> AFFECT[PhotoAffect]
  MAIN --> VIEW[PhotoViewer]

  FILES -->|inventory updates| MAN
  MAN -->|invalid photo| FILES
  MAN -->|photo requests| LOAD
  LOAD -->|decoded image| AFFECT
  AFFECT -->|processed image| VIEW
  LOAD -->|invalid photo| FILES
  VIEW -->|displayed event| MAN
```

## Configuration

All configuration options—from playlist weighting and greeting screens to transition tuning—are documented in depth, including starter YAML examples and per-key reference tables. [Full details →](docs/configuration.md)

## Fabrication

Plan the physical build of the frame with dedicated fabrication guidance that covers laser cutting, 3D-printed brackets, cabinetry, and a final assembly checklist. [Full details →](docs/fabrication.md)

## References

- **Procedural studio mat weave texture.** Our weave shading is adapted from Mike Cauchi’s breakdown of tillable cloth shading, which layers sine-profiled warp/weft threads with randomized grain to keep the pattern from banding. See ["Research – Tillable Images and Cloth Shading"](https://www.mikecauchiart.com/single-post/2017/01/23/research-tillable-images-and-cloth-shading).
- **Print simulation shading.** The gallery-lighting and relief model follows guidance from Rohit A. Patil, Mark D. Fairchild, and Garrett M. Johnson’s paper ["3D Simulation of Prints for Improved Soft Proofing"](https://doi.org/10.1117/12.813471).

## License

This project is licensed under the **MIT License**.
See the [LICENSE](LICENSE) file for full text.

© 2025 Vincent Lucarelli
