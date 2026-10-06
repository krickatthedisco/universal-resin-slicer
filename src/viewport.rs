//! Orbit camera and the OpenGL plate view.

use crate::mesh::Mesh;
use crate::scene::{Document, Selection};
use crate::supports;
use glam::{Mat3, Mat4, Vec3};
use glow::HasContext;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[derive(Clone)]
pub struct Camera {
    pub yaw: f32,
    pub pitch: f32,
    pub distance: f32,
    pub target: Vec3,
}

impl Camera {
    pub fn looking_at_plate(size: Vec3) -> Self {
        Self {
            yaw: 32.0,
            pitch: 28.0,
            distance: size.x.max(size.y) * 0.95,
            target: Vec3::new(size.x * 0.5, size.y * 0.5, size.z * 0.08),
        }
    }

    pub fn eye(&self) -> Vec3 {
        let pitch = self.pitch.clamp(-80.0, 89.0).to_radians();
        let yaw = self.yaw.to_radians();
        let cp = pitch.cos();
        let offset = Vec3::new(
            self.distance * cp * yaw.sin(),
            -self.distance * cp * yaw.cos(),
            self.distance * pitch.sin(),
        );
        self.target + offset
    }

    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        let view = Mat4::look_at_rh(self.eye(), self.target, Vec3::Z);
        let near = (self.distance * 0.008).clamp(0.15, 40.0);
        let far = near + self.distance * 6.0 + 2000.0;
        let proj = Mat4::perspective_rh(40.0_f32.to_radians(), aspect.max(0.2), near, far);
        proj * view
    }

    pub fn ray(&self, rel_x: f32, rel_y: f32, aspect: f32) -> (Vec3, Vec3) {
        let ndc_x = rel_x * 2.0 - 1.0;
        let ndc_y = 1.0 - rel_y * 2.0;
        let inv = self.view_proj(aspect).inverse();
        let near = inv.project_point3(Vec3::new(ndc_x, ndc_y, -1.0));
        let far = inv.project_point3(Vec3::new(ndc_x, ndc_y, 1.0));
        (near, (far - near).normalize())
    }

    /// True when the eye has swung under the plate. The bed is drawn clear
    /// then, so an underside can be clicked for a manual support.
    pub fn under_bed(&self) -> bool {
        self.eye().z < 0.0
    }

    pub fn pan(&mut self, dx_px: f32, dy_px: f32) {
        let eye = self.eye();
        let forward = (self.target - eye).normalize();
        let right = forward.cross(Vec3::Z).normalize_or_zero();
        let up = right.cross(forward).normalize_or_zero();
        let scale = self.distance * 0.0016;
        self.target += right * (-dx_px) * scale + up * dy_px * scale;
    }
}

/// One frame of the plate. Models stay in local space; a move only changes
/// `model`. The support forest and the bed stay in `world` until the structure
/// of the plate changes.
#[derive(Clone)]
pub struct PlateFrame {
    pub world_gen: u64,
    pub world: Arc<Vec<f32>>,
    pub bed: Arc<Vec<f32>>,
    pub lines: Vec<f32>,
    pub line_gen: u64,
    pub objects: Vec<ObjectFrame>,
    pub rafts: Vec<RaftFrame>,
}

#[derive(Clone)]
pub struct ObjectFrame {
    pub id: u64,
    pub mesh_rev: u64,
    pub model: Mat4,
    pub color: [f32; 3],
    pub local: Arc<Vec<f32>>,
    /// World Z of the mesh bounds. A cut outside this range draws no cap.
    pub z_min: f32,
    pub z_max: f32,
}

#[derive(Clone)]
pub struct RaftFrame {
    pub id: u64,
    pub key: u64,
    pub model: Mat4,
    pub tris: Arc<Vec<f32>>,
}

/// What the plate draws. Hiding a piece does not change the slice.
#[derive(Clone, Copy, Debug)]
pub struct PlateView {
    pub contacts: bool,
    pub necks: bool,
    pub trunks: bool,
    pub feet: bool,
    pub branches: bool,
    pub braces: bool,
    pub rafts: bool,
    pub section: bool,
    /// Upper cut. Geometry above this is hidden.
    pub section_z: f32,
    /// Lower cut. Geometry below this is hidden.
    pub section_lo: f32,
}

impl Default for PlateView {
    fn default() -> Self {
        Self {
            contacts: true,
            necks: true,
            trunks: true,
            feet: true,
            branches: true,
            braces: true,
            rafts: true,
            section: false,
            section_z: 10.0,
            section_lo: 0.0,
        }
    }
}

impl PlateView {
    pub fn tips_only(&mut self) {
        self.contacts = true;
        self.necks = false;
        self.trunks = false;
        self.feet = false;
        self.branches = false;
        self.braces = false;
        self.rafts = false;
    }

    pub fn show_all_pieces(&mut self) {
        self.contacts = true;
        self.necks = true;
        self.trunks = true;
        self.feet = true;
        self.branches = true;
        self.braces = true;
        self.rafts = true;
    }

    fn piece_bits(&self) -> u64 {
        let mut bits = 0u64;
        if self.contacts {
            bits |= 1;
        }
        if self.necks {
            bits |= 2;
        }
        if self.trunks {
            bits |= 4;
        }
        if self.feet {
            bits |= 8;
        }
        if self.branches {
            bits |= 16;
        }
        if self.braces {
            bits |= 32;
        }
        if self.rafts {
            bits |= 64;
        }
        bits
    }
}

pub struct ViewCache {
    world_gen: u64,
    world: Arc<Vec<f32>>,
    bed: Arc<Vec<f32>>,
    grid_lines: Vec<f32>,
    locals: HashMap<(u64, u64), Arc<Vec<f32>>>,
}

impl ViewCache {
    pub fn new() -> Self {
        Self {
            world_gen: u64::MAX,
            world: Arc::new(Vec::new()),
            bed: Arc::new(Vec::new()),
            grid_lines: Vec::new(),
            locals: HashMap::new(),
        }
    }

