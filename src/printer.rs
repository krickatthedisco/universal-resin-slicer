//! The selected printer and the exposure settings for the current job.

use crate::catalog::{self, PrinterProfile};
use serde::{Deserialize, Serialize};

/// One printer from the catalog, plus the orientation toggles for this job.
///
/// `extension` is what Amber writes. `printer_extension` is the suffix the
/// machine's own slicer would use. Those match for Photon Workshop v516.
#[derive(Clone, Copy, Debug)]
pub struct Machine {
    pub id: &'static str,
    pub name: &'static str,
    pub vendor: &'static str,
    pub extension: &'static str,
    pub printer_extension: &'static str,
    pub format_name: &'static str,
    pub file_version: u32,
    pub native_photon: bool,
    pub size_x: f32,
    pub size_y: f32,
    pub size_z: f32,
    pub res_x: u32,
    pub res_y: u32,
    pub pixel_um: f32,
    pub pixel_um_y: f32,
    pub rotate_180: bool,
    pub mirror_x: bool,
    pub mirror_y: bool,
}

impl Machine {
    pub fn photon_m3_max() -> Self {
        Self::from_profile(
            catalog::find("anycubic-photon-m3-max")
                .expect("Photon M3 Max is in the printer catalog"),
        )
    }

    pub fn from_profile(profile: &PrinterProfile) -> Self {
        // The M3 Max file the printer accepts is rotated 180°, matching
        // Photonic Etcher. The community mirror flag is a different convention.
        let m3 = profile.id == "anycubic-photon-m3-max";
        let native = profile.native_photon;
        Self {
            id: profile.id,
            name: profile.name,
            vendor: profile.vendor,
            extension: if native {
                profile.printer_extension
            } else {
                "sl1"
            },
            printer_extension: profile.printer_extension,
            format_name: profile.format_name,
            file_version: if native { 516 } else { profile.file_version },
            native_photon: native,
            size_x: profile.size_x,
            size_y: profile.size_y,
            size_z: profile.size_z,
            res_x: profile.res_x,
            res_y: profile.res_y,
            pixel_um: profile.pixel_x_um,
            pixel_um_y: profile.pixel_y_um,
            rotate_180: m3,
            mirror_x: if m3 { false } else { profile.mirror_x },
            mirror_y: if m3 { false } else { profile.mirror_y },
        }
    }

    pub fn pixel_mm(self) -> f32 {
        self.pixel_um / 1000.0
    }

