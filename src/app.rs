//! Prepare and preview window.

use crate::catalog;
use crate::community;
use crate::formats::{self, format_from_extension};
use crate::mesh::{calibration_cube, overhang_bridge, BooleanOp};
use crate::plate::{self, PlateData};
use crate::printer::{Machine, PrintFormat, PrintSettings};
use crate::resins::{self, Resin, ResinProfile};
use crate::scene::{Document, Selection, VolumeKind};
use crate::shapes;
use crate::slice::{self, Slice};
use crate::supports::{SectionShape, PRESETS};
use crate::viewport::{self, Camera, PlateFrame, PlateView, Renderer, ViewCache};
use eframe::egui;
use glam::Vec3;
use glow::HasContext;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tool {
    Select,
    Move,
    Rotate,
    Scale,
    Mirror,
    Hollow,
    Support,
    Drain,
    Measure,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Prepare,
    Preview,
}

struct Job {
    progress: Arc<AtomicU32>,
    total: u32,
    cancel: Arc<AtomicBool>,
    done: Arc<Mutex<Option<Result<Slice, String>>>>,
    generation: u64,
    export_after: bool,
    started: std::time::Instant,
}

fn default_machine_id() -> String {
    "anycubic-photon-m3-max".into()
}

fn default_true() -> bool {
    true
}

fn default_lift() -> f32 {
    5.0
}

fn default_raft_mm() -> f32 {
    1.0
}

fn default_raft_margin() -> f32 {
    2.0
}

fn default_raft_angle() -> f32 {
    30.0
}

fn default_brace_angle() -> f32 {
    45.0
}

fn default_brace_dist() -> f32 {
    8.0
}

