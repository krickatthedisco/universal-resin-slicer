//! Branching resin supports: a contact tip, a short neck, and a branch into a trunk.
//!
//! Auto supports sample downward faces on a spacing grid. Nearby tips share one
//! trunk, the way a classic resin tree does, instead of each tip growing its
//! own column to the bed. The overhang angle is measured from vertical: a
//! higher angle keeps supports off of steeper walls.

use crate::mesh::{face_normal, Mesh};
use glam::Vec3;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct SupportStyle {
    pub overhang_deg: f32,
    pub spacing_mm: f32,
    pub contact_mm: f32,
    pub contact_depth: f32,
    pub ball: bool,
    pub tip_upper_mm: f32,
    pub tip_lower_mm: f32,
    pub tip_len_mm: f32,
    pub trunk_mm: f32,
    pub branch_deg: f32,
    pub cluster_mm: f32,
    pub foot_mm: f32,
    pub brace_mm: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct SupportPreset {
    pub name: &'static str,
    pub style: SupportStyle,
}

pub const PRESETS: &[SupportPreset] = &[
    SupportPreset {
        name: "Light",
        style: SupportStyle {
            overhang_deg: 35.0,
            spacing_mm: 2.0,
            contact_mm: 0.25,
            contact_depth: 0.20,
            ball: false,
            tip_upper_mm: 0.30,
            tip_lower_mm: 0.55,
            tip_len_mm: 2.0,
            trunk_mm: 0.90,
            branch_deg: 40.0,
            cluster_mm: 5.5,
            foot_mm: 0.6,
            brace_mm: 0.32,
        },
    },
    SupportPreset {
        name: "Medium",
        style: SupportStyle {
            overhang_deg: 45.0,
            spacing_mm: 3.0,
            contact_mm: 0.40,
            contact_depth: 0.30,
            ball: false,
            tip_upper_mm: 0.45,
            tip_lower_mm: 0.80,
            tip_len_mm: 2.4,
            trunk_mm: 1.20,
            branch_deg: 45.0,
            cluster_mm: 7.0,
            foot_mm: 0.8,
            brace_mm: 0.42,
        },
    },
    SupportPreset {
        name: "Heavy",
        style: SupportStyle {
            overhang_deg: 55.0,
            spacing_mm: 4.2,
            contact_mm: 0.60,
            contact_depth: 0.40,
            ball: false,
            tip_upper_mm: 0.70,
            tip_lower_mm: 1.10,
            tip_len_mm: 3.0,
            trunk_mm: 1.70,
            branch_deg: 50.0,
            cluster_mm: 9.0,
            foot_mm: 1.1,
            brace_mm: 0.55,
        },
    },
];

impl SupportStyle {
    pub fn from_preset(preset: &SupportPreset) -> Self {
        preset.style
    }

    pub fn sanitized(self) -> Self {
        Self {
            overhang_deg: self.overhang_deg.clamp(5.0, 85.0),
            spacing_mm: self.spacing_mm.clamp(0.6, 12.0),
            contact_mm: self.contact_mm.clamp(0.08, 2.0),
            contact_depth: self.contact_depth.clamp(0.05, 1.5),
            ball: self.ball,
            tip_upper_mm: self.tip_upper_mm.clamp(0.1, 3.0),
            tip_lower_mm: self.tip_lower_mm.clamp(0.15, 4.0),
            tip_len_mm: self.tip_len_mm.clamp(0.4, 8.0),
            trunk_mm: self.trunk_mm.clamp(0.3, 5.0),
            branch_deg: self.branch_deg.clamp(10.0, 75.0),
            cluster_mm: self.cluster_mm.clamp(1.5, 20.0),
            foot_mm: self.foot_mm.clamp(0.2, 3.0),
            brace_mm: self.brace_mm.clamp(0.1, 2.0),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Support {
    pub id: u64,
    pub x: f32,
    pub y: f32,
    pub z_top: f32,
    pub z_base: f32,
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
    style: &SupportStyle,
    platform_only: bool,
    next_id: &mut u64,
) -> Vec<Support> {
    let style = style.sanitized();
    let grid = TriGrid::build(verts, indices);
    let mut cells: HashMap<(i32, i32), [f32; 3]> = HashMap::new();
    let spacing = style.spacing_mm;
    for tri in indices.chunks_exact(3) {
        let a = verts[tri[0] as usize];
        let b = verts[tri[1] as usize];
        let c = verts[tri[2] as usize];
        let n = face_normal(a, b, c);
        if !needs_support(n, style.overhang_deg) {
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
        let landed = if platform_only {
            0.0
        } else {
            grid.highest_below(s[0], s[1], s[2]).unwrap_or(0.0).max(0.0)
        };
        if s[2] - landed < 0.8 {
            continue;
        }
        let id = *next_id;
        *next_id += 1;
        out.push(Support {
            id,
            x: s[0],
            y: s[1],
            z_top: s[2],
            z_base: landed,
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
    platform_only: bool,
    id: u64,
) -> Support {
    let base = if platform_only {
        0.0
    } else {
        let grid = TriGrid::build(verts, indices);
        grid.highest_below(x, y, z).unwrap_or(0.0).max(0.0)
    };
    Support {
        id,
        x,
        y,
        z_top: z,
        z_base: base,
    }
}

struct Trunk {
    x: f32,
    y: f32,
    z_base: f32,
    z_top: f32,
    radius: f32,
}

/// Tips, angled branches, shared trunks, feet, and cross-braces for the whole plate.
pub fn forest_mesh(
    supports: &[Support],
    style: &SupportStyle,
    raft_top: f32,
    braces_on: bool,
    brace_dist: f32,
) -> Mesh {
    let style = style.sanitized();
    let mut mesh = Mesh {
        vertices: vec![],
        indices: vec![],
    };
    if supports.is_empty() {
        return mesh;
    }
    let mut trunks = Vec::new();
    for group in cluster(supports, style.cluster_mm) {
        let anchor = anchor_index(supports, &group);
        let host = &supports[anchor];
        let mut z_base = host.z_base.max(0.0);
        let on_bed = z_base <= 0.35;
        if on_bed && raft_top > 0.0 {
            z_base = raft_top - 0.12;
        }
        let trunk_r = style.trunk_mm * 0.5;
        let tip_end = tip_end_z(host, z_base, &style);
        add_tip(&mut mesh, host, tip_end, &style);
        let shaft_top = tip_end;
        add_foot_and_shaft(
            &mut mesh, host.x, host.y, z_base, shaft_top, trunk_r, on_bed, &style,
        );
        trunks.push(Trunk {
            x: host.x,
            y: host.y,
            z_base,
            z_top: host.z_top,
            radius: trunk_r,
        });
        let ceiling = shaft_top;
        let floor = z_base + style.foot_mm + 0.25;
        for &idx in &group {
            if idx == anchor {
                continue;
            }
            let tip = &supports[idx];
            let branch_end = tip_end_z(tip, z_base, &style);
            add_tip(&mut mesh, tip, branch_end, &style);
            let horiz = ((tip.x - host.x).powi(2) + (tip.y - host.y).powi(2)).sqrt();
            let drop = horiz / style.branch_deg.to_radians().tan().max(0.15);
            let join_z = if branch_end > ceiling {
                ceiling
            } else {
                (branch_end - drop).clamp(floor.min(ceiling), ceiling)
            };
            let from = Vec3::new(tip.x, tip.y, branch_end);
            let to = Vec3::new(host.x, host.y, join_z);
            if from.distance(to) > 0.2 {
                mesh.append(&tapered(
                    from,
                    style.tip_lower_mm * 0.5,
                    to,
                    trunk_r * 0.92,
                    8,
                ));
                mesh.append(&sphere(to, trunk_r * 0.85, 5, 7));
            }
        }
    }
    if braces_on {
        for brace in trunk_braces(&trunks, brace_dist, style.brace_mm * 0.5) {
            mesh.append(&tapered(brace.a, brace.radius, brace.b, brace.radius, 6));
        }
    }
    mesh
}

/// The contact alone, used to highlight the support under the cursor.
pub fn tip_marker(support: &Support, style: &SupportStyle) -> Mesh {
    let style = style.sanitized();
    let mut mesh = Mesh {
        vertices: vec![],
        indices: vec![],
    };
    let end = tip_end_z(support, support.z_base.max(0.0), &style);
    add_tip(&mut mesh, support, end, &style);
    mesh
}

fn tip_end_z(support: &Support, z_base: f32, style: &SupportStyle) -> f32 {
    let span = (support.z_top - z_base).max(0.5);
    let len = style.tip_len_mm.min(span * 0.55).max(0.35);
    (support.z_top - len).max(z_base + 0.2)
}

fn add_tip(mesh: &mut Mesh, support: &Support, tip_end_z: f32, style: &SupportStyle) {
    let contact_r = style.contact_mm * 0.5;
    let upper_r = (style.tip_upper_mm * 0.5).max(contact_r);
    let lower_r = (style.tip_lower_mm * 0.5).max(upper_r * 0.8);
    if style.ball {
        let r = contact_r.max(0.08);
        let center = Vec3::new(
            support.x,
            support.y,
            support.z_top + style.contact_depth * 0.45,
        );
        mesh.append(&sphere(center, r, 6, 8));
        let neck = Vec3::new(
            support.x,
            support.y,
            (center.z - r * 0.65).min(support.z_top),
        );
        let end = Vec3::new(support.x, support.y, tip_end_z.min(neck.z - 0.05));
        if neck.z - end.z > 0.12 {
            mesh.append(&tapered(neck, r * 0.9, end, lower_r, 8));
        }
        return;
    }
    let apex = Vec3::new(support.x, support.y, support.z_top + style.contact_depth);
    let surface = Vec3::new(support.x, support.y, support.z_top);
    let end = Vec3::new(support.x, support.y, tip_end_z);
    mesh.append(&tapered(
        apex,
        (contact_r * 0.35).max(0.04),
        surface,
        contact_r.max(0.06),
        8,
    ));
    if surface.z - end.z > 0.12 {
        mesh.append(&tapered(surface, upper_r, end, lower_r, 8));
    }
}

fn add_foot_and_shaft(
    mesh: &mut Mesh,
    x: f32,
    y: f32,
    z_base: f32,
    shaft_top: f32,
    trunk_r: f32,
    on_bed: bool,
    style: &SupportStyle,
) {
    let foot_h = if on_bed {
        style.foot_mm.min((shaft_top - z_base).max(0.0) * 0.45)
    } else {
        style.contact_depth.min(0.4)
    };
    let shaft_bottom = if foot_h > 0.12 {
        z_base + foot_h
    } else {
        z_base
    };
    if shaft_top - shaft_bottom > 0.15 {
        mesh.append(&tapered(
            Vec3::new(x, y, shaft_bottom),
            trunk_r,
            Vec3::new(x, y, shaft_top),
            trunk_r,
            10,
        ));
    }
    if on_bed && foot_h > 0.12 {
        mesh.append(&tapered(
            Vec3::new(x, y, z_base),
            trunk_r * 2.1,
            Vec3::new(x, y, shaft_bottom),
            trunk_r,
            10,
        ));
    } else if !on_bed && foot_h > 0.08 {
        // Lower contact where the trunk lands on the model.
        let sole = Vec3::new(x, y, z_base - style.contact_depth * 0.5);
        mesh.append(&tapered(
            sole,
            style.contact_mm * 0.22,
            Vec3::new(x, y, shaft_bottom),
            trunk_r,
            8,
        ));
    }
}

fn cluster(supports: &[Support], cell: f32) -> Vec<Vec<usize>> {
    let cell = cell.max(1.0);
    let mut map: HashMap<(i32, i32, i32), Vec<usize>> = HashMap::new();
    for (i, s) in supports.iter().enumerate() {
        let bed = if s.z_base <= 0.35 { 0 } else { 1 };
        let band = if bed == 0 {
            0
        } else {
            (s.z_base / cell).floor() as i32
        };
        let key = (
            (s.x / cell).floor() as i32,
            (s.y / cell).floor() as i32,
            bed * 10_000 + band,
        );
        map.entry(key).or_default().push(i);
    }
    map.into_values().collect()
}

fn anchor_index(supports: &[Support], group: &[usize]) -> usize {
    let (cx, cy) = group.iter().fold((0.0f32, 0.0f32), |(x, y), &i| {
        (x + supports[i].x, y + supports[i].y)
    });
    let n = group.len() as f32;
    let (cx, cy) = (cx / n, cy / n);
    group
        .iter()
        .copied()
        .min_by(|&a, &b| {
            let da = (supports[a].x - cx).powi(2) + (supports[a].y - cy).powi(2);
            let db = (supports[b].x - cx).powi(2) + (supports[b].y - cy).powi(2);
            da.total_cmp(&db)
                .then_with(|| supports[a].z_base.total_cmp(&supports[b].z_base))
        })
        .unwrap_or(group[0])
}

fn trunk_braces(trunks: &[Trunk], max_dist: f32, radius: f32) -> Vec<Brace> {
    let mut out = Vec::new();
    let mut used = vec![0u8; trunks.len()];
    for i in 0..trunks.len() {
        if used[i] >= 2 {
            continue;
        }
        let a = &trunks[i];
        let mut best: Option<(usize, f32)> = None;
        for j in (i + 1)..trunks.len() {
            if used[j] >= 2 {
                continue;
            }
            let b = &trunks[j];
            let dx = a.x - b.x;
            let dy = a.y - b.y;
            let dist = (dx * dx + dy * dy).sqrt();
            if dist < a.radius * 2.0 || dist > max_dist {
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
            let b = &trunks[j];
            let z = ((a.z_base + a.z_top) * 0.5)
                .min((b.z_base + b.z_top) * 0.5)
                .clamp(a.z_base.max(b.z_base) + 1.2, a.z_top.min(b.z_top) - 1.5);
            out.push(Brace {
                a: Vec3::new(a.x, a.y, z),
                b: Vec3::new(b.x, b.y, z),
                radius,
            });
        }
    }
    out
}

pub fn raft_mesh(supports: &[Support], margin: f32, thickness: f32, reach: f32) -> Option<Mesh> {
    let bed: Vec<&Support> = supports.iter().filter(|s| s.z_base <= 0.35).collect();
    if bed.is_empty() || thickness <= 0.0 {
        return None;
    }
    let mut min_x = f32::MAX;
    let mut min_y = f32::MAX;
    let mut max_x = f32::MIN;
    let mut max_y = f32::MIN;
    for s in bed {
        min_x = min_x.min(s.x - reach);
        min_y = min_y.min(s.y - reach);
        max_x = max_x.max(s.x + reach);
        max_y = max_y.max(s.y + reach);
    }
    Some(crate::mesh::box_mesh(
        [min_x - margin, min_y - margin, 0.0],
        [max_x + margin, max_y + margin, thickness],
    ))
}

pub fn brace_mesh(brace: &Brace) -> Mesh {
    tapered(brace.a, brace.radius, brace.b, brace.radius, 8)
}

fn sphere(center: Vec3, radius: f32, stacks: usize, slices: usize) -> Mesh {
    let stacks = stacks.max(2);
    let slices = slices.max(3);
    let mut vertices = Vec::new();
    for stack in 0..=stacks {
        let v = stack as f32 / stacks as f32;
        let phi = std::f32::consts::PI * v;
        for slice_i in 0..=slices {
            let u = slice_i as f32 / slices as f32;
            let theta = std::f32::consts::TAU * u;
            vertices.push(
                (center
                    + Vec3::new(
                        radius * phi.sin() * theta.cos(),
                        radius * phi.sin() * theta.sin(),
                        radius * phi.cos(),
                    ))
                .to_array(),
            );
        }
    }
    let row = slices + 1;
    let mut indices = Vec::new();
    for stack in 0..stacks {
        for slice_i in 0..slices {
            let a = stack * row + slice_i;
            let b = a + row;
            indices.extend_from_slice(&[a as u32, b as u32, (a + 1) as u32]);
            indices.extend_from_slice(&[b as u32, (b + 1) as u32, (a + 1) as u32]);
        }
    }
    Mesh { vertices, indices }
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
        let style = PRESETS[1].style;
        let supports = auto_supports(&mesh.vertices, &mesh.indices, &style, false, &mut id);
        assert!(
            supports.len() >= 3,
            "expected several supports under the bridge, got {}",
            supports.len()
        );
        assert!(supports.iter().all(|s| s.z_top > s.z_base));
    }

    #[test]
    fn nearby_tips_share_a_trunk_and_grow_a_branch() {
        let supports = [
            Support {
                id: 1,
                x: 0.0,
                y: 0.0,
                z_top: 22.0,
                z_base: 0.0,
            },
            Support {
                id: 2,
                x: 3.2,
                y: 0.4,
                z_top: 20.0,
                z_base: 0.0,
            },
        ];
        let mesh = forest_mesh(&supports, &PRESETS[1].style, 0.0, true, 8.0);
        assert!(mesh.triangle_count() > 40);
        let joint = mesh
            .vertices
            .iter()
            .any(|v| (v[0] - 0.0).abs() < 0.35 && v[1].abs() < 0.8 && v[2] > 2.0 && v[2] < 19.0);
        assert!(joint, "expected the branch to meet the trunk");
        let reached = mesh
            .vertices
            .iter()
            .any(|v| (v[0] - 3.2).abs() < 0.3 && v[2] > 18.0);
        assert!(reached, "expected a tip on the second contact");
    }
}
