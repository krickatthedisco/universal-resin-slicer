//! Prepare and preview window.

use crate::mesh::{calibration_cube, overhang_bridge};
use crate::pm3m::write_pm3m;
use crate::printer::{Machine, PrintSettings, RESIN_PRESETS};
use crate::scene::{Document, Selection};
use crate::slice::{self, Slice};
use crate::supports::PRESETS;
use crate::viewport::{self, Camera, DrawLists, Renderer};
use eframe::egui;
use glam::Vec3;
use glow::HasContext;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tool {
    Orbit,
    Move,
    Rotate,
    Scale,
    Support,
    Drain,
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
}

#[derive(Serialize, Deserialize)]
struct Persist {
    settings: PrintSettings,
    rotate_180: bool,
    mirror_x: bool,
    mirror_y: bool,
    preset: usize,
    raft: bool,
}

pub struct AmberApp {
    machine: Machine,
    settings: PrintSettings,
    doc: Document,
    camera: Camera,
    tool: Tool,
    view: View,
    renderer: Option<viewport::SharedRenderer>,
    draw: Option<Arc<DrawLists>>,
    draw_gen: u64,
    status: String,
    job: Option<Job>,
    slice: Option<Slice>,
    slice_gen: u64,
    preview_index: usize,
    preview_for: Option<usize>,
    preview_tex: Option<egui::TextureHandle>,
    gesture: bool,
    export_name: String,
}

impl AmberApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        apply_theme(&cc.egui_ctx);
        let mut machine = Machine::photon_m3_max();
        let mut settings = PrintSettings::default();
        let mut preset = 1usize;
        let mut raft = true;
        if let Some(storage) = cc.storage {
            if let Some(raw) = storage.get_string("amber.print") {
                if let Ok(saved) = serde_json::from_str::<Persist>(&raw) {
                    settings = saved.settings;
                    machine.rotate_180 = saved.rotate_180;
                    machine.mirror_x = saved.mirror_x;
                    machine.mirror_y = saved.mirror_y;
                    preset = saved.preset;
                    raft = saved.raft;
                }
            }
        }
        let mut doc = Document::new();
        doc.preset = preset.min(PRESETS.len() - 1);
        doc.raft = raft;
        let plate = Vec3::new(machine.size_x, machine.size_y, machine.size_z);
        let renderer = cc.gl.as_ref().and_then(|gl| {
            Renderer::new(gl.as_ref())
                .map(|r| Arc::new(Mutex::new(r)))
                .ok()
        });
        Self {
            machine,
            settings,
            doc,
            camera: Camera::looking_at_plate(plate),
            tool: Tool::Orbit,
            view: View::Prepare,
            renderer,
            draw: None,
            draw_gen: 0,
            status: "Photon M3 Max · drop an STL or OBJ, or add the overhang bridge.".into(),
            job: None,
            slice: None,
            slice_gen: 0,
            preview_index: 0,
            preview_for: None,
            preview_tex: None,
            gesture: false,
            export_name: "print.pm3m".into(),
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
            self.status = format!("Slicing layer {done} of {total}…");
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
                self.status = format!(
                    "Sliced {layers} layers · {:.2} ml · {minutes} min · {islands} islands",
                    slice.cured_ml
                );
                if !slice.warnings.is_empty() {
                    self.status = format!("{} · {}", self.status, slice.warnings.join(" "));
                }
                self.slice = Some(slice);
                self.slice_gen = generation;
                self.preview_index = 0;
                self.preview_for = None;
                self.view = View::Preview;
                if export_after {
                    self.export_pm3m();
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
        });
        self.status = "Slicing…".into();
    }

    fn export_pm3m(&mut self) {
        let Some(slice) = &self.slice else {
            self.start_slice(true);
            return;
        };
        if self.slice_gen != self.doc.changed {
            self.start_slice(true);
            return;
        }
        let name = sanitize_filename(&self.export_name);
        if name.len() > 24 {
            self.status =
                "Keep the file name short. The M3 Max skips very long names on the USB stick."
                    .into();
        }
        let Some(path) = rfd::FileDialog::new()
            .set_file_name(&name)
            .add_filter("Photon M3 Max", &["pm3m"])
            .save_file()
        else {
            return;
        };
        match write_pm3m(&path, slice, self.machine, &self.settings) {
            Ok(()) => {
                self.status = format!(
                    "Wrote {} · {:.1} MB · copy it to a USB stick and print from the machine.",
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
                self.export_name = default_export_name(&self.doc);
                self.invalidate_slice();
                self.status = format!("Imported {}", path.display());
                self.view = View::Prepare;
            }
            Err(err) => self.status = err.to_string(),
        }
    }

    fn open_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Meshes", &["stl", "obj", "STL", "OBJ"])
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
        self.export_name = default_export_name(&self.doc);
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
        }
        for file in ctx.input(|i| i.raw.dropped_files.clone()) {
            self.import_path(file.path().to_path_buf());
        }
        if self.draw_gen != self.doc.changed || self.draw.is_none() {
            self.draw = Some(Arc::new(viewport::build_draw(
                &self.doc,
                self.plate(),
                self.doc.selection,
            )));
            self.draw_gen = self.doc.changed;
        }

        egui::Panel::top("menu").show(ui, |ui| self.menu(ui));
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        egui::Panel::left("scene")
            .resizable(true)
            .default_size(240.0)
            .show(ui, |ui| self.scene_panel(ui));
        egui::Panel::right("props")
            .resizable(true)
            .default_size(320.0)
            .show(ui, |ui| self.props_panel(ui));
        egui::CentralPanel::default().show(ui, |ui| self.center(ui));
    }
}

