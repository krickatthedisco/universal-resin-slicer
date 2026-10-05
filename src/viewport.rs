//! Orbit camera and the OpenGL plate view.

use crate::mesh::{smooth_normals, Mesh};
use crate::scene::{Document, Selection};
use crate::supports;
use glam::{Mat4, Vec3};
use glow::HasContext;
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
        let proj = Mat4::perspective_rh(40.0_f32.to_radians(), aspect.max(0.2), 0.5, 8000.0);
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
    let mut tris = Vec::new();
    let mut lines = Vec::new();
    push_plate(&mut tris, &mut lines, plate);
    let raft_top = doc.raft_top();
    if raft_top > 0.0 {
        if let Some(raft) = supports::raft_mesh(&doc.supports, doc.raft_margin, doc.raft_mm) {
            push_mesh_flat(&mut tris, &raft, [0.45, 0.38, 0.28]);
        }
    }
    for support in &doc.supports {
        let mesh = supports::support_mesh(support, raft_top);
        let color = if selection == Selection::Support(support.id) {
            [0.95, 0.78, 0.35]
        } else {
            [0.25, 0.62, 0.58]
        };
        push_mesh_flat(&mut tris, &mesh, color);
    }
    for brace in doc.brace_list() {
        push_mesh_flat(&mut tris, &supports::brace_mesh(&brace), [0.20, 0.48, 0.50]);
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

fn push_object(tris: &mut Vec<f32>, obj: &crate::scene::Object, color: [f32; 3]) {
    let mat = Document::matrix(obj);
    let normal_mat = mat.inverse().transpose();
    let normals = smooth_normals(&obj.mesh);
    let mut world = Mesh {
        vertices: Vec::with_capacity(obj.mesh.vertices.len()),
        indices: obj.mesh.indices.clone(),
    };
    for (p, n) in obj.mesh.vertices.iter().zip(normals.iter()) {
        let wp = mat.transform_point3(Vec3::from_array(*p));
        let wn = normal_mat
            .transform_vector3(Vec3::from_array(*n))
            .normalize_or_zero();
        world.vertices.push(wp.to_array());
        push_vert(tris, wp.to_array(), wn.to_array(), color);
    }
    // Indices are not used: vertices were expanded per index above? No, I pushed
    // one vert per mesh vertex, but the index buffer is separate. The GPU draw
    // uses a non-indexed expanded list. Rebuild expanded.
    tris.truncate(tris.len() - world.vertices.len() * 9);
    for tri in obj.mesh.indices.chunks_exact(3) {
        for &id in tri {
            let p = world.vertices[id as usize];
            let n = normal_mat
                .transform_vector3(Vec3::from_array(normals[id as usize]))
                .normalize_or_zero();
            push_vert(tris, p, n.to_array(), color);
        }
    }
}

fn push_mesh_flat(tris: &mut Vec<f32>, mesh: &Mesh, color: [f32; 3]) {
    for tri in mesh.indices.chunks_exact(3) {
        let a = mesh.vertices[tri[0] as usize];
        let b = mesh.vertices[tri[1] as usize];
        let c = mesh.vertices[tri[2] as usize];
        let n = crate::mesh::face_normal(a, b, c);
        push_vert(tris, a, n, color);
        push_vert(tris, b, n, color);
        push_vert(tris, c, n, color);
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
    vec3 n = normalize(v_nrm);
    if (!gl_FrontFacing) { n = -n; }
    float ndl = clamp(dot(n, normalize(u_light)), 0.0, 1.0);
    float fill = clamp(dot(n, normalize(u_fill)), 0.0, 1.0);
    float shade = 0.28 + 0.62 * ndl + 0.22 * fill;
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

pub struct Renderer {
    program: glow::Program,
    line_program: glow::Program,
    vao: glow::VertexArray,
    vbo: glow::Buffer,
    line_vao: glow::VertexArray,
    line_vbo: glow::Buffer,
    tri_verts: i32,
    line_verts: i32,
    generation: u64,
}

impl Renderer {
    pub fn new(gl: &glow::Context) -> Result<Self, String> {
        unsafe {
            let program = link(gl, VERT, FRAG)?;
            let line_program = link(gl, LINE_VERT, LINE_FRAG)?;
            let vao = gl.create_vertex_array().map_err(|e| e.to_string())?;
            let vbo = gl.create_buffer().map_err(|e| e.to_string())?;
            let line_vao = gl.create_vertex_array().map_err(|e| e.to_string())?;
            let line_vbo = gl.create_buffer().map_err(|e| e.to_string())?;
            Ok(Self {
                program,
                line_program,
                vao,
                vbo,
                line_vao,
                line_vbo,
                tri_verts: 0,
                line_verts: 0,
                generation: 0,
            })
        }
    }

    pub fn sync(&mut self, gl: &glow::Context, draw: &DrawLists, generation: u64) {
        if generation == self.generation && self.tri_verts > 0 {
            return;
        }
        self.generation = generation;
        unsafe {
            upload_attrib(
                gl,
                self.vao,
                self.vbo,
                &draw.tris,
                9,
                &[(0, 3, 0), (1, 3, 3), (2, 3, 6)],
            );
            upload_attrib(
                gl,
                self.line_vao,
                self.line_vbo,
                &draw.lines,
                6,
                &[(0, 3, 0), (1, 3, 3)],
            );
        }
        self.tri_verts = (draw.tris.len() / 9) as i32;
        self.line_verts = (draw.lines.len() / 6) as i32;
    }

    pub fn paint(&self, gl: &glow::Context, camera: &Camera, aspect: f32) {
        unsafe {
            let vp = camera.view_proj(aspect);
            gl.enable(glow::DEPTH_TEST);
            gl.depth_func(glow::LEQUAL);
            gl.clear_color(0.09, 0.10, 0.12, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT);
            gl.use_program(Some(self.program));
            let loc = gl.get_uniform_location(self.program, "u_mvp");
            gl.uniform_matrix_4_f32_slice(loc.as_ref(), false, vp.as_ref());
            let light = gl.get_uniform_location(self.program, "u_light");
            gl.uniform_3_f32(light.as_ref(), 0.35, -0.25, 0.90);
            let fill = gl.get_uniform_location(self.program, "u_fill");
            gl.uniform_3_f32(fill.as_ref(), -0.4, 0.6, 0.2);
            gl.bind_vertex_array(Some(self.vao));
            gl.draw_arrays(glow::TRIANGLES, 0, self.tri_verts);

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
            gl.delete_vertex_array(self.vao);
            gl.delete_buffer(self.vbo);
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
