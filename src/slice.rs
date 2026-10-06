//! Plane cut, scanline fill, shell, drains, islands, and suction checks.
//!
//! Layers are slabs stacked from the bed. Layer `i` is sampled on the plane
//! through the middle of that slab, so a part's height in the file matches
//! the printer's layer count. Blank layers under a floating part are kept:
//! the Photon exposes layer 0 on the plate and then steps by the layer height.

use crate::printer::{layer_motion, move_seconds, Machine, PrintSettings};
use rayon::prelude::*;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct Solid {
    pub vertices: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
    pub hollow: Option<Hollow>,
}

#[derive(Clone, Copy, Debug)]
pub struct Hollow {
    pub wall_mm: f32,
    pub bottom_cap_mm: f32,
    pub top_cap_mm: f32,
    pub infill_spacing_mm: f32,
    pub infill_thickness_mm: f32,
    pub gyroid: bool,
    pub z_min: f32,
    pub z_max: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct Drain {
    pub origin: [f32; 3],
    pub axis: [f32; 3],
    /// Radius at the mouth, outside the surface.
    pub radius_mm: f32,
    /// Radius at the inner end. Zero uses `radius_mm`.
    pub inner_radius_mm: f32,
    /// How far the mouth stands out past the surface.
    pub extend_mm: f32,
    /// How far the hole runs into the model.
    pub depth_mm: f32,
}

#[derive(Clone, Debug)]
pub struct Island {
    pub x_mm: f32,
    pub y_mm: f32,
    pub z_mm: f32,
    pub area_px: u32,
    pub bbox: [i32; 4],
}

#[derive(Clone, Debug)]
pub struct Layer {
    pub index: u32,
    /// Top of this slab, in millimetres above the bed.
    pub z_top_mm: f32,
    pub thickness_mm: f32,
    pub exposure_s: f32,
    pub lift_mm: f32,
    pub lift_speed: f32,
    pub rle: Vec<u8>,
    pub nonzero: u32,
    pub coverage: f64,
    pub islands: Vec<Island>,
    pub seals_cavity_px: u32,
}

#[derive(Clone, Debug)]
pub struct Slice {
    pub width: u32,
    pub height: u32,
    pub layers: Vec<Layer>,
    pub cured_ml: f32,
    pub weight_g: f32,
    pub seconds: u32,
    pub warnings: Vec<String>,
    /// 224×168 silhouette, row-major gray.
    pub thumbnail: Vec<u8>,
    /// Interior air summed over the slice, in millilitres.
    pub cavity_ml: f32,
    /// Layers that closed over an interior pocket.
    pub sealed_layers: u32,
}

pub struct Request<'a> {
    pub solids: &'a [Solid],
    pub drains: &'a [Drain],
    pub machine: Machine,
    pub settings: &'a PrintSettings,
    pub cancel: Option<Arc<AtomicBool>>,
    pub progress: Option<Arc<AtomicU32>>,
}

#[derive(Clone)]
pub(crate) struct Image {
    x0: i32,
    y0: i32,
    width: i32,
    height: i32,
    pixels: Vec<u8>,
}

impl Image {
    fn empty() -> Self {
        Self {
            x0: 0,
            y0: 0,
            width: 0,
            height: 0,
            pixels: Vec::new(),
        }
    }

    fn get(&self, x: i32, y: i32) -> u8 {
        if x < self.x0 || y < self.y0 || x >= self.x0 + self.width || y >= self.y0 + self.height {
            return 0;
        }
        let lx = (x - self.x0) as usize;
        let ly = (y - self.y0) as usize;
        self.pixels[ly * self.width as usize + lx]
    }
}

struct Bits {
    w: usize,
    h: usize,
    data: Vec<u64>,
    /// Inclusive pixel bounds of bits that are currently set.
    dirty: Option<(i32, i32, i32, i32)>,
}

impl Bits {
    fn new(w: usize, h: usize) -> Self {
        let words = (w * h).div_ceil(64);
        Self {
            w,
            h,
            data: vec![0; words],
            dirty: None,
        }
    }

    /// Forget the pixels set since the last clear. Only that rectangle is
    /// written, so a small part does not wipe a full Photon plate every layer.
    fn clear(&mut self) {
        let Some((x0, y0, x1, y1)) = self.dirty.take() else {
            return;
        };
        if self.w == 0 || self.h == 0 || self.data.is_empty() {
            return;
        }
        let x0 = x0.clamp(0, self.w as i32 - 1) as usize;
        let x1 = x1.clamp(0, self.w as i32 - 1) as usize;
        let y0 = y0.clamp(0, self.h as i32 - 1) as usize;
        let y1 = y1.clamp(0, self.h as i32 - 1) as usize;
        for y in y0..=y1 {
            let start = y * self.w + x0;
            let end = y * self.w + x1;
            let first = start / 64;
            let last = (end / 64).min(self.data.len() - 1);
            for word in &mut self.data[first..=last] {
                *word = 0;
            }
        }
    }

    fn set(&mut self, x: i32, y: i32) {
        if x < 0 || y < 0 || x as usize >= self.w || y as usize >= self.h {
            return;
        }
        let i = y as usize * self.w + x as usize;
        self.data[i / 64] |= 1u64 << (i % 64);
        self.dirty = Some(match self.dirty {
            Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
            None => (x, y, x, y),
        });
    }

    fn get(&self, x: i32, y: i32) -> bool {
        if x < 0 || y < 0 || x as usize >= self.w || y as usize >= self.h {
            return false;
        }
        let i = y as usize * self.w + x as usize;
        (self.data[i / 64] >> (i % 64)) & 1 == 1
    }
}

pub fn slice(req: Request<'_>) -> Result<Slice, String> {
    let settings = req.settings.sanitized();
    let machine = req.machine;
    let h = settings.layer_mm;
    let scaled = shrink_solids(req.solids, &settings);
    let solids: &[Solid] = scaled.as_deref().unwrap_or(req.solids);
    if solids.is_empty() {
        return Err("Nothing on the plate to slice.".into());
    }
    let mut max_z = 0.0f32;
    let mut min_z = f32::MAX;
    for solid in solids {
        for v in &solid.vertices {
            max_z = max_z.max(v[2]);
            min_z = min_z.min(v[2]);
        }
    }
    if max_z <= 0.0 {
        return Err("The parts sit entirely below the bed.".into());
    }
    let mut warnings = Vec::new();
    if max_z > machine.size_z + 0.05 {
        warnings.push(format!(
            "The scene is {max_z:.1} mm tall and {} stops at {:.0} mm. The slice is clipped.",
            machine.name, machine.size_z
        ));
        max_z = machine.size_z;
    }
    let mut outside = false;
    for solid in solids {
        for v in &solid.vertices {
            if v[0] < -0.05
                || v[1] < -0.05
                || v[0] > machine.size_x + 0.05
                || v[1] > machine.size_y + 0.05
            {
                outside = true;
                break;
            }
        }
    }
    if outside {
        warnings.push(format!(
            "Part of a model hangs off the {:.2} × {:.2} mm plate. Those pixels are clipped.",
            machine.size_x, machine.size_y
        ));
    }

    let layer_count = ((max_z / h).ceil() as u32).max(1);
    // Sort each mesh once. Every core then walks its own z range forward,
    // instead of re-sorting or clipping triangles that are already behind it.
    let templates: Vec<Sweep> = solids.iter().map(Sweep::build).collect();
    let cores = rayon::current_num_threads().max(1);
    // Several bands per core so a wide base does not leave the top of the
    // model waiting on one thread.
    let bands = cores.saturating_mul(4).min(layer_count as usize).max(1);
    let chunk = (layer_count as usize).div_ceil(bands);
    let ranges: Vec<(u32, u32)> = (0..bands)
        .map(|t| {
            let start = (t * chunk) as u32;
            let end = (((t + 1) * chunk) as u32).min(layer_count);
            (start, end)
        })
        .filter(|(start, end)| start < end)
        .collect();
    let cancel = req.cancel.clone();
    let progress = req.progress.clone();
    let drains = req.drains;
    let mut products: Vec<RangeProduct> = ranges
        .into_par_iter()
        .map(|(start, end)| {
            let sweeps: Vec<Sweep> = templates.iter().map(Sweep::fork).collect();
            raster_range(
                solids, drains, sweeps, machine, &settings, start, end, h, &cancel, &progress,
            )
        })
        .collect::<Result<Vec<_>, String>>()?;
    // Islands and sealed pockets need the previous layer. Each range already
    // did that for its own layers; only the first layer of a later range is
    // still open, and it is one decode per core.
    let mut carry_bits = Bits::new(machine.res_x as usize, machine.res_y as usize);
    let mut carry_seen = SeenMask::new();
    let mut carry_enclosed: Vec<(i32, i32, u32)> = Vec::new();
    let mut thumb = vec![0u8; 224 * 168];
    let mut cavity_px_layers = 0.0f64;
    let mut sealed_layers = 0u32;
    for product in &mut products {
        if let Some(layer) = product.layers.first_mut() {
            if layer.boundary {
                let img = image_from_rle(&layer.rle, machine.res_x as i32, machine.res_y as i32)?;
                let z = (layer.index as f32 + 0.5) * h;
                if img.width > 0 {
                    layer.islands = find_islands(&img, &carry_bits, machine, z, &mut carry_seen);
                    let mut seals = 0u32;
                    for (x, y, area) in &carry_enclosed {
                        if img.get(*x, *y) > 0 {
                            seals += *area;
                        }
                    }
                    layer.seals = seals;
                    if seals > 0 {
                        product.sealed += 1;
                    }
                }
            }
        }
        carry_bits = std::mem::replace(&mut product.tail_bits, Bits::new(1, 1));
        carry_enclosed = std::mem::take(&mut product.tail_enclosed);
        cavity_px_layers += product.cavity_px;
        sealed_layers += product.sealed;
        for (i, v) in product.thumb.iter().enumerate() {
            if *v > thumb[i] {
                thumb[i] = *v;
            }
        }
    }
    let mut last_solid = 0u32;
    let mut raw: Vec<Option<Layer>> = Vec::with_capacity(layer_count as usize);
    for product in products {
        for layer in product.layers {
            if layer.nonzero > 0 {
                last_solid = layer.index;
            }
            raw.push(Some(Layer {
                index: layer.index,
                z_top_mm: layer.z_top_mm,
                thickness_mm: h,
                exposure_s: layer.exposure_s,
                lift_mm: layer.lift_mm,
                lift_speed: layer.lift_speed,
                rle: layer.rle,
                nonzero: layer.nonzero,
                coverage: layer.coverage,
                islands: layer.islands,
                seals_cavity_px: layer.seals,
            }));
        }
    }
    let mut layers = Vec::with_capacity(layer_count as usize);
    for layer in raw.into_iter().take((last_solid + 1) as usize).flatten() {
        layers.push(layer);
    }
    let _ = min_z;
    if layers.is_empty() {
        return Err("The slice produced no exposed pixels. The mesh may be below the bed or inside out. Try Flip normals.".into());
    }

    let pixel_area = (machine.pixel_mm() as f64) * (machine.pixel_mm_y() as f64);
    let mut cured_mm3 = 0.0f64;
    let mut seconds = 0.0f32;
    for layer in &layers {
        cured_mm3 += layer.coverage * pixel_area * h as f64;
        let (_, lift, lift_speed, retract) = layer_motion(&settings, layer.index);
        seconds += layer.exposure_s + move_seconds(lift, lift_speed, retract, settings.wait_s());
    }
    let cured_ml = (cured_mm3 / 1000.0) as f32;
    let cavity_ml = (cavity_px_layers * pixel_area * h as f64 / 1000.0) as f32;
    if sealed_layers > 0 {
        warnings.push(format!(
            "{sealed_layers} layers seal an interior pocket ({cavity_ml:.2} ml of air in the slice). Add a drain at the bottom of a hollow, or it can suction onto the film. Fill enclosed voids closes accidental pockets and leaves a model you hollowed empty."
        ));
    }
    Ok(Slice {
        width: machine.res_x,
        height: machine.res_y,
        layers,
        cured_ml,
        weight_g: cured_ml * settings.density_g_ml,
        seconds: seconds.round() as u32,
        warnings,
        thumbnail: thumb,
        cavity_ml,
        sealed_layers,
    })
}

