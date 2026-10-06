//! Triangle meshes, STL/OBJ import, and a few built-in calibration shapes.

use anyhow::{anyhow, bail, Result};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

#[derive(Clone, Debug)]
pub struct Mesh {
    pub vertices: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
}

impl Mesh {
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    pub fn bounds(&self) -> Option<([f32; 3], [f32; 3])> {
        let mut iter = self.vertices.iter();
        let first = *iter.next()?;
        let mut min = first;
        let mut max = first;
        for v in iter {
            for a in 0..3 {
                min[a] = min[a].min(v[a]);
                max[a] = max[a].max(v[a]);
            }
        }
        Some((min, max))
    }

    pub fn signed_volume_mm3(&self) -> f32 {
        let mut acc = 0.0f32;
        for tri in self.indices.chunks_exact(3) {
            let a = self.vertices[tri[0] as usize];
            let b = self.vertices[tri[1] as usize];
            let c = self.vertices[tri[2] as usize];
            acc += a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
                + a[2] * (b[0] * c[1] - b[1] * c[0]);
        }
        acc / 6.0
    }

    pub fn volume_mm3(&self) -> f32 {
        self.signed_volume_mm3().abs()
    }

    /// Flip the whole shell if it is inside out, then weld duplicate corners.
    pub fn repair(&mut self) {
        if self.signed_volume_mm3() < 0.0 {
            self.flip_winding();
        }
        self.weld(1e-4);
    }

    pub fn flip_winding(&mut self) {
        for tri in self.indices.chunks_exact_mut(3) {
            tri.swap(1, 2);
        }
    }

    pub fn weld(&mut self, tolerance_mm: f32) {
        let inv = 1.0 / tolerance_mm.max(1e-6);
        let mut map: HashMap<(i32, i32, i32), u32> = HashMap::new();
        let mut new_verts = Vec::new();
        let mut remap = vec![0u32; self.vertices.len()];
        for (i, v) in self.vertices.iter().enumerate() {
            let key = (
                (v[0] * inv).round() as i32,
                (v[1] * inv).round() as i32,
                (v[2] * inv).round() as i32,
            );
            if let Some(id) = map.get(&key) {
                remap[i] = *id;
            } else {
                let id = new_verts.len() as u32;
                map.insert(key, id);
                new_verts.push(*v);
                remap[i] = id;
            }
        }
        self.vertices = new_verts;
        let mut tris = Vec::with_capacity(self.indices.len());
        for tri in self.indices.chunks_exact(3) {
            let a = remap[tri[0] as usize];
            let b = remap[tri[1] as usize];
            let c = remap[tri[2] as usize];
            if a != b && b != c && a != c {
                tris.extend_from_slice(&[a, b, c]);
            }
        }
        self.indices = tris;
    }

    pub fn transformed(&self, xf: impl Fn([f32; 3]) -> [f32; 3]) -> Mesh {
        Mesh {
            vertices: self.vertices.iter().copied().map(xf).collect(),
            indices: self.indices.clone(),
        }
    }

    pub fn append(&mut self, other: &Mesh) {
        let base = self.vertices.len() as u32;
        self.vertices.extend_from_slice(&other.vertices);
        self.indices.extend(other.indices.iter().map(|i| i + base));
    }

    /// Cut on the horizontal plane `z` and close both pieces across the cut.
    pub fn split_at_z(&self, z: f32) -> (Mesh, Mesh) {
        split_mesh_at_z(self, z)
    }
}

const CUT_EPS: f32 = 1e-4;

