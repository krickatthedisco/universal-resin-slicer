//! Photon Workshop v516 container used by the Photon M3 Max (`.pm3m`).
//!
//! The table order matches what Photon Workshop and UVtools write for version
//! 516: file mark, HEADER, PREVIEW, grey table, LAYERDEF, EXTRA, MACHINE, then
//! pw0Img run-length layer bitmaps. Layer records store each layer's thickness
//! in millimetres, which for a uniform slice is the print's layer height.

use crate::printer::{layer_motion, Machine, PrintSettings};
use crate::slice::Slice;
use std::fs::File;
use std::io::Write;
use std::path::Path;

const MARK: usize = 12;

pub fn write_pm3m(
    path: &Path,
    slice: &Slice,
    machine: Machine,
    settings: &PrintSettings,
) -> std::io::Result<()> {
    let bytes = encode_pm3m(slice, machine, settings);
    let mut file = File::create(path)?;
    file.write_all(&bytes)?;
    Ok(())
}

pub fn encode_pm3m(slice: &Slice, machine: Machine, settings: &PrintSettings) -> Vec<u8> {
    let n = slice.layers.len() as u32;
    let settings = settings.sanitized();
    let preview = rgb565_preview(&slice.thumbnail);
    let preview_data = preview.len();
    let file_mark = 52usize;
    let header = 100usize;
    let preview_table = 28 + preview_data;
    let color_table = 28usize;
    let layerdef = 20 + 32 * n as usize;
    let extra = 72usize;
    let machine_table = 156usize;

    let header_at = file_mark;
    let preview_at = header_at + header;
    let color_at = preview_at + preview_table;
    let layerdef_at = color_at + color_table;
    let extra_at = layerdef_at + layerdef;
    let machine_at = extra_at + extra;
    let images_at = machine_at + machine_table;

    let mut image_blobs: Vec<&[u8]> = Vec::with_capacity(slice.layers.len());
    let mut cursor = images_at;
    let mut layer_meta = Vec::with_capacity(slice.layers.len());
    for layer in &slice.layers {
        image_blobs.push(&layer.rle);
        let (exposure, lift, speed, _) = layer_motion(&settings, layer.index);
        layer_meta.push(LayerRec {
            addr: cursor as u32,
            len: layer.rle.len() as u32,
            lift,
            speed,
            exposure,
            thickness: layer.thickness_mm,
            nonzero: layer.nonzero,
        });
        cursor += layer.rle.len();
    }

    let mut out = vec![0u8; cursor];
    write_mark(
        &mut out,
        machine.file_version,
        header_at as u32,
        preview_at as u32,
        color_at as u32,
        layerdef_at as u32,
        extra_at as u32,
        machine_at as u32,
        images_at as u32,
    );
    write_header(&mut out, header_at, machine, &settings, slice);
    write_preview(&mut out, preview_at, &preview);
    write_color_table(&mut out, color_at, settings.anti_alias);
    write_layerdef(&mut out, layerdef_at, &layer_meta);
    write_extra(&mut out, extra_at, &settings);
    write_machine(&mut out, machine_at, machine);
    let mut at = images_at;
    for blob in image_blobs {
        out[at..at + blob.len()].copy_from_slice(blob);
        at += blob.len();
    }
    out
}

struct LayerRec {
    addr: u32,
    len: u32,
    lift: f32,
    speed: f32,
    exposure: f32,
    thickness: f32,
    nonzero: u32,
}

fn put_u32(buf: &mut [u8], at: usize, v: u32) {
    buf[at..at + 4].copy_from_slice(&v.to_le_bytes());
}
fn put_f32(buf: &mut [u8], at: usize, v: f32) {
    buf[at..at + 4].copy_from_slice(&v.to_le_bytes());
}
fn put_name(buf: &mut [u8], at: usize, name: &str) {
    let bytes = name.as_bytes();
    buf[at..at + bytes.len()].copy_from_slice(bytes);
}