struct LayerJob {
    index: u32,
    z_top_mm: f32,
    exposure_s: f32,
    lift_mm: f32,
    lift_speed: f32,
    rle: Vec<u8>,
    nonzero: u32,
    coverage: f64,
    islands: Vec<Island>,
    seals: u32,
    /// First exposed layer of a range that did not see the previous range.
    boundary: bool,
}

struct RangeProduct {
    layers: Vec<LayerJob>,
    thumb: Vec<u8>,
    cavity_px: f64,
    sealed: u32,
    tail_bits: Bits,
    tail_enclosed: Vec<(i32, i32, u32)>,
}

fn raster_range(
    solids: &[Solid],
    drains: &[Drain],
    mut sweeps: Vec<Sweep>,
    machine: Machine,
    settings: &PrintSettings,
    start: u32,
    end: u32,
    h: f32,
    cancel: &Option<Arc<AtomicBool>>,
    progress: &Option<Arc<AtomicU32>>,
) -> Result<RangeProduct, String> {
    let mut layers = Vec::with_capacity((end - start) as usize);
    let mut prev_solid = Bits::new(machine.res_x as usize, machine.res_y as usize);
    let mut seen = SeenMask::new();
    let mut prev_enclosed: Vec<(i32, i32, u32)> = Vec::new();
    let mut thumb = vec![0u8; 224 * 168];
    let mut cavity_px = 0.0f64;
    let mut sealed = 0u32;
    for index in start..end {
        if cancel.as_ref().is_some_and(|c| c.load(Ordering::Relaxed)) {
            return Err("Slice cancelled.".into());
        }
        let z = (index as f32 + 0.5) * h;
        let mut image = Image::empty();
        for (solid, sweep) in solids.iter().zip(sweeps.iter_mut()) {
            let active = sweep.activate(z);
            let mut part = raster_solid(solid, z, machine, settings.anti_alias, active);
            // Heal and fill before hollowing. The hollow is cut from the
            // repaired solid, so a model you hollowed stays empty while
            // speckled mesh gaps and accidental pockets become solid resin.
            if settings.fill_voids {
                let radius = (0.75 / machine.pixel_mm()).round().clamp(2.0, 24.0) as i32;
                heal_speckles(&mut part, radius);
                fill_enclosed(&mut part);
            }
            if let Some(hollow) = solid.hollow {
                let cap = z <= hollow.z_min + hollow.bottom_cap_mm
                    || z >= hollow.z_max - hollow.top_cap_mm;
                apply_shell(&mut part, hollow, machine, cap, z);
            }
            blit_max(&mut image, &part);
        }
        // Grow or shrink the cured shape, then punch drains so the hole
        // stays the diameter you asked for.
        offset_image(&mut image, settings.xy_offset_mm, machine);
        if index < settings.bottom_layers {
            offset_image(&mut image, -settings.elephant_foot_mm, machine);
        }
        apply_drains(&mut image, drains, z, machine);
        if settings.image_blur > 0 {
            box_blur(&mut image, settings.image_blur as i32);
        }
        let (exposure, lift, lift_speed, _) = layer_motion(settings, index);
        let boundary = index == start && start > 0 && image.width > 0;
        let mut islands = Vec::new();
        let mut seals = 0u32;
        if image.width > 0 {
            if !boundary && index > 0 {
                islands = find_islands(&image, &prev_solid, machine, z, &mut seen);
                for (x, y, area) in &prev_enclosed {
                    if image.get(*x, *y) > 0 {
                        seals += *area;
                    }
                }
                if seals > 0 {
                    sealed += 1;
                }
            }
            prev_enclosed = enclosed_centroids(&image);
            cavity_px += prev_enclosed.iter().map(|p| p.2 as f64).sum::<f64>();
            paint_bits(&mut prev_solid, &image);
            splat_thumb(&mut thumb, &image, machine);
        } else if index > 0 {
            prev_enclosed.clear();
            prev_solid.clear();
        }
        let (rle, nonzero, coverage) =
            encode_rle(&image, machine.res_x as i32, machine.res_y as i32);
        layers.push(LayerJob {
            index,
            z_top_mm: (index as f32 + 1.0) * h,
            exposure_s: exposure,
            lift_mm: lift,
            lift_speed,
            rle,
            nonzero,
            coverage,
            islands,
            seals,
            boundary,
        });
        if let Some(p) = progress {
            p.fetch_add(1, Ordering::Relaxed);
        }
    }
    Ok(RangeProduct {
        layers,
        thumb,
        cavity_px,
        sealed,
        tail_bits: prev_solid,
        tail_enclosed: prev_enclosed,
    })
}

/// Triangles sorted by the lowest vertex so a layer can skip the rest of a tall mesh.
struct Sweep {
    zmin: Arc<[f32]>,
    zmax: Arc<[f32]>,
    order: Arc<[u32]>,
    cursor: usize,
    active: Vec<u32>,
}

impl Sweep {
    fn build(solid: &Solid) -> Self {
        let n = solid.indices.len() / 3;
        let mut zmin = Vec::with_capacity(n);
        let mut zmax = Vec::with_capacity(n);
        let verts = &solid.vertices;
        for tri in solid.indices.chunks_exact(3) {
            let z_of = |i: u32| verts.get(i as usize).map(|v| v[2]).unwrap_or(f32::NAN);
            let z0 = z_of(tri[0]);
            let z1 = z_of(tri[1]);
            let z2 = z_of(tri[2]);
            if z0.is_finite() && z1.is_finite() && z2.is_finite() {
                zmin.push(z0.min(z1).min(z2));
                zmax.push(z0.max(z1).max(z2));
            } else {
                zmin.push(f32::INFINITY);
                zmax.push(f32::NEG_INFINITY);
            }
        }
        let mut order: Vec<u32> = (0..n as u32).collect();
        order.sort_by(|&a, &b| zmin[a as usize].total_cmp(&zmin[b as usize]));
        Self {
            zmin: zmin.into(),
            zmax: zmax.into(),
            order: order.into(),
            cursor: 0,
            active: Vec::new(),
        }
    }

    fn fork(&self) -> Self {
        Self {
            zmin: Arc::clone(&self.zmin),
            zmax: Arc::clone(&self.zmax),
            order: Arc::clone(&self.order),
            cursor: 0,
            active: Vec::new(),
        }
    }

    /// Triangles with a vertex below `z` and a vertex on or above it.
    /// That is the same test `clip_triangle` uses (`z` counts as above).
    fn activate(&mut self, z: f32) -> &[u32] {
        while self.cursor < self.order.len() {
            let i = self.order[self.cursor] as usize;
            if self.zmin[i] >= z {
                break;
            }
            if self.zmax[i] >= z {
                self.active.push(self.order[self.cursor]);
            }
            self.cursor += 1;
        }
        let zmax = &self.zmax;
        self.active.retain(|&i| zmax[i as usize] >= z);
        &self.active
    }
}

fn raster_solid(solid: &Solid, z: f32, machine: Machine, aa: u8, tris: &[u32]) -> Image {
    let mut segs = clip_tris(solid, z, machine, tris);
    stitch_open_ends(&mut segs);
    // Two supports in one mesh are two closed loops. Even-odd on both at
    // once paints the overlap as a hole, which shows up as a one-layer
    // line where a branch, a trunk, or a foot touch. Fill each loop on
    // its own and keep the darker pixel, the same as a union.
    fill_components(&segs, machine, aa)
}

fn fill_components(segs: &[([f32; 2], [f32; 2])], machine: Machine, aa: u8) -> Image {
    if segs.len() <= 1 {
        return fill_segments(segs, machine, aa);
    }
    let n = segs.len();
    let mut parent: Vec<usize> = (0..n).collect();
    let mut rank = vec![0u8; n];
    let quant = |p: [f32; 2]| ((p[0] * 2.0).round() as i32, (p[1] * 2.0).round() as i32);
    let mut ends: HashMap<(i32, i32), usize> = HashMap::with_capacity(n);
    for (i, (a, b)) in segs.iter().copied().enumerate() {
        for p in [a, b] {
            let key = quant(p);
            if let Some(&other) = ends.get(&key) {
                union_seg(&mut parent, &mut rank, i, other);
            } else {
                ends.insert(key, i);
            }
        }
    }
    let first = find_seg(&mut parent, 0);
    let mut split = false;
    for i in 1..n {
        if find_seg(&mut parent, i) != first {
            split = true;
            break;
        }
    }
    if !split {
        return fill_segments(segs, machine, aa);
    }
    let mut groups: Vec<Vec<([f32; 2], [f32; 2])>> = Vec::new();
    let mut slot: HashMap<usize, usize> = HashMap::new();
    for i in 0..n {
        let root = find_seg(&mut parent, i);
        let index = if let Some(&index) = slot.get(&root) {
            index
        } else {
            let index = groups.len();
            slot.insert(root, index);
            groups.push(Vec::new());
            index
        };
        groups[index].push(segs[i]);
    }
    let mut image = Image::empty();
    for group in &groups {
        blit_max(&mut image, &fill_segments(group, machine, aa));
    }
    image
}

fn find_seg(parent: &mut [usize], mut i: usize) -> usize {
    while parent[i] != i {
        parent[i] = parent[parent[i]];
        i = parent[i];
    }
    i
}

fn union_seg(parent: &mut [usize], rank: &mut [u8], a: usize, b: usize) {
    let mut a = find_seg(parent, a);
    let mut b = find_seg(parent, b);
    if a == b {
        return;
    }
    if rank[a] < rank[b] {
        std::mem::swap(&mut a, &mut b);
    }
    parent[b] = a;
    if rank[a] == rank[b] {
        rank[a] = rank[a].saturating_add(1);
    }
}

fn clip_tris(solid: &Solid, z: f32, machine: Machine, tris: &[u32]) -> Vec<([f32; 2], [f32; 2])> {
    let verts = &solid.vertices;
    let indices = &solid.indices;
    let one = |ti: u32| -> Option<([f32; 2], [f32; 2])> {
        let base = ti as usize * 3;
        if base + 2 >= indices.len() {
            return None;
        }
        let a = *verts.get(indices[base] as usize)?;
        let b = *verts.get(indices[base + 1] as usize)?;
        let c = *verts.get(indices[base + 2] as usize)?;
        let seg = clip_triangle(a, b, c, z)?;
        let p0 = snap_px(to_px(seg.0, machine));
        let p1 = snap_px(to_px(seg.1, machine));
        if (p0[0] - p1[0]).abs() + (p0[1] - p1[1]).abs() > 1e-5 {
            Some((p0, p1))
        } else {
            None
        }
    };
    // Order is preserved so a cracked contour still stitches the same way.
    // A layer worker is already on a rayon thread. Nested row or triangle
    // pools only fight that and make a tall slice slower.
    let nested = rayon::current_thread_index().is_some();
    if !nested && tris.len() >= 4096 {
        tris.par_iter().copied().filter_map(one).collect()
    } else {
        tris.iter().copied().filter_map(one).collect()
    }
}

fn snap_px(p: [f32; 2]) -> [f32; 2] {
    const Q: f32 = 32.0;
    [(p[0] * Q).round() / Q, (p[1] * Q).round() / Q]
}

