//! BSDL's MaterialX lobes, ported from OSL's `src/libbsdl` (`BSDL/MTX/`,
//! `microfacet_tools_impl.h`; BSD-3-Clause, Copyright Contributors to the
//! Open Shading Language project — see `THIRD-PARTY.md`) at the commit
//! `scripts/tables/bsdl_luts_to_rust.py` pins.
//!
//! MaterialX's OSL backend renders its BSDF nodes as these lobes in OSL's
//! `testrender`, so they are the reference crust's leaves are checked
//! against (`tests/bsdl_oracle.rs`). Names follow BSDL's, so each function
//! can be read against its source line by line. Two conventions differ:
//! a value here is the BSDF `f` without the cosine (crust's `eval_lobe`
//! multiplies it in), where BSDL returns `f·|cos θi|` as a sample weight times
//! its pdf; and sampling is crust's own, so nothing here samples.
//!
//! What is BSDL's: the GGX distribution and its roughness / anisotropy
//! parameterisation, the tabulated energy curves (`bsdl_tables`), Turquin's
//! multiple-scattering compensation on reflection-only lobes, the
//! renormalisation of refracting ones, the dielectric, conductor and Schlick
//! Fresnel models, the sheen and diffuse lobes and every lobe's layering
//! filter. What is not: the Airy thin film and the F82 tint (BSDL's lobes
//! declare neither; `mx` keeps MaterialX's GLSL for both).

use glam::Vec3A;
use std::f32::consts::{FRAC_1_PI, PI};

use super::bsdl_tables::{
    DIELECTRIC_BOTH_BACK, DIELECTRIC_BOTH_FRONT, DIELECTRIC_REFL_FRONT, GGX as GGX_TABLE, NC, NF,
    NR,
};

/// BSDL's `EPSILON`: the roughness floor its lobes clamp MaterialX's alphas to.
pub const EPSILON: f32 = 1e-4;
/// BSDL's `FLOAT_MIN`, the smallest normal `f32`.
const FLOAT_MIN: f32 = f32::MIN_POSITIVE;
/// BSDL's `BIG`.
const BIG: f32 = 1e12;

fn sq(x: f32) -> f32 {
    x * x
}

/// BSDL's `LERP`: `(1 − f)·a + f·b`, with `f` clamped to [0, 1].
fn lerp(f: f32, a: f32, b: f32) -> f32 {
    let f = f.clamp(0.0, 1.0);
    (1.0 - f) * a + f * b
}

/// BSDL's default `Fast::powf`: 0 for a base below the smallest normal.
fn powf(x: f32, y: f32) -> f32 {
    if x < FLOAT_MIN { 0.0 } else { x.powf(y) }
}

// ---------------------------------------------------------------------------
// GGX
// ---------------------------------------------------------------------------

/// `GGXDist`: an anisotropic GGX distribution, built from BSDL's
/// `(roughness, anisotropy)` pair.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GGXDist {
    pub ax: f32,
    pub ay: f32,
}

impl GGXDist {
    /// `GGXDist(rough, aniso, flip_aniso)`: `α = rough²`, stretched by
    /// `1 ± aniso` along x and y (swapped when `flip`), floored at 1e-5.
    pub fn new(rough: f32, aniso: f32, flip: bool) -> GGXDist {
        const ALPHA_MIN: f32 = 1e-5;
        let aniso = if flip { -aniso } else { aniso };
        let a = sq(rough);
        GGXDist {
            ax: (a * (1.0 + aniso)).max(ALPHA_MIN),
            ay: (a * (1.0 - aniso)).max(ALPHA_MIN),
        }
    }

    /// What every BSDL MaterialX microfacet lobe does with the authored
    /// alphas `(rx, ry)` (each already floored at [`EPSILON`]): it re-expresses
    /// them as `(roughness, anisotropy)` with `roughness = √(α_max / (1 +
    /// aniso))`, the index its energy tables are read at, and builds the
    /// distribution from that pair. Returns the distribution and the roughness.
    pub fn from_alphas(rx: f32, ry: f32) -> (GGXDist, f32) {
        let ax = rx.max(ry);
        let ay = rx.min(ry);
        let b = ay / ax;
        let aniso = (1.0 - b) / (1.0 + b);
        let roughness = (ax / (1.0 + aniso)).sqrt();
        (GGXDist::new(roughness, aniso, rx < ry), roughness)
    }

    /// `D(Hr)`.
    pub fn d(&self, h: Vec3A) -> f32 {
        let cos_phi2st2 = sq(h.x / self.ax);
        let sin_phi2st2 = sq(h.y / self.ay);
        let cos_theta_m2 = sq(h.z);
        let sin_theta_m2 = cos_phi2st2 + sin_phi2st2;
        1.0 / (PI * self.ax * self.ay * sq(cos_theta_m2 + sin_theta_m2))
    }

