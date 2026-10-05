//! Orbit camera and the OpenGL plate view.

use crate::mesh::Mesh;
use crate::scene::{Document, Selection};
use crate::supports;
use glam::{Mat4, Vec3};
use glow::HasContext;
use std::cell::RefCell;
use std::collections::HashMap;
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
        let pitch = self.pitch.clamp(4.0, 89.0).to_radians();
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

    pub fn pan(&mut self, dx_px: f32, dy_px: f32) {
        let eye = self.eye();
        let forward = (self.target - eye).normalize();
        let right = forward.cross(Vec3::Z).normalize_or_zero();
        let up = right.cross(forward).normalize_or_zero();
        let scale = self.distance * 0.0016;
        self.target += right * (-dx_px) * scale + up * dy_px * scale;
    }
}

#[derive(Clone)]
pub struct DrawLists {
    pub tris: Vec<f32>,
    pub lines: Vec<f32>,
}

pub fn build_draw(doc: &Document, plate: Vec3, selection: Selection) -> DrawLists {
    let ids: Vec<u64> = doc.objects.iter().map(|obj| obj.id).collect();
    prune_display(&ids);
    let mut tris = Vec::new();
    let mut lines = Vec::new();
    push_plate(&mut tris, &mut lines, plate);
    let raft_top = doc.raft_top();
    if raft_top > 0.0 {
        if let Some(raft) = supports::raft_mesh(
            &doc.supports,
            doc.raft_margin,
            doc.raft_mm,
            doc.style.trunk_mm,
        ) {
            push_mesh_flat(&mut tris, &raft, [0.45, 0.38, 0.28]);
        }
    }
    let forest = supports::forest_mesh(
        &doc.supports,
        &doc.style,
        raft_top,
        doc.braces_on,
        doc.brace_dist,
    );
    push_mesh_flat(&mut tris, &forest, [0.22, 0.55, 0.52]);
    if let Selection::Support(id) = selection {
        if let Some(support) = doc.supports.iter().find(|s| s.id == id) {
            push_mesh_flat(
                &mut tris,
                &supports::tip_marker(support, &doc.style),
                [0.95, 0.78, 0.35],
            );
        }
    }
    for drain in &doc.drains {
        let mesh = drain_mesh(drain.origin, drain.axis, drain.radius_mm, drain.depth_mm);
        let color = if selection == Selection::Drain(drain.id) {
            [0.95, 0.45, 0.30]
        } else {
            [0.75, 0.28, 0.22]
        };
        push_mesh_flat(&mut tris, &mesh, color);
    }
    for obj in &doc.objects {
        let selected = selection == Selection::Object(obj.id);
        let outside = Document::world_bounds(obj).is_some_and(|(min, max)| {
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
        push_object(&mut tris, obj, color);
        if selected {
            if let Some((min, max)) = Document::world_bounds(obj) {
                push_box_lines(&mut lines, min, max, [0.89, 0.63, 0.18]);
            }
        }
    }
    DrawLists { tris, lines }
}

/// Above this, a sculpt's triangles are smaller than a screen pixel and the
/// rasterizer drops them, which reads as holes. The slice still uses the
/// full mesh. The plate view keeps a welded, coarser copy.
const DISPLAY_TRIANGLES: usize = 280_000;

struct CachedView {
    stamp: u64,
    mesh: Mesh,
}

thread_local! {
    static DISPLAY: RefCell<HashMap<u64, CachedView>> = RefCell::new(HashMap::new());
}

fn prune_display(live: &[u64]) {
    DISPLAY.with(|cache| {
        cache
            .borrow_mut()
            .retain(|id, _| live.iter().any(|live_id| live_id == id));
    });
}

fn mesh_stamp(mesh: &Mesh) -> u64 {
    let mut stamp = ((mesh.vertices.len() as u64) << 32) ^ mesh.indices.len() as u64;
    if let Some(v) = mesh.vertices.first() {
        stamp ^= v[0].to_bits() as u64;
        stamp ^= (v[1].to_bits() as u64) << 1;
        stamp ^= (v[2].to_bits() as u64) << 2;
    }
    if let Some(v) = mesh.vertices.get(mesh.vertices.len() / 2) {
        stamp ^= (v[0].to_bits() as u64).rotate_left(13);
    }
    if let Some(index) = mesh.indices.last() {
        stamp ^= *index as u64;
    }
    stamp
}

fn push_object(tris: &mut Vec<f32>, obj: &crate::scene::Object, color: [f32; 3]) {
    let mat = Document::matrix(obj);
    if obj.mesh.triangle_count() <= DISPLAY_TRIANGLES {
        push_transformed(tris, &obj.mesh, mat, color);
        return;
    }
    let stamp = mesh_stamp(&obj.mesh);
    DISPLAY.with(|cache| {
        let mut cache = cache.borrow_mut();
        let stale = cache
            .get(&obj.id)
            .is_none_or(|cached| cached.stamp != stamp);
        if stale {
            cache.insert(
                obj.id,
                CachedView {
                    stamp,
                    mesh: cluster_for_display(&obj.mesh, DISPLAY_TRIANGLES),
                },
            );
        }
        if let Some(cached) = cache.get(&obj.id) {
            push_transformed(tris, &cached.mesh, mat, color);
        }
    });
}

fn push_transformed(tris: &mut Vec<f32>, mesh: &Mesh, mat: Mat4, color: [f32; 3]) {
    for tri in mesh.indices.chunks_exact(3) {
        let world = [
            mat.transform_point3(Vec3::from_array(mesh.vertices[tri[0] as usize])),
            mat.transform_point3(Vec3::from_array(mesh.vertices[tri[1] as usize])),
            mat.transform_point3(Vec3::from_array(mesh.vertices[tri[2] as usize])),
        ];
        let n = safe_normal(crate::mesh::face_normal(
            world[0].to_array(),
            world[1].to_array(),
            world[2].to_array(),
        ));
        for p in world {
            push_vert(tris, p.to_array(), n, color);
        }
    }
}

fn cluster_for_display(mesh: &Mesh, target: usize) -> Mesh {
    let Some((min, max)) = mesh.bounds() else {
        return Mesh {
            vertices: vec![],
            indices: vec![],
        };
    };
    let tris = mesh.triangle_count().max(1) as f32;
    let ratio = (tris / target as f32).sqrt().max(1.0);
    let dx = max[0] - min[0];
    let dy = max[1] - min[1];
    let dz = max[2] - min[2];
    let diag = (dx * dx + dy * dy + dz * dz).sqrt().max(1.0);
    let cell = ((diag / 160.0) * ratio.sqrt()).clamp(0.04, diag / 6.0);
    let inv = 1.0 / cell;
    let mut map: HashMap<(i32, i32, i32), u32> = HashMap::new();
    let mut vertices = Vec::new();
    let mut acc: Vec<[f32; 4]> = Vec::new();
    let mut remap = vec![0u32; mesh.vertices.len()];
    for (i, v) in mesh.vertices.iter().enumerate() {
        let key = (
            ((v[0] - min[0]) * inv).floor() as i32,
            ((v[1] - min[1]) * inv).floor() as i32,
            ((v[2] - min[2]) * inv).floor() as i32,
        );
        if let Some(&id) = map.get(&key) {
            remap[i] = id;
            let slot = &mut acc[id as usize];
            slot[0] += v[0];
            slot[1] += v[1];
            slot[2] += v[2];
            slot[3] += 1.0;
        } else {
            let id = vertices.len() as u32;
            map.insert(key, id);
            vertices.push(*v);
            acc.push([v[0], v[1], v[2], 1.0]);
            remap[i] = id;
        }
    }
    for (vertex, sum) in vertices.iter_mut().zip(&acc) {
        let n = sum[3].max(1.0);
        *vertex = [sum[0] / n, sum[1] / n, sum[2] / n];
    }
    let mut indices = Vec::new();
    for tri in mesh.indices.chunks_exact(3) {
        let a = remap[tri[0] as usize];
        let b = remap[tri[1] as usize];
        let c = remap[tri[2] as usize];
        if a != b && b != c && c != a {
            indices.extend_from_slice(&[a, b, c]);
        }
    }
    Mesh { vertices, indices }
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

fn push_vert(buf: &mut Vec<f32>, p: [f32; 3], n: [f32; 3], c: [f32; 3]) {
    buf.extend_from_slice(&[p[0], p[1], p[2], n[0], n[1], n[2], c[0], c[1], c[2]]);
}

fn push_plate(tris: &mut Vec<f32>, lines: &mut Vec<f32>, plate: Vec3) {
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

fn drain_mesh(origin: Vec3, axis: Vec3, radius: f32, depth: f32) -> Mesh {
    let axis = axis.normalize_or_zero();
    let start = origin - axis * 0.6;
    let end = origin + axis * depth;
    // Reuse the support taper by going through supports::brace_mesh shape.
    supports::brace_mesh(&supports::Brace {
        a: start,
        b: end,
        radius,
    })
}

const VERT: &str = r#"
layout(location = 0) in vec3 a_pos;
layout(location = 1) in vec3 a_nrm;
layout(location = 2) in vec3 a_col;
uniform mat4 u_mvp;
out vec3 v_nrm;
out vec3 v_col;
void main() {
    v_nrm = a_nrm;
    v_col = a_col;
    gl_Position = u_mvp * vec4(a_pos, 1.0);
}
"#;

const FRAG: &str = r#"
in vec3 v_nrm;
in vec3 v_col;
out vec4 out_color;
uniform vec3 u_light;
uniform vec3 u_fill;
void main() {
    float len2 = dot(v_nrm, v_nrm);
    vec3 n = len2 > 1e-8 ? normalize(v_nrm) : vec3(0.0, 0.0, 1.0);
    if (!gl_FrontFacing) { n = -n; }
    float ndl = clamp(dot(n, normalize(u_light)), 0.0, 1.0);
    float fill = clamp(dot(n, normalize(u_fill)), 0.0, 1.0);
    float shade = 0.62 + 0.34 * ndl + 0.10 * fill;
    out_color = vec4(v_col * shade, 1.0);
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

struct Batch {
    vao: glow::VertexArray,
    vbo: glow::Buffer,
    count: i32,
}

pub struct Renderer {
    program: glow::Program,
    line_program: glow::Program,
    batches: Vec<Batch>,
    line_vao: glow::VertexArray,
    line_vbo: glow::Buffer,
    line_verts: i32,
    generation: u64,
}

impl Renderer {
    pub fn new(gl: &glow::Context) -> Result<Self, String> {
        unsafe {
            let program = link(gl, VERT, FRAG)?;
            let line_program = link(gl, LINE_VERT, LINE_FRAG)?;
            let line_vao = gl.create_vertex_array().map_err(|e| e.to_string())?;
            let line_vbo = gl.create_buffer().map_err(|e| e.to_string())?;
            Ok(Self {
                program,
                line_program,
                batches: Vec::new(),
                line_vao,
                line_vbo,
                line_verts: 0,
                generation: 0,
            })
        }
    }

    pub fn sync(&mut self, gl: &glow::Context, draw: &DrawLists, generation: u64) {
        if generation == self.generation && !self.batches.is_empty() {
            return;
        }
        self.generation = generation;
        // One huge buffer fails on some Windows drivers and the mesh comes
        // back with holes. Keep each upload under about 25 MB.
        const CHUNK_VERTS: usize = 600_000;
        let vert_count = draw.tris.len() / 9;
        let chunks = if vert_count == 0 {
            0
        } else {
            vert_count.div_ceil(CHUNK_VERTS)
        };
        unsafe {
            while self.batches.len() > chunks {
                if let Some(batch) = self.batches.pop() {
                    gl.delete_vertex_array(batch.vao);
                    gl.delete_buffer(batch.vbo);
                }
            }
            while self.batches.len() < chunks {
                let Ok(vao) = gl.create_vertex_array() else {
                    break;
                };
                let Ok(vbo) = gl.create_buffer() else {
                    gl.delete_vertex_array(vao);
                    break;
                };
                self.batches.push(Batch { vao, vbo, count: 0 });
            }
            for (i, batch) in self.batches.iter_mut().enumerate() {
                let start = i * CHUNK_VERTS;
                let end = ((i + 1) * CHUNK_VERTS).min(vert_count);
                upload_attrib(
                    gl,
                    batch.vao,
                    batch.vbo,
                    &draw.tris[start * 9..end * 9],
                    9,
                    &[(0, 3, 0), (1, 3, 3), (2, 3, 6)],
                );
                batch.count = (end - start) as i32;
            }
            upload_attrib(
                gl,
                self.line_vao,
                self.line_vbo,
                &draw.lines,
                6,
                &[(0, 3, 0), (1, 3, 3)],
            );
        }
        self.line_verts = (draw.lines.len() / 6) as i32;
    }

    pub fn paint(&self, gl: &glow::Context, camera: &Camera, aspect: f32) {
        unsafe {
            let vp = camera.view_proj(aspect);
            gl.disable(glow::CULL_FACE);
            gl.disable(glow::BLEND);
            gl.enable(glow::DEPTH_TEST);
            gl.depth_func(glow::LEQUAL);
            gl.depth_mask(true);
            gl.enable(glow::POLYGON_OFFSET_FILL);
            gl.polygon_offset(1.0, 1.0);
            gl.front_face(glow::CCW);
            gl.clear_color(0.11, 0.12, 0.14, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT);
            gl.use_program(Some(self.program));
            let loc = gl.get_uniform_location(self.program, "u_mvp");
            gl.uniform_matrix_4_f32_slice(loc.as_ref(), false, vp.as_ref());
            let light = gl.get_uniform_location(self.program, "u_light");
            gl.uniform_3_f32(light.as_ref(), 0.35, -0.25, 0.90);
            let fill = gl.get_uniform_location(self.program, "u_fill");
            gl.uniform_3_f32(fill.as_ref(), -0.4, 0.6, 0.2);
            for batch in &self.batches {
                gl.bind_vertex_array(Some(batch.vao));
                gl.draw_arrays(glow::TRIANGLES, 0, batch.count);
            }
            gl.disable(glow::POLYGON_OFFSET_FILL);

            gl.use_program(Some(self.line_program));
            let loc = gl.get_uniform_location(self.line_program, "u_mvp");
            gl.uniform_matrix_4_f32_slice(loc.as_ref(), false, vp.as_ref());
            gl.bind_vertex_array(Some(self.line_vao));
            gl.draw_arrays(glow::LINES, 0, self.line_verts);
            gl.bind_vertex_array(None);
            gl.disable(glow::DEPTH_TEST);
        }
    }

    pub fn destroy(&self, gl: &glow::Context) {
        unsafe {
            gl.delete_program(self.program);
            gl.delete_program(self.line_program);
            for batch in &self.batches {
                gl.delete_vertex_array(batch.vao);
                gl.delete_buffer(batch.vbo);
            }
            gl.delete_vertex_array(self.line_vao);
            gl.delete_buffer(self.line_vbo);
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
    unsafe {
        let program = gl.create_program().map_err(|e| e.to_string())?;
        let header = "#version 330\n";
        for (kind, src) in [(glow::VERTEX_SHADER, vert), (glow::FRAGMENT_SHADER, frag)] {
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
