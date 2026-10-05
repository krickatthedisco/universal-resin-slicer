//! Overhang supports, braces, and the raft those supports stand on.
//!
//! Auto supports sample downward faces on a spacing grid and drop a column
//! to the bed, or to the model if the column hits it on the way down.
//! The overhang angle is measured from vertical: a higher angle keeps supports
//! off of steeper walls and leaves fewer of them.

use crate::mesh::{face_normal, Mesh};
use glam::Vec3;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug)]
pub struct SupportPreset {
    pub name: &'static str,
    pub overhang_deg: f32,
    pub spacing_mm: f32,
    pub tip_mm: f32,
    pub shaft_mm: f32,
    pub penetration_mm: f32,
    pub tip_len_mm: f32,
    pub foot_mm: f32,
}

pub const PRESETS: &[SupportPreset] = &[
    SupportPreset {
        name: "Light",
        overhang_deg: 35.0,
        spacing_mm: 2.2,
        tip_mm: 0.30,
        shaft_mm: 0.80,
        penetration_mm: 0.25,
        tip_len_mm: 2.0,
        foot_mm: 0.6,
    },
    SupportPreset {
        name: "Medium",
        overhang_deg: 45.0,
        spacing_mm: 3.2,
        tip_mm: 0.40,
        shaft_mm: 1.15,
        penetration_mm: 0.35,
        tip_len_mm: 2.4,
        foot_mm: 0.8,
    },
    SupportPreset {
        name: "Heavy",
        overhang_deg: 55.0,
        spacing_mm: 4.5,
        tip_mm: 0.60,
        shaft_mm: 1.70,
        penetration_mm: 0.45,
        tip_len_mm: 3.0,
        foot_mm: 1.1,
    },
];

#[derive(Clone, Copy, Debug)]
pub struct Support {
    pub id: u64,
    pub x: f32,
    pub y: f32,
    pub z_top: f32,
    pub z_base: f32,
    pub preset_tip: f32,
    pub preset_shaft: f32,
    pub penetration: f32,
    pub tip_len: f32,
    pub foot: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct Brace {
    pub a: Vec3,
    pub b: Vec3,
    pub radius: f32,
}

pub fn needs_support(normal: [f32; 3], overhang_deg: f32) -> bool {
    if normal[2] >= -0.02 {
        return false;
    }
    let down = (-normal[2]).clamp(0.0, 1.0).acos();
    let slope_from_vertical = std::f32::consts::FRAC_PI_2 - down;
    slope_from_vertical.to_degrees() + 0.05 >= overhang_deg
}

struct TriGrid<'a> {
    cell: f32,
    verts: &'a [[f32; 3]],
    indices: &'a [u32],
    map: HashMap<(i32, i32), Vec<usize>>,
    large: Vec<usize>,
}

impl<'a> TriGrid<'a> {
    fn build(verts: &'a [[f32; 3]], indices: &'a [u32]) -> Self {
        let cell = 8.0f32;
        let mut map: HashMap<(i32, i32), Vec<usize>> = HashMap::new();
        let mut large = Vec::new();
        for (t, tri) in indices.chunks_exact(3).enumerate() {
            let a = verts[tri[0] as usize];
            let b = verts[tri[1] as usize];
            let c = verts[tri[2] as usize];
            let min_x = a[0].min(b[0]).min(c[0]);
            let max_x = a[0].max(b[0]).max(c[0]);
            let min_y = a[1].min(b[1]).min(c[1]);
            let max_y = a[1].max(b[1]).max(c[1]);
            let x0 = (min_x / cell).floor() as i32;
            let x1 = (max_x / cell).floor() as i32;
            let y0 = (min_y / cell).floor() as i32;
            let y1 = (max_y / cell).floor() as i32;
            if (x1 - x0 + 1) * (y1 - y0 + 1) > 64 {
                large.push(t);
                continue;
            }
            for y in y0..=y1 {
                for x in x0..=x1 {
                    map.entry((x, y)).or_default().push(t);
                }
            }
        }
        Self {
            cell,
            verts,
            indices,
            map,
            large,
        }
    }

