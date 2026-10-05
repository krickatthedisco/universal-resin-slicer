//! The plate: models, supports, drain holes, and the edits the prepare view applies.

use crate::mesh::{face_normal, load_mesh, Mesh};
use crate::slice::{Drain, Hollow, Solid};
use crate::supports::{self, Support, SupportStyle};
use anyhow::Result;
use glam::{EulerRot, Mat4, Quat, Vec3};
use std::path::Path;

#[derive(Clone, Debug)]
pub struct Object {
    pub id: u64,
    pub name: String,
    pub mesh: Mesh,
    pub position: Vec3,
    pub rotation_deg: Vec3,
    pub scale: Vec3,
    pub hollow: bool,
    pub wall_mm: f32,
    pub bottom_cap_mm: f32,
    pub top_cap_mm: f32,
    pub infill: bool,
    pub infill_spacing_mm: f32,
    pub infill_thickness_mm: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct DrainHole {
    pub id: u64,
    pub origin: Vec3,
    pub axis: Vec3,
    pub radius_mm: f32,
    pub depth_mm: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Selection {
    None,
    Object(u64),
    Support(u64),
    Drain(u64),
}

enum Undo {
    Xform {
        id: u64,
        position: Vec3,
        rotation_deg: Vec3,
        scale: Vec3,
    },
    Supports(Vec<Support>),
    Drains(Vec<DrainHole>),
    Deleted(Object),
    Added(u64),
}

pub struct Document {
    pub objects: Vec<Object>,
    pub supports: Vec<Support>,
    pub drains: Vec<DrainHole>,
    pub selection: Selection,
    pub raft: bool,
    pub raft_mm: f32,
    pub raft_margin: f32,
    pub braces_on: bool,
    pub brace_dist: f32,
    pub preset: usize,
    pub style: SupportStyle,
    pub platform_only: bool,
    next_id: u64,
    undo: Vec<Undo>,
    pub changed: u64,
}

impl Document {
    pub fn new() -> Self {
        Self {
            objects: Vec::new(),
            supports: Vec::new(),
            drains: Vec::new(),
            selection: Selection::None,
            raft: true,
            raft_mm: 1.0,
            raft_margin: 2.0,
            braces_on: true,
            brace_dist: 8.0,
            preset: 1,
            style: supports::PRESETS[1].style,
            platform_only: false,
            next_id: 1,
            undo: Vec::new(),
            changed: 1,
        }
    }

    pub fn touch(&mut self) {
        self.changed = self.changed.wrapping_add(1);
    }

    fn alloc(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn set_preset(&mut self, index: usize) {
        self.preset = index.min(supports::PRESETS.len() - 1);
        self.style = supports::PRESETS[self.preset].style;
        self.touch();
    }

    pub fn add_mesh(&mut self, name: String, mesh: Mesh) -> u64 {
        let id = self.alloc();
        self.objects.push(Object {
            id,
            name,
            mesh,
            position: Vec3::ZERO,
            rotation_deg: Vec3::ZERO,
            scale: Vec3::ONE,
            hollow: false,
            wall_mm: 2.0,
            bottom_cap_mm: 1.5,
            top_cap_mm: 1.5,
            infill: false,
            infill_spacing_mm: 4.0,
            infill_thickness_mm: 0.6,
        });
        self.undo.push(Undo::Added(id));
        self.selection = Selection::Object(id);
        self.touch();
        id
    }

    pub fn import(&mut self, path: &Path) -> Result<u64> {
        let mesh = load_mesh(path)?;
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("model")
            .to_string();
        let id = self.add_mesh(name, mesh);
        self.center_object(id);
        self.drop_object(id);
        Ok(id)
    }

    pub fn object(&self, id: u64) -> Option<&Object> {
        self.objects.iter().find(|o| o.id == id)
    }

    pub fn object_mut(&mut self, id: u64) -> Option<&mut Object> {
        self.objects.iter_mut().find(|o| o.id == id)
    }

    pub fn selected_object(&self) -> Option<&Object> {
        match self.selection {
            Selection::Object(id) => self.object(id),
            _ => None,
        }
    }

    pub fn selected_object_mut(&mut self) -> Option<&mut Object> {
        let Selection::Object(id) = self.selection else {
            return None;
        };
        self.object_mut(id)
    }

    pub fn matrix(obj: &Object) -> Mat4 {
        Mat4::from_translation(obj.position)
            * Mat4::from_euler(
                EulerRot::XYZ,
                obj.rotation_deg.x.to_radians(),
                obj.rotation_deg.y.to_radians(),
                obj.rotation_deg.z.to_radians(),
            )
            * Mat4::from_scale(obj.scale)
    }

    pub fn world_mesh(obj: &Object) -> Mesh {
        let mat = Self::matrix(obj);
        obj.mesh
            .transformed(|p| mat.transform_point3(Vec3::from_array(p)).to_array())
    }

    pub fn world_bounds(obj: &Object) -> Option<(Vec3, Vec3)> {
        let mesh = Self::world_mesh(obj);
        let (min, max) = mesh.bounds()?;
        Some((Vec3::from_array(min), Vec3::from_array(max)))
    }

    pub fn push_xform_undo(&mut self, id: u64) {
        if let Some(obj) = self.object(id) {
            self.remember_xform(id, obj.position, obj.rotation_deg, obj.scale);
        }
    }

    pub fn remember_xform(&mut self, id: u64, position: Vec3, rotation_deg: Vec3, scale: Vec3) {
        self.undo.push(Undo::Xform {
            id,
            position,
            rotation_deg,
            scale,
        });
        if self.undo.len() > 40 {
            self.undo.remove(0);
        }
    }

    pub fn undo(&mut self) {
        let Some(edit) = self.undo.pop() else {
            return;
        };
        match edit {
            Undo::Xform {
                id,
                position,
                rotation_deg,
                scale,
            } => {
                if let Some(obj) = self.object_mut(id) {
                    obj.position = position;
                    obj.rotation_deg = rotation_deg;
                    obj.scale = scale;
                }
            }
            Undo::Supports(list) => self.supports = list,
            Undo::Drains(list) => self.drains = list,
            Undo::Deleted(obj) => {
                self.selection = Selection::Object(obj.id);
                self.objects.push(obj);
            }
            Undo::Added(id) => {
                self.objects.retain(|o| o.id != id);
                if self.selection == Selection::Object(id) {
                    self.selection = Selection::None;
                }
            }
        }
        self.touch();
    }

    pub fn drop_object(&mut self, id: u64) {
        let Some((min, _)) = self.object(id).and_then(Self::world_bounds) else {
            return;
        };
        if let Some(obj) = self.object_mut(id) {
            obj.position.z -= min.z;
        }
        self.touch();
    }

    pub fn center_object(&mut self, id: u64) {
        let Some((min, max)) = self.object(id).and_then(Self::world_bounds) else {
            return;
        };
        // Centering uses the machine later; here we center on the mesh's own XY
        // relative to a caller-supplied plate via `center_on_plate`.
        let _ = (min, max);
    }

    pub fn center_on_plate(&mut self, id: u64, plate_x: f32, plate_y: f32) {
        let Some((min, max)) = self.object(id).and_then(Self::world_bounds) else {
            return;
        };
        let dx = plate_x * 0.5 - (min.x + max.x) * 0.5;
        let dy = plate_y * 0.5 - (min.y + max.y) * 0.5;
        if let Some(obj) = self.object_mut(id) {
            obj.position.x += dx;
            obj.position.y += dy;
        }
        self.touch();
    }

    pub fn duplicate(&mut self, id: u64) -> Option<u64> {
        let obj = self.object(id)?.clone();
        let new_id = self.alloc();
        let mut copy = obj;
        copy.id = new_id;
        copy.name = format!("{} copy", copy.name);
        copy.position.x += 12.0;
        self.objects.push(copy);
        self.undo.push(Undo::Added(new_id));
        self.selection = Selection::Object(new_id);
        self.touch();
        Some(new_id)
    }

    pub fn delete_selection(&mut self) {
        match self.selection {
            Selection::Object(id) => {
                if let Some(i) = self.objects.iter().position(|o| o.id == id) {
                    let obj = self.objects.remove(i);
                    self.undo.push(Undo::Deleted(obj));
                    self.selection = Selection::None;
                    self.touch();
                }
            }
            Selection::Support(id) => {
                self.undo.push(Undo::Supports(self.supports.clone()));
                self.supports.retain(|s| s.id != id);
                self.selection = Selection::None;
                self.touch();
            }
            Selection::Drain(id) => {
                self.undo.push(Undo::Drains(self.drains.clone()));
                self.drains.retain(|d| d.id != id);
                self.selection = Selection::None;
                self.touch();
            }
            Selection::None => {}
        }
    }

    pub fn flip_normals(&mut self, id: u64) {
        if let Some(obj) = self.object_mut(id) {
            obj.mesh.flip_winding();
        }
        self.touch();
    }

    pub fn mirror(&mut self, id: u64, axis: usize) {
        self.push_xform_undo(id);
        if let Some(obj) = self.object_mut(id) {
            match axis {
                0 => obj.scale.x = -obj.scale.x,
                1 => obj.scale.y = -obj.scale.y,
                _ => obj.scale.z = -obj.scale.z,
            }
        }
        self.touch();
    }

    pub fn place_on_largest_face(&mut self, id: u64) {
        self.push_xform_undo(id);
        let Some(obj) = self.object(id) else {
            return;
        };
        let world = Self::world_mesh(obj);
        let Some((normal, _)) = largest_face(&world) else {
            return;
        };
        self.rotate_world_normal_down(id, normal);
        self.drop_object(id);
    }

    pub fn repair(&mut self, id: u64) {
        if let Some(obj) = self.object_mut(id) {
            obj.mesh.repair();
        }
        self.touch();
    }

    pub fn auto_orient_all(&mut self) {
        let ids: Vec<u64> = self.objects.iter().map(|o| o.id).collect();
        for id in ids {
            self.auto_orient(id);
        }
    }

    /// Tile copies of one model across the plate. Returns how many new copies were added.
    pub fn fill_bed(
        &mut self,
        id: u64,
        plate_x: f32,
        plate_y: f32,
    ) -> std::result::Result<usize, &'static str> {
        let Some((min, max)) = self.object(id).and_then(Self::world_bounds) else {
            return Err("That model has no triangles.");
        };
        let gap = 3.0f32;
        let w = (max.x - min.x).max(0.1);
        let d = (max.y - min.y).max(0.1);
        if w + gap > plate_x || d + gap > plate_y {
            return Err(
                "That model is larger than the plate, so it cannot be copied across the bed.",
            );
        }
        let cols = ((plate_x - gap) / (w + gap)).floor().max(1.0) as i32;
        let rows = ((plate_y - gap) / (d + gap)).floor().max(1.0) as i32;
        let total = (cols * rows) as usize;
        if total <= 1 {
            return Err("Only one copy fits. Scale it down or use Layout if several models are already on the plate.");
        }
        let Some(origin) = self.object(id).cloned() else {
            return Err("That model is gone.");
        };
        self.push_xform_undo(id);
        let dx0 = gap - min.x;
        let dy0 = gap - min.y;
        if let Some(obj) = self.object_mut(id) {
            obj.position.x += dx0;
            obj.position.y += dy0;
        }
        let mut added = 0usize;
        for row in 0..rows {
            for col in 0..cols {
                if row == 0 && col == 0 {
                    continue;
                }
                let new_id = self.alloc();
                let mut copy = origin.clone();
                copy.id = new_id;
                copy.name = format!("{} {}", origin.name, added + 2);
                copy.position.x += dx0 + col as f32 * (w + gap);
                copy.position.y += dy0 + row as f32 * (d + gap);
                self.objects.push(copy);
                self.undo.push(Undo::Added(new_id));
                added += 1;
            }
        }
        self.selection = Selection::Object(id);
        self.touch();
        Ok(added)
    }

    pub fn auto_orient(&mut self, id: u64) {
        self.push_xform_undo(id);
        let Some(obj) = self.object(id) else {
            return;
        };
        let world = Self::world_mesh(obj);
        let mut faces = face_areas(&world);
        faces.sort_by(|a, b| b.1.total_cmp(&a.1));
        faces.truncate(10);
        let mut best: Option<(Vec3, f32)> = None;
        for (normal, _) in faces {
            if normal.z > 0.2 {
                continue;
            }
            let q = Quat::from_rotation_arc(normal, Vec3::NEG_Z);
            let score = overhang_score(&world, q, 45.0);
            if best.map(|(_, s)| score < s).unwrap_or(true) {
                best = Some((normal, score));
            }
        }
        if let Some((normal, _)) = best {
            self.rotate_world_normal_down(id, normal);
            self.drop_object(id);
        }
    }

    fn rotate_world_normal_down(&mut self, id: u64, normal: Vec3) {
        let Some(obj) = self.object(id) else {
            return;
        };
        let current = Quat::from_euler(
            EulerRot::XYZ,
            obj.rotation_deg.x.to_radians(),
            obj.rotation_deg.y.to_radians(),
            obj.rotation_deg.z.to_radians(),
        );
        let q = Quat::from_rotation_arc(normal.normalize(), Vec3::NEG_Z) * current;
        let (x, y, z) = q.to_euler(EulerRot::XYZ);
        if let Some(obj) = self.object_mut(id) {
            obj.rotation_deg = Vec3::new(x.to_degrees(), y.to_degrees(), z.to_degrees());
        }
        self.touch();
    }

    pub fn auto_layout(&mut self, plate_x: f32, plate_y: f32) {
        for obj in &self.objects {
            self.undo.push(Undo::Xform {
                id: obj.id,
                position: obj.position,
                rotation_deg: obj.rotation_deg,
                scale: obj.scale,
            });
        }
        let mut boxes: Vec<(u64, Vec3, Vec3)> = Vec::new();
        for obj in &self.objects {
            if let Some((min, max)) = Self::world_bounds(obj) {
                boxes.push((obj.id, min, max));
            }
        }
        boxes.sort_by(|a, b| {
            let wa = a.2.x - a.1.x;
            let wb = b.2.x - b.1.x;
            wb.total_cmp(&wa)
        });
        let gap = 3.0;
        let mut cursor_x = gap;
        let mut cursor_y = gap;
        let mut row_h = 0.0f32;
        for (id, min, max) in boxes {
            let w = max.x - min.x;
            let h = max.y - min.y;
            if cursor_x + w + gap > plate_x && cursor_x > gap {
                cursor_x = gap;
                cursor_y += row_h + gap;
                row_h = 0.0;
            }
            let dx = cursor_x - min.x;
            let dy = cursor_y - min.y;
            if let Some(obj) = self.object_mut(id) {
                obj.position.x += dx;
                obj.position.y += dy;
            }
            cursor_x += w + gap;
            row_h = row_h.max(h);
            let _ = plate_y;
        }
        self.touch();
    }

    pub fn outside_plate(&self, plate: Vec3) -> bool {
        self.objects.iter().any(|obj| {
            Self::world_bounds(obj).is_some_and(|(min, max)| {
                min.x < -0.05
                    || min.y < -0.05
                    || max.x > plate.x + 0.05
                    || max.y > plate.y + 0.05
                    || max.z > plate.z + 0.05
                    || min.z < -0.05
            })
        })
    }

    pub fn add_auto_supports(&mut self, only_selected: bool) {
        self.undo.push(Undo::Supports(self.supports.clone()));
        let style = self.style;
        let platform_only = self.platform_only;
        let ids: Vec<u64> = if only_selected {
            self.selected_object().map(|o| o.id).into_iter().collect()
        } else {
            self.objects.iter().map(|o| o.id).collect()
        };
        for id in ids {
            let Some(obj) = self.object(id) else { continue };
            let world = Self::world_mesh(obj);
            let mut id_gen = self.next_id;
            let found = supports::auto_supports(
                &world.vertices,
                &world.indices,
                &style,
                platform_only,
                &mut id_gen,
            );
            self.next_id = id_gen;
            self.supports.extend(found);
        }
        self.touch();
    }

    pub fn add_support_at(&mut self, point: Vec3, object_id: u64) {
        let Some(obj) = self.object(object_id) else {
            return;
        };
        let world = Self::world_mesh(obj);
        self.undo.push(Undo::Supports(self.supports.clone()));
        let id = self.alloc();
        let support = supports::manual_support(
            point.x,
            point.y,
            point.z,
            &world.vertices,
            &world.indices,
            self.platform_only,
            id,
        );
        self.supports.push(support);
        self.selection = Selection::Support(id);
        self.touch();
    }

    pub fn add_island_supports(&mut self, islands: &[(f32, f32, f32)]) {
        if islands.is_empty() {
            return;
        }
        self.undo.push(Undo::Supports(self.supports.clone()));
        let platform_only = self.platform_only;
        // Hit-testing the whole scene is enough to land the foot.
        let mut verts = Vec::new();
        let mut indices = Vec::new();
        for obj in &self.objects {
            let world = Self::world_mesh(obj);
            let base = verts.len() as u32;
            verts.extend(world.vertices);
            indices.extend(world.indices.iter().map(|i| i + base));
        }
        for (x, y, z) in islands {
            let id = self.alloc();
            self.supports.push(supports::manual_support(
                *x,
                *y,
                *z,
                &verts,
                &indices,
                platform_only,
                id,
            ));
        }
        self.touch();
    }

    pub fn clear_supports(&mut self) {
        if self.supports.is_empty() {
            return;
        }
        self.undo.push(Undo::Supports(self.supports.clone()));
        self.supports.clear();
        self.touch();
    }

    pub fn add_drain(&mut self, origin: Vec3, into_model: Vec3) {
        self.undo.push(Undo::Drains(self.drains.clone()));
        let id = self.alloc();
        let axis = if into_model.length() < 1e-4 {
            Vec3::NEG_Z
        } else {
            -into_model.normalize()
        };
        self.drains.push(DrainHole {
            id,
            origin,
            axis,
            radius_mm: 1.2,
            depth_mm: 8.0,
        });
        self.selection = Selection::Drain(id);
        self.touch();
    }

    pub fn raft_top(&self) -> f32 {
        if self.raft && self.supports.iter().any(|s| s.z_base <= 0.2) {
            self.raft_mm
        } else {
            0.0
        }
    }

    pub fn solids(&self) -> Vec<Solid> {
        let mut solids = Vec::new();
        for obj in &self.objects {
            let world = Self::world_mesh(obj);
            let (min, max) = world.bounds().unwrap_or(([0.0; 3], [0.0; 3]));
            let hollow = if obj.hollow {
                Some(Hollow {
                    wall_mm: obj.wall_mm,
                    bottom_cap_mm: obj.bottom_cap_mm,
                    top_cap_mm: obj.top_cap_mm,
                    infill_spacing_mm: if obj.infill {
                        obj.infill_spacing_mm
                    } else {
                        0.0
                    },
                    infill_thickness_mm: obj.infill_thickness_mm,
                    z_min: min[2],
                    z_max: max[2],
                })
            } else {
                None
            };
            solids.push(Solid {
                vertices: world.vertices,
                indices: world.indices,
                hollow,
            });
        }
        let raft_top = self.raft_top();
        if raft_top > 0.0 {
            if let Some(raft) = supports::raft_mesh(
                &self.supports,
                self.raft_margin,
                self.raft_mm,
                self.style.trunk_mm,
            ) {
                solids.push(Solid {
                    vertices: raft.vertices,
                    indices: raft.indices,
                    hollow: None,
                });
            }
        }
        let forest = supports::forest_mesh(
            &self.supports,
            &self.style,
            raft_top,
            self.braces_on,
            self.brace_dist,
        );
        if forest.triangle_count() > 0 {
            solids.push(Solid {
                vertices: forest.vertices,
                indices: forest.indices,
                hollow: None,
            });
        }
        solids
    }

    pub fn drain_inputs(&self) -> Vec<Drain> {
        self.drains
            .iter()
            .map(|d| Drain {
                origin: d.origin.to_array(),
                axis: d.axis.to_array(),
                radius_mm: d.radius_mm,
                depth_mm: d.depth_mm,
            })
            .collect()
    }

    /// Closest triangle hit. Returns the object id, the point, and the geometric normal.
    pub fn raycast(&self, origin: Vec3, dir: Vec3) -> Option<(u64, Vec3, Vec3)> {
        let mut best: Option<(f32, u64, Vec3, Vec3)> = None;
        for obj in &self.objects {
            let world = Self::world_mesh(obj);
            for tri in world.indices.chunks_exact(3) {
                let a = Vec3::from_array(world.vertices[tri[0] as usize]);
                let b = Vec3::from_array(world.vertices[tri[1] as usize]);
                let c = Vec3::from_array(world.vertices[tri[2] as usize]);
                if let Some((t, p, n)) = ray_triangle(origin, dir, a, b, c) {
                    if best.map(|(bt, ..)| t < bt).unwrap_or(true) {
                        best = Some((t, obj.id, p, n));
                    }
                }
            }
        }
        best.map(|(_, id, p, n)| (id, p, n))
    }
}

impl Default for Document {
    fn default() -> Self {
        Self::new()
    }
}

fn largest_face(mesh: &Mesh) -> Option<(Vec3, f32)> {
    face_areas(mesh)
        .into_iter()
        .max_by(|a, b| a.1.total_cmp(&b.1))
}

fn face_areas(mesh: &Mesh) -> Vec<(Vec3, f32)> {
    let mut out = Vec::new();
    for tri in mesh.indices.chunks_exact(3) {
        let a = mesh.vertices[tri[0] as usize];
        let b = mesh.vertices[tri[1] as usize];
        let c = mesh.vertices[tri[2] as usize];
        let n = face_normal(a, b, c);
        let ux = b[0] - a[0];
        let uy = b[1] - a[1];
        let uz = b[2] - a[2];
        let vx = c[0] - a[0];
        let vy = c[1] - a[1];
        let vz = c[2] - a[2];
        let cross = [uy * vz - uz * vy, uz * vx - ux * vz, ux * vy - uy * vx];
        let area = 0.5 * (cross[0] * cross[0] + cross[1] * cross[1] + cross[2] * cross[2]).sqrt();
        if area > 0.05 {
            out.push((Vec3::from_array(n), area));
        }
    }
    out
}

fn overhang_score(mesh: &Mesh, rotation: Quat, angle: f32) -> f32 {
    let mut score = 0.0f32;
    for tri in mesh.indices.chunks_exact(3) {
        let a = mesh.vertices[tri[0] as usize];
        let b = mesh.vertices[tri[1] as usize];
        let c = mesh.vertices[tri[2] as usize];
        let n = Vec3::from_array(face_normal(a, b, c));
        let n = rotation * n;
        if supports::needs_support(n.to_array(), angle) {
            let area = (Vec3::from_array(b) - Vec3::from_array(a))
                .cross(Vec3::from_array(c) - Vec3::from_array(a))
                .length()
                * 0.5;
            score += area;
        }
    }
    score
}

fn ray_triangle(origin: Vec3, dir: Vec3, a: Vec3, b: Vec3, c: Vec3) -> Option<(f32, Vec3, Vec3)> {
    let ab = b - a;
    let ac = c - a;
    let pvec = dir.cross(ac);
    let det = ab.dot(pvec);
    if det.abs() < 1e-8 {
        return None;
    }
    let inv = 1.0 / det;
    let tvec = origin - a;
    let u = tvec.dot(pvec) * inv;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let qvec = tvec.cross(ab);
    let v = dir.dot(qvec) * inv;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = ac.dot(qvec) * inv;
    if t < 0.001 {
        return None;
    }
    let n = ab.cross(ac).normalize();
    let n = if n.dot(dir) > 0.0 { -n } else { n };
    Some((t, origin + dir * t, n))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::box_mesh;

    #[test]
    fn fill_bed_tiles_a_small_cube() {
        let mut doc = Document::new();
        let id = doc.add_mesh("cube".into(), box_mesh([0.0, 0.0, 0.0], [20.0, 20.0, 20.0]));
        let added = doc.fill_bed(id, 100.0, 50.0).unwrap();
        assert_eq!(added, 7);
        assert_eq!(doc.objects.len(), 8);
    }
}