    pub fn frame(
        &mut self,
        doc: &Document,
        plate: Vec3,
        selection: Selection,
        view: &PlateView,
        hidden: &HashSet<u64>,
    ) -> PlateFrame {
        let world_gen = world_stamp(doc.structure, plate, selection, view);
        if world_gen != self.world_gen {
            let mut tris = Vec::new();
            let mut lines = Vec::new();
            let mut bed = Vec::new();
            push_bed(&mut bed, plate);
            push_plate_lines(&mut lines, plate);
            let (rafts, parts) = doc.display_supports();
            if view.rafts {
                for raft in &rafts {
                    push_mesh_flat(&mut tris, raft, [0.45, 0.38, 0.28]);
                }
            }
            if view.contacts {
                push_mesh_flat(&mut tris, &parts.contacts, [0.93, 0.62, 0.28]);
            }
            if view.necks {
                push_mesh_flat(&mut tris, &parts.necks, [0.36, 0.70, 0.58]);
            }
            if view.trunks {
                push_mesh_flat(&mut tris, &parts.trunks, [0.22, 0.55, 0.52]);
            }
            if view.feet {
                push_mesh_flat(&mut tris, &parts.feet, [0.55, 0.42, 0.30]);
            }
            if view.branches {
                push_mesh_flat(&mut tris, &parts.branches, [0.30, 0.58, 0.46]);
            }
            if view.braces {
                push_mesh_flat(&mut tris, &parts.braces, [0.38, 0.50, 0.68]);
            }
            if let Selection::Support(id) = selection {
                if let Some(support) = doc.supports.iter().find(|s| s.id == id) {
                    let style = doc
                        .object(support.object_id)
                        .map(|obj| obj.support.style)
                        .unwrap_or(doc.style);
                    push_mesh_flat(
                        &mut tris,
                        &supports::contact_marker(support, &style),
                        [0.95, 0.78, 0.35],
                    );
                }
            }
            for drain in &doc.drains {
                let mesh = supports::hole_mesh(
                    drain.origin,
                    drain.axis,
                    drain.radius_mm,
                    drain.inner_radius(),
                    drain.extend_mm,
                    drain.depth_mm,
                );
                let color = if selection == Selection::Drain(drain.id) {
                    [0.95, 0.45, 0.30]
                } else {
                    [0.75, 0.28, 0.22]
                };
                push_mesh_flat(&mut tris, &mesh, color);
            }
            self.world = Arc::new(tris);
            self.bed = Arc::new(bed);
            self.grid_lines = lines;
            self.world_gen = world_gen;
        }

        let mut lines = self.grid_lines.clone();
        let mut objects = Vec::with_capacity(doc.objects.len());
        let mut live_locals = HashSet::new();
        for obj in &doc.objects {
            if hidden.contains(&obj.id) {
                continue;
            }
            let selected = selection == Selection::Object(obj.id);
            let outside = Document::display_bounds(obj).is_some_and(|(min, max)| {
                min.x < -0.2
                    || min.y < -0.2
                    || max.x > plate.x + 0.2
                    || max.y > plate.y + 0.2
                    || max.z > plate.z + 0.2
            });
            let color = if outside {
                [0.72, 0.32, 0.28]
            } else if selected {
                [0.93, 0.78, 0.52]
            } else {
                [0.76, 0.64, 0.46]
            };
            let shell = shell_stamp(obj);
            let local_key = (obj.id, shell);
            live_locals.insert(local_key);
            let local = if let Some(buf) = self.locals.get(&local_key) {
                buf.clone()
            } else {
                let buf = Arc::new(local_tris(&view_mesh(obj)));
                self.locals.insert(local_key, buf.clone());
                buf
            };
            let (z_min, z_max) = Document::display_bounds(obj)
                .map(|(min, max)| (min.z, max.z))
                .unwrap_or((1.0, 0.0));
            objects.push(ObjectFrame {
                id: obj.id,
                mesh_rev: shell,
                model: Document::matrix(obj),
                color,
                local,
                z_min,
                z_max,
            });
            if selected {
                if let Some((min, max)) = Document::display_bounds(obj) {
                    push_box_lines(&mut lines, min, max, [0.89, 0.63, 0.18]);
                }
            }
        }
        self.locals.retain(|key, _| live_locals.contains(key));
        if view.section {
            let mut lo = view.section_lo;
            let mut hi = view.section_z;
            if lo > hi {
                std::mem::swap(&mut lo, &mut hi);
            }
            if let Some((min_z, max_z)) = model_span(doc, hidden) {
                if hi > min_z + 0.03 && hi < max_z - 0.03 {
                    self.plane_lines(&mut lines, plate, hi, [0.95, 0.55, 0.18]);
                }
                if lo > min_z + 0.03 && lo < max_z - 0.03 {
                    self.plane_lines(&mut lines, plate, lo, [0.45, 0.62, 0.82]);
                }
            }
        }
        PlateFrame {
            world_gen,
            world: self.world.clone(),
            bed: self.bed.clone(),
            lines,
            line_gen: doc.changed,
            objects,
            rafts: Vec::new(),
        }
    }

    fn plane_lines(&self, lines: &mut Vec<f32>, plate: Vec3, z: f32, color: [f32; 3]) {
        let z = z.clamp(-1.0, plate.z.max(0.0) + 1.0);
        let corners = [
            [0.0, 0.0, z],
            [plate.x, 0.0, z],
            [plate.x, plate.y, z],
            [0.0, plate.y, z],
        ];
        for i in 0..4 {
            push_line(lines, corners[i], corners[(i + 1) % 4], color);
        }
    }
}

fn model_span(doc: &Document, hidden: &HashSet<u64>) -> Option<(f32, f32)> {
    let mut lo = f32::MAX;
    let mut hi = f32::MIN;
    let mut any = false;
    for obj in &doc.objects {
        if hidden.contains(&obj.id) {
            continue;
        }
        if let Some((min, max)) = Document::display_bounds(obj) {
            lo = lo.min(min.z);
            hi = hi.max(max.z);
            any = true;
        }
    }
    any.then_some((lo, hi))
}

impl PlateFrame {
    pub fn add_line(&mut self, a: Vec3, b: Vec3, color: [f32; 3]) {
        push_line(&mut self.lines, a.to_array(), b.to_array(), color);
    }
}

fn quant(v: f32) -> i32 {
    (v * 100.0).round() as i32
}

