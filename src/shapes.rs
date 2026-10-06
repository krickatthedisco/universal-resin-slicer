//! Built-in shapes and calibration pieces.
//!
//! Primitives are generated here. #3DBenchy is the original public-domain
//! STL. The exposure tests are Amber's own models. AmeraLabs Town and the
//! Cones of Calibration are not bundled; their pages are linked from the menu.

use crate::mesh::{box_mesh, mesh_from_stl_bytes, Mesh};

const TAU: f32 = std::f32::consts::TAU;

pub fn benchy() -> Mesh {
    let mut mesh = mesh_from_stl_bytes(include_bytes!("../assets/3DBenchy.stl"))
        .expect("the bundled #3DBenchy STL is a binary mesh");
    seat(&mut mesh);
    mesh
}

pub fn cube(size: f32) -> Mesh {
    let h = size * 0.5;
    box_mesh([-h, -h, 0.0], [h, h, size])
}

pub fn sphere(radius: f32) -> Mesh {
    let mut mesh = uv_sphere([0.0, 0.0, radius], radius, 16, 24);
    seat(&mut mesh);
    mesh
}

pub fn hemisphere(radius: f32) -> Mesh {
    let mesh = uv_sphere([0.0, 0.0, 0.0], radius, 12, 24);
    let mut kept = Mesh::empty();
    for tri in mesh.indices.chunks_exact(3) {
        let a = mesh.vertices[tri[0] as usize];
        let b = mesh.vertices[tri[1] as usize];
        let c = mesh.vertices[tri[2] as usize];
        if a[2] >= -1e-3 && b[2] >= -1e-3 && c[2] >= -1e-3 {
            push_tri(&mut kept, a, b, c);
        }
    }
    kept.append(&disk([0.0, 0.0, 0.0], radius, 24, false));
    seat(&mut kept);
    kept
}

pub fn cylinder(radius: f32, height: f32) -> Mesh {
    tube(radius, 0.0, height, 24)
}

pub fn tube(outer: f32, inner: f32, height: f32, seg: usize) -> Mesh {
    let seg = seg.max(3);
    let mut mesh = Mesh::empty();
    mesh.append(&cylinder_wall(outer, 0.0, height, seg, true));
    if inner > 0.05 && inner < outer - 0.05 {
        mesh.append(&cylinder_wall(inner, 0.0, height, seg, false));
        mesh.append(&annulus(0.0, outer, inner, seg, false));
        mesh.append(&annulus(height, outer, inner, seg, true));
    } else {
        mesh.append(&disk([0.0, 0.0, 0.0], outer, seg, false));
        mesh.append(&disk([0.0, 0.0, height], outer, seg, true));
    }
    mesh
}

pub fn cone(radius: f32, height: f32) -> Mesh {
    let seg = 24;
    let mut mesh = Mesh::empty();
    let apex = [0.0, 0.0, height];
    for i in 0..seg {
        let a = ring_point(radius, i, seg, 0.0);
        let b = ring_point(radius, i + 1, seg, 0.0);
        push_tri(&mut mesh, apex, a, b);
    }
    mesh.append(&disk([0.0, 0.0, 0.0], radius, seg, false));
    mesh
}

pub fn pyramid(size: f32, height: f32) -> Mesh {
    let h = size * 0.5;
    let apex = [0.0, 0.0, height];
    let p = [[-h, -h, 0.0], [h, -h, 0.0], [h, h, 0.0], [-h, h, 0.0]];
    let mut mesh = Mesh::empty();
    for i in 0..4 {
        push_tri(&mut mesh, apex, p[i], p[(i + 1) % 4]);
    }
    push_tri(&mut mesh, p[0], p[2], p[1]);
    push_tri(&mut mesh, p[0], p[3], p[2]);
    mesh
}

