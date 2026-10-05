//! Plane cut, scanline fill, shell, infill, drains, islands, and suction checks.
//!
//! Layers are slabs stacked from the bed. Layer `i` is sampled on the plane
//! through the middle of that slab, so a part's height in the file matches
//! the printer's layer count. Blank layers under a floating part are kept:
//! the Photon exposes layer 0 on the plate and then steps by the layer height.

use crate::printer::{layer_motion, move_seconds, Machine, PrintSettings};
use std::collections::VecDeque;
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
    pub z_min: f32,
    pub z_max: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct Drain {
    pub origin: [f32; 3],
    pub axis: [f32; 3],
    pub radius_mm: f32,
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
}

impl Bits {
    fn new(w: usize, h: usize) -> Self {
        let words = (w * h).div_ceil(64);
        Self {
            w,
            h,
            data: vec![0; words],
        }
    }

    fn clear(&mut self) {
        self.data.fill(0);
    }

    fn set(&mut self, x: i32, y: i32) {
        if x < 0 || y < 0 || x as usize >= self.w || y as usize >= self.h {
            return;
        }
        let i = y as usize * self.w + x as usize;
        self.data[i / 64] |= 1u64 << (i % 64);
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
    if req.solids.is_empty() {
        return Err("Nothing on the plate to slice.".into());
    }
    let mut max_z = 0.0f32;
    let mut min_z = f32::MAX;
    for solid in req.solids {
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
            "The scene is {max_z:.1} mm tall and the Photon M3 Max stops at {:.0} mm. The slice is clipped.",
            machine.size_z
        ));
        max_z = machine.size_z;
    }
    let mut outside = false;
    for solid in req.solids {
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
        warnings.push(
            "Part of a model hangs off the 298.08 × 165.6 mm plate. Those pixels are clipped."
                .into(),
        );
    }

    let layer_count = ((max_z / h).ceil() as u32).max(1);
    let mut layers = Vec::with_capacity(layer_count as usize);
    let mut prev_solid = Bits::new(machine.res_x as usize, machine.res_y as usize);
    let mut prev_enclosed: Vec<(i32, i32, u32)> = Vec::new();
    let mut thumb = vec![0u8; 224 * 168];
    let px = machine.pixel_mm();

    let mut last_solid = 0u32;
    let mut raw: Vec<Option<Layer>> = Vec::with_capacity(layer_count as usize);

    for index in 0..layer_count {
        if req
            .cancel
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Relaxed))
        {
            return Err("Slice cancelled.".into());
        }
        let z = (index as f32 + 0.5) * h;
        let mut image = Image::empty();
        for solid in req.solids {
            let mut part = raster_solid(solid, z, machine, settings.anti_alias);
            if let Some(hollow) = solid.hollow {
                let cap = z <= hollow.z_min + hollow.bottom_cap_mm
                    || z >= hollow.z_max - hollow.top_cap_mm;
                apply_shell(&mut part, hollow, machine, cap);
            }
            blit_max(&mut image, &part);
        }
        // Supports and other non-hollow solids are already in `solids`.
        apply_drains(&mut image, req.drains, z, machine);
        let (exposure, lift, lift_speed, retract) = layer_motion(&settings, index);
        let _ = retract;

        let mut islands = Vec::new();
        let mut seals = 0u32;
        if image.width > 0 {
            if index > 0 {
                islands = find_islands(&image, &prev_solid, machine, z);
            }
            for (x, y, area) in &prev_enclosed {
                if image.get(*x, *y) > 0 {
                    seals += *area;
                }
            }
            prev_enclosed = enclosed_centroids(&image);
            paint_bits(&mut prev_solid, &image);
            splat_thumb(&mut thumb, &image, machine);
        } else if index > 0 {
            prev_enclosed.clear();
            prev_solid.clear();
        }

        let (rle, nonzero, coverage) =
            encode_rle(&image, machine.res_x as i32, machine.res_y as i32);
        if nonzero > 0 {
            last_solid = index;
        }
        raw.push(Some(Layer {
            index,
            z_top_mm: (index as f32 + 1.0) * h,
            thickness_mm: h,
            exposure_s: exposure,
            lift_mm: lift,
            lift_speed,
            rle,
            nonzero,
            coverage,
            islands,
            seals_cavity_px: seals,
        }));
        if let Some(p) = &req.progress {
            p.store(index + 1, Ordering::Relaxed);
        }
        let _ = min_z;
    }

    for layer in raw.into_iter().take((last_solid + 1) as usize).flatten() {
        layers.push(layer);
    }
    if layers.is_empty() {
        return Err("The slice produced no exposed pixels. The mesh may be below the bed or inside out. Try Flip normals.".into());
    }

    let pixel_area = (px as f64) * (px as f64);
    let mut cured_mm3 = 0.0f64;
    let mut seconds = 0.0f32;
    for layer in &layers {
        cured_mm3 += layer.coverage * pixel_area * h as f64;
        let (_, lift, lift_speed, retract) = layer_motion(&settings, layer.index);
        seconds += layer.exposure_s + move_seconds(lift, lift_speed, retract, settings.light_off_s);
    }
    let cured_ml = (cured_mm3 / 1000.0) as f32;
    Ok(Slice {
        width: machine.res_x,
        height: machine.res_y,
        layers,
        cured_ml,
        weight_g: cured_ml * settings.density_g_ml,
        seconds: seconds.round() as u32,
        warnings,
        thumbnail: thumb,
    })
}