    /// `G1(w)`, for `w` above the plane.
    pub fn g1(&self, w: Vec3A) -> f32 {
        let w = Vec3A::new(w.x * self.ax, w.y * self.ay, w.z);
        2.0 * w.z / (w.z + w.length())
    }

    /// `G2_G1(wi, wo)`: the height-correlated `G2` over `G1(wo)`, both
    /// directions above the plane.
    pub fn g2_g1(&self, wi: Vec3A, wo: Vec3A) -> f32 {
        let wi = Vec3A::new(wi.x * self.ax, wi.y * self.ay, wi.z);
        let wo = Vec3A::new(wo.x * self.ax, wo.y * self.ay, wo.z);
        let nl = wi.length();
        let nv = wo.length();
        wi.z * (wo.z + nv) / (wo.z * nl + wi.z * nv)
    }

    /// The height-correlated `G2(wi, wo)`, `G2_G1 · G1` as BSDL multiplies it.
    pub fn g2(&self, wi: Vec3A, wo: Vec3A) -> f32 {
        self.g2_g1(wi, wo) * self.g1(wo)
    }

    /// `sample_for_refl`: Eto and Tokuyoshi's bounded VNDF sampling
    /// ("Bounded VNDF Sampling for Smith–GGX Reflections", listing 1), a
    /// micro-normal drawn from the spherical cap of visible normals that
    /// can reflect `wo` above the plane. Its density is
    /// [`GGXDist::d_refl_d`]` · D(m)`, per reflected solid angle over
    /// `4 cosθo`. BSDL draws refraction from the same cap.
    pub fn sample_for_refl(&self, wo: Vec3A, randu: f32, randv: f32) -> Vec3A {
        let i_std = Vec3A::new(wo.x * self.ax, wo.y * self.ay, wo.z).normalize();
        let phi = 2.0 * PI * randu;
        let a = self.ax.min(self.ay).clamp(0.0, 1.0); // Eq. 6
        let s = 1.0 + (sq(wo.x) + sq(wo.y)).sqrt(); // Omit sgn for a <= 1
        let a2 = sq(a);
        let s2 = sq(s);
        let k = (1.0 - a2) * s2 / (s2 + a2 * sq(wo.z)); // Eq. 5
        let b = k * i_std.z;
        let z = (1.0 - randv) * (1.0 + b) - b;
        let sin_theta = (1.0 - sq(z)).clamp(0.0, 1.0).sqrt();
        let o_std = Vec3A::new(sin_theta * phi.cos(), sin_theta * phi.sin(), z);
        let m_std = i_std + o_std;
        Vec3A::new(m_std.x * self.ax, m_std.y * self.ay, m_std.z).normalize()
    }

    /// `D_refl_D(wo, m)`: the bounded VNDF density over `D(m)` (listing 2,
    /// eq. 8), which depends on `wo` alone.
    pub fn d_refl_d(&self, wo: Vec3A) -> f32 {
        let len2 = sq(wo.x * self.ax) + sq(wo.y * self.ay);
        let t = (len2 + sq(wo.z)).sqrt();
        let a = self.ax.min(self.ay).clamp(0.0, 1.0); // Eq. 6
        let s = 1.0 + (sq(wo.x) + sq(wo.y)).sqrt(); // Omit sgn for a <= 1
        let a2 = sq(a);
        let s2 = sq(s);
        let k = (1.0 - a2) * s2 / (s2 + a2 * sq(wo.z)); // Eq. 5
        2.0 * wo.z / (k * wo.z + t) // Eq. 8
    }
}

/// BSDL's `reflect(E, N)`.
pub fn reflect(e: Vec3A, n: Vec3A) -> Vec3A {
    n * (2.0 * n.dot(e)) - e
}

/// BSDL's `refract(E, N, eta)`: `E` refracted through the plane of normal
/// `N` into a medium of relative IOR `eta`, or zero under total internal
/// reflection.
pub fn refract(e: Vec3A, n: Vec3A, eta: f32) -> Vec3A {
    if eta == 0.0 {
        return Vec3A::ZERO;
    }
    let mut cosi = e.dot(n);
    let (neta, nn) = if cosi > 0.0 {
        (1.0 / eta, n)
    } else {
        cosi = -cosi;
        (eta, -n)
    };
    let arg = 1.0 - neta * neta * (1.0 - cosi * cosi);
    if arg < 0.0 {
        return Vec3A::ZERO;
    }
    let nk = neta * cosi - arg.sqrt();
    (e * -neta + nn * nk).normalize()
}

/// `DielectricBSDF::reflection_probability`: the RGB mean of `F`.
pub fn reflection_probability(f: Vec3A) -> f32 {
    (f.x + f.y + f.z) / 3.0
}