pub fn torus(major: f32, minor: f32) -> Mesh {
    let seg_u = 24;
    let seg_v = 12;
    let mut vertices = Vec::new();
    for u in 0..seg_u {
        let theta = TAU * u as f32 / seg_u as f32;
        for v in 0..seg_v {
            let phi = TAU * v as f32 / seg_v as f32;
            let r = major + minor * phi.cos();
            vertices.push([r * theta.cos(), r * theta.sin(), minor + minor * phi.sin()]);
        }
    }
    let mut indices = Vec::new();
    for u in 0..seg_u {
        for v in 0..seg_v {
            let a = u * seg_v + v;
            let b = ((u + 1) % seg_u) * seg_v + v;
            let c = u * seg_v + (v + 1) % seg_v;
            let d = ((u + 1) % seg_u) * seg_v + (v + 1) % seg_v;
            indices.extend_from_slice(&[a as u32, b as u32, c as u32]);
            indices.extend_from_slice(&[b as u32, d as u32, c as u32]);
        }
    }
    Mesh { vertices, indices }
}

pub fn capsule(radius: f32, height: f32) -> Mesh {
    let mut mesh = cylinder_wall(radius, 0.0, height, 24, true);
    let top = uv_sphere([0.0, 0.0, height], radius, 10, 24);
    for tri in top.indices.chunks_exact(3) {
        let a = top.vertices[tri[0] as usize];
        let b = top.vertices[tri[1] as usize];
        let c = top.vertices[tri[2] as usize];
        if a[2] >= height - 1e-3 && b[2] >= height - 1e-3 && c[2] >= height - 1e-3 {
            push_tri(&mut mesh, a, b, c);
        }
    }
    let bottom = uv_sphere([0.0, 0.0, 0.0], radius, 10, 24);
    for tri in bottom.indices.chunks_exact(3) {
        let a = bottom.vertices[tri[0] as usize];
        let b = bottom.vertices[tri[1] as usize];
        let c = bottom.vertices[tri[2] as usize];
        if a[2] <= 1e-3 && b[2] <= 1e-3 && c[2] <= 1e-3 {
            push_tri(&mut mesh, a, b, c);
        }
    }
    seat(&mut mesh);
    mesh
}

pub fn wedge(width: f32, depth: f32, height: f32) -> Mesh {
    let x0 = -width * 0.5;
    let x1 = width * 0.5;
    let y0 = -depth * 0.5;
    let y1 = depth * 0.5;
    let p = [
        [x0, y0, 0.0],
        [x1, y0, 0.0],
        [x1, y1, 0.0],
        [x0, y1, 0.0],
        [x0, y0, height],
        [x1, y0, height],
    ];
    let mut mesh = Mesh::empty();
    push_tri(&mut mesh, p[0], p[1], p[2]);
    push_tri(&mut mesh, p[0], p[2], p[3]);
    push_tri(&mut mesh, p[0], p[4], p[5]);
    push_tri(&mut mesh, p[0], p[5], p[1]);
    push_tri(&mut mesh, p[0], p[3], p[4]);
    push_tri(&mut mesh, p[1], p[5], p[2]);
    push_tri(&mut mesh, p[3], p[2], p[5]);
    push_tri(&mut mesh, p[3], p[5], p[4]);
    mesh.flip_winding();
    seat(&mut mesh);
    mesh
}

pub fn hex_prism(across: f32, height: f32) -> Mesh {
    let radius = across / 3.0f32.sqrt();
    let seg = 6;
    let mut mesh = cylinder_wall(radius, 0.0, height, seg, true);
    mesh.append(&disk([0.0, 0.0, 0.0], radius, seg, false));
    mesh.append(&disk([0.0, 0.0, height], radius, seg, true));
    mesh
}

pub fn slab(width: f32, depth: f32, height: f32) -> Mesh {
    box_mesh(
        [-width * 0.5, -depth * 0.5, 0.0],
        [width * 0.5, depth * 0.5, height],
    )
}

/// A cup with a floor, for trying a hollow and a drain.
pub fn drain_cup() -> Mesh {
    let mut mesh = cylinder_wall(12.0, 0.0, 18.0, 32, true);
    mesh.append(&cylinder_wall(9.5, 1.6, 18.0, 32, false));
    mesh.append(&disk([0.0, 0.0, 0.0], 12.0, 32, false));
    mesh.append(&disk([0.0, 0.0, 1.6], 9.5, 32, true));
    mesh.append(&annulus(1.6, 12.0, 9.5, 32, true));
    mesh.append(&annulus(18.0, 12.0, 9.5, 32, true));
    mesh
}

