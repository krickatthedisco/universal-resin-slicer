//! Triangle meshes, STL/OBJ import, and a few built-in calibration shapes.

use anyhow::{anyhow, bail, Result};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
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
}

pub fn load_mesh(path: &Path) -> Result<Mesh> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let mut mesh = match ext.as_str() {
        "stl" => load_stl(path)?,
        "obj" => load_obj(path)?,
        _ => bail!("Amber imports STL and OBJ. `{ext}` is not one of those."),
    };
    if mesh.triangle_count() == 0 {
        bail!("That file has no triangles.");
    }
    mesh.weld(1e-4);
    Ok(mesh)
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
}