fn split_mesh_at_z(mesh: &Mesh, z: f32) -> (Mesh, Mesh) {
    let mut below = Weld::new();
    let mut above = Weld::new();
    let mut cap_edges: Vec<([f32; 3], [f32; 3])> = Vec::new();
    for tri in mesh.indices.chunks_exact(3) {
        let a = mesh.vertices[tri[0] as usize];
        let b = mesh.vertices[tri[1] as usize];
        let c = mesh.vertices[tri[2] as usize];
        if on_plane(a, z) && on_plane(b, z) && on_plane(c, z) {
            if face_normal(a, b, c)[2] >= 0.0 {
                below.add_tri(a, b, c);
            } else {
                above.add_tri(a, b, c);
            }
            continue;
        }
        let (low, edge) = clip_side(&[a, b, c], z, true);
        if low.len() >= 3 {
            below.add_poly(&low);
        }
        if let Some(edge) = edge {
            cap_edges.push(edge);
        }
        let (high, _) = clip_side(&[a, b, c], z, false);
        if high.len() >= 3 {
            above.add_poly(&high);
        }
    }
    let cap = cap_from_edges(&cap_edges, true);
    let mut cap_down = cap.clone();
    cap_down.flip_winding();
    below.mesh.append(&cap);
    above.mesh.append(&cap_down);
    (below.mesh, above.mesh)
}

fn on_plane(p: [f32; 3], z: f32) -> bool {
    (p[2] - z).abs() <= CUT_EPS
}

fn inside_side(p: [f32; 3], z: f32, keep_below: bool) -> bool {
    if keep_below {
        p[2] <= z + CUT_EPS
    } else {
        p[2] >= z - CUT_EPS
    }
}

fn plane_point(a: [f32; 3], b: [f32; 3], z: f32) -> [f32; 3] {
    let denom = b[2] - a[2];
    let t = if denom.abs() < 1e-12 {
        0.0
    } else {
        ((z - a[2]) / denom).clamp(0.0, 1.0)
    };
    [a[0] + t * (b[0] - a[0]), a[1] + t * (b[1] - a[1]), z]
}

/// Polygon kept on one side of the plane, plus the cut edge in boundary order.
fn clip_side(
    poly: &[[f32; 3]],
    z: f32,
    keep_below: bool,
) -> (Vec<[f32; 3]>, Option<([f32; 3], [f32; 3])>) {
    if poly.is_empty() {
        return (Vec::new(), None);
    }
    let mut out = Vec::new();
    let mut prev = poly[poly.len() - 1];
    let mut prev_in = inside_side(prev, z, keep_below);
    for &curr in poly {
        let curr_in = inside_side(curr, z, keep_below);
        if curr_in != prev_in {
            out.push(plane_point(prev, curr, z));
        }
        if curr_in {
            out.push(curr);
        }
        prev = curr;
        prev_in = curr_in;
    }
    let mut edge = None;
    if out.len() >= 2 {
        for i in 0..out.len() {
            let p = out[i];
            let q = out[(i + 1) % out.len()];
            if on_plane(p, z) && on_plane(q, z) && (p[0] - q[0]).abs() + (p[1] - q[1]).abs() > 1e-4
            {
                edge = Some((p, q));
                break;
            }
        }
    }
    (out, edge)
}

struct Weld {
    map: HashMap<(i32, i32, i32), u32>,
    mesh: Mesh,
}

impl Weld {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
            mesh: Mesh {
                vertices: Vec::new(),
                indices: Vec::new(),
            },
        }
    }

    fn add_poly(&mut self, poly: &[[f32; 3]]) {
        if poly.len() < 3 {
            return;
        }
        for i in 1..poly.len() - 1 {
            self.add_tri(poly[0], poly[i], poly[i + 1]);
        }
    }

    fn add_tri(&mut self, a: [f32; 3], b: [f32; 3], c: [f32; 3]) {
        let ia = self.vert(a);
        let ib = self.vert(b);
        let ic = self.vert(c);
        if ia != ib && ib != ic && ia != ic {
            self.mesh.indices.extend_from_slice(&[ia, ib, ic]);
        }
    }

    fn vert(&mut self, p: [f32; 3]) -> u32 {
        let key = (
            (p[0] * 1000.0).round() as i32,
            (p[1] * 1000.0).round() as i32,
            (p[2] * 1000.0).round() as i32,
        );
        if let Some(id) = self.map.get(&key) {
            return *id;
        }
        let id = self.mesh.vertices.len() as u32;
        self.map.insert(key, id);
        self.mesh.vertices.push(p);
        id
    }
}

