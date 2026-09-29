//! Random-walk subsurface scattering — MaterialX's `subsurface_bsdf`.
//!
//! A port of NVIDIA Typhoon's `ty::RandomWalkSSS` (hdEmbree,
//! `renderer/integrator/sss.cpp`), itself a port of Blender Cycles'
//! `subsurface_random_walk.h` (Apache 2.0). The walk is self-contained: once
//! the closure picks its subsurface leaf, the tracer hands the entry here, the
//! walk bounces through the object's interior against **its own geometry
//! only** (`geom_id`), and the path resumes at the exit point on a white
//! Lambertian ([`ExitLambertian`]) weighted by the walk's throughput. Nothing
//! inside the walk counts against the path depth, runs NEE or is recorded as a
//! vertex — the whole walk is one surface event, as in Typhoon and Cycles.
//!
//! The pieces, each in Typhoon's words and order:
//!
//! - **Chiang 2016 remap** ([`chiang_remap`]): the artist's `(color, radius,
//!   anisotropy)` become an extinction and a single-scattering albedo such that
//!   the walk reflects `color` off a semi-infinite slab. Albedos under 0.2 are
//!   clamped up for stability and the walk starts at `raw / 0.2` to compensate.
//! - **Channel MIS** (balance heuristic) per bounce: one channel's extinction
//!   drives the free flight, and the estimator divides by the
//!   throughput-and-albedo weighted mixture of all three, so a strongly
//!   chromatic radius (skin: red travels four times further than blue) does
//!   not blow up as the max-channel majorant of [`crate::Medium`] would.
//! - **Dwivedi guiding** ([`sample_phase_dwivedi`]): from the second bounce a
//!   fraction of directions is drawn around the entry normal with the
//!   zero-variance "stretched" extinction, pulling walks back to the surface
//!   they entered; with a known opposite interface a backward Dwivedi lobe
//!   pulls toward it. The first ray is extended to find that interface.
//! - **Similarity relation** after 9 bounces: isotropic scattering with the
//!   reduced `σₛ(1 − g)`.
//!
//! What this is *not*: a BSSRDF. There is no NEE at the entry (the leaf's
//! `eval` is zero, as Typhoon's is), the exit is a Lambertian with no Fresnel
//! (the Chiang fit assumes exactly that), and a walk that finds no exit within
//! 256 bounces — an open mesh, a sliver — is absorbed.

use glam::Vec3A;
use std::f32::consts::{FRAC_1_PI, PI};
use utils::cosine_hemisphere;

use crate::PathSampler;
use crate::hittable::HitRecord;
use crate::material::brdf::tangent_frame;
use crate::material::{Material, ScatterSample};
use crate::medium::{hg_phase, sample_henyey_greenstein};
use crate::ray::{MASK_ALL, Ray};
use crate::rt_world::World;

/// Walk steps before a walk is given up as absorbed (Typhoon, Cycles).
const MAX_BOUNCES: u32 = 256;
/// Bounces after which the similarity relation replaces the medium.
const SIMILARITY_LEVEL: u32 = 9;
/// Guided fraction in the similarity regime, where no HG peak competes.
const REDUCED_GUIDED_FRACTION: f32 = 0.75;
/// Albedo floor of the Chiang remap; the throughput starts at `raw / MIN`.
const MIN_ALPHA: f32 = 0.2;
/// The walk's self-intersection offset — Typhoon's `_kBias`, and the offset
/// the closure already puts on a refracted ray.
const BIAS: f32 = 1e-4;
const EXTINCTION_EPS: f32 = 1e-6;
const THROUGHPUT_EPS: f32 = 1e-6;
/// In-walk Russian roulette: once every channel's throughput is below this,
/// a scatter survives with probability `peak / RR_THRESHOLD` (never under
/// `RR_MIN_PROB`) and is reweighted on survival. Unbiased; it spares the
/// steps a walk spends carrying almost nothing — 10–15% of a skin walk's.
const RR_THRESHOLD: f32 = 0.05;
const RR_MIN_PROB: f32 = 0.05;