/// Towers, an arch, and a thin fin. Amber's own exposure city, not a copy
/// of anyone else's calibration town.
pub fn exposure_city() -> Mesh {
    let mut mesh = box_mesh([0.0, 0.0, 0.0], [28.0, 16.0, 1.6]);
    let towers = [
        (2.0_f32, 3.2_f32, 10.0_f32),
        (6.2, 2.2, 8.5),
        (9.4, 1.4, 7.0),
        (11.8, 0.9, 6.0),
        (13.6, 0.55, 5.0),
        (15.0, 0.32, 4.2),
    ];
    for (x, w, h) in towers {
        mesh.append(&box_mesh([x, 10.5, 1.6], [x + w, 14.4, 1.6 + h]));
    }
    mesh.append(&box_mesh([18.0, 2.0, 1.6], [20.2, 6.4, 8.0]));
    mesh.append(&box_mesh([25.2, 2.0, 1.6], [27.2, 6.4, 8.0]));
    mesh.append(&box_mesh([18.0, 2.0, 8.0], [27.2, 6.4, 9.6]));
    mesh.append(&box_mesh([2.0, 2.2, 1.6], [2.45, 8.5, 7.5]));
    seat(&mut mesh);
    mesh
}

/// Posts from thick to hairline, so the thinnest one that survives is the limit.
pub fn pin_card() -> Mesh {
    let mut mesh = box_mesh([0.0, 0.0, 0.0], [30.0, 10.0, 1.4]);
    let pins = [1.8_f32, 1.2, 0.8, 0.5, 0.3];
    for (i, diam) in pins.iter().enumerate() {
        let mut pin = cylinder(diam * 0.5, 7.0);
        pin.translate([4.0 + i as f32 * 5.2, 5.0, 1.4]);
        mesh.append(&pin);
    }
    seat(&mut mesh);
    mesh
}

/// Slots from wide to narrow. The smallest one that stays open is the limit.
pub fn hole_card() -> Mesh {
    let slots = [2.2_f32, 1.5, 1.0, 0.6, 0.35];
    let gap = 1.8_f32;
    let margin = 2.2_f32;
    let depth = 6.0_f32;
    let rail = 2.0_f32;
    let h = 2.4_f32;
    let span = margin * 2.0 + slots.iter().sum::<f32>() + gap * (slots.len() - 1) as f32;
    let mut mesh = Mesh::empty();
    mesh.append(&box_mesh([0.0, 0.0, 0.0], [span, rail, h]));
    mesh.append(&box_mesh(
        [0.0, rail + depth, 0.0],
        [span, rail * 2.0 + depth, h],
    ));
    mesh.append(&box_mesh([0.0, rail, 0.0], [margin, rail + depth, h]));
    let mut x = margin;
    for (i, width) in slots.iter().enumerate() {
        x += width;
        if i + 1 != slots.len() {
            mesh.append(&box_mesh([x, rail, 0.0], [x + gap, rail + depth, h]));
            x += gap;
        }
    }
    mesh.append(&box_mesh([x, rail, 0.0], [x + margin, rail + depth, h]));
    seat(&mut mesh);
    mesh
}

/// Overhangs at 30°, 45°, 60°, and 75° from vertical.
pub fn slope_card() -> Mesh {
    let mut mesh = Mesh::empty();
    let angles = [30.0_f32, 45.0, 60.0, 75.0];
    for (i, angle) in angles.iter().enumerate() {
        let mut block = slope_wedge(*angle, 6.0, 8.0);
        block.translate([i as f32 * 8.0, 0.0, 0.0]);
        mesh.append(&block);
    }
    seat(&mut mesh);
    mesh
}