fn cap_from_edges(segs: &[([f32; 3], [f32; 3])], normal_up: bool) -> Mesh {
    let loops = stitch_loops(segs);
    let mut mesh = Mesh {
        vertices: Vec::new(),
        indices: Vec::new(),
    };
    for mut loop_pts in loops {
        if loop_pts.len() < 3 {
            continue;
        }
        let area = signed_area_xy(&loop_pts);
        let want_positive = normal_up;
        if (area >= 0.0) != want_positive {
            loop_pts.reverse();
        }
        let flat: Vec<[f32; 2]> = loop_pts.iter().map(|p| [p[0], p[1]]).collect();
        let tris = ear_clip(&flat);
        let base = mesh.vertices.len() as u32;
        mesh.vertices.extend(loop_pts);
        for (a, b, c) in tris {
            mesh.indices
                .extend_from_slice(&[base + a as u32, base + b as u32, base + c as u32]);
        }
    }
    mesh
}

fn signed_area_xy(poly: &[[f32; 3]]) -> f32 {
    let mut area = 0.0f32;
    for i in 0..poly.len() {
        let p = poly[i];
        let q = poly[(i + 1) % poly.len()];
        area += p[0] * q[1] - q[0] * p[1];
    }
    area * 0.5
}

fn stitch_loops(segs: &[([f32; 3], [f32; 3])]) -> Vec<Vec<[f32; 3]>> {
    let key = |p: [f32; 3]| {
        (
            (p[0] * 1000.0).round() as i32,
            (p[1] * 1000.0).round() as i32,
        )
    };
    struct Seg {
        a: [f32; 3],
        b: [f32; 3],
        used: bool,
    }
    let mut segs: Vec<Seg> = segs
        .iter()
        .copied()
        .map(|(a, b)| Seg { a, b, used: false })
        .collect();
    let mut map: HashMap<(i32, i32), Vec<usize>> = HashMap::new();
    for (i, seg) in segs.iter().enumerate() {
        map.entry(key(seg.a)).or_default().push(i);
    }
    let mut loops = Vec::new();
    for i in 0..segs.len() {
        if segs[i].used {
            continue;
        }
        let start = segs[i].a;
        let mut current = segs[i].b;
        segs[i].used = true;
        let mut pts = vec![start];
        if key(current) != key(start) {
            pts.push(current);
        }
        let mut guard = 0;
        while key(current) != key(start) && guard < segs.len() + 2 {
            guard += 1;
            let Some(cands) = map.get(&key(current)) else {
                break;
            };
            let Some(j) = cands.iter().copied().find(|&j| !segs[j].used) else {
                break;
            };
            segs[j].used = true;
            current = segs[j].b;
            if key(current) != key(start) {
                pts.push(current);
            }
        }
        if pts.len() >= 3 && key(current) == key(start) {
            loops.push(pts);
        }
    }
    loops
}

fn ear_clip(poly: &[[f32; 2]]) -> Vec<(usize, usize, usize)> {
    let n = poly.len();
    if n < 3 {
        return Vec::new();
    }
    if n == 3 {
        return vec![(0, 1, 2)];
    }
    let mut left: Vec<usize> = (0..n).collect();
    let ccw = signed_area_flat(poly) >= 0.0;
    let mut tris = Vec::new();
    let mut guard = 0;
    while left.len() > 3 && guard < n * n {
        guard += 1;
        let m = left.len();
        let mut clipped = false;
        for i in 0..m {
            let i0 = left[(i + m - 1) % m];
            let i1 = left[i];
            let i2 = left[(i + 1) % m];
            if is_ear(poly, &left, i0, i1, i2, ccw) {
                tris.push((i0, i1, i2));
                left.remove(i);
                clipped = true;
                break;
            }
        }
        if !clipped {
            break;
        }
    }
    if left.len() >= 3 {
        for i in 1..left.len() - 1 {
            tris.push((left[0], left[i], left[i + 1]));
        }
    }
    tris
}

fn signed_area_flat(poly: &[[f32; 2]]) -> f32 {
    let mut area = 0.0f32;
    for i in 0..poly.len() {
        let p = poly[i];
        let q = poly[(i + 1) % poly.len()];
        area += p[0] * q[1] - q[0] * p[1];
    }
    area * 0.5
}

