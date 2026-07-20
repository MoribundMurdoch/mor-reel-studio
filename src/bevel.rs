// SPDX-License-Identifier: GPL-2.0-or-later
//! Pure-Rust port of the Intaglio/Cameo Bevel algorithm.
//! Copied verbatim from wearable-dictionary-designer/src-tauri/src/bevel.rs
//! (originally Krita's kis_ls_bevel_emboss_filter.cpp by Dmitry Kazakov,
//! ported to Python/numpy/scipy in the mor_cameo_emboss GIMP plugin, then Rust).
//!
//! Algorithm:
//!   1. Build a binary mask from the source alpha.
//!   2. Compute Euclidean distance transform (Felzenszwalb-Huttenlocher).
//!   3. Normalise to a 0–1 height ramp clamped at `size` pixels.
//!   4. Optional Gaussian softening (sigma = soften / 2, matching scipy).
//!   5. Sobel gradients → surface normals.
//!   6. Lambertian dot product with light vector.
//!   7. Split into highlight (above 0.5) and shadow (below 0.5) alphas.
//!   8. Output two RGBA buffers: pure white + hi alpha, pure black + sh alpha.
//!      The frontend composites these with Screen / Multiply blend modes.

// This file is kept in the shape of the port it came from so the two can still
// be diffed against each other. The index-based loops below are idiomatic in
// the C++ and Python originals and read the same in all three; rewriting them
// as iterators would clear the lint at the cost of that correspondence.
#![allow(clippy::needless_range_loop)]

pub struct BevelParams {
    pub size: u32,
    pub soften: u32,
    pub angle: f32,
    pub altitude: f32,
    pub depth: u32,
    pub hi_opacity: f32,
    pub sh_opacity: f32,
    /// Cameo (raised relief) inverts the surface normals — the only
    /// difference between the intaglio and cameo GIMP plugins.
    pub cameo: bool,
}

pub struct BevelResult {
    pub hi_rgba: Vec<u8>,
    pub sh_rgba: Vec<u8>,
}

fn dt_1d(f: &[f32], n: usize) -> Vec<f32> {
    let mut d = vec![0.0_f32; n];
    let mut v = vec![0_isize; n];
    let mut z = vec![0.0_f32; n + 1];
    let mut k: isize = 0;
    v[0] = 0;
    z[0] = f32::NEG_INFINITY;
    z[1] = f32::INFINITY;

    for q in 1..n {
        let mut s;
        loop {
            let vk = v[k as usize] as f32;
            let qf = q as f32;
            s = ((f[q] + qf * qf) - (f[v[k as usize] as usize] + vk * vk))
                / (2.0 * (qf - vk));
            if s <= z[k as usize] && k > 0 {
                k -= 1;
            } else {
                break;
            }
        }
        k += 1;
        v[k as usize] = q as isize;
        z[k as usize] = s;
        z[k as usize + 1] = f32::INFINITY;
    }

    let mut k: usize = 0;
    for q in 0..n {
        while z[k + 1] < q as f32 {
            k += 1;
        }
        let dx = q as f32 - v[k] as f32;
        d[q] = dx * dx + f[v[k] as usize];
    }
    d
}

fn edt(mask: &[bool], w: usize, h: usize) -> Vec<f32> {
    const INF: f32 = 1e10;
    let mut grid: Vec<f32> = mask.iter().map(|&b| if b { INF } else { 0.0 }).collect();

    for x in 0..w {
        let col: Vec<f32> = (0..h).map(|y| grid[y * w + x]).collect();
        let dt = dt_1d(&col, h);
        for y in 0..h {
            grid[y * w + x] = dt[y];
        }
    }

    for y in 0..h {
        let row_start = y * w;
        let row: Vec<f32> = grid[row_start..row_start + w].to_vec();
        let dt = dt_1d(&row, w);
        grid[row_start..row_start + w].copy_from_slice(&dt);
    }

    grid.iter().map(|&d| d.sqrt()).collect()
}

fn gaussian_kernel(sigma: f32, radius: usize) -> Vec<f32> {
    let mut k = Vec::with_capacity(radius * 2 + 1);
    let mut sum = 0.0_f32;
    for i in 0..=(radius * 2) {
        let x = i as f32 - radius as f32;
        let v = (-0.5 * (x / sigma) * (x / sigma)).exp();
        k.push(v);
        sum += v;
    }
    for v in &mut k {
        *v /= sum;
    }
    k
}

fn convolve_1d(src: &[f32], dst: &mut [f32], w: usize, h: usize, kernel: &[f32], horizontal: bool) {
    let radius = kernel.len() / 2;
    if horizontal {
        for y in 0..h {
            let row = y * w;
            for x in 0..w {
                let mut sum = 0.0_f32;
                for (ki, &kv) in kernel.iter().enumerate() {
                    let ox = x as isize + ki as isize - radius as isize;
                    let sx = ox.clamp(0, w as isize - 1) as usize;
                    sum += src[row + sx] * kv;
                }
                dst[row + x] = sum;
            }
        }
    } else {
        for x in 0..w {
            for y in 0..h {
                let mut sum = 0.0_f32;
                for (ki, &kv) in kernel.iter().enumerate() {
                    let oy = y as isize + ki as isize - radius as isize;
                    let sy = oy.clamp(0, h as isize - 1) as usize;
                    sum += src[sy * w + x] * kv;
                }
                dst[y * w + x] = sum;
            }
        }
    }
}