    fn consider(&self, best: &mut Option<f32>, t: usize, x: f32, y: f32, z: f32) {
        let tri = &self.indices[t * 3..t * 3 + 3];
        let a = self.verts[tri[0] as usize];
        let b = self.verts[tri[1] as usize];
        let c = self.verts[tri[2] as usize];
        if let Some(hit_z) = vertical_hit(x, y, a, b, c) {
            if hit_z < z - 0.35 {
                *best = Some(best.map(|v| v.max(hit_z)).unwrap_or(hit_z));
            }
        }
    }

    fn highest_below(&self, x: f32, y: f32, z: f32) -> Option<f32> {
        let key = (
            (x / self.cell).floor() as i32,
            (y / self.cell).floor() as i32,
        );
        let mut best: Option<f32> = None;
        if let Some(tris) = self.map.get(&key) {
            for &t in tris {
                self.consider(&mut best, t, x, y, z);
            }
        }
        for &t in &self.large {
            self.consider(&mut best, t, x, y, z);
        }
        best
    }
}

fn vertical_hit(x: f32, y: f32, a: [f32; 3], b: [f32; 3], c: [f32; 3]) -> Option<f32> {
    let denom = (b[1] - c[1]) * (a[0] - c[0]) + (c[0] - b[0]) * (a[1] - c[1]);
    if denom.abs() < 1e-8 {
        return None;
    }
    let w0 = ((b[1] - c[1]) * (x - c[0]) + (c[0] - b[0]) * (y - c[1])) / denom;
    let w1 = ((c[1] - a[1]) * (x - c[0]) + (a[0] - c[0]) * (y - c[1])) / denom;
    let w2 = 1.0 - w0 - w1;
    if w0 < -1e-4 || w1 < -1e-4 || w2 < -1e-4 {
        return None;
    }
    Some(w0 * a[2] + w1 * b[2] + w2 * c[2])
}

pub fn auto_supports(
    verts: &[[f32; 3]],
    indices: &[u32],
    preset: &SupportPreset,
    next_id: &mut u64,
) -> Vec<Support> {
    let grid = TriGrid::build(verts, indices);
    let mut cells: HashMap<(i32, i32), [f32; 3]> = HashMap::new();
    let spacing = preset.spacing_mm.max(0.6);
    for tri in indices.chunks_exact(3) {
        let a = verts[tri[0] as usize];
        let b = verts[tri[1] as usize];
        let c = verts[tri[2] as usize];
        let n = face_normal(a, b, c);
        if !needs_support(n, preset.overhang_deg) {
            continue;
        }
        let min_x = a[0].min(b[0]).min(c[0]);
        let max_x = a[0].max(b[0]).max(c[0]);
        let min_y = a[1].min(b[1]).min(c[1]);
        let max_y = a[1].max(b[1]).max(c[1]);
        let mut samples = vec![[
            (a[0] + b[0] + c[0]) / 3.0,
            (a[1] + b[1] + c[1]) / 3.0,
            (a[2] + b[2] + c[2]) / 3.0,
        ]];
        let span = (max_x - min_x).max(max_y - min_y);
        if span > spacing * 0.75 {
            let x_start = (min_x / spacing).floor() * spacing;
            let y_start = (min_y / spacing).floor() * spacing;
            let mut x = x_start;
            while x <= max_x {
                let mut y = y_start;
                while y <= max_y {
                    if let Some(z) = vertical_hit(x, y, a, b, c) {
                        samples.push([x, y, z]);
                    }
                    y += spacing;
                }
                x += spacing;
            }
        }
        for s in samples {
            if s[2] < 0.4 {
                continue;
            }
            let key = (
                (s[0] / spacing).floor() as i32,
                (s[1] / spacing).floor() as i32,
            );
            cells
                .entry(key)
                .and_modify(|old| {
                    if s[2] < old[2] {
                        *old = s;
                    }
                })
                .or_insert(s);
        }
    }
    let mut out = Vec::new();
    for s in cells.into_values() {
        let base = grid.highest_below(s[0], s[1], s[2]).unwrap_or(0.0).max(0.0);
        if s[2] - base < 0.8 {
            continue;
        }
        let id = *next_id;
        *next_id += 1;
        out.push(Support {
            id,
            x: s[0],
            y: s[1],
            z_top: s[2],
            z_base: base,
            preset_tip: preset.tip_mm,
            preset_shaft: preset.shaft_mm,
            penetration: preset.penetration_mm,
            tip_len: preset.tip_len_mm,
            foot: preset.foot_mm,
        });
    }
    out
}