fn default_section_z() -> f32 {
    10.0
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum ThemeMode {
    Dark,
    Light,
}

impl Default for ThemeMode {
    fn default() -> Self {
        Self::Dark
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Scheme {
    Amber,
    Slate,
    Pine,
    Plum,
}

impl Default for Scheme {
    fn default() -> Self {
        Self::Amber
    }
}

impl Scheme {
    fn label(self) -> &'static str {
        match self {
            Self::Amber => "Amber",
            Self::Slate => "Slate",
            Self::Pine => "Pine",
            Self::Plum => "Plum",
        }
    }

    fn all() -> [Scheme; 4] {
        [Self::Amber, Self::Slate, Self::Pine, Self::Plum]
    }
}

#[derive(Serialize, Deserialize)]
struct Persist {
    settings: PrintSettings,
    rotate_180: bool,
    mirror_x: bool,
    mirror_y: bool,
    preset: usize,
    /// Older saves stored this as `raft`, which defaulted on. A skate now
    /// stays off until the user asks for one.
    #[serde(default, rename = "raft_on")]
    raft: bool,
    #[serde(default = "default_raft_mm")]
    raft_mm: f32,
    #[serde(default = "default_raft_margin")]
    raft_margin: f32,
    #[serde(default = "default_raft_angle")]
    raft_angle: f32,
    #[serde(default = "default_true")]
    braces_on: bool,
    #[serde(default = "default_brace_dist")]
    brace_dist: f32,
    #[serde(default = "default_brace_angle")]
    brace_angle: f32,
    #[serde(default = "default_lift")]
    support_lift_mm: f32,
    #[serde(default)]
    style: Option<crate::supports::SupportStyle>,
    #[serde(default)]
    platform_only: bool,
    #[serde(default = "default_machine_id")]
    machine_id: String,
    /// Hide resins that have no published time for the selected printer.
    /// Missing on older saves, so those installs start filtered.
    #[serde(default = "default_true")]
    only_profiled: bool,
    /// Simple is the first-print view. Missing on older saves, so those open in Simple.
    #[serde(default)]
    workshop: bool,
    #[serde(default)]
    recent: Vec<String>,
    #[serde(default = "default_true")]
    show_contacts: bool,
    #[serde(default = "default_true")]
    show_necks: bool,
    #[serde(default = "default_true")]
    show_trunks: bool,
    #[serde(default = "default_true")]
    show_feet: bool,
    #[serde(default = "default_true")]
    show_branches: bool,
    #[serde(default = "default_true")]
    show_braces: bool,
    #[serde(default = "default_true")]
    show_rafts: bool,
    #[serde(default)]
    section: bool,
    #[serde(default = "default_section_z")]
    section_z: f32,
    #[serde(default)]
    section_lo: f32,
    #[serde(default)]
    theme_mode: ThemeMode,
    #[serde(default)]
    dark_scheme: Scheme,
    #[serde(default)]
    light_scheme: Scheme,
}

pub struct AmberApp {
    machine: Machine,
    settings: PrintSettings,
    doc: Document,
    camera: Camera,
    tool: Tool,
    view: View,
    renderer: Option<viewport::SharedRenderer>,
    view_cache: ViewCache,
    frame: Option<PlateFrame>,
    draw_gen: u64,
    status: String,
    job: Option<Job>,
    slice: Option<Slice>,
    slice_gen: u64,
    /// Lowest island of each column from the last slice, while the models stay put.
    island_marks: Vec<(f32, f32, f32)>,
    island_stamp: u64,
    island_gen: u64,
    island_drawn: u64,
    preview_index: usize,
    preview_for: Option<usize>,
    preview_tex: Option<egui::TextureHandle>,
    gesture: bool,
    export_name: String,
    keep_original: bool,
    printer_filter: String,
    resin_filter: String,
    profile_note: String,
    only_profiled: bool,
    part_menu: Option<egui::Pos2>,
    part_menu_fresh: bool,
    show_overhangs: bool,
    copy_count: u32,
    copy_gap: f32,
    cut_z: f32,
    workshop: bool,
    recent: Vec<String>,
    /// Last `.amber` plate, so Save can overwrite it.
    plate_path: Option<PathBuf>,
    /// 0 home, 1 printer, 2 resin. Only the Simple view uses it.
    simple_page: u8,
    help_open: bool,
    measure_a: Option<Vec3>,
    measure_b: Option<Vec3>,
    measure_gen: u64,
    measure_drawn: u64,
    /// Round a dragged move to whole millimetres.
    snap_mm: bool,
    /// Support tool: remove tips near the click instead of planting one.
    erase_supports: bool,
    erase_mm: f32,
    /// True after this drag has already stored one undo snapshot.
    stroke_saved: bool,
    /// The tip whose X/Y fields already have one undo snapshot for this edit.
    tip_field_for: Option<u64>,
    plate_view: PlateView,
    /// 0 follows the printer. 1 photon, 2 ctb, 3 sl1, 4 png zip.
    export_as: u8,
    /// The user has moved a cut handle, so loading a model does not reset it.
    cuts_custom: bool,
    /// Which cut handle is being dragged. 0 is the bottom, 1 is the top.
    cut_drag: Option<u8>,
    /// `doc.changed` the last time the cuts were fitted to the models.
    cuts_fit_gen: u64,
    /// Models left out of the plate view. They still slice.
    hidden_models: HashSet<u64>,
    view_rev: u64,
    view_drawn: u64,
    /// The drain whose sizes are currently shown in the hole panel.
    hole_panel_for: Option<u64>,
    theme_mode: ThemeMode,
    dark_scheme: Scheme,
    light_scheme: Scheme,
    /// Shift-clicked models. Assemble and the booleans use this set.
    picked: HashSet<u64>,
}

impl AmberApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut machine = Machine::photon_m3_max();
        let mut settings = PrintSettings::default();
        let mut preset = 1usize;
        let mut raft = false;
        let mut raft_mm = 1.0;
        let mut raft_margin = 2.0;
        let mut raft_angle = 30.0;
        let mut braces_on = true;
        let mut brace_dist = 8.0;
        let mut brace_angle = 45.0;
        let mut support_lift_mm = 5.0;
        let mut saved_style = None;
        let mut platform_only = false;
        let mut only_profiled = true;
        let mut workshop = false;
        let mut recent = Vec::new();
        let mut plate_view = PlateView::default();
        let mut theme_mode = ThemeMode::Dark;
        let mut dark_scheme = Scheme::Amber;
        let mut light_scheme = Scheme::Amber;
        if let Some(storage) = cc.storage {
            if let Some(raw) = storage.get_string("amber.print") {
                if let Ok(saved) = serde_json::from_str::<Persist>(&raw) {
                    settings = saved.settings;
                    if let Some(profile) = catalog::find(&saved.machine_id) {
                        machine = Machine::from_profile(profile);
                    }
                    machine.rotate_180 = saved.rotate_180;
                    machine.mirror_x = saved.mirror_x;
                    machine.mirror_y = saved.mirror_y;
                    preset = saved.preset;
                    raft = saved.raft;
                    raft_mm = saved.raft_mm;
                    raft_margin = saved.raft_margin;
                    raft_angle = saved.raft_angle;
                    braces_on = saved.braces_on;
                    brace_dist = saved.brace_dist;
                    brace_angle = saved.brace_angle;
                    support_lift_mm = saved.support_lift_mm;
                    saved_style = saved.style;
                    platform_only = saved.platform_only;
                    only_profiled = saved.only_profiled;
                    workshop = saved.workshop;
                    recent = saved.recent;
                    plate_view.contacts = saved.show_contacts;
                    plate_view.necks = saved.show_necks;
                    plate_view.trunks = saved.show_trunks;
                    plate_view.feet = saved.show_feet;
                    plate_view.branches = saved.show_branches;
                    plate_view.braces = saved.show_braces;
                    plate_view.rafts = saved.show_rafts;
                    plate_view.section = saved.section;
                    plate_view.section_z = saved.section_z;
                    plate_view.section_lo = saved.section_lo;
                    theme_mode = saved.theme_mode;
                    dark_scheme = saved.dark_scheme;
                    light_scheme = saved.light_scheme;
                }
            }
        }
        let shown = if theme_mode == ThemeMode::Light {
            light_scheme
        } else {
            dark_scheme
        };
        apply_theme(&cc.egui_ctx, theme_mode, shown);
        let mut doc = Document::new();
        doc.set_preset(preset);
        if let Some(style) = saved_style {
            doc.style = style.sanitized();
        }
        doc.raft = raft;
        doc.raft_mm = raft_mm;
        doc.raft_margin = raft_margin;
        doc.raft_angle = raft_angle;
        doc.braces_on = braces_on;
        doc.brace_dist = brace_dist;
        doc.brace_angle = brace_angle;
        doc.support_lift_mm = support_lift_mm;
        doc.platform_only = platform_only;
        let plate = Vec3::new(machine.size_x, machine.size_y, machine.size_z);
        let mut gpu_note = None;
        let startup = format!(
            "{} · {} printers in the list · drop an STL, OBJ, or 3MF",
            machine.name,
            catalog::PRINTERS.len()
        );
        let renderer = cc
            .gl
            .as_ref()
            .and_then(|gl| match Renderer::new(gl.as_ref()) {
                Ok(renderer) => Some(Arc::new(Mutex::new(renderer))),
                Err(err) => {
                    gpu_note = Some(format!("OpenGL plate view failed to start: {err}"));
                    None
                }
            });
        Self {
            machine,
            settings,
            doc,
            camera: Camera::looking_at_plate(plate),
            tool: Tool::Select,
            view: View::Prepare,
            renderer,
            view_cache: ViewCache::new(),
            frame: None,
            draw_gen: 0,
            status: gpu_note.unwrap_or(startup),
            job: None,
            slice: None,
            slice_gen: 0,
            island_marks: Vec::new(),
            island_stamp: 0,
            island_gen: 0,
            island_drawn: 0,
            preview_index: 0,
            preview_for: None,
            preview_tex: None,
            gesture: false,
            export_name: format!("print.{}", machine.extension),
            keep_original: false,
            printer_filter: String::new(),
            resin_filter: String::new(),
            profile_note: String::new(),
            only_profiled,
            part_menu: None,
            part_menu_fresh: false,
            show_overhangs: false,
            copy_count: 2,
            copy_gap: 3.0,
            cut_z: 10.0,
            workshop,
            recent,
            plate_path: None,
            simple_page: 0,
            help_open: false,
            measure_a: None,
            measure_b: None,
            measure_gen: 0,
            measure_drawn: 0,
            snap_mm: false,
            erase_supports: false,
            erase_mm: 3.0,
            stroke_saved: false,
            tip_field_for: None,
            plate_view,
            export_as: 0,
            cuts_custom: false,
            cut_drag: None,
            cuts_fit_gen: 0,
            hidden_models: HashSet::new(),
            view_rev: 0,
            view_drawn: 0,
            hole_panel_for: None,
            theme_mode,
            dark_scheme,
            light_scheme,
            picked: HashSet::new(),
        }
    }

    fn plate(&self) -> Vec3 {
        Vec3::new(
            self.machine.size_x,
            self.machine.size_y,
            self.machine.size_z,
        )
    }

    fn invalidate_slice(&mut self) {
        self.slice = None;
        self.preview_tex = None;
        self.preview_for = None;
    }

    fn poll_job(&mut self, ctx: &egui::Context) {
        let Some(job) = &self.job else {
            return;
        };
        ctx.request_repaint();
        let finished = job.done.lock().ok().and_then(|mut guard| guard.take());
        let Some(result) = finished else {
            let done = job.progress.load(Ordering::Relaxed);
            let total = job.total.max(1);
            let eta = slice_eta(job.started, done, total);
            self.status = format!("Slicing layer {done} of {total}…{eta}");
            return;
        };
        let generation = job.generation;
        let export_after = job.export_after;
        self.job = None;
        match result {
            Ok(slice) if generation == self.doc.changed => {
                let layers = slice.layers.len();
                let islands: usize = slice.layers.iter().map(|l| l.islands.len()).sum();
                let minutes = slice.seconds / 60;
                let mut line = format!(
                    "Sliced {layers} layers · {:.2} ml · {minutes} min · {islands} islands",
                    slice.cured_ml
                );
                if self.settings.price_per_liter > 0.0 {
                    let cost = slice.cured_ml / 1000.0 * self.settings.price_per_liter;
                    line = format!(
                        "{line} · {cost:.2} at {:.2}/L",
                        self.settings.price_per_liter
                    );
                }
                self.status = line;
                if !slice.warnings.is_empty() {
                    self.status = format!("{} · {}", self.status, slice.warnings.join(" "));
                }
                self.island_marks = slice::island_contacts(
                    slice.layers.iter().flat_map(|layer| {
                        layer
                            .islands
                            .iter()
                            .map(|island| (island.x_mm, island.y_mm, island.z_mm))
                    }),
                    0.8,
                );
                self.island_stamp = self.model_stamp();
                self.island_gen = self.island_gen.wrapping_add(1);
                if !self.island_marks.is_empty() {
                    self.status = format!(
                        "{} · {} red island marks on the plate",
                        self.status,
                        self.island_marks.len()
                    );
                }
                self.slice = Some(slice);
                self.slice_gen = generation;
                self.preview_index = layers.saturating_sub(1);
                self.preview_for = None;
                self.view = View::Preview;
                if export_after {
                    self.export_print(None);
                }
            }
            Ok(_) => {
                self.status = "The plate changed while slicing. Slice it again.".into();
            }
            Err(err) => self.status = err,
        }
    }

    fn start_slice(&mut self, export_after: bool) {
        if self.job.is_some() {
            return;
        }
        if self.doc.objects.is_empty() && self.doc.supports.is_empty() {
            self.status = "Put a model on the plate first.".into();
            return;
        }
        let solids = self.doc.solids();
        let drains = self.doc.drain_inputs();
        let machine = self.machine;
        let settings = self.settings.clone();
        let progress = Arc::new(AtomicU32::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let done = Arc::new(Mutex::new(None));
        let generation = self.doc.changed;
        let max_z = solids
            .iter()
            .flat_map(|s| s.vertices.iter().map(|v| v[2]))
            .fold(0.0f32, f32::max);
        let total = ((max_z / settings.layer_mm.max(0.01)).ceil() as u32).max(1);
        let progress_thread = Arc::clone(&progress);
        let cancel_thread = Arc::clone(&cancel);
        let done_thread = Arc::clone(&done);
        std::thread::spawn(move || {
            let result = slice::slice(slice::Request {
                solids: &solids,
                drains: &drains,
                machine,
                settings: &settings,
                cancel: Some(cancel_thread),
                progress: Some(progress_thread),
            });
            if let Ok(mut guard) = done_thread.lock() {
                *guard = Some(result);
            }
        });
        self.job = Some(Job {
            progress,
            total,
            cancel,
            done,
            generation,
            export_after,
            started: std::time::Instant::now(),
        });
        self.status = "Slicing…".into();
    }

    fn chosen_format(&self) -> PrintFormat {
        match self.export_as {
            1 => PrintFormat::Photon516,
            2 => PrintFormat::Ctb,
            3 => PrintFormat::Sl1,
            4 => PrintFormat::PngZip,
            _ => self.machine.default_format(),
        }
    }

    fn set_export_ext(&mut self, ext: &str) {
        let stem = self
            .export_name
            .rsplit_once('.')
            .map(|(stem, _)| stem)
            .unwrap_or(self.export_name.as_str());
        self.export_name = format!("{stem}.{ext}");
    }

    fn format_combo(&mut self, ui: &mut egui::Ui) {
        let auto = self.machine.default_format();
        let mut pick = self.export_as;
        let selected = if self.export_as == 0 {
            format!("{} (this printer)", auto.label())
        } else {
            self.chosen_format().label().to_string()
        };
        egui::ComboBox::from_label("File format")
            .selected_text(selected)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut pick, 0, format!("{} (this printer)", auto.label()));
                ui.selectable_value(&mut pick, 1, PrintFormat::Photon516.label());
                ui.selectable_value(&mut pick, 2, PrintFormat::Ctb.label());
                ui.selectable_value(&mut pick, 3, PrintFormat::Sl1.label());
                ui.selectable_value(&mut pick, 4, PrintFormat::PngZip.label());
            });
        if pick != self.export_as {
            self.export_as = pick;
            let ext = self.machine.format_extension(self.chosen_format());
            self.set_export_ext(ext);
        }
    }

    fn export_print(&mut self, force: Option<PrintFormat>) {
        let Some(slice) = &self.slice else {
            self.start_slice(true);
            return;
        };
        if self.slice_gen != self.doc.changed {
            self.start_slice(true);
            return;
        }
        let chosen = force.unwrap_or_else(|| self.chosen_format());
        let ext = self.machine.format_extension(chosen);
        let name = sanitize_filename(&self.export_name, ext);
        if chosen == PrintFormat::Photon516 && name.len() > 24 {
            self.status = format!(
                "Keep the file name short. {} skips very long names on the USB stick.",
                self.machine.name
            );
        }
        let mut dialog = rfd::FileDialog::new().set_file_name(&name);
        let mut seen: Vec<&str> = Vec::new();
        for format in [
            chosen,
            PrintFormat::Photon516,
            PrintFormat::Ctb,
            PrintFormat::Sl1,
            PrintFormat::PngZip,
        ] {
            let filter_ext = self.machine.format_extension(format);
            if seen.contains(&filter_ext) {
                continue;
            }
            seen.push(filter_ext);
            dialog = dialog.add_filter(
                &format!("{} (.{filter_ext})", format.label()),
                &[filter_ext],
            );
        }
        let Some(path) = dialog.save_file() else {
            return;
        };
        let format = path
            .extension()
            .and_then(|ext| ext.to_str())
            .and_then(format_from_extension)
            .unwrap_or(chosen);
        let written = formats::write_print(&path, slice, self.machine, &self.settings, format);
        match written {
            Ok(()) => {
                self.status = format!(
                    "Wrote {} · {:.1} MB. {}",
                    path.display(),
                    slice_bytes(slice) as f64 / 1_048_576.0,
                    formats::describe(self.machine, format)
                );
            }
            Err(err) => self.status = format!("Could not write the file: {err}"),
        }
    }

    fn export_layer_png(&mut self) {
        let Some(slice) = &self.slice else {
            self.status = "Slice first, then export a layer.".into();
            return;
        };
        let layer = &slice.layers[self.preview_index.min(slice.layers.len().saturating_sub(1))];
        let Ok((w, h, rgba)) =
            slice::preview_rgba(&layer.rle, slice.width, slice.height, 1600, &layer.islands)
        else {
            self.status = "Could not decode that layer.".into();
            return;
        };
        let Some(path) = rfd::FileDialog::new()
            .set_file_name(format!("layer_{:04}.png", layer.index))
            .add_filter("PNG", &["png"])
            .save_file()
        else {
            return;
        };
        match write_png(&path, w, h, &rgba) {
            Ok(()) => self.status = format!("Wrote {}", path.display()),
            Err(err) => self.status = err,
        }
    }

    fn import_path(&mut self, path: PathBuf) {
        if path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("amber"))
        {
            self.open_plate_file(path);
            return;
        }
        match self.doc.import(&path) {
            Ok(id) => {
                self.doc
                    .center_on_plate(id, self.machine.size_x, self.machine.size_y);
                self.doc.drop_object(id);
                self.export_name = default_export_name(&self.doc, self.machine.extension);
                self.invalidate_slice();
                self.remember_recent(&path);
                self.status = format!("Imported {}", path.display());
                self.view = View::Prepare;
            }
            Err(err) => self.status = err.to_string(),
        }
    }

    fn plate_data(&self) -> PlateData {
        PlateData {
            machine_id: self.machine.id.to_string(),
            rotate_180: self.machine.rotate_180,
            mirror_x: self.machine.mirror_x,
            mirror_y: self.machine.mirror_y,
            settings: self.settings.clone(),
            objects: self.doc.objects.clone(),
            supports: self.doc.supports.clone(),
            drains: self.doc.drains.clone(),
        }
    }

    fn save_plate(&mut self, ask: bool) {
        let path = if ask || self.plate_path.is_none() {
            let name = self
                .plate_path
                .as_ref()
                .and_then(|path| path.file_name())
                .and_then(|name| name.to_str())
                .unwrap_or("plate.amber")
                .to_string();
            let Some(path) = rfd::FileDialog::new()
                .set_file_name(name)
                .add_filter("Amber plate", &["amber"])
                .save_file()
            else {
                return;
            };
            with_amber_extension(path)
        } else {
            self.plate_path
                .clone()
                .unwrap_or_else(|| PathBuf::from("plate.amber"))
        };
        match plate::write_plate(&path, &self.plate_data()) {
            Ok(()) => {
                self.plate_path = Some(path.clone());
                self.remember_recent(&path);
                self.status = format!(
                    "Saved the plate to {}. Open it later with File → Open plate.",
                    path.display()
                );
            }
            Err(err) => self.status = err.to_string(),
        }
    }

    fn open_plate_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Amber plate", &["amber"])
            .pick_file()
        {
            self.open_plate_file(path);
        }
    }

    fn open_plate_file(&mut self, path: PathBuf) {
        let data = match plate::read_plate(&path) {
            Ok(data) => data,
            Err(err) => {
                self.status = err.to_string();
                return;
            }
        };
        let count = data.objects.len();
        let supports = data.supports.len();
        if let Some(profile) = catalog::find(&data.machine_id) {
            let rotate = data.rotate_180;
            let mirror_x = data.mirror_x;
            let mirror_y = data.mirror_y;
            self.machine = Machine::from_profile(profile);
            self.export_as = 0;
            self.machine.rotate_180 = rotate;
            self.machine.mirror_x = mirror_x;
            self.machine.mirror_y = mirror_y;
        } else {
            self.status = format!(
                "Opened the models. Printer {} is not in this list, so the printer stays {}.",
                data.machine_id, self.machine.name
            );
        }
        self.settings = data.settings;
        self.doc
            .load_plate(data.objects, data.supports, data.drains);
        self.export_name = default_export_name(&self.doc, self.machine.extension);
        self.camera = Camera::looking_at_plate(self.plate());
        self.invalidate_slice();
        self.plate_path = Some(path.clone());
        self.remember_recent(&path);
        self.view = View::Prepare;
        self.simple_page = 0;
        if catalog::find(&data.machine_id).is_some() {
            self.status = format!(
                "Opened {} · {count} models · {supports} supports. Slice again before you print.",
                path.display()
            );
        }
    }

    fn open_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Meshes", &["stl", "obj", "3mf", "STL", "OBJ", "3MF"])
            .pick_file()
        {
            self.import_path(path);
        }
    }

    fn add_builtin(&mut self, kind: &str) {
        let plate = self.plate();
        let (name, mesh) = if kind == "cube" {
            ("20 mm cube".to_string(), calibration_cube(plate.x, plate.y))
        } else {
            (
                "Overhang bridge".to_string(),
                overhang_bridge(plate.x, plate.y),
            )
        };
        let id = self.doc.add_mesh(name, mesh);
        self.picked.clear();
        self.picked.insert(id);
        self.note_added();
    }

    fn add_shape(&mut self, name: &str, mesh: crate::mesh::Mesh) {
        let mut mesh = mesh;
        let plate = self.plate();
        fit_on_plate(&mut mesh, plate);
        for v in &mut mesh.vertices {
            v[0] += plate.x * 0.5;
            v[1] += plate.y * 0.5;
        }
        let id = self.doc.add_mesh(name.to_string(), mesh);
        self.picked.clear();
        self.picked.insert(id);
        self.note_added();
    }

    fn note_added(&mut self) {
        self.export_name = default_export_name(&self.doc, self.machine.extension);
        self.invalidate_slice();
        self.view = View::Prepare;
    }

    fn shown_scheme(&self) -> Scheme {
        if self.theme_mode == ThemeMode::Light {
            self.light_scheme
        } else {
            self.dark_scheme
        }
    }

    fn theme_menu(&mut self, ui: &mut egui::Ui) {
        ui.label("Theme");
        ui.horizontal(|ui| {
            if ui
                .selectable_label(self.theme_mode == ThemeMode::Dark, "Dark")
                .clicked()
            {
                self.theme_mode = ThemeMode::Dark;
            }
            if ui
                .selectable_label(self.theme_mode == ThemeMode::Light, "Light")
                .clicked()
            {
                self.theme_mode = ThemeMode::Light;
            }
        });
        ui.label("Dark scheme");
        self.scheme_row(ui, true);
        ui.label("Light scheme");
        self.scheme_row(ui, false);
        ui.label("Each mode keeps the scheme you pick for it.");
    }

    fn scheme_row(&mut self, ui: &mut egui::Ui, dark: bool) {
        ui.horizontal_wrapped(|ui| {
            for scheme in Scheme::all() {
                let on = if dark {
                    self.dark_scheme == scheme
                } else {
                    self.light_scheme == scheme
                };
                if ui.selectable_label(on, scheme.label()).clicked() {
                    if dark {
                        self.dark_scheme = scheme;
                    } else {
                        self.light_scheme = scheme;
                    }
                }
            }
        });
    }

    fn pick_model(&mut self, id: u64, shift: bool) {
        if shift {
            if self.picked.contains(&id) && self.picked.len() > 1 {
                self.picked.remove(&id);
            } else {
                self.picked.insert(id);
            }
        } else {
            self.picked.clear();
            self.picked.insert(id);
        }
        self.doc.selection = Selection::Object(id);
        self.doc.touch_xform();
    }

    fn picked_ids(&self) -> Vec<u64> {
        let mut ids: Vec<u64> = self
            .picked
            .iter()
            .copied()
            .filter(|id| self.doc.object(*id).is_some())
            .collect();
        if let Selection::Object(id) = self.doc.selection {
            if self.doc.object(id).is_some() && !ids.contains(&id) {
                ids.push(id);
            }
        }
        ids.sort_unstable();
        ids
    }

    fn arrange_rerf(&mut self, id: u64) {
        let plate = self.plate();
        match self.doc.spread_on_rerf(id, plate.x, plate.y, plate.z) {
            Ok(spread) => {
                self.picked.clear();
                self.picked.insert(id);
                self.invalidate_slice();
                let anycubic = self.machine.vendor.eq_ignore_ascii_case("Anycubic");
                if anycubic {
                    self.export_name = format!("R_E_R_F.{}", self.machine.extension);
                }
                let scale = if spread.scale < 0.99 {
                    format!(
                        " Each copy was scaled to {:.0}% so it fits a zone.",
                        spread.scale * 100.0
                    )
                } else {
                    String::new()
                };
                let exposure = self.settings.exposure_s;
                self.status = if anycubic {
                    format!(
                        "Placed {} copies in a 4 by 2 grid, each one centered in its box. Zone 1 is the front-left box and uses the normal exposure ({exposure:.2} s). Save as R_E_R_F so the printer steps the time, often by 0.25 s. Rotate 180° can swap which corner that is. Check this machine's manual.{scale}",
                        spread.count
                    )
                } else {
                    format!(
                        "Placed {} copies in a 4 by 2 grid, each one centered in its box. {} exposes every zone the same ({exposure:.2} s).{scale}",
                        spread.count, self.machine.name
                    )
                };
            }
            Err(err) => self.status = err.into(),
        }
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let delete =
            ctx.input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace));
        let undo =
            ctx.input(|i| i.key_pressed(egui::Key::Z) && i.modifiers.command && !i.modifiers.shift);
        let redo = ctx.input(|i| {
            i.modifiers.command
                && (i.key_pressed(egui::Key::Y)
                    || (i.key_pressed(egui::Key::Z) && i.modifiers.shift))
        });
        let duplicate = ctx.input(|i| i.key_pressed(egui::Key::D) && i.modifiers.command);
        let open = ctx.input(|i| i.key_pressed(egui::Key::O) && i.modifiers.command);
        let slice_now = ctx.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.command);
        let save_plate =
            ctx.input(|i| i.key_pressed(egui::Key::S) && i.modifiers.command && i.modifiers.shift);
        let save =
            ctx.input(|i| i.key_pressed(egui::Key::S) && i.modifiers.command && !i.modifiers.shift);
        let help = ctx.input(|i| i.key_pressed(egui::Key::F1));
        let fit = ctx.input(|i| i.key_pressed(egui::Key::F));
        if help {
            self.help_open = true;
        }
        if fit {
            self.fit_view();
        }
        if open {
            self.open_dialog();
        }
        if slice_now && self.job.is_none() {
            self.start_slice(false);
        }
        if save_plate && self.job.is_none() {
            self.save_plate(false);
        }
        if save && self.job.is_none() {
            self.export_print(None);
        }
        if undo {
            self.undo_edit();
        }
        if redo {
            self.redo_edit();
        }
        if delete {
            self.doc.delete_selection();
            self.invalidate_slice();
        }
        if let Selection::Object(id) = self.doc.selection {
            if duplicate {
                self.doc.duplicate(id);
                self.invalidate_slice();
            }
        }
        if self.view == View::Prepare {
            let step = if ctx.input(|i| i.modifiers.shift) {
                0.1
            } else {
                1.0
            };
            let dx = if ctx.input(|i| i.key_pressed(egui::Key::ArrowLeft)) {
                -step
            } else if ctx.input(|i| i.key_pressed(egui::Key::ArrowRight)) {
                step
            } else {
                0.0
            };
            let dy = if ctx.input(|i| i.key_pressed(egui::Key::ArrowDown)) {
                -step
            } else if ctx.input(|i| i.key_pressed(egui::Key::ArrowUp)) {
                step
            } else {
                0.0
            };
            if dx != 0.0 || dy != 0.0 {
                match self.doc.selection {
                    Selection::Object(id) => {
                        self.tip_field_for = None;
                        self.doc.push_xform_undo(id);
                        if let Some(obj) = self.doc.object_mut(id) {
                            obj.position.x += dx;
                            obj.position.y += dy;
                        }
                        self.doc.touch_xform();
                        self.invalidate_slice();
                    }
                    Selection::Support(id) => {
                        let tip = self.doc.supports.iter().find(|s| s.id == id).copied();
                        if let Some(tip) = tip {
                            let x = tip.x + dx;
                            let y = tip.y + dy;
                            if self.doc.tip_can_land(id, x, y) {
                                self.doc.remember_supports();
                                self.tip_field_for = None;
                                let _ = self.doc.reseat_support(id, x, y);
                                self.invalidate_slice();
                                self.status =
                                    "Moved that tip along the model. Undo puts it back.".into();
                            } else {
                                self.status =
                                    "That nudge leaves the model, so the tip stayed put.".into();
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        if self.view == View::Preview {
            if let Some(slice) = &self.slice {
                let count = slice.layers.len();
                if count > 0 {
                    let up = ctx.input(|i| {
                        i.key_pressed(egui::Key::ArrowUp) || i.key_pressed(egui::Key::ArrowRight)
                    });
                    let down = ctx.input(|i| {
                        i.key_pressed(egui::Key::ArrowDown) || i.key_pressed(egui::Key::ArrowLeft)
                    });
                    if up {
                        self.preview_index = (self.preview_index + 1).min(count - 1);
                    }
                    if down {
                        self.preview_index = self.preview_index.saturating_sub(1);
                    }
                }
            }
        }
    }
}

impl eframe::App for AmberApp {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        let saved = Persist {
            settings: self.settings.clone(),
            rotate_180: self.machine.rotate_180,
            mirror_x: self.machine.mirror_x,
            mirror_y: self.machine.mirror_y,
            preset: self.doc.preset,
            raft: self.doc.raft,
            raft_mm: self.doc.raft_mm,
            raft_margin: self.doc.raft_margin,
            raft_angle: self.doc.raft_angle,
            braces_on: self.doc.braces_on,
            brace_dist: self.doc.brace_dist,
            brace_angle: self.doc.brace_angle,
            support_lift_mm: self.doc.support_lift_mm,
            style: Some(self.doc.style),
            platform_only: self.doc.platform_only,
            machine_id: self.machine.id.to_string(),
            only_profiled: self.only_profiled,
            workshop: self.workshop,
            recent: self.recent.clone(),
            show_contacts: self.plate_view.contacts,
            show_necks: self.plate_view.necks,
            show_trunks: self.plate_view.trunks,
            show_feet: self.plate_view.feet,
            show_branches: self.plate_view.branches,
            show_braces: self.plate_view.braces,
            show_rafts: self.plate_view.rafts,
            section: self.plate_view.section,
            section_z: self.plate_view.section_z,
            section_lo: self.plate_view.section_lo,
            theme_mode: self.theme_mode,
            dark_scheme: self.dark_scheme,
            light_scheme: self.light_scheme,
        };
        if let Ok(raw) = serde_json::to_string(&saved) {
            storage.set_string("amber.print", raw);
        }
    }

    fn on_exit(&mut self, gl: Option<&glow::Context>) {
        if let (Some(gl), Some(renderer)) = (gl, &self.renderer) {
            if let Ok(gpu) = renderer.lock() {
                gpu.destroy(gl);
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        apply_theme(&ctx, self.theme_mode, self.shown_scheme());
        self.poll_job(&ctx);
        self.handle_keys(&ctx);
        if !ctx.input(|i| i.pointer.primary_down() || i.pointer.secondary_down()) {
            self.gesture = false;
            self.stroke_saved = false;
        }
        self.sync_islands();
        for file in ctx.input(|i| i.raw.dropped_files.clone()) {
            self.import_path(file.path().to_path_buf());
        }
        if !self.cuts_custom && self.cuts_fit_gen != self.doc.changed {
            if self.model_z_span().is_some() {
                self.fit_cuts_to_models();
            }
            self.cuts_fit_gen = self.doc.changed;
        }
        if self.draw_gen != self.doc.changed
            || self.frame.is_none()
            || self.measure_drawn != self.measure_gen
            || self.view_drawn != self.view_rev
            || self.island_drawn != self.island_gen
        {
            let mut frame = self.view_cache.frame(
                &self.doc,
                self.plate(),
                self.doc.selection,
                &self.plate_view,
                &self.hidden_models,
            );
            self.paint_measure(&mut frame);
            self.paint_islands(&mut frame);
            frame.line_gen = frame
                .line_gen
                .wrapping_mul(31)
                .wrapping_add(self.measure_gen)
                .wrapping_mul(31)
                .wrapping_add(self.view_rev)
                .wrapping_mul(31)
                .wrapping_add(self.island_gen);
            self.frame = Some(frame);
            self.draw_gen = self.doc.changed;
            self.measure_drawn = self.measure_gen;
            self.view_drawn = self.view_rev;
            self.island_drawn = self.island_gen;
        }

        egui::Panel::top("menu").show(ui, |ui| self.menu(ui));
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        egui::Panel::left("tools")
            .resizable(false)
            .exact_size(104.0)
            .show(ui, |ui| self.tool_rail(ui));
        egui::Panel::right("props")
            .resizable(true)
            .default_size(332.0)
            .show(ui, |ui| self.props_panel(ui));
        egui::CentralPanel::default().show(ui, |ui| self.center(ui));
        self.show_part_menu(&ctx);
        self.show_help(&ctx);
    }
}

impl AmberApp {
    fn menu(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.menu_button("File", |ui| {
                if ui.button("Open STL, OBJ, or 3MF…").clicked() {
                    self.open_dialog();
                    ui.close();
                }
                if ui.button("Open plate…").clicked() {
                    self.open_plate_dialog();
                    ui.close();
                }
                if ui.button("Save plate…").clicked() {
                    self.save_plate(true);
                    ui.close();
                }
                if !self.recent.is_empty() {
                    ui.separator();
                    ui.label("Recent");
                    let recent = self.recent.clone();
                    for path in recent {
                        let label = std::path::Path::new(&path)
                            .file_name()
                            .and_then(|s| s.to_str())
                            .unwrap_or(&path)
                            .to_string();
                        if ui.button(label).clicked() {
                            self.import_path(PathBuf::from(path));
                            ui.close();
                        }
                    }
                }
                ui.menu_button("Add a useful object", |ui| {
                    if ui
                        .button("#3DBenchy")
                        .on_hover_text("The public-domain boat from 3dbenchy.com. CC0.")
                        .clicked()
                    {
                        self.add_shape("3DBenchy", shapes::benchy());
                        ui.close();
                    }
                    if ui.button("20 mm cube").clicked() {
                        self.add_builtin("cube");
                        ui.close();
                    }
                    if ui.button("Overhang bridge").clicked() {
                        self.add_builtin("bridge");
                        ui.close();
                    }
                    if ui
                        .button("Drain cup")
                        .on_hover_text("A cup with a floor, for a hollow and a drain hole.")
                        .clicked()
                    {
                        self.add_shape("Drain cup", shapes::drain_cup());
                        ui.close();
                    }
                });
                ui.menu_button("Add a primitive", |ui| {
                    let shapes: [(&str, fn() -> crate::mesh::Mesh); 12] = [
                        ("Cube", || shapes::cube(20.0)),
                        ("Sphere", || shapes::sphere(10.0)),
                        ("Hemisphere", || shapes::hemisphere(10.0)),
                        ("Cylinder", || shapes::cylinder(8.0, 20.0)),
                        ("Cone", || shapes::cone(10.0, 20.0)),
                        ("Pyramid", || shapes::pyramid(16.0, 16.0)),
                        ("Torus", || shapes::torus(10.0, 3.0)),
                        ("Tube", || shapes::tube(10.0, 7.0, 20.0, 32)),
                        ("Capsule", || shapes::capsule(6.0, 16.0)),
                        ("Wedge", || shapes::wedge(16.0, 12.0, 10.0)),
                        ("Hex prism", || shapes::hex_prism(12.0, 16.0)),
                        ("Slab", || shapes::slab(30.0, 20.0, 2.0)),
                    ];
                    for (name, make) in shapes {
                        if ui.button(name).clicked() {
                            self.add_shape(name, make());
                            ui.close();
                        }
                    }
                });
                ui.menu_button("Calibration models", |ui| {
                    ui.label("These pages have the real exposure tests. Amber does not ship the files.");
                    if ui
                        .button("AmeraLabs Town…")
                        .on_hover_text("Download the town, then right-click it and choose Arrange on the RERF grid.")
                        .clicked()
                    {
                        ui.ctx().open_url(egui::OpenUrl::new_tab(
                            "https://ameralabs.com/blog/town-calibration-part/",
                        ));
                        ui.close();
                    }
                    if ui
                        .button("Cones of Calibration…")
                        .on_hover_text("Tableflip Foundry. The download page is the license.")
                        .clicked()
                    {
                        ui.ctx().open_url(egui::OpenUrl::new_tab(
                            "https://www.tableflipfoundry.com/3d-printing/the-cones-of-calibration-v3/",
                        ));
                        ui.close();
                    }
                    if ui
                        .button("Photonsters XP2 matrix…")
                        .on_hover_text("The Validation Matrix STL is on the GitHub release. That repository has no license that lets Amber ship the file.")
                        .clicked()
                    {
                        ui.ctx().open_url(egui::OpenUrl::new_tab(
                            "https://github.com/Photonsters/Resin-exposure-finder-v2/releases",
                        ));
                        ui.close();
                    }
                });
                if ui.button("Quit").clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button("Edit", |ui| {
                if ui.button("Undo").clicked() {
                    self.undo_edit();
                    ui.close();
                }
                if ui.button("Redo").clicked() {
                    self.redo_edit();
                    ui.close();
                }
                if ui.button("Duplicate").clicked() {
                    if let Selection::Object(id) = self.doc.selection {
                        self.doc.duplicate(id);
                        self.invalidate_slice();
                    }
                    ui.close();
                }
                if ui.button("Delete").clicked() {
                    self.doc.delete_selection();
                    self.invalidate_slice();
                    ui.close();
                }
            });
            ui.menu_button("View", |ui| {
                if ui.button("Home").clicked() {
                    self.camera = Camera::looking_at_plate(self.plate());
                    ui.close();
                }
                if ui.button("Top").clicked() {
                    self.camera.pitch = 89.0;
                    self.camera.yaw = 0.0;
                    ui.close();
                }
                if ui.button("Front").clicked() {
                    self.camera.pitch = 8.0;
                    self.camera.yaw = 0.0;
                    ui.close();
                }
                if ui.button("Below the bed").clicked() {
                    self.camera.pitch = -55.0;
                    ui.close();
                }
                ui.checkbox(&mut self.show_overhangs, "Show overhangs");
                if ui.button("Right").clicked() {
                    self.camera.pitch = 8.0;
                    self.camera.yaw = 90.0;
                    ui.close();
                }
                if ui.button("Fit to selection").clicked() {
                    self.fit_view();
                    ui.close();
                }
                ui.separator();
                self.section_controls(ui);
                ui.separator();
                self.support_visibility(ui);
                ui.separator();
                self.theme_menu(ui);
            });
            ui.menu_button("Setting", |ui| {
                let mut changed = false;
                changed |= ui
                    .checkbox(&mut self.machine.rotate_180, "Rotate exposure 180°")
                    .changed();
                changed |= ui
                    .checkbox(&mut self.machine.mirror_x, "Mirror X")
                    .changed();
                changed |= ui
                    .checkbox(&mut self.machine.mirror_y, "Mirror Y")
                    .changed();
                if changed {
                    self.invalidate_slice();
                }
            });
            ui.menu_button("Slice", |ui| {
                if ui.button("Slice").clicked() {
                    self.start_slice(false);
                    ui.close();
                }
                if ui.button("Slice and save…").clicked() {
                    self.start_slice(true);
                    ui.close();
                }
                let export_label = format!(
                    "Export .{}…",
                    self.machine.format_extension(self.chosen_format())
                );
                if ui.button(export_label).clicked() {
                    self.export_print(None);
                    ui.close();
                }
                if self.chosen_format() != PrintFormat::Sl1 && ui.button("Export .sl1…").clicked()
                {
                    self.export_print(Some(PrintFormat::Sl1));
                    ui.close();
                }
                if ui.button("Export preview PNG…").clicked() {
                    self.export_layer_png();
                    ui.close();
                }
            });
            ui.menu_button("Help", |ui| {
                if ui.button("How to print").clicked() {
                    self.help_open = true;
                    ui.close();
                }
                if ui.button("Buy me a coffee").clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(
                        "https://buymeacoffee.com/krickatthedisco",
                    ));
                    ui.close();
                }
            });
            ui.separator();
            if ui.selectable_label(!self.workshop, "Simple").clicked() {
                self.workshop = false;
                self.simple_page = 0;
            }
            if ui.selectable_label(self.workshop, "Workshop").clicked() {
                self.workshop = true;
            }
            if ui
                .selectable_label(self.view == View::Prepare, "Prepare")
                .clicked()
            {
                self.view = View::Prepare;
            }
            if ui
                .selectable_label(self.view == View::Preview, "Preview")
                .clicked()
            {
                self.view = View::Preview;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let slicing = self.job.is_some();
                if ui
                    .add_enabled(!slicing, egui::Button::new("Export"))
                    .clicked()
                {
                    self.export_print(None);
                }
                let slice = egui::Button::new("Slice").fill(egui::Color32::from_rgb(224, 122, 47));
                if ui.add_enabled(!slicing, slice).clicked() {
                    self.start_slice(false);
                }
            });
        });
    }

    fn tool_rail(&mut self, ui: &mut egui::Ui) {
        ui.spacing_mut().button_padding = egui::vec2(4.0, 8.0);
        for (tool, label, tip) in [
            (
                Tool::Select,
                "Select",
                "Click a model. Left-drag moves it in X and Y. Right-drag orbits, including under the bed. A right-click opens the model menu. Shift-drag or middle-drag pans.",
            ),
            (
                Tool::Move,
                "Move",
                "Drag a model on the plate. Hold Shift to lift it.",
            ),
            (Tool::Rotate, "Rotate", "Drag to turn the selected model."),
            (Tool::Scale, "Scale", "Drag to scale the selected model."),
            (
                Tool::Mirror,
                "Mirror",
                "Mirror the selected model. Turn on Keep original to leave a copy.",
            ),
            (
                Tool::Hollow,
                "Hollow",
                "Hollow the selected model. The inside stays empty so resin can drain.",
            ),
            (
                Tool::Drain,
                "Hole",
                "Click the outside of a hollow to punch a drain.",
            ),
            (
                Tool::Support,
                "Support",
                "Click an underside to plant a support. Drag a tip to move it. Cut the view to reach a hidden surface. Orbit under the bed and the plate turns clear.",
            ),
            (
                Tool::Measure,
                "Measure",
                "Click two points. Amber shows the distance in millimetres.",
            ),
        ] {
            let on = self.tool == tool;
            let button = egui::Button::new(label).min_size(egui::vec2(88.0, 32.0));
            let response = ui.add(if on {
                button.fill(egui::Color32::from_rgb(176, 92, 32))
            } else {
                button
            });
            if response.on_hover_text(tip).clicked() {
                self.tool = tool;
            }
        }
    }

    fn next_step(&self) -> String {
        if self.job.is_some() {
            return "Slicing. The bar shows how far along it is.".into();
        }
        if self.doc.objects.is_empty() {
            return "Next: open a model, or drop an STL, OBJ, or 3MF on the plate.".into();
        }
        if self.doc.outside_plate(self.plate()) {
            return "Next: move the model back onto the plate.".into();
        }
        if let Some(name) = self.doc.hollow_without_drain() {
            return format!("Next: punch a hole in {name} so resin can drain.");
        }
        let floating = self.doc.objects.iter().any(|obj| {
            Document::display_bounds(obj).is_some_and(|(min, _)| min.z > 0.4)
                && !self
                    .doc
                    .supports
                    .iter()
                    .any(|support| support.object_id == obj.id)
        });
        if floating {
            return "Next: add supports, or put the model on the bed.".into();
        }
        if self.slice.is_none() {
            return "Next: slice, then save the file the printer reads.".into();
        }
        if self.slice_gen != self.doc.changed {
            return "Next: slice again. The plate changed after the last slice.".into();
        }
        if !self.island_marks.is_empty() && self.model_stamp() == self.island_stamp {
            return "Next: the red marks are islands. Click one with Support, then slice again."
                .into();
        }
        "Next: save the file, copy it to a USB stick, and print it from the printer.".into()
    }

    fn undo_edit(&mut self) {
        self.tip_field_for = None;
        if self.doc.undo() {
            self.invalidate_slice();
            self.status = "Undid the last edit.".into();
        } else {
            self.status = "Nothing to undo.".into();
        }
    }

    fn redo_edit(&mut self) {
        self.tip_field_for = None;
        if self.doc.redo() {
            self.invalidate_slice();
            self.status = "Redid that edit.".into();
        } else {
            self.status = "Nothing to redo.".into();
        }
    }

    fn status_bar(&self, ui: &mut egui::Ui) {
        ui.label(egui::RichText::new(self.next_step()).strong());
        ui.horizontal(|ui| {
            let tris: usize = self
                .doc
                .objects
                .iter()
                .map(|o| o.mesh.triangle_count())
                .sum();
            ui.label(format!(
                "{} models · {tris} triangles · {} supports · {} drains",
                self.doc.objects.len(),
                self.doc.supports.len(),
                self.doc.drains.len()
            ));
            if self.doc.outside_plate(self.plate()) {
                ui.colored_label(egui::Color32::from_rgb(220, 120, 90), "Outside the plate");
            }
            if let Some(overlap) = self.doc.overlap_warning() {
                ui.colored_label(egui::Color32::from_rgb(220, 120, 90), overlap);
            }
            ui.separator();
            ui.label(&self.status);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(format!("Amber {}", crate::VERSION));
            });
        });
    }

    fn simple_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Let's print");
        ui.label(egui::RichText::new(self.next_step()).strong());
        ui.label("Simple keeps the path short. Workshop, in the top bar, has every control.");
        if self.simple_page == 1 {
            if ui.button("Back").clicked() {
                self.simple_page = 0;
            }
            ui.strong("Printer");
            ui.add(
                egui::TextEdit::singleline(&mut self.printer_filter)
                    .hint_text("Search printers")
                    .desired_width(f32::INFINITY),
            );
            let filter = self.printer_filter.to_ascii_lowercase();
            let current = self.machine.id;
            let mut pick = None;
            egui::ScrollArea::vertical()
                .id_salt("simple-printers")
                .max_height(360.0)
                .show(ui, |ui| {
                    for printer in catalog::PRINTERS {
                        if !filter.is_empty()
                            && !printer.name.to_ascii_lowercase().contains(&filter)
                            && !printer.vendor.to_ascii_lowercase().contains(&filter)
                        {
                            continue;
                        }
                        if ui
                            .selectable_label(printer.id == current, printer.name)
                            .clicked()
                        {
                            pick = Some(printer.id);
                        }
                    }
                });
            if let Some(id) = pick {
                self.select_printer(id);
                self.simple_page = 0;
            }
            return;
        }
        if self.simple_page == 2 {
            if ui.button("Back").clicked() {
                self.simple_page = 0;
            }
            ui.strong("Resin");
            ui.checkbox(
                &mut self.only_profiled,
                "Only resins with settings for this printer",
            );
            ui.add(
                egui::TextEdit::singleline(&mut self.resin_filter)
                    .hint_text("Search resins")
                    .desired_width(f32::INFINITY),
            );
            let filter = self.resin_filter.to_ascii_lowercase();
            let machine_id = self.machine.id;
            let mut with_profile = std::collections::HashSet::new();
            for profile in resins::PROFILES.iter().chain(community::PROFILES.iter()) {
                if profile.machine_id == machine_id {
                    with_profile.insert(profile.resin_id);
                }
            }
            let mut pick = None;
            egui::ScrollArea::vertical()
                .id_salt("simple-resins")
                .max_height(360.0)
                .show(ui, |ui| {
                    for resin in resin_catalog() {
                        if self.only_profiled && !with_profile.contains(resin.id) {
                            continue;
                        }
                        let blob = format!("{} {}", resin.vendor, resin.name);
                        if !filter.is_empty() && !blob.to_ascii_lowercase().contains(&filter) {
                            continue;
                        }
                        if ui
                            .selectable_label(resin.name == self.settings.resin, blob)
                            .clicked()
                        {
                            pick = Some(resin.name);
                        }
                    }
                });
            if let Some(name) = pick {
                self.apply_resin(name);
                self.simple_page = 0;
            }
            return;
        }
        ui.label(format!("Printer: {}", self.machine.name));
        if ui.button("Change printer").clicked() {
            self.simple_page = 1;
        }
        ui.label(format!("Resin: {}", self.settings.resin));
        if ui.button("Change resin").clicked() {
            self.simple_page = 2;
        }
        ui.separator();
        if self.doc.objects.is_empty() {
            ui.label("Start with a model. Drop an STL, OBJ, or 3MF on the window, or open one.");
            if ui.button("Open a model…").clicked() {
                self.open_dialog();
            }
            if ui.button("Add a 20 mm test cube").clicked() {
                self.add_builtin("cube");
            }
            return;
        }
        self.model_list(ui);
        ui.separator();
        self.section_controls(ui);
        ui.horizontal_wrapped(|ui| {
            if ui.button("Show only the contact points").clicked() {
                self.plate_view.tips_only();
                self.touch_view();
            }
            if ui.button("Show all pieces").clicked() {
                self.plate_view.show_all_pieces();
                self.touch_view();
            }
        });
        ui.separator();
        ui.strong("Before you print");
        for line in self.readiness() {
            ui.label(line);
        }
        ui.separator();
        let id = self.doc.edit_target();
        if let Some(id) = id {
            if ui.button("Put it on the bed").clicked() {
                self.doc.push_xform_undo(id);
                self.doc.drop_object(id);
                self.invalidate_slice();
            }
            let hollow = self.doc.object(id).is_some_and(|obj| obj.hollow);
            if hollow {
                let mut wall = self.doc.object(id).map(|obj| obj.wall_mm).unwrap_or(2.0);
                if drag_f32(ui, "Wall thickness", &mut wall, 0.05, 0.4, 8.0, "mm") {
                    if let Some(obj) = self.doc.object_mut(id) {
                        obj.wall_mm = wall;
                    }
                    self.doc.touch_xform();
                    self.invalidate_slice();
                }
            }
        }
        ui.add_space(8.0);
        ui.label("File name on the USB stick");
        self.format_combo(ui);
        ui.text_edit_singleline(&mut self.export_name);
        ui.label("Keep it short. The Photon skips a very long name.");
        if ui.button("Save this plate…").clicked() {
            self.save_plate(true);
        }
        ui.label("That keeps the models and supports. It is not the file the printer reads.");
        let slicing = self.job.is_some();
        let save = egui::Button::new("Slice and save…").fill(egui::Color32::from_rgb(224, 122, 47));
        if ui.add_enabled(!slicing, save).clicked() {
            self.start_slice(true);
        }
        ui.collapsing("Layer and exposure", |ui| {
            let mut changed = false;
            changed |= drag_f32(
                ui,
                "Layer height",
                &mut self.settings.layer_mm,
                0.005,
                0.01,
                0.2,
                "mm",
            );
            changed |= drag_f32(
                ui,
                "Exposure",
                &mut self.settings.exposure_s,
                0.05,
                0.5,
                20.0,
                "s",
            );
            changed |= drag_f32(
                ui,
                "Bottom exposure",
                &mut self.settings.bottom_exposure_s,
                0.5,
                5.0,
                80.0,
                "s",
            );
            if changed {
                self.invalidate_slice();
            }
            ui.label(
                "These come from the resin you picked. Change them only if a test print says so.",
            );
        });
    }

    fn readiness(&self) -> Vec<String> {
        let mut lines = Vec::new();
        let tall = self
            .doc
            .objects
            .iter()
            .filter_map(Document::display_bounds)
            .map(|(_, max)| max.z)
            .fold(0.0f32, f32::max);
        if tall > 0.0 {
            let layers = ((tall / self.settings.layer_mm.max(0.01)).ceil() as u32).max(1);
            lines.push(format!(
                "About {layers} layers, {tall:.0} mm tall, on {}.",
                self.machine.name
            ));
        }
        if self.doc.outside_plate(self.plate()) {
            lines.push("A model hangs off the plate. Drag it back on.".into());
        } else {
            lines.push("Everything sits on the plate.".into());
        }
        if let Some(overlap) = self.doc.overlap_warning() {
            lines.push(format!("{overlap}. Separate them before you slice."));
        }
        if let Some(name) = self.doc.hollow_without_drain() {
            lines.push(format!(
                "{name} is hollow and has no hole. Punch one at the bottom so resin can drain."
            ));
        }
        if self.slice.is_none() {
            lines.push("Not sliced yet. Slice and save writes the file the printer reads.".into());
        } else if self.slice_gen != self.doc.changed {
            lines.push("The plate changed after the last slice. Slice it again.".into());
        } else if let Some(slice) = &self.slice {
            let minutes = slice.seconds / 60;
            lines.push(format!(
                "Last slice is {minutes} min and {:.1} ml. Save it, then copy the file to a USB stick.",
                slice.cured_ml
            ));
        }
        lines
    }

    fn measure_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Measure");
        ui.label("Click a point on a model, or on the bed, then click a second point.");
        match (self.measure_a, self.measure_b) {
            (Some(a), Some(b)) => {
                let d = b - a;
                ui.label(
                    egui::RichText::new(format!("{:.2} mm", d.length()))
                        .size(28.0)
                        .strong(),
                );
                ui.label(format!(
                    "X {:.2}   Y {:.2}   Z {:.2} mm",
                    d.x.abs(),
                    d.y.abs(),
                    d.z.abs()
                ));
                ui.label(format!("From {:.1}, {:.1}, {:.1}", a.x, a.y, a.z));
                ui.label(format!("To {:.1}, {:.1}, {:.1}", b.x, b.y, b.z));
            }
            (Some(_), None) => {
                ui.label("Click the second point.");
            }
            _ => {
                ui.label("Click the first point.");
            }
        }
        if ui.button("Clear").clicked() {
            self.measure_a = None;
            self.measure_b = None;
            self.measure_gen = self.measure_gen.wrapping_add(1);
        }
    }

    fn paint_measure(&self, frame: &mut PlateFrame) {
        let Some(a) = self.measure_a else {
            return;
        };
        let color = [0.96, 0.84, 0.28];
        let cross = |frame: &mut PlateFrame, p: Vec3| {
            let n = 1.2;
            frame.add_line(p - Vec3::X * n, p + Vec3::X * n, color);
            frame.add_line(p - Vec3::Y * n, p + Vec3::Y * n, color);
            frame.add_line(p - Vec3::Z * n, p + Vec3::Z * n, color);
        };
        cross(frame, a);
        if let Some(b) = self.measure_b {
            cross(frame, b);
            frame.add_line(a, b, color);
        }
    }

    fn remember_recent(&mut self, path: &std::path::Path) {
        let text = path.display().to_string();
        self.recent.retain(|p| p != &text);
        self.recent.insert(0, text);
        self.recent.truncate(8);
    }

    fn touch_view(&mut self) {
        self.view_rev = self.view_rev.wrapping_add(1);
    }

    fn toggle_view(&mut self, ui: &mut egui::Ui, label: &str, value: bool) -> bool {
        let mut value = value;
        if ui.checkbox(&mut value, label).changed() {
            self.touch_view();
        }
        value
    }

    fn toggle_hidden(&mut self, id: u64) {
        if !self.hidden_models.remove(&id) {
            self.hidden_models.insert(id);
        }
        self.touch_view();
    }

    fn section_controls(&mut self, ui: &mut egui::Ui) {
        let mut on = self.plate_view.section;
        if ui.checkbox(&mut on, "Cut the view").changed() {
            self.plate_view.section = on;
            self.cuts_custom = true;
            self.touch_view();
        }
        let (min_z, max_z) = self.cut_limits();
        let mut z = self.plate_view.section_z;
        if drag_f32(ui, "Top cut", &mut z, 0.1, min_z, max_z, "mm") {
            self.plate_view.section_z = z.max(self.plate_view.section_lo + 0.15);
            self.plate_view.section = true;
            self.cuts_custom = true;
            self.touch_view();
        }
        let mut lo = self.plate_view.section_lo;
        if drag_f32(ui, "Bottom cut", &mut lo, 0.1, min_z, max_z, "mm") {
            self.plate_view.section_lo = lo.min(self.plate_view.section_z - 0.15);
            self.plate_view.section = true;
            self.cuts_custom = true;
            self.touch_view();
        }
        ui.label("Hides the model outside the two cuts and fills each cut with a solid face. Supports stay drawn. A hidden model still prints.");
    }

    fn model_z_span(&self) -> Option<(f32, f32)> {
        let mut lo = f32::MAX;
        let mut hi = f32::MIN;
        let mut any = false;
        for obj in &self.doc.objects {
            if self.hidden_models.contains(&obj.id) {
                continue;
            }
            if let Some((min, max)) = Document::display_bounds(obj) {
                lo = lo.min(min.z);
                hi = hi.max(max.z);
                any = true;
            }
        }
        if any && hi > lo + 0.05 {
            Some((lo, hi))
        } else {
            None
        }
    }

    fn cut_limits(&self) -> (f32, f32) {
        if let Some((lo, hi)) = self.model_z_span() {
            (lo, hi.max(lo + 0.5))
        } else {
            (0.0, self.machine.size_z.max(1.0))
        }
    }

    /// Park the handles on the top and bottom of the part until the user drags.
    fn fit_cuts_to_models(&mut self) {
        let Some((lo, hi)) = self.model_z_span() else {
            return;
        };
        self.plate_view.section_lo = lo;
        self.plate_view.section_z = hi;
        self.plate_view.section = true;
        self.touch_view();
    }

    fn nudge_top_cut(&mut self, dy: f32) {
        let (_, max_z) = self.cut_limits();
        let (min_z, _) = self.cut_limits();
        let gap = 0.15;
        self.plate_view.section_z = (self.plate_view.section_z + dy)
            .clamp(self.plate_view.section_lo + gap, max_z.max(min_z));
        self.plate_view.section = true;
        self.cuts_custom = true;
        self.touch_view();
        self.sync_preview_to_cut();
    }

    fn sync_preview_to_cut(&mut self) {
        let Some(slice) = &self.slice else {
            return;
        };
        if slice.layers.is_empty() {
            return;
        }
        let z = self.plate_view.section_z;
        let mut best = 0usize;
        let mut best_d = f32::MAX;
        for (i, layer) in slice.layers.iter().enumerate() {
            let d = (layer.z_top_mm - z).abs();
            if d < best_d {
                best_d = d;
                best = i;
            }
        }
        self.preview_index = best;
        self.preview_for = None;
    }

    fn support_visibility(&mut self, ui: &mut egui::Ui) {
        ui.label("Support pieces");
        self.plate_view.contacts = self.toggle_view(ui, "Contact points", self.plate_view.contacts);
        self.plate_view.necks = self.toggle_view(ui, "Necks", self.plate_view.necks);
        self.plate_view.trunks = self.toggle_view(ui, "Trunks", self.plate_view.trunks);
        self.plate_view.feet = self.toggle_view(ui, "Feet", self.plate_view.feet);
        self.plate_view.branches = self.toggle_view(ui, "Branches", self.plate_view.branches);
        self.plate_view.braces = self.toggle_view(ui, "Braces", self.plate_view.braces);
        self.plate_view.rafts = self.toggle_view(ui, "Rafts", self.plate_view.rafts);
        ui.horizontal_wrapped(|ui| {
            if ui.button("Tips only").clicked() {
                self.plate_view.tips_only();
                self.touch_view();
            }
            if ui.button("Show all pieces").clicked() {
                self.plate_view.show_all_pieces();
                self.touch_view();
            }
        });
        ui.label("Tips only draws the contact points. Hiding a piece only changes the view. The slice still includes it.");
    }

    fn model_list(&mut self, ui: &mut egui::Ui) {
        ui.heading("Models");
        ui.label(format!(
            "{}  ·  {:.0} × {:.0} × {:.0} mm",
            self.machine.name, self.machine.size_x, self.machine.size_y, self.machine.size_z
        ));
        if self.doc.objects.is_empty() {
            ui.label("Open an STL, OBJ, or 3MF, or drop it on the plate.");
        }
        let rows: Vec<(u64, String, usize, bool)> = self
            .doc
            .objects
            .iter()
            .map(|obj| {
                let n = self
                    .doc
                    .supports
                    .iter()
                    .filter(|s| s.object_id == obj.id)
                    .count();
                let asm = obj.assembly_id();
                let grouped = self
                    .doc
                    .objects
                    .iter()
                    .filter(|other| other.assembly_id() == asm)
                    .count()
                    > 1;
                let negative = obj.kind == VolumeKind::Negative;
                let mut name = obj.name.clone();
                if negative {
                    name = format!("{name}  ·  negative");
                } else if grouped {
                    name = format!("{name}  ·  part");
                }
                (obj.id, name, n, negative)
            })
            .collect();
        let mut select = None;
        let mut toggle = None;
        let shift = ui.input(|i| i.modifiers.shift);
        for (id, name, n, _) in rows {
            let selected = self.doc.selection == Selection::Object(id) || self.picked.contains(&id);
            let label = if n > 0 {
                format!("{name}  ·  {n}")
            } else {
                name
            };
            ui.horizontal(|ui| {
                let mut shown = !self.hidden_models.contains(&id);
                if ui
                    .checkbox(&mut shown, "")
                    .on_hover_text("Show or hide this model in the view. It still prints.")
                    .changed()
                {
                    toggle = Some((id, shown));
                }
                if ui
                    .selectable_label(selected, label)
                    .on_hover_text("Shift-click to add it to the assemble and boolean set.")
                    .clicked()
                {
                    select = Some((id, shift));
                }
            });
        }
        if !self.hidden_models.is_empty() && ui.button("Show every model").clicked() {
            self.hidden_models.clear();
            self.touch_view();
        }
        if let Some((id, shown)) = toggle {
            if shown {
                self.hidden_models.remove(&id);
            } else {
                self.hidden_models.insert(id);
            }
            self.touch_view();
        }
        if let Some((id, shift)) = select {
            self.pick_model(id, shift);
        }
        let picked_n = self.picked_ids().len();
        if picked_n > 1 {
            ui.label(format!(
                "{picked_n} models selected. Right-click one for assemble, boolean, or the RERF grid."
            ));
        }
        if let Selection::Object(id) = self.doc.selection {
            if let Some(obj) = self.doc.object_mut(id) {
                ui.horizontal(|ui| {
                    ui.label("Name");
                    ui.text_edit_singleline(&mut obj.name);
                });
            }
        }
        ui.add_space(6.0);
        self.model_actions(ui);
    }

    fn model_actions(&mut self, ui: &mut egui::Ui) {
        let Some(id) = self.doc.edit_target() else {
            ui.label("Select a model. Hollow, holes, supports, and size changes apply only to it. Right-click a model for the full menu.");
            return;
        };
        let hollow = self.doc.object(id).is_some_and(|obj| obj.hollow);
        ui.horizontal_wrapped(|ui| {
            if ui
                .button(if hollow { "Make solid" } else { "Hollow" })
                .clicked()
            {
                self.toggle_hollow(id);
            }
            if ui.button("Punch hole").clicked() {
                self.punch_hole(id);
            }
            if ui.button("Add supports").clicked() {
                self.grow_supports(false);
            }
        });
    }

    fn toggle_hollow(&mut self, id: u64) {
        let Some(obj) = self.doc.object_mut(id) else {
            return;
        };
        obj.hollow = !obj.hollow;
        let on = obj.hollow;
        self.doc.touch_xform();
        self.invalidate_slice();
        self.status = if on {
            "The cut shows the wall and the empty inside. Punch a hole so resin can drain.".into()
        } else {
            "This model will print solid.".into()
        };
    }

    fn punch_hole(&mut self, id: u64) {
        let kept = self.doc.punch_bottom_drain(id);
        self.tool = Tool::Drain;
        self.invalidate_slice();
        self.status = if kept {
            "Punched a hole and set the plug beside the model. Print that piece and glue it back in.".into()
        } else {
            "Punched a hole at the bottom of this model. Click the shell to place another.".into()
        };
    }

    fn grow_supports(&mut self, platform_only: bool) {
        let Some(id) = self.doc.edit_target() else {
            self.status = "Select a model first. Supports are added only to that model.".into();
            return;
        };
        if let Some(obj) = self.doc.object_mut(id) {
            obj.support.platform_only = platform_only;
        }
        self.doc.sync_defaults_from(id);
        if self.doc.add_auto_supports() {
            self.invalidate_slice();
            self.status = "Supports added to the selected model.".into();
        } else {
            self.status = "No overhangs on the selected model needed a support.".into();
        }
    }

    fn props_panel(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            if !self.workshop {
                self.simple_panel(ui);
                return;
            }
            self.model_list(ui);
            ui.separator();
            match self.tool {
                Tool::Select => self.select_ui(ui),
                Tool::Move => self.move_ui(ui),
                Tool::Rotate => self.rotate_ui(ui),
                Tool::Scale => self.scale_ui(ui),
                Tool::Mirror => self.mirror_ui(ui),
                Tool::Hollow => self.hollow_ui(ui),
                Tool::Drain => self.drain_ui(ui),
                Tool::Support => self.support_ui(ui),
                Tool::Measure => self.measure_ui(ui),
            }
            ui.separator();
            egui::CollapsingHeader::new("Print settings")
                .default_open(true)
                .show(ui, |ui| self.slice_ui(ui));
        });
    }

    fn edit_vec(
        &mut self,
        ui: &mut egui::Ui,
        id: u64,
        kind: &'static str,
        speed: f32,
        suffix: &str,
    ) {
        let Some(obj) = self.doc.object(id) else {
            return;
        };
        let before_pos = obj.position;
        let before_rot = obj.rotation_deg;
        let before_scale = obj.scale;
        let mut value = match kind {
            "pos" => obj.position,
            "rot" => obj.rotation_deg,
            _ => obj.scale,
        };
        let mut started = false;
        let mut changed = false;
        grid_drag(ui, &mut value, speed, suffix, &mut started, &mut changed);
        if started && !self.gesture {
            self.doc
                .remember_xform(id, before_pos, before_rot, before_scale);
            self.gesture = true;
        }
        if changed {
            if let Some(obj) = self.doc.object_mut(id) {
                match kind {
                    "pos" => obj.position = value,
                    "rot" => obj.rotation_deg = value,
                    _ => {
                        obj.scale = Vec3::new(nonzero(value.x), nonzero(value.y), nonzero(value.z))
                    }
                }
            }
            self.doc.touch_xform();
            self.invalidate_slice();
        }
    }

    fn tip_fields(&mut self, ui: &mut egui::Ui) {
        let Selection::Support(id) = self.doc.selection else {
            self.tip_field_for = None;
            return;
        };
        let Some(support) = self.doc.supports.iter().find(|s| s.id == id).copied() else {
            self.tip_field_for = None;
            return;
        };
        ui.label(
            "X and Y move this tip and keep it on the model. Arrow keys do the same, 1 mm, or 0.1 mm with Shift.",
        );
        let mut x = support.x;
        let mut y = support.y;
        let mut started = false;
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.label("X");
            let rx = ui.add(egui::DragValue::new(&mut x).speed(0.1).suffix(" mm"));
            ui.label("Y");
            let ry = ui.add(egui::DragValue::new(&mut y).speed(0.1).suffix(" mm"));
            started = rx.drag_started() || ry.drag_started();
            changed = rx.changed() || ry.changed();
        });
        ui.label(format!(
            "Tip {:.1} mm up. Foot at {:.1} mm.",
            support.z_top, support.z_base
        ));
        let moved = (x - support.x).abs() > 1e-4 || (y - support.y).abs() > 1e-4;
        if (started || changed) && moved {
            if self.doc.tip_can_land(id, x, y) {
                if self.tip_field_for != Some(id) {
                    self.doc.remember_supports();
                    self.tip_field_for = Some(id);
                }
                let _ = self.doc.reseat_support(id, x, y);
                self.invalidate_slice();
            } else {
                self.status = "That spot is off the model, so the tip stayed put.".into();
            }
        }
        if ui.button("Delete this support").clicked() {
            self.tip_field_for = None;
            self.doc.delete_selection();
            self.invalidate_slice();
        }
    }

    fn select_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Select");
        ui.label("Left-drag moves the selected model in X and Y. Right-drag orbits, and can swing under the bed. Right-click the model for hollow, holes, supports, and the rest.");
        ui.checkbox(&mut self.snap_mm, "Snap moves to 1 mm");
        if let Selection::Support(_) = self.doc.selection {
            self.tip_fields(ui);
            return;
        }
        if let Selection::Drain(id) = self.doc.selection {
            if let Some(drain) = self.doc.drains.iter().find(|d| d.id == id).copied() {
                ui.label(format!(
                    "Hole {:.1} mm across, {:.1} mm deep.",
                    drain.radius_mm * 2.0,
                    drain.depth_mm
                ));
                ui.label("Switch to Hole to change the size. Delete removes it.");
            }
            return;
        }
        let Selection::Object(id) = self.doc.selection else {
            ui.label("Click a model on the plate, or pick one in the list.");
            self.arrange_ui(ui);
            return;
        };
        self.size_label(ui, id);
        ui.add_space(6.0);
        self.arrange_ui(ui);
        ui.horizontal_wrapped(|ui| {
            if ui.button("Drop to bed").clicked() {
                self.doc.push_xform_undo(id);
                self.doc.drop_object(id);
                self.invalidate_slice();
            }
            if ui.button("Center").clicked() {
                self.doc.push_xform_undo(id);
                self.doc
                    .center_on_plate(id, self.machine.size_x, self.machine.size_y);
                self.invalidate_slice();
            }
            if ui.button("Flip normals").clicked() {
                self.doc.flip_normals(id);
                self.invalidate_slice();
            }
            if ui.button("Repair").clicked() {
                self.doc.repair(id);
                self.invalidate_slice();
                self.status =
                    "Welded duplicate corners and flipped the shell if it was inside out.".into();
            }
        });
    }

    fn move_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Move");
        ui.label("Drag on the plate. Hold Shift and drag to lift.");
        ui.checkbox(&mut self.snap_mm, "Snap moves to 1 mm");
        let Selection::Object(id) = self.doc.selection else {
            ui.label("Select a model first.");
            return;
        };
        self.edit_vec(ui, id, "pos", 0.1, "mm");
        ui.horizontal(|ui| {
            if ui.button("Put on plate").clicked() {
                self.doc.push_xform_undo(id);
                self.doc.drop_object(id);
                self.invalidate_slice();
            }
            if ui.button("Center").clicked() {
                self.doc.push_xform_undo(id);
                self.doc
                    .center_on_plate(id, self.machine.size_x, self.machine.size_y);
                self.invalidate_slice();
            }
        });
    }

    fn rotate_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Rotate");
        ui.label("Drag to turn. Values are degrees. Hold Ctrl while dragging to snap to 15°.");
        let Selection::Object(id) = self.doc.selection else {
            ui.label("Select a model first.");
            return;
        };
        self.edit_vec(ui, id, "rot", 0.5, "°");
        ui.horizontal_wrapped(|ui| {
            if ui.button("Auto orient").clicked() {
                self.doc.auto_orient(id);
                self.invalidate_slice();
                self.status = "Oriented to cut overhangs, then dropped to the bed.".into();
            }
            if ui.button("Largest face down").clicked() {
                self.doc.place_on_largest_face(id);
                self.invalidate_slice();
            }
        });
        ui.label("Quarter turns");
        ui.horizontal_wrapped(|ui| {
            for (label, axis, deg) in [
                ("−90° X", 0, -90.0),
                ("+90° X", 0, 90.0),
                ("−90° Y", 1, -90.0),
                ("+90° Y", 1, 90.0),
                ("−90° Z", 2, -90.0),
                ("+90° Z", 2, 90.0),
            ] {
                if ui.button(label).clicked() {
                    self.turn(id, axis, deg);
                }
            }
        });
        ui.separator();
        ui.label("Cut on Z keeps both pieces. The upper piece stays selected.");
        drag_f32(ui, "Cut height", &mut self.cut_z, 0.1, 0.0, 400.0, "mm");
        if ui.button("Cut").clicked() {
            match self.doc.cut_at_z(id, self.cut_z) {
                Ok(_) => {
                    self.invalidate_slice();
                    self.status = "Cut the model and kept both pieces.".into();
                }
                Err(err) => self.status = err.into(),
            }
        }
    }

    fn turn(&mut self, id: u64, axis: usize, deg: f32) {
        self.doc.push_xform_undo(id);
        if let Some(obj) = self.doc.object_mut(id) {
            match axis {
                0 => obj.rotation_deg.x += deg,
                1 => obj.rotation_deg.y += deg,
                _ => obj.rotation_deg.z += deg,
            }
        }
        self.doc.touch_xform();
        self.invalidate_slice();
    }

    fn scale_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Scale");
        ui.label("1.00 is the size in the file. Drag up to enlarge. A drag scales all three axes together. The numbers change one axis.");
        let Selection::Object(id) = self.doc.selection else {
            ui.label("Select a model first.");
            return;
        };
        self.edit_vec(ui, id, "scale", 0.005, "");
        if ui.button("Reset to 100%").clicked() {
            self.doc.push_xform_undo(id);
            if let Some(obj) = self.doc.object_mut(id) {
                obj.scale = Vec3::ONE;
            }
            self.doc.touch_xform();
            self.invalidate_slice();
        }
        self.size_editors(ui, id);
        self.size_label(ui, id);
    }

    fn size_editors(&mut self, ui: &mut egui::Ui, id: u64) {
        let Some(obj) = self.doc.object(id) else {
            return;
        };
        let local = [
            (obj.bounds_max[0] - obj.bounds_min[0]).abs().max(1e-4),
            (obj.bounds_max[1] - obj.bounds_min[1]).abs().max(1e-4),
            (obj.bounds_max[2] - obj.bounds_min[2]).abs().max(1e-4),
        ];
        let signs = [
            obj.scale.x.signum(),
            obj.scale.y.signum(),
            obj.scale.z.signum(),
        ];
        let mut size = Vec3::new(
            local[0] * obj.scale.x.abs(),
            local[1] * obj.scale.y.abs(),
            local[2] * obj.scale.z.abs(),
        );
        let before_pos = obj.position;
        let before_rot = obj.rotation_deg;
        let before_scale = obj.scale;
        ui.label("Size along the model axes");
        let mut started = false;
        let mut changed = false;
        for (axis, label) in [(0, "X"), (1, "Y"), (2, "Z")] {
            let value = match axis {
                0 => &mut size.x,
                1 => &mut size.y,
                _ => &mut size.z,
            };
            ui.horizontal(|ui| {
                ui.label(label);
                let response = ui.add(
                    egui::DragValue::new(value)
                        .speed(0.1)
                        .range(0.1..=2000.0)
                        .suffix(" mm"),
                );
                started |= response.drag_started();
                changed |= response.changed();
            });
        }
        if started && !self.gesture {
            self.doc
                .remember_xform(id, before_pos, before_rot, before_scale);
            self.gesture = true;
        }
        if changed {
            if let Some(obj) = self.doc.object_mut(id) {
                let sign = |s: f32| if s < 0.0 { -1.0 } else { 1.0 };
                obj.scale = Vec3::new(
                    sign(signs[0]) * (size.x / local[0]).max(0.01),
                    sign(signs[1]) * (size.y / local[1]).max(0.01),
                    sign(signs[2]) * (size.z / local[2]).max(0.01),
                );
            }
            self.doc.touch_xform();
            self.invalidate_slice();
        }
    }

    fn mirror_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Mirror");
        ui.checkbox(&mut self.keep_original, "Keep the original model");
        let Selection::Object(id) = self.doc.selection else {
            ui.label("Select a model first.");
            return;
        };
        ui.horizontal(|ui| {
            if ui.button("Mirror X").clicked() {
                self.apply_mirror(id, 0);
            }
            if ui.button("Mirror Y").clicked() {
                self.apply_mirror(id, 1);
            }
            if ui.button("Mirror Z").clicked() {
                self.apply_mirror(id, 2);
            }
        });
        ui.label(
            "Mirroring flips a scale axis. Repair afterwards if the shell comes out inside out.",
        );
    }

    fn apply_mirror(&mut self, id: u64, axis: usize) {
        if self.keep_original {
            if let Some(copy) = self.doc.duplicate_mesh_only(id) {
                self.doc.mirror(copy, axis);
            }
        } else {
            self.doc.mirror(id, axis);
        }
        self.invalidate_slice();
    }

    fn drain_ui(&mut self, ui: &mut egui::Ui) {
        self.sync_hole_panel();
        ui.heading("Punch");
        ui.label("Move over the model to see the hole, then click to punch it. It points into the part so resin can drain and air can enter.");
        ui.label("Put at least one hole near the lowest point of a cup, or the layer that seals it will suction onto the film.");
        ui.add_space(4.0);
        let along = self.doc.hole_along_view;
        ui.radio_value(
            &mut self.doc.hole_along_view,
            false,
            "Perpendicular to the model",
        );
        ui.radio_value(
            &mut self.doc.hole_along_view,
            true,
            "Perpendicular to the screen",
        );
        if self.doc.hole_along_view != along {
            self.doc.touch_xform();
        }
        ui.add_space(6.0);
        let mut edited = false;
        edited |= hole_field(
            ui,
            "Outer Circle Diameter D1(mm)",
            &mut self.doc.drain_diameter_mm,
            0.4,
            16.0,
        );
        edited |= hole_field(
            ui,
            "Inside Circle Diameter D2(mm)",
            &mut self.doc.drain_inner_mm,
            0.4,
            16.0,
        );
        edited |= hole_field(
            ui,
            "Extended Length L1(mm)",
            &mut self.doc.drain_extend_mm,
            0.0,
            40.0,
        );
        edited |= hole_field(
            ui,
            "Groove Depth L2(mm)",
            &mut self.doc.drain_depth_mm,
            0.2,
            80.0,
        );
        ui.add_space(4.0);
        ui.checkbox(&mut self.doc.hole_keep, "Keep Hole")
            .on_hover_text(
                "Save the resin this hole removes as its own model, set beside the part. Print it and glue it back in.",
            );
        if edited {
            self.write_selected_hole();
        }
        ui.add_space(6.0);
        if ui.button("Punch hole at the bottom").clicked() {
            if let Some(id) = self.doc.edit_target() {
                self.punch_hole(id);
            } else {
                self.status = "Select the model you want a hole in.".into();
            }
        }
    }

    fn sync_hole_panel(&mut self) {
        let Selection::Drain(id) = self.doc.selection else {
            self.hole_panel_for = None;
            return;
        };
        if self.hole_panel_for == Some(id) {
            return;
        }
        let Some(drain) = self.doc.drains.iter().find(|d| d.id == id).copied() else {
            self.hole_panel_for = None;
            return;
        };
        self.doc.drain_diameter_mm = drain.radius_mm * 2.0;
        self.doc.drain_inner_mm = drain.inner_radius() * 2.0;
        self.doc.drain_extend_mm = drain.extend_mm;
        self.doc.drain_depth_mm = drain.depth_mm;
        self.hole_panel_for = Some(id);
    }

    fn write_selected_hole(&mut self) {
        let Selection::Drain(id) = self.doc.selection else {
            return;
        };
        let (outer, inner, extend, depth) = self.doc.hole_dims();
        if let Some(slot) = self.doc.drains.iter_mut().find(|d| d.id == id) {
            slot.radius_mm = outer;
            slot.inner_radius_mm = inner;
            slot.extend_mm = extend;
            slot.depth_mm = depth;
        } else {
            return;
        }
        self.doc.touch();
        self.invalidate_slice();
    }

    fn size_label(&self, ui: &mut egui::Ui, id: u64) {
        if let Some(obj) = self.doc.object(id) {
            if let Some((min, max)) = crate::scene::Document::world_bounds(obj) {
                ui.label(format!(
                    "Size {:.1} × {:.1} × {:.1} mm",
                    max.x - min.x,
                    max.y - min.y,
                    max.z - min.z
                ));
            }
            let ml = crate::scene::Document::world_mesh(obj).volume_mm3() / 1000.0;
            ui.label(format!("Volume {ml:.2} ml before hollowing"));
        }
    }

    fn arrange_ui(&mut self, ui: &mut egui::Ui) {
        ui.label("Layout");
        ui.horizontal_wrapped(|ui| {
            if ui.button("Layout all").clicked() {
                self.doc
                    .auto_layout(self.machine.size_x, self.machine.size_y);
                self.invalidate_slice();
            }
            if ui.button("Fill bed").clicked() {
                let Selection::Object(id) = self.doc.selection else {
                    self.status = "Select the model you want copied across the bed.".into();
                    return;
                };
                match self
                    .doc
                    .fill_bed(id, self.machine.size_x, self.machine.size_y)
                {
                    Ok(n) => {
                        self.invalidate_slice();
                        self.status = format!("Placed {n} copies. The bed holds {} in all.", n + 1);
                    }
                    Err(err) => self.status = err.into(),
                }
            }
        });
        ui.separator();
        ui.label("Or make a set number of copies, with a gap between them.");
        let mut count = self.copy_count as f32;
        if drag_f32(ui, "Copies", &mut count, 0.05, 1.0, 40.0, "") {
            self.copy_count = count.round().clamp(1.0, 40.0) as u32;
        }
        drag_f32(ui, "Gap", &mut self.copy_gap, 0.1, 0.0, 30.0, "mm");
        if ui.button("Make copies").clicked() {
            let Selection::Object(id) = self.doc.selection else {
                self.status = "Select the model you want to copy.".into();
                return;
            };
            match self.doc.duplicate_copies(
                id,
                self.copy_count,
                self.copy_gap,
                self.machine.size_x,
                self.machine.size_y,
            ) {
                Ok(n) => {
                    self.invalidate_slice();
                    self.status = format!("Added {n} copies.");
                }
                Err(err) => self.status = err.into(),
            }
        }
    }

    fn hollow_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Hollow");
        ui.label("The inside stays empty. The cut on the right shows the wall and that cavity. Resin infill is not offered: it traps resin and blows out the print.");
        let Some(id) = self.doc.edit_target() else {
            ui.label("Select a model. Hollowing applies only to it.");
            return;
        };
        if ui.button("Punch hole at the bottom").clicked() {
            self.punch_hole(id);
            return;
        }
        let Some(obj) = self.doc.object_mut(id) else {
            return;
        };
        let mut changed = false;
        changed |= ui.checkbox(&mut obj.hollow, "Hollow this model").changed();
        ui.add_enabled_ui(obj.hollow, |ui| {
            changed |= drag_f32(ui, "Wall", &mut obj.wall_mm, 0.05, 0.4, 8.0, "mm");
            changed |= drag_f32(
                ui,
                "Bottom cap",
                &mut obj.bottom_cap_mm,
                0.05,
                0.0,
                20.0,
                "mm",
            );
            changed |= drag_f32(ui, "Top cap", &mut obj.top_cap_mm, 0.05, 0.0, 20.0, "mm");
        });
        ui.label("Hole, in the tool list, clicks more drains through the shell. Put one near the lowest point of a cup.");
        if changed {
            self.doc.touch_xform();
            self.invalidate_slice();
        }
    }

    fn write_support(&mut self, id: u64, profile: crate::scene::ModelSupport) {
        if let Some(obj) = self.doc.object_mut(id) {
            obj.support = profile;
        }
        self.doc.sync_defaults_from(id);
        self.doc.touch();
    }

    fn support_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Support");
        ui.label("Click an underside to plant a tip. Click a tip to select it, then drag it, type X and Y, or nudge it with the arrow keys. Every control here applies only to the selected model.");
        if let Selection::Support(_) = self.doc.selection {
            self.tip_fields(ui);
            ui.separator();
        }
        if !self.island_marks.is_empty() && self.model_stamp() == self.island_stamp {
            ui.label(format!(
                "{} red marks are islands from the last slice. Click one to plant a support. They stay until a model moves.",
                self.island_marks.len()
            ));
        }
        ui.checkbox(&mut self.erase_supports, "Erase tips");
        if self.erase_supports {
            let _ = drag_f32(
                ui,
                "Erase radius",
                &mut self.erase_mm,
                0.05,
                0.4,
                20.0,
                "mm",
            );
            ui.label("Click or drag across tips to remove them. Undo puts them back.");
        }
        self.section_controls(ui);
        ui.add_space(4.0);
        self.support_visibility(ui);
        ui.separator();
        let Some(id) = self.doc.edit_target() else {
            ui.label("Select a model first.");
            return;
        };
        let Some(mut profile) = self.doc.object(id).map(|obj| obj.support) else {
            return;
        };
        let mut preset = profile.preset.min(PRESETS.len() - 1);
        let before = preset;
        egui::ComboBox::from_label("Preset")
            .selected_text(PRESETS[preset].name)
            .show_ui(ui, |ui| {
                for (i, preset_def) in PRESETS.iter().enumerate() {
                    ui.selectable_value(&mut preset, i, preset_def.name);
                }
            });
        if preset != before {
            profile.preset = preset;
            profile.style = PRESETS[preset].style;
            self.write_support(id, profile);
            self.invalidate_slice();
        }
        let mut style = profile.style;
        let mut changed = false;
        changed |= drag_f32(
            ui,
            "Overhang angle",
            &mut style.overhang_deg,
            0.5,
            5.0,
            85.0,
            "°",
        );
        changed |= drag_f32(
            ui,
            "Tip distance",
            &mut style.spacing_mm,
            0.05,
            0.6,
            12.0,
            "mm",
        );
        changed |= ui.checkbox(&mut style.ball, "Ball contact").changed();
        ui.strong("Tip");
        ui.label(if style.ball {
            "Ball contact: a sphere bitten into the surface."
        } else {
            "Point contact: a cone bitten into the surface."
        });
        changed |= drag_f32(
            ui,
            "Contact diameter",
            &mut style.contact_mm,
            0.01,
            0.08,
            2.0,
            "mm",
        );
        changed |= drag_f32(
            ui,
            "Contact depth",
            &mut style.contact_depth,
            0.01,
            0.05,
            1.5,
            "mm",
        );
        changed |= drag_f32(
            ui,
            "Upper diameter",
            &mut style.tip_upper_mm,
            0.01,
            0.1,
            3.0,
            "mm",
        );
        changed |= drag_f32(
            ui,
            "Lower diameter",
            &mut style.tip_lower_mm,
            0.01,
            0.15,
            4.0,
            "mm",
        );
        changed |= drag_f32(
            ui,
            "Connection length",
            &mut style.tip_len_mm,
            0.05,
            0.4,
            8.0,
            "mm",
        );
        ui.strong("Middle");
        changed |= drag_f32(
            ui,
            "Trunk diameter",
            &mut style.trunk_mm,
            0.02,
            0.3,
            5.0,
            "mm",
        );
        let before_shape = style.section;
        egui::ComboBox::from_label("Cross section")
            .selected_text(style.section.label())
            .show_ui(ui, |ui| {
                for shape in [
                    SectionShape::Hexagon,
                    SectionShape::Round,
                    SectionShape::Square,
                ] {
                    ui.selectable_value(&mut style.section, shape, shape.label());
                }
            });
        if style.section != before_shape {
            changed = true;
        }
        ui.label("Hexagon is the usual pillar. The diameter is measured flat to flat.");
        changed |= drag_f32(
            ui,
            "Branch angle",
            &mut style.branch_deg,
            0.5,
            10.0,
            75.0,
            "°",
        );
        changed |= drag_f32(
            ui,
            "Trunk spacing",
            &mut style.cluster_mm,
            0.1,
            1.5,
            20.0,
            "mm",
        );
        ui.strong("Foot");
        changed |= drag_f32(ui, "Foot height", &mut style.foot_mm, 0.02, 0.2, 3.0, "mm");
        let mut foot_diam = if style.foot_diam_mm > 0.05 {
            style.foot_diam_mm
        } else {
            style.trunk_mm * 2.1
        };
        if drag_f32(ui, "Foot diameter", &mut foot_diam, 0.02, 0.4, 8.0, "mm") {
            style.foot_diam_mm = foot_diam;
            changed = true;
        }
        changed |= drag_f32(
            ui,
            "Brace diameter",
            &mut style.brace_mm,
            0.01,
            0.1,
            2.0,
            "mm",
        );
        let mut platform_only = profile.platform_only;
        changed |= ui
            .checkbox(&mut platform_only, "To the platform only")
            .changed();
        ui.label("Branch angle is the lean off vertical. Trunk spacing is how far apart tips must be before they get their own trunk.");
        if changed {
            profile.style = style.sanitized();
            profile.platform_only = platform_only;
            self.write_support(id, profile);
            self.invalidate_slice();
        }
        ui.horizontal_wrapped(|ui| {
            if ui.button("Add supports").clicked() {
                self.grow_supports(false);
            }
            if ui.button("To the bed").clicked() {
                self.grow_supports(true);
            }
            if ui.button("Clear").clicked() {
                if self.doc.clear_supports() {
                    self.invalidate_slice();
                    self.status = "Cleared supports on the selected model.".into();
                } else {
                    self.status = "That model has no supports.".into();
                }
            }
        });
        let mut lift = profile.lift_mm;
        if drag_f32(ui, "Lift above bed", &mut lift, 0.05, 0.0, 40.0, "mm") {
            profile.lift_mm = lift;
            self.write_support(id, profile);
        }
        ui.label("Adding supports raises this model until its lowest point is this far above the bed. A second pass does not stack another lift. 0 leaves it where it is.");
        let mut raft = profile.raft;
        let mut braces = profile.braces_on;
        let mut raft_mm = profile.raft_mm;
        let mut raft_margin = profile.raft_margin;
        let mut raft_angle = profile.raft_angle;
        let mut brace_dist = profile.brace_dist;
        let mut brace_angle = profile.brace_angle;
        let mut raft_changed = false;
        raft_changed |= ui.checkbox(&mut raft, "Skate raft").changed();
        raft_changed |= drag_f32(ui, "Raft thickness", &mut raft_mm, 0.05, 0.2, 5.0, "mm");
        raft_changed |= drag_f32(ui, "Raft oversize", &mut raft_margin, 0.05, 0.0, 20.0, "mm");
        raft_changed |= drag_f32(ui, "Raft wall angle", &mut raft_angle, 0.5, 0.0, 70.0, "°");
        ui.label("Off until you turn it on, and only under this model's supports that reach the bed. Each pillar gets a square pad. Nearby pads join into one skate. The top overhangs the plate so a scraper can get under the lip.");
        raft_changed |= ui.checkbox(&mut braces, "Diagonal braces").changed();
        raft_changed |= drag_f32(ui, "Brace angle", &mut brace_angle, 0.5, 15.0, 75.0, "°");
        raft_changed |= drag_f32(ui, "Brace spacing", &mut brace_dist, 0.1, 2.0, 20.0, "mm");
        ui.label("Braces join nearby trunks and rise at this angle to the bed. 45° is the usual lean. Spacing is the gap between brace layers and the farthest two trunks a brace will join.");
        if raft_changed {
            profile.raft = raft;
            profile.raft_mm = raft_mm;
            profile.raft_margin = raft_margin;
            profile.raft_angle = raft_angle;
            profile.braces_on = braces;
            profile.brace_dist = brace_dist;
            profile.brace_angle = brace_angle;
            self.write_support(id, profile);
            self.invalidate_slice();
        }
        if ui.button("Support islands from last slice").clicked() {
            if let Some(slice) = &self.slice {
                let points: Vec<(f32, f32, f32)> = slice
                    .layers
                    .iter()
                    .flat_map(|layer| {
                        layer
                            .islands
                            .iter()
                            .map(|island| (island.x_mm, island.y_mm, island.z_mm))
                    })
                    .collect();
                let n = self.doc.add_island_supports(&points);
                self.invalidate_slice();
                self.status = if n == 0 {
                    "No islands from the last slice sit under the selected model.".into()
                } else {
                    format!("Added {n} supports under this model's islands. Slice again.")
                };
                self.view = View::Prepare;
            } else {
                self.status = "Slice once so Amber can see the islands.".into();
            }
        }
    }

    fn select_printer(&mut self, id: &str) {
        if self.machine.id == id {
            return;
        }
        let Some(profile) = catalog::find(id) else {
            return;
        };
        self.machine = Machine::from_profile(profile);
        self.export_as = 0;
        self.settings.light_off_s = profile.light_off_s;
        self.settings.lift_mm = profile.lift_mm.max(1.0);
        self.settings.lift_speed = profile.lift_speed;
        self.settings.retract_speed = profile.retract_speed;
        self.settings.bottom_lift_mm = self.settings.lift_mm;
        self.settings.bottom_lift_speed = profile.lift_speed;
        self.settings.bottom_retract_speed = profile.retract_speed;
        self.camera = Camera::looking_at_plate(self.plate());
        self.doc.touch();
        self.export_name = default_export_name(&self.doc, self.machine.extension);
        let name = self.settings.resin.clone();
        self.apply_resin(&name);
    }

    fn apply_resin(&mut self, name: &str) {
        let Some(resin) = find_resin(name) else {
            self.settings.resin = name.to_string();
            self.profile_note.clear();
            self.invalidate_slice();
            return;
        };
        self.settings.resin = resin.name.to_string();
        self.settings.density_g_ml = resin.density_g_ml;
        if let Some(profile) = lookup_profile(resin.id, self.machine.id, self.settings.layer_mm) {
            self.settings.layer_mm = profile.layer_mm;
            self.settings.exposure_s = profile.exposure_s;
            self.settings.bottom_exposure_s = profile.bottom_exposure_s;
            self.settings.bottom_layers = profile.bottom_layers;
            self.settings.light_off_s = profile.light_off_s;
            self.settings.lift_mm = profile.lift_mm;
            self.settings.lift_speed = profile.lift_speed;
            self.settings.retract_speed = profile.retract_speed;
            self.settings.bottom_lift_mm = profile.lift_mm;
            self.settings.bottom_lift_speed = profile.lift_speed;
            self.settings.bottom_retract_speed = profile.retract_speed;
            self.profile_note = profile.source.to_string();
            self.status = format!(
                "{} on {}: {:.2} s normal, {:.0} s bottom.",
                resin.name, self.machine.name, profile.exposure_s, profile.bottom_exposure_s
            );
        } else {
            self.profile_note = format!(
                "No published profile for {} on {}. Times were left as they are. Run a RERF before a long print.",
                resin.name, self.machine.name
            );
            self.status = self.profile_note.clone();
        }
        self.invalidate_slice();
    }

    fn slice_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Slice");
        ui.label("Times are filled from a published table for this printer. With no row, they stay as they are — run a RERF.");
        let px = if (self.machine.pixel_um - self.machine.pixel_um_y).abs() < 0.05 {
            format!("{:.0} µm", self.machine.pixel_um)
        } else {
            format!(
                "{:.0}×{:.0} µm",
                self.machine.pixel_um, self.machine.pixel_um_y
            )
        };
        ui.label(format!(
            "{} · {}×{} · {:.2} × {:.2} × {:.0} mm · {px}",
            self.machine.name,
            self.machine.res_x,
            self.machine.res_y,
            self.machine.size_x,
            self.machine.size_y,
            self.machine.size_z
        ));
        self.format_combo(ui);
        let format = self.chosen_format();
        let ext = self.machine.format_extension(format);
        if self.machine.reads_format(format) {
            ui.label(format!("Saves .{ext}, which {} reads.", self.machine.name));
        } else {
            ui.label(format!(
                "Saves .{ext}. {} reads .{} ({}), which Amber does not encode.",
                self.machine.name, self.machine.printer_extension, self.machine.format_name
            ));
        }
        ui.strong("Printer");
        ui.add(
            egui::TextEdit::singleline(&mut self.printer_filter)
                .hint_text("Search printers")
                .desired_width(f32::INFINITY),
        );
        let machine_id = self.machine.id;
        let filter = self.printer_filter.to_ascii_lowercase();
        let mut pick_printer: Option<&'static str> = None;
        egui::ScrollArea::vertical()
            .id_salt("printers")
            .max_height(130.0)
            .show(ui, |ui| {
                for printer in catalog::PRINTERS {
                    if !filter.is_empty()
                        && !printer.name.to_ascii_lowercase().contains(&filter)
                        && !printer.vendor.to_ascii_lowercase().contains(&filter)
                    {
                        continue;
                    }
                    let label = format!("{}  {}×{}", printer.name, printer.res_x, printer.res_y);
                    if ui
                        .selectable_label(printer.id == machine_id, label)
                        .clicked()
                    {
                        pick_printer = Some(printer.id);
                    }
                }
            });
        ui.strong("Resin");
        ui.checkbox(
            &mut self.only_profiled,
            "Only resins with settings for this printer",
        )
        .on_hover_text("Hides bottles that have no published time for the printer above. Uncheck it to browse the whole library.");
        ui.add(
            egui::TextEdit::singleline(&mut self.resin_filter)
                .hint_text("Search resins")
                .desired_width(f32::INFINITY),
        );
        let resin_filter = self.resin_filter.to_ascii_lowercase();
        let only_profiled = self.only_profiled;
        let mut with_profile = std::collections::HashSet::new();
        for profile in resins::PROFILES.iter().chain(community::PROFILES.iter()) {
            if profile.machine_id == machine_id {
                with_profile.insert(profile.resin_id);
            }
        }
        let current_resin = self.settings.resin.clone();
        let mut listed: Vec<&Resin> = resin_catalog()
            .filter(|resin| {
                if only_profiled && !with_profile.contains(resin.id) {
                    return false;
                }
                if resin_filter.is_empty() {
                    return true;
                }
                let blob = format!("{} {} {}", resin.vendor, resin.name, resin.family);
                blob.to_ascii_lowercase().contains(&resin_filter)
            })
            .collect();
        listed.sort_by(|a, b| {
            let ha = with_profile.contains(a.id);
            let hb = with_profile.contains(b.id);
            hb.cmp(&ha)
                .then_with(|| a.vendor.cmp(b.vendor))
                .then_with(|| a.name.cmp(b.name))
        });
        let shown = listed.len();
        let matched = listed
            .iter()
            .filter(|r| with_profile.contains(r.id))
            .count();
        if only_profiled {
            ui.label(format!("{shown} with a published profile on this printer"));
        } else {
            ui.label(format!(
                "{shown} resins · {matched} with a published profile on this printer"
            ));
        }
        if shown == 0 {
            ui.colored_label(
                egui::Color32::from_rgb(214, 154, 62),
                "Nothing published for this printer. Uncheck the filter to pick a resin by name, then run a RERF.",
            );
        } else if only_profiled
            && !current_resin.is_empty()
            && !listed.iter().any(|r| r.name == current_resin)
        {
            ui.label(format!(
                "Current resin “{current_resin}” has no profile here, so it is hidden."
            ));
        }
        let mut pick_resin: Option<&'static str> = None;
        egui::ScrollArea::vertical()
            .id_salt("resins")
            .max_height(170.0)
            .show(ui, |ui| {
                for resin in listed {
                    let mark = if with_profile.contains(resin.id) {
                        "● "
                    } else {
                        ""
                    };
                    let label = format!("{mark}{} · {}", resin.vendor, resin.name);
                    if ui
                        .selectable_label(resin.name == current_resin, label)
                        .clicked()
                    {
                        pick_resin = Some(resin.name);
                    }
                }
            });
        if self.profile_note.starts_with("No published") {
            ui.colored_label(
                egui::Color32::from_rgb(214, 154, 62),
                self.profile_note.as_str(),
            );
        } else if !self.profile_note.is_empty() {
            egui::CollapsingHeader::new("Where these times come from")
                .default_open(false)
                .id_salt("profile-source")
                .show(ui, |ui| {
                    ui.add(egui::Label::new(self.profile_note.as_str()).wrap());
                });
        }
        if let Some(id) = pick_printer {
            self.select_printer(id);
        }
        if let Some(name) = pick_resin {
            self.apply_resin(name);
        }
        let s = &mut self.settings;
        let mut changed = false;
        changed |= drag_f32(ui, "Layer height", &mut s.layer_mm, 0.005, 0.01, 0.2, "mm");
        changed |= drag_f32(ui, "Exposure", &mut s.exposure_s, 0.05, 0.5, 20.0, "s");
        changed |= drag_f32(
            ui,
            "Bottom exposure",
            &mut s.bottom_exposure_s,
            0.5,
            5.0,
            80.0,
            "s",
        );
        let mut bottoms = s.bottom_layers as f32;
        if drag_f32(ui, "Bottom layers", &mut bottoms, 0.1, 1.0, 20.0, "") {
            s.bottom_layers = bottoms.round() as u32;
            changed = true;
        }
        let mut transition = s.transition_layers as f32;
        if drag_f32(ui, "Transition layers", &mut transition, 0.1, 0.0, 20.0, "") {
            s.transition_layers = transition.round() as u32;
            changed = true;
        }
        changed |= drag_f32(ui, "Light-off", &mut s.light_off_s, 0.05, 0.0, 20.0, "s");
        changed |= drag_f32(
            ui,
            "Rest before cure",
            &mut s.rest_before_s,
            0.05,
            0.0,
            20.0,
            "s",
        );
        changed |= drag_f32(
            ui,
            "Rest after lift",
            &mut s.rest_after_lift_s,
            0.05,
            0.0,
            20.0,
            "s",
        );
        ui.label("Rest is added to the light-off the Photon file can store, and counted once in the time estimate.");
        changed |= drag_f32(ui, "Lift distance", &mut s.lift_mm, 0.1, 2.0, 15.0, "mm");
        changed |= drag_f32(ui, "Lift speed", &mut s.lift_speed, 0.05, 0.5, 8.0, "mm/s");
        changed |= drag_f32(
            ui,
            "Retract speed",
            &mut s.retract_speed,
            0.05,
            0.5,
            8.0,
            "mm/s",
        );
        let mut aa = s.anti_alias as f32;
        if drag_f32(ui, "Anti-alias", &mut aa, 0.05, 1.0, 8.0, "×") {
            s.anti_alias = match aa.round() as u8 {
                1..=2 => 2,
                3..=5 => 4,
                6..=8 => 8,
                _ => 1,
            };
            if aa.round() <= 1.0 {
                s.anti_alias = 1;
            }
            changed = true;
        }
        let mut blur = s.image_blur as f32;
        if drag_f32(ui, "Image blur", &mut blur, 0.05, 0.0, 4.0, "px") {
            s.image_blur = blur.round() as u8;
            changed = true;
        }
        ui.label("Blur stays off unless you set it. It softens the anti-aliased edge and adds time to the slice.");
        changed |= ui
            .checkbox(&mut s.fill_voids, "Heal hairline gaps")
            .changed();
        ui.label("Closes a one-pixel crack and a speckle. A hole in the model stays open, including the infinity mark, lettering, and pin holes. A model you hollowed stays empty. Drain holes are cut after this.");
        ui.collapsing("Compensation and cost", |ui| {
            changed |= drag_f32(ui, "XY offset", &mut s.xy_offset_mm, 0.01, -0.5, 0.5, "mm");
            changed |= drag_f32(
                ui,
                "Elephant foot",
                &mut s.elephant_foot_mm,
                0.01,
                0.0,
                0.5,
                "mm",
            );
            changed |= drag_f32(ui, "Shrink XY", &mut s.shrink_xy_pct, 0.01, 0.0, 5.0, "%");
            changed |= drag_f32(ui, "Shrink Z", &mut s.shrink_z_pct, 0.01, 0.0, 5.0, "%");
            changed |= drag_f32(ui, "Price", &mut s.price_per_liter, 1.0, 0.0, 200.0, "/L");
            ui.label("Positive XY offset grows the part. Elephant foot insets the bottom layers. Shrink scales the mesh up so the cured part comes out the drawn size.");
        });
        ui.collapsing("Bottom lift and image", |ui| {
            changed |= drag_f32(ui, "Bottom lift", &mut s.bottom_lift_mm, 0.1, 2.0, 15.0, "mm");
            changed |= drag_f32(ui, "Bottom lift speed", &mut s.bottom_lift_speed, 0.05, 0.5, 8.0, "mm/s");
            changed |= drag_f32(ui, "Bottom retract", &mut s.bottom_retract_speed, 0.05, 0.5, 8.0, "mm/s");
            changed |= ui.checkbox(&mut self.machine.rotate_180, "Rotate exposure 180°").changed();
            changed |= ui.checkbox(&mut self.machine.mirror_x, "Mirror X").changed();
            changed |= ui.checkbox(&mut self.machine.mirror_y, "Mirror Y").changed();
            ui.label("Print a 20 mm cube and flip these if the part comes out mirrored. The Photon M3 Max default is rotate 180°.");
        });
        if changed {
            self.invalidate_slice();
        }
        ui.add_space(6.0);
        ui.label("File name on the USB stick");
        ui.text_edit_singleline(&mut self.export_name);
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let slicing = self.job.is_some();
            if ui
                .add_enabled(!slicing, egui::Button::new("Slice"))
                .clicked()
            {
                self.start_slice(false);
            }
            let export_label = format!(
                "Export .{}",
                self.machine.format_extension(self.chosen_format())
            );
            if ui
                .add_enabled(!slicing, egui::Button::new(export_label))
                .clicked()
            {
                self.export_print(None);
            }
            if slicing && ui.button("Cancel").clicked() {
                if let Some(job) = &self.job {
                    job.cancel.store(true, Ordering::Relaxed);
                }
            }
        });
        if let Some(job) = &self.job {
            let frac = job.progress.load(Ordering::Relaxed) as f32 / job.total.max(1) as f32;
            ui.add(egui::ProgressBar::new(frac).show_percentage());
        }
    }

    fn center(&mut self, ui: &mut egui::Ui) {
        if self.view == View::Preview {
            self.preview(ui);
        } else {
            self.viewport(ui);
        }
    }

    fn viewport(&mut self, ui: &mut egui::Ui) {
        let avail = ui.available_size();
        let bar = 96.0;
        ui.horizontal(|ui| {
            ui.allocate_ui_with_layout(
                egui::vec2((avail.x - bar - 8.0).max(1.0), avail.y),
                egui::Layout::top_down(egui::Align::Min),
                |ui| self.viewport_scene(ui),
            );
            ui.allocate_ui_with_layout(
                egui::vec2(bar, avail.y),
                egui::Layout::top_down(egui::Align::Center),
                |ui| self.cut_bar(ui),
            );
        });
    }

    /// Two cut handles. The top one starts at the top of the part.
    fn cut_bar(&mut self, ui: &mut egui::Ui) {
        let step = self.settings.layer_mm.max(0.01);
        ui.label("Top");
        if ui
            .add(egui::Button::new("▲").min_size(egui::vec2(64.0, 26.0)))
            .on_hover_text("Raise the top cut by one layer")
            .clicked()
        {
            self.nudge_top_cut(step);
        }
        let footer = 118.0;
        let slider_h = (ui.available_height() - footer).max(72.0);
        let (min_z, max_z) = self.cut_limits();
        self.dual_cut_slider(ui, min_z, max_z, slider_h);
        if ui
            .add(egui::Button::new("▼").min_size(egui::vec2(64.0, 26.0)))
            .on_hover_text("Lower the top cut by one layer")
            .clicked()
        {
            self.nudge_top_cut(-step);
        }
        ui.label(format!("{:.2} mm", self.plate_view.section_z));
        ui.label(format!("bottom {:.2}", self.plate_view.section_lo));
        let mut cut = self.plate_view.section;
        if ui
            .checkbox(&mut cut, "Cut")
            .on_hover_text("Hide the model outside the two handles and fill each cut solid.")
            .changed()
        {
            self.plate_view.section = cut;
            self.cuts_custom = true;
            self.touch_view();
        }
    }

    /// Vertical range slider. The upper handle cuts down from the top.
    /// The lower handle cuts up from the bottom.
    fn dual_cut_slider(&mut self, ui: &mut egui::Ui, min_z: f32, max_z: f32, height: f32) -> bool {
        let (rect, response) =
            ui.allocate_exact_size(egui::vec2(36.0, height), egui::Sense::click_and_drag());
        let span = (max_z - min_z).max(0.2);
        let y_of = |v: f32| {
            let t = ((v - min_z) / span).clamp(0.0, 1.0);
            rect.bottom() - t * rect.height()
        };
        let v_of = |y: f32| {
            let t = ((rect.bottom() - y) / rect.height().max(1.0)).clamp(0.0, 1.0);
            min_z + t * span
        };
        let hi = self.plate_view.section_z.clamp(min_z, max_z);
        let lo = self.plate_view.section_lo.clamp(min_z, hi);
        let y_hi = y_of(hi);
        let y_lo = y_of(lo);
        let painter = ui.painter();
        let track = egui::Rect::from_center_size(rect.center(), egui::vec2(6.0, rect.height()));
        painter.rect_filled(track, 3.0, egui::Color32::from_rgb(48, 52, 58));
        let band = egui::Rect::from_min_max(
            egui::pos2(track.left(), y_hi.min(y_lo)),
            egui::pos2(track.right(), y_hi.max(y_lo)),
        );
        painter.rect_filled(band, 3.0, egui::Color32::from_rgb(176, 112, 42));
        painter.circle_filled(
            egui::pos2(rect.center().x, y_hi),
            7.0,
            egui::Color32::from_rgb(232, 168, 72),
        );
        painter.circle_filled(
            egui::pos2(rect.center().x, y_lo),
            7.0,
            egui::Color32::from_rgb(120, 156, 196),
        );
        let mut which = self.cut_drag;
        if response.drag_started() || response.clicked() {
            if let Some(pos) = response.interact_pointer_pos() {
                let d_hi = (pos.y - y_hi).abs();
                let d_lo = (pos.y - y_lo).abs();
                which = Some(if d_hi <= d_lo { 1 } else { 0 });
                self.cut_drag = which;
            }
        }
        if !response.dragged() && !response.drag_started() {
            return false;
        }
        let Some(pos) = response.interact_pointer_pos() else {
            return false;
        };
        let v = v_of(pos.y);
        let gap = 0.15;
        let (next_lo, next_hi) = if which == Some(1) {
            (lo, v.clamp(lo + gap, max_z))
        } else if which == Some(0) {
            (v.clamp(min_z, hi - gap), hi)
        } else {
            return false;
        };
        if (next_lo - self.plate_view.section_lo).abs() < 1e-4
            && (next_hi - self.plate_view.section_z).abs() < 1e-4
        {
            return false;
        }
        self.plate_view.section_lo = next_lo;
        self.plate_view.section_z = next_hi;
        self.plate_view.section = true;
        self.cuts_custom = true;
        self.touch_view();
        self.sync_preview_to_cut();
        true
    }

    fn viewport_scene(&mut self, ui: &mut egui::Ui) {
        let response = ui.allocate_response(ui.available_size(), egui::Sense::click_and_drag());
        let rect = response.rect;
        if response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                self.camera.distance =
                    (self.camera.distance * (-scroll * 0.0015).exp()).clamp(30.0, 2500.0);
            }
        }
        let shift = ui.input(|i| i.modifiers.shift);
        let select_move = self.tool == Tool::Select
            && matches!(self.doc.selection, Selection::Object(_))
            && response.dragged_by(egui::PointerButton::Primary)
            && !shift;
        if response.dragged_by(egui::PointerButton::Secondary)
            || (response.dragged_by(egui::PointerButton::Primary)
                && self.tool == Tool::Select
                && !select_move
                && !shift)
        {
            let d = response.drag_delta();
            // Dragging right turns the plate to the right, the same way a
            // grabbed model moves in Chitubox.
            self.camera.yaw -= d.x * 0.4;
            self.camera.pitch = (self.camera.pitch + d.y * 0.3).clamp(-80.0, 89.0);
            self.part_menu = None;
        }
        if response.dragged_by(egui::PointerButton::Middle)
            || (response.dragged_by(egui::PointerButton::Primary)
                && shift
                && self.tool == Tool::Select)
        {
            let d = response.drag_delta();
            self.camera.pan(d.x, d.y);
        }
        if response.dragged_by(egui::PointerButton::Primary)
            && (matches!(self.tool, Tool::Move | Tool::Rotate | Tool::Scale) || select_move)
        {
            self.drag_transform(&response);
        }
        if response.dragged_by(egui::PointerButton::Primary) && self.tool == Tool::Support {
            self.drag_support(&response);
        }
        if response.clicked_by(egui::PointerButton::Secondary) {
            self.open_part_menu(&response);
        }
        if response.clicked_by(egui::PointerButton::Primary)
            && matches!(
                self.tool,
                Tool::Support
                    | Tool::Drain
                    | Tool::Select
                    | Tool::Move
                    | Tool::Rotate
                    | Tool::Scale
                    | Tool::Mirror
                    | Tool::Hollow
                    | Tool::Measure
            )
        {
            self.part_menu = None;
            self.click_plate(&response);
        }
        let Some(renderer) = self.renderer.clone() else {
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "OpenGL is not available, so the plate view is off. The command line slicer still works.",
                egui::FontId::proportional(16.0),
                egui::Color32::LIGHT_GRAY,
            );
            return;
        };
        let frame = self.frame.clone();
        let camera = self.camera.clone();
        let aspect = (rect.width() / rect.height().max(1.0)).clamp(0.2, 5.0);
        let overhang = self.show_overhangs.then(|| {
            self.doc
                .edit_target()
                .and_then(|id| self.doc.object(id))
                .map(|obj| obj.support.style.overhang_deg)
                .unwrap_or(45.0)
        });
        let clip = self.view_clip();
        let ghost = self.hole_ghost(&response);
        let callback = egui::PaintCallback {
            rect,
            callback: Arc::new(egui_glow::CallbackFn::new(move |info, painter| {
                let gl = painter.gl().as_ref();
                let vp = info.viewport_in_pixels();
                unsafe {
                    gl.viewport(
                        vp.left_px,
                        vp.from_bottom_px,
                        vp.width_px.max(1),
                        vp.height_px.max(1),
                    );
                }
                let Ok(mut gpu) = renderer.lock() else {
                    return;
                };
                if let Some(frame) = &frame {
                    gpu.sync(gl, frame);
                }
                gpu.paint(gl, &camera, aspect, overhang, clip, None, ghost.as_slice());
            })),
        };
        ui.painter().add(callback);
        if self.doc.objects.is_empty() {
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "Drop an STL or OBJ here",
                egui::FontId::proportional(22.0),
                egui::Color32::from_rgb(180, 164, 140),
            );
        }
    }

    fn fit_view(&mut self) {
        let bounds = if let Selection::Object(id) = self.doc.selection {
            self.doc.object(id).and_then(Document::display_bounds)
        } else {
            let mut lo = Vec3::splat(f32::MAX);
            let mut hi = Vec3::splat(f32::MIN);
            let mut any = false;
            for obj in &self.doc.objects {
                if let Some((min, max)) = Document::display_bounds(obj) {
                    lo = lo.min(min);
                    hi = hi.max(max);
                    any = true;
                }
            }
            any.then_some((lo, hi))
        };
        let Some((min, max)) = bounds else {
            self.camera = Camera::looking_at_plate(self.plate());
            return;
        };
        let span = (max - min).max_element().max(8.0);
        self.camera.target = (min + max) * 0.5;
        self.camera.distance = (span * 2.4).clamp(30.0, 2500.0);
    }

    fn drag_transform(&mut self, response: &egui::Response) {
        let Selection::Object(id) = self.doc.selection else {
            return;
        };
        let delta = response.drag_delta();
        if response.drag_started() && !self.gesture {
            self.doc.push_xform_undo(id);
            self.gesture = true;
        }
        let aspect = (response.rect.width() / response.rect.height().max(1.0)).clamp(0.2, 5.0);
        match self.tool {
            Tool::Move | Tool::Select => {
                let shift = response.ctx.input(|i| i.modifiers.shift);
                if shift {
                    if let Some(obj) = self.doc.object_mut(id) {
                        obj.position.z = (obj.position.z - delta.y * 0.05).max(0.0);
                    }
                } else if let Some(pointer) = response.interact_pointer_pos() {
                    let rel_x = (pointer.x - response.rect.left()) / response.rect.width().max(1.0);
                    let rel_y = (pointer.y - response.rect.top()) / response.rect.height().max(1.0);
                    let prev = egui::pos2(pointer.x - delta.x, pointer.y - delta.y);
                    let prev_rel_x =
                        (prev.x - response.rect.left()) / response.rect.width().max(1.0);
                    let prev_rel_y =
                        (prev.y - response.rect.top()) / response.rect.height().max(1.0);
                    let z = self.doc.object(id).map(|o| o.position.z).unwrap_or(0.0);
                    let (o0, d0) = self.camera.ray(prev_rel_x, prev_rel_y, aspect);
                    let (o1, d1) = self.camera.ray(rel_x, rel_y, aspect);
                    if let (Some(a), Some(b)) = (hit_z(o0, d0, z), hit_z(o1, d1, z)) {
                        if let Some(obj) = self.doc.object_mut(id) {
                            obj.position.x += b.x - a.x;
                            obj.position.y += b.y - a.y;
                            if self.snap_mm {
                                obj.position.x = obj.position.x.round();
                                obj.position.y = obj.position.y.round();
                            }
                        }
                    }
                }
            }
            Tool::Rotate => {
                let snap = response.ctx.input(|i| i.modifiers.command);
                if let Some(obj) = self.doc.object_mut(id) {
                    obj.rotation_deg.z += delta.x * 0.4;
                    obj.rotation_deg.x += delta.y * 0.4;
                    if snap {
                        obj.rotation_deg.x = (obj.rotation_deg.x / 15.0).round() * 15.0;
                        obj.rotation_deg.z = (obj.rotation_deg.z / 15.0).round() * 15.0;
                    }
                }
            }
            Tool::Scale => {
                if let Some(obj) = self.doc.object_mut(id) {
                    let factor = 1.0 - delta.y * 0.005;
                    obj.scale *= factor;
                    obj.scale.x = nonzero(obj.scale.x);
                    obj.scale.y = nonzero(obj.scale.y);
                    obj.scale.z = nonzero(obj.scale.z);
                }
            }
            _ => {}
        }
        self.doc.touch_xform();
        self.invalidate_slice();
    }

    fn open_part_menu(&mut self, response: &egui::Response) {
        let Some(pointer) = response.interact_pointer_pos() else {
            return;
        };
        let rel_x = (pointer.x - response.rect.left()) / response.rect.width().max(1.0);
        let rel_y = (pointer.y - response.rect.top()) / response.rect.height().max(1.0);
        let aspect = (response.rect.width() / response.rect.height().max(1.0)).clamp(0.2, 5.0);
        let (origin, dir) = self.camera.ray(rel_x, rel_y, aspect);
        if let Some((id, _, _)) = self.visible_hit(origin, dir) {
            if !self.picked.contains(&id) {
                self.picked.clear();
                self.picked.insert(id);
            }
            self.doc.selection = Selection::Object(id);
            self.doc.touch_xform();
            self.part_menu = Some(pointer);
            self.part_menu_fresh = true;
        }
    }

    fn model_stamp(&self) -> u64 {
        let mut hash = self.doc.objects.len() as u64;
        for obj in &self.doc.objects {
            let nums = [
                obj.id,
                obj.mesh_rev,
                obj.position.x.to_bits() as u64,
                obj.position.y.to_bits() as u64,
                obj.position.z.to_bits() as u64,
                obj.rotation_deg.x.to_bits() as u64,
                obj.rotation_deg.y.to_bits() as u64,
                obj.rotation_deg.z.to_bits() as u64,
                obj.scale.x.to_bits() as u64,
                obj.scale.y.to_bits() as u64,
                obj.scale.z.to_bits() as u64,
            ];
            for n in nums {
                hash = hash.wrapping_mul(0x9E37_79B1).wrapping_add(n);
            }
        }
        hash
    }

    fn sync_islands(&mut self) {
        if self.island_marks.is_empty() {
            return;
        }
        if self.model_stamp() != self.island_stamp {
            self.island_marks.clear();
            self.island_gen = self.island_gen.wrapping_add(1);
        }
    }

    fn paint_islands(&self, frame: &mut PlateFrame) {
        if self.model_stamp() != self.island_stamp {
            return;
        }
        let color = [0.90, 0.22, 0.18];
        for &(x, y, z) in &self.island_marks {
            let arm = 0.7;
            frame.add_line(Vec3::new(x - arm, y, z), Vec3::new(x + arm, y, z), color);
            frame.add_line(Vec3::new(x, y - arm, z), Vec3::new(x, y + arm, z), color);
            frame.add_line(Vec3::new(x, y, z), Vec3::new(x, y, z + 1.4), color);
        }
    }

    fn island_near(&self, origin: Vec3, dir: Vec3) -> Option<Vec3> {
        if self.model_stamp() != self.island_stamp {
            return None;
        }
        let limit = self.tip_pick_mm();
        let mut best: Option<(f32, Vec3)> = None;
        for &(x, y, z) in &self.island_marks {
            let tip = Vec3::new(x, y, z);
            let along = (tip - origin).dot(dir);
            if along < 0.0 {
                continue;
            }
            let dist = (tip - (origin + dir * along)).length();
            if dist <= limit && best.map(|(old, _)| dist < old).unwrap_or(true) {
                best = Some((dist, tip));
            }
        }
        best.map(|(_, point)| point)
    }

    fn model_under(&self, point: Vec3) -> Option<u64> {
        let contains = |id: u64| {
            self.doc
                .object(id)
                .and_then(Document::display_bounds)
                .is_some_and(|(min, max)| {
                    point.x >= min.x - 1.5
                        && point.x <= max.x + 1.5
                        && point.y >= min.y - 1.5
                        && point.y <= max.y + 1.5
                        && point.z >= min.z - 1.0
                        && point.z <= max.z + 1.5
                })
        };
        if let Some(id) = self.doc.edit_target() {
            if contains(id) {
                return Some(id);
            }
        }
        self.doc
            .objects
            .iter()
            .find(|obj| contains(obj.id))
            .map(|obj| obj.id)
    }

    fn plant_mark(&mut self, point: Vec3) {
        let Some(id) = self.model_under(point) else {
            self.status = "That island mark is not on a model.".into();
            return;
        };
        self.doc.add_support_at(point, id);
        self.invalidate_slice();
        self.status =
            "Planted a support on that island. The other red marks stay until a model moves or you slice again."
                .into();
    }

    fn tip_pick_mm(&self) -> f32 {
        (self.camera.distance * 0.02).clamp(1.2, 8.0)
    }

    fn pointer_ray(&self, response: &egui::Response) -> Option<(Vec3, Vec3)> {
        let pointer = response.interact_pointer_pos()?;
        let rel_x = (pointer.x - response.rect.left()) / response.rect.width().max(1.0);
        let rel_y = (pointer.y - response.rect.top()) / response.rect.height().max(1.0);
        let aspect = (response.rect.width() / response.rect.height().max(1.0)).clamp(0.2, 5.0);
        Some(self.camera.ray(rel_x, rel_y, aspect))
    }

    fn drag_support(&mut self, response: &egui::Response) {
        let Some((origin, dir)) = self.pointer_ray(response) else {
            return;
        };
        if self.erase_supports {
            if response.drag_started() {
                self.gesture = true;
            }
            self.erase_at(origin, dir, !self.stroke_saved);
            return;
        }
        if response.drag_started() && !self.gesture {
            if let Some(id) = self.doc.support_near_ray(origin, dir, self.tip_pick_mm()) {
                self.doc.selection = Selection::Support(id);
                self.doc.remember_supports();
                self.gesture = true;
            }
        }
        if !self.gesture {
            return;
        }
        let Selection::Support(id) = self.doc.selection else {
            return;
        };
        let Some((hit_id, point, _)) = self.visible_hit(origin, dir) else {
            return;
        };
        let owner = self
            .doc
            .supports
            .iter()
            .find(|s| s.id == id)
            .map(|s| s.object_id);
        if owner == Some(hit_id) && self.doc.relocate_support(id, point) {
            self.invalidate_slice();
        }
    }

    fn erase_at(&mut self, origin: Vec3, dir: Vec3, record_undo: bool) {
        let hit = self.visible_hit(origin, dir);
        let object_id = self.doc.edit_target().or_else(|| hit.map(|(id, _, _)| id));
        let Some(object_id) = object_id else {
            self.status = "Select a model, then erase tips on that model.".into();
            return;
        };
        let point = hit.map(|(_, point, _)| point).or_else(|| {
            let id =
                self.doc
                    .support_near_ray(origin, dir, self.erase_mm.max(self.tip_pick_mm()))?;
            self.doc
                .supports
                .iter()
                .find(|s| s.id == id)
                .map(|s| Vec3::new(s.x, s.y, s.z_top))
        });
        let Some(point) = point else {
            return;
        };
        let limit = self.erase_mm.max(0.0);
        let pending = self
            .doc
            .supports
            .iter()
            .filter(|support| {
                support.object_id == object_id
                    && (Vec3::new(support.x, support.y, support.z_top) - point).length() <= limit
            })
            .count();
        if pending == 0 {
            return;
        }
        if record_undo {
            self.doc.remember_supports();
            self.stroke_saved = true;
        }
        let removed = self
            .doc
            .erase_supports_near(object_id, point, self.erase_mm);
        if removed > 0 {
            self.invalidate_slice();
            self.status = format!("Removed {removed} tips. Undo puts them back.");
        }
    }

    fn view_clip(&self) -> Option<(f32, f32)> {
        if !self.plate_view.section {
            return None;
        }
        let mut lo = self.plate_view.section_lo;
        let mut hi = self.plate_view.section_z;
        if lo > hi {
            std::mem::swap(&mut lo, &mut hi);
        }
        Some((lo, hi))
    }

    fn hole_ghost(&self, response: &egui::Response) -> Vec<f32> {
        if self.tool != Tool::Drain {
            return Vec::new();
        }
        let Some(pos) = response.hover_pos() else {
            return Vec::new();
        };
        let rect = response.rect;
        let rel_x = (pos.x - rect.left()) / rect.width().max(1.0);
        let rel_y = (pos.y - rect.top()) / rect.height().max(1.0);
        let aspect = (rect.width() / rect.height().max(1.0)).clamp(0.2, 5.0);
        let (origin, dir) = self.camera.ray(rel_x, rel_y, aspect);
        let Some((_, point, normal)) = self.visible_hit(origin, dir) else {
            return Vec::new();
        };
        let axis = self.doc.hole_axis(normal, dir);
        let (outer, inner, extend, depth) = self.doc.hole_dims();
        let mesh = crate::supports::hole_mesh(point, axis, outer, inner, extend, depth);
        viewport::colored_tris(&mesh, [0.95, 0.72, 0.20])
    }

    fn visible_hit(&self, origin: Vec3, dir: Vec3) -> Option<(u64, Vec3, Vec3)> {
        self.doc
            .raycast_visible(origin, dir, &self.hidden_models, self.view_clip())
    }

    fn show_help(&mut self, ctx: &egui::Context) {
        if !self.help_open {
            return;
        }
        let mut open = true;
        egui::Window::new("How to print")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(460.0)
            .show(ctx, |ui| {
                ui.label("1. Open a model, or drop it on the window.");
                ui.label("2. Pick your printer and your resin. The times fill in from a published table.");
                ui.label("3. If the model is hollow, punch a hole at the bottom so resin can drain.");
                ui.label("4. If a part floats or has a steep underside, add supports. They lift the model off the bed.");
                ui.label("5. Slice and save. Copy that file onto a USB stick and print it from the printer.");
                ui.label("File → Save plate keeps the models, supports, and holes in an .amber file. Ctrl+Shift+S saves it again. That is not the file the printer reads.");
                ui.separator();
                ui.label("Simple is the short path. Workshop is every control: rafts, rest times, compensation, and the rest.");
                ui.label("Right-click a model for the same edits. Right-drag orbits, and you can swing under the bed. The bed turns clear so you can click an underside.");
                ui.label("The bar on the right of Prepare cuts the model. The top handle starts at the top of the part so you can drag down through it, and the bottom handle cuts up from the base. Each cut is one solid face, the way a model viewer caps a clip, so the opening stays smooth. View → Cut the view is the same pair of heights. The checkbox beside a model hides it in the view. It still prints. Tips only draws the contact points. After a slice, red marks on the plate are islands. Click one with Support to plant a tip there.");
                ui.label("The Hole tool draws the punch under the pointer. Perpendicular to the model follows the surface. Perpendicular to the screen follows the camera. Keep Hole saves the removed resin as its own model, set beside the part, so you can print it and glue it back.");
                ui.separator();
                ui.label("Ctrl+O open    Ctrl+Shift+S save the plate    Ctrl+S save the sliced file");
                ui.label("Ctrl+Z undo    Ctrl+Y redo    Ctrl+D duplicate    Delete remove");
                ui.label("Ctrl+Enter slice    Ctrl+S save    F1 this page");
                ui.label("On the plate, the arrow keys nudge the selected model by 1 mm (Shift is 0.1 mm). A selected tip moves the same way and stays on the model. F fits the camera. In the layer view, the arrows step through layers.");
                ui.separator();
                ui.label("Amber is free. If it saves you a print, you can buy Tyler a coffee.");
                let coffee = egui::Button::new(
                    egui::RichText::new("Buy me a coffee")
                        .color(egui::Color32::BLACK)
                        .strong(),
                )
                .fill(egui::Color32::from_rgb(255, 221, 0));
                if ui.add(coffee).clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(
                        "https://buymeacoffee.com/krickatthedisco",
                    ));
                }
                ui.separator();
                ui.label(format!(
                    "Amber {}  ·  built for the Anycubic Photon M3 Max, and the other printers in the list.",
                    crate::VERSION
                ));
            });
        self.help_open = open;
    }

    fn show_part_menu(&mut self, ctx: &egui::Context) {
        let Some(pos) = self.part_menu else {
            return;
        };
        let Some(id) = self.doc.edit_target() else {
            self.part_menu = None;
            return;
        };
        let name = self
            .doc
            .object(id)
            .map(|obj| obj.name.clone())
            .unwrap_or_else(|| "Model".into());
        let mut close = false;
        let mut menu_rect = egui::Rect::NOTHING;
        egui::Area::new(egui::Id::new("amber_part_menu"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .show(ctx, |ui| {
                let frame = egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(220.0);
                    ui.label(egui::RichText::new(name).strong());
                    ui.separator();
                    close |= self.part_menu_buttons(ui, id);
                });
                menu_rect = frame.response.rect;
            });
        let outside = ctx.input(|i| {
            i.key_pressed(egui::Key::Escape)
                || (i.pointer.any_click()
                    && i.pointer
                        .interact_pos()
                        .is_some_and(|p| !menu_rect.contains(p)))
        });
        if outside && !self.part_menu_fresh {
            close = true;
        }
        self.part_menu_fresh = false;
        if close {
            self.part_menu = None;
        }
    }

    fn part_menu_buttons(&mut self, ui: &mut egui::Ui, id: u64) -> bool {
        let mut close = false;
        let click = |ui: &mut egui::Ui, label: &str| ui.button(label).clicked();
        if click(ui, "Drop to bed") {
            self.doc.push_xform_undo(id);
            self.doc.drop_object(id);
            self.invalidate_slice();
            close = true;
        }
        if click(ui, "Center on plate") {
            self.doc.push_xform_undo(id);
            self.doc
                .center_on_plate(id, self.machine.size_x, self.machine.size_y);
            self.invalidate_slice();
            close = true;
        }
        if click(ui, "Auto orient") {
            self.doc.auto_orient(id);
            self.invalidate_slice();
            self.status = "Oriented to cut overhangs, then dropped to the bed.".into();
            close = true;
        }
        if click(ui, "Largest face down") {
            self.doc.place_on_largest_face(id);
            self.invalidate_slice();
            close = true;
        }
        if click(ui, "+90° Z") {
            self.turn(id, 2, 90.0);
            close = true;
        }
        if click(ui, "−90° Z") {
            self.turn(id, 2, -90.0);
            close = true;
        }
        if click(ui, "Cut in half on Z") {
            let z = self
                .doc
                .object(id)
                .and_then(Document::world_bounds)
                .map(|(min, max)| (min.z + max.z) * 0.5)
                .unwrap_or(self.cut_z);
            self.cut_z = z;
            match self.doc.cut_at_z(id, z) {
                Ok(_) => {
                    self.invalidate_slice();
                    self.status = "Cut the model and kept both pieces.".into();
                }
                Err(err) => self.status = err.into(),
            }
            close = true;
        }
        if click(ui, "Reset scale to 100%") {
            self.doc.push_xform_undo(id);
            if let Some(obj) = self.doc.object_mut(id) {
                obj.scale = Vec3::ONE;
            }
            self.doc.touch_xform();
            self.invalidate_slice();
            close = true;
        }
        ui.separator();
        if click(ui, "Mirror X") {
            self.apply_mirror(id, 0);
            close = true;
        }
        if click(ui, "Mirror Y") {
            self.apply_mirror(id, 1);
            close = true;
        }
        if click(ui, "Mirror Z") {
            self.apply_mirror(id, 2);
            close = true;
        }
        ui.separator();
        let hollow = self.doc.object(id).is_some_and(|obj| obj.hollow);
        if click(ui, if hollow { "Make solid" } else { "Hollow" }) {
            self.toggle_hollow(id);
            close = true;
        }
        if click(ui, "Punch hole at the bottom") {
            self.punch_hole(id);
            close = true;
        }
        if click(ui, "Place holes by clicking") {
            self.tool = Tool::Drain;
            self.status =
                "Click the shell to punch a drain. Orbit under the bed if you need the underside."
                    .into();
            close = true;
        }
        ui.separator();
        if click(ui, "Add supports") {
            self.grow_supports(false);
            close = true;
        }
        if click(ui, "Supports to the bed") {
            self.grow_supports(true);
            close = true;
        }
        let hidden = self.hidden_models.contains(&id);
        if click(
            ui,
            if hidden {
                "Show this model"
            } else {
                "Hide this model"
            },
        ) {
            self.toggle_hidden(id);
            close = true;
        }
        if click(ui, "Cut the view through this model") {
            if let Some((min, max)) = self.doc.object(id).and_then(Document::display_bounds) {
                self.plate_view.section = true;
                self.plate_view.section_lo = min.z;
                self.plate_view.section_z = ((min.z + max.z) * 0.5).clamp(0.0, self.machine.size_z);
                self.cuts_custom = true;
                self.touch_view();
                self.status = "The model outside the cut is hidden, and the cut is filled solid. Click the surface you can see to place a support.".into();
            }
            close = true;
        }
        if click(ui, "Show only the contact points") {
            self.plate_view.tips_only();
            self.touch_view();
            close = true;
        }
        if click(ui, "Show all pieces") {
            self.plate_view.show_all_pieces();
            self.touch_view();
            close = true;
        }
        if click(ui, "Clear supports") {
            if self.doc.clear_supports() {
                self.invalidate_slice();
            }
            close = true;
        }
        let raft = self.doc.object(id).is_some_and(|obj| obj.support.raft);
        if click(
            ui,
            if raft {
                "Turn skate raft off"
            } else {
                "Turn skate raft on"
            },
        ) {
            if let Some(obj) = self.doc.object_mut(id) {
                obj.support.raft = !raft;
            }
            self.doc.sync_defaults_from(id);
            self.doc.touch();
            self.invalidate_slice();
            close = true;
        }
        ui.separator();
        ui.separator();
        if click(ui, "Split into objects") {
            match self.doc.split_shells(id, false) {
                Ok(n) => {
                    self.picked.clear();
                    self.picked.insert(id);
                    self.invalidate_slice();
                    self.status = format!("Split into {n} objects.");
                }
                Err(err) => self.status = err.into(),
            }
            close = true;
        }
        if click(ui, "Split into parts") {
            match self.doc.split_shells(id, true) {
                Ok(n) => {
                    self.picked.clear();
                    self.picked.insert(id);
                    self.invalidate_slice();
                    self.status = format!("Split into {n} parts of one object.");
                }
                Err(err) => self.status = err.into(),
            }
            close = true;
        }
        if click(ui, "Assemble selected") {
            let ids = self.picked_ids();
            match self.doc.assemble(&ids) {
                Ok(()) => {
                    self.invalidate_slice();
                    self.status =
                        "Those models are parts of one object. A negative cuts only its siblings."
                            .into();
                }
                Err(err) => self.status = err.into(),
            }
            close = true;
        }
        let negative = self
            .doc
            .object(id)
            .is_some_and(|obj| obj.kind == VolumeKind::Negative);
        if click(
            ui,
            if negative {
                "Use as a part"
            } else {
                "Use as a negative volume"
            },
        ) {
            let kind = if negative {
                VolumeKind::Part
            } else {
                VolumeKind::Negative
            };
            self.doc.set_volume_kind(id, kind);
            self.invalidate_slice();
            self.status = if kind == VolumeKind::Negative {
                "This volume cuts the other parts of its object. Assemble it onto a model if it is on its own.".into()
            } else {
                "This volume adds resin again.".into()
            };
            close = true;
        }
        let others: Vec<u64> = self
            .picked_ids()
            .into_iter()
            .filter(|other| *other != id)
            .collect();
        let tool = if others.len() == 1 {
            Some(others[0])
        } else {
            None
        };
        let boolean = |app: &mut Self, ui: &mut egui::Ui, label: &str, op: BooleanOp| -> bool {
            if !ui.button(label).clicked() {
                return false;
            }
            let Some(tool_id) = tool else {
                app.status = "Shift-click the model to union, subtract, or intersect, then run it from this menu.".into();
                return true;
            };
            match app.doc.boolean_objects(id, tool_id, op) {
                Ok(()) => {
                    app.picked.clear();
                    app.picked.insert(id);
                    app.invalidate_slice();
                    app.status = "Baked the boolean into one mesh and removed the other model. Undo puts both back.".into();
                }
                Err(err) => app.status = err.into(),
            }
            true
        };
        if boolean(self, ui, "Union with the other selection", BooleanOp::Union) {
            close = true;
        }
        if boolean(
            self,
            ui,
            "Subtract the other selection",
            BooleanOp::Difference,
        ) {
            close = true;
        }
        if boolean(
            self,
            ui,
            "Intersect with the other selection",
            BooleanOp::Intersection,
        ) {
            close = true;
        }
        ui.separator();
        if click(ui, "Arrange on the RERF grid") {
            self.arrange_rerf(id);
            close = true;
        }
        if click(ui, "Duplicate") {
            self.doc.duplicate(id);
            self.invalidate_slice();
            close = true;
        }
        if click(ui, "Repair") {
            self.doc.repair(id);
            self.invalidate_slice();
            self.status =
                "Welded duplicate corners and flipped the shell if it was inside out.".into();
            close = true;
        }
        if click(ui, "Flip normals") {
            self.doc.flip_normals(id);
            self.invalidate_slice();
            close = true;
        }
        if click(ui, "Delete") {
            self.doc.delete_selection();
            self.invalidate_slice();
            close = true;
        }
        close
    }

    fn click_plate(&mut self, response: &egui::Response) {
        let Some(pointer) = response.interact_pointer_pos() else {
            return;
        };
        let rel_x = (pointer.x - response.rect.left()) / response.rect.width().max(1.0);
        let rel_y = (pointer.y - response.rect.top()) / response.rect.height().max(1.0);
        let aspect = (response.rect.width() / response.rect.height().max(1.0)).clamp(0.2, 5.0);
        let (origin, dir) = self.camera.ray(rel_x, rel_y, aspect);
        match self.tool {
            Tool::Support => {
                if self.erase_supports {
                    self.erase_at(origin, dir, true);
                } else if let Some(id) = self.doc.support_near_ray(origin, dir, self.tip_pick_mm())
                {
                    self.doc.selection = Selection::Support(id);
                    self.doc.touch_xform();
                    self.status =
                        "Tip selected. Drag it to move the contact. Delete removes it.".into();
                } else if let Some(point) = self.island_near(origin, dir) {
                    self.plant_mark(point);
                } else if let Some((id, point, _)) = self.visible_hit(origin, dir) {
                    self.doc.add_support_at(point, id);
                    self.invalidate_slice();
                }
            }
            Tool::Drain => {
                if let Some((id, point, normal)) = self.visible_hit(origin, dir) {
                    let kept = self.doc.add_hole(point, normal, dir, id);
                    self.invalidate_slice();
                    self.status = if kept {
                        "Punched a hole and set the plug beside the model. Print that piece and glue it back in.".into()
                    } else {
                        "Punched a hole. Undo removes it.".into()
                    };
                }
            }
            Tool::Measure => {
                let point = self
                    .visible_hit(origin, dir)
                    .map(|(_, p, _)| p)
                    .or_else(|| {
                        hit_z(origin, dir, 0.0).filter(|p| {
                            p.x >= -1.0
                                && p.y >= -1.0
                                && p.x <= self.machine.size_x + 1.0
                                && p.y <= self.machine.size_y + 1.0
                        })
                    });
                if let Some(point) = point {
                    if self.measure_a.is_none() || self.measure_b.is_some() {
                        self.measure_a = Some(point);
                        self.measure_b = None;
                    } else {
                        self.measure_b = Some(point);
                    }
                    self.measure_gen = self.measure_gen.wrapping_add(1);
                }
            }
            _ => {
                let shift = response.ctx.input(|i| i.modifiers.shift);
                if let Some((id, _, _)) = self.visible_hit(origin, dir) {
                    self.pick_model(id, shift);
                } else if !shift {
                    self.picked.clear();
                    self.doc.selection = Selection::None;
                    self.doc.touch_xform();
                }
            }
        }
    }

    fn preview(&mut self, ui: &mut egui::Ui) {
        let mark_note = !self.island_marks.is_empty() && self.model_stamp() == self.island_stamp;
        let Some(slice) = &self.slice else {
            ui.label("Slice the plate to see layers. Islands show up in red.");
            return;
        };
        let count = slice.layers.len();
        if count == 0 {
            return;
        }
        self.preview_index = self.preview_index.min(count - 1);
        let clock = slice_clock(slice);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(&clock).strong());
            if ui.button("Copy").clicked() {
                ui.ctx().copy_text(clock.clone());
                self.status = "Copied the print summary.".into();
            }
        });
        if mark_note {
            ui.label(
                "Red marks on the plate (Prepare) are these islands. Support, then click a mark.",
            );
        }
        let (caption, seals) = {
            let layer = &slice.layers[self.preview_index];
            (
                format!(
                    "Layer {} / {}    z {:.2} mm    {:.2} s    {} px    {} islands",
                    layer.index + 1,
                    count,
                    layer.z_top_mm,
                    layer.exposure_s,
                    layer.nonzero,
                    layer.islands.len()
                ),
                layer.seals_cavity_px,
            )
        };
        let avail = ui.available_size();
        let bar_w = 92.0;
        ui.horizontal(|ui| {
            ui.allocate_ui_with_layout(
                egui::vec2((avail.x - bar_w - 8.0).max(1.0), avail.y),
                egui::Layout::top_down(egui::Align::Center),
                |ui| {
                    ui.label(caption);
                    if seals > 0 {
                        ui.colored_label(
                            egui::Color32::from_rgb(90, 160, 220),
                            format!("seals a cavity ({seals} px)"),
                        );
                    }
                    self.preview_image(ui);
                },
            );
            ui.allocate_ui_with_layout(
                egui::vec2(bar_w, avail.y),
                egui::Layout::top_down(egui::Align::Center),
                |ui| self.layer_bar(ui, count),
            );
        });
    }

    fn layer_bar(&mut self, ui: &mut egui::Ui, count: usize) {
        ui.label("Layer");
        if ui
            .add(egui::Button::new("▲").min_size(egui::vec2(64.0, 28.0)))
            .on_hover_text("Up one layer")
            .clicked()
        {
            self.preview_index = (self.preview_index + 1).min(count - 1);
        }
        // egui's vertical slider uses Spacing::slider_width for its length,
        // not the rectangle passed to add_sized. Stretch it to the gap
        // between the step buttons.
        let footer = 128.0;
        let slider_h = (ui.available_height() - footer).max(64.0);
        ui.spacing_mut().slider_width = slider_h;
        let mut layer = self.preview_index;
        ui.add(
            egui::Slider::new(&mut layer, 0..=count - 1)
                .vertical()
                .show_value(false),
        );
        self.preview_index = layer;
        if ui
            .add(egui::Button::new("▼").min_size(egui::vec2(64.0, 28.0)))
            .on_hover_text("Down one layer")
            .clicked()
        {
            self.preview_index = self.preview_index.saturating_sub(1);
        }
        ui.add_space(4.0);
        ui.label("Go to");
        let mut text = format!("{}", self.preview_index + 1);
        let response = ui.add(egui::TextEdit::singleline(&mut text).desired_width(64.0));
        if response.changed() {
            if let Ok(n) = text.trim().parse::<usize>() {
                if n >= 1 {
                    self.preview_index = (n - 1).min(count - 1);
                }
            }
        }
        ui.label(format!("/ {count}"));
    }

    fn preview_image(&mut self, ui: &mut egui::Ui) {
        if self.preview_for != Some(self.preview_index) {
            let decoded = self.slice.as_ref().and_then(|slice| {
                let layer = slice.layers.get(self.preview_index)?;
                slice::preview_rgba(&layer.rle, slice.width, slice.height, 1400, &layer.islands)
                    .ok()
            });
            if let Some((w, h, rgba)) = decoded {
                let image =
                    egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
                self.preview_tex = Some(ui.ctx().load_texture(
                    "layer-preview",
                    image,
                    egui::TextureOptions::LINEAR,
                ));
                self.preview_for = Some(self.preview_index);
            }
        }
        if let Some(tex) = &self.preview_tex {
            let size = ui.available_size();
            let aspect = tex.aspect_ratio();
            let mut w = size.x;
            let mut h = w / aspect;
            if h > size.y {
                h = size.y;
                w = h * aspect;
            }
            ui.image((tex.id(), egui::vec2(w.max(1.0), h.max(1.0))));
        }
    }
}