/// What a subsurface leaf hands the tracer when the closure selects it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SubsurfaceEntry {
    /// World-space direction into the medium from the entry point.
    pub dir: Vec3A,
    /// The target multiple-scattering albedo (the leaf's `color`).
    pub albedo: Vec3A,
    /// Per-channel mean free path, world units (`radius`).
    pub radius: Vec3A,
    /// Henyey–Greenstein anisotropy; the walk clamps it to `[0, 0.99]`
    /// ([`walk_anisotropy`]).
    pub anisotropy: f32,
}

/// Where a successful walk left the object.
#[derive(Clone, Copy)]
pub struct WalkExit {
    /// The exit hit, as seen from **outside**: `normal` points out of the
    /// object and `front_face` is true, so the exit vertex shades it like any
    /// surface a ray arrived at from the outside.
    pub rec: HitRecord,
    /// The walk's last direction, interior to exterior.
    pub dir: Vec3A,
    /// The walk's throughput, per channel — what the path is multiplied by.
    pub weight: Vec3A,
}

/// Work a walk did, for `--stats`.
#[derive(Clone, Copy, Debug, Default)]
pub struct WalkCost {
    /// Loop iterations (free flights).
    pub steps: u32,
    /// Closest-hit queries, foreign surfaces stepped past included.
    pub rays: u32,
}

/// Chiang et al. 2016's albedo inversion for one channel (Cycles'
/// `subsurface_random_walk_remap`): `(extinction, raw single-scattering
/// albedo)`. The extinction is `1 / (radius · (1 − g))`.
fn chiang_remap_channel(albedo: f32, radius: f32, g: f32) -> (f32, f32) {
    let g2 = g * g;
    let g3 = g2 * g;
    let g4 = g3 * g;
    let g5 = g4 * g;
    let g6 = g5 * g;
    let g7 = g6 * g;
    let a = 1.826_052_4
        + -1.284_510_6 * g
        + -1.799_046_3 * g2
        + 9.193_933 * g3
        + -22.821_558 * g4
        + 32.023_487 * g5
        + -23.626_48 * g6
        + 7.210_67 * g7;
    let b = 4.985_112
        + 0.127_355_96
            * (31.149_158 * g
                + -201.847_02 * g2
                + 841.576 * g3
                + -2_018.092_9 * g4
                + 2_731.715_6 * g5
                + -1_935.414_2 * g6
                + 559.009 * g7)
                .exp();
    let c = 1.096_861
        + -0.394_704_06 * g
        + 1.052_581_2 * g2
        + -8.839_637 * g3
        + 28.864_323 * g4
        + -46.880_29 * g5
        + 38.540_283 * g6
        + -12.718_104 * g7;
    let d = 0.496_310_2
        + 0.360_146_58 * g
        + -2.151_393 * g2
        + 17.889_69 * g3
        + -55.298_4 * g4
        + 82.065_98 * g5
        + -58.510_6 * g6
        + 15.847_83 * g7;
    let e = 4.231_903
        + 0.003_106_039_5
            * (76.731_63 * g
                + -594.356_8 * g2
                + 2_448.883_4 * g3
                + -5_576.685 * g4
                + 7_116.602 * g5
                + -4_763.545 * g6
                + 1_303.531_8 * g7)
                .exp();
    let f = 2.406_03
        + -2.518_148_4 * g
        + 9.184_949 * g2
        + -79.219_17 * g3
        + 259.082_87 * g4
        + -403.613_8 * g5
        + 302.857_12 * g6
        + -87.437_05 * g7;
    let blend = albedo.powf(0.25);
    let alpha =
        (1.0 - blend) * a * (b * albedo).atan().powf(c) + blend * d * (e * albedo).atan().powf(f);
    let alpha = alpha.clamp(0.0, 0.999_999);
    let extinction = 1.0 / radius.max(1e-16) / (1.0 - g).max(1e-6);
    (extinction, alpha)
}