fn slope_wedge(angle_from_vertical: f32, width: f32, height: f32) -> Mesh {
    let run = height * angle_from_vertical.to_radians().tan();
    let foot = 1.5_f32;
    let y_lip = foot + run;
    let p = [
        [0.0, 0.0, 0.0],
        [width, 0.0, 0.0],
        [width, y_lip, 0.0],
        [0.0, y_lip, 0.0],
        [0.0, 0.0, height],
        [width, 0.0, height],
    ];
    let mut mesh = Mesh::empty();
    push_tri(&mut mesh, p[0], p[2], p[1]);
    push_tri(&mut mesh, p[0], p[3], p[2]);
    push_tri(&mut mesh, p[0], p[5], p[4]);
    push_tri(&mut mesh, p[0], p[1], p[5]);
    push_tri(&mut mesh, p[0], p[4], p[3]);
    push_tri(&mut mesh, p[1], p[2], p[5]);
    push_tri(&mut mesh, p[4], p[5], p[2]);
    push_tri(&mut mesh, p[4], p[2], p[3]);
    mesh
}

/// A raised digit, 1 through 8, sitting on the bed. Used to mark a RERF zone.
pub fn digit_mesh(n: u8) -> Mesh {
    let rows: [&str; 5] = match n {
        1 => [".#.", "##.", ".#.", ".#.", "###"],
        2 => ["###", "..#", "###", "#..", "###"],
        3 => ["###", "..#", "###", "..#", "###"],
        4 => ["#.#", "#.#", "###", "..#", "..#"],
        5 => ["###", "#..", "###", "..#", "###"],
        6 => ["#..", "#..", "###", "#.#", "###"],
        7 => ["###", "..#", "..#", "..#", "..#"],
        _ => ["###", "#.#", "###", "#.#", "###"],
    };
    let pitch = 1.55_f32;
    let pix = 1.25_f32;
    let mut mesh = Mesh::empty();
    for (row, pattern) in rows.iter().enumerate() {
        for (col, ch) in pattern.chars().enumerate() {
            if ch == '#' {
                let x = col as f32 * pitch;
                let y = (4 - row) as f32 * pitch;
                mesh.append(&box_mesh([x, y, 0.0], [x + pix, y + pitch * 0.8, 0.8]));
            }
        }
    }
    mesh
}

pub fn seat(mesh: &mut Mesh) {
    let Some((min, max)) = mesh.bounds() else {
        return;
    };
    let dx = -0.5 * (min[0] + max[0]);
    let dy = -0.5 * (min[1] + max[1]);
    let dz = -min[2];
    mesh.translate([dx, dy, dz]);
}

fn uv_sphere(center: [f32; 3], radius: f32, stacks: usize, slices: usize) -> Mesh {
    let stacks = stacks.max(2);
    let slices = slices.max(3);
    let mut vertices = Vec::new();
    for stack in 0..=stacks {
        let phi = std::f32::consts::PI * stack as f32 / stacks as f32;
        for slice in 0..=slices {
            let theta = TAU * slice as f32 / slices as f32;
            vertices.push([
                center[0] + radius * phi.sin() * theta.cos(),
                center[1] + radius * phi.sin() * theta.sin(),
                center[2] + radius * phi.cos(),
            ]);
        }
    }
    let row = slices + 1;
    let mut indices = Vec::new();
    for stack in 0..stacks {
        for slice in 0..slices {
            let a = stack * row + slice;
            let b = a + row;
            indices.extend_from_slice(&[a as u32, b as u32, (a + 1) as u32]);
            indices.extend_from_slice(&[b as u32, (b + 1) as u32, (a + 1) as u32]);
        }
    }
    Mesh { vertices, indices }
}

fn ring_point(radius: f32, i: usize, seg: usize, z: f32) -> [f32; 3] {
    let theta = TAU * i as f32 / seg as f32;
    [radius * theta.cos(), radius * theta.sin(), z]
}