fn grid_drag(
    ui: &mut egui::Ui,
    value: &mut Vec3,
    speed: f32,
    suffix: &str,
    started: &mut bool,
    changed: &mut bool,
) {
    ui.horizontal(|ui| {
        for (label, slot) in [
            ("X", &mut value.x),
            ("Y", &mut value.y),
            ("Z", &mut value.z),
        ] {
            ui.label(label);
            let response = ui.add(egui::DragValue::new(slot).speed(speed).suffix(suffix));
            *started |= response.drag_started();
            *changed |= response.changed();
        }
    });
}

fn hole_field(ui: &mut egui::Ui, label: &str, value: &mut f32, min: f32, max: f32) -> bool {
    ui.label(label);
    ui.add(
        egui::DragValue::new(value)
            .speed(0.01)
            .range(min..=max)
            .fixed_decimals(3)
            .min_decimals(3),
    )
    .changed()
}

fn drag_f32(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    speed: f32,
    min: f32,
    max: f32,
    suffix: &str,
) -> bool {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.add(
            egui::DragValue::new(value)
                .speed(speed)
                .range(min..=max)
                .suffix(suffix),
        )
        .changed()
    })
    .inner
}

fn hit_z(origin: Vec3, dir: Vec3, z: f32) -> Option<Vec3> {
    if dir.z.abs() < 1e-5 {
        return None;
    }
    let t = (z - origin.z) / dir.z;
    if t < 0.0 {
        None
    } else {
        Some(origin + dir * t)
    }
}