/// Join contour ends that nearly meet. A sculpt often has a crack a pixel or
/// two wide; even-odd fill turns that into a missing line through the layer.
fn stitch_open_ends(segs: &mut Vec<([f32; 2], [f32; 2])>) {
    const GAP: f32 = 2.5;
    const ALREADY: f32 = 0.4;
    if segs.len() < 2 || segs.len() > 1_500_000 {
        return;
    }
    let cell = GAP;
    let key = |p: [f32; 2]| ((p[0] / cell).floor() as i32, (p[1] / cell).floor() as i32);
    let mut grid: HashMap<(i32, i32), Vec<usize>> = HashMap::new();
    for (i, (a, b)) in segs.iter().enumerate() {
        grid.entry(key(*a)).or_default().push(i * 2);
        grid.entry(key(*b)).or_default().push(i * 2 + 1);
    }
    let ends: Vec<[f32; 2]> = segs.iter().flat_map(|(a, b)| [*a, *b]).collect();
    let nearest = |id: usize, p: [f32; 2]| -> Option<f32> {
        let (cx, cy) = key(p);
        let mut best = f32::MAX;
        for dy in -1..=1 {
            for dx in -1..=1 {
                let Some(bucket) = grid.get(&(cx + dx, cy + dy)) else {
                    continue;
                };
                for &other in bucket {
                    if other == id || other == id ^ 1 {
                        continue;
                    }
                    let q = ends[other];
                    let d2 = (p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2);
                    if d2 < best {
                        best = d2;
                    }
                }
            }
        }
        if best < f32::MAX {
            Some(best)
        } else {
            None
        }
    };
    let mut open = Vec::new();
    for (id, p) in ends.iter().copied().enumerate() {
        match nearest(id, p) {
            None => open.push(id),
            Some(d2) => {
                let d = d2.sqrt();
                if d > ALREADY && d <= GAP {
                    open.push(id);
                }
            }
        }
    }
    if open.len() > 8_000 {
        return;
    }
    let mut used = vec![false; open.len()];
    let mut extra = Vec::new();
    for i in 0..open.len() {
        if used[i] {
            continue;
        }
        let p = ends[open[i]];
        let mut best: Option<(usize, f32)> = None;
        for j in (i + 1)..open.len() {
            if used[j] {
                continue;
            }
            let q = ends[open[j]];
            let d2 = (p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2);
            if d2 > ALREADY * ALREADY
                && d2 <= GAP * GAP
                && best.map(|(_, bd)| d2 < bd).unwrap_or(true)
            {
                best = Some((j, d2));
            }
        }
        if let Some((j, _)) = best {
            used[i] = true;
            used[j] = true;
            extra.push((p, ends[open[j]]));
        }
    }
    segs.extend(extra);
}

fn to_px(p: [f32; 2], machine: Machine) -> [f32; 2] {
    let w = machine.res_x as f32;
    let h = machine.res_y as f32;
    let mut x = p[0] / machine.pixel_mm();
    let mut y = p[1] / machine.pixel_mm_y();
    if machine.mirror_x {
        x = w - x;
    }
    if machine.mirror_y {
        y = h - y;
    }
    if machine.rotate_180 {
        x = w - x;
        y = h - y;
    }
    [x, y]
}

fn from_px(x: f32, y: f32, machine: Machine) -> [f32; 2] {
    let w = machine.res_x as f32;
    let h = machine.res_y as f32;
    let mut px = x;
    let mut py = y;
    if machine.rotate_180 {
        px = w - px;
        py = h - py;
    }
    if machine.mirror_x {
        px = w - px;
    }
    if machine.mirror_y {
        py = h - py;
    }
    [px * machine.pixel_mm(), py * machine.pixel_mm_y()]
}

/// Intersection of a triangle with a horizontal plane.
///
/// A vertex that lands exactly on the plane counts as above it. The wall under
/// a flat face then emits that edge, and the face itself does not emit it a
/// second time. Treating the hit as "on the plane, drop it" left a missing
/// scanline whenever a vertex sat on the layer.
fn clip_triangle(v0: [f32; 3], v1: [f32; 3], v2: [f32; 3], z: f32) -> Option<([f32; 2], [f32; 2])> {
    let vs = [v0, v1, v2];
    let above = |v: [f32; 3]| v[2] >= z;
    let mut pts = Vec::with_capacity(3);
    for i in 0..3 {
        let a = vs[i];
        let b = vs[(i + 1) % 3];
        if above(a) == above(b) {
            continue;
        }
        let denom = b[2] - a[2];
        if denom.abs() < 1e-12 {
            continue;
        }
        let t = ((z - a[2]) / denom).clamp(0.0, 1.0);
        pts.push([a[0] + t * (b[0] - a[0]), a[1] + t * (b[1] - a[1])]);
    }
    if pts.len() < 2 {
        return None;
    }
    let p0 = pts[0];
    let p1 = pts[pts.len() - 1];
    if (p0[0] - p1[0]).abs() + (p0[1] - p1[1]).abs() < 1e-6 {
        None
    } else {
        Some((p0, p1))
    }
}

fn fill_segments(segs: &[([f32; 2], [f32; 2])], machine: Machine, aa: u8) -> Image {
    if segs.is_empty() {
        return Image::empty();
    }
    let mut min_x = f32::MAX;
    let mut min_y = f32::MAX;
    let mut max_x = f32::MIN;
    let mut max_y = f32::MIN;
    for (a, b) in segs {
        min_x = min_x.min(a[0]).min(b[0]);
        min_y = min_y.min(a[1]).min(b[1]);
        max_x = max_x.max(a[0]).max(b[0]);
        max_y = max_y.max(a[1]).max(b[1]);
    }
    let x0 = (min_x.floor() as i32 - 2).clamp(0, machine.res_x as i32);
    let y0 = (min_y.floor() as i32 - 2).clamp(0, machine.res_y as i32);
    let x1 = (max_x.ceil() as i32 + 3).clamp(0, machine.res_x as i32);
    let y1 = (max_y.ceil() as i32 + 3).clamp(0, machine.res_y as i32);
    if x1 <= x0 || y1 <= y0 {
        return Image::empty();
    }
    let width = x1 - x0;
    let height = y1 - y0;
    let mut pixels = vec![0u8; (width * height) as usize];
    let aa = match aa {
        2 => 2,
        4 => 4,
        8 => 8,
        _ => 1,
    };
    // Active edges. A row only tests segments that cross it, which is the
    // same half-open rule as walking every segment: y in [min(y), max(y)).
    #[derive(Clone, Copy)]
    struct Edge {
        ax: f32,
        ay: f32,
        bx: f32,
        by: f32,
        y_lo: f32,
        y_hi: f32,
    }
    let y0f = y0 as f32;
    let mut buckets: Vec<Vec<Edge>> = vec![Vec::new(); height as usize];
    for (a, b) in segs {
        let y_lo = a[1].min(b[1]);
        let y_hi = a[1].max(b[1]);
        if y_hi - y_lo < 1e-8 {
            continue;
        }
        let mut row = ((y_lo - y0f - 0.5).ceil() as i32).max(0);
        if row >= height {
            continue;
        }
        let y_here = y0f + row as f32 + 0.5;
        if y_here < y_lo {
            row += 1;
        }
        if row >= height {
            continue;
        }
        let y_here = y0f + row as f32 + 0.5;
        if y_here >= y_hi {
            continue;
        }
        buckets[row as usize].push(Edge {
            ax: a[0],
            ay: a[1],
            bx: b[0],
            by: b[1],
            y_lo,
            y_hi,
        });
    }
    let mut active: Vec<Edge> = Vec::new();
    for row in 0..height {
        let y = y0f + row as f32 + 0.5;
        active.retain(|e| y >= e.y_lo && y < e.y_hi);
        for edge in &buckets[row as usize] {
            if y >= edge.y_lo && y < edge.y_hi {
                active.push(*edge);
            }
        }
        if active.is_empty() {
            continue;
        }
        let mut hits: Vec<(f32, i32)> = Vec::with_capacity(active.len());
        for edge in &active {
            let denom = edge.by - edge.ay;
            if denom.abs() < 1e-12 {
                continue;
            }
            let t = (y - edge.ay) / denom;
            let x = edge.ax + t * (edge.bx - edge.ax);
            hits.push((x, 0));
        }
        if hits.is_empty() {
            continue;
        }
        hits.sort_by(|p, q| p.0.total_cmp(&q.0));
        // Even-odd across unique crossings. A sculpted STL often has flipped
        // or doubled faces; non-zero winding turns those into empty stripes.
        // Each model is filled on its own and then unioned, so two objects
        // that overlap still print as one solid.
        //
        // Only exact duplicate hits are merged. A wider merge was collapsing
        // two real walls into one crossing and leaving the rest of that
        // scanline blank.
        let mut crossings: Vec<f32> = Vec::with_capacity(hits.len());
        for (x, _) in hits {
            if let Some(prev) = crossings.last() {
                if (x - *prev).abs() < 0.02 {
                    continue;
                }
            }
            crossings.push(x);
        }
        // An odd count is a tangent or a cracked edge, not a license to
        // pair this shape with the next one. A near-duplicate is dropped.
        // Anything else, the most isolated hit is the stray, and removing
        // a real wall instead would paint a line across the gap.
        repair_odd_crossings(&mut crossings);
        if crossings.len() < 2 {
            continue;
        }
        let mut i = 0;
        while i + 1 < crossings.len() {
            paint_span(
                &mut pixels,
                width,
                x0,
                row,
                crossings[i],
                crossings[i + 1],
                aa,
            );
            i += 2;
        }
    }
    seal_hairlines(&mut pixels, width, height);
    Image {
        x0,
        y0,
        width,
        height,
        pixels,
    }
}

/// Fill a one- or two-pixel crack that has solid resin on both sides.
/// That is the missing line in an otherwise solid layer, not a real hole.
fn seal_hairlines(pixels: &mut [u8], width: i32, height: i32) {
    let w = width as usize;
    let h = height as usize;
    if w < 3 || h < 3 {
        return;
    }
    let mut extra: Vec<(usize, u8)> = Vec::new();
    for y in 1..h - 1 {
        for x in 0..w {
            let i = y * w + x;
            if pixels[i] == 0 && pixels[i - w] > 0 && pixels[i + w] > 0 {
                extra.push((i, pixels[i - w].min(pixels[i + w])));
            }
        }
    }
    for y in 1..h.saturating_sub(2) {
        for x in 0..w {
            let i = y * w + x;
            let j = i + w;
            if pixels[i] == 0 && pixels[j] == 0 && pixels[i - w] > 0 && pixels[j + w] > 0 {
                let v = pixels[i - w].min(pixels[j + w]);
                extra.push((i, v));
                extra.push((j, v));
            }
        }
    }
    for (i, v) in &extra {
        if pixels[*i] < *v {
            pixels[*i] = *v;
        }
    }
    extra.clear();
    for y in 0..h {
        let row = y * w;
        for x in 1..w - 1 {
            let i = row + x;
            if pixels[i] == 0 && pixels[i - 1] > 0 && pixels[i + 1] > 0 {
                extra.push((i, pixels[i - 1].min(pixels[i + 1])));
            }
        }
    }
    for (i, v) in extra {
        if pixels[i] < v {
            pixels[i] = v;
        }
    }
}

fn repair_odd_crossings(crossings: &mut Vec<f32>) {
    const DUP: f32 = 0.75;
    while crossings.len() % 2 == 1 && !crossings.is_empty() {
        if crossings.len() == 1 {
            crossings.clear();
            return;
        }
        let mut close_i = 0usize;
        let mut close_d = f32::MAX;
        for i in 0..crossings.len() - 1 {
            let d = crossings[i + 1] - crossings[i];
            if d < close_d {
                close_d = d;
                close_i = i;
            }
        }
        if close_d < DUP {
            crossings.remove(close_i + 1);
            continue;
        }
        let mut lone_i = 0usize;
        let mut lone_d = -1.0f32;
        for i in 0..crossings.len() {
            let left = if i == 0 {
                f32::MAX
            } else {
                crossings[i] - crossings[i - 1]
            };
            let right = if i + 1 == crossings.len() {
                f32::MAX
            } else {
                crossings[i + 1] - crossings[i]
            };
            let near = left.min(right);
            if near > lone_d {
                lone_d = near;
                lone_i = i;
            }
        }
        crossings.remove(lone_i);
    }
}

fn paint_span(pixels: &mut [u8], width: i32, x0: i32, row: i32, s: f32, e: f32, aa: u8) {
    if e <= s {
        return;
    }
    let left = s.floor().max(x0 as f32) as i32;
    let right = (e.ceil() as i32).min(x0 + width);
    let base = row as usize * width as usize;
    for x in left..right {
        let pix_l = x as f32;
        let pix_r = pix_l + 1.0;
        let v = if aa == 1 {
            let c = pix_l + 0.5;
            if c >= s && c < e {
                255
            } else {
                0
            }
        } else {
            let cover = (e.min(pix_r) - s.max(pix_l)).clamp(0.0, 1.0);
            let levels = aa as f32;
            let q = (cover * levels).round() / levels;
            (q * 255.0).round() as u8
        };
        if v > 0 {
            let i = base + (x - x0) as usize;
            if v > pixels[i] {
                pixels[i] = v;
            }
        }
    }
}

