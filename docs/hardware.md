# Hardware Guide

Everything you need to source before starting the software setup. Take care of this first — it's much easier to route cables and plan enclosures before assembly than after.

---

## Bill of materials

| Component | Why it matters |
| --- | --- |
| **Raspberry Pi 5 (4 GiB+)** | 4 GiB handles 1080p with effects; 8 GiB is better for 4K or heavy matting. Pi 4 may work but is untested — GPU-intensive transitions may perform differently. |
| **4K monitor, portrait-capable** | Tested: Dell S2725QC (27", 4K, USB-C). Note: HDMI-CEC is not supported on this model — `cec-ctl` cannot power it down. Use `wlr-randr` for power control instead. |
| **USB-C power supply** | The Pi 5 needs a 27W USB-C PD supply under sustained rendering load. The official Raspberry Pi 27W adapter works. Standard phone chargers are not sufficient. |
| **HDMI cable** | Short runs (under 1m) with a quality cable are most reliable. Marginal cables show up as display flicker or mode negotiation failures. |
| **High-endurance microSD** | 32 GiB+ for the OS + build artifacts + a small photo library. High-endurance cards (marketed for dashcams or security cameras) handle the write cycles of an always-on system better than consumer cards. |
| **Momentary pushbutton** | Optional — wires to Pi 5 power pads for hardware wake/sleep. See [Power button wiring](#power-button-wiring) below. |
| **Mounting + enclosure** | Drives cable routing, thermals, and serviceability. See [maker/fabrication.md](../maker/fabrication.md) for a tested French-cleat build. |

---

## Power button wiring

The hardware button is **optional**. You can wake and sleep the frame using SSH commands at any time; see [docs/quick-reference.md](quick-reference.md). If you want a physical button, here's how it works.

### What kind of button

A **normally-open momentary pushbutton** — one that makes contact only while pressed, then springs back open. Momentary switches from any electronics supplier work fine; common sizes are 6mm tactile switches or panel-mount buttons for enclosure mounting.

### Where it connects on the Pi 5

The Raspberry Pi 5 has a dedicated **power button header (J2)**, a two-pin header near the USB-C power connector. This is not a GPIO pin — it's the same circuit as the button on the Pi 5 official case.

- **Pin 1 and Pin 2** of the J2 header — wire your button across these two pins.
- No resistors, no capacitors, no additional components needed.
- The button is debounced in software by `buttond` (configurable via `buttond.debounce-ms` in `config.yaml`).

### What the button does

| Press | Action |
| --- | --- |
| Short press (< 3 seconds) | `buttond` toggles wake/sleep state via the control socket |
| Long press (≥ 3 seconds) | Pi 5 hardware initiates a clean shutdown (`systemctl poweroff`) |

The long-press shutdown is handled by the Pi 5 hardware, not by `buttond`. It works even if the software is unresponsive.

### Finding J2 on the board

J2 is a 2-pin 1.27mm pitch header near the USB-C power inlet on the Raspberry Pi 5. It is labeled `J2` on the PCB silkscreen. On a bare Pi 5 board without headers soldered, you can attach a button wire directly to the pads or solder a connector.

### Skipping the button entirely

You can operate the frame with no button at all. Use the wake/sleep commands from an SSH session, or configure an `awake-schedule` in `config.yaml` to drive sleep automatically. The `buttond` daemon runs regardless — it handles the schedule even without a physical button connected.

If you don't have a button and don't want `buttond` searching for an evdev device, set `device: null` in the `buttond` config block (that's the default) and it will skip device enumeration.

---

## Optional accessories

| Item | Notes |
| --- | --- |
| Active cooling | A heatsink or fan is recommended for enclosures with limited airflow. The Pi 5 throttles under sustained GPU load without adequate cooling. |
| Cable sleeves / channels | Hide HDMI and power lines for a clean wall installation. |
| VESA mount hardware | 75mm or 100mm VESA adapters for flush wall mounting. The Dell S2725QC uses 100mm VESA. |
| USB hub | If you plan to add a USB drive for local photos or extra peripherals. |

---

## Planning tips

- **Map power and network before assembly.** Identify the outlet location and cable path before the frame goes on the wall. Running cables after the fact is frustrating.
- **Reserve GPIO access.** If you might add sensors, status LEDs, or additional buttons later, leave those header pins free during initial assembly.
- **Plan enclosure passthrough points.** Design cable exits into the frame so upgrades don't require disassembly.
- **Thermals matter.** A tightly sealed wooden frame with no ventilation will cause thermal throttling. Plan a gap or slot near the top of the enclosure for convective airflow.
- **Keep service access.** The SD card, USB-C power connector, and button wire should be accessible without fully disassembling the frame.
- **HDMI connector clearance.** Check that the HDMI cable can route without a sharp bend near the Pi — micro-HDMI connectors on the Pi 5 are fragile under side-load.

---

## Where to go next

- Physical build: [maker/fabrication.md](../maker/fabrication.md)
- Software install: [docs/installation.md](installation.md)
- Power and sleep configuration: [docs/power-and-sleep.md](power-and-sleep.md)