impl AmberApp {
    fn menu(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.menu_button("File", |ui| {
                if ui.button("Open STL or OBJ…").clicked() {
                    self.open_dialog();
                    ui.close();
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
            ui.menu_button("Slice", |ui| {
                if ui.button("Slice").clicked() {
                    self.start_slice(false);
                    ui.close();
                }
                if ui.button("Export .pm3m…").clicked() {
                    self.export_pm3m();
                    ui.close();
                }
                if ui.button("Export preview PNG…").clicked() {
                    self.export_layer_png();
                    ui.close();
                }
            });
            ui.separator();
            ui.heading("Amber");
            ui.label("Photon M3 Max");
        });
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
            ui.separator();
            ui.label(&self.status);
        });
    }

    fn scene_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Plate");
        ui.label("298.08 × 165.6 × 300 mm · 6480 × 3600 · 46 µm");
        ui.add_space(6.0);
        if self.doc.objects.is_empty() {
            ui.label("No models yet.");
        }
        let mut select = None;
        for obj in &self.doc.objects {
            let selected = self.doc.selection == Selection::Object(obj.id);
            if ui.selectable_label(selected, &obj.name).clicked() {
                select = Some(Selection::Object(obj.id));
            }
        }
        if let Some(sel) = select {
            self.doc.selection = sel;
            self.doc.touch();
        }
        ui.add_space(8.0);
        ui.label(format!("Supports {}", self.doc.supports.len()));
        let mut select_support = None;
        for support in &self.doc.supports {
            let label = format!("#{:.0} mm", support.z_top);
            let selected = self.doc.selection == Selection::Support(support.id);
            if ui.selectable_label(selected, label).clicked() {
                select_support = Some(Selection::Support(support.id));
            }
        }
        if let Some(sel) = select_support {
            self.doc.selection = sel;
            self.doc.touch();
        }
        ui.add_space(8.0);
        if ui.button("Auto layout").clicked() {
            self.doc
                .auto_layout(self.machine.size_x, self.machine.size_y);
            self.invalidate_slice();
        }
        if ui.button("Clear supports").clicked() {
            self.doc.clear_supports();
            self.invalidate_slice();
        }
    }

    fn props_panel(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            self.transform_ui(ui);
            ui.separator();
            self.hollow_ui(ui);
            ui.separator();
            self.support_ui(ui);
            ui.separator();
            self.slice_ui(ui);
        });
    }

    fn transform_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Model");
        let Selection::Object(id) = self.doc.selection else {
            ui.label("Select a model on the plate or in the list.");
            return;
        };
        let Some(obj) = self.doc.object(id) else {
            return;
        };
        let before_pos = obj.position;
        let before_rot = obj.rotation_deg;
        let before_scale = obj.scale;
        let mut position = obj.position;
        let mut rotation = obj.rotation_deg;
        let mut scale = obj.scale;
        let mut started = false;
        let mut changed = false;
        ui.label("Position");
        grid_drag(ui, &mut position, 0.1, "mm", &mut started, &mut changed);
        ui.label("Rotation");
        grid_drag(ui, &mut rotation, 0.5, "°", &mut started, &mut changed);
        ui.label("Scale");
        grid_drag(ui, &mut scale, 0.005, "", &mut started, &mut changed);
        if started && !self.gesture {
            self.doc
                .remember_xform(id, before_pos, before_rot, before_scale);
            self.gesture = true;
        }
        if changed {
            if let Some(obj) = self.doc.object_mut(id) {
                obj.position = position;
                obj.rotation_deg = rotation;
                obj.scale = Vec3::new(nonzero(scale.x), nonzero(scale.y), nonzero(scale.z));
            }
            self.doc.touch();
            self.invalidate_slice();
        }
        ui.add_space(4.0);
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
            if ui.button("Largest face down").clicked() {
                self.doc.place_on_largest_face(id);
                self.invalidate_slice();
            }
            if ui.button("Auto orient").clicked() {
                self.doc.auto_orient(id);
                self.invalidate_slice();
            }
            if ui.button("Flip normals").clicked() {
                self.doc.flip_normals(id);
                self.invalidate_slice();
            }
            if ui.button("Mirror X").clicked() {
                self.doc.mirror(id, 0);
                self.invalidate_slice();
            }
            if ui.button("Mirror Y").clicked() {
                self.doc.mirror(id, 1);
                self.invalidate_slice();
            }
        });
        if let Some(obj) = self.doc.object(id) {
            let ml = crate::scene::Document::world_mesh(obj).volume_mm3() / 1000.0;
            ui.label(format!("Mesh volume {ml:.2} ml, before hollowing"));
        }
    }

    fn hollow_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Hollow");
        let Selection::Object(id) = self.doc.selection else {
            ui.label("Hollowing is per model, applied when you slice.");
            return;
        };
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
            changed |= ui.checkbox(&mut obj.infill, "Lattice infill").changed();
            changed |= drag_f32(
                ui,
                "Infill spacing",
                &mut obj.infill_spacing_mm,
                0.1,
                1.0,
                20.0,
                "mm",
            );
            changed |= drag_f32(
                ui,
                "Infill thickness",
                &mut obj.infill_thickness_mm,
                0.05,
                0.2,
                2.0,
                "mm",
            );
        });
        ui.label("Drain tool punches a hole through the shell. Place one near the lowest point of a cup.");
        if changed {
            self.doc.touch();
            self.invalidate_slice();
        }
    }

    fn support_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Supports");
        let mut preset = self.doc.preset.min(PRESETS.len() - 1);
        let before = preset;
        egui::ComboBox::from_label("Preset")
            .selected_text(PRESETS[preset].name)
            .show_ui(ui, |ui| {
                for (i, preset_def) in PRESETS.iter().enumerate() {
                    ui.selectable_value(&mut preset, i, preset_def.name);
                }
            });
        self.doc.preset = preset;
        if preset != before {
            self.doc.touch();
        }
        let current = PRESETS[self.doc.preset];
        ui.label(format!(
            "{} · overhang {:.0}° from vertical · spacing {:.1} mm · tip {:.2} / shaft {:.2} mm",
            current.name,
            current.overhang_deg,
            current.spacing_mm,
            current.tip_mm,
            current.shaft_mm
        ));
        ui.label("A higher overhang angle leaves steep walls alone and uses fewer supports.");
        if ui.button("Auto support selected").clicked() {
            self.doc.add_auto_supports(true);
            self.invalidate_slice();
        }
        if ui.button("Auto support everything").clicked() {
            self.doc.add_auto_supports(false);
            self.invalidate_slice();
        }
        let mut raft = self.doc.raft;
        let mut braces = self.doc.braces_on;
        let mut raft_mm = self.doc.raft_mm;
        let mut brace_dist = self.doc.brace_dist;
        let mut changed = false;
        changed |= ui.checkbox(&mut raft, "Raft under bed supports").changed();
        changed |= drag_f32(ui, "Raft thickness", &mut raft_mm, 0.05, 0.4, 3.0, "mm");
        changed |= ui
            .checkbox(&mut braces, "Braces between neighbours")
            .changed();
        changed |= drag_f32(ui, "Brace distance", &mut brace_dist, 0.1, 2.0, 20.0, "mm");
        if changed {
            self.doc.raft = raft;
            self.doc.raft_mm = raft_mm;
            self.doc.braces_on = braces;
            self.doc.brace_dist = brace_dist;
            self.doc.touch();
            self.invalidate_slice();
        }
        ui.label("Support tool: click an underside. Drain tool: click the outside of a hollow.");
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
                let n = points.len();
                self.doc.add_island_supports(&points);
                self.invalidate_slice();
                self.status = format!("Added {n} supports under islands. Slice again.");
                self.view = View::Prepare;
            } else {
                self.status = "Slice once so Amber can see the islands.".into();
            }
        }
    }

    fn slice_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Slice");
        ui.label("Resin starting points are Anycubic's Photon M3 Max table. Run a RERF on your bottle before a long print.");
        let mut resin = self.settings.resin.clone();
        egui::ComboBox::from_label("Resin")
            .selected_text(&resin)
            .show_ui(ui, |ui| {
                for preset in RESIN_PRESETS {
                    ui.selectable_value(&mut resin, preset.name.to_string(), preset.name);
                }
            });
        if resin != self.settings.resin {
            if let Some(preset) = RESIN_PRESETS.iter().find(|p| p.name == resin) {
                let aa = self.settings.anti_alias;
                let density = self.settings.density_g_ml;
                let transition = self.settings.transition_layers;
                self.settings = PrintSettings::from_preset(preset);
                self.settings.anti_alias = aa;
                self.settings.density_g_ml = density;
                self.settings.transition_layers = transition;
                self.invalidate_slice();
            }
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
        ui.collapsing("Bottom lift and image", |ui| {
            changed |= drag_f32(ui, "Bottom lift", &mut s.bottom_lift_mm, 0.1, 2.0, 15.0, "mm");
            changed |= drag_f32(ui, "Bottom lift speed", &mut s.bottom_lift_speed, 0.05, 0.5, 8.0, "mm/s");
            changed |= drag_f32(ui, "Bottom retract", &mut s.bottom_retract_speed, 0.05, 0.5, 8.0, "mm/s");
            changed |= ui.checkbox(&mut self.machine.rotate_180, "Rotate exposure 180°").changed();
            changed |= ui.checkbox(&mut self.machine.mirror_x, "Mirror X").changed();
            changed |= ui.checkbox(&mut self.machine.mirror_y, "Mirror Y").changed();
            ui.label("Rotate 180° is the M3 Max default used by Photonic Etcher. Print the 20 mm cube and flip these if the part comes out mirrored.");
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
            if ui
                .add_enabled(!slicing, egui::Button::new("Export .pm3m"))
                .clicked()
            {
                self.export_pm3m();
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
        ui.horizontal(|ui| {
            for (tool, label) in [
                (Tool::Orbit, "Orbit"),
                (Tool::Move, "Move"),
                (Tool::Rotate, "Rotate"),
                (Tool::Scale, "Scale"),
                (Tool::Support, "Support"),
                (Tool::Drain, "Drain"),
            ] {
                if ui.selectable_label(self.tool == tool, label).clicked() {
                    self.tool = tool;
                }
            }
            ui.separator();
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
        });
        ui.add_space(4.0);
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
        if response.dragged_by(egui::PointerButton::Secondary)
            || (response.dragged_by(egui::PointerButton::Primary) && self.tool == Tool::Orbit)
        {
            let d = response.drag_delta();
            self.camera.yaw += d.x * 0.4;
            self.camera.pitch = (self.camera.pitch + d.y * 0.3).clamp(4.0, 89.0);
        }
        if response.dragged_by(egui::PointerButton::Middle)
            || (response.dragged_by(egui::PointerButton::Primary)
                && ui.input(|i| i.modifiers.shift)
                && self.tool == Tool::Orbit)
        {
            let d = response.drag_delta();
            self.camera.pan(d.x, d.y);
        }
        if response.dragged_by(egui::PointerButton::Primary)
            && matches!(self.tool, Tool::Move | Tool::Rotate | Tool::Scale)
        {
            self.drag_transform(&response);
        }
        if response.clicked_by(egui::PointerButton::Primary)
            && matches!(
                self.tool,
                Tool::Orbit | Tool::Move | Tool::Support | Tool::Drain
            )
        {
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
        let draw = self.draw.clone();
        let camera = self.camera.clone();
        let generation = self.draw_gen;
        let aspect = (rect.width() / rect.height().max(1.0)).clamp(0.2, 5.0);
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
                if let Some(draw) = &draw {
                    gpu.sync(gl, draw, generation);
                }
                gpu.paint(gl, &camera, aspect);
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
            Tool::Move => {
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
                        }
                    }
                }
            }
            Tool::Rotate => {
                if let Some(obj) = self.doc.object_mut(id) {
                    obj.rotation_deg.z += delta.x * 0.4;
                    obj.rotation_deg.x += delta.y * 0.4;
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
        self.doc.touch();
        self.invalidate_slice();
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
                if let Some((id, point, _)) = self.doc.raycast(origin, dir) {
                    self.doc.add_support_at(point, id);
                    self.invalidate_slice();
                }
            }
            Tool::Drain => {
                if let Some((_, point, normal)) = self.doc.raycast(origin, dir) {
                    self.doc.add_drain(point, normal);
                    self.invalidate_slice();
                }
            }
            _ => {
                if let Some((id, _, _)) = self.doc.raycast(origin, dir) {
                    self.doc.selection = Selection::Object(id);
                } else {
                    self.doc.selection = Selection::None;
                }
                self.doc.touch();
            }
        }
    }

    fn preview(&mut self, ui: &mut egui::Ui) {
        let Some(slice) = &self.slice else {
            ui.label("Slice the plate to see layers. Islands show up in red.");
            return;
        };
        let count = slice.layers.len();
        if count == 0 {
            return;
        }
        self.preview_index = self.preview_index.min(count - 1);
        ui.horizontal(|ui| {
            ui.label("Layer");
            ui.add(egui::Slider::new(&mut self.preview_index, 0..=count - 1).show_value(false));
            let layer = &slice.layers[self.preview_index];
            ui.label(format!(
                "{} / {}  z {:.2} mm  {:.2} s  {} px  {} islands",
                layer.index + 1,
                count,
                layer.z_top_mm,
                layer.exposure_s,
                layer.nonzero,
                layer.islands.len()
            ));
            if layer.seals_cavity_px > 0 {
                ui.colored_label(
                    egui::Color32::from_rgb(90, 160, 220),
                    format!("seals a cavity ({} px)", layer.seals_cavity_px),
                );
            }
        });
        if self.preview_for != Some(self.preview_index) {
            let layer = &slice.layers[self.preview_index];
            if let Ok((w, h, rgba)) =
                slice::preview_rgba(&layer.rle, slice.width, slice.height, 1400, &layer.islands)
            {
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

fn sanitize_filename(name: &str) -> String {
    let mut stem = name.trim().trim_end_matches(".pm3m").to_string();
    stem.retain(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if stem.is_empty() {
        stem = "print".into();
    }
    format!("{stem}.pm3m")
}

fn default_export_name(doc: &Document) -> String {
    let stem = doc
        .objects
        .first()
        .map(|o| o.name.as_str())
        .unwrap_or("print");
    sanitize_filename(stem)
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
    visuals.selection.bg_fill = egui::Color32::from_rgb(92, 64, 24);
    visuals.selection.stroke.color = amber;
    visuals.hyperlink_color = amber;
    visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(42, 46, 51);
    visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(58, 52, 40);
    visuals.widgets.active.bg_fill = egui::Color32::from_rgb(92, 68, 32);
    visuals.widgets.inactive.fg_stroke.color = egui::Color32::from_rgb(228, 220, 206);
    visuals.override_text_color = Some(egui::Color32::from_rgb(232, 224, 210));
    ctx.set_visuals(visuals);
}

pub fn run() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([1100.0, 700.0])
            .with_title("Amber — Photon M3 Max"),
        renderer: eframe::Renderer::Glow,
        // 0 keeps the window opening on software GL and on machines without MSAA.
        multisampling: 0,
        ..Default::default()
    };
    eframe::run_native(
        "Amber",
        options,
        Box::new(|cc| Ok(Box::new(AmberApp::new(cc)))),
    )
}