/// The anisotropy a walk runs with. Chiang's polynomials are fitted over
/// `g ∈ [0, 1)` only: at `g = −0.4` the `d` coefficient is already −3.7 and
/// the remapped albedo is negative, so every walk dies. Typhoon clamps to
/// ±0.99 and inherits that; Cycles clamps its subsurface anisotropy to
/// non-negative values, and so does this.
pub fn walk_anisotropy(g: f32) -> f32 {
    if g.is_finite() {
        g.clamp(0.0, 0.99)
    } else {
        0.0
    }
}

/// Chiang 2016 remap per channel: `(extinction, alpha, raw_alpha)`, with
/// `alpha` floored at 0.2 (Typhoon's `ty::ChiangRemap`).
pub fn chiang_remap(albedo: Vec3A, radius: Vec3A, anisotropy: f32) -> (Vec3A, Vec3A, Vec3A) {
    let g = walk_anisotropy(anisotropy);
    let mut ext = [0.0; 3];
    let mut raw = [0.0; 3];
    for c in 0..3 {
        (ext[c], raw[c]) = chiang_remap_channel(albedo[c].clamp(0.0, 1.0), radius[c].max(1e-16), g);
    }
    let raw = Vec3A::from(raw);
    (Vec3A::from(ext), raw.max(Vec3A::splat(MIN_ALPHA)), raw)
}

/// Dwivedi's diffusion length `ν = 1/√(1 − α^k)` with the exponent fit of
/// d'Eon & Křivánek 2020, eq. 67.
pub fn diffusion_length_dwivedi(alpha: f32) -> f32 {
    let alpha = alpha.clamp(1e-6, 0.999_999);
    let denom = 1.0 - alpha.powf(2.442_94 - 0.021_581_3 * alpha + 0.578_637 / alpha);
    1.0 / denom.max(1e-12).sqrt()
}

/// The Dwivedi phase function over `cos θ` (Meng et al. 2016, eq. 9), with
/// `phase_log = ln((ν + 1)/(ν − 1))`. Normalised over `cos θ ∈ [−1, 1]`;
/// the azimuth is uniform.
pub fn eval_phase_dwivedi(nu: f32, phase_log: f32, cos_theta: f32) -> f32 {
    1.0 / ((nu - cos_theta).max(1e-6) * phase_log)
}

/// Inverse CDF of [`eval_phase_dwivedi`] (Meng et al. 2016, eq. 10).
pub fn sample_phase_dwivedi(nu: f32, phase_log: f32, u: f32) -> f32 {
    nu - (nu + 1.0) * (-u.clamp(0.0, 1.0) * phase_log).exp()
}

/// Probability of the backward Dwivedi lobe at a depth `from_entry` below
/// the entry plane, for an opposite interface `opposite` away.
pub fn backward_dwivedi_fraction(opposite: f32, from_entry: f32, nu: f32) -> f32 {
    if opposite <= 0.0 || nu <= 0.0 {
        return 0.0;
    }
    let d = from_entry.clamp(0.0, opposite);
    1.0 / (1.0 + ((opposite - 2.0 * d) / nu).exp())
}

fn exp3(v: Vec3A) -> Vec3A {
    Vec3A::new(v.x.exp(), v.y.exp(), v.z.exp())
}

/// The interval every other `World::intersect` in the renderer asks for.
///
/// The walk asks for the same one and moves the ray's origin instead of its
/// bounds. With every caller passing `(0.001, ∞)`, LLVM propagates both
/// constants into the kernel; one caller passing variables was enough to
/// lose that, and cornellbox — which never walks — ran 0.3% more
/// instructions in the triangle test.
const TRACE_T_MIN: f32 = 0.001;

