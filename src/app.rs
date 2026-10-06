//! Prepare and preview window.

use crate::catalog;
use crate::community;
use crate::mesh::{calibration_cube, overhang_bridge};
use crate::pm3m::write_pm3m;
use crate::printer::{Machine, PrintSettings};
use crate::resins::{self, Resin, ResinProfile};
use crate::scene::{Document, Selection};
use crate::sl1::write_sl1;
use crate::slice::{self, Slice};
use crate::supports::PRESETS;
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
    plate_view: PlateView,
    /// Models left out of the plate view. They still slice.
    hidden_models: HashSet<u64>,
    view_rev: u64,
    view_drawn: u64,
}

impl AmberApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        apply_theme(&cc.egui_ctx);
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
                }
            }
        }
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
            plate_view,
            hidden_models: HashSet::new(),
            view_rev: 0,
            view_drawn: 0,
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
                    self.export_print(false);
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

    fn export_print(&mut self, force_sl1: bool) {
        let Some(slice) = &self.slice else {
            self.start_slice(true);
            return;
        };
        if self.slice_gen != self.doc.changed {
            self.start_slice(true);
            return;
        }
        let native = self.machine.native_photon && !force_sl1;
        let ext = if native {
            self.machine.extension
        } else {
            "sl1"
        };
        let name = sanitize_filename(&self.export_name, ext);
        if native && name.len() > 24 {
            self.status = format!(
                "Keep the file name short. {} skips very long names on the USB stick.",
                self.machine.name
            );
        }
        let filter_name = if native {
            format!("Photon Workshop v516 (.{})", self.machine.extension)
        } else {
            "Prusa SL1".into()
        };
        let Some(path) = rfd::FileDialog::new()
            .set_file_name(&name)
            .add_filter(&filter_name, &[ext])
            .save_file()
        else {
            return;
        };
        let written = if native {
            write_pm3m(&path, slice, self.machine, &self.settings)
        } else {
            write_sl1(&path, slice, self.machine, &self.settings)
        };
        match written {
            Ok(()) => {
                let note = if native {
                    "copy it to a USB stick and print from the machine"
                } else if self.machine.native_photon {
                    "open .sl1 in a converter if you want a different file"
                } else {
                    "this printer does not read .sl1 directly; convert it, or use it to check the layers"
                };
                self.status = format!(
                    "Wrote {} · {:.1} MB · {note}.",
                    path.display(),
                    slice_bytes(slice) as f64 / 1_048_576.0
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
        self.doc.add_mesh(name, mesh);
        self.export_name = default_export_name(&self.doc, self.machine.extension);
        self.invalidate_slice();
        self.view = View::Prepare;
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let delete =
            ctx.input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace));
        let undo = ctx.input(|i| i.key_pressed(egui::Key::Z) && i.modifiers.command);
        let duplicate = ctx.input(|i| i.key_pressed(egui::Key::D) && i.modifiers.command);
        let open = ctx.input(|i| i.key_pressed(egui::Key::O) && i.modifiers.command);
        let slice_now = ctx.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.command);
        let save = ctx.input(|i| i.key_pressed(egui::Key::S) && i.modifiers.command);
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
        if save && self.job.is_none() {
            self.export_print(false);
        }
        if undo {
            self.doc.undo();
            self.invalidate_slice();
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
                if let Selection::Object(id) = self.doc.selection {
                    self.doc.push_xform_undo(id);
                    if let Some(obj) = self.doc.object_mut(id) {
                        obj.position.x += dx;
                        obj.position.y += dy;
                    }
                    self.doc.touch_xform();
                    self.invalidate_slice();
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
                if ui.button("Add 20 mm cube").clicked() {
                    self.add_builtin("cube");
                    ui.close();
                }
                if ui.button("Add overhang bridge").clicked() {
                    self.add_builtin("bridge");
                    ui.close();
                }
                if ui.button("Quit").clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button("Edit", |ui| {
                if ui.button("Undo").clicked() {
                    self.doc.undo();
                    self.invalidate_slice();
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
                let export_label = if self.machine.native_photon {
                    format!("Export .{}…", self.machine.extension)
                } else {
                    "Export .sl1…".into()
                };
                if ui.button(export_label).clicked() {
                    self.export_print(false);
                    ui.close();
                }
                if self.machine.native_photon && ui.button("Export .sl1…").clicked() {
                    self.export_print(true);
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
                    self.export_print(false);
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

    fn status_bar(&self, ui: &mut egui::Ui) {
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
        if ui.button("Show only the contact points").clicked() {
            self.plate_view.tips_only();
            self.touch_view();
        }
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
        ui.text_edit_singleline(&mut self.export_name);
        ui.label("Keep it short. The Photon skips a very long name.");
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
            self.touch_view();
        }
        let mut z = self.plate_view.section_z;
        if drag_f32(
            ui,
            "Cut height",
            &mut z,
            0.1,
            0.0,
            self.machine.size_z.max(1.0),
            "mm",
        ) {
            self.plate_view.section_z = z;
            self.plate_view.section = true;
            self.touch_view();
        }
        ui.label("Hides the model above this height so you can click the surface that is left. Supports stay drawn. A hidden model still prints.");
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
        let rows: Vec<(u64, String, usize)> = self
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
                (obj.id, obj.name.clone(), n)
            })
            .collect();
        let mut select = None;
        let mut toggle = None;
        for (id, name, n) in rows {
            let selected = self.doc.selection == Selection::Object(id);
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
                if ui.selectable_label(selected, label).clicked() {
                    select = Some(Selection::Object(id));
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
        if let Some(sel) = select {
            self.doc.selection = sel;
            self.doc.touch_xform();
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
            "This model will be hollowed when you slice. Punch a hole so resin can drain.".into()
        } else {
            "This model will print solid.".into()
        };
    }

    fn punch_hole(&mut self, id: u64) {
        self.doc.punch_bottom_drain(id);
        self.tool = Tool::Drain;
        self.invalidate_slice();
        self.status =
            "Punched a drain at the bottom of this model. Click the shell to place another.".into();
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

    fn select_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Select");
        ui.label("Left-drag moves the selected model in X and Y. Right-drag orbits, and can swing under the bed. Right-click the model for hollow, holes, supports, and the rest.");
        ui.checkbox(&mut self.snap_mm, "Snap moves to 1 mm");
        if let Selection::Support(id) = self.doc.selection {
            if let Some(support) = self.doc.supports.iter().find(|s| s.id == id).copied() {
                ui.label(format!(
                    "Tip at {:.1}, {:.1} mm, {:.1} mm up. Foot at {:.1} mm.",
                    support.x, support.y, support.z_top, support.z_base
                ));
                if ui.button("Delete this support").clicked() {
                    self.doc.delete_selection();
                    self.invalidate_slice();
                }
            }
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
            if let Some(copy) = self.doc.duplicate(id) {
                self.doc.mirror(copy, axis);
            }
        } else {
            self.doc.mirror(id, axis);
        }
        self.invalidate_slice();
    }

    fn drain_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Hole");
        ui.label("Click the outside of a hollow. The hole points inward so resin can drain and air can enter.");
        ui.label("Put at least one hole near the lowest point of a cup, or the layer that seals it will suction onto the film.");
        let mut hole = false;
        hole |= drag_f32(
            ui,
            "Diameter",
            &mut self.doc.drain_diameter_mm,
            0.05,
            0.4,
            12.0,
            "mm",
        );
        hole |= drag_f32(
            ui,
            "Depth",
            &mut self.doc.drain_depth_mm,
            0.1,
            1.0,
            40.0,
            "mm",
        );
        if hole {
            self.doc.touch();
        }
        if let Selection::Drain(id) = self.doc.selection {
            let current = self.doc.drains.iter().find(|d| d.id == id).copied();
            if let Some(mut drain) = current {
                let mut diameter = drain.radius_mm * 2.0;
                let mut edited = false;
                edited |= drag_f32(ui, "This hole", &mut diameter, 0.05, 0.4, 12.0, "mm");
                edited |= drag_f32(ui, "This depth", &mut drain.depth_mm, 0.1, 1.0, 40.0, "mm");
                if edited {
                    drain.radius_mm = diameter * 0.5;
                    if let Some(slot) = self.doc.drains.iter_mut().find(|d| d.id == id) {
                        *slot = drain;
                    }
                    self.doc.touch();
                    self.invalidate_slice();
                }
            }
        }
        if ui.button("Punch hole at the bottom").clicked() {
            if let Some(id) = self.doc.edit_target() {
                self.punch_hole(id);
            } else {
                self.status = "Select the model you want a drain in.".into();
            }
        }
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
        ui.label("The inside stays empty. Resin infill is not offered: it traps resin and blows out the print.");
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
        ui.label("Click an underside to plant a tip. Click a tip to select it, then drag it to a new spot. Every control here applies only to the selected model.");
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
        changed |= drag_f32(
            ui,
            "Trunk diameter",
            &mut style.trunk_mm,
            0.02,
            0.3,
            5.0,
            "mm",
        );
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
        if self.machine.native_photon {
            ui.label(format!(
                "Writes Photon Workshop v{} .{}",
                self.machine.file_version, self.machine.extension
            ));
        } else {
            ui.label(format!(
                "Writes .sl1. This printer reads .{} ({}), which Amber does not encode.",
                self.machine.printer_extension, self.machine.format_name
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
            .checkbox(&mut s.fill_voids, "Fill enclosed voids")
            .changed();
        ui.label("Heals speckled gaps and cures closed pockets in each layer. A model you hollowed stays empty. Drain holes are cut after, so a drain still opens.");
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
            let export_label = if self.machine.native_photon {
                format!("Export .{}", self.machine.extension)
            } else {
                "Export .sl1".to_string()
            };
            if ui
                .add_enabled(!slicing, egui::Button::new(export_label))
                .clicked()
            {
                self.export_print(false);
            }
            if self.machine.native_photon
                && ui
                    .add_enabled(!slicing, egui::Button::new("Export .sl1"))
                    .clicked()
            {
                self.export_print(true);
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
        let clip = self.plate_view.section.then_some(self.plate_view.section_z);
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
                gpu.paint(gl, &camera, aspect, overhang, clip);
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
            frame.add_line(
                Vec3::new(x - arm, y, z),
                Vec3::new(x + arm, y, z),
                color,
            );
            frame.add_line(
                Vec3::new(x, y - arm, z),
                Vec3::new(x, y + arm, z),
                color,
            );
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

    fn visible_hit(&self, origin: Vec3, dir: Vec3) -> Option<(u64, Vec3, Vec3)> {
        let clip = self.plate_view.section.then_some(self.plate_view.section_z);
        self.doc
            .raycast_visible(origin, dir, &self.hidden_models, clip)
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
                ui.separator();
                ui.label("Simple is the short path. Workshop is every control: rafts, rest times, compensation, and the rest.");
                ui.label("Right-click a model for the same edits. Right-drag orbits, and you can swing under the bed. The bed turns clear so you can click an underside.");
                ui.label("View → Cut the view hides the model above a height, so you can click the surface that is left and place a support there. The checkbox beside a model hides it in the view. It still prints. Tips only draws the contact points. After a slice, red marks on the plate are islands. Click one with Support to plant a tip there.");
                ui.separator();
                ui.label("Ctrl+O open    Ctrl+Z undo    Ctrl+D duplicate    Delete remove");
                ui.label("Ctrl+Enter slice    Ctrl+S save    F1 this page");
                ui.label("On the plate, the arrow keys nudge the model by 1 mm (Shift is 0.1 mm). F fits the camera. In the layer view, the arrows step through layers.");
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
                self.plate_view.section_z = ((min.z + max.z) * 0.5).clamp(0.0, self.machine.size_z);
                self.touch_view();
                self.status = "The model above the orange line is hidden. Click the surface you can see to place a support.".into();
            }
            close = true;
        }
        if click(ui, "Show only the contact points") {
            self.plate_view.tips_only();
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
                if let Some((_, point, normal)) = self.visible_hit(origin, dir) {
                    self.doc.add_drain(point, normal);
                    self.invalidate_slice();
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
                if let Some((id, _, _)) = self.visible_hit(origin, dir) {
                    self.doc.selection = Selection::Object(id);
                } else {
                    self.doc.selection = Selection::None;
                }
                self.doc.touch_xform();
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
            ui.label("Red marks on the plate (Prepare) are these islands. Support, then click a mark.");
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

fn apply_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    let bg = egui::Color32::from_rgb(22, 24, 27);
    let panel = egui::Color32::from_rgb(30, 33, 37);
    let amber = egui::Color32::from_rgb(214, 154, 62);
    visuals.panel_fill = panel;
    visuals.window_fill = bg;
    visuals.extreme_bg_color = egui::Color32::from_rgb(16, 17, 19);
    visuals.faint_bg_color = egui::Color32::from_rgb(38, 41, 46);
    visuals.selection.bg_fill = egui::Color32::from_rgb(176, 92, 32);
    visuals.selection.stroke.color = amber;
    visuals.hyperlink_color = amber;
    visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(42, 46, 51);
    visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(58, 52, 40);
    visuals.widgets.active.bg_fill = egui::Color32::from_rgb(176, 92, 32);
    visuals.widgets.inactive.fg_stroke.color = egui::Color32::from_rgb(228, 220, 206);
    visuals.override_text_color = Some(egui::Color32::from_rgb(232, 224, 210));
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
