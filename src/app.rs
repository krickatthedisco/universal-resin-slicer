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
    Select,
    Move,
    Rotate,
    Scale,
    Mirror,
    Hollow,
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
    #[serde(default)]
    style: Option<crate::supports::SupportStyle>,
    #[serde(default)]
    platform_only: bool,
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
    keep_original: bool,
}

impl AmberApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        apply_theme(&cc.egui_ctx);
        let mut machine = Machine::photon_m3_max();
        let mut settings = PrintSettings::default();
        let mut preset = 1usize;
        let mut raft = true;
        let mut saved_style = None;
        let mut platform_only = false;
        if let Some(storage) = cc.storage {
            if let Some(raw) = storage.get_string("amber.print") {
                if let Ok(saved) = serde_json::from_str::<Persist>(&raw) {
                    settings = saved.settings;
                    machine.rotate_180 = saved.rotate_180;
                    machine.mirror_x = saved.mirror_x;
                    machine.mirror_y = saved.mirror_y;
                    preset = saved.preset;
                    raft = saved.raft;
                    saved_style = saved.style;
                    platform_only = saved.platform_only;
                }
            }
        }
        let mut doc = Document::new();
        doc.set_preset(preset);
        if let Some(style) = saved_style {
            doc.style = style.sanitized();
        }
        doc.raft = raft;
        doc.platform_only = platform_only;
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
            tool: Tool::Select,
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
            keep_original: false,
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
            style: Some(self.doc.style),
            platform_only: self.doc.platform_only,
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
        egui::Panel::left("tools")
            .resizable(false)
            .exact_size(76.0)
            .show(ui, |ui| self.tool_rail(ui));
        egui::Panel::right("props")
            .resizable(true)
            .default_size(332.0)
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
                if ui.button("Right").clicked() {
                    self.camera.pitch = 8.0;
                    self.camera.yaw = 90.0;
                    ui.close();
                }
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
                    self.export_pm3m();
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
                "Click a model. Right-drag orbits. Shift-drag or middle-drag pans.",
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
                "Shell the selected model when it slices.",
            ),
            (
                Tool::Drain,
                "Hole",
                "Click the outside of a hollow to punch a drain.",
            ),
            (
                Tool::Support,
                "Support",
                "Click an underside to plant a tree support.",
            ),
        ] {
            let on = self.tool == tool;
            let button = egui::Button::new(label).min_size(egui::vec2(64.0, 32.0));
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
            ui.separator();
            ui.label(&self.status);
        });
    }

    fn model_list(&mut self, ui: &mut egui::Ui) {
        ui.heading("Models");
        ui.label("Photon M3 Max  ·  298 × 166 × 300 mm");
        if self.doc.objects.is_empty() {
            ui.label("Open an STL or OBJ, or drop it on the plate.");
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
        if !self.doc.supports.is_empty() {
            ui.add_space(4.0);
            ui.label(format!("Supports {}", self.doc.supports.len()));
        }
    }

    fn props_panel(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
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
            }
            ui.separator();
            ui.collapsing("Print settings", |ui| self.slice_ui(ui));
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
            self.doc.touch();
            self.invalidate_slice();
        }
    }

    fn select_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Select");
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
        ui.label("Drag to turn. Values are degrees.");
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
            if ui.button("Orient all").clicked() {
                self.doc.auto_orient_all();
                self.invalidate_slice();
            }
        });
    }

    fn scale_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Scale");
        ui.label("1.00 is the size in the file. Drag up to enlarge.");
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
            self.doc.touch();
            self.invalidate_slice();
        }
        self.size_label(ui, id);
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
        ui.heading("Dig hole");
        ui.label("Click the outside of a hollow. The hole points inward so resin can drain and air can enter.");
        ui.label("Put at least one hole near the lowest point of a cup, or the layer that seals it will suction onto the film.");
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
        ui.heading("Support");
        ui.label("Tips branch into a shared trunk. Click an underside to add one by hand.");
        let mut preset = self.doc.preset.min(PRESETS.len() - 1);
        let before = preset;
        egui::ComboBox::from_label("Preset")
            .selected_text(PRESETS[preset].name)
            .show_ui(ui, |ui| {
                for (i, preset_def) in PRESETS.iter().enumerate() {
                    ui.selectable_value(&mut preset, i, preset_def.name);
                }
            });
        if preset != before {
            self.doc.set_preset(preset);
            self.invalidate_slice();
        }
        let mut style = self.doc.style;
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
        changed |= drag_f32(
            ui,
            "Brace diameter",
            &mut style.brace_mm,
            0.01,
            0.1,
            2.0,
            "mm",
        );
        let mut platform_only = self.doc.platform_only;
        if ui
            .checkbox(&mut platform_only, "To the platform only")
            .changed()
        {
            self.doc.platform_only = platform_only;
            self.doc.touch();
        }
        ui.label("Branch angle is the lean off vertical. Trunk spacing is how far apart tips must be before they get their own trunk.");
        if changed {
            self.doc.style = style.sanitized();
            self.doc.touch();
            self.invalidate_slice();
        }
        ui.horizontal_wrapped(|ui| {
            if ui.button("+ All").clicked() {
                self.doc.platform_only = false;
                self.doc.add_auto_supports(true);
                self.invalidate_slice();
            }
            if ui.button("+ Platform").clicked() {
                self.doc.platform_only = true;
                self.doc.add_auto_supports(true);
                self.invalidate_slice();
            }
            if ui.button("+ Everything").clicked() {
                self.doc.add_auto_supports(false);
                self.invalidate_slice();
            }
            if ui.button("Clear").clicked() {
                self.doc.clear_supports();
                self.invalidate_slice();
            }
        });
        let mut raft = self.doc.raft;
        let mut braces = self.doc.braces_on;
        let mut raft_mm = self.doc.raft_mm;
        let mut brace_dist = self.doc.brace_dist;
        let mut raft_changed = false;
        raft_changed |= ui.checkbox(&mut raft, "Raft").changed();
        raft_changed |= drag_f32(ui, "Raft thickness", &mut raft_mm, 0.05, 0.4, 3.0, "mm");
        raft_changed |= ui.checkbox(&mut braces, "Cross bracing").changed();
        raft_changed |= drag_f32(ui, "Brace spacing", &mut brace_dist, 0.1, 2.0, 20.0, "mm");
        if raft_changed {
            self.doc.raft = raft;
            self.doc.raft_mm = raft_mm;
            self.doc.braces_on = braces;
            self.doc.brace_dist = brace_dist;
            self.doc.touch();
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
            || (response.dragged_by(egui::PointerButton::Primary) && self.tool == Tool::Select)
        {
            let d = response.drag_delta();
            self.camera.yaw += d.x * 0.4;
            self.camera.pitch = (self.camera.pitch + d.y * 0.3).clamp(4.0, 89.0);
        }
        if response.dragged_by(egui::PointerButton::Middle)
            || (response.dragged_by(egui::PointerButton::Primary)
                && ui.input(|i| i.modifiers.shift)
                && self.tool == Tool::Select)
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
                Tool::Support
                    | Tool::Drain
                    | Tool::Select
                    | Tool::Move
                    | Tool::Rotate
                    | Tool::Scale
                    | Tool::Mirror
                    | Tool::Hollow
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
        } else if self
            .doc
            .objects
            .iter()
            .any(|obj| obj.mesh.triangle_count() > 280_000)
        {
            ui.painter().text(
                rect.left_bottom() + egui::vec2(12.0, -16.0),
                egui::Align2::LEFT_BOTTOM,
                "Dense mesh: the plate view is simplified so the surface stays solid. Slicing still uses every triangle.",
                egui::FontId::proportional(13.0),
                egui::Color32::from_rgb(190, 176, 150),
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
        let slider_h = (ui.available_height() - 128.0).max(80.0);
        let mut layer = self.preview_index;
        ui.add_sized(
            [28.0, slider_h],
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