/// Closest hit on `owner` from `pos` along the unit `dir` within
/// `(t_min, t_max)`, stepping past every other geometry. The record's `t`
/// is measured from `pos`.
///
/// Typhoon traces the owner's prototype scene alone; crust's world is one
/// BVH, so a segment steps past foreign surfaces instead, as many as lie in
/// the segment. There is deliberately no cap: a capped search read "too many
/// foreign surfaces" as "no boundary", and a walk past nine spheres embedded in
/// its object carried on outside it. It terminates because `t_max` is finite
/// and every step advances by at least `BIAS` (relative, at large `t`).
#[allow(clippy::too_many_arguments)]
fn trace_owner(
    world: &World,
    owner: u32,
    pos: Vec3A,
    dir: Vec3A,
    mut t_min: f32,
    t_max: f32,
    time: f32,
    cost: &mut WalkCost,
) -> Option<HitRecord> {
    if !t_max.is_finite() {
        return None;
    }
    while t_min < t_max {
        cost.rays += 1;
        let shift = t_min - TRACE_T_MIN;
        let ray = Ray::new(pos + dir * shift, dir)
            .with_time(time)
            .with_mask(MASK_ALL);
        let hit = world.intersect(&ray, TRACE_T_MIN, f32::INFINITY)?;
        let t = hit.rec.t + shift;
        if t >= t_max {
            return None;
        }
        if hit.geom_id == owner {
            return Some(HitRecord { t, ..hit.rec });
        }
        // Past this surface; relative at large `t`, where `BIAS` is below
        // an ulp and would not move the search.
        t_min = t + BIAS.max(t.abs() * 1e-6);
    }
    None
}