fn blit_max(dst: &mut Image, src: &Image) {
    if src.width == 0 {
        return;
    }
    if dst.width == 0 {
        *dst = src.clone();
        return;
    }
    let x0 = dst.x0.min(src.x0);
    let y0 = dst.y0.min(src.y0);
    let x1 = (dst.x0 + dst.width).max(src.x0 + src.width);
    let y1 = (dst.y0 + dst.height).max(src.y0 + src.height);
    let width = x1 - x0;
    let height = y1 - y0;
    let mut pixels = vec![0u8; (width * height) as usize];
    for y in 0..height {
        for x in 0..width {
            let px = x0 + x;
            let py = y0 + y;
            let v = dst.get(px, py).max(src.get(px, py));
            pixels[(y * width + x) as usize] = v;
        }
    }
    *dst = Image {
        x0,
        y0,
        width,
        height,
        pixels,
    };
}

fn shrink_solids(solids: &[Solid], settings: &PrintSettings) -> Option<Vec<Solid>> {
    let xy = settings.shrink_xy_pct;
    let z = settings.shrink_z_pct;
    if xy.abs() < 0.01 && z.abs() < 0.01 {
        return None;
    }
    let sx = 1.0 / (1.0 - xy / 100.0);
    let sz = 1.0 / (1.0 - z / 100.0);
    let mut n = 0.0f32;
    let mut cx = 0.0f32;
    let mut cy = 0.0f32;
    for solid in solids {
        for v in &solid.vertices {
            cx += v[0];
            cy += v[1];
            n += 1.0;
        }
    }
    if n < 1.0 {
        return None;
    }
    cx /= n;
    cy /= n;
    Some(
        solids
            .iter()
            .map(|solid| {
                let vertices = solid
                    .vertices
                    .iter()
                    .map(|v| [cx + (v[0] - cx) * sx, cy + (v[1] - cy) * sx, v[2] * sz])
                    .collect();
                let mut hollow = solid.hollow;
                if let Some(h) = hollow.as_mut() {
                    h.z_min *= sz;
                    h.z_max *= sz;
                }
                Solid {
                    vertices,
                    indices: solid.indices.clone(),
                    hollow,
                }
            })
            .collect(),
    )
}

fn gyroid_wall(x: f32, y: f32, z: f32, spacing: f32, thickness: f32) -> bool {
    let period = spacing.max(0.4);
    let freq = std::f32::consts::TAU / period;
    let g = (freq * x).sin() * (freq * y).cos()
        + (freq * y).sin() * (freq * z).cos()
        + (freq * z).sin() * (freq * x).cos();
    let band = (thickness / period * 3.0).clamp(0.08, 1.2);
    g.abs() < band
}

fn offset_image(img: &mut Image, delta_mm: f32, machine: Machine) {
    if img.width == 0 || delta_mm.abs() < 0.001 {
        return;
    }
    let px = (machine.pixel_mm() + machine.pixel_mm_y()) * 0.5;
    let radius = delta_mm.abs() / px.max(1e-4);
    let limit = (radius * 3.0).round().max(1.0) as u32;
    if delta_mm > 0.0 {
        dilate_image(
            img,
            radius.ceil() as i32 + 1,
            limit,
            machine.res_x as i32,
            machine.res_y as i32,
        );
    } else {
        erode_image(img, limit);
    }
}

fn dilate_image(img: &mut Image, pad: i32, limit: u32, plate_w: i32, plate_h: i32) {
    let pad = pad.max(1);
    let x0 = (img.x0 - pad).clamp(0, plate_w);
    let y0 = (img.y0 - pad).clamp(0, plate_h);
    let x1 = (img.x0 + img.width + pad).clamp(0, plate_w);
    let y1 = (img.y0 + img.height + pad).clamp(0, plate_h);
    let w = (x1 - x0) as usize;
    let h = (y1 - y0) as usize;
    if w == 0 || h == 0 {
        return;
    }
    const INF: u32 = 1_000_000;
    let mut dist = vec![INF; w * h];
    let mut gray = vec![0u8; w * h];
    for y in 0..img.height {
        for x in 0..img.width {
            let v = img.pixels[(y * img.width + x) as usize];
            if v == 0 {
                continue;
            }
            let nx = (img.x0 + x - x0) as usize;
            let ny = (img.y0 + y - y0) as usize;
            let i = ny * w + nx;
            dist[i] = 0;
            gray[i] = v;
        }
    }
    chamfer(&mut dist, w, h);
    let mut pixels = vec![0u8; w * h];
    for i in 0..w * h {
        if dist[i] == 0 {
            pixels[i] = gray[i];
        } else if dist[i] <= limit {
            pixels[i] = 255;
        }
    }
    *img = Image {
        x0,
        y0,
        width: w as i32,
        height: h as i32,
        pixels,
    };
}

fn erode_image(img: &mut Image, limit: u32) {
    let w = img.width as usize;
    let h = img.height as usize;
    if w == 0 || h == 0 {
        return;
    }
    const INF: u32 = 1_000_000;
    let mut dist = vec![0u32; w * h];
    for i in 0..w * h {
        if img.pixels[i] > 0 {
            dist[i] = INF;
        }
    }
    chamfer(&mut dist, w, h);
    for i in 0..w * h {
        if dist[i] <= limit {
            img.pixels[i] = 0;
        }
    }
}

/// Chamfer distance. Orthogonal steps cost 3, diagonals cost 4.
fn chamfer(dist: &mut [u32], w: usize, h: usize) {
    if w == 0 || h == 0 {
        return;
    }
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if dist[i] == 0 {
                continue;
            }
            let mut d = dist[i];
            if x > 0 {
                d = d.min(dist[i - 1].saturating_add(3));
            }
            if y > 0 {
                d = d.min(dist[i - w].saturating_add(3));
            }
            if x > 0 && y > 0 {
                d = d.min(dist[i - w - 1].saturating_add(4));
            }
            if x + 1 < w && y > 0 {
                d = d.min(dist[i - w + 1].saturating_add(4));
            }
            dist[i] = d;
        }
    }
    for y in (0..h).rev() {
        for x in (0..w).rev() {
            let i = y * w + x;
            if dist[i] == 0 {
                continue;
            }
            let mut d = dist[i];
            if x + 1 < w {
                d = d.min(dist[i + 1].saturating_add(3));
            }
            if y + 1 < h {
                d = d.min(dist[i + w].saturating_add(3));
            }
            if x + 1 < w && y + 1 < h {
                d = d.min(dist[i + w + 1].saturating_add(4));
            }
            if x > 0 && y + 1 < h {
                d = d.min(dist[i + w - 1].saturating_add(4));
            }
            dist[i] = d;
        }
    }
}

fn apply_shell(img: &mut Image, hollow: Hollow, machine: Machine, cap: bool, z: f32) {
    if img.width == 0 || cap || hollow.wall_mm <= 0.0 {
        return;
    }
    let w = img.width as usize;
    let h = img.height as usize;
    let n = w * h;
    let mut dist = vec![0u32; n];
    const INF: u32 = 1_000_000;
    for i in 0..n {
        if img.pixels[i] > 0 {
            dist[i] = INF;
        }
    }
    // Chamfer 3-4. Outside pixels stay 0, so this is distance-to-air.
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if dist[i] == 0 {
                continue;
            }
            let mut d = dist[i];
            if x > 0 {
                d = d.min(dist[i - 1].saturating_add(3));
            }
            if y > 0 {
                d = d.min(dist[i - w].saturating_add(3));
            }
            if x > 0 && y > 0 {
                d = d.min(dist[i - w - 1].saturating_add(4));
            }
            if x + 1 < w && y > 0 {
                d = d.min(dist[i - w + 1].saturating_add(4));
            }
            dist[i] = d;
        }
    }
    for y in (0..h).rev() {
        for x in (0..w).rev() {
            let i = y * w + x;
            if dist[i] == 0 {
                continue;
            }
            let mut d = dist[i];
            if x + 1 < w {
                d = d.min(dist[i + 1].saturating_add(3));
            }
            if y + 1 < h {
                d = d.min(dist[i + w].saturating_add(3));
            }
            if x + 1 < w && y + 1 < h {
                d = d.min(dist[i + w + 1].saturating_add(4));
            }
            if x > 0 && y + 1 < h {
                d = d.min(dist[i + w - 1].saturating_add(4));
            }
            dist[i] = d;
        }
    }
    let wall_px = hollow.wall_mm / machine.pixel_mm();
    let threshold = (wall_px * 3.0).round().max(3.0) as u32;
    let period = if hollow.infill_spacing_mm > 0.2 {
        (hollow.infill_spacing_mm / machine.pixel_mm())
            .round()
            .max(2.0) as i32
    } else {
        0
    };
    let thick = (hollow.infill_thickness_mm / machine.pixel_mm())
        .round()
        .max(1.0) as i32;
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let i = (y as usize) * w + x as usize;
            if img.pixels[i] == 0 {
                continue;
            }
            if dist[i] <= threshold {
                continue;
            }
            let keep_infill = if period > 0 && hollow.gyroid {
                let mm = from_px(
                    (img.x0 + x) as f32 + 0.5,
                    (img.y0 + y) as f32 + 0.5,
                    machine,
                );
                gyroid_wall(
                    mm[0],
                    mm[1],
                    z,
                    hollow.infill_spacing_mm,
                    hollow.infill_thickness_mm,
                )
            } else if period > 0 {
                let gx = img.x0 + x;
                let gy = img.y0 + y;
                gx.rem_euclid(period) < thick || gy.rem_euclid(period) < thick
            } else {
                false
            };
            if keep_infill {
                img.pixels[i] = 255;
            } else {
                img.pixels[i] = 0;
            }
        }
    }
}

fn apply_drains(img: &mut Image, drains: &[Drain], z: f32, machine: Machine) {
    if img.width == 0 || drains.is_empty() {
        return;
    }
    for y in 0..img.height {
        for x in 0..img.width {
            let i = (y * img.width + x) as usize;
            if img.pixels[i] == 0 {
                continue;
            }
            let px = img.x0 + x;
            let py = img.y0 + y;
            let mm = from_px(px as f32 + 0.5, py as f32 + 0.5, machine);
            let point = [mm[0], mm[1], z];
            for drain in drains {
                if point_near_drain(point, drain) {
                    img.pixels[i] = 0;
                    break;
                }
            }
        }
    }
}

fn point_near_drain(point: [f32; 3], drain: &Drain) -> bool {
    let axis = normalize(drain.axis);
    let extend = drain.extend_mm.max(0.0);
    let depth = drain.depth_mm.max(0.0);
    let start = [
        drain.origin[0] - axis[0] * extend,
        drain.origin[1] - axis[1] * extend,
        drain.origin[2] - axis[2] * extend,
    ];
    let end = [
        drain.origin[0] + axis[0] * depth,
        drain.origin[1] + axis[1] * depth,
        drain.origin[2] + axis[2] * depth,
    ];
    let ab = [end[0] - start[0], end[1] - start[1], end[2] - start[2]];
    let ap = [
        point[0] - start[0],
        point[1] - start[1],
        point[2] - start[2],
    ];
    let ab2 = ab[0] * ab[0] + ab[1] * ab[1] + ab[2] * ab[2];
    let t = if ab2 < 1e-12 {
        0.0
    } else {
        ((ap[0] * ab[0] + ap[1] * ab[1] + ap[2] * ab[2]) / ab2).clamp(0.0, 1.0)
    };
    let q = [
        start[0] + ab[0] * t,
        start[1] + ab[1] * t,
        start[2] + ab[2] * t,
    ];
    let d = [point[0] - q[0], point[1] - q[1], point[2] - q[2]];
    let dist = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    let outer = drain.radius_mm.max(0.05);
    let inner = if drain.inner_radius_mm > 0.05 {
        drain.inner_radius_mm
    } else {
        outer
    };
    let radius = outer + (inner - outer) * t;
    dist <= radius
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len < 1e-8 {
        [0.0, 0.0, -1.0]
    } else {
        [v[0] / len, v[1] / len, v[2] / len]
    }
}

fn paint_bits(bits: &mut Bits, img: &Image) {
    bits.clear();
    for y in 0..img.height {
        for x in 0..img.width {
            if img.pixels[(y * img.width + x) as usize] > 0 {
                bits.set(img.x0 + x, img.y0 + y);
            }
        }
    }
}

/// Visited pixels for one island scan. The same buffer is restamped each
/// layer, so a tall part does not allocate and wipe a fresh mask every time.
struct SeenMask {
    data: Vec<u8>,
    gen: u8,
}

impl SeenMask {
    fn new() -> Self {
        Self {
            data: Vec::new(),
            gen: 0,
        }
    }

