// Simple blocks with a hole to space cleat on Dell S2725QC, OpenSCAD (units: mm)

// ----- Parameters you can tweak -----
corner_radius = 1.0;   // "slightly rounded corners" — adjust if you like
hole_d        = 4.5;   // hole diameter
edge_offset   = 11.0;  // distance of hole center from right/top edges

// Part specs
size1   = 17.0;   height1 = 10.75; // Part 1
size2   = 18.5;   height2 = 14.0;  // Part 2

$fn = 64; // smoothness for circles/cylinders

// ----- Helper: rounded square that ends up exactly 'size' × 'size' -----
module rounded_square(size, r=1.0) {
    // Build a size×size rounded rectangle by minkowski-expanding a smaller square
    // with a circle of radius r. The inner square is (size-2r).
    minkowski() {
        square(size - 2*r, center=false);
        circle(r=r);
    }
}

// ----- Helper: block with through-hole at (right/top) offsets -----
module block_with_hole(size, height, r, hole_dia, offset_rt) {
    difference() {
        // Main body
        linear_extrude(height)
            rounded_square(size, r);

        // Through-hole: 11 mm from right and 11 mm from top edges
        translate([size - offset_rt, size - offset_rt, -1])
            cylinder(h = height + 2, d = hole_dia);  // overshoot Z so it's definitely through
    }
}

// ----- Individual parts -----
module part1() block_with_hole(size1, height1, corner_radius, hole_d, edge_offset);
module part2() block_with_hole(size2, height2, corner_radius, hole_d, edge_offset);

// ----- Preview both (side-by-side). Comment one out to export STLs separately. -----
translate([0, 0, 0])      part1();
translate([max(size1,size2) + 10, 0, 0]) part2();

// To export:
// 1) Comment out the other part so only one remains visible.
// 2) Design -> Export -> Export as STL.
