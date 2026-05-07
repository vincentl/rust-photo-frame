# Build the Frame

Everything to source and assemble before the software install. The hardware decisions (especially cable routing and ventilation) are much easier to handle before the monitor is on the wall.

> **Safety:** This guide describes laser cutting, 3D printing, woodworking, and electrical work. A wall-mounted display in a wooden enclosure can pose long-term fire hazards if thermal management is neglected. Proceed at your own risk.

---

## Bill of materials

| Component | Why it matters |
| --- | --- |
| **Raspberry Pi 5 (4 GiB+)** | 4 GiB handles 1080p with effects; 8 GiB recommended for 4K with heavy matting. Pi 4 is untested. |
| **4K monitor, portrait-capable** | Tested: Dell S2725QC (27", 4K, USB-C, 100mm VESA). HDMI-CEC is *not* supported on this model — use `wlr-randr` for power control. |
| **USB-C power supply** | Pi 5 needs 27W USB-C PD under sustained render load. Use the official Raspberry Pi 27W adapter or equivalent. Phone chargers won't cut it. |
| **HDMI cable** | Short (≤1m), high-quality. Marginal cables show up as flicker or mode-negotiation failures. |
| **High-endurance microSD** | 32 GiB+ for OS + build artifacts + small library. High-endurance cards (dashcam/security-cam grade) survive always-on writes. |
| **Momentary pushbutton** | Optional. Wires to Pi 5 power pads for hardware wake/sleep. See [Power button wiring](#power-button-wiring). |
| **Mounting hardware** | French cleat + VESA bracket + Pi bracket. CAD files in [`../maker/`](../maker/). |

### Optional accessories

| Item | Notes |
| --- | --- |
| Active cooling (heatsink/fan) | Recommended in any sealed enclosure. Pi 5 throttles under sustained GPU load without airflow. |
| Cable channels / sleeves | Hide HDMI and power for a clean wall installation. |
| USB hub | If you'll add a USB drive for local photos or peripherals. |

---

## Power button wiring

The button is **optional**. The frame can be controlled entirely over SSH or via a wake/sleep schedule. If you want a physical button:

**What to use:** a normally-open momentary pushbutton (6mm tactile or a panel-mount button).

**Where it goes:** the Raspberry Pi 5 has a dedicated 2-pin **power button header (J2)** near the USB-C inlet. This is *not* a GPIO pin — it's the same circuit as the button on the official Pi 5 case. Wire your button across pins 1 and 2 of J2. No resistors or capacitors needed; debouncing is handled in software by `buttond`.

**What it does:**

| Press | Action |
| --- | --- |
| Short press (< 3s) | `buttond` toggles wake/sleep via the control socket |
| Long press (≥ 3s) | Pi 5 hardware initiates clean shutdown (`systemctl poweroff`) — works even if the software is unresponsive |

**Skipping the button:** leave J2 unwired and use SSH commands (see [Operate](operate.md)) or an `awake-schedule` (see [Configure](configure.md)) instead. The default `buttond` config has `device: null`, which skips evdev enumeration.

---

## Planning before assembly

- **Map power and network first.** Identify the outlet location and cable path before the frame goes on the wall.
- **Plan service access.** SD card, USB-C power, and the button wire should be reachable without full disassembly.
- **Reserve GPIO.** If you might add sensors or LEDs later, leave header pins free.
- **Thermals matter.** A sealed wooden frame with no ventilation will throttle. Plan a slot near the top for convective airflow (target ≥5–10 cm² minimum opening, more is better).
- **HDMI clearance.** The micro-HDMI connector on the Pi 5 is fragile under side-load. Don't route the cable into a sharp bend.

---

## Physical assembly

The reference build uses a French cleat for mounting, a 3D-printed Pi bracket on the rear of the monitor, and a wooden shadow box around the panel. CAD/laser files live in `../maker/`:

| File | Purpose |
| --- | --- |
| [`../maker/cleat-spacers.scad`](../maker/cleat-spacers.scad) / `.stl` / `.3mf` | French cleat spacer blocks (3D print) |
| [`../maker/cleat-drill-template.svg`](../maker/cleat-drill-template.svg) | Drill template for the cleat bolt pattern |
| [`../maker/pi5_bracket.svg`](../maker/pi5_bracket.svg) | Laser-cut bracket for mounting the Pi 5 to VESA hardware |

### Target hardware

- Monitor depth (rear protrusion): ~30 mm max without the stand
- VESA pattern: 100×100 mm, M4 × 10 mm (shallow — the plastic shell is thin)
- Spacer blocks raise cleat hardware 6 mm off the shell to prevent crushing

### Tools

| Tool | Required? | Notes |
| --- | --- | --- |
| 3D printer (FDM) | Yes | Cleat spacers, optional Pi bracket. PLA fine. |
| Laser cutter | Optional | For the Pi bracket SVG. Drill + jigsaw substitutes. |
| Drill + bits | Yes | M4 clearance for VESA, pilot holes for cleat |
| Pocket-screw jig | Recommended | Frame joints; wood screws are a fallback |
| Miter or circular saw | Yes | Shadow-box lumber |
| Clamps | Yes | Several during glue-up |

### Steps

**1. Print the cleat spacers.** Slice `cleat-spacers.3mf` (or the STL) at 0.2 mm layer height, 40%+ infill, no supports. They sit between the cleat hardware and the monitor's rear shell so clamping force is distributed and the plastic doesn't crush.

**2. Mount the French cleat to the monitor.** Print `cleat-drill-template.svg` at 100% scale and verify 100×100 mm spacing against your monitor before drilling. Install M4 × 10 mm bolts through the cleat and spacers into the VESA holes — snug, not tight. Confirm the monitor hangs plumb on the receiver before finalizing.

**3. Mount the Raspberry Pi.** Cut `pi5_bracket.svg` from 3 mm acrylic or plywood. Mount the Pi with M2.5 standoffs (56×49 mm pattern). Orient the Pi so the USB-C and micro-HDMI ports face the cable routing path. Without a laser cutter, any flat rigid material drilled to the Pi 5 mounting pattern works.

**4. Build the shadow box.** A shallow wooden frame around the monitor that conceals the Pi, cables, and hardware.
- **Depth:** ≥40 mm of clearance behind the panel for the Pi, cables, and air.
- **Ventilation:** convective slot near the top, ≥5–10 cm². More is better.
- **Construction:** 18–22 mm lumber/MDF; pocket screws or biscuits at corners; wood glue. Rabbet the back if you want the monitor flush. Screw (don't glue) the cleat receiver to the back panel so the frame can be removed from the wall.
- **Finish:** satin or matte minimizes glare. Dark finishes downplay the bezel.

**5. Cable management.**
- **HDMI:** ~30 cm micro-HDMI to HDMI, gentle curve.
- **USB-C power:** through a slot in the bottom or back; recessed grommet keeps it tidy.
- **Button wire (if present):** route from J2 to the button cutout. A 2-pin JST or dupont connector at the Pi end makes service easy.
- **Strain relief:** secure cables near connectors with ties or adhesive mounts so they don't flex at the plug.

**6. Dry-fit, power on, verify.** Before any final gluing or finishing:
1. Hang the monitor on the wall cleat.
2. Attach the Pi bracket and route all cables with the back panel off.
3. Run the [software install](install.md) to completion.
4. Run `vcgencmd measure_temp` after 30 minutes of cycling — should stay well below 80°C with any airflow.
5. Confirm the Wi-Fi recovery hotspot raises (see [Operate › Wi-Fi recovery validation](operate.md#wi-fi-recovery-validation)).
6. Close the back panel.

---

## Assembly checklist

- [ ] Cleat spacers printed and fit-checked on VESA holes
- [ ] French cleat mounted; monitor hangs plumb
- [ ] Pi bracket cut and Pi mounted
- [ ] HDMI cable connected with strain relief
- [ ] USB-C power routed and secured
- [ ] Button wire routed (if using)
- [ ] Shadow box built; ventilation slot present
- [ ] Software fully installed and tested before closing the frame
- [ ] Thermal check passed (< 80°C under load)
- [ ] Back panel installed