fn world_stamp(structure: u64, plate: Vec3, selection: Selection, view: &PlateView) -> u64 {
    let sel = match selection {
        Selection::Support(id) => id.wrapping_add(1),
        Selection::Drain(id) => id.wrapping_add(0x1000_0001),
        _ => 0,
    };
    let mut h = structure;
    for n in [
        quant(plate.x) as u64,
        quant(plate.y) as u64,
        quant(plate.z) as u64,
        sel,
        view.piece_bits(),
    ] {
        h = h.wrapping_mul(0x9E3779B1).wrapping_add(n);
    }
    h
}

/// Outside shell, plus the empty inside when the model is hollow.
fn view_mesh(obj: &crate::scene::Object) -> Mesh {
    if !obj.hollow || obj.wall_mm < 0.05 {
        return obj.mesh.clone();
    }
    let mat = Document::matrix(obj);
    if mat.determinant().abs() < 1e-8 {
        return obj.mesh.clone();
    }
    let world = Document::world_mesh(obj);
    let Some((min, max)) = world.bounds() else {
        return obj.mesh.clone();
    };
    let z_lo = min[2] + obj.bottom_cap_mm;
    let z_hi = max[2] - obj.top_cap_mm;
    let Some(cavity) = crate::mesh::cavity_shell(&world, obj.wall_mm, z_lo, z_hi) else {
        return obj.mesh.clone();
    };
    let inv = mat.inverse();
    let local = cavity.transformed(|p| inv.transform_point3(Vec3::from_array(p)).to_array());
    let mut mesh = obj.mesh.clone();
    mesh.append(&local);
    mesh
}

fn shell_stamp(obj: &crate::scene::Object) -> u64 {
    if !obj.hollow {
        return obj.mesh_rev;
    }
    let mut stamp = obj.mesh_rev.wrapping_add(0xA11);
    for n in [
        quant(obj.wall_mm) as u64,
        quant(obj.bottom_cap_mm) as u64,
        quant(obj.top_cap_mm) as u64,
        quant(obj.rotation_deg.x) as u64,
        quant(obj.rotation_deg.y) as u64,
        quant(obj.rotation_deg.z) as u64,
        (obj.scale.x * 1000.0).round() as u64,
        (obj.scale.y * 1000.0).round() as u64,
        (obj.scale.z * 1000.0).round() as u64,
    ] {
        stamp = stamp.wrapping_mul(131).wrapping_add(n);
    }
    stamp
}

fn local_tris(mesh: &Mesh) -> Vec<f32> {
    let normals = smooth_world_normals(mesh, Mat4::IDENTITY);
    let mut tris = Vec::with_capacity(mesh.indices.len() * 9);
    for tri in mesh.indices.chunks_exact(3) {
        for &index in tri {
            let i = index as usize;
            if i >= mesh.vertices.len() || i >= normals.len() {
                continue;
            }
            push_vert(&mut tris, mesh.vertices[i], normals[i], [1.0, 1.0, 1.0]);
        }
    }
    tris
}

fn smooth_world_normals(mesh: &Mesh, mat: Mat4) -> Vec<[f32; 3]> {
    let mut acc = vec![Vec3::ZERO; mesh.vertices.len()];
    for tri in mesh.indices.chunks_exact(3) {
        let p = [
            mat.transform_point3(Vec3::from_array(mesh.vertices[tri[0] as usize])),
            mat.transform_point3(Vec3::from_array(mesh.vertices[tri[1] as usize])),
            mat.transform_point3(Vec3::from_array(mesh.vertices[tri[2] as usize])),
        ];
        let cross = (p[1] - p[0]).cross(p[2] - p[0]);
        for &index in tri {
            acc[index as usize] += cross;
        }
    }
    acc.into_iter()
        .map(|n| {
            let len = n.length();
            if len < 1e-8 {
                [0.0, 0.0, 1.0]
            } else {
                (n / len).to_array()
            }
        })
        .collect()
}

fn push_mesh_flat(tris: &mut Vec<f32>, mesh: &Mesh, color: [f32; 3]) {
    for tri in mesh.indices.chunks_exact(3) {
        let a = mesh.vertices[tri[0] as usize];
        let b = mesh.vertices[tri[1] as usize];
        let c = mesh.vertices[tri[2] as usize];
        let n = safe_normal(crate::mesh::face_normal(a, b, c));
        push_vert(tris, a, n, color);
        push_vert(tris, b, n, color);
        push_vert(tris, c, n, color);
    }
}

fn safe_normal(n: [f32; 3]) -> [f32; 3] {
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    if len < 1e-8 {
        [0.0, 0.0, 1.0]
    } else {
        [n[0] / len, n[1] / len, n[2] / len]
    }
}

fn cap_quad_verts() -> Vec<f32> {
    let s = 20_000.0;
    let n = [0.0, 0.0, 1.0];
    let c = [1.0, 1.0, 1.0];
    let corners = [[-s, -s, 0.0], [s, -s, 0.0], [s, s, 0.0], [-s, s, 0.0]];
    let mut tris = Vec::new();
    for id in [0, 1, 2, 0, 2, 3] {
        push_vert(&mut tris, corners[id], n, c);
    }
    tris
}

fn push_vert(buf: &mut Vec<f32>, p: [f32; 3], n: [f32; 3], c: [f32; 3]) {
    buf.extend_from_slice(&[p[0], p[1], p[2], n[0], n[1], n[2], c[0], c[1], c[2]]);
}

fn push_bed(tris: &mut Vec<f32>, plate: Vec3) {
    let z = -0.04;
    let color = [0.16, 0.18, 0.21];
    let n = [0.0, 0.0, 1.0];
    let corners = [
        [0.0, 0.0, z],
        [plate.x, 0.0, z],
        [plate.x, plate.y, z],
        [0.0, plate.y, z],
    ];
    for id in [0, 1, 2, 0, 2, 3] {
        push_vert(tris, corners[id], n, color);
    }
}

fn push_plate_lines(lines: &mut Vec<f32>, plate: Vec3) {
    let grid = [0.28, 0.30, 0.33];
    let major = [0.42, 0.36, 0.24];
    let mut x = 0.0;
    while x <= plate.x + 0.01 {
        let color = if (x / 50.0).round() * 50.0 == x {
            major
        } else {
            grid
        };
        push_line(lines, [x, 0.0, 0.02], [x, plate.y, 0.02], color);
        x += 10.0;
    }
    let mut y = 0.0;
    while y <= plate.y + 0.01 {
        let color = if (y / 50.0).round() * 50.0 == y {
            major
        } else {
            grid
        };
        push_line(lines, [0.0, y, 0.02], [plate.x, y, 0.02], color);
        y += 10.0;
    }
    let edge = [0.86, 0.62, 0.22];
    push_box_lines(lines, Vec3::ZERO, Vec3::new(plate.x, plate.y, 0.0), edge);
}

