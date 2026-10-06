//! Unencrypted Chitubox `.ctb` (magic `0x12FD0086`, version 2).
//!
//! The layout follows the Catibo notes for the common CTB file: little-endian
//! header, RLE7 layers, RLE15 previews, and an encryption key of zero. Printers
//! that require the later encrypted container are not written this way.

use crate::printer::{layer_motion, Machine, PrintSettings};
use crate::slice::{decode_rle, Slice};
use std::path::Path;

const MAGIC: u32 = 0x12FD_0086;
const HEADER: usize = 0x70;
const EXT_CONFIG: usize = 0x40;
const EXT_CONFIG2: usize = 0x50;
const LAYER_REC: usize = 36;

pub fn write_ctb(
    path: &Path,
    slice: &Slice,
    machine: Machine,
    settings: &PrintSettings,
) -> std::io::Result<()> {
    let bytes = encode_ctb(slice, machine, settings)?;
    std::fs::write(path, bytes)
}

pub fn encode_ctb(
    slice: &Slice,
    machine: Machine,
    settings: &PrintSettings,
) -> std::io::Result<Vec<u8>> {
    let settings = settings.sanitized();
    let layers = &slice.layers;
    if layers.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "nothing to export",
        ));
    }
    let large = preview_rle15(&slice.thumbnail, 224, 168, 400, 300);
    let small = preview_rle15(&slice.thumbnail, 224, 168, 200, 125);
    let mut blobs = Vec::with_capacity(layers.len());
    for layer in layers {
        let gray = decode_rle(&layer.rle, slice.width, slice.height)
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
        blobs.push(encode_rle7(&gray));
    }

    let name = machine.name.as_bytes();
    let ext_at = HEADER;
    let ext2_at = ext_at + EXT_CONFIG;
    let name_at = ext2_at + EXT_CONFIG2;
    let large_at = name_at + name.len();
    let large_data_at = large_at + 32;
    let small_at = large_data_at + large.len();
    let small_data_at = small_at + 32;
    let table_at = small_data_at + small.len();
    let mut data_at = table_at + LAYER_REC * layers.len();
    let mut layer_at = Vec::with_capacity(layers.len());
    for blob in &blobs {
        layer_at.push(data_at);
        data_at += blob.len();
    }

    let mut out = vec![0u8; data_at];
    put_u32(&mut out, 0x00, MAGIC);
    put_u32(&mut out, 0x04, 2);
    put_f32(&mut out, 0x08, machine.size_x);
    put_f32(&mut out, 0x0C, machine.size_y);
    put_f32(&mut out, 0x10, machine.size_z);
    put_f32(
        &mut out,
        0x1C,
        layers.last().map(|l| l.z_top_mm).unwrap_or(0.0),
    );
    put_f32(&mut out, 0x20, settings.layer_mm);
    put_f32(&mut out, 0x24, settings.exposure_s);
    put_f32(&mut out, 0x28, settings.bottom_exposure_s);
    put_f32(&mut out, 0x2C, settings.wait_s());
    put_u32(&mut out, 0x30, settings.bottom_layers);
    put_f32(&mut out, 0x34, machine.res_x as f32);
    put_f32(&mut out, 0x38, machine.res_y as f32);
    put_u32(&mut out, 0x3C, large_at as u32);
    put_u32(&mut out, 0x40, table_at as u32);
    put_u32(&mut out, 0x44, layers.len() as u32);
    put_u32(&mut out, 0x48, small_at as u32);
    put_u32(&mut out, 0x4C, slice.seconds);
    put_u32(&mut out, 0x50, 1);
    put_u32(&mut out, 0x54, ext_at as u32);
    put_u32(&mut out, 0x58, EXT_CONFIG as u32);
    put_u32(&mut out, 0x5C, 1);
    put_u16(&mut out, 0x60, 255);
    put_u16(&mut out, 0x62, 255);
    put_u32(&mut out, 0x64, 0);
    put_u32(&mut out, 0x68, ext2_at as u32);
    put_u32(&mut out, 0x6C, EXT_CONFIG2 as u32);

    let (bot_lift, bot_speed, _, bot_retract) = layer_motion(&settings, 0);
    let (_, lift, speed, retract) = layer_motion(&settings, settings.bottom_layers.max(1));
    put_f32(&mut out, ext_at, bot_lift);
    put_f32(&mut out, ext_at + 4, bot_speed * 60.0);
    put_f32(&mut out, ext_at + 8, lift);
    put_f32(&mut out, ext_at + 12, speed * 60.0);
    put_f32(&mut out, ext_at + 16, retract.max(bot_retract) * 60.0);
    put_f32(&mut out, ext_at + 20, slice.cured_ml);
    put_f32(&mut out, ext_at + 24, slice.weight_g);
    put_f32(
        &mut out,
        ext_at + 28,
        slice.cured_ml * settings.price_per_liter / 1000.0,
    );
    put_f32(&mut out, ext_at + 32, settings.wait_s());
    put_f32(&mut out, ext_at + 36, settings.wait_s());
    put_u32(&mut out, ext_at + 40, settings.bottom_layers);

    put_u32(&mut out, ext2_at + 0x1C, name_at as u32);
    put_u32(&mut out, ext2_at + 0x20, name.len() as u32);
    put_u32(&mut out, ext2_at + 0x24, 0xF);
    put_u32(&mut out, ext2_at + 0x2C, settings.anti_alias as u32);
    put_u32(&mut out, ext2_at + 0x30, 0x0106_0300);
    put_u32(&mut out, ext2_at + 0x34, 0x200);
    out[name_at..name_at + name.len()].copy_from_slice(name);

    write_preview(&mut out, large_at, large_data_at, 400, 300, &large);
    write_preview(&mut out, small_at, small_data_at, 200, 125, &small);

    for (i, layer) in layers.iter().enumerate() {
        let at = table_at + i * LAYER_REC;
        let (_, lift_mm, lift_speed, _) = layer_motion(&settings, layer.index);
        let _ = (lift_mm, lift_speed);
        put_f32(&mut out, at, layer.z_top_mm);
        put_f32(&mut out, at + 4, layer.exposure_s);
        put_f32(&mut out, at + 8, settings.wait_s());
        put_u32(&mut out, at + 12, layer_at[i] as u32);
        put_u32(&mut out, at + 16, blobs[i].len() as u32);
    }
    for (i, blob) in blobs.iter().enumerate() {
        let at = layer_at[i];
        out[at..at + blob.len()].copy_from_slice(blob);
    }
    Ok(out)
}