fn is_ear(poly: &[[f32; 2]], left: &[usize], i0: usize, i1: usize, i2: usize, ccw: bool) -> bool {
    let a = poly[i0];
    let b = poly[i1];
    let c = poly[i2];
    let cross = (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]);
    if ccw && cross <= 1e-6 {
        return false;
    }
    if !ccw && cross >= -1e-6 {
        return false;
    }
    for &i in left {
        if i == i0 || i == i1 || i == i2 {
            continue;
        }
        if point_in_tri(poly[i], a, b, c) {
            return false;
        }
    }
    true
}

fn point_in_tri(p: [f32; 2], a: [f32; 2], b: [f32; 2], c: [f32; 2]) -> bool {
    let sign = |p: [f32; 2], q: [f32; 2], r: [f32; 2]| {
        (p[0] - r[0]) * (q[1] - r[1]) - (q[0] - r[0]) * (p[1] - r[1])
    };
    let d1 = sign(p, a, b);
    let d2 = sign(p, b, c);
    let d3 = sign(p, c, a);
    let neg = d1 < -1e-6 || d2 < -1e-6 || d3 < -1e-6;
    let pos = d1 > 1e-6 || d2 > 1e-6 || d3 > 1e-6;
    !(neg && pos)
}

pub fn load_mesh(path: &Path) -> Result<Mesh> {
    let mut meshes = load_meshes(path)?;
    if meshes.is_empty() {
        bail!("That file has no triangles.");
    }
    if meshes.len() == 1 {
        return Ok(meshes.remove(0).1);
    }
    let mut mesh = Mesh {
        vertices: Vec::new(),
        indices: Vec::new(),
    };
    for (_, part) in meshes {
        mesh.append(&part);
    }
    Ok(mesh)
}

pub fn load_meshes(path: &Path) -> Result<Vec<(String, Mesh)>> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let mut meshes = match ext.as_str() {
        "stl" => vec![("model".into(), load_stl(path)?)],
        "obj" => vec![("model".into(), load_obj(path)?)],
        "3mf" => load_3mf(path)?,
        _ => bail!("Amber imports STL, OBJ, and 3MF. `{ext}` is not one of those."),
    };
    meshes.retain(|(_, mesh)| mesh.triangle_count() > 0);
    if meshes.is_empty() {
        bail!("That file has no triangles.");
    }
    for (_, mesh) in &mut meshes {
        mesh.weld(1e-4);
    }
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("model");
    for (name, _) in &mut meshes {
        if name == "model" || name.is_empty() {
            *name = stem.to_string();
        }
    }
    Ok(meshes)
}

fn load_stl(path: &Path) -> Result<Mesh> {
    let bytes = std::fs::read(path)?;
    if bytes.len() >= 84 {
        let count = u32::from_le_bytes(bytes[80..84].try_into().unwrap());
        let expected = 84usize.saturating_add(count as usize * 50);
        if count > 0 && expected == bytes.len() {
            return parse_stl_binary(&bytes, count);
        }
    }
    load_stl_ascii(path)
}

fn parse_stl_binary(bytes: &[u8], count: u32) -> Result<Mesh> {
    let mut vertices = Vec::with_capacity(count as usize * 3);
    let mut indices = Vec::with_capacity(count as usize * 3);
    for i in 0..count as usize {
        let face = &bytes[84 + i * 50..84 + (i + 1) * 50];
        for v in 0..3 {
            let o = 12 + v * 12;
            let x = f32::from_le_bytes(face[o..o + 4].try_into().unwrap());
            let y = f32::from_le_bytes(face[o + 4..o + 8].try_into().unwrap());
            let z = f32::from_le_bytes(face[o + 8..o + 12].try_into().unwrap());
            if !x.is_finite() || !y.is_finite() || !z.is_finite() {
                bail!("STL contains a non-finite vertex.");
            }
            let id = vertices.len() as u32;
            vertices.push([x, y, z]);
            indices.push(id);
        }
    }
    Ok(Mesh { vertices, indices })
}