pub fn manual_support(
    x: f32,
    y: f32,
    z: f32,
    verts: &[[f32; 3]],
    indices: &[u32],
    preset: &SupportPreset,
    id: u64,
) -> Support {
    let grid = TriGrid::build(verts, indices);
    let base = grid.highest_below(x, y, z).unwrap_or(0.0).max(0.0);
    Support {
        id,
        x,
        y,
        z_top: z,
        z_base: base,
        preset_tip: preset.tip_mm,
        preset_shaft: preset.shaft_mm,
        penetration: preset.penetration_mm,
        tip_len: preset.tip_len_mm,
        foot: preset.foot_mm,
    }
}

pub fn support_mesh(support: &Support, raft_top: f32) -> Mesh {
    let mut mesh = Mesh {
        vertices: vec![],
        indices: vec![],
    };
    let mut z_base = support.z_base;
    let on_bed = z_base <= 0.05;
    if on_bed && raft_top > 0.0 {
        z_base = raft_top - 0.15;
    }
    let z_top = support.z_top;
    let shaft_r = support.preset_shaft * 0.5;
    let tip_r = support.preset_tip * 0.5;
    let available = (z_top - z_base).max(0.4);
    let tip_len = support.tip_len.min(available * 0.55).max(0.4);
    let cone_base_z = z_top - tip_len;
    let apex_z = z_top + support.penetration;
    mesh.append(&tapered(
        Vec3::new(support.x, support.y, cone_base_z),
        shaft_r,
        Vec3::new(support.x, support.y, apex_z),
        tip_r.max(0.08),
        10,
    ));
    let foot_h = if on_bed {
        support.foot.min((cone_base_z - z_base).max(0.0) * 0.45)
    } else {
        0.0
    };
    let shaft_bottom = z_base + foot_h;
    if cone_base_z - shaft_bottom > 0.15 {
        mesh.append(&tapered(
            Vec3::new(support.x, support.y, shaft_bottom),
            shaft_r,
            Vec3::new(support.x, support.y, cone_base_z),
            shaft_r,
            10,
        ));
    }
    if foot_h > 0.1 {
        mesh.append(&tapered(
            Vec3::new(support.x, support.y, z_base),
            shaft_r * 1.8,
            Vec3::new(support.x, support.y, shaft_bottom),
            shaft_r,
            10,
        ));
    }
    mesh
}

pub fn braces(supports: &[Support], max_dist: f32) -> Vec<Brace> {
    let mut out = Vec::new();
    let mut used = vec![0u8; supports.len()];
    for i in 0..supports.len() {
        if used[i] >= 2 {
            continue;
        }
        let a = &supports[i];
        let mut best: Option<(usize, f32)> = None;
        for j in (i + 1)..supports.len() {
            if used[j] >= 2 {
                continue;
            }
            let b = &supports[j];
            let dx = a.x - b.x;
            let dy = a.y - b.y;
            let dist = (dx * dx + dy * dy).sqrt();
            if dist < a.preset_shaft || dist > max_dist {
                continue;
            }
            let overlap_top = a.z_top.min(b.z_top) - 2.0;
            let overlap_bot = a.z_base.max(b.z_base) + 1.5;
            if overlap_top - overlap_bot < 3.0 {
                continue;
            }
            if best.map(|(_, d)| dist < d).unwrap_or(true) {
                best = Some((j, dist));
            }
        }
        if let Some((j, _)) = best {
            used[i] += 1;
            used[j] += 1;
            let b = &supports[j];
            let z = ((a.z_base + a.z_top) * 0.5).min((b.z_base + b.z_top) * 0.5);
            let z = z.clamp(a.z_base.max(b.z_base) + 1.2, a.z_top.min(b.z_top) - 1.5);
            out.push(Brace {
                a: Vec3::new(a.x, a.y, z),
                b: Vec3::new(b.x, b.y, z),
                radius: a.preset_shaft.min(b.preset_shaft) * 0.28,
            });
        }
    }
    out
}