/// One random walk from `entry_p` on geometry `owner`, whose outward
/// (viewer-side) surface normal there is `guide`, entered through a face of
/// facing `entry_front`. `None` when the walk is absorbed or finds no exit.
/// Draws come from `sampler`; the first bounce is stratified, the rest
/// incidental.
#[allow(clippy::too_many_arguments)]
pub fn random_walk(
    world: &World,
    owner: u32,
    entry_p: Vec3A,
    guide: Vec3A,
    entry_front: bool,
    entry: &SubsurfaceEntry,
    time: f32,
    sampler: PathSampler,
    cost: &mut WalkCost,
) -> Option<WalkExit> {
    let g = walk_anisotropy(entry.anisotropy);
    let (extinction, alpha, raw) = chiang_remap(entry.albedo, entry.radius, g);
    let scattering = extinction * alpha;
    // Min-alpha correction: the clamp raised the albedo, so the walk starts
    // lower by the same ratio.
    let mut throughput = Vec3A::select(
        raw.cmplt(Vec3A::splat(MIN_ALPHA)),
        raw / MIN_ALPHA,
        Vec3A::ONE,
    );

    let nu = diffusion_length_dwivedi(alpha.max_element());
    if nu <= 1.0 + 1e-6 {
        return None;
    }
    let phase_log = ((nu + 1.0) / (nu - 1.0)).ln();
    let guided_fraction = 1.0 - g.abs().powf(0.125).max(0.5);
    let scattering_star = scattering * (1.0 - g);
    let extinction_star = extinction - scattering + scattering_star;
    let (gx, gy) = tangent_frame(guide);

    let mut pos = entry_p;
    let mut dir = entry.dir.normalize();
    let mut opposite: Option<f32> = None;

    for bounce in 0..MAX_BOUNCES {
        cost.steps += 1;
        let d = sampler.new_domain(bounce as i32);
        // [channel, guide coin, backward coin, free flight] and the direction.
        let (u, uv) = if bounce == 0 {
            (d.draw_sample_f32::<4>(), [0.0; 2])
        } else {
            (d.draw_rnd_f32::<4>(), d.new_domain(1).draw_rnd_f32::<2>())
        };
        let (ext_eff, sca_eff, g_eff, guided_eff) = if bounce <= SIMILARITY_LEVEL {
            (extinction, scattering, g, guided_fraction)
        } else {
            (
                extinction_star,
                scattering_star,
                0.0,
                REDUCED_GUIDED_FRACTION,
            )
        };

        // Channel MIS: pick a channel in proportion to throughput · albedo.
        let w = throughput * alpha;
        let sum = w.x + w.y + w.z;
        let channel_pdf = if sum > 0.0 && sum.is_finite() {
            w / sum
        } else {
            Vec3A::splat(1.0 / 3.0)
        };
        let channel = if u[0] < channel_pdf.x {
            0
        } else if u[0] < channel_pdf.x + channel_pdf.y {
            1
        } else {
            2
        };
        let mut sample_ext = ext_eff[channel];
        if sample_ext <= EXTINCTION_EPS {
            return None;
        }

        // Direction: the entry direction first, then HG or Dwivedi.
        let mut stretch_fwd = 1.0;
        let mut stretch_bwd = 1.0;
        let mut pdf_factor_fwd = 0.0;
        let mut pdf_factor_bwd = 0.0;
        let mut backward_fraction = 0.0;
        if bounce > 0 {
            let guided = u[1] < guided_eff;
            let mut backward = false;
            if let Some(opp) = opposite {
                backward_fraction = backward_dwivedi_fraction(opp, (pos - entry_p).dot(-guide), nu);
                backward = guided && u[2] < backward_fraction;
            }
            let new_dir = if guided {
                let mut cos = sample_phase_dwivedi(nu, phase_log, uv[0]);
                if backward {
                    cos = -cos;
                }
                let sin = (1.0 - cos * cos).max(0.0).sqrt();
                let phi = 2.0 * PI * uv[1];
                (gx * (sin * phi.cos()) + gy * (sin * phi.sin()) + guide * cos).normalize_or(guide)
            } else {
                sample_henyey_greenstein(dir, g_eff, uv[0], uv[1])
            };
            let cos_entry = new_dir.dot(guide);
            let p_hg = hg_phase(dir.dot(new_dir), g_eff).max(1e-8);
            let inv_2pi = 0.5 * FRAC_1_PI;
            pdf_factor_fwd = inv_2pi * eval_phase_dwivedi(nu, phase_log, cos_entry) / p_hg;
            pdf_factor_bwd = inv_2pi * eval_phase_dwivedi(nu, phase_log, -cos_entry) / p_hg;
            stretch_fwd = 1.0 - cos_entry / nu;
            stretch_bwd = 1.0 + cos_entry / nu;
            if guided {
                sample_ext *= if backward { stretch_bwd } else { stretch_fwd };
            }
            dir = new_dir;
        }

        // Free flight for the (possibly stretched) channel extinction.
        let uf = u[3].clamp(1e-6, 1.0 - 1e-6);
        let t_free = -(1.0 - uf).ln() / sample_ext.max(EXTINCTION_EPS);
        // The first ray reaches past its free flight, to find the far side.
        let t_max = if bounce == 0 {
            let min_ext = [ext_eff.x, ext_eff.y, ext_eff.z]
                .into_iter()
                .filter(|&e| e > EXTINCTION_EPS)
                .fold(f32::MAX, f32::min);
            if min_ext < f32::MAX {
                t_free.max(10.0 / min_ext)
            } else {
                t_free
            }
        } else {
            t_free
        };
        // Only the entry point lies on a surface. A scatter point is inside
        // the volume, and offsetting its ray too would step it straight
        // through a boundary it sits within `BIAS` of — the walk would then
        // continue outside the object and come back in through a front face.
        // Typhoon offsets every segment; that leak is why this does not.
        let t_min = if bounce == 0 { BIAS } else { 0.0 };
        let found = trace_owner(world, owner, pos, dir, t_min, t_max, time, cost);
        if bounce == 0
            && let Some(h) = &found
        {
            let depth = (h.p - entry_p).dot(-guide);
            if depth > BIAS {
                opposite = Some(depth);
            }
        }
        // From inside, the boundary is met from the side opposite the one the
        // walk came in through. Meeting it from the entry's side means the
        // walk is outside: a scatter point rounded across the surface, then
        // came back in. That walk is lost, not exited through its way in.
        if found
            .as_ref()
            .is_some_and(|h| h.front_face == entry_front && h.t < t_free)
        {
            return None;
        }
        let exit = found.filter(|h| h.t < t_free);
        let t = exit.as_ref().map_or(t_free, |h| h.t);

        // Classic pdf and contribution: a surface hit pays T(t) against T(t),
        // a scatter σₛ·T(t) against σₜ·T(t).
        let tr = exp3(-ext_eff * t);
        let (classic_pdf, contrib) = if exit.is_some() {
            (tr, tr)
        } else {
            (ext_eff * tr, sca_eff * tr)
        };
        let mut pdf = classic_pdf;
        if bounce > 0 {
            // The stretched transmittances. The forward one is its own
            // exponential; the backward one follows from
            // `exp(−σ(1 + c/ν)t) = exp(−σt)² / exp(−σ(1 − c/ν)t)`, which
            // spares three exponentials a step (of nine). The quotient is
            // used while both terms are comfortably normal floats; a flight
            // long enough for `tr²` to underflow takes the exponential.
            let tr_fwd = exp3(-ext_eff * stretch_fwd * t);
            let lobe = |stretch: f32, tr_s: Vec3A| {
                if exit.is_some() {
                    tr_s
                } else {
                    ext_eff * stretch * tr_s
                }
            };
            let fwd = lobe(stretch_fwd, tr_fwd) * pdf_factor_fwd;
            let guided_pdf = if opposite.is_some() {
                let tr_bwd = if tr.min_element() > 1e-18 && tr_fwd.min_element() > 1e-18 {
                    tr * tr / tr_fwd
                } else {
                    exp3(-ext_eff * stretch_bwd * t)
                };
                fwd * (1.0 - backward_fraction)
                    + lobe(stretch_bwd, tr_bwd) * pdf_factor_bwd * backward_fraction
            } else {
                fwd
            };
            pdf = classic_pdf * (1.0 - guided_eff) + guided_pdf * guided_eff;
        }
        let denom = channel_pdf.dot(pdf);
        if !denom.is_finite() || denom <= 1e-20 {
            return None;
        }
        throughput *= contrib / denom;
        let peak = throughput.max_element();
        if !peak.is_finite() || peak < THROUGHPUT_EPS {
            return None;
        }
        // Roulette on a continuing walk whose every channel has gone dim.
        if exit.is_none() && peak < RR_THRESHOLD {
            let p = (peak / RR_THRESHOLD).max(RR_MIN_PROB);
            if d.new_domain(2).draw_rnd_f32::<1>()[0] >= p {
                return None;
            }
            throughput /= p;
        }

        if let Some(h) = exit {
            // Seen from inside, the record faces the walk; the exit vertex
            // sees it from outside.
            let rec = HitRecord {
                normal: -h.normal,
                front_face: true,
                ..h
            };
            return Some(WalkExit {
                rec,
                dir,
                weight: throughput,
            });
        }
        pos += dir * t;
    }
    None
}

