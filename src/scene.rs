//! The plate: models, supports, drain holes, and the edits the prepare view applies.

use crate::mesh::{face_normal, load_mesh, Mesh};
use crate::slice::{Drain, Hollow, Solid};
use crate::supports::{self, Support, SupportStyle};
use anyhow::Result;
use glam::{EulerRot, Mat4, Quat, Vec3};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

/// Support and raft settings that belong to one model. Changing them does
/// not reshape the supports on any other model.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct ModelSupport {
    pub preset: usize,
    pub style: SupportStyle,
    pub platform_only: bool,
    pub lift_mm: f32,
    pub raft: bool,
    pub raft_mm: f32,
    pub raft_margin: f32,
    pub raft_angle: f32,
    pub braces_on: bool,
    pub brace_dist: f32,
    pub brace_angle: f32,
}

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
    pub support: ModelSupport,
    /// Bumped when the mesh data itself changes, so the plate view can keep
    /// the vertex buffer and only resend it then.
    pub mesh_rev: u64,
    /// Local-space bounding box, cached so a move does not scan the sculpt.
    pub bounds_min: [f32; 3],
    pub bounds_max: [f32; 3],
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
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
    Supports {
        list: Vec<Support>,
        /// Positions from before a support pass lifted these models.
        lifted: Vec<(u64, Vec3)>,
    },
    Drains(Vec<DrainHole>),
    Deleted {
        object: Object,
        supports: Vec<Support>,
    },
    Added(u64),
    Cut {
        original: Object,
        added: u64,
    },
    /// Both pieces of a cut, so redo can put them back.
    RestoreCut {
        lower: Object,
        upper: Object,
    },
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
    /// Lean of a cross-brace above the bed. 45° is the usual resin brace.
    pub brace_angle: f32,
    pub preset: usize,
    pub style: SupportStyle,
    pub platform_only: bool,
    /// How far above the bed a part sits after supports are added.
    pub support_lift_mm: f32,
    /// Diameter and depth used by a new drain hole.
    pub drain_diameter_mm: f32,
    pub drain_depth_mm: f32,
    /// Skate-raft wall, measured from vertical. 0 is a straight edge.
    pub raft_angle: f32,
    next_id: u64,
    undo: Vec<Undo>,
    redo: Vec<Undo>,
    pub changed: u64,
    /// Bumped for supports, meshes, and raft settings. A plain move does not.
    pub structure: u64,
}

impl Document {
    pub fn new() -> Self {
        Self {
            objects: Vec::new(),
            supports: Vec::new(),
            drains: Vec::new(),
            selection: Selection::None,
            raft: false,
            raft_mm: 1.0,
            raft_margin: 2.0,
            braces_on: true,
            brace_dist: 8.0,
            brace_angle: 45.0,
            preset: 1,
            style: supports::PRESETS[1].style,
            platform_only: false,
            support_lift_mm: 5.0,
            drain_diameter_mm: 2.4,
            drain_depth_mm: 8.0,
            raft_angle: 30.0,
            next_id: 1,
            undo: Vec::new(),
            redo: Vec::new(),
            changed: 1,
            structure: 1,
        }
    }

    pub fn touch(&mut self) {
        self.changed = self.changed.wrapping_add(1);
        self.structure = self.structure.wrapping_add(1);
    }

    /// A move, rotate, or scale. The sculpt's vertex buffer can stay put.
    pub fn touch_xform(&mut self) {
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
        let preset = self.preset;
        let style = self.style;
        if let Some(id) = self.edit_target() {
            if let Some(obj) = self.object_mut(id) {
                obj.support.preset = preset;
                obj.support.style = style;
            }
        }
        self.touch();
    }

    fn profile(&self) -> ModelSupport {
        ModelSupport {
            preset: self.preset,
            style: self.style,
            platform_only: self.platform_only,
            lift_mm: self.support_lift_mm,
            raft: self.raft,
            raft_mm: self.raft_mm,
            raft_margin: self.raft_margin,
            raft_angle: self.raft_angle,
            braces_on: self.braces_on,
            brace_dist: self.brace_dist,
            brace_angle: self.brace_angle,
        }
    }

    /// Remember this model's support setup as the starting point for the next one.
    pub fn sync_defaults_from(&mut self, id: u64) {
        let Some(support) = self.object(id).map(|obj| obj.support) else {
            return;
        };
        self.preset = support.preset;
        self.style = support.style;
        self.platform_only = support.platform_only;
        self.support_lift_mm = support.lift_mm;
        self.raft = support.raft;
        self.raft_mm = support.raft_mm;
        self.raft_margin = support.raft_margin;
        self.raft_angle = support.raft_angle;
        self.braces_on = support.braces_on;
        self.brace_dist = support.brace_dist;
        self.brace_angle = support.brace_angle;
    }

    /// The model an edit should land on: the selected model, or the model
    /// that owns the selected support.
    pub fn edit_target(&self) -> Option<u64> {
        let id = match self.selection {
            Selection::Object(id) => id,
            Selection::Support(sid) => self
                .supports
                .iter()
                .find(|s| s.id == sid)
                .map(|s| s.object_id)?,
            Selection::Drain(_) | Selection::None => return None,
        };
        self.object(id).map(|obj| obj.id)
    }

