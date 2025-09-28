# Photo Frame

A digital photo frame driver implemented in Rust with a pipeline tuned for Raspberry Pi hardware. Watches a photo library, weights the playlist so new images appear more frequently, and renders each slide with configurable matting, transitions, and photo effects.

**Built for:** Raspberry Pi hobbyists, makers or photographers who want a bespoke display, and Rust developers interested in embedded graphics pipelines.

**Highlights:**

- Runs entirely on-device with a configurable playlist weighting system.
- Supports rich visual treatments (mats, transitions, print simulation) without requiring graphics expertise.

## Table of Contents

1. [Hardware](#hardware)
2. [Software Setup](#software-setup)
3. [Features](#features)
4. [Architecture Overview](#architecture-overview)
5. [Configuration](#configuration)
6. [Fabrication](#fabrication)
7. [References](#references)
8. [License](#license)

## Hardware

Plan your build around a Raspberry Pi 5, a portrait-capable 4K monitor, and mounting hardware that hides cables while keeping airflow open. The dedicated hardware guide covers the recommended bill of materials plus optional accessories and planning tips. [Full details →](docs/hardware.md)

## Software Setup

From flashing Raspberry Pi OS to deploying the watcher, hotspot, and sync services, the setup guide walks through every command you need to bring the slideshow online. It also documents CLI flags for local debugging and a quickstart checklist for provisioning. [Full details →](docs/software.md)

### Deployment Pipeline

The provisioning workflow now runs in two explicit stages so you can separate one-time operating-system preparation from rapid application iteration. Each stage is idempotent—safe to re-run whenever you need to update dependencies or redeploy a fresh build.

1. **System prerequisites**

   ```bash
   ./setup/system/system-setup.sh
   ```

   Installs apt dependencies, enables the 4K HDMI boot profile, and bootstraps a user-scoped Rust toolchain (`rustup`, `cargo`, `rustc`). The script only elevates for package-manager and firmware edits and exports `~/.cargo/bin` into `PATH` for subsequent stages.

2. **Application build & install**

   ```bash
   ./setup/app/app-setup.sh
   ```

   Builds the Rust binaries as the invoking user, stages the release artifacts, and installs them into `/opt/photo-frame`. Systemd unit files are copied into `/opt/photo-frame/systemd` and linked into `/etc/systemd/system`, then `photo-frame.service` is enabled and started.

Both stages respect the following knobs:

| Variable | Default | Purpose |
|----------|---------|---------|
| `INSTALL_ROOT` | `/opt/photo-frame` | Installation prefix for binaries, configs, docs, and unit files. |
| `SERVICE_USER` | `photo-frame` | Systemd user that owns `/opt/photo-frame/var` and runs the services. |
| `CARGO_PROFILE` | `release` | Cargo profile used for builds (passed through to `cargo build`). |
| `DRY_RUN` | unset | When set to `1`, modules print the actions they would take instead of executing them. |

After a successful install the filesystem layout is:

```
/opt/photo-frame/
  bin/       # Executables (e.g., rust-photo-frame)
  lib/       # Reserved for shared assets and helper scripts
  etc/       # Read-only configuration templates
  var/       # Writable runtime state owned by ${SERVICE_USER}
  docs/      # Offline documentation and licensing
  systemd/   # Deployed unit files sourced by the installer
```

The `photo-frame.service` unit runs the application with its working directory set to `/opt/photo-frame/var` and automatically restarts on failure. Override configuration can be dropped in `/etc/default/photo-frame` when needed.

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