// ---------------------------------------------------------------------------
// Tabulated energy
// ---------------------------------------------------------------------------

/// One of BSDL's baked energy tables and how its cosines are spaced.
#[derive(Clone, Copy, Debug)]
pub struct Table {
    data: &'static [f32],
    nf: usize,
    /// `MiniMicrofacet`'s `(i/15)²` spacing rather than the dielectric's `i/15`.
    squared: bool,
}

/// `spi::MiniMicrofacetGGX`: white single-scattering GGX.
pub const GGX: Table = Table {
    data: &GGX_TABLE,
    nf: 1,
    squared: true,
};
/// `mtx::DielectricReflFront`: a dielectric that only reflects.
pub const REFL_FRONT: Table = Table {
    data: &DIELECTRIC_REFL_FRONT,
    nf: NF,
    squared: false,
};
/// `mtx::DielectricBothFront`: a dielectric that reflects and refracts, from outside.
pub const BOTH_FRONT: Table = Table {
    data: &DIELECTRIC_BOTH_FRONT,
    nf: NF,
    squared: false,
};
/// `mtx::DielectricBothBack`: the same, from inside.
pub const BOTH_BACK: Table = Table {
    data: &DIELECTRIC_BOTH_BACK,
    nf: NF,
    squared: false,
};

impl Table {
    /// `get_cosine(i)`.
    fn cosine(&self, i: usize) -> f32 {
        let c = i as f32 * (1.0 / (NC - 1) as f32);
        (if self.squared { sq(c) } else { c }).max(1e-6)
    }
}

/// `TabulatedEnergyCurve`: a table read at one roughness and Fresnel index.
///
/// The four rows the bilinear interpolation reads and its two weights are
/// found once, when the curve is made, rather than per cosine as BSDL's
/// `interpolate_emiss` recomputes them; the arithmetic per value is BSDL's.
#[derive(Clone, Copy, Debug)]
pub struct TabulatedEnergyCurve {
    table: Table,
    /// Row offsets `(fa, ra)`, `(fa, rb)`, `(fb, ra)`, `(fb, rb)`.
    rows: [usize; 4],
    rf: f32,
    ff: f32,
}

impl TabulatedEnergyCurve {
    pub fn new(table: Table, roughness: f32, fresnel_index: f32) -> Self {
        let rf = roughness.clamp(0.0, 1.0) * (NR - 1) as f32;
        let ra = rf as usize;
        let rb = (ra + 1).min(NR - 1);
        let rf = rf - ra as f32;
        let (fa, fb, ff) = if table.nf == 1 {
            (0, 0, 0.0)
        } else {
            let ff = fresnel_index.clamp(0.0, 1.0) * (table.nf - 1) as f32;
            let fa = ff as usize;
            (fa, (fa + 1).min(table.nf - 1), ff - fa as f32)
        };
        let row = |f: usize, r: usize| (f * NR + r) * NC;
        TabulatedEnergyCurve {
            table,
            rows: [row(fa, ra), row(fa, rb), row(fb, ra), row(fb, rb)],
            rf,
            ff,
        }
    }

    /// `interpolate_emiss(i)`: bilinear in (Fresnel index, roughness) at
    /// cosine `i`.
    fn interpolate_emiss(&self, i: usize) -> f32 {
        let t = self.table.data;
        let [aa, ab, ba, bb] = self.rows;
        let a = lerp(self.rf, t[aa + i], t[ab + i]);
        if self.table.nf == 1 {
            return a;
        }
        lerp(self.ff, a, lerp(self.rf, t[ba + i], t[bb + i]))
    }

    /// `Emiss_eval(c)`: piecewise linear in the cosine. The bin is found
    /// from the spacing (`c·15`, or `√c·15` for squared cosines) and then
    /// nudged against the stored cosines, so it is the bin BSDL's scan finds.
    pub fn emiss_eval(&self, c: f32) -> f32 {
        let cos = |i: usize| self.table.cosine(i);
        if c <= cos(0) {
            return self.interpolate_emiss(0);
        }
        let x = if self.table.squared { c.sqrt() } else { c };
        // The first `i ≥ 1` with `c < cos(i)`, as BSDL's loop picks it.
        let mut i = ((x * (NC - 1) as f32) as usize + 1).clamp(1, NC);
        while i > 1 && c < cos(i - 1) {
            i -= 1;
        }
        while i < NC && c >= cos(i) {
            i += 1;
        }
        if i == NC {
            return self.interpolate_emiss(NC - 1);
        }
        let (cos0, cos1) = (cos(i - 1), cos(i));
        let q = (c - cos0) / (cos1 - cos0);
        lerp(q, self.interpolate_emiss(i - 1), self.interpolate_emiss(i))
    }
}

// ---------------------------------------------------------------------------
// Fresnel
// ---------------------------------------------------------------------------

