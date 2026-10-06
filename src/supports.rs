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

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Support {
    pub id: u64,
    /// Model this pillar was grown for. Later edits stay on that model.
    pub object_id: u64,
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

    fn nearest_at(&self, best: &mut Option<(f32, f32)>, t: usize, x: f32, y: f32, z: f32) {
        let tri = &self.indices[t * 3..t * 3 + 3];
        let a = self.verts[tri[0] as usize];
        let b = self.verts[tri[1] as usize];
        let c = self.verts[tri[2] as usize];
        if let Some(hit_z) = vertical_hit(x, y, a, b, c) {
            let dist = (hit_z - z).abs();
            if best.map(|(old, _)| dist < old).unwrap_or(true) {
                *best = Some((dist, hit_z));
            }
        }
    }

    /// The triangle the vertical line hits that sits closest to `z`.
    fn nearest_surface(&self, x: f32, y: f32, z: f32) -> Option<f32> {
        let key = (
            (x / self.cell).floor() as i32,
            (y / self.cell).floor() as i32,
        );
        let mut best: Option<(f32, f32)> = None;
        if let Some(tris) = self.map.get(&key) {
            for &t in tris {
                self.nearest_at(&mut best, t, x, y, z);
            }
        }
        for &t in &self.large {
            self.nearest_at(&mut best, t, x, y, z);
        }
        best.map(|(_, hit_z)| hit_z)
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
            object_id: 0,
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
        object_id: 0,
        x,
        y,
        z_top: z,
        z_base: base,
    }
}

/// Drop a tip onto the surface nearest `hint_z` at this column.
/// Returns nothing when the column misses the mesh, so a nudge cannot float the tip.
pub fn seat_on_mesh(
    x: f32,
    y: f32,
    hint_z: f32,
    verts: &[[f32; 3]],
    indices: &[u32],
    platform_only: bool,
    id: u64,
) -> Option<Support> {
    if verts.is_empty() || indices.len() < 3 {
        return None;
    }
    let grid = TriGrid::build(verts, indices);
    let z = grid.nearest_surface(x, y, hint_z)?;
    let base = if platform_only {
        0.0
    } else {
        grid.highest_below(x, y, z).unwrap_or(0.0).max(0.0)
    };
    Some(Support {
        id,
        object_id: 0,
        x,
        y,
        z_top: z,
        z_base: base,
    })
}

struct Trunk {
    x: f32,
    y: f32,
    z_base: f32,
    z_top: f32,
    radius: f32,
}

fn empty_mesh() -> Mesh {
    Mesh {
        vertices: vec![],
        indices: vec![],
    }
}

/// The support forest split so the plate can show one piece at a time.
/// The slice still uses [`forest_mesh`], which is these pieces joined.
#[derive(Clone, Debug)]
pub struct SupportParts {
    pub contacts: Mesh,
    pub necks: Mesh,
    pub trunks: Mesh,
    pub feet: Mesh,
    pub branches: Mesh,
    pub braces: Mesh,
}

impl SupportParts {
    pub fn empty() -> Self {
        Self {
            contacts: empty_mesh(),
            necks: empty_mesh(),
            trunks: empty_mesh(),
            feet: empty_mesh(),
            branches: empty_mesh(),
            braces: empty_mesh(),
        }
    }

    pub fn append(&mut self, other: &SupportParts) {
        self.contacts.append(&other.contacts);
        self.necks.append(&other.necks);
        self.trunks.append(&other.trunks);
        self.feet.append(&other.feet);
        self.branches.append(&other.branches);
        self.braces.append(&other.braces);
    }

    pub fn append_into(&self, mesh: &mut Mesh) {
        mesh.append(&self.contacts);
        mesh.append(&self.necks);
        mesh.append(&self.trunks);
        mesh.append(&self.feet);
        mesh.append(&self.branches);
        mesh.append(&self.braces);
    }
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
    let mut mesh = empty_mesh();
    forest_parts(
        supports,
        style,
        raft_top,
        braces_on,
        brace_dist,
        brace_angle,
    )
    .append_into(&mut mesh);
    mesh
}