fn push_box_lines(lines: &mut Vec<f32>, min: Vec3, max: Vec3, color: [f32; 3]) {
    let p = [
        [min.x, min.y, min.z],
        [max.x, min.y, min.z],
        [max.x, max.y, min.z],
        [min.x, max.y, min.z],
        [min.x, min.y, max.z],
        [max.x, min.y, max.z],
        [max.x, max.y, max.z],
        [min.x, max.y, max.z],
    ];
    let edges = [
        (0, 1),
        (1, 2),
        (2, 3),
        (3, 0),
        (4, 5),
        (5, 6),
        (6, 7),
        (7, 4),
        (0, 4),
        (1, 5),
        (2, 6),
        (3, 7),
    ];
    for (a, b) in edges {
        push_line(lines, p[a], p[b], color);
    }
}

fn push_line(lines: &mut Vec<f32>, a: [f32; 3], b: [f32; 3], color: [f32; 3]) {
    lines.extend_from_slice(&[a[0], a[1], a[2], color[0], color[1], color[2]]);
    lines.extend_from_slice(&[b[0], b[1], b[2], color[0], color[1], color[2]]);
}

pub fn colored_tris(mesh: &Mesh, color: [f32; 3]) -> Vec<f32> {
    let mut tris = Vec::new();
    push_mesh_flat(&mut tris, mesh, color);
    tris
}

// Solid shading follows the usual slicer path (PrusaSlicer / OrcaSlicer
// gouraud): transform the real triangle, interpolate a smooth normal, and
// let the depth buffer hide whatever is behind the shell. Two-sided so a
// hollow still reads when you look into it.
const VERT: &str = r#"
layout(location = 0) in vec3 a_pos;
layout(location = 1) in vec3 a_nrm;
layout(location = 2) in vec3 a_col;
uniform mat4 u_mvp;
uniform mat4 u_model;
uniform mat4 u_normal;
uniform vec3 u_color;
uniform float u_use_color;
out vec3 v_nrm;
out vec3 v_col;
out vec3 v_pos;
void main() {
    v_nrm = mat3(u_normal) * a_nrm;
    v_col = mix(a_col, u_color, u_use_color);
    v_pos = (u_model * vec4(a_pos, 1.0)).xyz;
    gl_Position = u_mvp * vec4(a_pos, 1.0);
}
"#;

const FRAG: &str = r#"
in vec3 v_nrm;
in vec3 v_col;
in vec3 v_pos;
out vec4 out_color;
uniform vec3 u_light;
uniform vec3 u_fill;
uniform vec3 u_eye;
uniform float u_alpha;
uniform float u_show_overhang;
uniform float u_overhang_deg;
uniform float u_clip_on;
uniform float u_clip_z;
uniform float u_clip_lo;
void main() {
    if (u_clip_on > 0.5 && (v_pos.z > u_clip_z || v_pos.z < u_clip_lo)) {
        discard;
    }
    float len2 = dot(v_nrm, v_nrm);
    vec3 n = len2 > 1e-8 ? normalize(v_nrm) : vec3(0.0, 0.0, 1.0);
    if (!gl_FrontFacing) { n = -n; }
    vec3 light = normalize(u_light);
    float ndl = clamp(dot(n, light), 0.0, 1.0);
    float fill = clamp(dot(n, normalize(u_fill)), 0.0, 1.0);
    float shade = 0.50 + 0.44 * ndl + 0.10 * fill;
    vec3 view = normalize(u_eye - v_pos);
    vec3 half_dir = normalize(light + view);
    float spec = pow(clamp(dot(n, half_dir), 0.0, 1.0), 48.0);
    vec3 albedo = v_col;
    if (u_show_overhang > 0.5 && n.z < -0.02) {
        float slope = 90.0 - degrees(acos(clamp(-n.z, 0.0, 1.0)));
        if (slope + 0.05 >= u_overhang_deg) {
            albedo = mix(albedo, vec3(0.86, 0.24, 0.16), 0.82);
        }
    }
    out_color = vec4(albedo * shade + vec3(spec * 0.16), u_alpha);
}
"#;

const LINE_VERT: &str = r#"
layout(location = 0) in vec3 a_pos;
layout(location = 1) in vec3 a_col;
uniform mat4 u_mvp;
out vec3 v_col;
void main() {
    v_col = a_col;
    gl_Position = u_mvp * vec4(a_pos, 1.0);
}
"#;

const LINE_FRAG: &str = r#"
in vec3 v_col;
out vec4 out_color;
void main() {
    out_color = vec4(v_col, 1.0);
}
"#;

const SHEET_VERT: &str = r#"
layout(location = 0) in vec3 a_pos;
layout(location = 1) in vec2 a_uv;
uniform mat4 u_mvp;
out vec2 v_uv;
void main() {
    v_uv = a_uv;
    gl_Position = u_mvp * vec4(a_pos, 1.0);
}
"#;

const SHEET_FRAG: &str = r#"
in vec2 v_uv;
out vec4 out_color;
uniform sampler2D u_tex;
void main() {
    vec4 c = texture(u_tex, v_uv);
    if (c.a < 0.04) {
        discard;
    }
    out_color = vec4(c.rgb, c.a * 0.92);
}
"#;

/// One sliced layer, laid on the plate in millimetres.
#[derive(Clone)]
pub struct LayerSheet {
    pub key: u64,
    pub w: i32,
    pub h: i32,
    pub rgba: Arc<Vec<u8>>,
    pub z: f32,
    pub size_x: f32,
    pub size_y: f32,
}

struct Batch {
    vao: glow::VertexArray,
    vbo: glow::Buffer,
    count: i32,
}

struct MeshGpu {
    batches: Vec<Batch>,
}

struct ObjectGpu {
    id: u64,
    mesh_rev: u64,
    batches: Vec<Batch>,
    model: Mat4,
    color: [f32; 3],
    z_min: f32,
    z_max: f32,
}

struct RaftGpu {
    id: u64,
    key: u64,
    batches: Vec<Batch>,
    model: Mat4,
}

