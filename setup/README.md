# Photo Frame Setup Pipeline

This directory houses idempotent provisioning scripts for Raspberry Pi photo frame deployments. Each stage can be re-run safely after OS updates or image refreshes.

If you are installing from a blank microSD card, start with the operator runbook in [`../docs/software.md`](../docs/software.md). This file focuses on setup script behavior and internals.

## Fast path

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

## Advanced notes

For module-level internals, idempotency behavior, and systemd helper library details, use [`NOTES.md`](NOTES.md).