/// The same forest as [`forest_mesh`], kept in separate meshes for the view.
pub fn forest_parts(
    supports: &[Support],
    style: &SupportStyle,
    raft_top: f32,
    braces_on: bool,
    brace_dist: f32,
    brace_angle: f32,
) -> SupportParts {
    let style = style.sanitized();
    let mut parts = SupportParts::empty();
    if supports.is_empty() {
        return parts;
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
        add_tip(&mut parts.contacts, &mut parts.necks, host, tip_end, &style);
        let shaft_top = tip_end;
        add_foot_and_shaft(
            &mut parts.trunks,
            &mut parts.feet,
            host.x,
            host.y,
            z_base,
            shaft_top,
            trunk_r,
            on_bed,
            &style,
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
            add_tip(
                &mut parts.contacts,
                &mut parts.necks,
                tip,
                branch_end,
                &style,
            );
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
                parts.branches.append(&tapered(
                    from,
                    style.tip_lower_mm * 0.5,
                    to,
                    trunk_r * 0.92,
                    8,
                ));
                parts.branches.append(&sphere(to, trunk_r * 0.85, 5, 7));
            }
        }
    }
    if braces_on {
        for brace in trunk_braces(&trunks, brace_dist, style.brace_mm * 0.5, brace_angle) {
            parts
                .braces
                .append(&tapered(brace.a, brace.radius, brace.b, brace.radius, 6));
        }
    }
    parts
}

/// The contact alone, used to highlight the support under the cursor.
pub fn tip_marker(support: &Support, style: &SupportStyle) -> Mesh {
    contact_marker(support, style)
}

/// Just the point or ball that touches the model, without the neck.
pub fn contact_marker(support: &Support, style: &SupportStyle) -> Mesh {
    let style = style.sanitized();
    let mut contacts = empty_mesh();
    let mut necks = empty_mesh();
    let end = tip_end_z(support, support.z_base.max(0.0), &style);
    add_tip(&mut contacts, &mut necks, support, end, &style);
    contacts
}

fn tip_end_z(support: &Support, z_base: f32, style: &SupportStyle) -> f32 {
    let span = (support.z_top - z_base).max(0.5);
    let len = style.tip_len_mm.min(span * 0.55).max(0.35);
    (support.z_top - len).max(z_base + 0.2)
}

fn add_tip(
    contacts: &mut Mesh,
    necks: &mut Mesh,
    support: &Support,
    tip_end_z: f32,
    style: &SupportStyle,
) {
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
        contacts.append(&sphere(center, r, 6, 8));
        let neck = Vec3::new(
            support.x,
            support.y,
            (center.z - r * 0.65).min(support.z_top),
        );
        let end = Vec3::new(support.x, support.y, tip_end_z.min(neck.z - 0.05));
        if neck.z - end.z > 0.12 {
            necks.append(&tapered(neck, r * 0.9, end, lower_r, 8));
        }
        return;
    }
    let apex = Vec3::new(support.x, support.y, support.z_top + style.contact_depth);
    let surface = Vec3::new(support.x, support.y, support.z_top);
    let end = Vec3::new(support.x, support.y, tip_end_z);
    contacts.append(&tapered(
        apex,
        (contact_r * 0.35).max(0.04),
        surface,
        contact_r.max(0.06),
        8,
    ));
    if surface.z - end.z > 0.12 {
        necks.append(&tapered(surface, upper_r, end, lower_r, 8));
    }
}

