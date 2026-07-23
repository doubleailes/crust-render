//! Free-standing participating volumes — smoke, fog, absorption, fire.
//!
//! A `VolumeRegion` is an oriented box (a prim's composed transform applied
//! to a local cube) filled with a `DensityField`, carrying scattering /
//! absorption / emission coefficients and a Henyey-Greenstein anisotropy.
//! Regions live *outside* the surface BVH: the integrator asks the
//! `Volumes` aggregate to sample an interaction along each path segment
//! (weighted delta tracking with null collisions against a per-region
//! majorant) and to estimate transmittance along shadow rays (ratio
//! tracking, with an exact analytic fast path when every region crossed is
//! homogeneous). Keeping volumes out of the BVH means their bounds never
//! occlude shadow rays and no placeholder boundary material is needed.

use crate::aabb::AABB;
use crate::medium::hg_phase;
use crate::ray::Ray;
use glam::{Mat4, Vec3, Vec3A};
use sampler::Sampler;

/// Spatial density in local box coordinates, normalized to `[0, 1]^3`.
/// Values are dimensionless multipliers on the region's coefficients.
#[derive(Debug, Clone)]
pub enum DensityField {
    /// density == 1 everywhere inside the box.
    Homogeneous,
    /// Deterministic hash-based value-noise fBm — procedural smoke.
    Noise {
        /// Base frequency (cells across the unit box at octave 0).
        scale: f32,
        octaves: u32,
        /// Per-octave amplitude falloff.
        gain: f32,
        /// Per-octave frequency growth.
        lacunarity: f32,
        /// density = max(0, fbm − threshold) / (1 − threshold); carves the
        /// wispy holes that make noise read as smoke instead of haze.
        threshold: f32,
        seed: u32,
    },
    /// Explicit voxel grid: x-fastest layout `index = x + nx·(y + ny·z)`,
    /// samples at voxel centers, trilinear interpolation, edge clamp.
    Grid {
        nx: usize,
        ny: usize,
        nz: usize,
        data: Vec<f32>,
    },
}

impl DensityField {
    /// Density at `u` in `[0,1]^3` local box coordinates.
    pub fn density(&self, u: Vec3A) -> f32 {
        match self {
            DensityField::Homogeneous => 1.0,
            DensityField::Noise {
                scale,
                octaves,
                gain,
                lacunarity,
                threshold,
                seed,
            } => {
                let fbm = fbm_value_noise(u, *scale, *octaves, *gain, *lacunarity, *seed);
                let t = threshold.clamp(0.0, 0.999);
                ((fbm - t) / (1.0 - t)).max(0.0)
            }
            DensityField::Grid { nx, ny, nz, data } => {
                grid_trilinear(u, *nx, *ny, *nz, data)
            }
        }
    }

    /// A bound on `density` over the box — the field majorant.
    pub fn max_value(&self) -> f32 {
        match self {
            DensityField::Homogeneous => 1.0,
            // fbm is normalized to [0,1]; the threshold remap keeps it ≤ 1.
            DensityField::Noise { .. } => 1.0,
            DensityField::Grid { data, .. } => {
                data.iter().copied().fold(0.0f32, f32::max)
            }
        }
    }
}

/// Integer hash → f32 in [0, 1). Pure integer mixing (no `std::hash`), so
/// noise is bit-identical across platforms and runs.
fn hash3(ix: i32, iy: i32, iz: i32, seed: u32) -> f32 {
    let mut h = (ix as u32).wrapping_mul(0x8da6b343)
        ^ (iy as u32).wrapping_mul(0xd8163841)
        ^ (iz as u32).wrapping_mul(0xcb1ab31f)
        ^ seed.wrapping_mul(0x9e3779b9);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b3c6d);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297a2d39);
    h ^= h >> 15;
    (h >> 8) as f32 / (1u32 << 24) as f32
}