struct SolidLocs {
    mvp: Option<glow::UniformLocation>,
    model: Option<glow::UniformLocation>,
    normal: Option<glow::UniformLocation>,
    color: Option<glow::UniformLocation>,
    use_color: Option<glow::UniformLocation>,
    light: Option<glow::UniformLocation>,
    fill: Option<glow::UniformLocation>,
    eye: Option<glow::UniformLocation>,
    alpha: Option<glow::UniformLocation>,
    show_overhang: Option<glow::UniformLocation>,
    overhang_deg: Option<glow::UniformLocation>,
    clip_on: Option<glow::UniformLocation>,
    clip_z: Option<glow::UniformLocation>,
    clip_lo: Option<glow::UniformLocation>,
}

pub struct Renderer {
    program: glow::Program,
    line_program: glow::Program,
    sheet_program: glow::Program,
    locs: SolidLocs,
    line_mvp: Option<glow::UniformLocation>,
    sheet_mvp: Option<glow::UniformLocation>,
    world: MeshGpu,
    bed: MeshGpu,
    world_gen: u64,
    objects: Vec<ObjectGpu>,
    rafts: Vec<RaftGpu>,
    line_vao: glow::VertexArray,
    line_vbo: glow::Buffer,
    line_verts: i32,
    line_gen: u64,
    sheet_vao: glow::VertexArray,
    sheet_vbo: glow::Buffer,
    sheet_tex: glow::Texture,
    sheet_key: u64,
    ghost: MeshGpu,
    /// One huge quad on Z = 0. A cut draws it where the stencil says the
    /// model was sliced, so the face is a single polygon instead of strips.
    cap_quad: MeshGpu,
}

impl Renderer {
    pub fn new(gl: &glow::Context) -> Result<Self, String> {
        unsafe {
            let program = link(gl, VERT, FRAG)?;
            let line_program = link(gl, LINE_VERT, LINE_FRAG)?;
            let sheet_program = link(gl, SHEET_VERT, SHEET_FRAG)?;
            let locs = SolidLocs {
                mvp: gl.get_uniform_location(program, "u_mvp"),
                model: gl.get_uniform_location(program, "u_model"),
                normal: gl.get_uniform_location(program, "u_normal"),
                color: gl.get_uniform_location(program, "u_color"),
                use_color: gl.get_uniform_location(program, "u_use_color"),
                light: gl.get_uniform_location(program, "u_light"),
                fill: gl.get_uniform_location(program, "u_fill"),
                eye: gl.get_uniform_location(program, "u_eye"),
                alpha: gl.get_uniform_location(program, "u_alpha"),
                show_overhang: gl.get_uniform_location(program, "u_show_overhang"),
                overhang_deg: gl.get_uniform_location(program, "u_overhang_deg"),
                clip_on: gl.get_uniform_location(program, "u_clip_on"),
                clip_z: gl.get_uniform_location(program, "u_clip_z"),
                clip_lo: gl.get_uniform_location(program, "u_clip_lo"),
            };
            let line_mvp = gl.get_uniform_location(line_program, "u_mvp");
            let sheet_mvp = gl.get_uniform_location(sheet_program, "u_mvp");
            let line_vao = gl.create_vertex_array().map_err(|e| e.to_string())?;
            let line_vbo = gl.create_buffer().map_err(|e| e.to_string())?;
            let sheet_vao = gl.create_vertex_array().map_err(|e| e.to_string())?;
            let sheet_vbo = gl.create_buffer().map_err(|e| e.to_string())?;
            let sheet_tex = gl.create_texture().map_err(|e| e.to_string())?;
            let mut cap_quad = MeshGpu {
                batches: Vec::new(),
            };
            upload_mesh(gl, &mut cap_quad.batches, &cap_quad_verts());
            Ok(Self {
                program,
                line_program,
                sheet_program,
                locs,
                line_mvp,
                sheet_mvp,
                world: MeshGpu {
                    batches: Vec::new(),
                },
                bed: MeshGpu {
                    batches: Vec::new(),
                },
                world_gen: u64::MAX,
                objects: Vec::new(),
                rafts: Vec::new(),
                line_vao,
                line_vbo,
                line_verts: 0,
                line_gen: u64::MAX,
                sheet_vao,
                sheet_vbo,
                sheet_tex,
                sheet_key: u64::MAX,
                ghost: MeshGpu {
                    batches: Vec::new(),
                },
                cap_quad,
            })
        }
    }

    pub fn sync(&mut self, gl: &glow::Context, frame: &PlateFrame) {
        if frame.world_gen != self.world_gen {
            upload_mesh(gl, &mut self.world.batches, &frame.world);
            upload_mesh(gl, &mut self.bed.batches, &frame.bed);
            self.world_gen = frame.world_gen;
        }
        if frame.line_gen != self.line_gen {
            unsafe {
                upload_attrib(
                    gl,
                    self.line_vao,
                    self.line_vbo,
                    &frame.lines,
                    6,
                    &[(0, 3, 0), (1, 3, 3)],
                );
            }
            self.line_verts = (frame.lines.len() / 6) as i32;
            self.line_gen = frame.line_gen;
        }
        self.sync_objects(gl, frame);
        self.sync_rafts(gl, frame);
    }

    fn sync_objects(&mut self, gl: &glow::Context, frame: &PlateFrame) {
        let mut next = Vec::with_capacity(frame.objects.len());
        for obj in &frame.objects {
            if obj.local.is_empty() {
                continue;
            }
            if let Some(i) = self.objects.iter().position(|gpu| gpu.id == obj.id) {
                let mut gpu = self.objects.swap_remove(i);
                if gpu.mesh_rev != obj.mesh_rev {
                    upload_mesh(gl, &mut gpu.batches, &obj.local);
                    gpu.mesh_rev = obj.mesh_rev;
                }
                gpu.model = obj.model;
                gpu.color = obj.color;
                gpu.z_min = obj.z_min;
                gpu.z_max = obj.z_max;
                next.push(gpu);
            } else {
                let mut batches = Vec::new();
                upload_mesh(gl, &mut batches, &obj.local);
                next.push(ObjectGpu {
                    id: obj.id,
                    mesh_rev: obj.mesh_rev,
                    batches,
                    model: obj.model,
                    color: obj.color,
                    z_min: obj.z_min,
                    z_max: obj.z_max,
                });
            }
        }
        for gpu in &self.objects {
            drop_batches(gl, &gpu.batches);
        }
        self.objects = next;
    }