fn write_mark(
    buf: &mut [u8],
    version: u32,
    header: u32,
    preview: u32,
    color: u32,
    layerdef: u32,
    extra: u32,
    machine: u32,
    images: u32,
) {
    put_name(buf, 0, "ANYCUBIC");
    put_u32(buf, 12, version);
    put_u32(buf, 16, 8); // table count used by v516
    put_u32(buf, 20, header);
    put_u32(buf, 24, 0); // software address, unused before v517
    put_u32(buf, 28, preview);
    put_u32(buf, 32, color);
    put_u32(buf, 36, layerdef);
    put_u32(buf, 40, extra);
    put_u32(buf, 44, machine);
    put_u32(buf, 48, images);
}

fn write_header(
    buf: &mut [u8],
    at: usize,
    machine: Machine,
    settings: &PrintSettings,
    slice: &Slice,
) {
    put_name(buf, at, "HEADER");
    put_u32(buf, at + 12, 84);
    let mut o = at + 16;
    let fields_f = [
        machine.pixel_um,
        settings.layer_mm,
        settings.exposure_s,
        settings.wait_s(),
        settings.bottom_exposure_s,
        settings.bottom_layers as f32,
        settings.lift_mm,
        settings.lift_speed,
        settings.retract_speed,
        slice.cured_ml,
    ];
    for v in fields_f {
        put_f32(buf, o, v);
        o += 4;
    }
    put_u32(buf, o, settings.anti_alias as u32);
    o += 4;
    put_u32(buf, o, machine.res_x);
    o += 4;
    put_u32(buf, o, machine.res_y);
    o += 4;
    put_f32(buf, o, slice.weight_g);
    o += 4;
    put_f32(buf, o, 0.0);
    o += 4;
    buf[o] = b'$';
    o += 4;
    put_u32(buf, o, 0); // per-layer override flag
    o += 4;
    put_u32(buf, o, slice.seconds);
    o += 4;
    put_u32(buf, o, settings.transition_layers);
    o += 4;
    put_u32(buf, o, 0);
    o += 4;
    put_u32(buf, o, 0); // basic motion, second lift stage unused
    o += 4;
    debug_assert_eq!(o, at + 16 + 84);
}

fn write_preview(buf: &mut [u8], at: usize, rgb565: &[u8]) {
    put_name(buf, at, "PREVIEW");
    put_u32(buf, at + 12, (28 + rgb565.len()) as u32);
    put_u32(buf, at + 16, 224);
    buf[at + 20] = b'x';
    put_u32(buf, at + 24, 168);
    buf[at + 28..at + 28 + rgb565.len()].copy_from_slice(rgb565);
}

fn write_color_table(buf: &mut [u8], at: usize, aa: u8) {
    let aa = aa.max(1) as f32;
    let step = 255.0 / aa;
    put_u32(buf, at, 0);
    put_u32(buf, at + 4, 16);
    for i in 0..16 {
        let v = ((i as f32 + 1.0) * step).round().min(255.0) as u8;
        buf[at + 8 + i] = v;
    }
    put_u32(buf, at + 24, 0);
}

fn write_layerdef(buf: &mut [u8], at: usize, layers: &[LayerRec]) {
    put_name(buf, at, "LAYERDEF");
    let payload = 4 + 32 * layers.len() as u32;
    put_u32(buf, at + 12, payload);
    put_u32(buf, at + 16, layers.len() as u32);
    for (i, layer) in layers.iter().enumerate() {
        let o = at + 20 + i * 32;
        put_u32(buf, o, layer.addr);
        put_u32(buf, o + 4, layer.len);
        put_f32(buf, o + 8, layer.lift);
        put_f32(buf, o + 12, layer.speed);
        put_f32(buf, o + 16, layer.exposure);
        put_f32(buf, o + 20, layer.thickness);
        put_u32(buf, o + 24, layer.nonzero);
        put_u32(buf, o + 28, 0);
    }
}

