# Fabricating a Framed Display

> **⚠️ Safety Disclaimer**
>
> The fabrication and mounting method described is this document involve **laser cutting, 3D printing, woodworking, and electrical work**. These activities carry inherent risks of injury, fire, or damage to equipment and property.
>
> Building and operating a wall-mounted display in a wooden frame may also present **long-term safety hazards**, such as overheating and fire.
>
> Follow all applicable safety guidelines and proceed **at your own risk**. This information is provided for educational purposes only, and the authors **assume no responsibility for outcomes**.

## Introduction

The `maker` directory contains files used to outfit a Dell 27-inch 4K monitor (model **S2725QC**) with a custom wall mount and frame.

## Manifest

- `cleat-spacers.scad`: OpenSCAD source file for spacer blocks.  
  Ensures the French cleat bolts tighten into the VESA screw holes without crushing the monitor’s plastic back shell.

- `cleat-spacers.stl`: STL render of the spacer blocks, ready for 3D printing.

## Laser cutting

- Export bezel or mat templates from your CAD tool in SVG or DXF format.
- Test the cut on cardboard to validate fit before committing to acrylic or wood.
- Mask visible surfaces to avoid smoke staining during the cut.

## 3D printing

- Model brackets that clamp the Raspberry Pi to VESA mount points or the display chassis.
- Print test pieces with draft settings to dial in tolerances before switching to final material.
- Add channels or clips for cable routing if you plan to hide wiring behind the frame.

## Carpentry and finishing

- Build a shallow shadow-box to house the monitor. Leave ventilation slots near the top for heat exhaust.
- Use pocket screws or biscuits on long runs so the frame remains rigid after repeated removals from the wall.
- Finish with paint or stain that complements the room lighting; satin sheens minimize glare from the display.

## Assembly checklist

1. Dry-fit the monitor, Pi, brackets, and wiring before final gluing or finishing.
2. Attach French cleats or your preferred wall-mounting hardware.
3. Install the electronics, route cables, and secure strain relief.
4. Close the frame, power it on, and run the software setup steps.

# Support Files for Constructing a Wall-Mounted Frame