fn nonzero(v: f32) -> f32 {
    if v.abs() < 0.01 {
        if v.is_sign_negative() {
            -0.01
        } else {
            0.01
        }
    } else {
        v
    }
}

fn sanitize_filename(name: &str, ext: &str) -> String {
    let mut stem = name.trim().to_string();
    for suffix in [".pm3m", ".pm3", ".sl1", ".ctb", ".goo", ".stl", ".obj"] {
        if stem.len() > suffix.len() && stem.to_ascii_lowercase().ends_with(suffix) {
            stem.truncate(stem.len() - suffix.len());
        }
    }
    if let Some((left, _)) = stem.rsplit_once('.') {
        if left.len() >= 3 {
            stem = left.to_string();
        }
    }
    stem.retain(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if stem.is_empty() {
        stem = "print".into();
    }
    format!("{stem}.{ext}")
}

fn with_amber_extension(path: PathBuf) -> PathBuf {
    if path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("amber"))
    {
        path
    } else {
        path.with_extension("amber")
    }
}

fn default_export_name(doc: &Document, ext: &str) -> String {
    let stem = doc
        .objects
        .first()
        .map(|o| o.name.as_str())
        .unwrap_or("print");
    sanitize_filename(stem, ext)
}

fn resin_catalog() -> impl Iterator<Item = &'static Resin> {
    resins::RESINS.iter().chain(community::EXTRA_RESINS.iter())
}

