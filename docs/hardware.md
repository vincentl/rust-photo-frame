# Hardware

## Overview
The photo frame targets Raspberry Pi hardware and a wall-mounted display. This guide collects the bill of materials and planning notes so you can source parts with confidence before starting the software setup.

## Core components

Gather these before starting software setup.

1. Raspberry Pi 5 (4 GiB+)
2. Portrait-capable 4K monitor
3. Stable USB-C power supply for Pi 5 load
4. HDMI cable (short/high quality)
5. High-endurance microSD card
6. Normally-open momentary pushbutton for Pi 5 power pads
7. Mounting method + frame/enclosure plan

| Component | Why it matters |
| --- | --- |
| Raspberry Pi 5 (4 GiB+) | Provides GPU and memory headroom for transitions and decode workload. |
| 4K monitor | Preserves detail and gives flexibility for portrait installations. |
| Power supply | Prevents undervoltage instability under sustained rendering load. |
| Momentary power button | Enables sleep/shutdown control while preserving long-press hardware cutoff. |
| HDMI cable | Carries reliable display signal; shorter runs reduce signal issues. |
| High-endurance SD card | Improves longevity for always-on deployments. |
| Mounting + enclosure plan | Drives cable routing, thermals, and serviceability. |

## Optional accessories

- Active cooling (fan or heatsink) for enclosures with limited airflow.
- Cable sleeves or channels to hide HDMI and power lines.
- VESA mount hardware for flush wall installations.

## Planning tips

- Map power and network access before assembly; pre-routed cables and nearby outlets simplify final installation.
- Reserve GPIO access if you plan to add sensors, status LEDs, or extra buttons later.
- Plan enclosure passthrough points for future wiring so upgrades do not require disassembling the frame.
- Keep thermals in mind early; tightly sealed enclosures need deliberate airflow strategy.
- Keep service access to SD card, power connector, and button wiring for maintenance.
