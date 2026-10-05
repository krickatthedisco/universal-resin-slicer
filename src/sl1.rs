//! Prusa `.sl1` layer stack. Every machine Amber knows can be written this way.
//! Printers that do not read `.sl1` directly can open it in a converter.

use crate::printer::{Machine, PrintSettings};
use crate::slice::{decode_rle, Slice};
use std::io::{Cursor, Write};
use std::path::Path;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

pub fn write_sl1(
    path: &Path,
    slice: &Slice,
    machine: Machine,
    settings: &PrintSettings,
) -> std::io::Result<()> {
    let file = std::fs::File::create(path)?;
    let mut zip = ZipWriter::new(file);
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    zip.start_file("config.ini", opts)?;
    zip.write_all(config_ini(slice, machine, settings).as_bytes())?;
    for (i, layer) in slice.layers.iter().enumerate() {
        let png = layer_png(&layer.rle, slice.width, slice.height)
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
        zip.start_file(format!("{i:05}.png"), opts)?;
        zip.write_all(&png)?;
    }
    zip.finish()?;
    Ok(())
}

fn config_ini(slice: &Slice, machine: Machine, settings: &PrintSettings) -> String {
    let settings = settings.sanitized();
    format!(
        "action = print\n\
         expTime = {exp:.3}\n\
         expTimeFirst = {first:.3}\n\
         layerHeight = {h:.3}\n\
         materialName = {resin}\n\
         numFade = {fade}\n\
         numSlow = {slow}\n\
         printTime = {secs}\n\
         usedMaterial = {ml:.3}\n\
         layerCount = {layers}\n\
         printerModel = {model}\n\
         printerVendor = {vendor}\n\
         displayWidth = {sx:.3}\n\
         displayHeight = {sy:.3}\n\
         displayPixelsX = {rx}\n\
         displayPixelsY = {ry}\n\
         prusaSlicerVersion = Amber\n",
        exp = settings.exposure_s,
        first = settings.bottom_exposure_s,
        h = settings.layer_mm,
        resin = settings.resin.replace(['\n', '\r'], " "),
        fade = settings.transition_layers,
        slow = settings.bottom_layers,
        secs = slice.seconds,
        ml = slice.cured_ml,
        layers = slice.layers.len(),
        model = machine.name,
        vendor = machine.vendor,
        sx = machine.size_x,
        sy = machine.size_y,
        rx = machine.res_x,
        ry = machine.res_y,
    )
}

pub fn layer_png(rle: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    let gray = decode_rle(rle, width, height)?;
    let mut buf = Vec::new();
    let mut encoder = png::Encoder::new(Cursor::new(&mut buf), width, height);
    encoder.set_color(png::ColorType::Grayscale);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(&gray).map_err(|e| e.to_string())?;
    drop(writer);
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::box_mesh;
    use crate::slice::{slice, Request, Solid};
    use std::io::Read;

    #[test]
    fn sl1_zip_contains_config_and_a_layer() {
        let mut machine = Machine::photon_m3_max();
        machine.res_x = 200;
        machine.res_y = 200;
        machine.size_x = 20.0;
        machine.size_y = 20.0;
        machine.pixel_um = 100.0;
        machine.pixel_um_y = 100.0;
        machine.rotate_180 = false;
        machine.mirror_x = false;
        machine.mirror_y = false;
        let mesh = box_mesh([2.0, 2.0, 0.0], [8.0, 8.0, 1.0]);
        let solid = Solid {
            vertices: mesh.vertices,
            indices: mesh.indices,
            hollow: None,
        };
        let mut settings = crate::printer::PrintSettings::default();
        settings.layer_mm = 0.5;
        settings.anti_alias = 1;
        let sliced = slice(Request {
            solids: &[solid],
            drains: &[],
            machine,
            settings: &settings,
            cancel: None,
            progress: None,
        })
        .unwrap();
        let path = std::env::temp_dir().join("amber-sl1-test.sl1");
        write_sl1(&path, &sliced, machine, &settings).unwrap();
        let file = std::fs::File::open(&path).unwrap();
        let mut zip = zip::ZipArchive::new(file).unwrap();
        let mut ini = String::new();
        zip.by_name("config.ini")
            .unwrap()
            .read_to_string(&mut ini)
            .unwrap();
        assert!(ini.contains("expTime = "));
        assert!(ini.contains("displayPixelsX = 200"));
        assert!(zip.by_name("00000.png").is_ok());
        let _ = std::fs::remove_file(path);
    }
}