    pub fn add_mesh(&mut self, name: String, mesh: Mesh) -> u64 {
        let id = self.alloc();
        let support = self.profile();
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
            support,
            mesh_rev: 1,
            bounds_min: [0.0; 3],
            bounds_max: [0.0; 3],
        });
        if let Some(obj) = self.objects.last_mut() {
            cache_bounds(obj);
        }
        self.push_undo(Undo::Added(id));
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

    /// Replace the plate with a saved one. Undo history starts over.
    pub fn load_plate(
        &mut self,
        mut objects: Vec<Object>,
        supports: Vec<Support>,
        drains: Vec<DrainHole>,
    ) {
        let mut next = 1u64;
        for obj in &mut objects {
            cache_bounds(obj);
            obj.mesh_rev = obj.mesh_rev.max(1);
            next = next.max(obj.id.saturating_add(1));
        }
        for support in &supports {
            next = next.max(support.id.saturating_add(1));
        }
        for drain in &drains {
            next = next.max(drain.id.saturating_add(1));
        }
        self.objects = objects;
        self.supports = supports;
        self.drains = drains;
        self.selection = if self.objects.len() == 1 {
            Selection::Object(self.objects[0].id)
        } else {
            Selection::None
        };
        self.undo.clear();
        self.redo.clear();
        self.next_id = next;
        self.touch();
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

    /// Eight corners of the cached local bounds, after the transform.
    pub fn display_bounds(obj: &Object) -> Option<(Vec3, Vec3)> {
        if obj.mesh.vertices.is_empty() {
            return None;
        }
        let min = obj.bounds_min;
        let max = obj.bounds_max;
        let mat = Self::matrix(obj);
        let mut lo = Vec3::splat(f32::MAX);
        let mut hi = Vec3::splat(f32::MIN);
        for ix in 0..2 {
            for iy in 0..2 {
                for iz in 0..2 {
                    let p = Vec3::new(
                        if ix == 0 { min[0] } else { max[0] },
                        if iy == 0 { min[1] } else { max[1] },
                        if iz == 0 { min[2] } else { max[2] },
                    );
                    let w = mat.transform_point3(p);
                    lo = lo.min(w);
                    hi = hi.max(w);
                }
            }
        }
        Some((lo, hi))
    }

    pub fn push_xform_undo(&mut self, id: u64) {
        if let Some(obj) = self.object(id) {
            self.remember_xform(id, obj.position, obj.rotation_deg, obj.scale);
        }
    }

    pub fn remember_xform(&mut self, id: u64, position: Vec3, rotation_deg: Vec3, scale: Vec3) {
        self.push_undo(Undo::Xform {
            id,
            position,
            rotation_deg,
            scale,
        });
    }

    fn push_undo(&mut self, edit: Undo) {
        self.undo.push(edit);
        if self.undo.len() > 40 {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    /// Put the last edit back. Returns false when there is nothing to undo.
    pub fn undo(&mut self) -> bool {
        let Some(edit) = self.undo.pop() else {
            return false;
        };
        if let Some(inverse) = self.capture_inverse(&edit) {
            self.redo.push(inverse);
            if self.redo.len() > 40 {
                self.redo.remove(0);
            }
        }
        self.apply_history(edit);
        self.touch();
        true
    }

    /// Reapply the last undone edit. Returns false when there is nothing to redo.
    pub fn redo(&mut self) -> bool {
        let Some(edit) = self.redo.pop() else {
            return false;
        };
        if let Some(inverse) = self.capture_inverse(&edit) {
            self.undo.push(inverse);
            if self.undo.len() > 40 {
                self.undo.remove(0);
            }
        }
        self.apply_history(edit);
        self.touch();
        true
    }

    fn capture_inverse(&self, edit: &Undo) -> Option<Undo> {
        match edit {
            Undo::Xform { id, .. } => {
                let obj = self.object(*id)?;
                Some(Undo::Xform {
                    id: *id,
                    position: obj.position,
                    rotation_deg: obj.rotation_deg,
                    scale: obj.scale,
                })
            }
            Undo::Supports { lifted, .. } => {
                let mut now = Vec::new();
                for (id, _) in lifted {
                    if let Some(obj) = self.object(*id) {
                        now.push((*id, obj.position));
                    }
                }
                Some(Undo::Supports {
                    list: self.supports.clone(),
                    lifted: now,
                })
            }
            Undo::Drains(_) => Some(Undo::Drains(self.drains.clone())),
            Undo::Added(id) => {
                let object = self.object(*id)?.clone();
                let supports = self
                    .supports
                    .iter()
                    .copied()
                    .filter(|support| support.object_id == *id)
                    .collect();
                Some(Undo::Deleted { object, supports })
            }
            Undo::Deleted { object, .. } => Some(Undo::Added(object.id)),
            Undo::Cut { original, added } => {
                let lower = self.object(original.id)?.clone();
                let upper = self.object(*added)?.clone();
                Some(Undo::RestoreCut { lower, upper })
            }
            Undo::RestoreCut { lower, upper } => {
                let original = self.object(lower.id)?.clone();
                Some(Undo::Cut {
                    original,
                    added: upper.id,
                })
            }
        }
    }

    fn apply_history(&mut self, edit: Undo) {
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
            Undo::Supports { list, lifted } => {
                self.supports = list;
                for (id, position) in lifted {
                    if let Some(obj) = self.object_mut(id) {
                        obj.position = position;
                    }
                }
            }
            Undo::Drains(list) => self.drains = list,
            Undo::Deleted { object, supports } => {
                self.selection = Selection::Object(object.id);
                self.supports.extend(supports);
                self.objects.push(object);
            }
            Undo::Added(id) => {
                self.objects.retain(|o| o.id != id);
                self.supports.retain(|support| support.object_id != id);
                if self.selection == Selection::Object(id) {
                    self.selection = Selection::None;
                }
            }
            Undo::Cut { original, added } => {
                self.objects.retain(|o| o.id != added);
                let id = original.id;
                if let Some(pos) = self.objects.iter().position(|o| o.id == id) {
                    self.objects[pos] = original;
                } else {
                    self.objects.push(original);
                }
                self.selection = Selection::Object(id);
            }
            Undo::RestoreCut { lower, upper } => {
                let lower_id = lower.id;
                let upper_id = upper.id;
                if let Some(pos) = self.objects.iter().position(|o| o.id == lower_id) {
                    self.objects[pos] = lower;
                } else {
                    self.objects.push(lower);
                }
                if let Some(pos) = self.objects.iter().position(|o| o.id == upper_id) {
                    self.objects[pos] = upper;
                } else {
                    self.objects.push(upper);
                }
                self.selection = Selection::Object(upper_id);
            }
        }
    }

    pub fn drop_object(&mut self, id: u64) {
        let Some((min, _)) = self.object(id).and_then(Self::world_bounds) else {
            return;
        };
        if let Some(obj) = self.object_mut(id) {
            obj.position.z -= min.z;
        }
        self.touch_xform();
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
        self.touch_xform();
    }

    pub fn duplicate(&mut self, id: u64) -> Option<u64> {
        let obj = self.object(id)?.clone();
        let new_id = self.alloc();
        let mut copy = obj;
        copy.id = new_id;
        copy.name = format!("{} copy", copy.name);
        copy.position.x += 12.0;
        self.objects.push(copy);
        self.push_undo(Undo::Added(new_id));
        self.selection = Selection::Object(new_id);
        self.touch();
        Some(new_id)
    }

    /// Place `extra` copies of one model, stepping by the part size plus `gap`.
    pub fn duplicate_copies(
        &mut self,
        id: u64,
        extra: u32,
        gap: f32,
        plate_x: f32,
        plate_y: f32,
    ) -> Result<usize, &'static str> {
        let extra = extra.clamp(1, 40) as usize;
        let gap = gap.clamp(0.0, 40.0);
        let Some((min, max)) = self.object(id).and_then(Self::world_bounds) else {
            return Err("That model has no triangles.");
        };
        let Some(origin) = self.object(id).cloned() else {
            return Err("That model is gone.");
        };
        let w = (max.x - min.x).max(0.1);
        let d = (max.y - min.y).max(0.1);
        let step_x = w + gap;
        let step_y = d + gap;
        let mut added = 0usize;
        let mut col = 1i32;
        let mut row = 0i32;
        for n in 0..extra {
            let mut x = min.x + col as f32 * step_x;
            let mut y = min.y + row as f32 * step_y;
            if x + w > plate_x + 0.5 {
                col = 0;
                row += 1;
                x = min.x;
                y = min.y + row as f32 * step_y;
            }
            if x + w > plate_x + 0.5 || y + d > plate_y + 0.5 {
                return if added == 0 {
                    Err("Those copies do not fit on the plate. Use a smaller gap or fewer copies.")
                } else {
                    Ok(added)
                };
            }
            let new_id = self.alloc();
            let mut copy = origin.clone();
            copy.id = new_id;
            copy.name = format!("{} {}", origin.name, n + 2);
            copy.position.x += x - min.x;
            copy.position.y += y - min.y;
            self.objects.push(copy);
            self.push_undo(Undo::Added(new_id));
            added += 1;
            col += 1;
        }
        self.selection = Selection::Object(id);
        self.touch();
        Ok(added)
    }

    /// Split the selected model on a horizontal plane and keep both pieces.
    pub fn cut_at_z(&mut self, id: u64, z: f32) -> Result<u64, &'static str> {
        let Some(obj) = self.object(id).cloned() else {
            return Err("Select a model first.");
        };
        let world = Self::world_mesh(&obj);
        let Some((min, max)) = world.bounds() else {
            return Err("That model has no triangles.");
        };
        if z <= min[2] + 0.05 || z >= max[2] - 0.05 {
            return Err(
                "The cut sits outside the model. Pick a height between its bottom and its top.",
            );
        }
        let (below, above) = world.split_at_z(z);
        if below.triangle_count() == 0 || above.triangle_count() == 0 {
            return Err("The cut did not produce two pieces.");
        }
        let added = self.alloc();
        if let Some(lower) = self.object_mut(id) {
            lower.mesh = below;
            lower.position = Vec3::ZERO;
            lower.rotation_deg = Vec3::ZERO;
            lower.scale = Vec3::ONE;
            lower.mesh_rev = lower.mesh_rev.wrapping_add(1);
            if !lower.name.ends_with(" lower") {
                lower.name = format!("{} lower", lower.name);
            }
            cache_bounds(lower);
        }
        let mut upper = obj.clone();
        upper.id = added;
        upper.mesh = above;
        upper.position = Vec3::ZERO;
        upper.rotation_deg = Vec3::ZERO;
        upper.scale = Vec3::ONE;
        upper.mesh_rev = 1;
        upper.name = format!("{} upper", obj.name);
        cache_bounds(&mut upper);
        self.objects.push(upper);
        self.push_undo(Undo::Cut {
            original: obj,
            added,
        });
        self.selection = Selection::Object(added);
        self.touch();
        Ok(added)
    }

    /// A hollow model with no drain near it. Resin and air need that hole.
    pub fn hollow_without_drain(&self) -> Option<&str> {
        for obj in &self.objects {
            if !obj.hollow {
                continue;
            }
            let Some((min, max)) = Self::display_bounds(obj) else {
                continue;
            };
            let covered = self.drains.iter().any(|drain| {
                drain.origin.x >= min.x - 2.0
                    && drain.origin.x <= max.x + 2.0
                    && drain.origin.y >= min.y - 2.0
                    && drain.origin.y <= max.y + 2.0
                    && drain.origin.z >= min.z - 4.0
                    && drain.origin.z <= max.z + 2.0
            });
            if !covered {
                return Some(obj.name.as_str());
            }
        }
        None
    }

    /// Names of the first two models whose boxes occupy the same space.
    pub fn overlap_warning(&self) -> Option<String> {
        let mut boxes = Vec::new();
        for obj in &self.objects {
            if let Some((min, max)) = Self::display_bounds(obj) {
                boxes.push((obj.name.as_str(), min, max));
            }
        }
        for i in 0..boxes.len() {
            for j in (i + 1)..boxes.len() {
                let (an, a0, a1) = boxes[i];
                let (bn, b0, b1) = boxes[j];
                let hit = a0.x < b1.x - 0.2
                    && a1.x > b0.x + 0.2
                    && a0.y < b1.y - 0.2
                    && a1.y > b0.y + 0.2
                    && a0.z < b1.z - 0.2
                    && a1.z > b0.z + 0.2;
                if hit {
                    return Some(format!("{an} overlaps {bn}"));
                }
            }
        }
        None
    }

    pub fn delete_selection(&mut self) {
        match self.selection {
            Selection::Object(id) => {
                if let Some(i) = self.objects.iter().position(|o| o.id == id) {
                    let object = self.objects.remove(i);
                    let supports: Vec<Support> = self
                        .supports
                        .iter()
                        .copied()
                        .filter(|s| s.object_id == id)
                        .collect();
                    self.supports.retain(|s| s.object_id != id);
                    self.push_undo(Undo::Deleted { object, supports });
                    self.selection = Selection::None;
                    self.touch();
                }
            }
            Selection::Support(id) => {
                self.push_undo(Undo::Supports {
                    list: self.supports.clone(),
                    lifted: Vec::new(),
                });
                self.supports.retain(|s| s.id != id);
                self.selection = Selection::None;
                self.touch();
            }
            Selection::Drain(id) => {
                self.push_undo(Undo::Drains(self.drains.clone()));
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
            obj.mesh_rev = obj.mesh_rev.wrapping_add(1);
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
        self.touch_xform();
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
            obj.mesh_rev = obj.mesh_rev.wrapping_add(1);
            cache_bounds(obj);
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
                self.push_undo(Undo::Added(new_id));
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
        self.touch_xform();
    }

    pub fn auto_layout(&mut self, plate_x: f32, plate_y: f32) {
        let prior: Vec<Undo> = self
            .objects
            .iter()
            .map(|obj| Undo::Xform {
                id: obj.id,
                position: obj.position,
                rotation_deg: obj.rotation_deg,
                scale: obj.scale,
            })
            .collect();
        for edit in prior {
            self.push_undo(edit);
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
        self.touch_xform();
    }

    pub fn outside_plate(&self, plate: Vec3) -> bool {
        self.objects.iter().any(|obj| {
            Self::display_bounds(obj).is_some_and(|(min, max)| {
                min.x < -0.05
                    || min.y < -0.05
                    || max.x > plate.x + 0.05
                    || max.y > plate.y + 0.05
                    || max.z > plate.z + 0.05
                    || min.z < -0.05
            })
        })
    }

    /// Grow supports on the selected model only. Returns false when nothing
    /// is selected, so another model on the plate is left alone.
    pub fn add_auto_supports(&mut self) -> bool {
        let Some(id) = self.edit_target() else {
            return false;
        };
        let Some(profile) = self.object(id).map(|obj| obj.support) else {
            return false;
        };
        let list = self.supports.clone();
        let mut lifted = Vec::new();
        let previous = self.raise_bottom(id, profile.lift_mm.max(0.0));
        let Some(obj) = self.object(id) else {
            return false;
        };
        let world = Self::world_mesh(obj);
        let mut id_gen = self.next_id;
        let mut found = supports::auto_supports(
            &world.vertices,
            &world.indices,
            &profile.style,
            profile.platform_only,
            &mut id_gen,
        );
        self.next_id = id_gen;
        if found.is_empty() {
            if let Some(position) = previous {
                if let Some(obj) = self.object_mut(id) {
                    obj.position = position;
                }
            }
            return false;
        }
        for support in &mut found {
            support.object_id = id;
        }
        if let Some(position) = previous {
            lifted.push((id, position));
        }
        self.supports.retain(|s| s.object_id != id);
        self.supports.extend(found);
        self.push_undo(Undo::Supports { list, lifted });
        self.touch();
        true
    }

    /// Remember the support list once, so a drag can move many times and still undo in one step.
    pub fn remember_supports(&mut self) {
        self.push_undo(Undo::Supports {
            list: self.supports.clone(),
            lifted: Vec::new(),
        });
    }

    /// The support tip closest to the ray, when it is within `radius` millimetres.
    pub fn support_near_ray(&self, origin: Vec3, dir: Vec3, radius: f32) -> Option<u64> {
        let mut best: Option<(f32, u64)> = None;
        let limit = radius.max(0.0) * radius.max(0.0);
        for support in &self.supports {
            let tip = Vec3::new(support.x, support.y, support.z_top);
            let along = (tip - origin).dot(dir);
            if along < 0.0 {
                continue;
            }
            let closest = origin + dir * along;
            let dist2 = (tip - closest).length_squared();
            if dist2 <= limit && best.map(|(old, _)| dist2 < old).unwrap_or(true) {
                best = Some((dist2, support.id));
            }
        }
        best.map(|(_, id)| id)
    }

    /// Move one tip onto a new point on its own model. The caller records undo.
    pub fn relocate_support(&mut self, id: u64, point: Vec3) -> bool {
        let Some(current) = self.supports.iter().find(|s| s.id == id).copied() else {
            return false;
        };
        let Some(obj) = self.object(current.object_id) else {
            return false;
        };
        let platform_only = obj.support.platform_only;
        let world = Self::world_mesh(obj);
        let mut next = supports::manual_support(
            point.x,
            point.y,
            point.z,
            &world.vertices,
            &world.indices,
            platform_only,
            id,
        );
        next.object_id = current.object_id;
        if let Some(slot) = self.supports.iter_mut().find(|s| s.id == id) {
            *slot = next;
        }
        self.selection = Selection::Support(id);
        self.touch();
        true
    }

    /// Remove tips on one model that sit within `radius` of `point`.
    /// Returns how many were removed. The caller records undo.
    pub fn erase_supports_near(&mut self, object_id: u64, point: Vec3, radius: f32) -> usize {
        let limit = radius.max(0.0) * radius.max(0.0);
        let before = self.supports.len();
        self.supports.retain(|support| {
            if support.object_id != object_id {
                return true;
            }
            let tip = Vec3::new(support.x, support.y, support.z_top);
            (tip - point).length_squared() > limit
        });
        let removed = before - self.supports.len();
        if removed == 0 {
            return 0;
        }
        if let Selection::Support(id) = self.selection {
            if !self.supports.iter().any(|s| s.id == id) {
                self.selection = Selection::Object(object_id);
            }
        }
        self.touch();
        removed
    }

    pub fn add_support_at(&mut self, point: Vec3, object_id: u64) {
        let Some(obj) = self.object(object_id) else {
            return;
        };
        let platform_only = obj.support.platform_only;
        let world = Self::world_mesh(obj);
        self.push_undo(Undo::Supports {
            list: self.supports.clone(),
            lifted: Vec::new(),
        });
        let id = self.alloc();
        let mut support = supports::manual_support(
            point.x,
            point.y,
            point.z,
            &world.vertices,
            &world.indices,
            platform_only,
            id,
        );
        support.object_id = object_id;
        self.supports.push(support);
        self.selection = Selection::Support(id);
        self.touch();
    }

    /// Plant supports under islands that belong to the selected model.
    /// Returns how many were added.
    pub fn add_island_supports(&mut self, islands: &[(f32, f32, f32)]) -> usize {
        let Some(object_id) = self.edit_target() else {
            return 0;
        };
        let Some((min, max)) = self.object(object_id).and_then(Self::world_bounds) else {
            return 0;
        };
        let mine: Vec<(f32, f32, f32)> = islands
            .iter()
            .copied()
            .filter(|(x, y, _)| {
                *x >= min.x - 2.0 && *x <= max.x + 2.0 && *y >= min.y - 2.0 && *y <= max.y + 2.0
            })
            .collect();
        if mine.is_empty() {
            return 0;
        }
        let platform_only = self
            .object(object_id)
            .map(|obj| obj.support.platform_only)
            .unwrap_or(false);
        let world = self
            .object(object_id)
            .map(Self::world_mesh)
            .unwrap_or(Mesh {
                vertices: Vec::new(),
                indices: Vec::new(),
            });
        self.push_undo(Undo::Supports {
            list: self.supports.clone(),
            lifted: Vec::new(),
        });
        for (x, y, z) in &mine {
            let id = self.alloc();
            let mut support = supports::manual_support(
                *x,
                *y,
                *z,
                &world.vertices,
                &world.indices,
                platform_only,
                id,
            );
            support.object_id = object_id;
            self.supports.push(support);
        }
        self.touch();
        mine.len()
    }

    /// Remove supports that belong to the selected model.
    pub fn clear_supports(&mut self) -> bool {
        let Some(id) = self.edit_target() else {
            return false;
        };
        if !self.supports.iter().any(|s| s.object_id == id) {
            return false;
        }
        self.push_undo(Undo::Supports {
            list: self.supports.clone(),
            lifted: Vec::new(),
        });
        self.supports.retain(|s| s.object_id != id);
        if let Selection::Support(sid) = self.selection {
            if !self.supports.iter().any(|s| s.id == sid) {
                self.selection = Selection::Object(id);
            }
        }
        self.touch();
        true
    }

    /// A drain through the bottom center, aimed up into the part.
    pub fn punch_bottom_drain(&mut self, id: u64) {
        let Some(obj) = self.object(id) else {
            return;
        };
        let Some((min, max)) = Self::world_bounds(obj) else {
            return;
        };
        let need = obj.bottom_cap_mm + obj.wall_mm + 1.0;
        let depth = self.drain_depth_mm.max(need).clamp(1.0, 40.0);
        let radius = (self.drain_diameter_mm * 0.5).clamp(0.2, 8.0);
        let origin = Vec3::new((min.x + max.x) * 0.5, (min.y + max.y) * 0.5, min.z + 0.3);
        self.push_undo(Undo::Drains(self.drains.clone()));
        let drain_id = self.alloc();
        self.drains.push(DrainHole {
            id: drain_id,
            origin,
            axis: Vec3::Z,
            radius_mm: radius,
            depth_mm: depth,
        });
        self.selection = Selection::Drain(drain_id);
        self.touch();
    }

    /// Raise the part until its lowest point is `lift` above the bed.
    /// A second support pass does not stack another lift on top.
    fn raise_bottom(&mut self, id: u64, lift: f32) -> Option<Vec3> {
        if lift <= 0.05 {
            return None;
        }
        let (min, _) = Self::world_bounds(self.object(id)?)?;
        let gap = lift - min.z;
        if gap <= 0.05 {
            return None;
        }
        let obj = self.object_mut(id)?;
        let previous = obj.position;
        obj.position.z += gap;
        Some(previous)
    }

    pub fn add_drain(&mut self, origin: Vec3, into_model: Vec3) {
        self.push_undo(Undo::Drains(self.drains.clone()));
        let id = self.alloc();
        let axis = if into_model.length() < 1e-4 {
            Vec3::NEG_Z
        } else {
            -into_model.normalize()
        };
        let radius = (self.drain_diameter_mm * 0.5).clamp(0.2, 8.0);
        let depth = self.drain_depth_mm.clamp(1.0, 40.0);
        self.drains.push(DrainHole {
            id,
            origin,
            axis,
            radius_mm: radius,
            depth_mm: depth,
        });
        self.selection = Selection::Drain(id);
        self.touch();
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
                    infill_spacing_mm: 0.0,
                    infill_thickness_mm: 0.0,
                    gyroid: false,
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
        let (rafts, forest) = self.baked_supports();
        for raft in rafts {
            solids.push(Solid {
                vertices: raft.vertices,
                indices: raft.indices,
                hollow: None,
            });
        }
        if forest.triangle_count() > 0 {
            solids.push(Solid {
                vertices: forest.vertices,
                indices: forest.indices,
                hollow: None,
            });
        }
        solids
    }

    /// Rafts and the support forest, each model with its own settings.
    pub fn baked_supports(&self) -> (Vec<Mesh>, Mesh) {
        let (rafts, parts) = self.display_supports();
        let mut forest = Mesh {
            vertices: Vec::new(),
            indices: Vec::new(),
        };
        parts.append_into(&mut forest);
        (rafts, forest)
    }

    /// Rafts plus each support piece, so the view can hide a piece.
    /// The slice uses [`Self::baked_supports`], which joins these pieces back.
    pub fn display_supports(&self) -> (Vec<Mesh>, supports::SupportParts) {
        let mut rafts = Vec::new();
        let mut parts = supports::SupportParts::empty();
        for obj in &self.objects {
            let mine: Vec<Support> = self
                .supports
                .iter()
                .copied()
                .filter(|s| s.object_id == obj.id)
                .collect();
            if mine.is_empty() {
                continue;
            }
            let style = &obj.support.style;
            let foot = if style.foot_diam_mm > 0.05 {
                style.foot_diam_mm
            } else {
                style.trunk_mm * 2.1
            };
            let on_bed = mine.iter().any(|s| s.z_base <= 0.45);
            if obj.support.raft && on_bed {
                if let Some(raft) = supports::support_raft(
                    &mine,
                    foot,
                    obj.support.raft_mm,
                    obj.support.raft_margin,
                    obj.support.raft_angle,
                ) {
                    rafts.push(raft);
                }
            }
            let raft_top = if obj.support.raft && on_bed {
                obj.support.raft_mm.max(0.2)
            } else {
                0.0
            };
            parts.append(&supports::forest_parts(
                &mine,
                style,
                raft_top,
                obj.support.braces_on,
                obj.support.brace_dist,
                obj.support.brace_angle,
            ));
        }
        (rafts, parts)
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
        self.raycast_visible(origin, dir, &HashSet::new(), None)
    }

    /// Closest hit on a model that is still shown. `clip_z` drops hits above a
    /// section plane, so a click lands on the surface the cut left visible.
    pub fn raycast_visible(
        &self,
        origin: Vec3,
        dir: Vec3,
        hidden: &HashSet<u64>,
        clip_z: Option<f32>,
    ) -> Option<(u64, Vec3, Vec3)> {
        let mut best: Option<(f32, u64, Vec3, Vec3)> = None;
        for obj in &self.objects {
            if hidden.contains(&obj.id) {
                continue;
            }
            let world = Self::world_mesh(obj);
            for tri in world.indices.chunks_exact(3) {
                let a = Vec3::from_array(world.vertices[tri[0] as usize]);
                let b = Vec3::from_array(world.vertices[tri[1] as usize]);
                let c = Vec3::from_array(world.vertices[tri[2] as usize]);
                if let Some((t, p, n)) = ray_triangle(origin, dir, a, b, c) {
                    if let Some(z) = clip_z {
                        if p.z > z + 0.02 {
                            continue;
                        }
                    }
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

fn cache_bounds(obj: &mut Object) {
    if let Some((min, max)) = obj.mesh.bounds() {
        obj.bounds_min = min;
        obj.bounds_max = max;
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
    fn cut_keeps_two_pieces_and_undo_restores_one() {
        let mut doc = Document::new();
        let id = doc.add_mesh("box".into(), box_mesh([0.0, 0.0, 0.0], [10.0, 10.0, 20.0]));
        let upper = doc.cut_at_z(id, 8.0).unwrap();
        assert_eq!(doc.objects.len(), 2);
        assert_eq!(doc.selection, Selection::Object(upper));
        doc.undo();
        assert_eq!(doc.objects.len(), 1);
        let obj = doc.object(id).unwrap();
        let (_, max) = Document::world_bounds(obj).unwrap();
        assert!((max.z - 20.0).abs() < 0.1, "restored height {}", max.z);
    }

    #[test]
    fn a_hollow_without_a_drain_is_named() {
        let mut doc = Document::new();
        let id = doc.add_mesh("cup".into(), box_mesh([0.0, 0.0, 0.0], [10.0, 10.0, 10.0]));
        doc.object_mut(id).unwrap().hollow = true;
        assert_eq!(doc.hollow_without_drain(), Some("cup"));
        doc.punch_bottom_drain(id);
        assert_eq!(doc.hollow_without_drain(), None);
    }

    #[test]
    fn overlapping_boxes_are_named() {
        let mut doc = Document::new();
        doc.add_mesh("left".into(), box_mesh([0.0, 0.0, 0.0], [10.0, 10.0, 10.0]));
        doc.add_mesh(
            "right".into(),
            box_mesh([5.0, 0.0, 0.0], [15.0, 10.0, 10.0]),
        );
        let warning = doc.overlap_warning().unwrap();
        assert!(
            warning.contains("left") && warning.contains("right"),
            "{warning}"
        );
    }

    #[test]
    fn fill_bed_tiles_a_small_cube() {
        let mut doc = Document::new();
        let id = doc.add_mesh("cube".into(), box_mesh([0.0, 0.0, 0.0], [20.0, 20.0, 20.0]));
        let added = doc.fill_bed(id, 100.0, 50.0).unwrap();
        assert_eq!(added, 7);
        assert_eq!(doc.objects.len(), 8);
    }

    #[test]
    fn supporting_a_part_lifts_it_once() {
        let mut doc = Document::new();
        let id = doc.add_mesh("box".into(), box_mesh([0.0, 0.0, 0.0], [10.0, 10.0, 4.0]));
        doc.support_lift_mm = 5.0;
        if let Some(obj) = doc.object_mut(id) {
            obj.support.lift_mm = 5.0;
        }
        assert!(doc.add_auto_supports());
        let (min, _) = Document::world_bounds(doc.object(id).unwrap()).unwrap();
        assert!(min.z >= 4.9 && min.z <= 5.2, "bottom at {}", min.z);
        assert!(doc.add_auto_supports());
        let (again, _) = Document::world_bounds(doc.object(id).unwrap()).unwrap();
        assert!(again.z <= 5.2, "second pass stacked to {}", again.z);
        doc.undo();
        doc.undo();
        let (back, _) = Document::world_bounds(doc.object(id).unwrap()).unwrap();
        assert!(back.z < 0.2, "undo should drop it back, {}", back.z);
    }

    #[test]
    fn edits_stay_on_the_selected_model() {
        let mut doc = Document::new();
        let a = doc.add_mesh("a".into(), box_mesh([0.0, 0.0, 0.0], [10.0, 10.0, 4.0]));
        let b = doc.add_mesh("b".into(), box_mesh([40.0, 0.0, 0.0], [50.0, 10.0, 4.0]));
        doc.selection = Selection::Object(a);
        if let Some(obj) = doc.object_mut(a) {
            obj.support.lift_mm = 5.0;
            obj.support.style.trunk_mm = 2.4;
            obj.scale = Vec3::splat(1.5);
        }
        assert!(doc.add_auto_supports());
        assert!(doc.supports.iter().all(|s| s.object_id == a));
        let (min_b, _) = Document::world_bounds(doc.object(b).unwrap()).unwrap();
        assert!(min_b.z < 0.2, "the other model was lifted to {}", min_b.z);
        assert!(
            (doc.object(b).unwrap().support.style.trunk_mm - 2.4).abs() > 0.5,
            "trunk size leaked onto the other model"
        );
        assert_eq!(doc.object(b).unwrap().scale, Vec3::ONE);
        doc.selection = Selection::Object(b);
        if let Some(obj) = doc.object_mut(b) {
            obj.support.lift_mm = 0.0;
        }
        let _ = doc.add_auto_supports();
        let b_count = doc.supports.iter().filter(|s| s.object_id == b).count();
        doc.selection = Selection::Object(a);
        assert!(doc.clear_supports());
        assert!(doc.supports.iter().all(|s| s.object_id == b));
        assert_eq!(doc.supports.len(), b_count);
    }

    #[test]
    fn a_tip_can_move_and_a_nearby_tip_can_be_erased() {
        let mut doc = Document::new();
        let id = doc.add_mesh("box".into(), box_mesh([0.0, 0.0, 0.0], [20.0, 20.0, 10.0]));
        doc.add_support_at(Vec3::new(4.0, 4.0, 10.0), id);
        doc.add_support_at(Vec3::new(16.0, 4.0, 10.0), id);
        let first = doc.supports[0].id;
        let origin = Vec3::new(4.0, 4.0, 30.0);
        let dir = Vec3::new(0.0, 0.0, -1.0);
        assert_eq!(doc.support_near_ray(origin, dir, 2.0), Some(first));
        assert!(doc.relocate_support(first, Vec3::new(6.0, 5.0, 10.0)));
        let moved = doc.supports.iter().find(|s| s.id == first).unwrap();
        assert!((moved.x - 6.0).abs() < 0.05, "x {}", moved.x);
        assert_eq!(moved.object_id, id);
        doc.remember_supports();
        let removed = doc.erase_supports_near(id, Vec3::new(6.0, 5.0, 10.0), 2.0);
        assert_eq!(removed, 1);
        assert_eq!(doc.supports.len(), 1);
        assert!(doc.supports.iter().all(|s| s.object_id == id));
        doc.undo();
        assert_eq!(doc.supports.len(), 2);
    }

    #[test]
    fn a_section_ignores_the_model_above_the_cut() {
        let mut doc = Document::new();
        doc.add_mesh("box".into(), box_mesh([0.0, 0.0, 0.0], [10.0, 10.0, 10.0]));
        let origin = Vec3::new(5.0, 5.0, 30.0);
        let dir = Vec3::new(0.0, 0.0, -1.0);
        let (_, top, _) = doc.raycast(origin, dir).unwrap();
        assert!(top.z > 9.0, "top hit {}", top.z);
        let (_, cut, _) = doc
            .raycast_visible(origin, dir, &HashSet::new(), Some(4.0))
            .unwrap();
        assert!(cut.z <= 4.05, "section hit {}", cut.z);
    }

    #[test]
    fn a_hidden_model_is_not_clickable() {
        let mut doc = Document::new();
        let id = doc.add_mesh("box".into(), box_mesh([0.0, 0.0, 0.0], [10.0, 10.0, 10.0]));
        let origin = Vec3::new(5.0, 5.0, 30.0);
        let dir = Vec3::new(0.0, 0.0, -1.0);
        let mut hidden = HashSet::new();
        hidden.insert(id);
        assert!(doc.raycast_visible(origin, dir, &hidden, None).is_none());
        assert!(doc.raycast(origin, dir).is_some());
    }

    #[test]
    fn redo_puts_a_move_and_a_delete_back() {
        let mut doc = Document::new();
        let id = doc.add_mesh("box".into(), box_mesh([0.0, 0.0, 0.0], [10.0, 10.0, 10.0]));
        doc.push_xform_undo(id);
        doc.object_mut(id).unwrap().position.x = 8.0;
        assert!(doc.undo());
        assert!(doc.object(id).unwrap().position.x.abs() < 0.01);
        assert!(doc.redo());
        assert!((doc.object(id).unwrap().position.x - 8.0).abs() < 0.01);
        doc.selection = Selection::Object(id);
        doc.delete_selection();
        assert!(doc.objects.is_empty());
        assert!(doc.undo());
        assert_eq!(doc.objects.len(), 1);
        assert!(doc.redo());
        assert!(doc.objects.is_empty());
        assert!(doc.undo());
        let id = doc.objects[0].id;
        doc.push_xform_undo(id);
        doc.object_mut(id).unwrap().position.y = 3.0;
        assert!(!doc.redo());
    }

    #[test]
    fn redo_restores_a_cut() {
        let mut doc = Document::new();
        let id = doc.add_mesh("box".into(), box_mesh([0.0, 0.0, 0.0], [10.0, 10.0, 20.0]));
        doc.cut_at_z(id, 8.0).unwrap();
        assert_eq!(doc.objects.len(), 2);
        assert!(doc.undo());
        assert_eq!(doc.objects.len(), 1);
        assert!(doc.redo());
        assert_eq!(doc.objects.len(), 2);
        let heights: Vec<f32> = doc
            .objects
            .iter()
            .filter_map(|obj| Document::world_bounds(obj).map(|(_, max)| max.z))
            .collect();
        assert!(
            heights.iter().any(|z| (*z - 8.0).abs() < 0.2),
            "{heights:?}"
        );
        assert!(
            heights.iter().any(|z| (*z - 20.0).abs() < 0.2),
            "{heights:?}"
        );
    }
}
