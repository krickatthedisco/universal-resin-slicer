//! Photon M3 Max machine profile and the resin starting points Anycubic publishes for it.

use serde::{Deserialize, Serialize};

/// Build volume is exactly the pixel grid times 46 µm:
/// 6480 × 0.046 = 298.08 mm, 3600 × 0.046 = 165.6 mm, Z = 300 mm.
/// Those figures match Anycubic's 7K panel and the SoulCrafted M3 Max profile.
#[derive(Clone, Copy, Debug)]
pub struct Machine {
    pub name: &'static str,
    pub extension: &'static str,
    pub file_version: u32,
    pub size_x: f32,
    pub size_y: f32,
    pub size_z: f32,
    pub res_x: u32,
    pub res_y: u32,
    pub pixel_um: f32,
    pub rotate_180: bool,
    pub mirror_x: bool,
    pub mirror_y: bool,
}

impl Machine {
    pub fn photon_m3_max() -> Self {
        Self {
            name: "Anycubic Photon M3 Max",
            extension: "pm3m",
            file_version: 516,
            size_x: 298.08,
            size_y: 165.6,
            size_z: 300.0,
            res_x: 6480,
            res_y: 3600,
            pixel_um: 46.0,
            // Photonic Etcher's M3 Max profile rotates the exposure 180°.
            // The prepare view can flip this before the first real print.
            rotate_180: true,
            mirror_x: false,
            mirror_y: false,
        }
    }

    pub fn pixel_mm(self) -> f32 {
        self.pixel_um / 1000.0
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