fn find_resin(name: &str) -> Option<&'static Resin> {
    resin_catalog().find(|resin| resin.name == name)
}

fn lookup_profile(
    resin_id: &str,
    machine_id: &str,
    layer_mm: f32,
) -> Option<&'static ResinProfile> {
    resins::profile_for(resin_id, machine_id, layer_mm)
        .or_else(|| community::profile_for(resin_id, machine_id, layer_mm))
}

fn slice_eta(started: std::time::Instant, done: u32, total: u32) -> String {
    if done < 3 || done >= total {
        return String::new();
    }
    let elapsed = started.elapsed().as_secs_f32();
    let remain = elapsed / done as f32 * (total - done) as f32;
    if remain < 90.0 {
        format!("  about {:.0} s left", remain.max(1.0))
    } else {
        format!("  about {:.0} min left", remain / 60.0)
    }
}

fn slice_clock(slice: &Slice) -> String {
    let expose: f32 = slice.layers.iter().map(|layer| layer.exposure_s).sum();
    let motion = (slice.seconds as f32 - expose).max(0.0);
    let minutes = slice.seconds / 60;
    let extra = slice.seconds % 60;
    format!(
        "{minutes} min {extra} s on the printer  ·  {:.0} min of light, {:.0} min of lifting  ·  {:.2} ml  ·  {:.1} g",
        expose / 60.0,
        motion / 60.0,
        slice.cured_ml,
        slice.weight_g
    )
}