/// `DielectricFresnel`'s IOR range.
pub const IOR_MIN: f32 = 1.001;
pub const IOR_MAX: f32 = 5.0;

/// `DielectricFresnel(eta, backside)`: the relative IOR across the
/// interface as the ray meets it, inverted on the back side and clamped into
/// the tables' range.
pub fn dielectric_eta(eta: f32, backside: bool) -> f32 {
    let eta = if backside { 1.0 / eta } else { eta };
    if eta >= 1.0 {
        eta.clamp(IOR_MIN, IOR_MAX)
    } else {
        eta.clamp(1.0 / IOR_MAX, 1.0 / IOR_MIN)
    }
}

/// `DielectricFresnel::eval`: 1 under total internal reflection.
pub fn dielectric_fresnel(c: f32, eta: f32) -> f32 {
    let g = (eta - 1.0) * (eta + 1.0) + c * c;
    if g > 0.0 {
        let g = g.sqrt();
        let a = (g - c) / (g + c);
        let b = (c * (g + c) - 1.0) / (c * (g - c) + 1.0);
        return 0.5 * a * a * (1.0 + b * b);
    }
    1.0
}

/// `DielectricFresnel::table_index`: `√((η − 1.001)/(5 − 1.001))`, the same
/// for `η` and `1/η`.
pub fn table_index(eta: f32) -> f32 {
    let seta = (if eta < 1.0 { 1.0 / eta } else { eta }).clamp(IOR_MIN, IOR_MAX);
    ((seta - IOR_MIN) * (1.0 / (IOR_MAX - IOR_MIN))).sqrt()
}

/// `ConductorFresnel::eval`, per channel.
pub fn conductor_fresnel(cos_theta: f32, n: Vec3A, k: Vec3A) -> Vec3A {
    let c = cos_theta.clamp(0.0, 1.0);
    let cos2 = Vec3A::splat(c * c);
    let sin2 = Vec3A::ONE - cos2;
    let n2 = n * n;
    let k2 = k * k;
    let t0 = n2 - k2 - sin2;
    let a2plusb2 = (t0 * t0 + 4.0 * n2 * k2).sqrt();
    let t1 = a2plusb2 + cos2;
    let a = (0.5 * (a2plusb2 + t0)).sqrt();
    let t2 = (2.0 * c) * a;
    let clamp = |v: Vec3A| v.clamp(Vec3A::splat(FLOAT_MIN), Vec3A::splat(BIG));
    let rs = (t1 - t2) / clamp(t1 + t2);
    let t3 = cos2 * a2plusb2 + sin2 * sin2;
    let t4 = t2 * sin2;
    let rp = rs * (t3 - t4) / clamp(t3 + t4);
    0.5 * (rp + rs).clamp(Vec3A::ZERO, Vec3A::splat(2.0))
}

/// `SchlickFresnel::eval`: `lerp((1 − c)^exponent, F0, F90)`, 1 past the
/// critical angle `tir_cos` (0 on the front side).
pub fn schlick_fresnel(c: f32, f0: Vec3A, f90: Vec3A, exponent: f32, tir_cos: f32) -> Vec3A {
    let c = c.clamp(0.0, 1.0);
    if c < tir_cos {
        return Vec3A::ONE;
    }
    let t = powf(1.0 - c, exponent).clamp(0.0, 1.0);
    (1.0 - t) * f0 + t * f90
}

/// `SchlickFresnel`'s critical cosine for a relative IOR `eta`: 0 when
/// `eta ≥ 1`, else Snell's `√(1 − η²)`.
pub fn schlick_tir_cos(eta: f32) -> f32 {
    if eta >= 1.0 {
        0.0
    } else {
        (1.0 - sq(eta)).sqrt()
    }
}

/// `SchlickLobe`'s refraction IOR: the inverse Schlick approximation of the
/// average `F0`, clamped below 0.99.
pub fn schlick_ior(f0: Vec3A) -> f32 {
    let avg_f0 = ((f0.x + f0.y + f0.z) / 3.0).clamp(0.0, 0.99);
    let s = avg_f0.sqrt();
    (1.0 + s) / (1.0 - s)
}

// ---------------------------------------------------------------------------
// Microfacet lobes
// ---------------------------------------------------------------------------

/// `eval_turquin_microms_reflection`, as `f` (without the cosine): single
/// scattering times Turquin's `1 + F·E_ms/(1 − E_ms)`, with `E_ms` the white
/// GGX loss at the view (`GGX` table) and `F = F(cos θ_mo)`. `fresnel` is
/// evaluated at `cos θ_mo` only.
pub fn eval_turquin_microms_reflection(
    dist: &GGXDist,
    fresnel: impl Fn(f32) -> Vec3A,
    e_ms: f32,
    wo: Vec3A,
    wi: Vec3A,
) -> Vec3A {
    if wi.z <= 0.0 || wo.z <= 0.0 {
        return Vec3A::ZERO;
    }
    let m = (wo + wi).normalize();
    let cos_mo = m.dot(wo);
    let d = dist.d(m);
    let g2 = dist.g2(wi, wo);
    let f = fresnel(cos_mo);
    let msf = e_ms / (1.0 - e_ms).max(0.01);
    f * (Vec3A::ONE + f * msf) * (d * g2 / (4.0 * wo.z * wi.z))
}

