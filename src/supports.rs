//! Branching resin supports: a contact tip, a short neck, and a branch into a trunk.
//!
//! Auto supports sample downward faces on a spacing grid. Nearby tips share one
//! trunk, the way a classic resin tree does, instead of each tip growing its
//! own column to the bed. The overhang angle is measured from vertical: a
//! higher angle keeps supports off of steeper walls.

use crate::mesh::{face_normal, Mesh};
use glam::Vec3;
use rayon::prelude::*;
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
    /// Bottom diameter of a foot on the bed. 0 keeps a little over twice the trunk.
    #[serde(default = "default_foot_diam")]
    pub foot_diam_mm: f32,
    pub brace_mm: f32,
}

fn default_foot_diam() -> f32 {
    0.0
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
            foot_diam_mm: 1.9,
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
            foot_diam_mm: 2.5,
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
            foot_diam_mm: 3.6,
            brace_mm: 0.55,
        },
    },
    SupportPreset {
        name: "Hairpin",
        style: SupportStyle {
            overhang_deg: 30.0,
            spacing_mm: 1.2,
            contact_mm: 0.15,
            contact_depth: 0.12,
            ball: false,
            tip_upper_mm: 0.18,
            tip_lower_mm: 0.32,
            tip_len_mm: 1.2,
            trunk_mm: 0.50,
            branch_deg: 35.0,
            cluster_mm: 3.5,
            foot_mm: 0.4,
            foot_diam_mm: 1.05,
            brace_mm: 0.18,
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
            foot_diam_mm: if self.foot_diam_mm <= 0.05 {
                0.0
            } else {
                self.foot_diam_mm.clamp(0.4, 12.0)
            },
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

fn face_samples(
    a: [f32; 3],
    b: [f32; 3],
    c: [f32; 3],
    spacing: f32,
    overhang_deg: f32,
) -> Vec<[f32; 3]> {
    let n = face_normal(a, b, c);
    if !needs_support(n, overhang_deg) {
        return Vec::new();
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
        let mut x = (min_x / spacing).floor() * spacing;
        while x <= max_x {
            let mut y = (min_y / spacing).floor() * spacing;
            while y <= max_y {
                if let Some(z) = vertical_hit(x, y, a, b, c) {
                    samples.push([x, y, z]);
                }
                y += spacing;
            }
            x += spacing;
        }
    }
    samples.retain(|s| s[2] >= 0.4);
    samples
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
    let spacing = style.spacing_mm;
    let overhang = style.overhang_deg;
    let cells = indices
        .par_chunks_exact(3)
        .fold(HashMap::<(i32, i32), [f32; 3]>::new, |mut cells, tri| {
            let a = verts[tri[0] as usize];
            let b = verts[tri[1] as usize];
            let c = verts[tri[2] as usize];
            for sample in face_samples(a, b, c, spacing, overhang) {
                let key = (
                    (sample[0] / spacing).floor() as i32,
                    (sample[1] / spacing).floor() as i32,
                );
                cells
                    .entry(key)
                    .and_modify(|old| {
                        if sample[2] < old[2] {
                            *old = sample;
                        }
                    })
                    .or_insert(sample);
            }
            cells
        })
        .reduce(HashMap::<(i32, i32), [f32; 3]>::new, |mut left, right| {
            for (key, sample) in right {
                left.entry(key)
                    .and_modify(|old| {
                        if sample[2] < old[2] {
                            *old = sample;
                        }
                    })
                    .or_insert(sample);
            }
            left
        });
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
    brace_angle: f32,
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
        for brace in trunk_braces(&trunks, brace_dist, style.brace_mm * 0.5, brace_angle) {
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
        let foot_r = if style.foot_diam_mm > 0.05 {
            style.foot_diam_mm * 0.5
        } else {
            trunk_r * 2.1
        };
        mesh.append(&tapered(
            Vec3::new(x, y, z_base),
            foot_r,
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

fn trunk_braces(trunks: &[Trunk], max_dist: f32, radius: f32, angle_deg: f32) -> Vec<Brace> {
    // Same non-crossing links as before, but each one rises at 45° to the
    // bed. Alternate layers lean the other way so the columns stay braced.
    if trunks.len() < 2 || max_dist < 0.5 || radius <= 0.0 {
        return Vec::new();
    }
    let step = max_dist.clamp(3.0, 20.0);
    let z_min = trunks.iter().map(|t| t.z_base).fold(f32::MAX, f32::min);
    let z_max = trunks.iter().map(|t| t.z_top).fold(f32::MIN, f32::max);
    let mut out = Vec::new();
    let mut z = z_min + step * 0.5;
    for level in 0..48 {
        if z >= z_max - 1.0 {
            break;
        }
        add_rung(
            &mut out,
            trunks,
            z,
            max_dist,
            radius,
            level % 2 == 1,
            angle_deg,
        );
        z += step;
    }
    out
}

fn trunk_holds(trunk: &Trunk, z: f32) -> bool {
    trunk.z_base + 0.6 < z && z < trunk.z_top - 0.6
}

fn add_rung(
    out: &mut Vec<Brace>,
    trunks: &[Trunk],
    z: f32,
    max_dist: f32,
    radius: f32,
    flip: bool,
    angle_deg: f32,
) {
    let nodes: Vec<usize> = trunks
        .iter()
        .enumerate()
        .filter(|(_, t)| t.z_base + 0.8 < z + max_dist && z < t.z_top - 0.8)
        .map(|(i, _)| i)
        .collect();
    if nodes.len() < 2 {
        return;
    }
    let mut edges = Vec::new();
    for a in 0..nodes.len() {
        for b in (a + 1)..nodes.len() {
            let ia = nodes[a];
            let ib = nodes[b];
            let dx = trunks[ia].x - trunks[ib].x;
            let dy = trunks[ia].y - trunks[ib].y;
            let dist = (dx * dx + dy * dy).sqrt();
            let min_sep = (trunks[ia].radius + trunks[ib].radius) * 1.2;
            if dist >= min_sep && dist <= max_dist {
                edges.push((dist, ia, ib));
            }
        }
    }
    edges.sort_by(|p, q| p.0.total_cmp(&q.0));
    let mut parent: Vec<usize> = (0..trunks.len()).collect();
    for &(dist, mut ia, mut ib) in &edges {
        let ra = find_root(&mut parent, ia);
        let rb = find_root(&mut parent, ib);
        if ra == rb {
            continue;
        }
        parent[ra] = rb;
        if (trunks[ib].x, trunks[ib].y) < (trunks[ia].x, trunks[ia].y) {
            std::mem::swap(&mut ia, &mut ib);
        }
        let rise = dist * angle_deg.clamp(5.0, 80.0).to_radians().tan();
        let (za, zb) = if flip { (z + rise, z) } else { (z, z + rise) };
        if trunk_holds(&trunks[ia], za) && trunk_holds(&trunks[ib], zb) {
            out.push(Brace {
                a: Vec3::new(trunks[ia].x, trunks[ia].y, za),
                b: Vec3::new(trunks[ib].x, trunks[ib].y, zb),
                radius,
            });
        }
    }
}

fn find_root(parent: &mut [usize], mut i: usize) -> usize {
    while parent[i] != i {
        parent[i] = parent[parent[i]];
        i = parent[i];
    }
    i
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

/// One skate raft for a part: the outline of the mesh, grown by `oversize`,
/// with the outer wall leaning out. `angle_deg` is measured from vertical,
/// so 45° makes the base wider than the top by the raft thickness.
pub fn skate_raft(
    vertices: &[[f32; 3]],
    indices: &[u32],
    thickness: f32,
    oversize: f32,
    angle_deg: f32,
) -> Option<Mesh> {
    let thickness = thickness.clamp(0.2, 5.0);
    let oversize = oversize.clamp(0.0, 30.0);
    let flare = thickness * angle_deg.clamp(0.0, 70.0).to_radians().tan();
    if vertices.is_empty() || indices.len() < 3 {
        return None;
    }
    let cell = 0.8_f32;
    let mut min_x = f32::MAX;
    let mut min_y = f32::MAX;
    let mut max_x = f32::MIN;
    let mut max_y = f32::MIN;
    for v in vertices {
        min_x = min_x.min(v[0]);
        min_y = min_y.min(v[1]);
        max_x = max_x.max(v[0]);
        max_y = max_y.max(v[1]);
    }
    if !min_x.is_finite() {
        return None;
    }
    let pad = oversize + flare + cell * 2.0;
    let origin_x = min_x - pad;
    let origin_y = min_y - pad;
    let w = ((max_x - min_x + pad * 2.0) / cell).ceil() as usize + 1;
    let h = ((max_y - min_y + pad * 2.0) / cell).ceil() as usize + 1;
    if w < 2 || h < 2 || w.saturating_mul(h) > 2_000_000 {
        return None;
    }
    let mut mask = vec![false; w * h];
    for tri in indices.chunks_exact(3) {
        stamp_tri(
            &mut mask,
            origin_x,
            origin_y,
            cell,
            w,
            h,
            vertices[tri[0] as usize],
            vertices[tri[1] as usize],
            vertices[tri[2] as usize],
        );
    }
    fill_silhouette_holes(&mut mask, w, h);
    if !mask.iter().any(|bit| *bit) {
        return None;
    }
    let dist = distance_outside(&mask, w, h, cell);
    let mesh = heightfield(
        origin_x, origin_y, cell, w, h, &dist, thickness, oversize, flare,
    );
    if mesh.triangle_count() == 0 {
        None
    } else {
        Some(mesh)
    }
}

fn stamp_tri(
    mask: &mut [bool],
    origin_x: f32,
    origin_y: f32,
    cell: f32,
    w: usize,
    h: usize,
    a: [f32; 3],
    b: [f32; 3],
    c: [f32; 3],
) {
    let min_x = a[0].min(b[0]).min(c[0]);
    let max_x = a[0].max(b[0]).max(c[0]);
    let min_y = a[1].min(b[1]).min(c[1]);
    let max_y = a[1].max(b[1]).max(c[1]);
    let x0 = (((min_x - origin_x) / cell).floor() as isize).clamp(0, w as isize - 1) as usize;
    let x1 = (((max_x - origin_x) / cell).ceil() as isize).clamp(0, w as isize - 1) as usize;
    let y0 = (((min_y - origin_y) / cell).floor() as isize).clamp(0, h as isize - 1) as usize;
    let y1 = (((max_y - origin_y) / cell).ceil() as isize).clamp(0, h as isize - 1) as usize;
    for y in y0..=y1 {
        for x in x0..=x1 {
            let px = origin_x + (x as f32 + 0.5) * cell;
            let py = origin_y + (y as f32 + 0.5) * cell;
            if point_in_tri(px, py, a, b, c) {
                mask[y * w + x] = true;
            }
        }
    }
}

fn point_in_tri(px: f32, py: f32, a: [f32; 3], b: [f32; 3], c: [f32; 3]) -> bool {
    let denom = (b[1] - c[1]) * (a[0] - c[0]) + (c[0] - b[0]) * (a[1] - c[1]);
    if denom.abs() < 1e-8 {
        return false;
    }
    let w0 = ((b[1] - c[1]) * (px - c[0]) + (c[0] - b[0]) * (py - c[1])) / denom;
    let w1 = ((c[1] - a[1]) * (px - c[0]) + (a[0] - c[0]) * (py - c[1])) / denom;
    let w2 = 1.0 - w0 - w1;
    w0 >= -1e-3 && w1 >= -1e-3 && w2 >= -1e-3
}

fn fill_silhouette_holes(mask: &mut [bool], w: usize, h: usize) {
    let mut outside = vec![false; mask.len()];
    let mut stack = Vec::new();
    for x in 0..w {
        stack.push(x);
        stack.push((h - 1) * w + x);
    }
    for y in 0..h {
        stack.push(y * w);
        stack.push(y * w + (w - 1));
    }
    while let Some(i) = stack.pop() {
        if outside[i] || mask[i] {
            continue;
        }
        outside[i] = true;
        let x = i % w;
        let y = i / w;
        if x > 0 {
            stack.push(i - 1);
        }
        if x + 1 < w {
            stack.push(i + 1);
        }
        if y > 0 {
            stack.push(i - w);
        }
        if y + 1 < h {
            stack.push(i + w);
        }
    }
    for (bit, out) in mask.iter_mut().zip(outside.iter()) {
        if !out {
            *bit = true;
        }
    }
}

fn distance_outside(mask: &[bool], w: usize, h: usize, cell: f32) -> Vec<f32> {
    let diag = cell * std::f32::consts::SQRT_2;
    let mut dist = vec![1.0e9_f32; w * h];
    for (i, bit) in mask.iter().enumerate() {
        if *bit {
            dist[i] = 0.0;
        }
    }
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let mut d = dist[i];
            if x > 0 {
                d = d.min(dist[i - 1] + cell);
            }
            if y > 0 {
                d = d.min(dist[i - w] + cell);
            }
            if x > 0 && y > 0 {
                d = d.min(dist[i - w - 1] + diag);
            }
            if x + 1 < w && y > 0 {
                d = d.min(dist[i - w + 1] + diag);
            }
            dist[i] = d;
        }
    }
    for y in (0..h).rev() {
        for x in (0..w).rev() {
            let i = y * w + x;
            let mut d = dist[i];
            if x + 1 < w {
                d = d.min(dist[i + 1] + cell);
            }
            if y + 1 < h {
                d = d.min(dist[i + w] + cell);
            }
            if x + 1 < w && y + 1 < h {
                d = d.min(dist[i + w + 1] + diag);
            }
            if x > 0 && y + 1 < h {
                d = d.min(dist[i + w - 1] + diag);
            }
            dist[i] = d;
        }
    }
    dist
}

fn raft_height(dist: f32, thickness: f32, oversize: f32, flare: f32) -> f32 {
    if dist <= oversize {
        thickness
    } else if flare < 0.05 || dist >= oversize + flare {
        0.0
    } else {
        thickness * (1.0 - (dist - oversize) / flare)
    }
}

fn heightfield(
    origin_x: f32,
    origin_y: f32,
    cell: f32,
    w: usize,
    h: usize,
    dist: &[f32],
    thickness: f32,
    oversize: f32,
    flare: f32,
) -> Mesh {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    let tri = |vertices: &mut Vec<[f32; 3]>, indices: &mut Vec<u32>, a, b, c| {
        let base = vertices.len() as u32;
        vertices.push(a);
        vertices.push(b);
        vertices.push(c);
        indices.extend_from_slice(&[base, base + 1, base + 2]);
    };
    let height_at = |x: i32, y: i32| -> f32 {
        if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
            0.0
        } else {
            raft_height(
                dist[y as usize * w + x as usize],
                thickness,
                oversize,
                flare,
            )
        }
    };
    for y in 0..h {
        for x in 0..w {
            let z = height_at(x as i32, y as i32);
            if z < 0.02 {
                continue;
            }
            let x0 = origin_x + x as f32 * cell;
            let y0 = origin_y + y as f32 * cell;
            let x1 = x0 + cell;
            let y1 = y0 + cell;
            tri(
                &mut vertices,
                &mut indices,
                [x0, y0, z],
                [x1, y0, z],
                [x1, y1, z],
            );
            tri(
                &mut vertices,
                &mut indices,
                [x0, y0, z],
                [x1, y1, z],
                [x0, y1, z],
            );
            tri(
                &mut vertices,
                &mut indices,
                [x0, y0, 0.0],
                [x1, y1, 0.0],
                [x1, y0, 0.0],
            );
            tri(
                &mut vertices,
                &mut indices,
                [x0, y0, 0.0],
                [x0, y1, 0.0],
                [x1, y1, 0.0],
            );
            let walls = [
                (0, -1, [x0, y0], [x1, y0]),
                (0, 1, [x1, y1], [x0, y1]),
                (-1, 0, [x0, y1], [x0, y0]),
                (1, 0, [x1, y0], [x1, y1]),
            ];
            for (dx, dy, p0, p1) in walls {
                let zn = height_at(x as i32 + dx, y as i32 + dy);
                if zn + 0.02 >= z {
                    continue;
                }
                tri(
                    &mut vertices,
                    &mut indices,
                    [p0[0], p0[1], z],
                    [p0[0], p0[1], zn],
                    [p1[0], p1[1], zn],
                );
                tri(
                    &mut vertices,
                    &mut indices,
                    [p0[0], p0[1], z],
                    [p1[0], p1[1], zn],
                    [p1[0], p1[1], z],
                );
            }
        }
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
        let mesh = forest_mesh(&supports, &PRESETS[1].style, 0.0, true, 8.0, 45.0);
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

    #[test]
    fn skate_raft_follows_the_part_and_leans_out() {
        let mesh = crate::mesh::box_mesh([0.0, 0.0, 0.0], [10.0, 10.0, 8.0]);
        let raft = skate_raft(&mesh.vertices, &mesh.indices, 1.0, 2.0, 45.0).unwrap();
        let (min, max) = raft.bounds().unwrap();
        assert!(
            min[0] < -1.5,
            "oversize should pass the outline, min x {}",
            min[0]
        );
        assert!(
            max[0] > 11.5,
            "oversize should pass the outline, max x {}",
            max[0]
        );
        assert!(
            raft.vertices.iter().any(|v| v[2] > 0.8),
            "the top of the raft is the thickness"
        );
        assert!(
            raft.vertices
                .iter()
                .any(|v| v[2] < 0.05 && (v[0] < -0.5 || v[0] > 10.5)),
            "the leaned wall meets the bed outside the part"
        );
    }

    #[test]
    fn braces_rise_at_45_degrees() {
        let trunks = [
            Trunk {
                x: 0.0,
                y: 0.0,
                z_base: 0.0,
                z_top: 40.0,
                radius: 0.6,
            },
            Trunk {
                x: 10.0,
                y: 0.0,
                z_base: 0.0,
                z_top: 40.0,
                radius: 0.6,
            },
            Trunk {
                x: 10.0,
                y: 10.0,
                z_base: 0.0,
                z_top: 40.0,
                radius: 0.6,
            },
            Trunk {
                x: 0.0,
                y: 10.0,
                z_base: 0.0,
                z_top: 40.0,
                radius: 0.6,
            },
        ];
        let braces = trunk_braces(&trunks, 12.0, 0.2, 45.0);
        let level: Vec<_> = braces
            .iter()
            .filter(|b| {
                let low = b.a.z.min(b.b.z);
                low > 5.0 && low < 7.0
            })
            .collect();
        assert_eq!(
            level.len(),
            3,
            "a square of trunks keeps three sides, not both diagonals"
        );
        assert!(braces.len() >= 6, "braces repeat up the trunks");
        for brace in &braces {
            let horiz = ((brace.a.x - brace.b.x).powi(2) + (brace.a.y - brace.b.y).powi(2)).sqrt();
            let rise = (brace.a.z - brace.b.z).abs();
            let angle = rise.atan2(horiz).to_degrees();
            assert!(
                (angle - 45.0).abs() < 1.0,
                "brace angle {angle}°, rise {rise}, run {horiz}"
            );
        }
    }
}
