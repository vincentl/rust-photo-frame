# Photo Frame Setup Pipeline

This directory houses idempotent provisioning scripts for Raspberry Pi photo frame deployments. Each stage can be re-run safely after OS updates or image refreshes.

If you are installing from a blank microSD card, start with the operator runbook in [`../docs/software.md`](../docs/software.md). This file focuses on setup script behavior and internals.

## Standard deployment flow

Use this sequence for a typical deployment:

```bash
./setup/install-all.sh
./setup/tools/verify.sh
sudo ./setup/system/tools/diagnostics.sh
```

`install-all.sh` provisions the OS (sudo), builds/deploys the app as your unprivileged user, and activates kiosk services.

Set `CARGO_BUILD_JOBS` to cap build parallelism on low-memory devices.

## System provisioning (Trixie)

Provision Raspberry Pi OS (Trixie) for kiosk duty and install shared dependencies with:

```bash
sudo ./setup/system/install.sh
```

Run this before building so toolchain and kiosk dependencies are ready. After application deploy, re-running system provisioning is optional but safe.

## Diagnose kiosk health

Inspect the greetd session, kiosk user, and display-manager wiring:

```bash
sudo ./setup/system/tools/diagnostics.sh
```

Run this from the device when display login fails or the kiosk session will not start; it flags missing packages, disabled units, and other common misconfigurations.

## Application deployment

Build and install release artifacts from an unprivileged shell:

```bash
./setup/application/deploy.sh
```

The application stage compiles the workspace, stages binaries and documentation under `setup/application/build/stage`, ensures the kiosk service user exists, and installs the artifacts into `/opt/photo-frame`.

## Operator quick reference

- Daily operations and triage: [`../docs/sop.md`](../docs/sop.md)
- Fresh-install + Wi-Fi recovery validation: [`../docs/software.md`](../docs/software.md)
- Full validation matrix for release testing: [`../developer/test-plan.md`](../developer/test-plan.md)

The kiosk account is unprivileged; use your operator account (for example `frame`) for maintenance commands.

## System provisioning actions

`./setup/system/install.sh` executes numbered modules and performs these key actions:

- Installs base apt packages (graphics stack, build tools, networking utilities).
- Installs or updates system-wide Rust toolchain under `/usr/local/cargo`.
- Sets `dtoverlay=vc4-kms-v3d-pi5,cma-512` in boot config for GPU CMA.
- Replaces legacy swapfile with `systemd-zram-generator` (half-RAM zram target).
- Verifies Debian 13 (Trixie) and applies Pi 5 firmware tweaks.
- Ensures locked `kiosk` user with `render`, `video`, and `input` group membership.
- Provisions runtime directories and polkit policy.
- Installs greetd configuration and kiosk wrapper at `/usr/local/bin/photoframe-session`.
- Deploys `photoframe-*` systemd units and enables/starts them when binaries are present.

Set `ENABLE_4K_BOOT=0` before running if you need to skip the 4K60 profile during provisioning.

## Idempotency notes

- Re-running system provisioning after OS or app updates is supported.
- Application deploy (`./setup/application/deploy.sh`) already installs/updates app unit files and starts kiosk services, so re-running system provisioning is optional for normal deploys.

## Shared systemd helper library

Provisioning and diagnostics scripts use `setup/lib/systemd.sh`.

Canonical source pattern:

```bash
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib/systemd.sh
source "${SCRIPT_DIR}/../lib/systemd.sh"
```

Core helper groups:

- Availability and reload:
  - `systemd_available`, `systemd_daemon_reload`
- Unit lifecycle:
  - `systemd_enable_unit`, `systemd_start_unit`, `systemd_restart_unit`, `systemd_stop_unit`
- Enable/disable/mask patterns:
  - `systemd_enable_now_unit`, `systemd_disable_unit`, `systemd_disable_now_unit`, `systemd_mask_unit`, `systemd_unmask_unit`, `systemd_set_default_target`
- State/metadata checks:
  - `systemd_unit_exists`, `systemd_is_active`, `systemd_is_enabled`, `systemd_status`, `systemd_unit_property`
- Unit/drop-in management:
  - `systemd_install_unit_file`, `systemd_install_dropin`, `systemd_remove_dropins`