/// The reflection half of `DielectricBSDF::eval` with refraction on, as `f`:
/// single scattering, no compensation (the lobe's renormalisation is the
/// caller's).
pub fn eval_dielectric_reflection(
    dist: &GGXDist,
    fresnel: impl Fn(f32) -> Vec3A,
    wo: Vec3A,
    wi: Vec3A,
) -> Vec3A {
    if wi.z <= 0.0 || wo.z <= 0.0 {
        return Vec3A::ZERO;
    }
    let m = (wo + wi).normalize();
    let cos_mo = m.dot(wo);
    if cos_mo <= 0.0 {
        return Vec3A::ZERO;
    }
    fresnel(cos_mo) * (dist.d(m) * dist.g2(wi, wo) / (4.0 * wo.z * wi.z))
}

/// The refraction half of `DielectricBSDF::eval`, as `f`: Walter's BTDF
/// with `1 − F` per channel. `eta` is the relative IOR the ray refracts
/// into (`DielectricFresnel::refraction_eta`), `wi` below the plane.
pub fn eval_dielectric_refraction(
    dist: &GGXDist,
    fresnel: impl Fn(f32) -> Vec3A,
    eta: f32,
    wo: Vec3A,
    wi: Vec3A,
) -> Vec3A {
    if wi.z >= 0.0 || wo.z <= 0.0 {
        return Vec3A::ZERO;
    }
    // Flipped to the same side as N.
    let ht = (eta * wi + wo).normalize() * if eta > 1.0 { -1.0 } else { 1.0 };
    let cos_ho = ht.dot(wo);
    let cos_hi = ht.dot(wi);
    if cos_ho <= 0.0 || cos_hi >= 0.0 || ht.z <= 0.0 {
        return Vec3A::ZERO;
    }
    let ft = Vec3A::ONE - fresnel(cos_ho);
    let d = dist.d(ht);
    let g2 = dist.g2(Vec3A::new(wi.x, wi.y, -wi.z), wo);
    let j = (-cos_hi * cos_ho * sq(eta)) / (wo.z * sq(cos_hi * eta + cos_ho));
    ft * (d * g2 * j / -wi.z)
}

// ---------------------------------------------------------------------------
// Sheen
// ---------------------------------------------------------------------------

/// `ContyKullaDist::MIN_ROUGHNESS`.
pub const CONTY_KULLA_MIN_ROUGHNESS: f32 = 0.06;
/// `ZeltnerBurleySheen::MIN_ROUGHNESS` with LTC sampling, as BSDL renders.
pub const ZELTNER_MIN_ROUGHNESS: f32 = 0.02;

/// `SheenMicrofacet<ContyKullaDist<false>>::eval` as `f`: Conty and Kulla's
/// "Charlie" distribution with the non-legacy visibility
/// `cosθi·cosθo / (cosθi + cosθo − cosθi·cosθo)`, at a roughness already
/// floored at [`CONTY_KULLA_MIN_ROUGHNESS`].
pub fn conty_kulla_sheen(roughness: f32, wo: Vec3A, wi: Vec3A) -> f32 {
    let a = roughness.clamp(CONTY_KULLA_MIN_ROUGHNESS, 1.0);
    let (cos_no, cos_ni) = (wo.z, wi.z);
    if cos_ni <= 1e-5 || cos_no <= 1e-5 {
        return 0.0;
    }
    let cos_theta = (wo + wi).normalize().z.clamp(0.0, 1.0);
    let sin_theta = (1.0 - sq(cos_theta)).sqrt();
    let d = powf(sin_theta, 1.0 / a) * (2.0 + 1.0 / a) * 0.5 * FRAC_1_PI;
    if d < 1e-6 {
        return 0.0;
    }
    let (ci, co) = (cos_ni.min(1.0), cos_no.min(1.0));
    let g2 = (ci * co) / (ci + co - ci * co);
    d * g2 / (4.0 * cos_no * cos_ni)
}

/// `ContyKullaSheenMTX::albedo`: MaterialX's rational fit to the lobe's
/// directional albedo.
#[allow(clippy::excessive_precision)]
pub fn conty_kulla_sheen_albedo(roughness: f32, cos_no: f32) -> f32 {
    let r = roughness;
    let x = 13.67300 - 68.78018 * cos_no + 799.08825 * r - 905.00061 * cos_no * r
        + 60.28956 * cos_no * cos_no
        + 1086.96473 * r * r;
    let y = 1.0
        + 61.57746 * cos_no
        + 442.78211 * r
        + 2597.49308 * cos_no * r
        + 121.81241 * cos_no * cos_no
        + 3045.55075 * r * r;
    (x / y).clamp(0.0, 1.0)
}