fn smoothstep(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

/// Trilinearly smoothed value noise at frequency `freq`, in [0, 1).
fn value_noise(p: Vec3A, freq: f32, seed: u32) -> f32 {
    let q = p * freq;
    let base = q.floor();
    let (ix, iy, iz) = (base.x as i32, base.y as i32, base.z as i32);
    let f = q - base;
    let (fx, fy, fz) = (smoothstep(f.x), smoothstep(f.y), smoothstep(f.z));

    let mut c = [0.0f32; 8];
    for (n, v) in c.iter_mut().enumerate() {
        let (dx, dy, dz) = ((n & 1) as i32, ((n >> 1) & 1) as i32, ((n >> 2) & 1) as i32);
        *v = hash3(ix + dx, iy + dy, iz + dz, seed);
    }
    let x00 = c[0] + (c[1] - c[0]) * fx;
    let x10 = c[2] + (c[3] - c[2]) * fx;
    let x01 = c[4] + (c[5] - c[4]) * fx;
    let x11 = c[6] + (c[7] - c[6]) * fx;
    let y0 = x00 + (x10 - x00) * fy;
    let y1 = x01 + (x11 - x01) * fy;
    y0 + (y1 - y0) * fz
}

/// fBm over `octaves` octaves, normalized to [0, 1].
fn fbm_value_noise(p: Vec3A, scale: f32, octaves: u32, gain: f32, lacunarity: f32, seed: u32) -> f32 {
    let octaves = octaves.max(1);
    let mut sum = 0.0;
    let mut norm = 0.0;
    let mut amp = 1.0;
    let mut freq = scale;
    for o in 0..octaves {
        sum += amp * value_noise(p, freq, seed.wrapping_add(o));
        norm += amp;
        amp *= gain;
        freq *= lacunarity;
    }
    sum / norm.max(1e-6)
}

/// Trilinear lookup with samples at voxel centers and edge clamping.
fn grid_trilinear(u: Vec3A, nx: usize, ny: usize, nz: usize, data: &[f32]) -> f32 {
    let lookup = |x: usize, y: usize, z: usize| data[x + nx * (y + ny * z)];
    let coord = |v: f32, n: usize| -> (usize, usize, f32) {
        let x = v * n as f32 - 0.5;
        let i = x.floor();
        let f = x - i;
        let i0 = (i.max(0.0) as usize).min(n - 1);
        let i1 = (i0 + 1).min(n - 1);
        // Clamp the fraction too so points outside the sample lattice
        // (v < half a voxel from the boundary) hold the edge value.
        let f = if i < 0.0 { 0.0 } else { f.min(1.0) };
        (i0, i1, f)
    };
    let (x0, x1, fx) = coord(u.x, nx);
    let (y0, y1, fy) = coord(u.y, ny);
    let (z0, z1, fz) = coord(u.z, nz);

    let mut out = 0.0;
    for (wz, z) in [(1.0 - fz, z0), (fz, z1)] {
        for (wy, y) in [(1.0 - fy, y0), (fy, y1)] {
            for (wx, x) in [(1.0 - fx, x0), (fx, x1)] {
                out += wz * wy * wx * lookup(x, y, z);
            }
        }
    }
    out
}

/// An oriented box of participating medium in the scene.
pub struct VolumeRegion {
    /// Maps world points into the local box frame.
    world_to_local: Mat4,
    /// Local box is `[-half, +half]^3` per axis (a USD Cube's `size/2`).
    half_extent: Vec3A,
    /// Conservative world-space AABB of the 8 transformed corners.
    world_aabb: AABB,
    /// Per-channel scattering coefficient at density 1 (densityScale folded in).
    pub sigma_s: Vec3A,
    /// Per-channel absorption coefficient at density 1 (densityScale folded in).
    pub sigma_a: Vec3A,
    /// Henyey-Greenstein anisotropy, `g > 0` = forward scattering.
    pub g: f32,
    /// Emitted radiance; the source term is `σₐ(x) · emission` so emission
    /// follows the density field (fire). ZERO = non-emissive.
    pub emission: Vec3A,
    field: DensityField,
    /// `max_channel(σₐ + σₛ) · field.max_value()` — the tracking majorant.
    majorant_sigma_t: f32,
}

impl VolumeRegion {
    pub fn new(
        local_to_world: Mat4,
        half_extent: Vec3A,
        sigma_s: Vec3A,
        sigma_a: Vec3A,
        g: f32,
        emission: Vec3A,
        density_scale: f32,
        field: DensityField,
    ) -> Self {
        let sigma_s = sigma_s * density_scale;
        let sigma_a = sigma_a * density_scale;
        let mut min = Vec3A::splat(f32::INFINITY);
        let mut max = Vec3A::splat(f32::NEG_INFINITY);
        for n in 0..8 {
            let corner = Vec3A::new(
                if n & 1 == 0 { -half_extent.x } else { half_extent.x },
                if n & 2 == 0 { -half_extent.y } else { half_extent.y },
                if n & 4 == 0 { -half_extent.z } else { half_extent.z },
            );
            let w = local_to_world.transform_point3(Vec3::from(corner));
            min = min.min(Vec3A::from(w));
            max = max.max(Vec3A::from(w));
        }
        let majorant_sigma_t = (sigma_a + sigma_s).max_element() * field.max_value();
        Self {
            world_to_local: local_to_world.inverse(),
            half_extent,
            world_aabb: AABB::new(min, max),
            sigma_s,
            sigma_a,
            g: g.clamp(-0.99, 0.99),
            emission,
            field,
            majorant_sigma_t,
        }
    }

    /// Density multiplier at a world point; 0 outside the box.
    pub fn density(&self, p_world: Vec3A) -> f32 {
        let p = Vec3A::from(self.world_to_local.transform_point3(Vec3::from(p_world)));
        let h = self.half_extent;
        if p.x.abs() > h.x || p.y.abs() > h.y || p.z.abs() > h.z {
            return 0.0;
        }
        let u = (p + h) / (h * 2.0);
        self.field.density(u)
    }

    /// Entry/exit distances of `ray` through the box, in world-ray
    /// parameter units. The local direction is deliberately NOT
    /// renormalized so the returned interval stays parameterized on the
    /// world ray.
    pub fn intersect(&self, ray: &Ray) -> Option<(f32, f32)> {
        let o = Vec3A::from(self.world_to_local.transform_point3(Vec3::from(ray.origin())));
        let d = Vec3A::from(self.world_to_local.transform_vector3(Vec3::from(ray.direction())));
        let mut t0 = 0.0f32;
        let mut t1 = f32::INFINITY;
        for a in 0..3 {
            let h = self.half_extent[a];
            if d[a].abs() < 1e-9 {
                if o[a].abs() > h {
                    return None;
                }
                continue;
            }
            let inv = 1.0 / d[a];
            let mut ta = (-h - o[a]) * inv;
            let mut tb = (h - o[a]) * inv;
            if ta > tb {
                std::mem::swap(&mut ta, &mut tb);
            }
            t0 = t0.max(ta);
            t1 = t1.min(tb);
            if t1 <= t0 {
                return None;
            }
        }
        Some((t0, t1))
    }

    pub fn is_homogeneous(&self) -> bool {
        matches!(self.field, DensityField::Homogeneous)
    }

    fn sigma_t_at_density(&self, d: f32) -> Vec3A {
        (self.sigma_a + self.sigma_s) * d
    }
}

/// The phase function at a scatter point — a σₛ-weighted mixture of the
/// HG lobes of every region overlapping that point. One entry in the
/// common single-region case. Sampling picks a lobe by weight and samples
/// its HG exactly, so `pdf` (the mixture) equals the actual phase-function
/// value and the two stay consistent for MIS by construction.
#[derive(Debug, Clone)]
pub struct PhaseMix {
    /// `(weight, g)` pairs; weights sum to 1.
    lobes: Vec<(f32, f32)>,
}

impl PhaseMix {
    pub fn single(g: f32) -> Self {
        Self { lobes: vec![(1.0, g)] }
    }

    /// Sample an outgoing direction given the incoming propagation
    /// direction `wi` (normalized).
    pub fn sample(&self, wi: Vec3A, sampler: &mut dyn Sampler) -> Vec3A {
        let mut pick = sampler.next_1d();
        let mut g = self.lobes[self.lobes.len() - 1].1;
        for &(w, lg) in &self.lobes {
            if pick < w {
                g = lg;
                break;
            }
            pick -= w;
        }
        let uv = sampler.next_2d();
        crate::medium::sample_henyey_greenstein(wi, g, uv[0], uv[1])
    }

    /// Solid-angle pdf of `sample` — also the phase-function value.
    pub fn pdf(&self, cos_theta: f32) -> f32 {
        self.lobes
            .iter()
            .map(|&(w, g)| w * hg_phase(cos_theta, g))
            .sum()
    }
}

/// Result of sampling a path segment against the volume aggregate.
pub enum VolumeEvent {
    /// A real scattering collision inside some region.
    Scatter {
        /// Distance along the ray.
        t: f32,
        /// The collision point.
        p: Vec3A,
        /// Full path weight of the event: accumulated null-collision
        /// weights × `σₛ(x) / (σ̄ · P_s)`. Already includes transmittance
        /// up to `t`; multiplies everything at and beyond this vertex.
        weight: Vec3A,
        /// Phase function at the point (mixture under region overlap).
        phase: PhaseMix,
        /// Volume emission accumulated along `[0, t]`, pre-weighted by the
        /// walk weights at each emission point. Must NOT be attenuated
        /// again by the caller.
        emitted: Vec3A,
    },
    /// The segment was crossed without a real collision.
    Passthrough {
        /// Transmittance estimate over the segment (exact for
        /// all-homogeneous regions; a ratio-tracking sample otherwise).
        transmittance: Vec3A,
        /// Pre-weighted volume emission along the segment.
        emitted: Vec3A,
    },
}

/// All volume regions in the scene.
#[derive(Default)]
pub struct Volumes {
    regions: Vec<VolumeRegion>,
}

impl Volumes {
    pub fn new(regions: Vec<VolumeRegion>) -> Self {
        Self { regions }
    }

    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }

    pub fn regions(&self) -> &[VolumeRegion] {
        &self.regions
    }

    /// Clipped per-region intervals over `(t_eps, t_max)`, and the summed
    /// majorant of the intersected regions. Summing majorants over the
    /// union span majorizes the summed extinction everywhere on it
    /// (superposed extinction of overlapping media is exact).
    fn active_intervals(
        &self,
        ray: &Ray,
        t_eps: f32,
        t_max: f32,
    ) -> (Vec<(usize, f32, f32)>, f32) {
        let mut spans = Vec::new();
        let mut majorant = 0.0f32;
        for (i, region) in self.regions.iter().enumerate() {
            if region.majorant_sigma_t <= 0.0 {
                continue;
            }
            if !region.world_aabb.hit(ray, t_eps, t_max) {
                continue;
            }
            if let Some((t0, t1)) = region.intersect(ray) {
                let a = t0.max(t_eps);
                let b = t1.min(t_max);
                if b > a {
                    spans.push((i, a, b));
                    majorant += region.majorant_sigma_t;
                }
            }
        }
        (spans, majorant)
    }

    /// Sample one volume interaction along `ray` over `(t_eps, t_max)`,
    /// where `t_max` is the distance to whatever event (surface hit,
    /// carried-medium scatter) would otherwise terminate the segment.
    pub fn sample_interaction(
        &self,
        ray: &Ray,
        t_eps: f32,
        t_max: f32,
        sampler: &mut dyn Sampler,
    ) -> VolumeEvent {
        let (spans, majorant) = self.active_intervals(ray, t_eps, t_max);
        if spans.is_empty() || majorant <= 0.0 {
            return VolumeEvent::Passthrough {
                transmittance: Vec3A::ONE,
                emitted: Vec3A::ZERO,
            };
        }
        let start = spans.iter().map(|s| s.1).fold(f32::INFINITY, f32::min);
        let end = spans.iter().map(|s| s.2).fold(0.0f32, f32::max);

        let mut t = start;
        let mut w = Vec3A::ONE;
        let mut emitted = Vec3A::ZERO;
        loop {
            t += -(1.0 - sampler.next_1d()).ln() / majorant;
            if t >= end {
                return VolumeEvent::Passthrough {
                    transmittance: w,
                    emitted,
                };
            }
            let p = ray.at(t);

            // Pointwise coefficients summed over regions covering `p`.
            let mut sigma_s_x = Vec3A::ZERO;
            let mut sigma_t_x = Vec3A::ZERO;
            let mut lobes: Vec<(f32, f32)> = Vec::new();
            for &(i, a, b) in &spans {
                if t < a || t > b {
                    continue;
                }
                let region = &self.regions[i];
                let d = region.density(p);
                if d <= 0.0 {
                    continue;
                }
                let ss = region.sigma_s * d;
                sigma_s_x += ss;
                sigma_t_x += region.sigma_t_at_density(d);
                emitted += w * (region.sigma_a * d) * region.emission / majorant;
                let m = ss.max_element();
                if m > 0.0 {
                    lobes.push((m, region.g));
                }
            }

            let p_scatter = (sigma_s_x.max_element() / majorant).clamp(0.0, 1.0);
            if sampler.next_1d() < p_scatter {
                let total: f32 = lobes.iter().map(|l| l.0).sum();
                for l in &mut lobes {
                    l.0 /= total;
                }
                return VolumeEvent::Scatter {
                    t,
                    p,
                    weight: w * sigma_s_x / (majorant * p_scatter),
                    phase: PhaseMix { lobes },
                    emitted,
                };
            }
            // Null/absorb combined: per-channel numerators are
            // non-negative because the majorant bounds max-channel σₜ.
            w *= (Vec3A::splat(majorant) - sigma_t_x) / (majorant * (1.0 - p_scatter));
            if w.max_element() < 1e-5 {
                return VolumeEvent::Passthrough {
                    transmittance: Vec3A::ZERO,
                    emitted,
                };
            }
        }
    }

    /// Transmittance along `ray` over `(t_eps, t_max)` — ratio tracking,
    /// with an exact analytic product when every region crossed is
    /// homogeneous (noise-free fog shadows; exponents of overlapping
    /// regions add, so the per-region product is exact).
    pub fn transmittance(
        &self,
        ray: &Ray,
        t_eps: f32,
        t_max: f32,
        sampler: &mut dyn Sampler,
    ) -> Vec3A {
        let (spans, majorant) = self.active_intervals(ray, t_eps, t_max);
        if spans.is_empty() || majorant <= 0.0 {
            return Vec3A::ONE;
        }

        if spans.iter().all(|&(i, _, _)| self.regions[i].is_homogeneous()) {
            let mut tr = Vec3A::ONE;
            for &(i, a, b) in &spans {
                let e = self.regions[i].sigma_t_at_density(1.0) * (b - a);
                tr *= Vec3A::new((-e.x).exp(), (-e.y).exp(), (-e.z).exp());
            }
            return tr;
        }

        let start = spans.iter().map(|s| s.1).fold(f32::INFINITY, f32::min);
        let end = spans.iter().map(|s| s.2).fold(0.0f32, f32::max);
        let mut t = start;
        let mut w = Vec3A::ONE;
        loop {
            t += -(1.0 - sampler.next_1d()).ln() / majorant;
            if t >= end {
                return w;
            }
            let p = ray.at(t);
            let mut sigma_t_x = Vec3A::ZERO;
            for &(i, a, b) in &spans {
                if t < a || t > b {
                    continue;
                }
                let region = &self.regions[i];
                sigma_t_x += region.sigma_t_at_density(region.density(p));
            }
            w *= (Vec3A::splat(majorant) - sigma_t_x) / majorant;
            if w.max_element() < 1e-5 {
                return Vec3A::ZERO;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sampler::RngSampler;

    fn unit_region(sigma_s: Vec3A, sigma_a: Vec3A, g: f32, field: DensityField) -> VolumeRegion {
        VolumeRegion::new(
            Mat4::IDENTITY,
            Vec3A::splat(0.5),
            sigma_s,
            sigma_a,
            g,
            Vec3A::ZERO,
            1.0,
            field,
        )
    }

    fn x_ray() -> Ray {
        Ray::new(Vec3A::new(-2.0, 0.0, 0.0), Vec3A::X)
    }

    #[test]
    fn homogeneous_transmittance_is_exact_beer_lambert() {
        let volumes = Volumes::new(vec![unit_region(
            Vec3A::splat(0.7),
            Vec3A::new(0.2, 0.4, 0.9),
            0.0,
            DensityField::Homogeneous,
        )]);
        let mut s = RngSampler::default();
        let tr = volumes.transmittance(&x_ray(), 1e-3, 10.0, &mut s);
        let sigma_t = Vec3A::splat(0.7) + Vec3A::new(0.2, 0.4, 0.9);
        let expect = Vec3A::new(
            (-sigma_t.x).exp(),
            (-sigma_t.y).exp(),
            (-sigma_t.z).exp(),
        );
        assert!((tr - expect).abs().max_element() < 1e-5, "{tr} vs {expect}");
        // And again — the fast path is deterministic, zero variance.
        let tr2 = volumes.transmittance(&x_ray(), 1e-3, 10.0, &mut s);
        assert_eq!(tr, tr2);
    }

    #[test]
    fn ratio_tracking_matches_analytic_on_grid() {
        // A constant-valued grid is heterogeneous to the tracker but has a
        // known analytic transmittance.
        let d = 0.6f32;
        let volumes = Volumes::new(vec![unit_region(
            Vec3A::new(0.3, 0.5, 0.8),
            Vec3A::splat(0.4),
            0.0,
            DensityField::Grid {
                nx: 4,
                ny: 4,
                nz: 4,
                data: vec![d; 64],
            },
        )]);
        let mut s = RngSampler::default();
        let n = 20_000;
        let mut mean = Vec3A::ZERO;
        for _ in 0..n {
            mean += volumes.transmittance(&x_ray(), 1e-3, 10.0, &mut s);
        }
        mean /= n as f32;
        let e = (Vec3A::new(0.3, 0.5, 0.8) + Vec3A::splat(0.4)) * d;
        let expect = Vec3A::new((-e.x).exp(), (-e.y).exp(), (-e.z).exp());
        assert!(
            (mean - expect).abs().max_element() < 0.01,
            "{mean} vs {expect}"
        );
    }

    #[test]
    fn delta_tracking_scatter_probability_matches_analytic() {
        // Pure scattering (σa = 0), gray: P(scatter in slab) = 1 − e^{−σt·d}.
        let sigma = 1.3f32;
        let volumes = Volumes::new(vec![unit_region(
            Vec3A::splat(sigma),
            Vec3A::ZERO,
            0.0,
            DensityField::Homogeneous,
        )]);
        let mut s = RngSampler::default();
        let n = 20_000;
        let mut scatters = 0u32;
        for _ in 0..n {
            if let VolumeEvent::Scatter { weight, .. } =
                volumes.sample_interaction(&x_ray(), 1e-3, 10.0, &mut s)
            {
                scatters += 1;
                // Gray pure scattering: the event weight must be exactly 1.
                assert!((weight - Vec3A::ONE).abs().max_element() < 1e-5);
            }
        }
        let observed = scatters as f32 / n as f32;
        let expect = 1.0 - (-sigma).exp();
        assert!((observed - expect).abs() < 0.01, "{observed} vs {expect}");
    }

    #[test]
    fn emission_walk_matches_analytic_slab() {
        // Absorbing emissive slab: L = (σa/σt)·Le·(1 − e^{−σt·d}); here
        // σs = 0 so σa/σt = 1.
        let sigma_a = 0.8f32;
        let le = Vec3A::new(4.0, 1.5, 0.3);
        let region = VolumeRegion::new(
            Mat4::IDENTITY,
            Vec3A::splat(0.5),
            Vec3A::ZERO,
            Vec3A::splat(sigma_a),
            0.0,
            le,
            1.0,
            DensityField::Homogeneous,
        );
        let volumes = Volumes::new(vec![region]);
        let mut s = RngSampler::default();
        let n = 40_000;
        let mut mean = Vec3A::ZERO;
        for _ in 0..n {
            match volumes.sample_interaction(&x_ray(), 1e-3, 10.0, &mut s) {
                VolumeEvent::Passthrough { emitted, .. } => mean += emitted,
                VolumeEvent::Scatter { emitted, .. } => mean += emitted,
            }
        }
        mean /= n as f32;
        let expect = le * (1.0 - (-sigma_a).exp());
        assert!(
            ((mean - expect) / expect).abs().max_element() < 0.03,
            "{mean} vs {expect}"
        );
    }

    #[test]
    fn grid_trilinear_exact_at_centers() {
        let data: Vec<f32> = (0..8).map(|i| i as f32).collect();
        let field = DensityField::Grid {
            nx: 2,
            ny: 2,
            nz: 2,
            data: data.clone(),
        };
        // Voxel centers of a 2^3 grid sit at 0.25 and 0.75.
        for z in 0..2usize {
            for y in 0..2usize {
                for x in 0..2usize {
                    let u = Vec3A::new(
                        0.25 + 0.5 * x as f32,
                        0.25 + 0.5 * y as f32,
                        0.25 + 0.5 * z as f32,
                    );
                    let expect = data[x + 2 * (y + 2 * z)];
                    assert!((field.density(u) - expect).abs() < 1e-6);
                }
            }
        }
        // Box center is the mean of all 8 samples.
        let center = field.density(Vec3A::splat(0.5));
        assert!((center - 3.5).abs() < 1e-6);
    }

    #[test]
    fn noise_deterministic_and_bounded_by_majorant() {
        let field = DensityField::Noise {
            scale: 4.0,
            octaves: 4,
            gain: 0.5,
            lacunarity: 2.0,
            threshold: 0.3,
            seed: 42,
        };
        let m = field.max_value();
        let mut prev = Vec::new();
        for pass in 0..2 {
            let mut vals = Vec::new();
            for i in 0..1000 {
                let u = Vec3A::new(
                    hash3(i, 1, 2, 7),
                    hash3(i, 3, 4, 7),
                    hash3(i, 5, 6, 7),
                );
                let d = field.density(u);
                assert!(d >= 0.0 && d <= m, "density {d} outside [0, {m}]");
                vals.push(d);
            }
            if pass == 0 {
                prev = vals;
            } else {
                assert_eq!(prev, vals, "noise is not deterministic");
            }
        }
        // The field must not be trivially empty.
        assert!(prev.iter().any(|&d| d > 0.0));
    }

    #[test]
    fn oriented_box_interval_in_world_units() {
        // Box scaled ×2 in x, translated to x = 5: world slab [3, 7].
        let xf = Mat4::from_translation(Vec3::new(5.0, 0.0, 0.0))
            * Mat4::from_scale(Vec3::new(2.0, 1.0, 1.0));
        let region = VolumeRegion::new(
            xf,
            Vec3A::ONE,
            Vec3A::ONE,
            Vec3A::ZERO,
            0.0,
            Vec3A::ZERO,
            1.0,
            DensityField::Homogeneous,
        );
        let (t0, t1) = region.intersect(&x_ray()).expect("must hit");
        assert!((t0 - 5.0).abs() < 1e-4 && (t1 - 9.0).abs() < 1e-4, "{t0} {t1}");
        // Rotation: 45° about z, ray along x through origin-centered box.
        let rot = Mat4::from_rotation_z(std::f32::consts::FRAC_PI_4);
        let region = VolumeRegion::new(
            rot,
            Vec3A::ONE,
            Vec3A::ONE,
            Vec3A::ZERO,
            0.0,
            Vec3A::ZERO,
            1.0,
            DensityField::Homogeneous,
        );
        let (t0, t1) = region.intersect(&x_ray()).expect("must hit");
        // The rotated unit-half box spans ±√2 along x through its center.
        let s = 2.0f32.sqrt();
        assert!((t0 - (2.0 - s)).abs() < 1e-4 && (t1 - (2.0 + s)).abs() < 1e-4);
    }

    #[test]
    fn overlapping_regions_compose_exactly() {
        // Two overlapping homogeneous boxes: transmittance is the product.
        let a = unit_region(Vec3A::ZERO, Vec3A::splat(0.5), 0.0, DensityField::Homogeneous);
        let b = VolumeRegion::new(
            Mat4::from_translation(Vec3::new(0.25, 0.0, 0.0)),
            Vec3A::splat(0.5),
            Vec3A::ZERO,
            Vec3A::splat(0.75),
            0.0,
            Vec3A::ZERO,
            1.0,
            DensityField::Homogeneous,
        );
        let volumes = Volumes::new(vec![a, b]);
        let mut s = RngSampler::default();
        let tr = volumes.transmittance(&x_ray(), 1e-3, 10.0, &mut s);
        let expect = (-0.5f32 - 0.75).exp();
        assert!((tr.x - expect).abs() < 1e-5, "{tr} vs {expect}");
    }

    #[test]
    fn phase_mix_pdf_matches_single_lobe() {
        let mix = PhaseMix::single(0.4);
        for &mu in &[-0.9f32, -0.2, 0.0, 0.5, 0.99] {
            assert!((mix.pdf(mu) - hg_phase(mu, 0.4)).abs() < 1e-7);
        }
    }
}
