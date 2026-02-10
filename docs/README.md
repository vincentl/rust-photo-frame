# Documentation Guide

Use this page as the map for all Photo Frame documentation.

## Reading paths

### Operator

- Fresh install from blank microSD: [`software.md`](software.md)
- Software install appendix (toolchain/OOM/layout/env): [`software-notes.md`](software-notes.md)
- Day-2 operations and incident triage: [`sop.md`](sop.md)
- Power/sleep tuning and diagnostics: [`power-and-sleep.md`](power-and-sleep.md)
- Power/sleep hardware notes and troubleshooting appendix: [`power-and-sleep-notes.md`](power-and-sleep-notes.md)
- Runtime configuration reference: [`configuration.md`](configuration.md)
- Copy/paste configuration recipes: [`configuration-examples.md`](configuration-examples.md)

### Maintainer

- Kiosk stack architecture and greetd/Sway wiring: [`kiosk.md`](kiosk.md)
- Kiosk provisioning internals and deep verification notes: [`kiosk-notes.md`](kiosk-notes.md)
- Wi-Fi manager behavior, config, and troubleshooting: [`wifi-manager.md`](wifi-manager.md)
- Wi-Fi manager service operations and deep troubleshooting: [`wifi-manager-operations.md`](wifi-manager-operations.md)
- Setup pipeline internals appendix: [`../setup/NOTES.md`](../setup/NOTES.md)

### Developer

- Full system validation matrix: [`../developer/test-plan.md`](../developer/test-plan.md)
- Advanced kiosk/Sway debugging workflows: [`../developer/kiosk-debug.md`](../developer/kiosk-debug.md)

### Maker

- Hardware BOM and planning: [`hardware.md`](hardware.md)
- Hardware accessories and planning appendix: [`hardware-notes.md`](hardware-notes.md)
- Physical build/fabrication: [`../maker/fabrication.md`](../maker/fabrication.md)

## Canonical ownership

### Operator-owned runbooks

- Installation workflow and first boot checks: [`software.md`](software.md)
- Install edge cases and advanced notes: [`software-notes.md`](software-notes.md)
- Routine runtime operations and recovery triage: [`sop.md`](sop.md)
- Display power/sleep setup: [`power-and-sleep.md`](power-and-sleep.md)
- Display power/sleep advanced notes: [`power-and-sleep-notes.md`](power-and-sleep-notes.md)
- Runtime configuration reference and examples: [`configuration.md`](configuration.md), [`configuration-examples.md`](configuration-examples.md)

### Maintainer-owned references

- Kiosk stack advanced notes: [`kiosk-notes.md`](kiosk-notes.md)
- Wi-Fi manager internals and service mechanics: [`wifi-manager.md`](wifi-manager.md)
- Wi-Fi manager operational runbook: [`wifi-manager-operations.md`](wifi-manager-operations.md)
- Setup script internals and module behavior: [`../setup/README.md`](../setup/README.md)
- Setup helper-library and module appendix: [`../setup/NOTES.md`](../setup/NOTES.md)

### Developer-owned references

- Exhaustive release validation checklist: [`../developer/test-plan.md`](../developer/test-plan.md)
- Advanced kiosk/Sway debugging workflows: [`../developer/kiosk-debug.md`](../developer/kiosk-debug.md)

### Maker-owned references

- Hardware planning and accessory notes: [`hardware-notes.md`](hardware-notes.md)
- Physical build/fabrication: [`../maker/fabrication.md`](../maker/fabrication.md)

## Account glossary

- Operator account: your normal SSH/admin user (example: `frame`).
- Service account: `kiosk` (runs greetd session and system services).

Use the operator account for maintenance commands and `sudo` when required. Use the service account only when a command must run in the kiosk runtime context.