fn load_stl_ascii(path: &Path) -> Result<Mesh> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    let mut facet = Vec::<[f32; 3]>::new();
    for line in reader.lines() {
        let line = line?;
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("vertex") {
            let mut nums = rest.split_whitespace();
            let x: f32 = nums
                .next()
                .ok_or_else(|| anyhow!("short vertex"))?
                .parse()?;
            let y: f32 = nums
                .next()
                .ok_or_else(|| anyhow!("short vertex"))?
                .parse()?;
            let z: f32 = nums
                .next()
                .ok_or_else(|| anyhow!("short vertex"))?
                .parse()?;
            facet.push([x, y, z]);
            if facet.len() == 3 {
                for v in facet.drain(..) {
                    let id = vertices.len() as u32;
                    vertices.push(v);
                    indices.push(id);
                }
            }
        }
    }
    if indices.is_empty() {
        bail!("Could not read an ASCII or binary STL from that file.");
    }
    Ok(Mesh { vertices, indices })
}

fn load_obj(path: &Path) -> Result<Mesh> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    for line in reader.lines() {
        let line = line?;
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("v ") {
            let mut nums = rest.split_whitespace();
            let x: f32 = nums.next().ok_or_else(|| anyhow!("short v"))?.parse()?;
            let y: f32 = nums.next().ok_or_else(|| anyhow!("short v"))?.parse()?;
            let z: f32 = nums.next().ok_or_else(|| anyhow!("short v"))?.parse()?;
            vertices.push([x, y, z]);
        } else if let Some(rest) = line.strip_prefix("f ") {
            let corners: Vec<u32> = rest
                .split_whitespace()
                .filter_map(|tok| tok.split('/').next())
                .map(|s| s.parse::<i64>())
                .collect::<std::result::Result<Vec<_>, _>>()?
                .into_iter()
                .map(|i| {
                    if i > 0 {
                        (i as u32) - 1
                    } else {
                        (vertices.len() as i64 + i) as u32
                    }
                })
                .collect();
            if corners.len() >= 3 {
                for k in 1..corners.len() - 1 {
                    indices.extend_from_slice(&[corners[0], corners[k], corners[k + 1]]);
                }
            }
        }
    }
    if indices.is_empty() {
        bail!("OBJ file has no faces.");
    }
    Ok(Mesh { vertices, indices })
}

fn load_3mf(path: &Path) -> Result<Vec<(String, Mesh)>> {
    let file = File::open(path)?;
    let mut archive = zip::ZipArchive::new(file).map_err(|err| anyhow!("3MF zip: {err}"))?;
    let mut xml = String::new();
    let mut found = false;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|err| anyhow!("3MF entry: {err}"))?;
        let name = entry.name().to_string();
        if name.ends_with(".model") {
            entry
                .read_to_string(&mut xml)
                .map_err(|err| anyhow!("3MF read: {err}"))?;
            found = true;
            break;
        }
    }
    if !found {
        bail!("3MF archive has no model part.");
    }
    let scale = unit_scale(&xml);
    let mut meshes = Vec::new();
    for (index, object) in xml.split("<object").skip(1).enumerate() {
        let body = object.split("</object").next().unwrap_or(object);
        let name = attr_str(body, "name").unwrap_or_else(|| format!("part {}", index + 1));
        let mut vertices = Vec::new();
        for tag in body.split("<vertex").skip(1) {
            let tag = tag.split('>').next().unwrap_or(tag);
            let Some(x) = attr_f32(tag, "x") else {
                continue;
            };
            let Some(y) = attr_f32(tag, "y") else {
                continue;
            };
            let Some(z) = attr_f32(tag, "z") else {
                continue;
            };
            vertices.push([x * scale, y * scale, z * scale]);
        }
        let mut indices = Vec::new();
        for tag in body.split("<triangle").skip(1) {
            let tag = tag.split('>').next().unwrap_or(tag);
            let Some(a) = attr_f32(tag, "v1") else {
                continue;
            };
            let Some(b) = attr_f32(tag, "v2") else {
                continue;
            };
            let Some(c) = attr_f32(tag, "v3") else {
                continue;
            };
            indices.extend_from_slice(&[a as u32, b as u32, c as u32]);
        }
        if !indices.is_empty() {
            meshes.push((name, Mesh { vertices, indices }));
        }
    }
    if meshes.is_empty() {
        bail!("3MF model has no triangles.");
    }
    Ok(meshes)
}

