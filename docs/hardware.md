# Hardware

## Overview
The photo frame targets Raspberry Pi hardware and a wall-mounted display. This guide collects the bill of materials and planning notes so you can source parts with confidence before starting the software setup.

## Core components
- **Raspberry Pi 5 (4 GiB or higher).** Provides the GPU performance and memory headroom needed to render transitions smoothly.
- **4K monitor.** Any portrait-capable monitor with solid viewing angles works. Aim for thin bezels if you plan to hide the display behind a custom frame.
- **Power supply.** Use an official Pi PSU or a USB-C power brick that can sustain the Pi 5 under load.
- **HDMI cable.** Connects the Pi to the display. Keep runs short to avoid signal degradation.
- **High-endurance SD card.** Stores the operating system, binaries, and cached thumbnails.
- **Mounting plan.** Decide how you will secure the Pi behind the screen and route power safely.
- **Frame or enclosure.** Design a bezel that hides display edges and matches the décor.

## Optional accessories
- Active cooling (fan or heatsink) for enclosures with limited airflow.
- Cable sleeves or channels to hide HDMI and power lines.
- VESA mount hardware for flush wall installations.

## Planning tips
Map out power and network access before assembly—routed cables and a nearby outlet make final installation much easier. If you intend to add sensors, buttons, or LEDs, reserve GPIO access and plan passthrough holes in your enclosure so future wiring does not require disassembling the frame.