/// `SheenLobe`'s `Emiss`, its layering filter: `1 − min(E(cosθo)·max(tint), 1)`.
pub fn sheen_emiss(albedo: f32, tint: Vec3A) -> f32 {
    1.0 - (albedo * tint.max_element()).min(1.0)
}

// ---------------------------------------------------------------------------
// Diffuse
// ---------------------------------------------------------------------------

/// `constant1_FON` and `constant2_FON` (Fujii).
const CONSTANT1_FON: f32 = 0.5 - 2.0 / (3.0 * PI);
const CONSTANT2_FON: f32 = 2.0 / 3.0 - 28.0 / (15.0 * PI);

/// `OrenNayarDiffuseLobe::E_FON_analytic`.
fn e_fon_analytic(sigma: f32, mu: f32) -> f32 {
    let af = 1.0 / (1.0 + CONSTANT1_FON * sigma);
    let bf = sigma * af;
    let si = (1.0 - mu * mu).max(0.0).sqrt();
    let g = si * (mu.acos() - si * mu)
        + 2.0 * ((si / mu.max(1e-7)) * (1.0 - si * si * si) - si) * (1.0 / 3.0);
    af + (bf * FRAC_1_PI) * g
}

/// `OrenNayarDiffuseLobe::eval_impl` as `f`: the qualitative Oren–Nayar
/// lobe, or with `energy_compensation` Portsmouth's EON.
pub fn oren_nayar(
    albedo: Vec3A,
    sigma: f32,
    energy_compensation: bool,
    wo: Vec3A,
    wi: Vec3A,
) -> Vec3A {
    let cos_ni = wi.z.clamp(0.0, 1.0);
    let cos_no = wo.z.clamp(0.0, 1.0);
    if cos_ni <= 0.0 || cos_no <= 0.0 {
        return Vec3A::ZERO;
    }
    let cos_io = wo.dot(wi).clamp(-1.0, 1.0);
    let s = cos_io - cos_ni * cos_no;
    if !energy_compensation {
        let s2 = sq(sigma);
        let a = 1.0 - 0.50 * s2 / (s2 + 0.33);
        let b = 0.45 * s2 / (s2 + 0.09);
        let stinv = if s > 0.0 { s / cos_ni.max(cos_no) } else { 0.0 };
        return albedo * ((a + b * stinv) * FRAC_1_PI);
    }
    let af = 1.0 / (1.0 + CONSTANT1_FON * sigma);
    let stinv = if s > 0.0 { s / cos_ni.max(cos_no) } else { s };
    let f_ss = af * (1.0 + sigma * stinv);
    let efo = e_fon_analytic(sigma, cos_no);
    let efi = e_fon_analytic(sigma, cos_ni);
    let avg_ef = af * (1.0 + CONSTANT2_FON * sigma);
    let rho_ms = albedo * albedo * avg_ef / (Vec3A::ONE - albedo * (1.0 - avg_ef).max(0.0));
    let f_ms = (1.0 - efo).max(1e-7) * (1.0 - efi).max(1e-7) / (1.0 - avg_ef).max(1e-7);
    (albedo * f_ss + rho_ms * f_ms) * FRAC_1_PI
}