fn slice_bytes(slice: &Slice) -> usize {
    80_000 + slice.layers.iter().map(|l| l.rle.len()).sum::<usize>()
}

fn write_png(path: &std::path::Path, w: u32, h: u32, rgba: &[u8]) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut encoder = png::Encoder::new(file, w, h);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(rgba).map_err(|e| e.to_string())
}

fn fit_on_plate(mesh: &mut crate::mesh::Mesh, plate: Vec3) {
    let Some((min, max)) = mesh.bounds() else {
        return;
    };
    let w = (max[0] - min[0]).max(0.2);
    let d = (max[1] - min[1]).max(0.2);
    let h = (max[2] - min[2]).max(0.2);
    let margin = 8.0_f32;
    let fit = ((plate.x - margin) / w)
        .min((plate.y - margin) / d)
        .min((plate.z - margin) / h)
        .min(1.0);
    if fit >= 0.999 {
        return;
    }
    let cx = (min[0] + max[0]) * 0.5;
    let cy = (min[1] + max[1]) * 0.5;
    for v in &mut mesh.vertices {
        v[0] = cx + (v[0] - cx) * fit;
        v[1] = cy + (v[1] - cy) * fit;
        v[2] = min[2] + (v[2] - min[2]) * fit;
    }
}

struct Palette {
    bg: [u8; 3],
    panel: [u8; 3],
    extreme: [u8; 3],
    faint: [u8; 3],
    select: [u8; 3],
    accent: [u8; 3],
    widget: [u8; 3],
    hover: [u8; 3],
    text: [u8; 3],
    muted: [u8; 3],
}