    pub fn pixel_mm_y(self) -> f32 {
        self.pixel_um_y / 1000.0
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PrintSettings {
    pub resin: String,
    pub layer_mm: f32,
    pub exposure_s: f32,
    pub bottom_exposure_s: f32,
    pub bottom_layers: u32,
    pub transition_layers: u32,
    pub light_off_s: f32,
    pub lift_mm: f32,
    pub lift_speed: f32,
    pub retract_speed: f32,
    pub bottom_lift_mm: f32,
    pub bottom_lift_speed: f32,
    pub bottom_retract_speed: f32,
    /// 1, 2, 4, or 8. 1 is a hard edge.
    pub anti_alias: u8,
    pub density_g_ml: f32,
    /// Positive grows the solid (and shrinks holes). Millimetres.
    #[serde(default)]
    pub xy_offset_mm: f32,
    /// Extra inset on the bottom layers, against elephant's foot.
    #[serde(default)]
    pub elephant_foot_mm: f32,
    /// How much the resin shrinks in XY. The slice is scaled up to match.
    #[serde(default)]
    pub shrink_xy_pct: f32,
    /// How much the resin shrinks in Z.
    #[serde(default)]
    pub shrink_z_pct: f32,
    /// Currency per litre. Zero hides the cost.
    #[serde(default)]
    pub price_per_liter: f32,
    /// Cure closed holes in each layer so a detected pocket does not stay empty.
    #[serde(default)]
    pub fill_voids: bool,
}

impl Default for PrintSettings {
    fn default() -> Self {
        Self::from_preset(&RESIN_PRESETS[0])
    }
}

impl PrintSettings {
    pub fn from_preset(preset: &ResinPreset) -> Self {
        Self {
            resin: preset.name.to_string(),
            layer_mm: preset.layer_mm,
            exposure_s: preset.exposure_s,
            bottom_exposure_s: preset.bottom_exposure_s,
            bottom_layers: preset.bottom_layers,
            transition_layers: 0,
            light_off_s: preset.light_off_s,
            lift_mm: preset.lift_mm,
            lift_speed: preset.lift_speed,
            retract_speed: preset.retract_speed,
            bottom_lift_mm: preset.lift_mm,
            bottom_lift_speed: preset.lift_speed,
            bottom_retract_speed: preset.retract_speed,
            anti_alias: 4,
            density_g_ml: 1.10,
            xy_offset_mm: 0.0,
            elephant_foot_mm: 0.0,
            shrink_xy_pct: 0.0,
            shrink_z_pct: 0.0,
            price_per_liter: 0.0,
            fill_voids: false,
        }
    }

    pub fn sanitized(&self) -> Self {
        let mut s = self.clone();
        s.layer_mm = s.layer_mm.clamp(0.01, 0.20);
        s.exposure_s = s.exposure_s.clamp(0.5, 30.0);
        s.bottom_exposure_s = s.bottom_exposure_s.clamp(1.0, 120.0);
        s.bottom_layers = s.bottom_layers.clamp(1, 30);
        s.transition_layers = s.transition_layers.min(40);
        s.light_off_s = s.light_off_s.clamp(0.0, 30.0);
        s.lift_mm = s.lift_mm.clamp(1.0, 20.0);
        s.bottom_lift_mm = s.bottom_lift_mm.clamp(1.0, 20.0);
        s.lift_speed = s.lift_speed.clamp(0.2, 10.0);
        s.retract_speed = s.retract_speed.clamp(0.2, 10.0);
        s.bottom_lift_speed = s.bottom_lift_speed.clamp(0.2, 10.0);
        s.bottom_retract_speed = s.bottom_retract_speed.clamp(0.2, 10.0);
        s.anti_alias = match s.anti_alias {
            2 => 2,
            4 => 4,
            8 => 8,
            _ => 1,
        };
        s.density_g_ml = s.density_g_ml.clamp(0.8, 2.0);
        s.xy_offset_mm = s.xy_offset_mm.clamp(-1.0, 1.0);
        s.elephant_foot_mm = s.elephant_foot_mm.clamp(0.0, 1.0);
        s.shrink_xy_pct = s.shrink_xy_pct.clamp(-2.0, 8.0);
        s.shrink_z_pct = s.shrink_z_pct.clamp(-2.0, 8.0);
        s.price_per_liter = s.price_per_liter.clamp(0.0, 500.0);
        s
    }
}

#[derive(Clone, Copy)]
pub struct ResinPreset {
    pub name: &'static str,
    pub layer_mm: f32,
    pub exposure_s: f32,
    pub light_off_s: f32,
    pub bottom_exposure_s: f32,
    pub bottom_layers: u32,
    pub lift_mm: f32,
    pub lift_speed: f32,
    pub retract_speed: f32,
}

/// Anycubic's published Photon M3 Max table (store guide, Nov 2023).
/// These are starting points. A RERF on the bottle in the vat is the real calibration.
pub const RESIN_PRESETS: &[ResinPreset] = &[
    ResinPreset {
        name: "Colored UV",
        layer_mm: 0.05,
        exposure_s: 3.0,
        light_off_s: 2.5,
        bottom_exposure_s: 50.0,
        bottom_layers: 6,
        lift_mm: 10.0,
        lift_speed: 2.0,
        retract_speed: 3.0,
    },
    ResinPreset {
        name: "Plant-Based",
        layer_mm: 0.05,
        exposure_s: 3.0,
        light_off_s: 2.5,
        bottom_exposure_s: 50.0,
        bottom_layers: 6,
        lift_mm: 10.0,
        lift_speed: 2.0,
        retract_speed: 3.0,
    },
    ResinPreset {
        name: "DLP Craftsman",
        layer_mm: 0.05,
        exposure_s: 2.0,
        light_off_s: 2.5,
        bottom_exposure_s: 35.0,
        bottom_layers: 6,
        lift_mm: 8.0,
        lift_speed: 3.0,
        retract_speed: 4.0,
    },
    ResinPreset {
        name: "UV Tough",
        layer_mm: 0.05,
        exposure_s: 3.0,
        light_off_s: 2.5,
        bottom_exposure_s: 50.0,
        bottom_layers: 6,
        lift_mm: 10.0,
        lift_speed: 2.0,
        retract_speed: 3.0,
    },
    ResinPreset {
        name: "Water-Wash+",
        layer_mm: 0.05,
        exposure_s: 3.0,
        light_off_s: 2.5,
        bottom_exposure_s: 50.0,
        bottom_layers: 6,
        lift_mm: 10.0,
        lift_speed: 2.0,
        retract_speed: 3.0,
    },
    ResinPreset {
        name: "ABS-Like+",
        layer_mm: 0.05,
        exposure_s: 3.0,
        light_off_s: 2.5,
        bottom_exposure_s: 35.0,
        bottom_layers: 6,
        lift_mm: 10.0,
        lift_speed: 2.0,
        retract_speed: 2.0,
    },
    ResinPreset {
        name: "ABS-Like Pro",
        layer_mm: 0.05,
        exposure_s: 3.0,
        light_off_s: 2.5,
        bottom_exposure_s: 35.0,
        bottom_layers: 6,
        lift_mm: 10.0,
        lift_speed: 2.0,
        retract_speed: 3.0,
    },
    ResinPreset {
        name: "High Clear",
        layer_mm: 0.05,
        exposure_s: 4.5,
        light_off_s: 2.5,
        bottom_exposure_s: 30.0,
        bottom_layers: 6,
        lift_mm: 10.0,
        lift_speed: 2.0,
        retract_speed: 3.0,
    },
];

pub fn layer_motion(settings: &PrintSettings, index: u32) -> (f32, f32, f32, f32) {
    let s = settings;
    if index < s.bottom_layers {
        return (
            s.bottom_exposure_s,
            s.bottom_lift_mm,
            s.bottom_lift_speed,
            s.bottom_retract_speed,
        );
    }
    let exposure = if s.transition_layers > 0 && index < s.bottom_layers + s.transition_layers {
        let t = (index - s.bottom_layers + 1) as f32 / (s.transition_layers as f32 + 1.0);
        s.bottom_exposure_s + (s.exposure_s - s.bottom_exposure_s) * t
    } else {
        s.exposure_s
    };
    (exposure, s.lift_mm, s.lift_speed, s.retract_speed)
}

pub fn move_seconds(lift_mm: f32, lift_speed: f32, retract_speed: f32, light_off: f32) -> f32 {
    light_off + lift_mm / lift_speed.max(0.05) + lift_mm / retract_speed.max(0.05)
}