fn gaussian_blur(src: &mut [f32], w: usize, h: usize, sigma: f32) {
    if sigma <= 0.0 {
        return;
    }
    let radius = (3.0 * sigma).ceil() as usize;
    let kernel = gaussian_kernel(sigma, radius);
    let mut tmp = vec![0.0_f32; w * h];
    convolve_1d(src, &mut tmp, w, h, &kernel, true);
    convolve_1d(&tmp, src, w, h, &kernel, false);
}

fn sobel(height: &[f32], w: usize, h: usize) -> (Vec<f32>, Vec<f32>) {
    let mut gx = vec![0.0_f32; w * h];
    let mut gy = vec![0.0_f32; w * h];
    if w < 3 || h < 3 {
        return (gx, gy);
    }

    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let tl = height[(y - 1) * w + x - 1];
            let tc = height[(y - 1) * w + x];
            let tr = height[(y - 1) * w + x + 1];
            let ml = height[y * w + x - 1];
            let mr = height[y * w + x + 1];
            let bl = height[(y + 1) * w + x - 1];
            let bc = height[(y + 1) * w + x];
            let br = height[(y + 1) * w + x + 1];

            gx[y * w + x] = (tr + 2.0 * mr + br) - (tl + 2.0 * ml + bl);
            gy[y * w + x] = (bl + 2.0 * bc + br) - (tl + 2.0 * tc + tr);
        }
    }
    (gx, gy)
}

pub fn compute_bevel(rgba: &[u8], width: u32, height: u32, params: &BevelParams) -> BevelResult {
    let w = width as usize;
    let h = height as usize;
    let n = w * h;
    assert_eq!(rgba.len(), n * 4, "rgba buffer size mismatch");

    let mask: Vec<bool> = (0..n).map(|i| rgba[i * 4 + 3] > 127).collect();

    let dist = edt(&mask, w, h);
    let size_f = params.size.max(1) as f32;
    let mut height_map: Vec<f32> = dist.iter().map(|&d| (d / size_f).clamp(0.0, 1.0)).collect();

    if params.soften > 0 {
        gaussian_blur(&mut height_map, w, h, params.soften as f32 / 2.0);
    }

    let (gx, gy) = sobel(&height_map, w, h);
    let mut depth_scale = params.depth as f32 / 50.0;
    if params.cameo {
        depth_scale = -depth_scale;
    }

    let az = params.angle.to_radians();
    let alt = params.altitude.to_radians();
    let lx = az.cos() * alt.cos();
    let ly = -az.sin() * alt.cos();
    let lz = alt.sin();

    let mut hi_rgba = vec![0u8; n * 4];
    let mut sh_rgba = vec![0u8; n * 4];

    for i in 0..n {
        let alpha = rgba[i * 4 + 3] as f32 / 255.0;
        let ggx = gx[i] * depth_scale;
        let ggy = gy[i] * depth_scale;
        let denom = (ggx * ggx + ggy * ggy + 1.0).sqrt();
        let nx = ggx / denom;
        let ny = ggy / denom;
        let nz = 1.0 / denom;

        let lighting = (nx * lx + ny * ly + nz * lz).clamp(0.0, 1.0) * alpha;

        let hi_a = ((lighting - 0.5) * 2.0).clamp(0.0, 1.0) * params.hi_opacity * alpha;
        let sh_a = ((0.5 - lighting) * 2.0).clamp(0.0, 1.0) * params.sh_opacity * alpha;

        hi_rgba[i * 4] = 255;
        hi_rgba[i * 4 + 1] = 255;
        hi_rgba[i * 4 + 2] = 255;
        hi_rgba[i * 4 + 3] = (hi_a * 255.0) as u8;

        sh_rgba[i * 4 + 3] = (sh_a * 255.0) as u8;
    }

    BevelResult { hi_rgba, sh_rgba }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disc_rgba(size: usize) -> Vec<u8> {
        let c = size as f32 / 2.0;
        let r = size as f32 * 0.35;
        let mut rgba = vec![0u8; size * size * 4];
        for y in 0..size {
            for x in 0..size {
                let dx = x as f32 + 0.5 - c;
                let dy = y as f32 + 0.5 - c;
                if (dx * dx + dy * dy).sqrt() < r {
                    rgba[(y * size + x) * 4 + 3] = 255;
                }
            }
        }
        rgba
    }

    fn params(cameo: bool) -> BevelParams {
        BevelParams {
            size: 6,
            soften: 0,
            angle: 120.0,
            altitude: 30.0,
            depth: 100,
            hi_opacity: 0.75,
            sh_opacity: 0.75,
            cameo,
        }
    }

    #[test]
    fn bevel_lights_a_disc_and_cameo_inverts_it() {
        let n = 48;
        let rgba = disc_rgba(n);
        let alpha_sum = |buf: &[u8]| -> u64 { buf.chunks(4).map(|p| p[3] as u64).sum() };

        let intaglio = compute_bevel(&rgba, n as u32, n as u32, &params(false));
        assert!(alpha_sum(&intaglio.hi_rgba) > 0, "intaglio produced no highlight");
        assert!(alpha_sum(&intaglio.sh_rgba) > 0, "intaglio produced no shadow");

        // Cameo negates the normals: highlight and shadow swap sides.
        let cameo = compute_bevel(&rgba, n as u32, n as u32, &params(true));
        assert!(alpha_sum(&cameo.hi_rgba) > 0 && alpha_sum(&cameo.sh_rgba) > 0);
        let overlap: u64 = intaglio
            .hi_rgba
            .chunks(4)
            .zip(cameo.hi_rgba.chunks(4))
            .map(|(a, b)| (a[3].min(b[3])) as u64)
            .sum();
        assert!(
            overlap * 4 < alpha_sum(&intaglio.hi_rgba),
            "cameo highlight should land where intaglio highlight is not"
        );
    }
}