fn palette(mode: ThemeMode, scheme: Scheme) -> Palette {
    let dark = mode == ThemeMode::Dark;
    match (dark, scheme) {
        (true, Scheme::Amber) => Palette {
            bg: [22, 24, 27],
            panel: [30, 33, 37],
            extreme: [16, 17, 19],
            faint: [38, 41, 46],
            select: [176, 92, 32],
            accent: [214, 154, 62],
            widget: [42, 46, 51],
            hover: [58, 52, 40],
            text: [232, 224, 210],
            muted: [228, 220, 206],
        },
        (false, Scheme::Amber) => Palette {
            bg: [244, 236, 224],
            panel: [252, 247, 238],
            extreme: [230, 218, 200],
            faint: [236, 226, 210],
            select: [214, 148, 52],
            accent: [140, 78, 16],
            widget: [236, 226, 208],
            hover: [248, 214, 168],
            text: [42, 32, 22],
            muted: [72, 56, 40],
        },
        (true, Scheme::Slate) => Palette {
            bg: [18, 22, 28],
            panel: [26, 32, 40],
            extreme: [12, 16, 20],
            faint: [36, 44, 54],
            select: [46, 110, 168],
            accent: [126, 178, 220],
            widget: [36, 44, 54],
            hover: [40, 58, 76],
            text: [220, 228, 236],
            muted: [196, 208, 220],
        },
        (false, Scheme::Slate) => Palette {
            bg: [236, 240, 244],
            panel: [248, 250, 252],
            extreme: [218, 224, 230],
            faint: [226, 232, 238],
            select: [46, 110, 168],
            accent: [18, 70, 118],
            widget: [226, 232, 238],
            hover: [196, 216, 232],
            text: [22, 28, 36],
            muted: [52, 64, 78],
        },
        (true, Scheme::Pine) => Palette {
            bg: [18, 26, 22],
            panel: [26, 36, 31],
            extreme: [12, 18, 16],
            faint: [34, 46, 40],
            select: [36, 120, 78],
            accent: [122, 196, 150],
            widget: [34, 46, 40],
            hover: [40, 62, 50],
            text: [220, 232, 224],
            muted: [190, 210, 198],
        },
        (false, Scheme::Pine) => Palette {
            bg: [236, 244, 238],
            panel: [246, 252, 248],
            extreme: [214, 228, 220],
            faint: [224, 236, 228],
            select: [36, 120, 78],
            accent: [16, 84, 48],
            widget: [224, 236, 228],
            hover: [190, 224, 200],
            text: [22, 36, 28],
            muted: [48, 70, 56],
        },
        (true, Scheme::Plum) => Palette {
            bg: [26, 20, 28],
            panel: [36, 28, 38],
            extreme: [18, 14, 20],
            faint: [46, 36, 50],
            select: [140, 64, 120],
            accent: [214, 150, 196],
            widget: [46, 36, 50],
            hover: [64, 44, 68],
            text: [236, 224, 232],
            muted: [214, 196, 210],
        },
        (false, Scheme::Plum) => Palette {
            bg: [246, 238, 244],
            panel: [252, 246, 250],
            extreme: [230, 216, 226],
            faint: [240, 226, 234],
            select: [150, 64, 122],
            accent: [102, 28, 82],
            widget: [240, 226, 234],
            hover: [236, 200, 220],
            text: [42, 24, 36],
            muted: [78, 52, 68],
        },
    }
}