pub fn brace_mesh(brace: &Brace) -> Mesh {
    tapered(brace.a, brace.radius, brace.b, brace.radius, 8)
}

pub fn raft_mesh(supports: &[Support], margin: f32, thickness: f32) -> Option<Mesh> {
    let bed: Vec<&Support> = supports.iter().filter(|s| s.z_base <= 0.2).collect();
    if bed.is_empty() || thickness <= 0.0 {
        return None;
    }
    let mut min_x = f32::MAX;
    let mut min_y = f32::MAX;
    let mut max_x = f32::MIN;
    let mut max_y = f32::MIN;
    for s in bed {
        let r = s.preset_shaft;
        min_x = min_x.min(s.x - r);
        min_y = min_y.min(s.y - r);
        max_x = max_x.max(s.x + r);
        max_y = max_y.max(s.y + r);
    }
    Some(crate::mesh::box_mesh(
        [min_x - margin, min_y - margin, 0.0],
        [max_x + margin, max_y + margin, thickness],
    ))
}

fn tapered(a: Vec3, ra: f32, b: Vec3, rb: f32, seg: usize) -> Mesh {
    let seg = seg.max(3);
    let mut axis = b - a;
    let len = axis.length();
    if len < 1e-4 {
        return Mesh {
            vertices: vec![],
            indices: vec![],
        };
    }
    axis /= len;
    let helper = if axis.z.abs() < 0.9 { Vec3::Z } else { Vec3::X };
    let u = axis.cross(helper).normalize();
    let v = axis.cross(u).normalize();
    let mut vertices = Vec::with_capacity(seg * 2 + 2);
    for ring in 0..2 {
        let center = if ring == 0 { a } else { b };
        let r = if ring == 0 { ra } else { rb };
        for i in 0..seg {
            let t = i as f32 / seg as f32 * std::f32::consts::TAU;
            vertices.push((center + (u * t.cos() + v * t.sin()) * r).to_array());
        }
    }
    let cap_a = vertices.len() as u32;
    vertices.push(a.to_array());
    let cap_b = vertices.len() as u32;
    vertices.push(b.to_array());
    let mut indices = Vec::new();
    for i in 0..seg {
        let i0 = i as u32;
        let i1 = ((i + 1) % seg) as u32;
        let j0 = i0 + seg as u32;
        let j1 = i1 + seg as u32;
        indices.extend_from_slice(&[i0, j0, j1, i0, j1, i1]);
        indices.extend_from_slice(&[cap_a, i1, i0]);
        indices.extend_from_slice(&[cap_b, j0, j1]);
    }
    Mesh { vertices, indices }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_down_face_wants_a_support_and_a_wall_does_not() {
        assert!(needs_support([0.0, 0.0, -1.0], 45.0));
        assert!(!needs_support([1.0, 0.0, 0.0], 45.0));
        assert!(!needs_support([0.0, 0.0, 1.0], 45.0));
    }

    #[test]
    fn overhang_bar_grows_columns() {
        let plate_x = 100.0;
        let plate_y = 80.0;
        let mesh = crate::mesh::overhang_bridge(plate_x, plate_y);
        let mut id = 1u64;
        let supports = auto_supports(&mesh.vertices, &mesh.indices, &PRESETS[1], &mut id);
        assert!(
            supports.len() >= 3,
            "expected several supports under the bridge, got {}",
            supports.len()
        );
        assert!(supports.iter().all(|s| s.z_top > s.z_base));
    }
}