    fn sync_rafts(&mut self, gl: &glow::Context, frame: &PlateFrame) {
        let mut next = Vec::with_capacity(frame.rafts.len());
        for raft in &frame.rafts {
            if raft.tris.is_empty() {
                continue;
            }
            if let Some(i) = self.rafts.iter().position(|gpu| gpu.id == raft.id) {
                let mut gpu = self.rafts.swap_remove(i);
                if gpu.key != raft.key {
                    upload_mesh(gl, &mut gpu.batches, &raft.tris);
                    gpu.key = raft.key;
                }
                gpu.model = raft.model;
                next.push(gpu);
            } else {
                let mut batches = Vec::new();
                upload_mesh(gl, &mut batches, &raft.tris);
                next.push(RaftGpu {
                    id: raft.id,
                    key: raft.key,
                    batches,
                    model: raft.model,
                });
            }
        }
        for gpu in &self.rafts {
            drop_batches(gl, &gpu.batches);
        }
        self.rafts = next;
    }

    pub fn paint(
        &mut self,
        gl: &glow::Context,
        camera: &Camera,
        aspect: f32,
        overhang_deg: Option<f32>,
        clip: Option<(f32, f32)>,
        sheet: Option<&LayerSheet>,
        ghost: &[f32],
    ) {
        unsafe {
            let vp = camera.view_proj(aspect);
            let eye = camera.eye();
            gl.disable(glow::CULL_FACE);
            gl.disable(glow::BLEND);
            gl.enable(glow::DEPTH_TEST);
            gl.depth_func(glow::LEQUAL);
            gl.depth_mask(true);
            gl.clear_depth_f32(1.0);
            gl.clear_stencil(0);
            gl.front_face(glow::CCW);
            gl.disable(glow::POLYGON_OFFSET_FILL);
            gl.clear_color(0.11, 0.12, 0.14, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT | glow::STENCIL_BUFFER_BIT);
            gl.use_program(Some(self.program));
            gl.uniform_3_f32(self.locs.light.as_ref(), 0.35, -0.25, 0.90);
            gl.uniform_3_f32(self.locs.fill.as_ref(), -0.4, 0.6, 0.2);
            gl.uniform_3_f32(self.locs.eye.as_ref(), eye.x, eye.y, eye.z);
            gl.uniform_1_f32(self.locs.show_overhang.as_ref(), 0.0);
            gl.uniform_1_f32(
                self.locs.overhang_deg.as_ref(),
                overhang_deg.unwrap_or(45.0),
            );
            let (clip_lo, clip_hi) = clip.unwrap_or((0.0, 0.0));
            gl.uniform_1_f32(self.locs.clip_on.as_ref(), 0.0);
            gl.uniform_1_f32(self.locs.clip_lo.as_ref(), clip_lo);
            gl.uniform_1_f32(self.locs.clip_z.as_ref(), clip_hi);
            let under = camera.under_bed();
            if !under {
                self.draw_solid(
                    gl,
                    &self.bed.batches,
                    Mat4::IDENTITY,
                    [1.0, 1.0, 1.0],
                    0.0,
                    1.0,
                    vp,
                );
            }
            self.draw_solid(
                gl,
                &self.world.batches,
                Mat4::IDENTITY,
                [1.0, 1.0, 1.0],
                0.0,
                1.0,
                vp,
            );
            let raft_color = [0.45, 0.38, 0.28];
            for raft in &self.rafts {
                self.draw_solid(gl, &raft.batches, raft.model, raft_color, 0.0, 1.0, vp);
            }
            if overhang_deg.is_some() {
                gl.uniform_1_f32(self.locs.show_overhang.as_ref(), 1.0);
            }
            if clip.is_some() {
                gl.uniform_1_f32(self.locs.clip_on.as_ref(), 1.0);
            }
            for obj in &self.objects {
                self.draw_solid(gl, &obj.batches, obj.model, obj.color, 1.0, 1.0, vp);
            }
            gl.uniform_1_f32(self.locs.show_overhang.as_ref(), 0.0);
            gl.uniform_1_f32(self.locs.clip_on.as_ref(), 0.0);
            if let Some((lo, hi)) = clip {
                self.draw_section_caps(gl, lo, hi, vp);
            }
            if !ghost.is_empty() {
                upload_mesh(gl, &mut self.ghost.batches, ghost);
                gl.enable(glow::BLEND);
                gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);
                gl.depth_mask(false);
                gl.depth_func(glow::LEQUAL);
                self.draw_solid(
                    gl,
                    &self.ghost.batches,
                    Mat4::IDENTITY,
                    [1.0, 1.0, 1.0],
                    0.0,
                    0.55,
                    vp,
                );
                gl.depth_func(glow::GREATER);
                self.draw_solid(
                    gl,
                    &self.ghost.batches,
                    Mat4::IDENTITY,
                    [1.0, 1.0, 1.0],
                    0.0,
                    0.28,
                    vp,
                );
                gl.depth_func(glow::LEQUAL);
                gl.depth_mask(true);
                gl.disable(glow::BLEND);
            }
            if under {
                gl.enable(glow::BLEND);
                gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);
                gl.depth_mask(false);
                self.draw_solid(
                    gl,
                    &self.bed.batches,
                    Mat4::IDENTITY,
                    [1.0, 1.0, 1.0],
                    0.0,
                    0.16,
                    vp,
                );
                gl.depth_mask(true);
                gl.disable(glow::BLEND);
            }

            if let Some(sheet) = sheet {
                self.draw_sheet(gl, vp, sheet);
            }