    fn begin(&mut self, n: usize) {
        if self.data.len() < n {
            self.data.resize(n, 0);
        }
        if self.gen == u8::MAX {
            self.data[..n].fill(0);
            self.gen = 1;
        } else {
            self.gen += 1;
        }
    }

    fn seen(&self, i: usize) -> bool {
        self.data[i] == self.gen
    }

    fn mark(&mut self, i: usize) {
        self.data[i] = self.gen;
    }
}

fn find_islands(
    img: &Image,
    prev: &Bits,
    machine: Machine,
    z: f32,
    seen: &mut SeenMask,
) -> Vec<Island> {
    let n = (img.width as usize).saturating_mul(img.height as usize);
    if n == 0 {
        return Vec::new();
    }
    seen.begin(n);
    let mut islands = Vec::new();
    let dirs = [(1, 0), (-1, 0), (0, 1), (0, -1)];
    for sy in 0..img.height {
        for sx in 0..img.width {
            let start = (sy * img.width + sx) as usize;
            if img.pixels[start] == 0 || seen.seen(start) {
                continue;
            }
            let mut stack = VecDeque::new();
            stack.push_back((sx, sy));
            seen.mark(start);
            let mut area = 0u32;
            let mut touches = false;
            let mut sx_sum = 0i64;
            let mut sy_sum = 0i64;
            let mut min_x = sx;
            let mut max_x = sx;
            let mut min_y = sy;
            let mut max_y = sy;
            while let Some((x, y)) = stack.pop_front() {
                area += 1;
                let px = img.x0 + x;
                let py = img.y0 + y;
                sx_sum += px as i64;
                sy_sum += py as i64;
                min_x = min_x.min(x);
                max_x = max_x.max(x);
                min_y = min_y.min(y);
                max_y = max_y.max(y);
                if prev.get(px, py) {
                    touches = true;
                }
                for (dx, dy) in dirs {
                    let nx = x + dx;
                    let ny = y + dy;
                    if nx < 0 || ny < 0 || nx >= img.width || ny >= img.height {
                        continue;
                    }
                    let ni = (ny * img.width + nx) as usize;
                    if img.pixels[ni] == 0 || seen.seen(ni) {
                        continue;
                    }
                    seen.mark(ni);
                    stack.push_back((nx, ny));
                }
            }
            if !touches && area >= 8 {
                let cx = sx_sum as f32 / area as f32 + 0.5;
                let cy = sy_sum as f32 / area as f32 + 0.5;
                let mm = from_px(cx, cy, machine);
                islands.push(Island {
                    x_mm: mm[0],
                    y_mm: mm[1],
                    z_mm: z,
                    area_px: area,
                    bbox: [
                        img.x0 + min_x,
                        img.y0 + min_y,
                        img.x0 + max_x,
                        img.y0 + max_y,
                    ],
                });
            }
        }
    }
    islands
}

/// Close small gaps, then paint the interior solid. Gray speckles and
/// pinholes up to about twice `radius` become 255. A cavity much larger
/// than that, such as a hollow, is left alone. The outer anti-aliased
/// fringe stays, because only the inside of the healed shape is forced.
fn heal_speckles(img: &mut Image, radius: i32) {
    let radius = radius.max(1);
    let w = img.width;
    let h = img.height;
    if w < 3 || h < 3 || img.pixels.len() < (w * h) as usize {
        return;
    }
    let r = radius as usize;
    let pw = w as usize + r * 2;
    let ph = h as usize + r * 2;
    let mut solid = vec![false; pw * ph];
    for y in 0..h as usize {
        for x in 0..w as usize {
            if img.pixels[y * w as usize + x] > 0 {
                solid[(y + r) * pw + (x + r)] = true;
            }
        }
    }
    let closed = morph_close(&solid, pw, ph, radius);
    let interior = erode_mask(&closed, pw, ph, 1);
    for y in 0..h as usize {
        for x in 0..w as usize {
            let i = y * w as usize + x;
            let p = (y + r) * pw + (x + r);
            if interior[p] || (closed[p] && img.pixels[i] == 0) {
                img.pixels[i] = 255;
            }
        }
    }
}

fn morph_close(solid: &[bool], w: usize, h: usize, radius: i32) -> Vec<bool> {
    let dilated = dilate_mask(solid, w, h, radius);
    erode_mask(&dilated, w, h, radius)
}

fn dilate_mask(solid: &[bool], w: usize, h: usize, radius: i32) -> Vec<bool> {
    window_any(solid, w, h, radius, true)
}

fn erode_mask(solid: &[bool], w: usize, h: usize, radius: i32) -> Vec<bool> {
    window_any(solid, w, h, radius, false)
}

/// `any` keeps a pixel when the window contains one solid pixel (dilate).
/// Otherwise the whole window must be solid and in-bounds (erode).
fn window_any(solid: &[bool], w: usize, h: usize, radius: i32, any: bool) -> Vec<bool> {
    let stride = w + 1;
    let mut integral = vec![0u32; stride * (h + 1)];
    for y in 0..h {
        let mut row = 0u32;
        for x in 0..w {
            row += u32::from(solid[y * w + x]);
            integral[(y + 1) * stride + (x + 1)] = integral[y * stride + (x + 1)] + row;
        }
    }
    let mut out = vec![false; w * h];
    let r = radius.max(0) as i32;
    let full = (r as u32 * 2) + 1;
    let full_area = full * full;
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let x0 = (x - r).max(0) as usize;
            let y0 = (y - r).max(0) as usize;
            let x1 = (x + r + 1).min(w as i32) as usize;
            let y1 = (y + r + 1).min(h as i32) as usize;
            let sum = integral[y1 * stride + x1] + integral[y0 * stride + x0]
                - integral[y0 * stride + x1]
                - integral[y1 * stride + x0];
            let span = ((x1 - x0) * (y1 - y0)) as u32;
            out[y as usize * w + x as usize] = if any {
                sum > 0
            } else {
                sum == span && span == full_area
            };
        }
    }
    out
}

/// Paint empty pixels that cannot reach the border of this layer. Those are
/// the same closed holes the suction check reports. Call this before
/// hollowing so the carved cavity is not painted back in.
fn fill_enclosed(img: &mut Image) {
    if img.width <= 0 || img.height <= 0 {
        return;
    }
    let w = img.width;
    let h = img.height;
    let n = (w * h) as usize;
    if img.pixels.len() < n {
        return;
    }
    let mut reach = vec![false; n];
    let mut q = VecDeque::new();
    let push_empty =
        |x: i32, y: i32, reach: &mut [bool], q: &mut VecDeque<(i32, i32)>, pixels: &[u8]| {
            if x < 0 || y < 0 || x >= w || y >= h {
                return;
            }
            let i = (y * w + x) as usize;
            if reach[i] || pixels[i] > 0 {
                return;
            }
            reach[i] = true;
            q.push_back((x, y));
        };
    for x in 0..w {
        push_empty(x, 0, &mut reach, &mut q, &img.pixels);
        push_empty(x, h - 1, &mut reach, &mut q, &img.pixels);
    }
    for y in 0..h {
        push_empty(0, y, &mut reach, &mut q, &img.pixels);
        push_empty(w - 1, y, &mut reach, &mut q, &img.pixels);
    }
    let dirs = [(1i32, 0i32), (-1, 0), (0, 1), (0, -1)];
    while let Some((x, y)) = q.pop_front() {
        for (dx, dy) in dirs {
            push_empty(x + dx, y + dy, &mut reach, &mut q, &img.pixels);
        }
    }
    for (pixel, reached) in img.pixels.iter_mut().zip(reach.iter()) {
        if *pixel == 0 && !reached {
            *pixel = 255;
        }
    }
}

fn enclosed_centroids(img: &Image) -> Vec<(i32, i32, u32)> {
    if img.width == 0 {
        return Vec::new();
    }
    let w = img.width;
    let h = img.height;
    let mut reach = vec![false; (w * h) as usize];
    let mut q = VecDeque::new();
    let push_empty = |x: i32, y: i32, reach: &mut [bool], q: &mut VecDeque<(i32, i32)>| {
        if x < 0 || y < 0 || x >= w || y >= h {
            return;
        }
        let i = (y * w + x) as usize;
        if reach[i] || img.pixels[i] > 0 {
            return;
        }
        reach[i] = true;
        q.push_back((x, y));
    };
    for x in 0..w {
        push_empty(x, 0, &mut reach, &mut q);
        push_empty(x, h - 1, &mut reach, &mut q);
    }
    for y in 0..h {
        push_empty(0, y, &mut reach, &mut q);
        push_empty(w - 1, y, &mut reach, &mut q);
    }
    let dirs = [(1i32, 0i32), (-1, 0), (0, 1), (0, -1)];
    while let Some((x, y)) = q.pop_front() {
        for (dx, dy) in dirs {
            push_empty(x + dx, y + dy, &mut reach, &mut q);
        }
    }
    let mut out = Vec::new();
    let mut seen = reach;
    for sy in 0..h {
        for sx in 0..w {
            let si = (sy * w + sx) as usize;
            if seen[si] || img.pixels[si] > 0 {
                continue;
            }
            let mut stack = VecDeque::new();
            stack.push_back((sx, sy));
            seen[si] = true;
            let mut area = 0u32;
            let mut sx_sum = 0i64;
            let mut sy_sum = 0i64;
            while let Some((x, y)) = stack.pop_front() {
                area += 1;
                sx_sum += (img.x0 + x) as i64;
                sy_sum += (img.y0 + y) as i64;
                for (dx, dy) in dirs {
                    let nx = x + dx;
                    let ny = y + dy;
                    if nx < 0 || ny < 0 || nx >= w || ny >= h {
                        continue;
                    }
                    let ni = (ny * w + nx) as usize;
                    if seen[ni] || img.pixels[ni] > 0 {
                        continue;
                    }
                    seen[ni] = true;
                    stack.push_back((nx, ny));
                }
            }
            if area >= 20 {
                out.push((
                    (sx_sum / area as i64) as i32,
                    (sy_sum / area as i64) as i32,
                    area,
                ));
            }
        }
    }
    out
}

fn splat_thumb(thumb: &mut [u8], img: &Image, machine: Machine) {
    let tw = 224f32;
    let th = 168f32;
    for y in 0..img.height {
        for x in 0..img.width {
            let v = img.pixels[(y * img.width + x) as usize];
            if v == 0 {
                continue;
            }
            let px = img.x0 + x;
            let py = img.y0 + y;
            let tx = ((px as f32 / machine.res_x as f32) * tw) as i32;
            let ty = ((py as f32 / machine.res_y as f32) * th) as i32;
            if (0..224).contains(&tx) && (0..168).contains(&ty) {
                let i = (ty * 224 + tx) as usize;
                if v > thumb[i] {
                    thumb[i] = v;
                }
            }
        }
    }
}

pub(crate) fn encode_rle(img: &Image, plate_w: i32, plate_h: i32) -> (Vec<u8>, u32, f64) {
    if img.width <= 0 || img.height <= 0 {
        let mut out = Vec::new();
        let n = (plate_w.max(0) as usize).saturating_mul(plate_h.max(0) as usize);
        push_pw0(&mut out, 0, n);
        return (out, 0, 0.0);
    }
    let content = img.width as i64 * img.height as i64;
    // A full-plate layer is tens of millions of pixels. Rows do not depend on
    // each other, so those layers split across cores. Smaller layers stay on
    // one core and keep a single run across empty rows. Inside a layer worker
    // the cores are already busy, so the encode stays on that worker.
    let nested = rayon::current_thread_index().is_some();
    if !nested && content >= 250_000 && plate_w > 0 && plate_h > 0 {
        encode_rle_rows(img, plate_w, plate_h)
    } else {
        encode_rle_serial(img, plate_w, plate_h)
    }
}