fn write_preview(out: &mut [u8], header: usize, data: usize, w: u32, h: u32, bytes: &[u8]) {
    put_u32(out, header, w);
    put_u32(out, header + 4, h);
    put_u32(out, header + 8, data as u32);
    put_u32(out, header + 12, bytes.len() as u32);
    out[data..data + bytes.len()].copy_from_slice(bytes);
}

fn preview_rle15(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    let mut pixels = Vec::with_capacity((dw * dh) as usize);
    for y in 0..dh {
        for x in 0..dw {
            let sx = ((x as f32 + 0.5) / dw as f32 * sw as f32) as u32;
            let sy = ((y as f32 + 0.5) / dh as f32 * sh as f32) as u32;
            let sx = sx.min(sw.saturating_sub(1));
            let sy = sy.min(sh.saturating_sub(1));
            let gray = if src.is_empty() {
                0
            } else {
                src[(sy * sw + sx) as usize]
            };
            pixels.push(rgb565(gray));
        }
    }
    encode_rle15(&pixels)
}

fn rgb565(gray: u8) -> u16 {
    let c = (gray as u16) >> 3;
    (c << 11) | (c << 6) | c
}

/// Run length is the number of extra copies after the first pixel.
fn encode_rle15(pixels: &[u16]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < pixels.len() {
        let v = pixels[i] & 0xFFDF;
        let mut n = 1usize;
        while i + n < pixels.len() && n < 0x0FFF && (pixels[i + n] & 0xFFDF) == v {
            n += 1;
        }
        if n == 1 {
            out.extend_from_slice(&v.to_le_bytes());
        } else {
            out.extend_from_slice(&(v | 0x0020).to_le_bytes());
            let extra = 0x3000 | ((n - 1) as u16);
            out.extend_from_slice(&extra.to_le_bytes());
        }
        i += n;
    }
    out
}

/// 7-bit pixels. A run's length is the total number of pixels, not extras.
pub fn encode_rle7(gray: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < gray.len() {
        let v = ((gray[i] as u16 * 127) / 255) as u8 & 0x7F;
        let mut n = 1usize;
        while i + n < gray.len() {
            let w = ((gray[i + n] as u16 * 127) / 255) as u8 & 0x7F;
            if w != v || n == 0x0FFF_FFFF {
                break;
            }
            n += 1;
        }
        if n == 1 {
            out.push(v);
        } else {
            out.push(v | 0x80);
            push_run(&mut out, n);
        }
        i += n;
    }
    out
}