            gl.use_program(Some(self.line_program));
            gl.uniform_matrix_4_f32_slice(self.line_mvp.as_ref(), false, vp.as_ref());
            gl.bind_vertex_array(Some(self.line_vao));
            gl.draw_arrays(glow::LINES, 0, self.line_verts);
            gl.bind_vertex_array(None);
            gl.disable(glow::DEPTH_TEST);
            gl.disable(glow::STENCIL_TEST);
            gl.disable(glow::CULL_FACE);
            gl.color_mask(true, true, true, true);
        }
    }

    /// Fill each cut the way a model viewer does (three.js clipping stencil).
    ///
    /// Back faces increment the stencil and front faces decrement it, with
    /// color and depth writes off, clipped to that one plane. A closed mesh
    /// leaves a non-zero stencil only on the cross-section, and a hole stays
    /// zero. One quad is drawn there, so the face has no strip edges to band.
    fn draw_section_caps(&self, gl: &glow::Context, clip_lo: f32, clip_hi: f32, vp: Mat4) {
        unsafe {
            let planes = [(-1.0e6, clip_hi, clip_hi), (clip_lo, 1.0e6, clip_lo)];
            gl.enable(glow::STENCIL_TEST);
            gl.stencil_mask(0xff);
            for obj in &self.objects {
                for (lo, hi, z) in planes {
                    if z <= obj.z_min + 0.03 || z >= obj.z_max - 0.03 {
                        continue;
                    }
                    gl.clear_stencil(0);
                    gl.clear(glow::STENCIL_BUFFER_BIT);
                    gl.color_mask(false, false, false, false);
                    gl.depth_mask(false);
                    gl.disable(glow::DEPTH_TEST);
                    gl.enable(glow::CULL_FACE);
                    gl.stencil_func(glow::ALWAYS, 0, 0xff);
                    gl.uniform_1_f32(self.locs.clip_on.as_ref(), 1.0);
                    gl.uniform_1_f32(self.locs.clip_lo.as_ref(), lo);
                    gl.uniform_1_f32(self.locs.clip_z.as_ref(), hi);
                    gl.uniform_1_f32(self.locs.show_overhang.as_ref(), 0.0);

                    gl.cull_face(glow::FRONT);
                    gl.stencil_op(glow::INCR_WRAP, glow::INCR_WRAP, glow::INCR_WRAP);
                    self.draw_solid(gl, &obj.batches, obj.model, obj.color, 1.0, 1.0, vp);

                    gl.cull_face(glow::BACK);
                    gl.stencil_op(glow::DECR_WRAP, glow::DECR_WRAP, glow::DECR_WRAP);
                    self.draw_solid(gl, &obj.batches, obj.model, obj.color, 1.0, 1.0, vp);

                    gl.color_mask(true, true, true, true);
                    gl.depth_mask(true);
                    gl.enable(glow::DEPTH_TEST);
                    gl.depth_func(glow::LEQUAL);
                    gl.disable(glow::CULL_FACE);
                    gl.uniform_1_f32(self.locs.clip_on.as_ref(), 0.0);
                    gl.stencil_func(glow::NOTEQUAL, 0, 0xff);
                    gl.stencil_op(glow::KEEP, glow::KEEP, glow::KEEP);
                    let model = Mat4::from_translation(Vec3::new(0.0, 0.0, z));
                    self.draw_solid(gl, &self.cap_quad.batches, model, obj.color, 1.0, 1.0, vp);
                }
            }
            gl.disable(glow::STENCIL_TEST);
            gl.disable(glow::CULL_FACE);
            gl.color_mask(true, true, true, true);
            gl.depth_mask(true);
            gl.enable(glow::DEPTH_TEST);
            gl.stencil_op(glow::KEEP, glow::KEEP, glow::KEEP);
            gl.stencil_func(glow::ALWAYS, 0, 0xff);
            gl.uniform_1_f32(self.locs.clip_on.as_ref(), 0.0);
        }
    }

    fn draw_sheet(&mut self, gl: &glow::Context, vp: Mat4, sheet: &LayerSheet) {
        unsafe {
            if self.sheet_key != sheet.key
                && sheet.w > 0
                && sheet.h > 0
                && sheet.rgba.len() >= (sheet.w * sheet.h * 4) as usize
            {
                gl.bind_texture(glow::TEXTURE_2D, Some(self.sheet_tex));
                gl.tex_parameter_i32(
                    glow::TEXTURE_2D,
                    glow::TEXTURE_MIN_FILTER,
                    glow::LINEAR as i32,
                );
                gl.tex_parameter_i32(
                    glow::TEXTURE_2D,
                    glow::TEXTURE_MAG_FILTER,
                    glow::LINEAR as i32,
                );
                gl.tex_parameter_i32(
                    glow::TEXTURE_2D,
                    glow::TEXTURE_WRAP_S,
                    glow::CLAMP_TO_EDGE as i32,
                );
                gl.tex_parameter_i32(
                    glow::TEXTURE_2D,
                    glow::TEXTURE_WRAP_T,
                    glow::CLAMP_TO_EDGE as i32,
                );
                gl.tex_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    glow::RGBA as i32,
                    sheet.w,
                    sheet.h,
                    0,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(Some(sheet.rgba.as_slice())),
                );
                self.sheet_key = sheet.key;
            }
            let z = sheet.z + 0.04;
            let sx = sheet.size_x;
            let sy = sheet.size_y;
            let quad = [
                0.0, 0.0, z, 0.0, 0.0, sx, 0.0, z, 1.0, 0.0, sx, sy, z, 1.0, 1.0, 0.0, 0.0, z, 0.0,
                0.0, sx, sy, z, 1.0, 1.0, 0.0, sy, z, 0.0, 1.0,
            ];
            upload_attrib(
                gl,
                self.sheet_vao,
                self.sheet_vbo,
                &quad,
                5,
                &[(0, 3, 0), (1, 2, 3)],
            );
            gl.enable(glow::BLEND);
            gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);
            gl.depth_mask(false);
            gl.use_program(Some(self.sheet_program));
            gl.uniform_matrix_4_f32_slice(self.sheet_mvp.as_ref(), false, vp.as_ref());
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(self.sheet_tex));
            gl.bind_vertex_array(Some(self.sheet_vao));
            gl.draw_arrays(glow::TRIANGLES, 0, 6);
            gl.bind_vertex_array(None);
            gl.depth_mask(true);
            gl.disable(glow::BLEND);
        }
    }

    fn draw_solid(
        &self,
        gl: &glow::Context,
        batches: &[Batch],
        model: Mat4,
        color: [f32; 3],
        use_color: f32,
        alpha: f32,
        vp: Mat4,
    ) {
        unsafe {
            let mvp = vp * model;
            let normal = normal_matrix(model);
            gl.uniform_matrix_4_f32_slice(self.locs.mvp.as_ref(), false, mvp.as_ref());
            gl.uniform_matrix_4_f32_slice(self.locs.model.as_ref(), false, model.as_ref());
            gl.uniform_matrix_4_f32_slice(self.locs.normal.as_ref(), false, normal.as_ref());
            gl.uniform_3_f32(self.locs.color.as_ref(), color[0], color[1], color[2]);
            gl.uniform_1_f32(self.locs.use_color.as_ref(), use_color);
            gl.uniform_1_f32(self.locs.alpha.as_ref(), alpha);
            for batch in batches {
                gl.bind_vertex_array(Some(batch.vao));
                gl.draw_arrays(glow::TRIANGLES, 0, batch.count);
            }
        }
    }

    pub fn destroy(&self, gl: &glow::Context) {
        unsafe {
            gl.delete_program(self.program);
            gl.delete_program(self.line_program);
            gl.delete_program(self.sheet_program);
            gl.delete_texture(self.sheet_tex);
            gl.delete_vertex_array(self.sheet_vao);
            gl.delete_buffer(self.sheet_vbo);
            drop_batches(gl, &self.world.batches);
            drop_batches(gl, &self.bed.batches);
            for obj in &self.objects {
                drop_batches(gl, &obj.batches);
            }
            for raft in &self.rafts {
                drop_batches(gl, &raft.batches);
            }
            drop_batches(gl, &self.ghost.batches);
            drop_batches(gl, &self.cap_quad.batches);
            gl.delete_vertex_array(self.line_vao);
            gl.delete_buffer(self.line_vbo);
        }
    }
}

