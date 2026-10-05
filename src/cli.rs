//! Headless slice. Useful for checking a file before opening the window,
//! and for the test printer workflow:
//! `amber slice model.stl -o model.pm3m`

use crate::pm3m::write_pm3m;
use crate::printer::{Machine, PrintSettings};
use crate::scene::Document;
use crate::slice::{slice, Request};
use crate::supports;
use anyhow::{bail, Context, Result};
use std::path::PathBuf;

pub fn run(args: &[String]) -> Result<()> {
    let mut input: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut layer = 0.05f32;
    let mut exposure: Option<f32> = None;
    let mut supports_name = "none".to_string();
    let mut hollow = 0.0f32;
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        let mut next = || {
            i += 1;
            args.get(i)
                .cloned()
                .context(format!("missing value after {arg}"))
        };
        match arg.as_str() {
            "-o" | "--output" => output = Some(PathBuf::from(next()?)),
            "--layer" => layer = next()?.parse().context("--layer")?,
            "--exposure" => exposure = Some(next()?.parse().context("--exposure")?),
            "--supports" => supports_name = next()?,
            "--hollow" => hollow = next()?.parse().context("--hollow")?,
            "-h" | "--help" => {
                print_help();
                return Ok(());
            }
            other if !other.starts_with('-') && input.is_none() => {
                input = Some(PathBuf::from(other))
            }
            other => bail!("unknown argument {other}"),
        }
        i += 1;
    }
    let input = input.context("usage: amber slice model.stl -o model.pm3m")?;
    let output = output.unwrap_or_else(|| input.with_extension("pm3m"));
    let machine = Machine::photon_m3_max();
    let mut doc = Document::new();
    let id = doc.import(&input)?;
    doc.center_on_plate(id, machine.size_x, machine.size_y);
    doc.drop_object(id);
    if hollow > 0.0 {
        if let Some(obj) = doc.object_mut(id) {
            obj.hollow = true;
            obj.wall_mm = hollow;
        }
    }
    if supports_name != "none" {
        let index = match supports_name.to_ascii_lowercase().as_str() {
            "light" => 0,
            "medium" => 1,
            "heavy" => 2,
            other => bail!("--supports expected none, light, medium, or heavy, got {other}"),
        };
        doc.set_preset(index);
        doc.add_auto_supports(true);
    }
    let mut settings = PrintSettings::default();
    settings.layer_mm = layer;
    if let Some(exposure) = exposure {
        settings.exposure_s = exposure;
    }
    let solids = doc.solids();
    let drains = doc.drain_inputs();
    let sliced = slice(Request {
        solids: &solids,
        drains: &drains,
        machine,
        settings: &settings,
        cancel: None,
        progress: None,
    })
    .map_err(|e| anyhow::anyhow!(e))?;
    write_pm3m(&output, &sliced, machine, &settings)?;
    println!(
        "wrote {}  {} layers  {:.2} ml  {} min  {} supports",
        output.display(),
        sliced.layers.len(),
        sliced.cured_ml,
        sliced.seconds / 60,
        doc.supports.len()
    );
    let islands: usize = sliced.layers.iter().map(|l| l.islands.len()).sum();
    if islands > 0 {
        println!("{islands} island regions. Open the prepare view and use Support islands, or pass --supports medium.");
    }
    let _ = supports::PRESETS;
    Ok(())
}

fn print_help() {
    println!(
        "amber slice <model.stl|obj> -o <file.pm3m> [--layer 0.05] [--exposure 3] [--supports light|medium|heavy] [--hollow 2.0]"
    );
}