fn push_run(out: &mut Vec<u8>, n: usize) {
    if n < 0x80 {
        out.push(n as u8);
    } else if n < 0x4000 {
        out.push(0x80 | ((n >> 8) as u8 & 0x3F));
        out.push((n & 0xFF) as u8);
    } else if n < 0x20_0000 {
        out.push(0xC0 | ((n >> 16) as u8 & 0x1F));
        out.push(((n >> 8) & 0xFF) as u8);
        out.push((n & 0xFF) as u8);
    } else {
        out.push(0xE0 | ((n >> 24) as u8 & 0x0F));
        out.push(((n >> 16) & 0xFF) as u8);
        out.push(((n >> 8) & 0xFF) as u8);
        out.push((n & 0xFF) as u8);
    }
}

fn put_u16(buf: &mut [u8], at: usize, v: u16) {
    buf[at..at + 2].copy_from_slice(&v.to_le_bytes());
}
fn put_u32(buf: &mut [u8], at: usize, v: u32) {
    buf[at..at + 4].copy_from_slice(&v.to_le_bytes());
}
fn put_f32(buf: &mut [u8], at: usize, v: f32) {
    buf[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::printer::PrintSettings;
    use crate::slice::Layer;

    fn tiny_slice() -> Slice {
        Slice {
            width: 8,
            height: 4,
            layers: vec![Layer {
                index: 0,
                z_top_mm: 0.05,
                thickness_mm: 0.05,
                exposure_s: 40.0,
                lift_mm: 6.0,
                lift_speed: 1.0,
                // 32 black pixels, pw0Img.
                rle: vec![0x00, 0x20],
                nonzero: 0,
                coverage: 0.0,
                islands: Vec::new(),
                seals_cavity_px: 0,
            }],
            cured_ml: 0.01,
            weight_g: 0.011,
            seconds: 50,
            warnings: Vec::new(),
            thumbnail: vec![0; 224 * 168],
            cavity_ml: 0.0,
            sealed_layers: 0,
        }
    }

    #[test]
    fn ctb_header_is_unencrypted_v3() {
        let machine = Machine::photon_m3_max();
        let settings = PrintSettings::default();
        let bytes = encode_ctb(&tiny_slice(), machine, &settings).unwrap();
        let magic = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
        let version = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        let key = u32::from_le_bytes(bytes[0x64..0x68].try_into().unwrap());
        let res_x = f32::from_le_bytes(bytes[0x34..0x38].try_into().unwrap());
        assert_eq!(magic, 0x12FD_0086);
        assert_eq!(version, 2);
        assert_eq!(key, 0);
        assert!((res_x - machine.res_x as f32).abs() < 0.5);
        let count = u32::from_le_bytes(bytes[0x44..0x48].try_into().unwrap());
        assert_eq!(count, 1);
    }

    #[test]
    fn rle7_round_trip_counts_every_pixel() {
        let src = [0u8, 0, 255, 255, 255, 128, 0];
        let enc = encode_rle7(&src);
        let dec = decode_rle7(&enc, src.len());
        let expect: Vec<u8> = src
            .iter()
            .map(|v| ((*v as u16 * 127) / 255) as u8)
            .collect();
        assert_eq!(dec, expect);
    }

    fn decode_rle7(data: &[u8], n: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(n);
        let mut i = 0;
        while out.len() < n && i < data.len() {
            let b = data[i];
            i += 1;
            let v = b & 0x7F;
            if b & 0x80 == 0 {
                out.push(v);
                continue;
            }
            let lb = data[i];
            i += 1;
            let len = if lb & 0x80 == 0 {
                lb as usize
            } else if lb & 0xC0 == 0x80 {
                let next = data[i] as usize;
                i += 1;
                (((lb & 0x3F) as usize) << 8) | next
            } else if lb & 0xE0 == 0xC0 {
                let a = data[i] as usize;
                let b = data[i + 1] as usize;
                i += 2;
                (((lb & 0x1F) as usize) << 16) | (a << 8) | b
            } else {
                let a = data[i] as usize;
                let b = data[i + 1] as usize;
                let c = data[i + 2] as usize;
                i += 3;
                (((lb & 0x0F) as usize) << 24) | (a << 16) | (b << 8) | c
            };
            for _ in 0..len {
                if out.len() == n {
                    break;
                }
                out.push(v);
            }
        }
        out
    }
}
