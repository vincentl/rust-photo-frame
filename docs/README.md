# Documentation Guide

Use this page as the map for Photo Frame documentation.

## First-time path (in order)

If this is your first time setting up the project, read these in sequence:

1. Hardware planning and bill of materials: [`hardware.md`](hardware.md)
2. Full Raspberry Pi provisioning and install workflow: [`software.md`](software.md)
3. Runtime tuning and YAML reference: [`configuration.md`](configuration.md)
4. Daily operations and troubleshooting: [`sop.md`](sop.md)

## Start here by role

### Operator

1. Fresh install from blank microSD: [`software.md`](software.md)
2. Day-2 operations and incident triage: [`sop.md`](sop.md)
3. Wi-Fi recovery behavior and troubleshooting: [`wifi-manager.md`](wifi-manager.md)
4. Display power/sleep setup and diagnostics: [`power-and-sleep.md`](power-and-sleep.md)
5. Runtime configuration reference and examples: [`configuration.md`](configuration.md)

### Maintainer

1. Kiosk stack architecture and provisioning details: [`kiosk.md`](kiosk.md)
2. Setup pipeline internals and module behavior: [`../setup/README.md`](../setup/README.md)
3. Wi-Fi manager service lifecycle and runbooks: [`wifi-manager.md`](wifi-manager.md)

### Developer

1. Full system validation matrix: [`../developer/test-plan.md`](../developer/test-plan.md)
2. Advanced kiosk/Sway debugging workflows: [`../developer/kiosk-debug.md`](../developer/kiosk-debug.md)

### Maker

1. Hardware BOM, accessories, and planning: [`hardware.md`](hardware.md)
2. Physical build/fabrication: [`../maker/fabrication.md`](../maker/fabrication.md)

## Canonical docs

- Install + provisioning workflow: [`software.md`](software.md)
- Runtime operations SOP: [`sop.md`](sop.md)
- Wi-Fi recovery architecture + operations: [`wifi-manager.md`](wifi-manager.md)
- Kiosk stack reference: [`kiosk.md`](kiosk.md)
- Display power/sleep guide: [`power-and-sleep.md`](power-and-sleep.md)
- Runtime configuration (reference + examples): [`configuration.md`](configuration.md)
- Setup script internals: [`../setup/README.md`](../setup/README.md)
- Hardware planning: [`hardware.md`](hardware.md)

## Account glossary

- Operator account: your normal SSH/admin user (example: `frame`).
- Service account: `kiosk` (runs greetd session and system services).

Use the operator account for maintenance commands and `sudo` when required. Use the service account only when a command must run in the kiosk runtime context.