/// The white Lambertian a walk exits through — Typhoon's (and Cycles')
/// synthetic exit closure, replacing the surface's own material at the exit
/// so the walk's albedo is not applied twice. It emits nothing: light the
/// surface emits there travels outward, not into the walk.
pub struct ExitLambertian;

impl Material for ExitLambertian {
    fn kind(&self) -> &'static str {
        "subsurface_exit"
    }

    fn scatter_importance(
        &self,
        _r_in: &Ray,
        rec: &HitRecord,
        sampler: PathSampler,
    ) -> Option<ScatterSample> {
        let (t, b) = tangent_frame(rec.normal);
        let l = cosine_hemisphere(sampler.draw_sample_f32::<2>());
        let wi = (t * l.x + b * l.y + rec.normal * l.z).normalize();
        let cos = l.z.max(0.0);
        if cos <= 0.0 {
            return None;
        }
        Some(ScatterSample {
            ray: Ray::new(rec.p, wi),
            value: Vec3A::splat(cos * FRAC_1_PI),
            pdf: cos * FRAC_1_PI,
            delta: false,
            spread: crate::RayCone::MAX_SPREAD,
            subsurface: None,
        })
    }

    fn eval(&self, _r_in: &Ray, rec: &HitRecord, wi: Vec3A) -> Option<(Vec3A, f32)> {
        let cos = rec.normal.dot(wi.normalize());
        if cos <= 0.0 {
            return Some((Vec3A::ZERO, 1e-4));
        }
        Some((Vec3A::splat(cos * FRAC_1_PI), (cos * FRAC_1_PI).max(1e-4)))
    }
}

#[cfg(test)]
mod tests;