/// Rebuild the cropped gray image a layer stored. Empty plates stay empty
/// instead of allocating the full LCD.
fn image_from_rle(rle: &[u8], plate_w: i32, plate_h: i32) -> Result<Image, String> {
    if plate_w <= 0 || plate_h <= 0 {
        return Ok(Image::empty());
    }
    let n = (plate_w as usize)
        .checked_mul(plate_h as usize)
        .ok_or("resolution overflow")?;
    let mut min_x = plate_w;
    let mut min_y = plate_h;
    let mut max_x = -1i32;
    let mut max_y = -1i32;
    let mut i = 0usize;
    let mut p = 0usize;
    while i < rle.len() && p < n {
        let (color, reps, step) = next_run(rle, i)?;
        i += step;
        if reps == 0 || p + reps > n {
            return Err(format!("RLE run of {reps} at pixel {p} does not fit {n}"));
        }
        if color > 0 {
            cover_span(
                p,
                reps,
                plate_w as usize,
                &mut min_x,
                &mut min_y,
                &mut max_x,
                &mut max_y,
            );
        }
        p += reps;
    }
    if p != n {
        return Err(format!("RLE decoded {p} pixels, expected {n}"));
    }
    if max_x < min_x || max_y < min_y {
        return Ok(Image::empty());
    }
    let width = max_x - min_x + 1;
    let height = max_y - min_y + 1;
    let mut pixels = vec![0u8; (width * height) as usize];
    let w = plate_w as usize;
    i = 0;
    p = 0;
    while i < rle.len() && p < n {
        let (color, reps, step) = next_run(rle, i)?;
        i += step;
        if color > 0 {
            let mut left = p;
            let end = p + reps;
            while left < end {
                let y = left / w;
                let row_end = ((y + 1) * w).min(end);
                let lx = (left % w) as i32 - min_x;
                let ly = y as i32 - min_y;
                let count = row_end - left;
                let dst = (ly as usize) * width as usize + lx as usize;
                pixels[dst..dst + count].fill(color);
                left = row_end;
            }
        }
        p += reps;
    }
    Ok(Image {
        x0: min_x,
        y0: min_y,
        width,
        height,
        pixels,
    })
}

fn next_run(rle: &[u8], i: usize) -> Result<(u8, usize, usize), String> {
    if i >= rle.len() {
        return Err("RLE ended early".into());
    }
    let b = rle[i];
    let code = b >> 4;
    let low = b & 0x0f;
    if code == 0 || code == 0x0f {
        if i + 1 >= rle.len() {
            return Err("RLE ended inside a black or white run".into());
        }
        let reps = ((low as usize) << 8) | rle[i + 1] as usize;
        let color = if code == 0 { 0 } else { 255 };
        Ok((color, reps, 2))
    } else {
        let color = (code << 4) | code;
        Ok((color, low as usize, 1))
    }
}

fn cover_span(
    p: usize,
    reps: usize,
    plate_w: usize,
    min_x: &mut i32,
    min_y: &mut i32,
    max_x: &mut i32,
    max_y: &mut i32,
) {
    let mut left = p;
    let end = p + reps;
    while left < end {
        let y = (left / plate_w) as i32;
        let row_end = ((y as usize + 1) * plate_w).min(end);
        let x0 = (left % plate_w) as i32;
        let x1 = ((row_end - 1) % plate_w) as i32;
        *min_x = (*min_x).min(x0);
        *max_x = (*max_x).max(x1);
        *min_y = (*min_y).min(y);
        *max_y = (*max_y).max(y);
        left = row_end;
    }
}

fn box_blur(img: &mut Image, radius: i32) {
    let radius = radius.max(0) as usize;
    if radius == 0 || img.width < 1 || img.height < 1 {
        return;
    }
    let w = img.width as usize;
    let h = img.height as usize;
    if img.pixels.len() < w * h {
        return;
    }
    let mut tmp = vec![0u8; w * h];
    blur_rows(&img.pixels, &mut tmp, w, h, radius);
    blur_cols(&tmp, &mut img.pixels, w, h, radius);
}

fn blur_rows(src: &[u8], dst: &mut [u8], w: usize, h: usize, radius: usize) {
    for y in 0..h {
        let row = y * w;
        let mut prefix = vec![0u32; w + 1];
        for x in 0..w {
            prefix[x + 1] = prefix[x] + src[row + x] as u32;
        }
        for x in 0..w {
            let a = x.saturating_sub(radius);
            let b = (x + radius).min(w - 1);
            let sum = prefix[b + 1] - prefix[a];
            let n = (b - a + 1) as u32;
            dst[row + x] = (sum / n) as u8;
        }
    }
}

fn blur_cols(src: &[u8], dst: &mut [u8], w: usize, h: usize, radius: usize) {
    for x in 0..w {
        let mut prefix = vec![0u32; h + 1];
        for y in 0..h {
            prefix[y + 1] = prefix[y] + src[y * w + x] as u32;
        }
        for y in 0..h {
            let a = y.saturating_sub(radius);
            let b = (y + radius).min(h - 1);
            let sum = prefix[b + 1] - prefix[a];
            let n = (b - a + 1) as u32;
            dst[y * w + x] = (sum / n) as u8;
        }
    }
}

fn encode_rle_serial(img: &Image, plate_w: i32, plate_h: i32) -> (Vec<u8>, u32, f64) {
    let mut out = Vec::new();
    let mut nonzero = 0u32;
    let mut coverage = 0.0f64;
    let mut run_color: i32 = -1;
    let mut run_len = 0i32;
    let emit = |out: &mut Vec<u8>, run_color: &mut i32, run_len: &mut i32, color: i32, len: i32| {
        if len <= 0 {
            return;
        }
        if color == *run_color {
            *run_len += len;
            return;
        }
        if *run_color >= 0 && *run_len > 0 {
            push_pw0(out, *run_color as u8, *run_len as usize);
        }
        *run_color = color;
        *run_len = len;
    };
    for y in 0..plate_h {
        let inside = img.height > 0 && y >= img.y0 && y < img.y0 + img.height;
        if !inside {
            emit(&mut out, &mut run_color, &mut run_len, 0, plate_w);
            continue;
        }
        let left = img.x0.clamp(0, plate_w);
        let right = (img.x0 + img.width).clamp(0, plate_w);
        emit(&mut out, &mut run_color, &mut run_len, 0, left);
        let row = (y - img.y0) as usize * img.width as usize;
        for x in left..right {
            let v = img.pixels[row + (x - img.x0) as usize];
            if v > 0 {
                nonzero += 1;
                coverage += v as f64 / 255.0;
            }
            emit(&mut out, &mut run_color, &mut run_len, (v >> 4) as i32, 1);
        }
        emit(&mut out, &mut run_color, &mut run_len, 0, plate_w - right);
    }
    if run_color >= 0 && run_len > 0 {
        push_pw0(&mut out, run_color as u8, run_len as usize);
    }
    (out, nonzero, coverage)
}

fn encode_rle_rows(img: &Image, plate_w: i32, plate_h: i32) -> (Vec<u8>, u32, f64) {
    let y_start = img.y0.clamp(0, plate_h);
    let y_end = (img.y0 + img.height).clamp(y_start, plate_h);
    let mut out = Vec::new();
    push_pw0(
        &mut out,
        0,
        (y_start as usize).saturating_mul(plate_w as usize),
    );
    let rows: Vec<(Vec<u8>, u32, f64)> = (y_start..y_end)
        .into_par_iter()
        .map(|y| encode_one_row(img, plate_w, y))
        .collect();
    let mut nonzero = 0u32;
    let mut coverage = 0.0f64;
    for (bytes, nz, cov) in rows {
        out.extend_from_slice(&bytes);
        nonzero += nz;
        coverage += cov;
    }
    push_pw0(
        &mut out,
        0,
        ((plate_h - y_end) as usize).saturating_mul(plate_w as usize),
    );
    (out, nonzero, coverage)
}

fn encode_one_row(img: &Image, plate_w: i32, y: i32) -> (Vec<u8>, u32, f64) {
    let mut out = Vec::new();
    let mut nonzero = 0u32;
    let mut coverage = 0.0f64;
    let mut run_color: i32 = -1;
    let mut run_len = 0i32;
    let emit = |out: &mut Vec<u8>, run_color: &mut i32, run_len: &mut i32, color: i32, len: i32| {
        if len <= 0 {
            return;
        }
        if color == *run_color {
            *run_len += len;
            return;
        }
        if *run_color >= 0 && *run_len > 0 {
            push_pw0(out, *run_color as u8, *run_len as usize);
        }
        *run_color = color;
        *run_len = len;
    };
    let left = img.x0.clamp(0, plate_w);
    let right = (img.x0 + img.width).clamp(0, plate_w);
    emit(&mut out, &mut run_color, &mut run_len, 0, left);
    if img.height > 0 && y >= img.y0 && y < img.y0 + img.height {
        let row = (y - img.y0) as usize * img.width as usize;
        for x in left..right {
            let v = img.pixels[row + (x - img.x0) as usize];
            if v > 0 {
                nonzero += 1;
                coverage += v as f64 / 255.0;
            }
            emit(&mut out, &mut run_color, &mut run_len, (v >> 4) as i32, 1);
        }
    } else if right > left {
        emit(&mut out, &mut run_color, &mut run_len, 0, right - left);
    }
    emit(&mut out, &mut run_color, &mut run_len, 0, plate_w - right);
    if run_color >= 0 && run_len > 0 {
        push_pw0(&mut out, run_color as u8, run_len as usize);
    }
    (out, nonzero, coverage)
}

fn push_pw0(out: &mut Vec<u8>, color: u8, mut len: usize) {
    // color is already 0..=15
    while len > 0 {
        if color == 0 || color == 15 {
            let done = len.min(0xfff);
            let packed = (done as u16) | ((color as u16) << 12);
            out.push((packed >> 8) as u8);
            out.push((packed & 0xff) as u8);
            len -= done;
        } else {
            let done = len.min(0xf);
            out.push((done as u8) | (color << 4));
            len -= done;
        }
    }
}

pub fn decode_rle(rle: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    let n = (width as usize)
        .checked_mul(height as usize)
        .ok_or("resolution overflow")?;
    let mut pixels = vec![0u8; n];
    let mut i = 0usize;
    let mut p = 0usize;
    while i < rle.len() && p < n {
        let b = rle[i];
        let code = b >> 4;
        let low = b & 0x0f;
        let (color, reps) = if code == 0 || code == 0x0f {
            i += 1;
            if i >= rle.len() {
                return Err("RLE ended inside a black or white run".into());
            }
            let reps = ((low as usize) << 8) | rle[i] as usize;
            let color = if code == 0 { 0 } else { 255 };
            i += 1;
            (color, reps)
        } else {
            i += 1;
            let color = (code << 4) | code;
            (color, low as usize)
        };
        if reps == 0 || p + reps > n {
            return Err(format!("RLE run of {reps} at pixel {p} does not fit {n}"));
        }
        pixels[p..p + reps].fill(color);
        p += reps;
    }
    if p != n {
        return Err(format!("RLE decoded {p} pixels, expected {n}"));
    }
    Ok(pixels)
}

/// One point per column: the lowest island in a cell.
/// A floating blob is an island on many layers. A support wants the first one.
pub fn island_contacts(
    islands: impl IntoIterator<Item = (f32, f32, f32)>,
    cell_mm: f32,
) -> Vec<(f32, f32, f32)> {
    let cell = cell_mm.max(0.2);
    let mut map: HashMap<(i32, i32), (f32, f32, f32)> = HashMap::new();
    for (x, y, z) in islands {
        let key = ((x / cell).round() as i32, (y / cell).round() as i32);
        map.entry(key)
            .and_modify(|old| {
                if z < old.2 {
                    *old = (x, y, z);
                }
            })
            .or_insert((x, y, z));
    }
    let mut out: Vec<(f32, f32, f32)> = map.into_values().collect();
    out.sort_by(|a, b| a.2.total_cmp(&b.2).then(a.0.total_cmp(&b.0)));
    out.truncate(500);
    out
}

/// Downsample a decoded full-plate gray image. `island_boxes` tints matches red.
pub fn preview_rgba(
    rle: &[u8],
    width: u32,
    height: u32,
    max_edge: u32,
    islands: &[Island],
) -> Result<(u32, u32, Vec<u8>), String> {
    let scale = (max_edge as f32 / width as f32).min(max_edge as f32 / height as f32);
    let dw = ((width as f32 * scale).round() as u32).max(1);
    let dh = ((height as f32 * scale).round() as u32).max(1);
    let src = decode_rle(rle, width, height)?;
    let mut rgba = vec![0u8; (dw * dh * 4) as usize];
    for y in 0..dh {
        for x in 0..dw {
            let sx = ((x as f32 + 0.5) / dw as f32 * width as f32) as u32;
            let sy = ((y as f32 + 0.5) / dh as f32 * height as f32) as u32;
            let sx = sx.min(width - 1);
            let sy = sy.min(height - 1);
            let v = src[(sy * width + sx) as usize];
            let mut r = v;
            let mut g = v;
            let mut b = v;
            if v > 0 {
                let ix = sx as i32;
                let iy = sy as i32;
                for island in islands {
                    if ix >= island.bbox[0]
                        && iy >= island.bbox[1]
                        && ix <= island.bbox[2]
                        && iy <= island.bbox[3]
                    {
                        r = 230;
                        g = 70;
                        b = 60;
                        break;
                    }
                }
            }
            let o = ((y * dw + x) * 4) as usize;
            rgba[o] = r;
            rgba[o + 1] = g;
            rgba[o + 2] = b;
            rgba[o + 3] = 255;
        }
    }
    Ok((dw, dh, rgba))
}