fn normal_matrix(model: Mat4) -> Mat4 {
    let m = Mat3::from_mat4(model);
    if m.determinant().abs() < 1e-8 {
        return Mat4::IDENTITY;
    }
    Mat4::from_mat3(m.inverse().transpose())
}

const CHUNK_VERTS: usize = 600_000;

fn upload_mesh(gl: &glow::Context, batches: &mut Vec<Batch>, data: &[f32]) {
    let vert_count = data.len() / 9;
    let chunks = if vert_count == 0 {
        0
    } else {
        vert_count.div_ceil(CHUNK_VERTS)
    };
    unsafe {
        while batches.len() > chunks {
            if let Some(batch) = batches.pop() {
                gl.delete_vertex_array(batch.vao);
                gl.delete_buffer(batch.vbo);
            }
        }
        while batches.len() < chunks {
            let Ok(vao) = gl.create_vertex_array() else {
                break;
            };
            let Ok(vbo) = gl.create_buffer() else {
                gl.delete_vertex_array(vao);
                break;
            };
            batches.push(Batch { vao, vbo, count: 0 });
        }
        for (i, batch) in batches.iter_mut().enumerate() {
            let start = i * CHUNK_VERTS;
            let end = ((i + 1) * CHUNK_VERTS).min(vert_count);
            upload_attrib(
                gl,
                batch.vao,
                batch.vbo,
                &data[start * 9..end * 9],
                9,
                &[(0, 3, 0), (1, 3, 3), (2, 3, 6)],
            );
            batch.count = (end - start) as i32;
        }
    }
}

fn drop_batches(gl: &glow::Context, batches: &[Batch]) {
    unsafe {
        for batch in batches {
            gl.delete_vertex_array(batch.vao);
            gl.delete_buffer(batch.vbo);
        }
    }
}

unsafe fn upload_attrib(
    gl: &glow::Context,
    vao: glow::VertexArray,
    vbo: glow::Buffer,
    data: &[f32],
    stride_floats: i32,
    attrs: &[(u32, i32, i32)],
) {
    gl.bind_vertex_array(Some(vao));
    gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
    gl.buffer_data_u8_slice(
        glow::ARRAY_BUFFER,
        std::slice::from_raw_parts(data.as_ptr() as *const u8, data.len() * 4),
        glow::DYNAMIC_DRAW,
    );
    let stride = stride_floats * 4;
    for &(index, size, offset_floats) in attrs {
        gl.enable_vertex_attrib_array(index);
        gl.vertex_attrib_pointer_f32(index, size, glow::FLOAT, false, stride, offset_floats * 4);
    }
}

fn link(gl: &glow::Context, vert: &str, frag: &str) -> Result<glow::Program, String> {
    link_stages(
        gl,
        &[(glow::VERTEX_SHADER, vert), (glow::FRAGMENT_SHADER, frag)],
    )
}

fn link_stages(gl: &glow::Context, stages: &[(u32, &str)]) -> Result<glow::Program, String> {
    unsafe {
        let program = gl.create_program().map_err(|e| e.to_string())?;
        let header = "#version 330\n";
        for &(kind, src) in stages {
            let shader = gl.create_shader(kind).map_err(|e| e.to_string())?;
            gl.shader_source(shader, &format!("{header}{src}"));
            gl.compile_shader(shader);
            if !gl.get_shader_compile_status(shader) {
                let log = gl.get_shader_info_log(shader);
                return Err(format!("shader: {log}"));
            }
            gl.attach_shader(program, shader);
        }
        gl.link_program(program);
        if !gl.get_program_link_status(program) {
            return Err(gl.get_program_info_log(program));
        }
        Ok(program)
    }
}

pub type SharedRenderer = Arc<std::sync::Mutex<Renderer>>;

#[cfg(test)]
mod tests {
    use super::{smooth_world_normals, Camera};
    use glam::{Mat4, Vec3};

    #[test]
    fn orbit_can_drop_below_the_bed() {
        let mut cam = Camera::looking_at_plate(Vec3::new(200.0, 120.0, 200.0));
        assert!(!cam.under_bed());
        cam.pitch = -35.0;
        assert!(cam.under_bed(), "eye {}", cam.eye().z);
    }

    #[test]
    fn a_cube_corner_normal_points_out_of_the_box() {
        let mesh = crate::mesh::box_mesh([0.0, 0.0, 0.0], [10.0, 10.0, 10.0]);
        let normals = smooth_world_normals(&mesh, Mat4::IDENTITY);
        let corner = normals
            .iter()
            .zip(mesh.vertices.iter())
            .find(|(_, v)| v[0] > 9.0 && v[1] > 9.0 && v[2] > 9.0)
            .map(|(n, _)| *n)
            .expect("top corner");
        assert!(corner[0] > 0.4 && corner[1] > 0.4 && corner[2] > 0.4);
    }
}