fn cylinder_wall(radius: f32, z0: f32, z1: f32, seg: usize, outward: bool) -> Mesh {
    let mut mesh = Mesh::empty();
    for i in 0..seg {
        let a0 = ring_point(radius, i, seg, z0);
        let b0 = ring_point(radius, i + 1, seg, z0);
        let a1 = ring_point(radius, i, seg, z1);
        let b1 = ring_point(radius, i + 1, seg, z1);
        if outward {
            push_tri(&mut mesh, a0, b0, b1);
            push_tri(&mut mesh, a0, b1, a1);
        } else {
            push_tri(&mut mesh, a0, b1, b0);
            push_tri(&mut mesh, a0, a1, b1);
        }
    }
    mesh
}

fn disk(center: [f32; 3], radius: f32, seg: usize, up: bool) -> Mesh {
    let mut mesh = Mesh::empty();
    for i in 0..seg {
        let a = ring_point(radius, i, seg, center[2]);
        let b = ring_point(radius, i + 1, seg, center[2]);
        let a = [a[0] + center[0], a[1] + center[1], a[2]];
        let b = [b[0] + center[0], b[1] + center[1], b[2]];
        if up {
            push_tri(&mut mesh, center, a, b);
        } else {
            push_tri(&mut mesh, center, b, a);
        }
    }
    mesh
}

fn annulus(z: f32, outer: f32, inner: f32, seg: usize, up: bool) -> Mesh {
    let mut mesh = Mesh::empty();
    for i in 0..seg {
        let o0 = ring_point(outer, i, seg, z);
        let o1 = ring_point(outer, i + 1, seg, z);
        let i0 = ring_point(inner, i, seg, z);
        let i1 = ring_point(inner, i + 1, seg, z);
        if up {
            push_tri(&mut mesh, o0, i0, i1);
            push_tri(&mut mesh, o0, i1, o1);
        } else {
            push_tri(&mut mesh, o0, i1, i0);
            push_tri(&mut mesh, o0, o1, i1);
        }
    }
    mesh
}

fn push_tri(mesh: &mut Mesh, a: [f32; 3], b: [f32; 3], c: [f32; 3]) {
    let base = mesh.vertices.len() as u32;
    mesh.vertices.extend_from_slice(&[a, b, c]);
    mesh.indices.extend_from_slice(&[base, base + 1, base + 2]);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn positive(name: &str, mesh: &Mesh) {
        assert!(mesh.triangle_count() > 4, "{name} too few triangles");
        assert!(
            mesh.signed_volume_mm3() > 1.0,
            "{name} volume {}",
            mesh.signed_volume_mm3()
        );
    }

    #[test]
    fn primitives_are_closed_and_sit_on_the_bed() {
        let made: [(&str, Mesh); 18] = [
            ("cube", cube(10.0)),
            ("sphere", sphere(8.0)),
            ("hemisphere", hemisphere(8.0)),
            ("cylinder", cylinder(6.0, 12.0)),
            ("tube", tube(8.0, 5.0, 10.0, 16)),
            ("cone", cone(6.0, 12.0)),
            ("pyramid", pyramid(10.0, 12.0)),
            ("torus", torus(8.0, 2.0)),
            ("capsule", capsule(4.0, 10.0)),
            ("wedge", wedge(8.0, 10.0, 6.0)),
            ("hex", hex_prism(8.0, 10.0)),
            ("slab", slab(20.0, 12.0, 2.0)),
            ("cup", drain_cup()),
            ("city", exposure_city()),
            ("pins", pin_card()),
            ("holes", hole_card()),
            ("slopes", slope_card()),
            ("digit", digit_mesh(8)),
        ];
        for (name, mesh) in made {
            positive(name, &mesh);
            let (min, _) = mesh.bounds().unwrap();
            assert!(min[2].abs() < 0.05, "{name} not on the bed: {}", min[2]);
        }
    }

    #[test]
    fn benchy_is_the_bundled_boat() {
        let mesh = benchy();
        assert!(mesh.triangle_count() > 100_000);
        let (min, max) = mesh.bounds().unwrap();
        let size = [max[0] - min[0], max[1] - min[1], max[2] - min[2]];
        assert!(size[0] > 40.0 && size[0] < 80.0, "length {}", size[0]);
        assert!(size[2] > 30.0 && size[2] < 70.0, "height {}", size[2]);
        assert!(min[2].abs() < 0.2);
    }
}
