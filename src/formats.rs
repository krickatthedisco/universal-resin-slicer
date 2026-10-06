//! Pick the file a printer should receive, and write one of the containers
//! Amber can encode.

use crate::ctb::write_ctb;
use crate::pm3m::write_pm3m;
use crate::printer::{Machine, PrintFormat, PrintSettings};
use crate::sl1::write_sl1;
use crate::slice::Slice;
use std::path::Path;

pub fn write_print(
    path: &Path,
    slice: &Slice,
    machine: Machine,
    settings: &PrintSettings,
    format: PrintFormat,
) -> std::io::Result<()> {
    match format {
        PrintFormat::Photon516 => {
            let mut machine = machine;
            machine.file_version = 516;
            write_pm3m(path, slice, machine, settings)
        }
        PrintFormat::Ctb => write_ctb(path, slice, machine, settings),
        PrintFormat::Sl1 | PrintFormat::PngZip => write_sl1(path, slice, machine, settings),
    }
}

pub fn format_from_extension(ext: &str) -> Option<PrintFormat> {
    let ext = ext.trim_start_matches('.').to_ascii_lowercase();
    match ext.as_str() {
        "sl1" => Some(PrintFormat::Sl1),
        "ctb" => Some(PrintFormat::Ctb),
        "zip" | "cws" | "nanodlp" | "rgb" | "osf" => Some(PrintFormat::PngZip),
        "pm3m" | "pm3" | "pwma" | "pwms" | "pmsq" | "pwmb" | "pwmx" | "pwmo" | "dlp" => {
            Some(PrintFormat::Photon516)
        }
        _ => None,
    }
}

pub fn describe(machine: Machine, format: PrintFormat) -> String {
    let ext = machine.format_extension(format);
    if format == PrintFormat::PngZip && machine.reads_format(format) {
        format!(
            "A zip of one PNG per layer, saved as .{ext} for {}.",
            machine.name
        )
    } else if machine.reads_format(format) {
        format!(
            "{} reads .{ext} directly. Copy it to a USB stick and print from the machine.",
            machine.name
        )
    } else if format == PrintFormat::Sl1 || format == PrintFormat::PngZip {
        format!(
            "{} reads .{} ({}), which Amber does not encode. This .{ext} is an open layer stack.",
            machine.name, machine.printer_extension, machine.format_name
        )
    } else {
        format!(
            "Wrote .{ext}. {} itself reads .{} ({}).",
            machine.name, machine.printer_extension, machine.format_name
        )
    }
}
