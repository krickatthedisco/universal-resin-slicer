//! A saved plate: models, supports, drain holes, and the print settings.
//!
//! The file is a zip named `.amber`. `plate.json` describes the job.
//! Each mesh is a little-endian `AMSH` blob so a round trip does not
//! move a vertex.

use crate::mesh::Mesh;
use crate::printer::PrintSettings;
use crate::scene::{DrainHole, ModelSupport, Object};
use crate::supports::Support;
use anyhow::{anyhow, Context, Result};
use glam::Vec3;
use std::io::{Read, Write};
use std::path::Path;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

const VERSION: u32 = 1;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct PlateJson {
    version: u32,
    machine_id: String,
    #[serde(default)]
    rotate_180: bool,
    #[serde(default)]
    mirror_x: bool,
    #[serde(default)]
    mirror_y: bool,
    settings: PrintSettings,
    objects: Vec<ObjectJson>,
    supports: Vec<Support>,
    drains: Vec<DrainHole>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct ObjectJson {
    id: u64,
    name: String,
    position: Vec3,
    rotation_deg: Vec3,
    scale: Vec3,
    hollow: bool,
    wall_mm: f32,
    bottom_cap_mm: f32,
    top_cap_mm: f32,
    support: ModelSupport,
    mesh: String,
}

pub struct PlateData {
    pub machine_id: String,
    pub rotate_180: bool,
    pub mirror_x: bool,
    pub mirror_y: bool,
    pub settings: PrintSettings,
    pub objects: Vec<Object>,
    pub supports: Vec<Support>,
    pub drains: Vec<DrainHole>,
}

pub fn write_plate(path: &Path, plate: &PlateData) -> Result<()> {
    let file = std::fs::File::create(path)
        .with_context(|| format!("could not create {}", path.display()))?;
    let mut zip = ZipWriter::new(file);
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    let mut objects = Vec::new();
    for obj in &plate.objects {
        let name = format!("meshes/{}.amsh", obj.id);
        zip.start_file(&name, opts)?;
        zip.write_all(&encode_mesh(&obj.mesh))?;
        objects.push(ObjectJson {
            id: obj.id,
            name: obj.name.clone(),
            position: obj.position,
            rotation_deg: obj.rotation_deg,
            scale: obj.scale,
            hollow: obj.hollow,
            wall_mm: obj.wall_mm,
            bottom_cap_mm: obj.bottom_cap_mm,
            top_cap_mm: obj.top_cap_mm,
            support: obj.support,
            mesh: name,
        });
    }
    let json = PlateJson {
        version: VERSION,
        machine_id: plate.machine_id.clone(),
        rotate_180: plate.rotate_180,
        mirror_x: plate.mirror_x,
        mirror_y: plate.mirror_y,
        settings: plate.settings.clone(),
        objects,
        supports: plate.supports.clone(),
        drains: plate.drains.clone(),
    };
    zip.start_file("plate.json", opts)?;
    zip.write_all(&serde_json::to_vec_pretty(&json)?)?;
    zip.finish()?;
    Ok(())
}

pub fn read_plate(path: &Path) -> Result<PlateData> {
    let file =
        std::fs::File::open(path).with_context(|| format!("could not open {}", path.display()))?;
    let mut zip = ZipArchive::new(file).context("that file is not an Amber plate")?;
    let json: PlateJson = {
        let mut entry = zip
            .by_name("plate.json")
            .context("that file has no plate.json")?;
        let mut raw = String::new();
        entry.read_to_string(&mut raw)?;
        serde_json::from_str(&raw).context("plate.json could not be read")?
    };
    if json.version != VERSION {
        return Err(anyhow!(
            "This plate was written by a newer Amber (version {}). This copy reads version {VERSION}.",
            json.version
        ));
    }
    let mut objects = Vec::with_capacity(json.objects.len());
    for saved in json.objects {
        let mesh = {
            let mut entry = zip
                .by_name(&saved.mesh)
                .with_context(|| format!("missing mesh {}", saved.mesh))?;
            let mut raw = Vec::new();
            entry.read_to_end(&mut raw)?;
            decode_mesh(&raw).with_context(|| format!("bad mesh {}", saved.mesh))?
        };
        objects.push(Object {
            id: saved.id,
            name: saved.name,
            mesh,
            position: saved.position,
            rotation_deg: saved.rotation_deg,
            scale: saved.scale,
            hollow: saved.hollow,
            wall_mm: saved.wall_mm,
            bottom_cap_mm: saved.bottom_cap_mm,
            top_cap_mm: saved.top_cap_mm,
            support: saved.support,
            mesh_rev: 1,
            bounds_min: [0.0; 3],
            bounds_max: [0.0; 3],
        });
    }
    Ok(PlateData {
        machine_id: json.machine_id,
        rotate_180: json.rotate_180,
        mirror_x: json.mirror_x,
        mirror_y: json.mirror_y,
        settings: json.settings,
        objects,
        supports: json.supports,
        drains: json.drains,
    })
}

fn encode_mesh(mesh: &Mesh) -> Vec<u8> {
    let mut buf = Vec::with_capacity(8 + mesh.vertices.len() * 12 + mesh.indices.len() * 4);
    buf.extend_from_slice(b"AMSH");
    buf.extend_from_slice(&(mesh.vertices.len() as u32).to_le_bytes());
    buf.extend_from_slice(&(mesh.indices.len() as u32).to_le_bytes());
    for v in &mesh.vertices {
        for c in v {
            buf.extend_from_slice(&c.to_le_bytes());
        }
    }
    for i in &mesh.indices {
        buf.extend_from_slice(&i.to_le_bytes());
    }
    buf
}

fn decode_mesh(raw: &[u8]) -> Result<Mesh> {
    if raw.len() < 12 || &raw[..4] != b"AMSH" {
        return Err(anyhow!("mesh header is not AMSH"));
    }
    let verts = u32::from_le_bytes(raw[4..8].try_into().unwrap()) as usize;
    let indices = u32::from_le_bytes(raw[8..12].try_into().unwrap()) as usize;
    let need = 12 + verts * 12 + indices * 4;
    if raw.len() < need {
        return Err(anyhow!("mesh is shorter than its header claims"));
    }
    let mut vertices = Vec::with_capacity(verts);
    let mut cursor = 12;
    for _ in 0..verts {
        let mut point = [0.0f32; 3];
        for c in &mut point {
            *c = f32::from_le_bytes(raw[cursor..cursor + 4].try_into().unwrap());
            cursor += 4;
        }
        vertices.push(point);
    }
    let mut index = Vec::with_capacity(indices);
    for _ in 0..indices {
        index.push(u32::from_le_bytes(
            raw[cursor..cursor + 4].try_into().unwrap(),
        ));
        cursor += 4;
    }
    Ok(Mesh {
        vertices,
        indices: index,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::box_mesh;
    use crate::scene::Document;

    #[test]
    fn a_plate_round_trips_the_model_support_and_hole() {
        let mut doc = Document::new();
        let id = doc.add_mesh("cup".into(), box_mesh([0.0, 0.0, 0.0], [10.0, 12.0, 8.0]));
        doc.object_mut(id).unwrap().position = Vec3::new(30.0, 4.0, 0.0);
        doc.object_mut(id).unwrap().hollow = true;
        doc.object_mut(id).unwrap().wall_mm = 1.6;
        doc.add_support_at(Vec3::new(32.0, 6.0, 8.0), id);
        doc.punch_bottom_drain(id);
        let support = doc.supports[0];
        let drain = doc.drains[0];
        let path = std::env::temp_dir().join(format!("amber-plate-{}.amber", std::process::id()));
        let data = PlateData {
            machine_id: "anycubic-photon-m3-max".into(),
            rotate_180: true,
            mirror_x: false,
            mirror_y: false,
            settings: PrintSettings::default(),
            objects: doc.objects.clone(),
            supports: doc.supports.clone(),
            drains: doc.drains.clone(),
        };
        write_plate(&path, &data).unwrap();
        let back = read_plate(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(back.machine_id, "anycubic-photon-m3-max");
        assert!(back.rotate_180);
        assert_eq!(back.objects.len(), 1);
        assert_eq!(back.objects[0].id, id);
        assert_eq!(back.objects[0].name, "cup");
        assert!(back.objects[0].hollow);
        assert!((back.objects[0].wall_mm - 1.6).abs() < 1e-4);
        assert!((back.objects[0].position.x - 30.0).abs() < 1e-4);
        assert_eq!(
            back.objects[0].mesh.triangle_count(),
            doc.objects[0].mesh.triangle_count()
        );
        assert_eq!(back.objects[0].mesh.vertices, doc.objects[0].mesh.vertices);
        assert_eq!(back.supports.len(), 1);
        assert_eq!(back.supports[0].object_id, id);
        assert!((back.supports[0].x - support.x).abs() < 1e-3);
        assert_eq!(back.drains.len(), 1);
        assert!((back.drains[0].radius_mm - drain.radius_mm).abs() < 1e-4);
        let mut loaded = Document::new();
        loaded.load_plate(back.objects, back.supports, back.drains);
        let fresh = loaded.add_mesh("extra".into(), box_mesh([0.0, 0.0, 0.0], [1.0, 1.0, 1.0]));
        assert_ne!(fresh, id);
        assert_ne!(fresh, support.id);
        assert_ne!(fresh, drain.id);
    }
}