/// The sliced layer as a plate-aligned image. Empty pixels are transparent.
/// World millimetres go through the same mirror and rotation as the file,
/// so a white pixel sits on the model that produced it.
pub fn layer_on_plate(
    rle: &[u8],
    width: u32,
    height: u32,
    machine: Machine,
    max_edge: u32,
) -> Result<(u32, u32, Vec<u8>), String> {
    if width == 0 || height == 0 {
        return Err("empty layer".into());
    }
    let src = decode_rle(rle, width, height)?;
    let aspect = machine.size_x / machine.size_y.max(0.1);
    let max_edge = max_edge.clamp(32, 1024);
    let (dw, dh) = if aspect >= 1.0 {
        (max_edge, ((max_edge as f32 / aspect).round() as u32).max(1))
    } else {
        (((max_edge as f32 * aspect).round() as u32).max(1), max_edge)
    };
    let mut rgba = vec![0u8; (dw * dh * 4) as usize];
    for y in 0..dh {
        for x in 0..dw {
            let mx = (x as f32 + 0.5) / dw as f32 * machine.size_x;
            let my = (y as f32 + 0.5) / dh as f32 * machine.size_y;
            let [px, py] = to_px([mx, my], machine);
            let ix = px.floor() as i32;
            let iy = py.floor() as i32;
            let v = if ix >= 0 && iy >= 0 && (ix as u32) < width && (iy as u32) < height {
                src[(iy as u32 * width + ix as u32) as usize]
            } else {
                0
            };
            let o = ((y * dw + x) * 4) as usize;
            if v > 0 {
                rgba[o] = 232;
                rgba[o + 1] = 148;
                rgba[o + 2] = 48;
                rgba[o + 3] = v.max(170);
            }
        }
    }
    Ok((dw, dh, rgba))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::box_mesh;
    use crate::printer::PrintSettings;

    fn machine_no_flip() -> Machine {
        let mut m = Machine::photon_m3_max();
        m.rotate_180 = false;
        m
    }

    #[test]
    fn separate_shapes_do_not_grow_a_line_between_them() {
        let machine = machine_no_flip();
        let wall =
            |x0: f32, x1: f32, y0: f32, y1: f32| vec![([x0, y0], [x0, y1]), ([x1, y0], [x1, y1])];
        let mut segs = wall(10.0, 30.0, 10.0, 40.0);
        segs.extend(wall(80.0, 100.0, 10.0, 40.0));
        // One leftover edge, as a tangent or a cracked contour leaves behind.
        segs.push(([55.0, 10.0], [55.0, 40.0]));
        let img = fill_segments(&segs, machine, 1);
        assert!(img.get(20, 25) > 0, "first shape missing");
        assert!(img.get(90, 25) > 0, "second shape missing");
        assert_eq!(img.get(42, 25), 0, "filled the gap up to the stray edge");
        assert_eq!(img.get(68, 25), 0, "filled the gap past the stray edge");
    }

    #[test]
    fn a_tapered_hole_is_wide_at_the_mouth_and_narrow_inside() {
        let drain = Drain {
            origin: [0.0, 0.0, 0.0],
            axis: [0.0, 0.0, 1.0],
            radius_mm: 2.0,
            inner_radius_mm: 0.5,
            extend_mm: 1.0,
            depth_mm: 4.0,
        };
        assert!(point_near_drain([1.5, 0.0, -1.0], &drain));
        assert!(!point_near_drain([2.5, 0.0, -1.0], &drain));
        assert!(point_near_drain([0.3, 0.0, 4.0], &drain));
        assert!(
            !point_near_drain([1.5, 0.0, 4.0], &drain),
            "the inner end is the narrow diameter"
        );
    }

    #[test]
    fn overlapping_supports_stay_solid_where_they_cross() {
        let machine = machine_no_flip();
        let mut mesh = box_mesh([0.0, 0.0, 0.0], [10.0, 10.0, 5.0]);
        mesh.append(&box_mesh([7.0, 0.0, 0.0], [15.0, 10.0, 5.0]));
        let solid = Solid {
            vertices: mesh.vertices,
            indices: mesh.indices,
            hollow: None,
        };
        let sweep = Sweep::build(&solid);
        let mut sweep = sweep;
        let active = sweep.activate(2.0).to_vec();
        let img = raster_solid(&solid, 2.0, machine, 1, &active);
        let px = |x_mm: f32, y_mm: f32| {
            let [x, y] = to_px([x_mm, y_mm], machine);
            img.get(x.round() as i32, y.round() as i32)
        };
        assert!(px(3.0, 5.0) > 0, "left pillar missing");
        assert!(px(13.0, 5.0) > 0, "right pillar missing");
        assert!(
            px(8.5, 5.0) > 0,
            "the overlap was left empty, which is the one-layer line between supports"
        );
    }

    #[test]
    fn cube_cross_section_matches_its_footprint() {
        let mesh = box_mesh([10.0, 20.0, 0.0], [20.0, 30.0, 10.0]);
        let solid = Solid {
            vertices: mesh.vertices,
            indices: mesh.indices,
            hollow: None,
        };
        let mut settings = PrintSettings::default();
        settings.layer_mm = 0.5;
        settings.anti_alias = 1;
        let slice = slice(Request {
            solids: &[solid],
            drains: &[],
            machine: machine_no_flip(),
            settings: &settings,
            cancel: None,
            progress: None,
        })
        .unwrap();
        let mid = &slice.layers[4];
        let expected = (10.0_f32 / 0.046) * (10.0 / 0.046);
        let got = mid.nonzero as f32;
        assert!(
            (got - expected).abs() / expected < 0.04,
            "nonzero {got} expected ~{expected}"
        );
        let pixels = decode_rle(&mid.rle, slice.width, slice.height).unwrap();
        assert_eq!(
            pixels.iter().filter(|p| **p > 0).count(),
            mid.nonzero as usize
        );
    }

    #[test]
    fn flipped_triangles_still_fill_the_cube() {
        let mut mesh = box_mesh([10.0, 20.0, 0.0], [20.0, 30.0, 10.0]);
        for tri in mesh.indices.chunks_exact_mut(6) {
            tri.swap(1, 2);
        }
        let solid = Solid {
            vertices: mesh.vertices,
            indices: mesh.indices,
            hollow: None,
        };
        let mut settings = PrintSettings::default();
        settings.layer_mm = 0.5;
        settings.anti_alias = 1;
        let slice = slice(Request {
            solids: &[solid],
            drains: &[],
            machine: machine_no_flip(),
            settings: &settings,
            cancel: None,
            progress: None,
        })
        .unwrap();
        let expected = (10.0_f32 / 0.046) * (10.0 / 0.046);
        let got = slice.layers[4].nonzero as f32;
        assert!(
            (got - expected).abs() / expected < 0.06,
            "flipped nonzero {got} expected ~{expected}"
        );
    }

    #[test]
    fn hairline_between_solid_rows_is_filled() {
        let width = 6i32;
        let height = 4i32;
        let mut pixels = vec![0u8; (width * height) as usize];
        for x in 0..width as usize {
            pixels[x] = 255;
            pixels[3 * width as usize + x] = 255;
        }
        seal_hairlines(&mut pixels, width, height);
        for x in 0..width as usize {
            assert_eq!(pixels[width as usize + x], 255, "row 1 col {x}");
            assert_eq!(pixels[2 * width as usize + x], 255, "row 2 col {x}");
        }
    }

    #[test]
    fn edge_on_the_slice_plane_still_draws() {
        let seg = clip_triangle([0.0, 0.0, 5.0], [8.0, 0.0, 5.0], [0.0, 4.0, 0.0], 5.0);
        let (a, b) = seg.expect("a wall under a flat edge must cross the plane");
        let span = (a[0] - b[0]).abs() + (a[1] - b[1]).abs();
        assert!(span > 1.0, "segment collapsed to {a:?} {b:?}");
    }

    #[test]
    fn top_of_a_box_is_not_a_blank_layer() {
        let h = 0.5f32;
        let index = 8u32;
        let plane = (index as f32 + 0.5) * h;
        let mesh = box_mesh([10.0, 20.0, 0.0], [20.0, 30.0, plane]);
        let solid = Solid {
            vertices: mesh.vertices,
            indices: mesh.indices,
            hollow: None,
        };
        let mut settings = PrintSettings::default();
        settings.layer_mm = h;
        settings.anti_alias = 1;
        let slice = slice(Request {
            solids: &[solid],
            drains: &[],
            machine: machine_no_flip(),
            settings: &settings,
            cancel: None,
            progress: None,
        })
        .unwrap();
        let top = slice
            .layers
            .iter()
            .find(|layer| layer.index == index)
            .expect("the layer whose plane is the top face");
        let expected = (10.0_f32 / 0.046) * (10.0 / 0.046);
        let got = top.nonzero as f32;
        assert!(
            (got - expected).abs() / expected < 0.08,
            "top layer nonzero {got} expected ~{expected}"
        );
    }

    #[test]
    fn sphere_mid_layer_is_a_disk() {
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        let stacks = 12i32;
        let slices = 18i32;
        let radius = 8.0f32;
        let cx = 30.0f32;
        let cy = 30.0f32;
        let cz = 8.0f32;
        for stack in 0..=stacks {
            let v = stack as f32 / stacks as f32;
            let phi = std::f32::consts::PI * v;
            for slice_i in 0..=slices {
                let u = slice_i as f32 / slices as f32;
                let theta = std::f32::consts::TAU * u;
                vertices.push([
                    cx + radius * phi.sin() * theta.cos(),
                    cy + radius * phi.sin() * theta.sin(),
                    cz + radius * phi.cos(),
                ]);
            }
        }
        let row = slices + 1;
        for stack in 0..stacks {
            for slice_i in 0..slices {
                let a = stack * row + slice_i;
                let b = a + row;
                indices.extend_from_slice(&[a as u32, b as u32, (a + 1) as u32]);
                indices.extend_from_slice(&[b as u32, (b + 1) as u32, (a + 1) as u32]);
            }
        }
        let solid = Solid {
            vertices,
            indices,
            hollow: None,
        };
        let mut settings = PrintSettings::default();
        settings.layer_mm = 0.05;
        settings.anti_alias = 1;
        let slice = slice(Request {
            solids: &[solid],
            drains: &[],
            machine: machine_no_flip(),
            settings: &settings,
            cancel: None,
            progress: None,
        })
        .unwrap();
        let got = slice.layers.iter().map(|l| l.nonzero).max().unwrap_or(0) as f32;
        let expected = std::f32::consts::PI * (radius / 0.046) * (radius / 0.046);
        assert!(
            (got - expected).abs() / expected < 0.08,
            "sphere {got} expected ~{expected}"
        );
        let islands: usize = slice.layers.iter().map(|l| l.islands.len()).sum();
        assert!(islands < 5, "sphere broke into {islands} islands");
    }

    #[test]
    fn hollow_cube_loses_its_core() {
        let mesh = box_mesh([0.0, 0.0, 0.0], [20.0, 20.0, 10.0]);
        let solid = Solid {
            vertices: mesh.vertices.clone(),
            indices: mesh.indices.clone(),
            hollow: None,
        };
        let hollowed = Solid {
            vertices: mesh.vertices,
            indices: mesh.indices,
            hollow: Some(Hollow {
                wall_mm: 1.5,
                bottom_cap_mm: 0.0,
                top_cap_mm: 0.0,
                infill_spacing_mm: 0.0,
                infill_thickness_mm: 0.4,
                gyroid: false,
                z_min: 0.0,
                z_max: 10.0,
            }),
        };
        let mut settings = PrintSettings::default();
        settings.layer_mm = 1.0;
        settings.anti_alias = 1;
        let full = slice(Request {
            solids: &[solid],
            drains: &[],
            machine: machine_no_flip(),
            settings: &settings,
            cancel: None,
            progress: None,
        })
        .unwrap();
        let shell = slice(Request {
            solids: &[hollowed],
            drains: &[],
            machine: machine_no_flip(),
            settings: &settings,
            cancel: None,
            progress: None,
        })
        .unwrap();
        let full_px = full.layers[4].nonzero;
        let shell_px = shell.layers[4].nonzero;
        assert!(shell_px < full_px / 2, "shell {shell_px} full {full_px}");
        assert!(shell_px > 1000, "shell collapsed: {shell_px}");
    }

    #[test]
    fn heal_speckles_fills_noise_and_leaves_a_large_hole() {
        let mut pixels = vec![0u8; 48 * 48];
        for y in 8..40 {
            for x in 8..40 {
                let noisy = (15..28).contains(&x) && (15..28).contains(&y);
                pixels[(y * 48 + x) as usize] = if noisy {
                    if (x + y) % 3 == 0 {
                        0
                    } else {
                        40
                    }
                } else {
                    255
                };
            }
        }
        // A wide notch open to the right. Closing a couple of pixels must not pack it.
        for y in 20..30 {
            for x in 30..40 {
                pixels[(y * 48 + x) as usize] = 0;
            }
        }
        let mut img = Image {
            x0: 0,
            y0: 0,
            width: 48,
            height: 48,
            pixels,
        };
        heal_speckles(&mut img, 2);
        assert_eq!(img.get(20, 20), 255, "speckled interior stayed gray");
        assert_eq!(img.get(15, 15), 255, "a gap in the speckle stayed open");
        assert_eq!(img.get(34, 24), 0, "a wide notch was packed solid");
        assert_eq!(img.get(2, 2), 0, "the outside of the part was filled");
    }

    #[test]
    fn fill_voids_keeps_a_hollow_empty() {
        let mesh = box_mesh([40.0, 40.0, 0.0], [60.0, 60.0, 10.0]);
        let hollowed = Solid {
            vertices: mesh.vertices,
            indices: mesh.indices,
            hollow: Some(Hollow {
                wall_mm: 1.5,
                bottom_cap_mm: 2.0,
                top_cap_mm: 2.0,
                infill_spacing_mm: 0.0,
                infill_thickness_mm: 0.4,
                gyroid: false,
                z_min: 0.0,
                z_max: 10.0,
            }),
        };
        let mut open = PrintSettings::default();
        open.layer_mm = 0.2;
        open.anti_alias = 1;
        open.fill_voids = false;
        let mut shut = open.clone();
        shut.fill_voids = true;
        let slice_of = |settings: &PrintSettings| {
            slice(Request {
                solids: &[hollowed.clone()],
                drains: &[],
                machine: machine_no_flip(),
                settings,
                cancel: None,
                progress: None,
            })
            .unwrap()
        };
        let plain = slice_of(&open);
        let filled = slice_of(&shut);
        assert!(
            plain.sealed_layers > 0,
            "the cap should seal the hollow, sealed {} cavity {:.3} ml",
            plain.sealed_layers,
            plain.cavity_ml
        );
        assert!(
            filled.sealed_layers > 0,
            "the hollow should stay an empty pocket, sealed {} cavity {:.3} ml",
            filled.sealed_layers,
            filled.cavity_ml
        );
        let mid = |slice: &Slice| {
            slice
                .layers
                .iter()
                .find(|l| (l.z_top_mm - 5.0).abs() < 0.15)
                .map(|l| l.nonzero)
                .unwrap_or(0)
        };
        let plain_mid = mid(&plain);
        let filled_mid = mid(&filled);
        assert!(
            filled_mid < plain_mid + plain_mid / 5 + 200,
            "hollow was packed, filled {filled_mid} plain {plain_mid}"
        );
        assert!(
            filled_mid + 200 > plain_mid / 2,
            "shell collapsed, filled {filled_mid} plain {plain_mid}"
        );
    }

    #[test]
    fn floating_block_is_an_island_and_a_bed_block_is_not() {
        let bed = box_mesh([0.0, 0.0, 0.0], [8.0, 8.0, 2.0]);
        let float = box_mesh([20.0, 0.0, 6.0], [28.0, 8.0, 9.0]);
        let mut mesh = bed;
        mesh.append(&float);
        let solid = Solid {
            vertices: mesh.vertices,
            indices: mesh.indices,
            hollow: None,
        };
        let mut settings = PrintSettings::default();
        settings.layer_mm = 1.0;
        settings.anti_alias = 1;
        let slice = slice(Request {
            solids: &[solid],
            drains: &[],
            machine: machine_no_flip(),
            settings: &settings,
            cancel: None,
            progress: None,
        })
        .unwrap();
        assert!(slice.layers[0].islands.is_empty(), "bed layer flagged");
        let floating = slice
            .layers
            .iter()
            .find(|l| l.z_top_mm > 6.0 && l.nonzero > 0)
            .unwrap();
        assert!(
            !floating.islands.is_empty(),
            "floating block was not flagged, layer {}",
            floating.index
        );
    }

    #[test]
    fn stacked_boxes_keep_each_band_and_skip_the_gaps() {
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        // Layer height is clamped to 0.20 mm, so the gaps are wider than that.
        for i in 0..40 {
            let z0 = i as f32;
            let mesh = box_mesh([0.0, 0.0, z0], [10.0, 10.0, z0 + 0.4]);
            let base = vertices.len() as u32;
            vertices.extend(mesh.vertices);
            indices.extend(mesh.indices.into_iter().map(|idx| idx + base));
        }
        let solid = Solid {
            vertices,
            indices,
            hollow: None,
        };
        let mut settings = PrintSettings::default();
        settings.layer_mm = 0.2;
        settings.anti_alias = 1;
        let slice = slice(Request {
            solids: &[solid],
            drains: &[],
            machine: machine_no_flip(),
            settings: &settings,
            cancel: None,
            progress: None,
        })
        .unwrap();
        let expected = (10.0_f32 / 0.046) * (10.0 / 0.046);
        let check = |index: u32, solid_band: bool| {
            let layer = slice
                .layers
                .iter()
                .find(|layer| layer.index == index)
                .unwrap_or_else(|| panic!("missing layer {index}"));
            if solid_band {
                let got = layer.nonzero as f32;
                assert!(
                    (got - expected).abs() / expected < 0.06,
                    "layer {index} nonzero {got} expected ~{expected}"
                );
            } else {
                assert_eq!(layer.nonzero, 0, "gap layer {index} has pixels");
            }
        };
        check(0, true);
        check(2, false);
        check(5, true);
        check(195, true);
    }

    #[test]
    fn rle_roundtrip_of_a_short_pattern() {
        let mut img = Image {
            x0: 0,
            y0: 0,
            width: 8,
            height: 2,
            pixels: vec![
                0, 255, 128, 128, 0, 0, 255, 17, 0, 0, 0, 0, 255, 255, 255, 0,
            ],
        };
        // encode_rle walks the full plate. Use a tiny fake by calling push through decode of encode.
        let (rle, nonzero, _) = encode_rle(&img, 8, 2);
        assert!(nonzero >= 6);
        let back = decode_rle(&rle, 8, 2).unwrap();
        for (a, b) in img.pixels.iter().zip(back.iter()) {
            assert_eq!(a >> 4, b >> 4, "nibble mismatch {a} vs {b}");
        }
        img.pixels[2] = 0;
    }

    #[test]
    fn large_layer_rle_roundtrips_a_block() {
        let width = 800i32;
        let height = 400i32;
        let mut pixels = vec![0u8; (width * height) as usize];
        for y in 40..120 {
            for x in 50..200 {
                pixels[(y * width + x) as usize] = 255;
            }
        }
        let img = Image {
            x0: 10,
            y0: 20,
            width,
            height,
            pixels,
        };
        let (rle, nonzero, _) = encode_rle(&img, 900, 500);
        assert_eq!(nonzero, 80 * 150);
        let back = decode_rle(&rle, 900, 500).unwrap();
        assert_eq!(back[60 * 900 + 60], 255);
        assert_eq!(back[0], 0);
        assert_eq!(back.iter().filter(|p| **p > 0).count(), nonzero as usize);
        let cropped = image_from_rle(&rle, 900, 500).unwrap();
        assert!(cropped.get(60, 60) > 0);
        assert_eq!(cropped.get(0, 0), 0);
    }

    #[test]
    fn rest_is_counted_once_in_the_time_estimate() {
        let mesh = box_mesh([10.0, 10.0, 0.0], [16.0, 16.0, 1.0]);
        let solid = Solid {
            vertices: mesh.vertices,
            indices: mesh.indices,
            hollow: None,
        };
        let mut plain = PrintSettings::default();
        plain.layer_mm = 0.5;
        plain.anti_alias = 1;
        let mut rested = plain.clone();
        rested.rest_before_s = 1.0;
        rested.rest_after_lift_s = 0.5;
        let run = |settings: &PrintSettings| {
            slice(Request {
                solids: &[solid.clone()],
                drains: &[],
                machine: machine_no_flip(),
                settings,
                cancel: None,
                progress: None,
            })
            .unwrap()
        };
        let a = run(&plain);
        let b = run(&rested);
        assert_eq!(a.layers.len(), b.layers.len());
        assert_eq!(a.layers[0].nonzero, b.layers[0].nonzero);
        let extra = (b.layers.len() as f32) * 1.5;
        let got = b.seconds as f32 - a.seconds as f32;
        assert!(
            (got - extra).abs() < 2.0,
            "rest added {got}s, expected about {extra}"
        );
    }

    #[test]
    fn bits_clear_forgets_only_the_pixels_that_were_set() {
        let mut bits = Bits::new(100, 40);
        bits.set(10, 3);
        bits.set(90, 20);
        assert!(bits.get(10, 3));
        assert!(bits.get(90, 20));
        bits.clear();
        assert!(!bits.get(10, 3));
        assert!(!bits.get(90, 20));
        bits.set(4, 4);
        assert!(bits.get(4, 4));
        assert!(!bits.get(10, 3));
        assert!(!bits.get(90, 20));
        bits.clear();
        assert!(!bits.get(4, 4));
    }

    #[test]
    fn island_contacts_keep_the_lowest_of_a_column() {
        let marks = island_contacts([(1.0, 1.0, 5.0), (1.1, 0.9, 2.0), (8.0, 8.0, 4.0)], 0.8);
        assert_eq!(marks.len(), 2);
        let low = marks.iter().find(|point| point.0 < 3.0).unwrap();
        assert!((low.2 - 2.0).abs() < 0.01, "lowest z {}", low.2);
        assert!(marks.windows(2).all(|pair| pair[0].2 <= pair[1].2));
    }

    #[test]
    fn a_reused_island_mask_does_not_hide_the_next_layer() {
        let machine = machine_no_flip();
        let prev = Bits::new(machine.res_x as usize, machine.res_y as usize);
        let mut block = vec![0u8; 16 * 16];
        for y in 1..5 {
            for x in 1..5 {
                block[y * 16 + x] = 255;
            }
        }
        let first = Image {
            x0: 0,
            y0: 0,
            width: 16,
            height: 16,
            pixels: block,
        };
        let mut later = vec![0u8; 10 * 10];
        for y in 2..6 {
            for x in 4..8 {
                later[y * 10 + x] = 255;
            }
        }
        let second = Image {
            x0: 40,
            y0: 40,
            width: 10,
            height: 10,
            pixels: later,
        };
        let mut fresh_a = SeenMask::new();
        let mut fresh_b = SeenMask::new();
        let mut reused = SeenMask::new();
        let a = find_islands(&first, &prev, machine, 3.0, &mut fresh_a);
        let b = find_islands(&second, &prev, machine, 4.0, &mut fresh_b);
        let again_a = find_islands(&first, &prev, machine, 3.0, &mut reused);
        reused.gen = u8::MAX;
        let again_b = find_islands(&second, &prev, machine, 4.0, &mut reused);
        assert_eq!(a.len(), 1, "first island count {}", a.len());
        assert_eq!(b.len(), 1, "second island count {}", b.len());
        assert_eq!(again_a.len(), a.len());
        assert_eq!(again_a[0].area_px, a[0].area_px);
        assert_eq!(again_b.len(), b.len());
        assert_eq!(again_b[0].area_px, b[0].area_px);
        assert!((again_b[0].z_mm - 4.0).abs() < 0.01);
    }
}
