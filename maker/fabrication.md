# Fabricating a Framed Display

> **Safety disclaimer**
>
> This guide describes laser cutting, 3D printing, woodworking, and electrical work. These activities carry inherent risks of injury, fire, and property damage. A wall-mounted display in a wooden enclosure can pose long-term fire hazards if thermal management is neglected.
>
> Follow all applicable safety guidelines and proceed **at your own risk**. This information is provided for educational purposes only; the authors assume no responsibility for outcomes.

---

## What's in this directory

| File | Purpose |
| --- | --- |
| `cleat-spacers.scad` | OpenSCAD source for the French cleat spacer blocks |
| `cleat-spacers.stl` | STL for 3D printing — ready to slice |
| `cleat-spacers.3mf` | 3MF version for slicers that prefer it (e.g. Bambu Studio) |
| `cleat-drill-template.svg` | Drill template for the French cleat bolt pattern on the monitor |
| `pi5_bracket.svg` | Laser-cut bracket for mounting the Raspberry Pi 5 to VESA hardware |

---

## Target hardware

This build is designed around the **Dell S2725QC** (27", 4K, USB-C). The VESA hole pattern is 100×100mm. Key physical constraints:

- Monitor depth (rear protrusion): ~30mm max without stand
- VESA screw size: M4 × 10mm (shallow — the plastic shell is thin)
- French cleat bolt clearance: the spacer blocks in `cleat-spacers.3mf` raise the cleat hardware 6mm off the shell to prevent crushing the plastic

---

## Tools needed

| Tool | Required? | Notes |
| --- | --- | --- |
| 3D printer (FDM) | Required | For cleat spacers and Pi bracket. PLA works fine. |
| Laser cutter | Optional | For Pi bracket (SVG). A drill and jigsaw can substitute. |
| Drill + bits | Required | M4 clearance holes for VESA, pilot holes for cleat |
| Pocket-screw jig | Recommended | For frame joints; wood screws are a fallback |
| Miter saw or circular saw | Required | For shadow-box lumber |
| Clamps | Required | Several during glue-up |

---

## Step 1 — Print the cleat spacers

Print `cleat-spacers.3mf` (or use the STL). These are simple blocks — any material works, but PLA is fine. Recommended print settings:

- Layer height: 0.2mm
- Infill: 40%+
- No supports needed

**Why they exist:** The French cleat bolts thread into the M4 VESA holes. Without spacers, tightening the nuts crushes the monitor's plastic rear shell. The spacers sit between the cleat hardware and the shell surface so the clamping force is distributed.

---

## Step 2 — Mount the French cleat to the monitor

1. Print `cleat-drill-template.svg` at 100% scale and verify the 100×100mm hole spacing against your monitor's VESA pattern before drilling.
2. Position the cleat bracket over the VESA holes with spacers inserted.
3. Install M4 × 10mm bolts through the cleat and spacers into the VESA holes. Snug but do not overtighten — the shell is plastic.
4. Check that the monitor hangs plumb on the cleat receiver before finalizing placement.

---

## Step 3 — Mount the Raspberry Pi

The `pi5_bracket.svg` is a flat bracket designed to clamp the Pi 5 to VESA hardware or the display chassis. Cut from 3mm acrylic or 3mm plywood on a laser cutter.

- Four M2.5 standoffs mount the Pi to the bracket
- The bracket has slots that fit over standard M4 hardware
- Orient the Pi so the USB-C power port and micro-HDMI ports face toward the cable routing path

If you don't have a laser cutter, any flat rigid material with M2.5 mounting holes at the Pi 5 bolt pattern (56×49mm) works.

---

## Step 4 — Build the shadow box

The shadow box is a shallow wooden frame that surrounds the monitor and conceals the Pi, cables, and mounting hardware.

**Depth:** design for at least 40mm of clearance behind the monitor panel to accommodate the Pi, cables, and air circulation.

**Ventilation:** the Pi 5 under rendering load generates meaningful heat. Add a slot or gap near the top of the enclosure (convective flow exits at the top). A minimum opening of 5–10cm² is a starting point; more is better in tightly sealed enclosures.

**Construction:**
- Use 18–22mm lumber or MDF for rigidity
- Pocket screws or biscuits at frame corners; wood glue adds strength
- Rabbet the back edge if you want the monitor to sit flush
- Leave the cleat receiver screwed (not glued) to the back panel so you can remove the frame from the wall

**Finish:**
- Satin or matte paint minimizes glare reflections on the display
- Dark finishes make the monitor bezel less visually prominent

---

## Step 5 — Cable management

Before closing the frame, plan cable routing:

- **HDMI:** a short (~30cm) high-quality micro-HDMI to HDMI cable from Pi to monitor. Allow a gentle curve — the micro-HDMI connector on the Pi 5 is fragile under side-load.
- **USB-C power:** route the power cable through a slot in the frame bottom or back. A recessed grommet keeps it tidy.
- **Button wire:** if you're adding a hardware button, route the wire from the Pi 5 J2 header to the button cutout in the frame. A 2-pin JST or dupont connector at the Pi end makes it easy to disconnect for service.
- **Strain relief:** secure cables near connectors so they don't flex at the plug. Cable ties or adhesive mounts inside the box work fine.

---

## Step 6 — Dry fit and power on

Before any final gluing or finishing:

1. Mount the monitor on the wall cleat.
2. Attach the Pi bracket.
3. Route all cables without the back panel installed.
4. Power on and complete the software setup (see [docs/installation.md](../docs/installation.md)).
5. Verify the frame runs through at least one Wi-Fi recovery cycle (see [docs/installation.md — Fresh Install Wi-Fi Recovery Test](../docs/installation.md)).
6. Confirm thermals: let the frame run for 30 minutes cycling photos and check `vcgencmd measure_temp` — it should stay well below 80°C with any airflow.
7. Once everything is verified, close the back panel.

---

## Assembly checklist

- [ ] Cleat spacers printed and fit-checked on VESA holes
- [ ] French cleat mounted on monitor; plumb on wall
- [ ] Pi bracket cut/printed and Pi mounted
- [ ] HDMI cable connected with strain relief
- [ ] USB-C power routed and secured
- [ ] Button wire routed (if using hardware button)
- [ ] Shadow box built and dry-fit around monitor
- [ ] Ventilation slots present and clear
- [ ] Software fully installed and tested before closing frame
- [ ] Thermal check passed (< 80°C under load)
- [ ] Back panel installed; frame closed