fn raster_solid(solid: &Solid, z: f32, machine: Machine, aa: u8) -> Image {
    let mut segs: Vec<([f32; 2], [f32; 2])> = Vec::new();
    let verts = &solid.vertices;
    for tri in solid.indices.chunks_exact(3) {
        let a = verts[tri[0] as usize];
        let b = verts[tri[1] as usize];
        let c = verts[tri[2] as usize];
        if let Some(seg) = clip_triangle(a, b, c, z) {
            let p0 = to_px(seg.0, machine);
            let p1 = to_px(seg.1, machine);
            if (p0[0] - p1[0]).abs() + (p0[1] - p1[1]).abs() > 1e-5 {
                segs.push((p0, p1));
            }
        }
    }
    fill_segments(&segs, machine, aa)
}

fn to_px(p: [f32; 2], machine: Machine) -> [f32; 2] {
    let w = machine.res_x as f32;
    let h = machine.res_y as f32;
    let mut x = p[0] / machine.pixel_mm();
    let mut y = p[1] / machine.pixel_mm();
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
    [px * machine.pixel_mm(), py * machine.pixel_mm()]
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
    for row in 0..height {
        let y = y0 as f32 + row as f32 + 0.5;
        let mut hits: Vec<(f32, i32)> = Vec::new();
        for (a, b) in segs {
            let (y0e, y1e) = (a[1], b[1]);
            if (y0e <= y && y1e > y) || (y1e <= y && y0e > y) {
                let t = (y - y0e) / (y1e - y0e);
                let x = a[0] + t * (b[0] - a[0]);
                let dir = if y1e > y0e { 1 } else { -1 };
                hits.push((x, dir));
            }
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
        // scanline blank. An odd leftover is a numerical double: drop the
        // tighter of the two until the row pairs up.
        let mut crossings: Vec<f32> = Vec::with_capacity(hits.len());
        for (x, _) in hits {
            if let Some(prev) = crossings.last() {
                if (x - *prev).abs() < 0.02 {
                    continue;
                }
            }
            crossings.push(x);
        }
        while crossings.len() % 2 == 1 && crossings.len() >= 3 {
            let mut best_i = 0usize;
            let mut best_d = f32::MAX;
            for i in 0..crossings.len() - 1 {
                let d = crossings[i + 1] - crossings[i];
                if d < best_d {
                    best_d = d;
                    best_i = i;
                }
            }
            crossings.remove(best_i + 1);
        }
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
    Image {
        x0,
        y0,
        width,
        height,
        pixels,
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

fn apply_shell(img: &mut Image, hollow: Hollow, machine: Machine, cap: bool) {
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
            let keep_infill = if period > 0 {
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
    let start = [
        drain.origin[0] - axis[0] * 0.8,
        drain.origin[1] - axis[1] * 0.8,
        drain.origin[2] - axis[2] * 0.8,
    ];
    let end = [
        drain.origin[0] + axis[0] * drain.depth_mm,
        drain.origin[1] + axis[1] * drain.depth_mm,
        drain.origin[2] + axis[2] * drain.depth_mm,
    ];
    dist_point_segment(point, start, end) <= drain.radius_mm
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len < 1e-8 {
        [0.0, 0.0, -1.0]
    } else {
        [v[0] / len, v[1] / len, v[2] / len]
    }
}

fn dist_point_segment(p: [f32; 3], a: [f32; 3], b: [f32; 3]) -> f32 {
    let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let ap = [p[0] - a[0], p[1] - a[1], p[2] - a[2]];
    let ab2 = ab[0] * ab[0] + ab[1] * ab[1] + ab[2] * ab[2];
    let t = if ab2 < 1e-12 {
        0.0
    } else {
        ((ap[0] * ab[0] + ap[1] * ab[1] + ap[2] * ab[2]) / ab2).clamp(0.0, 1.0)
    };
    let q = [a[0] + ab[0] * t, a[1] + ab[1] * t, a[2] + ab[2] * t];
    let d = [p[0] - q[0], p[1] - q[1], p[2] - q[2]];
    (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
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

fn find_islands(img: &Image, prev: &Bits, machine: Machine, z: f32) -> Vec<Island> {
    let w = img.width as usize;
    let h = img.height as usize;
    let mut seen = vec![false; w * h];
    let mut islands = Vec::new();
    let dirs = [(1, 0), (-1, 0), (0, 1), (0, -1)];
    for sy in 0..img.height {
        for sx in 0..img.width {
            let start = (sy * img.width + sx) as usize;
            if seen[start] || img.pixels[start] == 0 {
                continue;
            }
            let mut stack = VecDeque::new();
            stack.push_back((sx, sy));
            seen[start] = true;
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
                    if seen[ni] || img.pixels[ni] == 0 {
                        continue;
                    }
                    seen[ni] = true;
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
}