/// `BurleyDiffuseLobe::eval_impl` as `f`.
pub fn burley(albedo: Vec3A, roughness: f32, wo: Vec3A, wi: Vec3A) -> Vec3A {
    if wo.z <= 0.0 || wi.z <= 0.0 {
        return Vec3A::ZERO;
    }
    let h = wi + wo;
    if h.abs().max_element() < EPSILON {
        return Vec3A::ZERO;
    }
    let hn = h.normalize();
    let cos_hi = wi.dot(hn).clamp(0.0, 1.0);
    let cos_no = wo.z.clamp(0.0, 1.0);
    let cos_ni = wi.z.clamp(0.0, 1.0);
    let f90 = 0.5 + 2.0 * roughness * sq(cos_hi);
    let fresnel = |c: f32| {
        let x = (1.0 - c).clamp(0.0, 1.0);
        lerp(sq(sq(x)) * x, 1.0, f90)
    };
    albedo * (fresnel(cos_ni) * fresnel(cos_no) * FRAC_1_PI)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ggx_reconstructs_the_authored_alphas() {
        // BSDL's (roughness, aniso) pair is a re-expression, not a change:
        // the distribution it builds has the alphas it was given.
        for (rx, ry) in [(0.3f32, 0.3f32), (0.5, 0.1), (0.1, 0.5), (1.0, 1e-4)] {
            let (d, r) = GGXDist::from_alphas(rx, ry);
            assert!((d.ax - rx).abs() < 1e-6 * rx.max(1.0), "{rx} {ry}: {d:?}");
            assert!((d.ay - ry).abs() < 1e-6 * ry.max(1.0), "{rx} {ry}: {d:?}");
            // `r² = (α_x + α_y)/2`.
            assert!((r * r - (rx + ry) / 2.0).abs() < 1e-6, "{r}");
        }
    }

    #[test]
    fn ggx_d_is_normalised_and_g_is_one_at_normal_incidence() {
        // ∫ D(h) cosθh dω = 1, by a midpoint rule over the hemisphere.
        let dist = GGXDist::new(0.6, 0.3, false);
        let (nt, np) = (400, 200);
        let mut sum = 0.0f64;
        for i in 0..nt {
            let t = (i as f32 + 0.5) / nt as f32 * PI / 2.0;
            for j in 0..np {
                let p = (j as f32 + 0.5) / np as f32 * 2.0 * PI;
                let h = Vec3A::new(t.sin() * p.cos(), t.sin() * p.sin(), t.cos());
                sum += (dist.d(h) * h.z * t.sin()) as f64;
            }
        }
        let integral = sum * (PI as f64 / 2.0 / nt as f64) * (2.0 * PI as f64 / np as f64);
        assert!((integral - 1.0).abs() < 2e-3, "∫D cos = {integral}");
        assert!((dist.g1(Vec3A::Z) - 1.0).abs() < 1e-6);
        assert!((dist.g2(Vec3A::Z, Vec3A::Z) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn the_tables_answer_at_their_grid_points() {
        // At a grid roughness, IOR index and cosine the curve is the stored
        // value; between cosines it is linear.
        let r = 5.0 / 15.0;
        let fi = 7.0 / 31.0;
        let curve = TabulatedEnergyCurve::new(REFL_FRONT, r, fi);
        let at = |c: usize| DIELECTRIC_REFL_FRONT[(7 * NR + 5) * NC + c];
        assert!((curve.emiss_eval(9.0 / 15.0) - at(9)).abs() < 1e-6);
        let mid = curve.emiss_eval(9.5 / 15.0);
        assert!((mid - 0.5 * (at(9) + at(10))).abs() < 1e-5);
        // The GGX table's cosines are squared: (i/15)².
        let g = TabulatedEnergyCurve::new(GGX, r, 0.0);
        assert!((g.emiss_eval(sq(9.0 / 15.0)) - GGX_TABLE[5 * NC + 9]).abs() < 1e-6);
    }

    /// BSDL's `Emiss_eval`, its linear scan, verbatim.
    fn scanned(curve: &TabulatedEnergyCurve, c: f32) -> f32 {
        let mut cos0 = curve.table.cosine(0);
        if c <= cos0 {
            return curve.interpolate_emiss(0);
        }
        for i in 1..NC {
            let cos1 = curve.table.cosine(i);
            if c < cos1 {
                let q = (c - cos0) / (cos1 - cos0);
                return lerp(
                    q,
                    curve.interpolate_emiss(i - 1),
                    curve.interpolate_emiss(i),
                );
            }
            cos0 = cos1;
        }
        curve.interpolate_emiss(NC - 1)
    }

    #[test]
    fn the_direct_bin_is_the_scanned_one() {
        // Every cosine on a fine grid, the grid points themselves and their
        // neighbouring floats, for both spacings.
        for table in [GGX, REFL_FRONT, BOTH_BACK] {
            let curve = TabulatedEnergyCurve::new(table, 0.37, 0.61);
            let mut cs: Vec<f32> = (0..=20_000).map(|i| i as f32 / 20_000.0).collect();
            for i in 0..NC {
                let c = table.cosine(i);
                cs.extend([
                    c,
                    f32::from_bits(c.to_bits() - 1),
                    f32::from_bits(c.to_bits() + 1),
                ]);
            }
            cs.push(1.5);
            for c in cs {
                assert_eq!(curve.emiss_eval(c), scanned(&curve, c), "cos {c}");
            }
        }
    }

    #[test]
    fn the_ggx_table_is_white_ggx_s_missing_energy() {
        // `1 − E_ss` of a white GGX mirror: none when smooth; at α = 1 the
        // values a 600² quadrature of `D·G2/(4 cosθo)` gives (0.153 at
        // μ = 0.05, 0.549 at 0.5, 0.693 head on — the loss grows toward
        // normal incidence).
        let e = |r: f32, c: f32| TabulatedEnergyCurve::new(GGX, r, 0.0).emiss_eval(c);
        assert!(e(0.0, 0.8) < 1e-3);
        for (c, want) in [(0.05f32, 0.153f32), (0.5, 0.549), (1.0, 0.693)] {
            assert!((e(1.0, c) - want).abs() < 0.01, "μ {c}: {}", e(1.0, c));
        }
    }

    #[test]
    fn dielectric_fresnel_has_the_textbook_values() {
        assert!((dielectric_fresnel(1.0, 1.5) - 0.04).abs() < 1e-6);
        assert_eq!(dielectric_fresnel(0.1, 1.0 / 1.5), 1.0, "TIR");
        assert_eq!(dielectric_eta(1.5, true), 1.0 / 1.5);
        assert_eq!(dielectric_eta(1.0, false), IOR_MIN);
        assert_eq!(table_index(1.5), table_index(1.0 / 1.5));
        assert_eq!(table_index(IOR_MAX), 1.0);
    }

    #[test]
    fn a_conductor_with_no_extinction_is_a_dielectric() {
        let c = conductor_fresnel(0.7, Vec3A::splat(1.5), Vec3A::ZERO);
        assert!((c.x - dielectric_fresnel(0.7, 1.5)).abs() < 1e-5, "{c}");
    }

    #[test]
    fn schlick_hits_f0_head_on_and_f90_at_grazing_or_past_tir() {
        let (f0, f90) = (Vec3A::new(0.9, 0.6, 0.2), Vec3A::ONE);
        assert_eq!(schlick_fresnel(1.0, f0, f90, 5.0, 0.0), f0);
        assert_eq!(schlick_fresnel(0.0, f0, f90, 5.0, 0.0), f90);
        let tir = schlick_tir_cos(1.0 / 1.5);
        assert!((tir - (1.0f32 - 1.0 / 2.25).sqrt()).abs() < 1e-6);
        assert_eq!(schlick_fresnel(tir * 0.9, f0, f0, 5.0, tir), Vec3A::ONE);
        // F0 = 0.04 is IOR 1.5.
        assert!((schlick_ior(Vec3A::splat(0.04)) - 1.5).abs() < 1e-5);
    }

    #[test]
    fn turquin_restores_a_white_mirror_s_lost_energy() {
        // A white compensated GGX reflects everything it receives: its
        // directional albedo, integrated by a cosine-weighted midpoint rule,
        // is 1 within the table's accuracy.
        for (r, c) in [(0.3f32, 0.7f32), (0.8, 0.5), (1.0, 0.9)] {
            let (dist, _) = GGXDist::from_alphas(r * r, r * r);
            let e_ms = TabulatedEnergyCurve::new(GGX, r, 0.0).emiss_eval(c);
            let wo = Vec3A::new((1.0 - c * c).sqrt(), 0.0, c);
            let n = 256;
            let mut sum = 0.0f64;
            for i in 0..n {
                for j in 0..n {
                    let u = (i as f32 + 0.5) / n as f32;
                    let v = (j as f32 + 0.5) / n as f32;
                    let (rr, phi) = (u.sqrt(), 2.0 * PI * v);
                    let wi = Vec3A::new(rr * phi.cos(), rr * phi.sin(), (1.0 - u).sqrt());
                    let f = eval_turquin_microms_reflection(&dist, |_| Vec3A::ONE, e_ms, wo, wi);
                    sum += f.x as f64;
                }
            }
            let albedo = sum * std::f64::consts::PI / (n * n) as f64;
            assert!((albedo - 1.0).abs() < 0.02, "r {r} c {c}: {albedo}");
        }
    }

    #[test]
    fn eon_is_white_on_white() {
        // EON's single plus multiple scattering keeps a white surface's
        // directional albedo at 1.
        for mu in [0.2f32, 0.5, 0.9] {
            let wo = Vec3A::new((1.0 - mu * mu).sqrt(), 0.0, mu);
            let n = 256;
            let mut sum = 0.0f64;
            for i in 0..n {
                for j in 0..n {
                    let u = (i as f32 + 0.5) / n as f32;
                    let v = (j as f32 + 0.5) / n as f32;
                    let (rr, phi) = (u.sqrt(), 2.0 * PI * v);
                    let wi = Vec3A::new(rr * phi.cos(), rr * phi.sin(), (1.0 - u).sqrt());
                    sum += oren_nayar(Vec3A::ONE, 0.7, true, wo, wi).x as f64;
                }
            }
            let albedo = sum * std::f64::consts::PI / (n * n) as f64;
            assert!((albedo - 1.0).abs() < 5e-3, "μ {mu}: {albedo}");
        }
    }

    #[test]
    fn sheen_emiss_is_the_tinted_albedo_s_complement() {
        assert_eq!(sheen_emiss(0.3, Vec3A::new(0.5, 1.0, 0.2)), 0.7);
        assert_eq!(sheen_emiss(0.8, Vec3A::splat(2.0)), 0.0);
    }
}