fn unit_scale(xml: &str) -> f32 {
    let unit = attr_str(xml, "unit").unwrap_or_else(|| "millimeter".into());
    match unit.to_ascii_lowercase().as_str() {
        "inch" => 25.4,
        "foot" => 304.8,
        "meter" => 1000.0,
        "micron" => 0.001,
        "centimeter" => 10.0,
        _ => 1.0,
    }
}

fn attr_str(tag: &str, key: &str) -> Option<String> {
    let needle = format!("{key}=\"");
    let rest = tag.split(&needle).nth(1)?;
    let value = rest.split('"').next()?;
    Some(value.to_string())
}

fn attr_f32(tag: &str, key: &str) -> Option<f32> {
    attr_str(tag, key)?.parse().ok()
}

pub fn box_mesh(min: [f32; 3], max: [f32; 3]) -> Mesh {
    let [x0, y0, z0] = min;
    let [x1, y1, z1] = max;
    let v = [
        [x0, y0, z0],
        [x1, y0, z0],
        [x1, y1, z0],
        [x0, y1, z0],
        [x0, y0, z1],
        [x1, y0, z1],
        [x1, y1, z1],
        [x0, y1, z1],
    ];
    let quads: [[u32; 4]; 6] = [
        [0, 1, 2, 3], // bottom -Z (outward is down, so 0,2,1? )
        [4, 7, 6, 5], // top +Z
        [0, 4, 5, 1], // -Y
        [3, 2, 6, 7], // +Y
        [0, 3, 7, 4], // -X
        [1, 5, 6, 2], // +X
    ];
    // Fix bottom to wind outward (down): looking from below, CCW.
    // From outside below, x right y up in that view is messy. Check volume sign later.
    let mut indices = Vec::new();
    for q in quads {
        indices.extend_from_slice(&[q[0], q[1], q[2], q[0], q[2], q[3]]);
    }
    let mut mesh = Mesh {
        vertices: v.to_vec(),
        indices,
    };
    if signed_volume(&mesh) < 0.0 {
        mesh.flip_winding();
    }
    mesh
}

fn signed_volume(mesh: &Mesh) -> f32 {
    let mut acc = 0.0f32;
    for tri in mesh.indices.chunks_exact(3) {
        let a = mesh.vertices[tri[0] as usize];
        let b = mesh.vertices[tri[1] as usize];
        let c = mesh.vertices[tri[2] as usize];
        acc += a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
            + a[2] * (b[0] * c[1] - b[1] * c[0]);
    }
    acc / 6.0
}

/// 20 mm cube sitting on the bed, centered on the plate.
pub fn calibration_cube(plate_x: f32, plate_y: f32) -> Mesh {
    let s = 20.0;
    box_mesh(
        [(plate_x - s) * 0.5, (plate_y - s) * 0.5, 0.0],
        [(plate_x + s) * 0.5, (plate_y + s) * 0.5, s],
    )
}

/// Two pillars and a bar. The middle of the bar is a classic overhang.
pub fn overhang_bridge(plate_x: f32, plate_y: f32) -> Mesh {
    let mut mesh = Mesh {
        vertices: vec![],
        indices: vec![],
    };
    let cx = plate_x * 0.5;
    let cy = plate_y * 0.5;
    let base = box_mesh([cx - 18.0, cy - 8.0, 0.0], [cx + 18.0, cy + 8.0, 2.0]);
    let left = box_mesh([cx - 16.0, cy - 3.0, 2.0], [cx - 10.0, cy + 3.0, 16.0]);
    let right = box_mesh([cx + 10.0, cy - 3.0, 2.0], [cx + 16.0, cy + 3.0, 16.0]);
    let bar = box_mesh([cx - 16.0, cy - 2.0, 16.0], [cx + 16.0, cy + 2.0, 20.0]);
    mesh.append(&base);
    mesh.append(&left);
    mesh.append(&right);
    mesh.append(&bar);
    mesh
}