fn add_foot_and_shaft(
    trunks: &mut Mesh,
    feet: &mut Mesh,
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
        trunks.append(&tapered(
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
        feet.append(&tapered(
            Vec3::new(x, y, z_base),
            foot_r,
            Vec3::new(x, y, shaft_bottom),
            trunk_r,
            10,
        ));
    } else if !on_bed && foot_h > 0.08 {
        // Lower contact where the trunk lands on the model.
        let sole = Vec3::new(x, y, z_base - style.contact_depth * 0.5);
        feet.append(&tapered(
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

/// Skate under the supports that reach the bed. Each pillar gets a square pad.
/// Pads that sit near each other are joined, so the outline follows the
/// supports instead of the whole part. `oversize` grows each square.
/// `angle_deg` is the wall measured from vertical. The top is wider than the
/// bed contact, so the lip overhangs and a scraper can get under it.
pub fn support_raft(
    supports: &[Support],
    foot_diam_mm: f32,
    thickness: f32,
    oversize: f32,
    angle_deg: f32,
) -> Option<Mesh> {
    let thickness = thickness.clamp(0.2, 5.0);
    let oversize = oversize.clamp(0.0, 30.0);
    let flare = thickness * angle_deg.clamp(0.0, 70.0).to_radians().tan();
    let half = (foot_diam_mm.max(0.8) * 0.5 + oversize).clamp(1.0, 40.0);
    let pillars: Vec<(f32, f32)> = supports
        .iter()
        .filter(|s| s.z_base <= 0.45)
        .map(|s| (s.x, s.y))
        .collect();
    if pillars.is_empty() {
        return None;
    }
    let cell = 0.25_f32;
    let pad = half + flare + cell * 2.0;
    let mut min_x = f32::MAX;
    let mut min_y = f32::MAX;
    let mut max_x = f32::MIN;
    let mut max_y = f32::MIN;
    for &(x, y) in &pillars {
        min_x = min_x.min(x - half);
        min_y = min_y.min(y - half);
        max_x = max_x.max(x + half);
        max_y = max_y.max(y + half);
    }
    let origin_x = min_x - pad;
    let origin_y = min_y - pad;
    let w = ((max_x - min_x + pad * 2.0) / cell).ceil() as usize + 2;
    let h = ((max_y - min_y + pad * 2.0) / cell).ceil() as usize + 2;
    if w < 2 || h < 2 || w.saturating_mul(h) > 4_000_000 {
        return None;
    }
    let mut solid = vec![false; w * h];
    for &(cx, cy) in &pillars {
        stamp_square(&mut solid, origin_x, origin_y, cell, w, h, cx, cy, half);
    }
    let link = half * 3.2;
    for i in 0..pillars.len() {
        for j in (i + 1)..pillars.len() {
            let (ax, ay) = pillars[i];
            let (bx, by) = pillars[j];
            let dx = bx - ax;
            let dy = by - ay;
            let dist = (dx * dx + dy * dy).sqrt();
            if dist < 0.2 || dist > link {
                continue;
            }
            let steps = ((dist / (cell * 0.5)).ceil() as i32).max(1);
            for s in 0..=steps {
                let t = s as f32 / steps as f32;
                stamp_square(
                    &mut solid,
                    origin_x,
                    origin_y,
                    cell,
                    w,
                    h,
                    ax + dx * t,
                    ay + dy * t,
                    half,
                );
            }
        }
    }
    fill_mask_holes(&mut solid, w, h);
    let loops = trace_loops(&solid, w, h, origin_x, origin_y, cell);
    let mesh = extrude_skates(&loops, thickness, flare);
    if mesh.triangle_count() == 0 {
        None
    } else {
        Some(mesh)
    }
}

fn stamp_square(
    solid: &mut [bool],
    origin_x: f32,
    origin_y: f32,
    cell: f32,
    w: usize,
    h: usize,
    cx: f32,
    cy: f32,
    half: f32,
) {
    let x0 = ((cx - half - origin_x) / cell).floor() as isize;
    let x1 = ((cx + half - origin_x) / cell).ceil() as isize;
    let y0 = ((cy - half - origin_y) / cell).floor() as isize;
    let y1 = ((cy + half - origin_y) / cell).ceil() as isize;
    for y in y0.max(0)..=y1.min(h as isize - 1) {
        for x in x0.max(0)..=x1.min(w as isize - 1) {
            let px = origin_x + (x as f32 + 0.5) * cell;
            let py = origin_y + (y as f32 + 0.5) * cell;
            if (px - cx).abs() <= half + cell * 0.5 && (py - cy).abs() <= half + cell * 0.5 {
                solid[y as usize * w + x as usize] = true;
            }
        }
    }
}

fn fill_mask_holes(mask: &mut [bool], w: usize, h: usize) {
    let mut outside = vec![false; mask.len()];
    let mut stack = Vec::new();
    for x in 0..w {
        stack.push(x);
        if h > 1 {
            stack.push((h - 1) * w + x);
        }
    }
    for y in 0..h {
        stack.push(y * w);
        if w > 1 {
            stack.push(y * w + (w - 1));
        }
    }
    while let Some(i) = stack.pop() {
        if i >= mask.len() || outside[i] || mask[i] {
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

fn trace_loops(
    solid: &[bool],
    w: usize,
    h: usize,
    origin_x: f32,
    origin_y: f32,
    cell: f32,
) -> Vec<Vec<[f32; 2]>> {
    let mut edges: HashMap<(i32, i32), Vec<(i32, i32)>> = HashMap::new();
    let add = |edges: &mut HashMap<(i32, i32), Vec<(i32, i32)>>, a: (i32, i32), b: (i32, i32)| {
        edges.entry(a).or_default().push(b);
    };
    let inside = |x: i32, y: i32| -> bool {
        x >= 0
            && y >= 0
            && (x as usize) < w
            && (y as usize) < h
            && solid[y as usize * w + x as usize]
    };
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            if !inside(x, y) {
                continue;
            }
            if !inside(x, y - 1) {
                add(&mut edges, (x, y), (x + 1, y));
            }
            if !inside(x + 1, y) {
                add(&mut edges, (x + 1, y), (x + 1, y + 1));
            }
            if !inside(x, y + 1) {
                add(&mut edges, (x + 1, y + 1), (x, y + 1));
            }
            if !inside(x - 1, y) {
                add(&mut edges, (x, y + 1), (x, y));
            }
        }
    }
    let starts: Vec<(i32, i32)> = edges.keys().copied().collect();
    let mut loops = Vec::new();
    for start in starts {
        while edges.get(&start).is_some_and(|v| !v.is_empty()) {
            let mut cur = start;
            let mut pts = Vec::new();
            for _ in 0..edges.len() + 4 {
                let Some(next) = edges.get_mut(&cur).and_then(|v| {
                    if v.is_empty() {
                        None
                    } else {
                        Some(v.remove(0))
                    }
                }) else {
                    break;
                };
                pts.push(cur);
                if next == start {
                    break;
                }
                cur = next;
            }
            let poly = simplify_loop(pts, origin_x, origin_y, cell);
            if poly.len() >= 3 {
                loops.push(poly);
            }
        }
    }
    loops
}

fn simplify_loop(pts: Vec<(i32, i32)>, origin_x: f32, origin_y: f32, cell: f32) -> Vec<[f32; 2]> {
    if pts.len() < 3 {
        return Vec::new();
    }
    let mut poly: Vec<[f32; 2]> = pts
        .into_iter()
        .map(|(x, y)| [origin_x + x as f32 * cell, origin_y + y as f32 * cell])
        .collect();
    let mut changed = true;
    let mut guard = 0;
    while changed && poly.len() >= 4 && guard < 8 {
        guard += 1;
        changed = false;
        let n = poly.len();
        let mut next = Vec::with_capacity(n);
        for i in 0..n {
            let a = poly[(i + n - 1) % n];
            let b = poly[i];
            let c = poly[(i + 1) % n];
            let abx = b[0] - a[0];
            let aby = b[1] - a[1];
            let bcx = c[0] - b[0];
            let bcy = c[1] - b[1];
            let cross = (abx * bcy - aby * bcx).abs();
            let scale = abx.hypot(aby) + bcx.hypot(bcy);
            if cross < 0.02 * scale.max(0.25) {
                changed = true;
                continue;
            }
            next.push(b);
        }
        if next.len() >= 3 {
            poly = next;
        } else {
            break;
        }
    }
    let area = polygon_area(&poly);
    if area.abs() < 0.4 {
        return Vec::new();
    }
    if area < 0.0 {
        poly.reverse();
    }
    poly
}

fn polygon_area(poly: &[[f32; 2]]) -> f32 {
    let mut acc = 0.0f32;
    for i in 0..poly.len() {
        let a = poly[i];
        let b = poly[(i + 1) % poly.len()];
        acc += a[0] * b[1] - b[0] * a[1];
    }
    acc * 0.5
}

fn extrude_skates(loops: &[Vec<[f32; 2]>], thickness: f32, flare: f32) -> Mesh {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    for top in loops {
        if top.len() < 3 {
            continue;
        }
        // The traced loop is the bed footprint. Offset the top outward so
        // the skate overhangs the plate instead of wedging onto it.
        let lip = offset_polygon(top, flare);
        if lip.len() != top.len() {
            continue;
        }
        let top_i = vertices.len() as u32;
        for p in &lip {
            vertices.push([p[0], p[1], thickness]);
        }
        let bot_i = vertices.len() as u32;
        for p in top {
            vertices.push([p[0], p[1], 0.0]);
        }
        for tri in triangulate(&lip) {
            indices.extend_from_slice(&[top_i + tri[0], top_i + tri[1], top_i + tri[2]]);
        }
        for tri in triangulate(top) {
            indices.extend_from_slice(&[bot_i + tri[0], bot_i + tri[2], bot_i + tri[1]]);
        }
        let n = top.len() as u32;
        for i in 0..n {
            let j = (i + 1) % n;
            indices.extend_from_slice(&[
                top_i + i,
                bot_i + i,
                bot_i + j,
                top_i + i,
                bot_i + j,
                top_i + j,
            ]);
        }
    }
    Mesh { vertices, indices }
}

fn offset_polygon(poly: &[[f32; 2]], dist: f32) -> Vec<[f32; 2]> {
    if dist < 0.02 {
        return poly.to_vec();
    }
    let n = poly.len();
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let prev = poly[(i + n - 1) % n];
        let curr = poly[i];
        let next = poly[(i + 1) % n];
        let e0 = unit2(curr[0] - prev[0], curr[1] - prev[1]);
        let e1 = unit2(next[0] - curr[0], next[1] - curr[1]);
        let n0 = [e0[1], -e0[0]];
        let n1 = [e1[1], -e1[0]];
        let mut bx = n0[0] + n1[0];
        let mut by = n0[1] + n1[1];
        let len = bx.hypot(by);
        if len < 1e-5 {
            bx = n0[0];
            by = n0[1];
        } else {
            bx /= len;
            by /= len;
        }
        let denom = (bx * n0[0] + by * n0[1]).abs().max(0.35);
        let scale = (dist / denom).min(dist * 3.0);
        out.push([curr[0] + bx * scale, curr[1] + by * scale]);
    }
    out
}

fn unit2(x: f32, y: f32) -> [f32; 2] {
    let len = x.hypot(y);
    if len < 1e-8 {
        [1.0, 0.0]
    } else {
        [x / len, y / len]
    }
}

fn triangulate(poly: &[[f32; 2]]) -> Vec<[u32; 3]> {
    let mut idx: Vec<usize> = (0..poly.len()).collect();
    let mut tris = Vec::new();
    let mut guard = 0;
    let limit = poly.len() * poly.len() + 4;
    while idx.len() > 3 && guard < limit {
        guard += 1;
        let n = idx.len();
        let mut clipped = false;
        for i in 0..n {
            let ia = idx[(i + n - 1) % n];
            let ib = idx[i];
            let ic = idx[(i + 1) % n];
            if !convex_corner(poly[ia], poly[ib], poly[ic]) {
                continue;
            }
            let mut blocked = false;
            for &j in &idx {
                if j == ia || j == ib || j == ic {
                    continue;
                }
                if point_in_tri2(poly[j], poly[ia], poly[ib], poly[ic]) {
                    blocked = true;
                    break;
                }
            }
            if blocked {
                continue;
            }
            tris.push([ia as u32, ib as u32, ic as u32]);
            idx.remove(i);
            clipped = true;
            break;
        }
        if !clipped {
            break;
        }
    }
    if idx.len() == 3 {
        tris.push([idx[0] as u32, idx[1] as u32, idx[2] as u32]);
    }
    tris
}

fn convex_corner(a: [f32; 2], b: [f32; 2], c: [f32; 2]) -> bool {
    let cross = (b[0] - a[0]) * (c[1] - b[1]) - (b[1] - a[1]) * (c[0] - b[0]);
    cross > 1e-5
}

fn point_in_tri2(p: [f32; 2], a: [f32; 2], b: [f32; 2], c: [f32; 2]) -> bool {
    let c0 = (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0]);
    let c1 = (c[0] - b[0]) * (p[1] - b[1]) - (c[1] - b[1]) * (p[0] - b[0]);
    let c2 = (a[0] - c[0]) * (p[1] - c[1]) - (a[1] - c[1]) * (p[0] - c[0]);
    c0 > 1e-4 && c1 > 1e-4 && c2 > 1e-4
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
                object_id: 0,
                x: 0.0,
                y: 0.0,
                z_top: 22.0,
                z_base: 0.0,
            },
            Support {
                id: 2,
                object_id: 0,
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
    fn contact_points_are_separate_from_the_trunk() {
        let supports = [Support {
            id: 1,
            object_id: 0,
            x: 0.0,
            y: 0.0,
            z_top: 22.0,
            z_base: 0.0,
        }];
        let style = &PRESETS[1].style;
        let parts = forest_parts(&supports, style, 0.0, false, 8.0, 45.0);
        assert!(parts.contacts.triangle_count() > 0);
        assert!(parts.necks.triangle_count() > 0);
        assert!(parts.trunks.triangle_count() > 0);
        assert!(parts.feet.triangle_count() > 0);
        assert!(
            parts.contacts.vertices.iter().any(|v| v[2] > 21.5),
            "the contact should reach the tip"
        );
        assert!(
            parts.trunks.vertices.iter().all(|v| v[2] < 21.0),
            "the trunk should stop below the contact"
        );
        let mesh = forest_mesh(&supports, style, 0.0, false, 8.0, 45.0);
        let split = parts.contacts.triangle_count()
            + parts.necks.triangle_count()
            + parts.trunks.triangle_count()
            + parts.feet.triangle_count()
            + parts.branches.triangle_count()
            + parts.braces.triangle_count();
        assert_eq!(mesh.triangle_count(), split);
    }

    #[test]
    fn support_raft_joins_nearby_squares_and_leans_out() {
        let supports = [
            Support {
                id: 1,
                object_id: 0,
                x: 0.0,
                y: 0.0,
                z_top: 12.0,
                z_base: 0.0,
            },
            Support {
                id: 2,
                object_id: 0,
                x: 8.0,
                y: 0.0,
                z_top: 12.0,
                z_base: 0.0,
            },
            Support {
                id: 3,
                object_id: 0,
                x: 8.0,
                y: 8.0,
                z_top: 12.0,
                z_base: 0.0,
            },
            Support {
                id: 4,
                object_id: 0,
                x: 0.0,
                y: 8.0,
                z_top: 12.0,
                z_base: 0.0,
            },
        ];
        let raft = support_raft(&supports, 2.5, 1.0, 2.0, 0.0).unwrap();
        assert!(
            raft.vertices.len() < 80,
            "a joined skate should be a polygon, not a pixel grid ({} verts)",
            raft.vertices.len()
        );
        assert!(
            covers_xy(&raft, 4.0, 4.0),
            "nearby pads should join across the middle"
        );
        let far = [Support {
            id: 5,
            object_id: 0,
            x: 80.0,
            y: 80.0,
            z_top: 12.0,
            z_base: 0.0,
        }];
        let mut split = supports.to_vec();
        split.extend(far);
        let apart = support_raft(&split, 2.5, 1.0, 2.0, 0.0).unwrap();
        assert!(
            !covers_xy(&apart, 40.0, 40.0),
            "pillars far apart stay separate skates"
        );
        let leaned = support_raft(&supports, 2.5, 1.0, 2.0, 45.0).unwrap();
        let top_min = leaned
            .vertices
            .iter()
            .filter(|v| v[2] > 0.8)
            .map(|v| v[0])
            .fold(f32::MAX, f32::min);
        let bed_min = leaned
            .vertices
            .iter()
            .filter(|v| v[2] < 0.05)
            .map(|v| v[0])
            .fold(f32::MAX, f32::min);
        assert!(
            top_min < bed_min - 0.4,
            "45° wall should overhang the bed so a scraper can get under it, top {top_min} bed {bed_min}"
        );
        assert!(support_raft(
            &[Support {
                id: 9,
                object_id: 0,
                x: 0.0,
                y: 0.0,
                z_top: 20.0,
                z_base: 8.0,
            }],
            2.5,
            1.0,
            2.0,
            30.0,
        )
        .is_none());
    }

    fn covers_xy(mesh: &Mesh, x: f32, y: f32) -> bool {
        mesh.indices.chunks_exact(3).any(|tri| {
            let a = mesh.vertices[tri[0] as usize];
            let b = mesh.vertices[tri[1] as usize];
            let c = mesh.vertices[tri[2] as usize];
            if a[2] < 0.2 && b[2] < 0.2 && c[2] < 0.2 {
                return false;
            }
            let points = [[a[0], a[1]], [b[0], b[1]], [c[0], c[1]]];
            let cross = |p: [f32; 2], q: [f32; 2], r: [f32; 2]| {
                (q[0] - p[0]) * (r[1] - p[1]) - (q[1] - p[1]) * (r[0] - p[0])
            };
            let c0 = cross(points[0], points[1], [x, y]);
            let c1 = cross(points[1], points[2], [x, y]);
            let c2 = cross(points[2], points[0], [x, y]);
            (c0 >= -1e-3 && c1 >= -1e-3 && c2 >= -1e-3) || (c0 <= 1e-3 && c1 <= 1e-3 && c2 <= 1e-3)
        })
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