fn rgb(c: [u8; 3]) -> egui::Color32 {
    egui::Color32::from_rgb(c[0], c[1], c[2])
}

fn apply_theme(ctx: &egui::Context, mode: ThemeMode, scheme: Scheme) {
    let mut visuals = if mode == ThemeMode::Light {
        egui::Visuals::light()
    } else {
        egui::Visuals::dark()
    };
    let p = palette(mode, scheme);
    let accent = rgb(p.accent);
    visuals.panel_fill = rgb(p.panel);
    visuals.window_fill = rgb(p.bg);
    visuals.extreme_bg_color = rgb(p.extreme);
    visuals.faint_bg_color = rgb(p.faint);
    visuals.selection.bg_fill = rgb(p.select);
    visuals.selection.stroke.color = accent;
    visuals.hyperlink_color = accent;
    visuals.window_stroke.color = rgb(p.faint);
    visuals.widgets.inactive.bg_fill = rgb(p.widget);
    visuals.widgets.hovered.bg_fill = rgb(p.hover);
    visuals.widgets.active.bg_fill = rgb(p.select);
    visuals.widgets.inactive.fg_stroke.color = rgb(p.muted);
    visuals.widgets.hovered.fg_stroke.color = rgb(p.text);
    visuals.widgets.active.fg_stroke.color = rgb(p.text);
    visuals.widgets.noninteractive.fg_stroke.color = rgb(p.text);
    visuals.override_text_color = Some(rgb(p.text));
    ctx.set_visuals(visuals);
}

pub fn run() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([1100.0, 700.0])
            .with_title(format!("Amber {}", crate::VERSION)),
        renderer: eframe::Renderer::Glow,
        // egui leaves this at 0, which draws every triangle on top of the
        // last one, so you can see through the shell. 24 bits is what the
        // glow 3D sample and the slicer viewports use.
        depth_buffer: 24,
        // The cut cap uses the stencil buffer, the same way a model viewer
        // fills a clip plane. egui does not read it.
        stencil_buffer: 8,
        // Left at 0 so a machine without multisample still opens the window.
        multisampling: 0,
        ..Default::default()
    };
    eframe::run_native(
        "Amber",
        options,
        Box::new(|cc| Ok(Box::new(AmberApp::new(cc)))),
    )
}