pub fn face_normal(a: [f32; 3], b: [f32; 3], c: [f32; 3]) -> [f32; 3] {
    let ux = b[0] - a[0];
    let uy = b[1] - a[1];
    let uz = b[2] - a[2];
    let vx = c[0] - a[0];
    let vy = c[1] - a[1];
    let vz = c[2] - a[2];
    let n = [uy * vz - uz * vy, uz * vx - ux * vz, ux * vy - uy * vx];
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    if len < 1e-12 {
        [0.0, 0.0, 1.0]
    } else {
        [n[0] / len, n[1] / len, n[2] / len]
    }
}

pub fn smooth_normals(mesh: &Mesh) -> Vec<[f32; 3]> {
    let mut acc = vec![[0.0f32; 3]; mesh.vertices.len()];
    for tri in mesh.indices.chunks_exact(3) {
        let a = mesh.vertices[tri[0] as usize];
        let b = mesh.vertices[tri[1] as usize];
        let c = mesh.vertices[tri[2] as usize];
        let n = face_normal(a, b, c);
        for id in tri {
            let v = &mut acc[*id as usize];
            v[0] += n[0];
            v[1] += n[1];
            v[2] += n[2];
        }
    }
    for n in &mut acc {
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        if len > 1e-8 {
            n[0] /= len;
            n[1] /= len;
            n[2] /= len;
        } else {
            *n = [0.0, 0.0, 1.0];
        }
    }
    acc
}

/// The empty inside of a hollow model: the surface moved in by `inset`,
/// closed flat at `z_lo` and `z_hi`, and wound so it subtracts from the shell.
///
/// The cut view draws this with the outside. The stencil cap then fills only
/// the wall, and the opening shows the cavity. Top and bottom caps stay solid
/// because the void stops at those planes.
pub fn cavity_shell(mesh: &Mesh, inset: f32, z_lo: f32, z_hi: f32) -> Option<Mesh> {
    if inset < 0.05 || z_hi < z_lo + 0.05 || mesh.indices.len() < 3 {
        return None;
    }
    let inner = inset_along_normals(mesh, inset)?;
    if inner.signed_volume_mm3() < 1.0 {
        return None;
    }
    let (below, _) = inner.split_at_z(z_hi);
    if below.triangle_count() == 0 {
        return None;
    }
    let (_, mut cavity) = below.split_at_z(z_lo);
    if cavity.signed_volume_mm3() < 1.0 {
        return None;
    }
    cavity.flip_winding();
    Some(cavity)
}