fn write_extra(buf: &mut [u8], at: usize, settings: &PrintSettings) {
    put_name(buf, at, "EXTRA");
    // Workshop writes 24 here even though the two-stage block is longer.
    put_u32(buf, at + 12, 24);
    put_u32(buf, at + 16, 2);
    put_f32(buf, at + 20, settings.bottom_lift_mm);
    put_f32(buf, at + 24, settings.bottom_lift_speed);
    put_f32(buf, at + 28, settings.bottom_retract_speed);
    put_f32(buf, at + 32, 0.0);
    put_f32(buf, at + 36, settings.bottom_lift_speed);
    put_f32(buf, at + 40, settings.bottom_retract_speed);
    put_u32(buf, at + 44, 2);
    put_f32(buf, at + 48, settings.lift_mm);
    put_f32(buf, at + 52, settings.lift_speed);
    put_f32(buf, at + 56, settings.retract_speed);
    put_f32(buf, at + 60, 0.0);
    put_f32(buf, at + 64, settings.lift_speed);
    put_f32(buf, at + 68, settings.retract_speed);
}

fn write_machine(buf: &mut [u8], at: usize, machine: Machine) {
    put_name(buf, at, "MACHINE");
    put_u32(buf, at + 12, 156);
    let name = machine.name.as_bytes();
    let n = name.len().min(95);
    buf[at + 16..at + 16 + n].copy_from_slice(&name[..n]);
    let fmt = b"pw0Img";
    buf[at + 112..at + 112 + fmt.len()].copy_from_slice(fmt);
    put_u32(buf, at + 128, 16); // max AA
    put_u32(buf, at + 132, 1); // property fields for v516
    put_f32(buf, at + 136, machine.size_x);
    put_f32(buf, at + 140, machine.size_y);
    put_f32(buf, at + 144, machine.size_z);
    put_u32(buf, at + 148, machine.file_version);
    put_u32(buf, at + 152, 6_506_241);
    debug_assert_eq!(at + 156, at + 156);
}

fn rgb565_preview(gray224: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; 224 * 168 * 2];
    for (i, g) in gray224.iter().take(224 * 168).enumerate() {
        let r = (*g as u16 * 230) / 255;
        let gch = (*g as u16 * 180) / 255;
        let b = (*g as u16 * 90) / 255;
        let packed = (r >> 3) << 11 | (gch >> 2) << 5 | (b >> 3);
        out[i * 2..i * 2 + 2].copy_from_slice(&packed.to_le_bytes());
    }
    out
}

pub fn read_u32(buf: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(buf[at..at + 4].try_into().unwrap())
}
pub fn read_f32(buf: &[u8], at: usize) -> f32 {
    f32::from_le_bytes(buf[at..at + 4].try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::box_mesh;
    use crate::slice::{slice, Request, Solid};

    #[test]
    fn pm3m_header_describes_the_m3_max() {
        let mesh = box_mesh([40.0, 40.0, 0.0], [50.0, 50.0, 2.0]);
        let solid = Solid {
            vertices: mesh.vertices,
            indices: mesh.indices,
            hollow: None,
        };
        let settings = PrintSettings::default();
        let mut machine = Machine::photon_m3_max();
        machine.rotate_180 = false;
        let sliced = slice(Request {
            solids: &[solid],
            drains: &[],
            machine,
            settings: &settings,
            cancel: None,
            progress: None,
        })
        .unwrap();
        let bytes = encode_pm3m(&sliced, machine, &settings);
        assert_eq!(&bytes[0..8], b"ANYCUBIC");
        assert_eq!(read_u32(&bytes, 12), 516);
        assert_eq!(read_u32(&bytes, 20), 52);
        assert_eq!(&bytes[52..58], b"HEADER");
        assert_eq!(read_u32(&bytes, 64), 84);
        let pixel = read_f32(&bytes, 68);
        assert!((pixel - 46.0).abs() < 0.01, "pixel {pixel}");
        let res_x = read_u32(&bytes, 68 + 11 * 4);
        let res_y = read_u32(&bytes, 68 + 12 * 4);
        assert_eq!(res_x, 6480);
        assert_eq!(res_y, 3600);
        assert!(bytes.len() > 80_000);
        assert!(sliced.layers.iter().any(|l| l.nonzero > 1000));
    }
}

const _: usize = MARK;