fn inset_along_normals(mesh: &Mesh, distance: f32) -> Option<Mesh> {
    let count = mesh.vertices.len();
    if count == 0 {
        return None;
    }
    let mut acc = vec![[0.0f32; 3]; count];
    for tri in mesh.indices.chunks_exact(3) {
        let a = mesh.vertices[tri[0] as usize];
        let b = mesh.vertices[tri[1] as usize];
        let c = mesh.vertices[tri[2] as usize];
        let ux = b[0] - a[0];
        let uy = b[1] - a[1];
        let uz = b[2] - a[2];
        let vx = c[0] - a[0];
        let vy = c[1] - a[1];
        let vz = c[2] - a[2];
        let cross = [uy * vz - uz * vy, uz * vx - ux * vz, ux * vy - uy * vx];
        for &index in tri {
            let slot = &mut acc[index as usize];
            slot[0] += cross[0];
            slot[1] += cross[1];
            slot[2] += cross[2];
        }
    }
    let mut vertices = Vec::with_capacity(count);
    for (point, normal) in mesh.vertices.iter().zip(acc.iter()) {
        let len = (normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2]).sqrt();
        if len < 1e-8 {
            vertices.push(*point);
            continue;
        }
        let scale = distance / len;
        vertices.push([
            point[0] - normal[0] * scale,
            point[1] - normal[1] * scale,
            point[2] - normal[2] * scale,
        ]);
    }
    Some(Mesh {
        vertices,
        indices: mesh.indices.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cube_volume_is_one_millilitre_per_centimetre() {
        let mesh = box_mesh([0.0, 0.0, 0.0], [10.0, 10.0, 10.0]);
        let ml = mesh.volume_mm3() / 1000.0;
        assert!((ml - 1.0).abs() < 1e-3, "ml {ml}");
        assert_eq!(mesh.triangle_count(), 12);
    }

    #[test]
    fn a_box_cut_in_half_keeps_both_volumes() {
        let mesh = box_mesh([0.0, 0.0, 0.0], [10.0, 20.0, 8.0]);
        let (below, above) = mesh.split_at_z(3.0);
        let whole = mesh.signed_volume_mm3();
        let low = below.signed_volume_mm3();
        let high = above.signed_volume_mm3();
        assert!(low > 0.0, "lower piece inside out: {low}");
        assert!(high > 0.0, "upper piece inside out: {high}");
        assert!((low - 10.0 * 20.0 * 3.0).abs() < 2.0, "lower {low}");
        assert!((high - 10.0 * 20.0 * 5.0).abs() < 2.0, "upper {high}");
        assert!(
            (low + high - whole).abs() < 2.0,
            "sum {low}+{high} vs {whole}"
        );
    }

    #[test]
    fn a_hollow_box_cavity_sits_inside_and_stops_at_the_caps() {
        let mesh = box_mesh([0.0, 0.0, 0.0], [10.0, 10.0, 10.0]);
        let cavity = cavity_shell(&mesh, 2.0, 1.5, 8.5).expect("cavity");
        assert!(
            cavity.signed_volume_mm3() < 0.0,
            "the cavity should subtract, volume {}",
            cavity.signed_volume_mm3()
        );
        let (min, max) = cavity.bounds().unwrap();
        assert!(min[2] >= 1.4 && max[2] <= 8.6, "z bounds {min:?} {max:?}");
        assert!(min[0] > 0.3 && max[0] < 9.7, "the void should sit inside x");
        assert!(
            mesh_contains(&cavity, [5.0, 5.0, 5.0]),
            "the middle of the hollow is not empty"
        );
        assert!(
            !mesh_contains(&cavity, [0.4, 5.0, 5.0]),
            "the wall was carved out"
        );
        assert!(
            !mesh_contains(&cavity, [5.0, 5.0, 9.2]),
            "the top cap was hollowed"
        );
    }

    fn mesh_contains(mesh: &Mesh, origin: [f32; 3]) -> bool {
        let dir = [1.0, 0.017, 0.011];
        let mut hits = 0i32;
        for tri in mesh.indices.chunks_exact(3) {
            let a = mesh.vertices[tri[0] as usize];
            let b = mesh.vertices[tri[1] as usize];
            let c = mesh.vertices[tri[2] as usize];
            if ray_hits_triangle(origin, dir, a, b, c) {
                hits += 1;
            }
        }
        hits % 2 == 1
    }

    fn ray_hits_triangle(
        origin: [f32; 3],
        dir: [f32; 3],
        a: [f32; 3],
        b: [f32; 3],
        c: [f32; 3],
    ) -> bool {
        let e1 = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let e2 = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let p = [
            dir[1] * e2[2] - dir[2] * e2[1],
            dir[2] * e2[0] - dir[0] * e2[2],
            dir[0] * e2[1] - dir[1] * e2[0],
        ];
        let det = e1[0] * p[0] + e1[1] * p[1] + e1[2] * p[2];
        if det.abs() < 1e-8 {
            return false;
        }
        let inv = 1.0 / det;
        let tvec = [origin[0] - a[0], origin[1] - a[1], origin[2] - a[2]];
        let u = (tvec[0] * p[0] + tvec[1] * p[1] + tvec[2] * p[2]) * inv;
        if !(0.0..=1.0).contains(&u) {
            return false;
        }
        let q = [
            tvec[1] * e1[2] - tvec[2] * e1[1],
            tvec[2] * e1[0] - tvec[0] * e1[2],
            tvec[0] * e1[1] - tvec[1] * e1[0],
        ];
        let v = (dir[0] * q[0] + dir[1] * q[1] + dir[2] * q[2]) * inv;
        if v < 0.0 || u + v > 1.0 {
            return false;
        }
        let t = (e2[0] * q[0] + e2[1] * q[1] + e2[2] * q[2]) * inv;
        t > 1e-4
    }
}
