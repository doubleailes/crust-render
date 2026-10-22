//! The MaterialX closure-tree evaluator.
//!
//! A MaterialX material compiles (`crust-mtlx`) to a tree of BSDF leaves
//! combined by `layer`, `mix`, `add` and `multiply`, with the semantics of
//! MaterialX's OSL backend as OSL's `testrender` renders it:
//! `layer(top, base) = top + base · (1 − clamp(O_top(ωo), 0, 1))`. The
//! opacity `O_top` depends on the outgoing direction alone, and ωo is fixed at
//! a path vertex, so there the tree is *exactly* a weighted sum of its leaves:
//!
//! - `mix` scales its branches by `m` and `1 − m`, and blends their opacities,
//! - `multiply` scales its input, response and opacity alike,
//! - `add` concatenates, and sums the opacities,
//! - `layer` scales every base leaf by `1 − clamp(O_top)`, and its own opacity
//!   is the sum of its top's and its base's (`evaluate_layer_opacity`).
//!
//! [`ResolvedClosure::resolve`] walks the tree once per vertex and keeps that
//! weighted list inline — no allocation, no second walk. Every BSDF query at
//! the vertex (the scatter, NEE's and guiding's `eval`) then sums a handful of
//! leaves, each with its own roughness, Fresnel, normal and tangent. Nothing
//! is pooled: that is the difference from the reduction this replaced, which
//! averaged every leaf onto one OpenPBR parameter set.
//!
//! The leaves are BSDL's `mtx::` lobes ([`bsdl`]), the ones testrender shades
//! MaterialX with: GGX with Turquin's compensation on reflection-only lobes
//! and a tabulated renormalisation on refracting ones, Oren–Nayar / EON /
//! Burley diffuse, Conty–Kulla and Zeltner sheen, each with BSDL's layering
//! filter. Sampling is crust's own (GGX VNDF, cosine, the sheen's LTC). The
//! Airy thin film, which BSDL's lobes do not apply, stays MaterialX's GLSL
//! ([`mx`]); `tests/bsdl_oracle.rs` checks the leaves against BSDL itself.

pub mod bsdl;
mod bsdl_tables;
pub mod hair;
pub mod mx;

use glam::Vec3A;
use std::cell::RefCell;
use std::f32::consts::FRAC_1_PI;
use utils::cosine_hemisphere;

use crate::hittable::HitRecord;
use crate::material::ScatterSample;
use crate::material::SubsurfaceEntry;
use crate::material::brdf::{
    ZeltnerSheen, pdf_vndf_ggx_aniso_local, pdf_vndf_h_aniso_local, sample_vndf_ggx_aniso_local,
    tangent_frame,
};
use crate::medium::Medium;
use crate::ray::Ray;
use bsdl::{GGXDist, TabulatedEnergyCurve};
use crust_mtlx::{Bsdf, Closure, Closures, DiffuseModel, NodeId, ScatterMode, SheenMode, Val};
use mx::{Fresnel, FresnelModel};

/// How many leaves a resolved closure holds inline.
///
/// The largest built-in expansion is seven (`standard_surface`: diffuse,
/// subsurface, sheen, transmission, dielectric, metal, coat). A document whose
/// tree has more leaves than this is refused at load, so the list never
/// silently drops one.
pub const MAX_LEAVES: usize = 8;

/// The GGX alpha floor: BSDL's `EPSILON`, which its MaterialX lobes clamp
/// the authored alphas to. A mirror this sharp is already indistinguishable
/// from a delta at any pixel footprint.
const MIN_ALPHA: f32 = bsdl::EPSILON;

/// A leaf's shading frame: its own normal and tangent.
#[derive(Clone, Copy, Debug)]
pub struct Frame {
    pub t: Vec3A,
    pub b: Vec3A,
    pub n: Vec3A,
}

impl Frame {
    fn new(n: Vec3A, tangent: Vec3A) -> Frame {
        let t = tangent - n * n.dot(tangent);
        let (t, b) = if t.length_squared() > 1e-10 {
            let t = t.normalize();
            (t, n.cross(t))
        } else {
            tangent_frame(n)
        };
        Frame { t, b, n }
    }

    /// The frame turned by `angle` radians about its normal, right-handed:
    /// the tangent moves toward the bitangent `n × t`
    /// ([`crust_mtlx::Leaf::rotation`]). A non-finite angle turns nothing.
    fn rotated(self, angle: f32) -> Frame {
        if !angle.is_finite() {
            return self;
        }
        let (sin, cos) = angle.sin_cos();
        let t = self.t * cos + self.n.cross(self.t) * sin;
        Frame {
            t,
            b: self.n.cross(t),
            n: self.n,
        }
    }

    fn to_local(self, v: Vec3A) -> Vec3A {
        Vec3A::new(v.dot(self.t), v.dot(self.b), v.dot(self.n))
    }

    fn to_world(self, v: Vec3A) -> Vec3A {
        self.t * v.x + self.b * v.y + self.n * v.z
    }
}

/// What a resolved leaf evaluates.
#[derive(Clone, Copy, Debug)]
pub enum Lobe {
    Diffuse {
        model: DiffuseModel,
        color: Vec3A,
        roughness: f32,
    },
    /// A GGX microfacet interface: `dielectric_bsdf`, `conductor_bsdf` or
    /// `generalized_schlick_bsdf`, BSDL's `DielectricLobe`, `ConductorLobe`
    /// and `SchlickLobe`.
    Specular {
        fresnel: Fresnel,
        /// The tint toward the reflecting side, and toward the far side:
        /// MaterialX's `tint` split by scatter mode, as genosl splits it.
        refl_tint: Vec3A,
        refr_tint: Vec3A,
        /// BSDL's distribution (`GGXDist::from_alphas`): the authored
        /// alphas, re-expressed through its roughness and anisotropy.
        ax: f32,
        ay: f32,
        mode: ScatterMode,
        /// Relative IOR across the interface in the ray-facing frame
        /// (`η_t / η_i`), for the refracted half of a transmitting leaf.
        eta: f32,
        /// A reflection-only leaf's Turquin `E_ms`: white GGX's missing
        /// energy toward this view. 0 for a refracting one.
        e_ms: f32,
        /// A refracting leaf's renormalisation `1 / max(0.01, 1 − E_ms)`,
        /// `E_ms` the reflect-and-refract table of its face. 1 otherwise.
        renorm: f32,
        /// A thin-walled surface transmits straight through (a delta lobe).
        thin_walled: bool,
    },
    /// `sheen_bsdf`, BSDL's `SheenLobe`. In `conty_kulla` mode the
    /// Conty–Kulla lobe, sampled cosine-weighted, at `roughness` floored at
    /// 0.06; in `zeltner` mode the LTC sheen at `√roughness` (floored at
    /// 0.02), prepared at this vertex's view ([`ZeltnerSheen`]). Black on a
    /// back face, which BSDL's sheen does not shade.
    Sheen {
        color: Vec3A,
        roughness: f32,
        zeltner: Option<ZeltnerSheen>,
    },
    Translucent {
        color: Vec3A,
    },
    /// `subsurface_bsdf`: no value toward any direction (as in Typhoon, the
    /// leaf does no NEE); selecting it enters a random walk
    /// (`crust_core`'s `subsurface` module) through the interface above it.
    Subsurface {
        color: Vec3A,
        radius: Vec3A,
        anisotropy: f32,
        /// The entry interface: its IOR (≥ 1) and GGX alpha.
        ior: f32,
        alpha: f32,
    },
    /// `chiang_hair_bsdf`: a fibre, over the whole sphere, prepared for this
    /// vertex's ωo ([`hair::Hair`]).
    Hair(hair::Hair),
}

/// One leaf at a vertex: its lobe, the weight the tree gives it there, its
/// frame, ωo in that frame, and its (unnormalised) selection weight.
#[derive(Clone, Copy, Debug)]
pub struct Prepared {
    pub lobe: Lobe,
    pub weight: Vec3A,
    pub frame: Frame,
    v: Vec3A,
    select: f32,
    /// The leaf's albedo estimate its selection weight is the luminance of
    /// (BSDL's `albedo_impl`), for probes.
    pub albedo: Vec3A,
    /// The MaterialX category, for probes.
    pub category: &'static str,
    /// A reflecting interface layered over another reflecting interface: the
    /// coat, for light path expressions (`'coat'` rather than `'specular'`).
    /// MaterialX leaves carry no component name, so the tree's shape is
    /// what tells — see `ResolvedClosure::walk`.
    pub coat: bool,
    /// The top of a layer with a weighted leaf under it — any leaf, not only
    /// a reflecting one, so a coat over a bare diffuse (`standard_surface`
    /// at `specular = 0`) is one too. The shading normal the AOVs report is
    /// read from the leaves under every layer.
    pub over_base: bool,
}

impl Prepared {
    /// The event this leaf contributes toward a direction on the reflecting
    /// side, or (`transmitted`) the far side. Total over [`Lobe`].
    pub fn event(&self, transmitted: bool) -> crate::material::lobe::LobeEvent {
        use crate::material::lobe::{LobeEvent, LobeLabel, Scatter, microfacet_scatter};
        match self.lobe {
            Lobe::Diffuse { .. } => LobeEvent::reflect(Scatter::Diffuse, LobeLabel::Diffuse),
            Lobe::Translucent { .. } => {
                LobeEvent::transmit(Scatter::Diffuse, LobeLabel::Translucent)
            }
            Lobe::Sheen { .. } => LobeEvent::reflect(Scatter::Glossy, LobeLabel::Sheen),
            Lobe::Subsurface { .. } => LobeEvent::transmit(Scatter::Diffuse, LobeLabel::Subsurface),
            // A fibre by hemisphere, like every other leaf: R and most of TRT
            // come back toward the viewer, TT and TRRT+ go on through.
            Lobe::Hair(_) => {
                if transmitted {
                    LobeEvent::transmit(Scatter::Glossy, LobeLabel::Transmission)
                } else {
                    LobeEvent::reflect(Scatter::Glossy, LobeLabel::Specular)
                }
            }
            Lobe::Specular {
                ax,
                ay,
                thin_walled,
                ..
            } => {
                if transmitted {
                    let scatter = if thin_walled {
                        Scatter::Singular
                    } else {
                        microfacet_scatter(mx::average_alpha(ax, ay))
                    };
                    LobeEvent::transmit(scatter, LobeLabel::Transmission)
                } else {
                    let label = if self.coat {
                        LobeLabel::Coat
                    } else {
                        LobeLabel::Specular
                    };
                    LobeEvent::reflect(microfacet_scatter(mx::average_alpha(ax, ay)), label)
                }
            }
        }
    }

    /// A one-line description of the lobe's parameters, for probes.
    pub fn describe(&self) -> String {
        let c = |v: Vec3A| format!("({:.4} {:.4} {:.4})", v.x, v.y, v.z);
        match self.lobe {
            Lobe::Diffuse {
                model,
                color,
                roughness,
            } => format!("{model:?} color {} roughness {roughness:.3}", c(color)),
            Lobe::Specular {
                fresnel,
                refl_tint,
                refr_tint,
                ax,
                ay,
                mode,
                eta,
                e_ms,
                renorm,
                thin_walled,
            } => {
                let f = match fresnel.model {
                    FresnelModel::Dielectric { ior } => format!("ior {ior:.4}"),
                    FresnelModel::Conductor { n, k } => format!("n {} k {}", c(n), c(k)),
                    FresnelModel::Schlick {
                        f0,
                        f82,
                        f90,
                        exponent,
                        ..
                    } => format!(
                        "f0 {} f82 {} f90 {} exp {exponent:.2}",
                        c(f0),
                        c(f82),
                        c(f90)
                    ),
                };
                let film = fresnel
                    .thin_film
                    .map(|(d, n)| format!(" film {d:.1}nm/{n:.3}"))
                    .unwrap_or_default();
                let wall = if thin_walled { " thin-walled" } else { "" };
                format!(
                    "{mode:?} {f}{film} tint {} / {} alpha ({ax:.4} {ay:.4}) eta {eta:.4} e_ms {e_ms:.4} renorm {renorm:.4}{wall}",
                    c(refl_tint),
                    c(refr_tint)
                )
            }
            Lobe::Sheen {
                color,
                roughness,
                zeltner,
            } => format!(
                "sheen ({}) color {} roughness {roughness:.3}",
                if zeltner.is_some() {
                    "zeltner"
                } else {
                    "conty_kulla"
                },
                c(color)
            ),
            Lobe::Translucent { color } => format!("translucent color {}", c(color)),
            Lobe::Hair(h) => format!("hair albedo {}", c(h.albedo())),
            Lobe::Subsurface {
                color,
                radius,
                anisotropy,
                ior,
                alpha,
            } => format!(
                "random walk color {} radius {} anisotropy {anisotropy:.3} entry ior {ior:.4} alpha {alpha:.4}",
                c(color),
                c(radius)
            ),
        }
    }
}

const EMPTY: Prepared = Prepared {
    lobe: Lobe::Translucent { color: Vec3A::ZERO },
    weight: Vec3A::ZERO,
    frame: Frame {
        t: Vec3A::X,
        b: Vec3A::Y,
        n: Vec3A::Z,
    },
    v: Vec3A::Z,
    select: 0.0,
    albedo: Vec3A::ZERO,
    category: "",
    coat: false,
    over_base: false,
};

/// A MaterialX closure tree collapsed at one vertex.
#[derive(Clone, Debug)]
pub struct ResolvedClosure {
    leaves: [Prepared; MAX_LEAVES],
    len: usize,
    /// Sum of the selection weights, for normalising.
    select_total: f32,
    /// The interior medium a ray refracting into a thick surface carries.
    medium: Option<Medium>,
    /// Whether any leaf transmits.
    transmits: bool,
    /// Whether any leaf is a fibre: its rays pass out of curve tubes
    /// ([`crust_rt::Ray::ignore_curve_exits`]).
    hair: bool,
    /// Bit `i` set when leaf `i` is a fibre.
    hair_leaves: u8,
}

thread_local! {
    /// Boxes [`PooledClosure`]s return to, per thread. Bounded, so a thread
    /// that once held many vertices at once does not keep them all.
    // Boxes, not values: a closure leaves the pool as a pointer move, where
    // `Vec<ResolvedClosure>` would copy its ~1.9 KB out on every pop.
    #[allow(clippy::vec_box)]
    static POOL: RefCell<Vec<Box<ResolvedClosure>>> = const { RefCell::new(Vec::new()) };
}

/// How many recycled closures a thread keeps.
const POOL_CAP: usize = 16;

/// A [`ResolvedClosure`] on the heap, recycled through a per-thread pool.
///
/// The inline form is ~1.9 KB, of which a typical vertex uses two or three
/// leaves, and the path from `Material::resolve` into a `ShadingPoint` moved
/// it whole about seven times, plus one copy per leaf to initialise it:
/// `memcpy` was 17% of the instructions of `materialx_basic`, and the
/// `ShadingPoint` every material shares grew by the same 1.9 KB. Boxed, a
/// vertex moves a pointer and resolves in place; recycled, it allocates
/// nothing once each thread's pool is warm.
pub struct PooledClosure(Option<Box<ResolvedClosure>>);

impl PooledClosure {
    /// [`ResolvedClosure::resolve`] into a recycled box.
    pub fn resolve(
        closures: &Closures,
        slots: &[Val],
        r_in: &Ray,
        rec: &HitRecord,
        luma: utils::Luma,
    ) -> Self {
        let mut b = POOL
            .try_with(|p| p.borrow_mut().pop())
            .ok()
            .flatten()
            .unwrap_or_else(|| Box::new(ResolvedClosure::empty()));
        b.resolve_into(closures, slots, r_in, rec, luma);
        PooledClosure(Some(b))
    }
}

impl std::ops::Deref for PooledClosure {
    type Target = ResolvedClosure;
    fn deref(&self) -> &ResolvedClosure {
        // `None` only between `drop` taking the box and the value vanishing.
        self.0
            .as_deref()
            .expect("a live PooledClosure holds its box")
    }
}

impl std::ops::DerefMut for PooledClosure {
    fn deref_mut(&mut self) -> &mut ResolvedClosure {
        self.0
            .as_deref_mut()
            .expect("a live PooledClosure holds its box")
    }
}

impl Drop for PooledClosure {
    // Inline, with the pool work out of line: every `ShadingPoint` has this
    // in its drop glue, and most of them hold no closure at all.
    #[inline]
    fn drop(&mut self) {
        if let Some(b) = self.0.take() {
            recycle(b);
        }
    }
}

#[inline(never)]
fn recycle(b: Box<ResolvedClosure>) {
    // During thread teardown the pool may already be gone; the box is then
    // simply freed.
    let _ = POOL.try_with(|p| {
        let mut p = p.borrow_mut();
        if p.len() < POOL_CAP {
            p.push(b);
        }
    });
}

/// What the tree walk needs besides the tree.
struct Walk<'a> {
    slots: &'a [Val],
    rec: &'a HitRecord<'a>,
    v_world: Vec3A,
    thin_walled: bool,
    /// The working space's luminance weights, for lobe selection.
    luma: utils::Luma,
}

/// The interface a subsurface leaf is entered through: the nearest
/// dielectric layered over it, or — with none, as for a bare
/// `subsurface_bsdf` — Typhoon's closure defaults, IOR 1.5 and roughness 0.5.
#[derive(Clone, Copy, Debug)]
struct Interface {
    ior: f32,
    alpha: f32,
}

const DEFAULT_INTERFACE: Interface = Interface {
    ior: 1.5,
    alpha: 0.25,
};

/// The live dielectric a layer's top reaches, as an entry interface —
/// the leaf that contributes, not merely the first one in the tree. A leaf
/// at weight 0, a `mix` branch at factor 0 and a `multiply` by 0 are skipped
/// exactly as the collapse walk drops them (a coat whose weight is textured
/// to 0 there must not set the entry's IOR); of a live `mix`, the heavier
/// branch is asked first.
fn interface_of(cl: &Closures, id: NodeId, slots: &[Val]) -> Option<Interface> {
    match &cl.nodes[id as usize] {
        Closure::Leaf(leaf) => match &leaf.bsdf {
            Bsdf::Dielectric { ior, roughness, .. } if slots[leaf.weight as usize].x() > 0.0 => {
                let ior = slots[*ior as usize].x();
                let ior = if ior.is_finite() { ior.max(1.0) } else { 1.5 };
                let (ax, ay) = alphas(slots[*roughness as usize]);
                Some(Interface {
                    ior,
                    alpha: mx::average_alpha(ax, ay),
                })
            }
            _ => None,
        },
        Closure::Multiply { input, weight } => {
            let k = sanitize(slots[*weight as usize].rgb());
            (k.max_element() > 0.0)
                .then(|| interface_of(cl, *input, slots))
                .flatten()
        }
        Closure::Mix { fg, bg, mix } => {
            let m = slots[*mix as usize].x().clamp(0.0, 1.0);
            let fg = (m > 0.0).then_some(*fg);
            let bg = (m < 1.0).then_some(*bg);
            let (first, second) = if m >= 0.5 { (fg, bg) } else { (bg, fg) };
            first
                .and_then(|b| interface_of(cl, b, slots))
                .or_else(|| second.and_then(|b| interface_of(cl, b, slots)))
        }
        Closure::Layer { top, .. } => interface_of(cl, *top, slots),
        Closure::Add { a, b } => {
            interface_of(cl, *a, slots).or_else(|| interface_of(cl, *b, slots))
        }
        Closure::Empty => None,
    }
}

impl ResolvedClosure {
    /// Collapses `closures` at a vertex: `slots` is the evaluated program,
    /// `rec` the hit (its normal the ray-facing shading normal), `r_in` the
    /// arriving ray, `luma` the working space's luminance weights, which
    /// leaves are selected by.
    pub fn resolve(
        closures: &Closures,
        slots: &[Val],
        r_in: &Ray,
        rec: &HitRecord,
        luma: utils::Luma,
    ) -> ResolvedClosure {
        let mut out = ResolvedClosure::empty();
        out.resolve_into(closures, slots, r_in, rec, luma);
        out
    }

    fn empty() -> ResolvedClosure {
        ResolvedClosure {
            leaves: [EMPTY; MAX_LEAVES],
            len: 0,
            select_total: 0.0,
            medium: None,
            transmits: false,
            hair: false,
            hair_leaves: 0,
        }
    }

    /// [`ResolvedClosure::resolve`] into `self`, overwriting whatever it
    /// held; leaves past the new length are left stale, never read.
    fn resolve_into(
        &mut self,
        closures: &Closures,
        slots: &[Val],
        r_in: &Ray,
        rec: &HitRecord,
        luma: utils::Luma,
    ) {
        let thin_walled = closures
            .thin_walled
            .is_some_and(|s| slots[s as usize].x() > 0.5);
        self.len = 0;
        self.select_total = 0.0;
        self.medium = None;
        self.transmits = false;
        self.hair = false;
        self.hair_leaves = 0;
        let walk = Walk {
            slots,
            rec,
            v_world: -r_in.direction().normalize(),
            thin_walled,
            luma,
        };
        if let Some(root) = closures.root {
            self.walk(closures, root, Vec3A::ONE, DEFAULT_INTERFACE, &walk);
        }
        self.select_total = self.leaves[..self.len].iter().map(|l| l.select).sum();
        if self.transmits && !thin_walled {
            self.medium = volume_medium(closures, slots);
        }
    }

    /// An unbiased estimate of the weight this closure gives the arriving
    /// ray continuing straight through: the summed contribution of its
    /// thin-walled transmitting leaves, each `weight · tint · (1 − F(v·h))`
    /// at one microfacet normal `h` drawn from the leaf's VNDF with the two
    /// uniforms `draw` produces, exactly as [`sample_lobe`] draws it for the
    /// delta sample. `draw` is called at most once, and only for a leaf
    /// that is one: a closure with no thin-walled transmitting leaf draws
    /// nothing.
    ///
    /// An estimate, not the value: a rough leaf's straight transmission is
    /// the average of `1 − F(v·h)` over its microfacets, which has no closed
    /// form. Its expectation is the delta lobe's, which is all the
    /// integrator's pass-through needs. Zero when no leaf is one.
    pub fn straight_transmittance(&self, draw: impl FnOnce() -> [f32; 2]) -> Vec3A {
        let mut sum = Vec3A::ZERO;
        let mut uv = None;
        let mut draw = Some(draw);
        for leaf in &self.leaves[..self.len] {
            let Lobe::Specular {
                fresnel,
                refr_tint,
                ax,
                ay,
                mode,
                thin_walled: true,
                ..
            } = leaf.lobe
            else {
                continue;
            };
            if !mode.transmits() {
                continue;
            }
            let uv = *uv.get_or_insert_with(|| draw.take().expect("drawn once")());
            let v = leaf.v;
            let v = Vec3A::new(v.x, v.y, v.z.max(1e-6)).normalize();
            let h = sample_vndf_ggx_aniso_local(v, ax, ay, uv);
            let vh = v.dot(h);
            if vh <= 1e-6 {
                continue;
            }
            sum += leaf.weight * refr_tint * (1.0 - scalar(fresnel.eval(vh)));
        }
        sum
    }

    /// Whether any leaf is a thin-walled transmitting interface.
    pub fn has_straight(&self) -> bool {
        self.leaves[..self.len].iter().any(|leaf| {
            matches!(leaf.lobe, Lobe::Specular { mode, thin_walled: true, .. } if mode.transmits())
        })
    }

    /// Drops the straight transmission from this closure: the lobe set a
    /// path meeting a thin wall it could have passed scatters through. A
    /// transmission-only thin leaf leaves the tree; a thin leaf that also
    /// reflects keeps its reflection, now sampled with the leaf's whole
    /// selection mass (its density loses the `F` factor accordingly). Every
    /// other leaf, weights included, is untouched, so the layering above
    /// the sheet still attenuates what is left exactly as before.
    pub(crate) fn exclude_straight(&mut self) {
        let mut kept = 0;
        let mut hair_leaves = 0;
        for i in 0..self.len {
            let mut leaf = self.leaves[i];
            if let Lobe::Specular {
                ref mut mode,
                thin_walled: true,
                ..
            } = leaf.lobe
            {
                match *mode {
                    ScatterMode::T => continue,
                    ScatterMode::RT => *mode = ScatterMode::R,
                    ScatterMode::R => {}
                }
            }
            if self.hair_leaves & (1 << i) != 0 {
                hair_leaves |= 1 << kept;
            }
            self.leaves[kept] = leaf;
            kept += 1;
        }
        self.len = kept;
        self.hair_leaves = hair_leaves;
        self.select_total = self.leaves[..self.len].iter().map(|l| l.select).sum();
    }

    /// The resolved leaves, for probes.
    pub fn leaves(&self) -> &[Prepared] {
        &self.leaves[..self.len]
    }

    /// The interior medium, for probes.
    pub fn medium(&self) -> Option<Medium> {
        self.medium
    }

    /// Whether any leaf transmits.
    pub fn transmits(&self) -> bool {
        self.transmits
    }

    /// Whether any leaf is a fibre, so the vertex's rays — the continuation
    /// and every shadow ray — pass out of curve tubes: the fibre model
    /// already accounts for light's path through its own strand. Where a
    /// fibre shares the vertex with a transmitting leaf
    /// ([`ResolvedClosure::mixes_hair`]), only the fibre's light passes.
    pub fn passes_out_of_curves(&self) -> bool {
        self.hair
    }

    /// Whether a fibre shares this vertex with a leaf that transmits (a
    /// refracting or translucent one). Light going into the tube then needs
    /// two answers to "is the way clear": the fibre's passes out of its own
    /// strand, the other leaf's meets the tube's far wall, as it would on
    /// its own.
    pub fn mixes_hair(&self) -> bool {
        self.hair && self.transmits
    }

    /// Bit `i` set when leaf `i` (in [`ResolvedClosure::eval_lobes`]'s order)
    /// is a fibre.
    pub fn hair_leaves(&self) -> u8 {
        self.hair_leaves
    }

    /// The share of leaf `i`'s term toward `wi` (in
    /// [`ResolvedClosure::eval_lobes`]'s order) that a light's
    /// `inputs:specular` scales, per channel: a fibre's R term
    /// ([`hair::Hair::r_fraction`]), or the whole leaf when its event's label
    /// is a specular one ([`LobeLabel::is_specular`]).
    ///
    /// [`LobeLabel::is_specular`]: crate::material::lobe::LobeLabel::is_specular
    pub fn specular_fraction(&self, i: usize, wi: Vec3A) -> Vec3A {
        let leaf = &self.leaves[i];
        let l = leaf.frame.to_local(wi.normalize());
        match &leaf.lobe {
            Lobe::Hair(h) => h.r_fraction(l),
            _ if leaf.event(l.z < 0.0).label.is_specular() => Vec3A::ONE,
            _ => Vec3A::ZERO,
        }
    }

    /// `eval`'s value toward `wi`, split into the fibres' share and the other
    /// leaves', each summed in leaf order.
    pub fn eval_hair_split(&self, wi: Vec3A) -> (Vec3A, Vec3A) {
        let wi = wi.normalize();
        let (mut hair, mut other) = (Vec3A::ZERO, Vec3A::ZERO);
        for (i, leaf) in self.leaves[..self.len].iter().enumerate() {
            let l = leaf.frame.to_local(wi);
            let term = leaf.weight * eval_lobe(&leaf.lobe, leaf.v, l).0 * l.z.abs();
            if self.hair_leaves & (1 << i) != 0 {
                hair += term;
            } else {
                other += term;
            }
        }
        (hair, other)
    }

    /// Walks the subtree `id` reached with `weight`, pushing its leaves, and
    /// returns its opacity toward ωo: testrender's `evaluate_layer_opacity`,
    /// linear in every weight and clamped only where a `layer` reads it.
    fn walk(
        &mut self,
        cl: &Closures,
        id: NodeId,
        weight: Vec3A,
        iface: Interface,
        w: &Walk<'_>,
    ) -> Vec3A {
        match &cl.nodes[id as usize] {
            Closure::Leaf(leaf) => {
                let own = w.slots[leaf.weight as usize].x().max(0.0);
                // A leaf at weight 0 adds neither response nor opacity.
                if own <= 0.0 {
                    return Vec3A::ZERO;
                }
                let (mut p, filter) = prepare(leaf, iface, w);
                let weight = weight * own;
                if weight.max_element() > 0.0 && self.len < MAX_LEAVES {
                    p.weight = weight;
                    p.select *= w.luma.of(weight);
                    if let Lobe::Specular { mode, .. } = p.lobe
                        && mode.transmits()
                    {
                        self.transmits = true;
                    }
                    if matches!(p.lobe, Lobe::Translucent { .. }) {
                        self.transmits = true;
                    }
                    if matches!(p.lobe, Lobe::Hair(_)) {
                        self.hair = true;
                        self.hair_leaves |= 1 << self.len;
                    }
                    self.leaves[self.len] = p;
                    self.len += 1;
                }
                // testrender: `weight · (1 − filter_o)`, which is `weight · E`
                // for a leaf that lets light through to a base and `weight`
                // for an opaque one (filter 0).
                own * (Vec3A::ONE - filter)
            }
            Closure::Layer { top, base } => {
                let first = self.len;
                let o_top = self.walk(cl, *top, weight, iface, w);
                let middle = self.len;
                // A dielectric over the base is what a random walk below it
                // is entered through.
                let under = interface_of(cl, *top, w.slots).unwrap_or(iface);
                let through = Vec3A::ONE - o_top.clamp(Vec3A::ZERO, Vec3A::ONE);
                let o_base = self.walk(cl, *base, weight * through, under, w);
                // A reflecting interface over another one is a coat — the
                // shape `standard_surface` and `open_pbr_surface` both expand
                // to. (A reflecting specular over a transmission-only one is
                // the base specular over its refraction, and stays specular.)
                let reflects =
                    |p: &Prepared| matches!(p.lobe, Lobe::Specular { mode, .. } if mode.reflects());
                if self.leaves[middle..self.len].iter().any(reflects) {
                    for p in &mut self.leaves[first..middle] {
                        if reflects(p) {
                            p.coat = true;
                        }
                    }
                }
                if self.leaves[middle..self.len].iter().any(|p| p.select > 0.0) {
                    for p in &mut self.leaves[first..middle] {
                        p.over_base = true;
                    }
                }
                // testrender's `evaluate_layer_opacity` sums a nested
                // layer's top and base, neither attenuated by the other.
                o_top + o_base
            }
            Closure::Mix { fg, bg, mix } => {
                let m = w.slots[*mix as usize].x().clamp(0.0, 1.0);
                let o_fg = self.walk(cl, *fg, weight * m, iface, w);
                let o_bg = self.walk(cl, *bg, weight * (1.0 - m), iface, w);
                o_fg * m + o_bg * (1.0 - m)
            }
            Closure::Add { a, b } => {
                let oa = self.walk(cl, *a, weight, iface, w);
                let ob = self.walk(cl, *b, weight, iface, w);
                oa + ob
            }
            Closure::Multiply { input, weight: k } => {
                let k = sanitize(w.slots[*k as usize].rgb()).min(Vec3A::ONE);
                k * self.walk(cl, *input, weight * k, iface, w)
            }
            // A pruned branch: nothing to shade, and nothing in the way.
            Closure::Empty => Vec3A::ZERO,
        }
    }

    /// Selection probability of leaf `i`.
    fn p(&self, i: usize) -> f32 {
        if self.select_total > 0.0 {
            self.leaves[i].select / self.select_total
        } else {
            1.0 / self.len as f32
        }
    }

    /// `Σ wᵢ·fᵢ·|cos|` and the mixture pdf toward `wi` — continuous lobes
    /// only; a delta lobe's selection mass is left out of the density,
    /// making it defective exactly as `OpenPBR`'s is.
    fn eval_pdf(&self, wi: Vec3A) -> (Vec3A, f32) {
        let mut value = Vec3A::ZERO;
        let mut pdf = 0.0;
        for i in 0..self.len {
            let leaf = &self.leaves[i];
            let l = leaf.frame.to_local(wi);
            let (f, p) = eval_lobe(&leaf.lobe, leaf.v, l);
            value += leaf.weight * f * l.z.abs();
            pdf += self.p(i) * p;
        }
        (value, pdf)
    }

    /// [`ResolvedClosure::eval`], split by leaf: each leaf's term of the
    /// sum `eval` computes, in the same order, so the shares add up to
    /// `eval`'s value bit for bit. `false` where `eval` answers `None`.
    pub fn eval_lobes(&self, wi: Vec3A, out: &mut crate::material::lobe::LobeSplit) -> bool {
        self.eval_lobes_at(wi.normalize(), out)
    }

    /// [`ResolvedClosure::eval_lobes`] toward an already normalised `wi`,
    /// normalised no further — the terms `scatter`'s own `eval_pdf(world)`
    /// sums for a continuous sample, so they sum to its value bit for bit.
    fn eval_lobes_at(&self, wi: Vec3A, out: &mut crate::material::lobe::LobeSplit) -> bool {
        out.clear();
        if self.len == 0 {
            return false;
        }
        for leaf in &self.leaves[..self.len] {
            let l = leaf.frame.to_local(wi);
            let (f, _) = eval_lobe(&leaf.lobe, leaf.v, l);
            out.push(leaf.event(l.z < 0.0), leaf.weight * f * l.z.abs());
        }
        true
    }

    /// [`ResolvedClosure::scatter`], also splitting a continuous sample's
    /// value by leaf at exactly the direction sampled (the ray's direction,
    /// which `scatter` stores as it evaluated it).
    pub fn scatter_split(
        &self,
        r_in: &Ray,
        rec: &HitRecord,
        draw: impl FnOnce() -> [f32; 4],
        out: &mut crate::material::lobe::LobeSplit,
    ) -> Option<ScatterSample> {
        out.clear();
        let (sample, choice) = self.scatter_choosing(r_in, rec, draw)?;
        if !sample.delta {
            self.eval_lobes_at(sample.ray.direction(), out);
            if let Some(c) = choice {
                out.scale_each(|i| c.scale(self.hair_leaves & (1 << i) != 0));
            }
        }
        Some(sample)
    }

    /// The event of a delta sample: a subsurface walk's entry, or a
    /// thin-walled interface passing straight through.
    pub fn delta_event(&self, sample: &ScatterSample) -> crate::material::lobe::LobeEvent {
        use crate::material::lobe::{LobeEvent, LobeLabel, Scatter};
        if sample.subsurface.is_some() {
            LobeEvent::transmit(Scatter::Diffuse, LobeLabel::Subsurface)
        } else {
            LobeEvent::transmit(Scatter::Singular, LobeLabel::Transmission)
        }
    }

    /// The diffuse colour the raw light AOVs divide by: the sum of the
    /// diffuse leaves' colour times their weight in the tree (which already
    /// carries the layering above them). Translucent and subsurface leaves
    /// transmit; they are not diffuse reflection.
    pub fn diffuse_filter(&self) -> Vec3A {
        self.leaves[..self.len]
            .iter()
            .filter_map(|leaf| match leaf.lobe {
                Lobe::Diffuse { color, .. } => Some(leaf.weight * color),
                _ => None,
            })
            .fold(Vec3A::ZERO, |a, x| a + x)
    }

    /// The albedo for denoising: each leaf's tint times its weight in the
    /// tree — a reflecting interface's tint at normal incidence — clamped to
    /// [0, 1].
    pub fn albedo(&self) -> Vec3A {
        let mut sum = Vec3A::ZERO;
        for leaf in &self.leaves[..self.len] {
            let tint = match leaf.lobe {
                Lobe::Diffuse { color, .. }
                | Lobe::Sheen { color, .. }
                | Lobe::Translucent { color }
                | Lobe::Subsurface { color, .. } => color,
                Lobe::Hair(h) => h.albedo(),
                Lobe::Specular {
                    fresnel,
                    refl_tint,
                    refr_tint,
                    mode,
                    ..
                } => {
                    if mode.reflects() {
                        refl_tint * fresnel.eval(1.0)
                    } else {
                        refr_tint
                    }
                }
            };
            sum += leaf.weight * tint;
        }
        sum.clamp(Vec3A::ZERO, Vec3A::ONE)
    }

    /// The shading normal the `normal` and `Neye` AOVs report: the normals
    /// of the leaves under every layer — each its MaterialX `normal` input
    /// where one is connected — averaged by their selection weights. A
    /// layer's top is left out while a weighted leaf sits under it, so a
    /// coat's `coat_normal` never reaches the pass, as UsdPreviewSurface's
    /// single normal map is the base's; `standard_surface`'s base specular
    /// shares the base's `normal`, so leaving it out loses nothing. `None`
    /// when no leaf has weight.
    pub fn shading_normal(&self) -> Option<Vec3A> {
        let leaves = &self.leaves[..self.len];
        let base = leaves.iter().any(|l| !l.over_base && l.select > 0.0);
        let n = leaves
            .iter()
            .filter(|l| !(base && l.over_base))
            .fold(Vec3A::ZERO, |a, l| a + l.frame.n * l.select.max(0.0));
        (n.is_finite() && n.length_squared() > 1e-12).then(|| n.normalize())
    }

    /// [`crate::Material::eval`].
    pub fn eval(&self, _r_in: &Ray, _rec: &HitRecord, wi: Vec3A) -> Option<(Vec3A, f32)> {
        if self.len == 0 {
            return None;
        }
        let (value, pdf) = self.eval_pdf(wi.normalize());
        Some((value, pdf.max(1e-4)))
    }

    /// [`crate::Material::scatter_importance`], from the four uniforms
    /// `draw` produces when called.
    pub fn scatter(
        &self,
        r_in: &Ray,
        rec: &HitRecord,
        draw: impl FnOnce() -> [f32; 4],
    ) -> Option<ScatterSample> {
        self.scatter_choosing(r_in, rec, draw).map(|(s, _)| s)
    }

    /// [`ResolvedClosure::scatter`], and — where a fibre shares the vertex
    /// with a transmitting leaf and the sample goes into the tube — which of
    /// the two the continuation ray carries. One ray cannot both pass out of
    /// the strand (the fibre's light) and meet its far wall (the other
    /// leaf's), so it carries one, picked in proportion to the two shares of
    /// the value toward the sampled direction, and the value is that share
    /// over its probability: unbiased, as a one-sample estimate of the sum
    /// of the two.
    fn scatter_choosing(
        &self,
        _r_in: &Ray,
        rec: &HitRecord,
        draw: impl FnOnce() -> [f32; 4],
    ) -> Option<(ScatterSample, Option<Choice>)> {
        if self.len == 0 {
            return None;
        }
        // One 4D block: `s[0]` picks the leaf, `s[1..3]` the direction and
        // `s[3]` a leaf's own discrete choice (reflect or refract). Drawn
        // here, after the empty-tree return.
        let s = draw();
        let mut u = s[0] * self.select_total.max(0.0);
        let mut pick = self.len - 1;
        // Where in its own slice of `s[0]` the pick landed: a fresh uniform
        // number, for the choice a mixed fibre vertex makes.
        let mut residual = 0.5;
        if self.select_total > 0.0 {
            for (i, l) in self.leaves[..self.len].iter().enumerate() {
                if u < l.select {
                    pick = i;
                    residual = u / l.select;
                    break;
                }
                u -= l.select;
            }
        } else {
            pick = ((s[0] * self.len as f32) as usize).min(self.len - 1);
            residual = (s[0] * self.len as f32).fract();
        }
        let leaf = &self.leaves[pick];
        match sample_lobe(&leaf.lobe, leaf.v, [s[1], s[2]], s[3])? {
            LobeSample::Subsurface { dir } => {
                let world = leaf.frame.to_world(dir).normalize();
                // Inward in the leaf's frame can still be outward through the
                // true face when a normal map tilts the frame hard; such an
                // entry is refused rather than redirected.
                if rec.normal.dot(world) >= 0.0 {
                    return None;
                }
                let p = self.p(pick).max(1e-4);
                Some((
                    ScatterSample {
                        ray: Ray::new(rec.p + world * 1e-4, world),
                        value: leaf.weight / p,
                        pdf: 1.0,
                        delta: true,
                        spread: crate::RayCone::MAX_SPREAD,
                        subsurface: Some(pick as u8),
                    },
                    None,
                ))
            }
            // A delta lobe is never a fibre's: its ray meets the tube as any
            // non-fibre ray does.
            LobeSample::Delta { dir, value } => {
                let world = leaf.frame.to_world(dir);
                let p = self.p(pick).max(1e-4);
                Some((
                    ScatterSample {
                        ray: self.ray(rec, world, false),
                        value: leaf.weight * value / p,
                        pdf: 1.0,
                        delta: true,
                        spread: 0.0,
                        subsurface: None,
                    },
                    None,
                ))
            }
            LobeSample::Continuous { dir, spread } => {
                let world = leaf.frame.to_world(dir).normalize();
                let (value, pdf) = self.eval_pdf(world);
                let (value, pass, choice) = if self.mixes_hair() && rec.normal.dot(world) < 0.0 {
                    let c = self.choose(world, residual);
                    (c.value, c.hair, Some(c))
                } else {
                    (value, self.hair, None)
                };
                Some((
                    ScatterSample {
                        ray: self.ray(rec, world, pass),
                        value,
                        pdf: pdf.max(1e-4),
                        delta: false,
                        spread,
                        subsurface: None,
                    },
                    choice,
                ))
            }
        }
    }

    /// Which share a mixed fibre vertex's continuation carries toward `wi`
    /// (see [`ResolvedClosure::scatter_choosing`]), `u` uniform in [0, 1).
    fn choose(&self, wi: Vec3A, u: f32) -> Choice {
        let (hair, other) = self.eval_hair_split(wi);
        let (h, o) = (hair.element_sum().max(0.0), other.element_sum().max(0.0));
        let p_hair = if h + o > 0.0 { h / (h + o) } else { 0.5 };
        let hair = u < p_hair;
        let c = Choice {
            hair,
            inv_p: if hair {
                1.0 / p_hair
            } else {
                1.0 / (1.0 - p_hair)
            },
            value: Vec3A::ZERO,
        };
        // Summed as `eval_lobes` splits it, each chosen share scaled, so the
        // split `scatter_split` reports adds up to this value bit for bit.
        let mut value = Vec3A::ZERO;
        for (i, leaf) in self.leaves[..self.len].iter().enumerate() {
            let l = leaf.frame.to_local(wi);
            let term = leaf.weight * eval_lobe(&leaf.lobe, leaf.v, l).0 * l.z.abs();
            value += term * c.scale(self.hair_leaves & (1 << i) != 0);
        }
        Choice { value, ..c }
    }

    /// The continuation ray toward `wi`: offset through the surface, and
    /// carrying the interior medium, when it refracts into a thick surface.
    /// `pass` makes it pass out of curve tubes: it carries a fibre's light.
    fn ray(&self, rec: &HitRecord, wi: Vec3A, pass: bool) -> Ray {
        if self.transmits && rec.normal.dot(wi) < 0.0 {
            if rec.front_face
                && let Some(m) = self.medium
            {
                return Ray::new_in_medium(rec.p + wi * 1e-4, wi, m).with_curve_exits_ignored(pass);
            }
            return Ray::new(rec.p + wi * 1e-4, wi).with_curve_exits_ignored(pass);
        }
        Ray::new(rec.p, wi).with_curve_exits_ignored(pass)
    }

    /// The walk leaf `index` enters toward `dir` — see
    /// [`crate::ScatterSample::subsurface`].
    pub fn subsurface_entry(&self, index: u8, dir: Vec3A) -> Option<SubsurfaceEntry> {
        match self.leaves[..self.len].get(index as usize)?.lobe {
            Lobe::Subsurface {
                color,
                radius,
                anisotropy,
                ..
            } => Some(SubsurfaceEntry {
                dir,
                albedo: color,
                radius,
                anisotropy,
            }),
            _ => None,
        }
    }

    /// [`crate::Material::make_ray`].
    pub fn make_ray(&self, rec: &HitRecord, wi: Vec3A) -> Ray {
        // A guided direction carries the whole value: a mixed fibre vertex
        // is not guided (the tracer turns the guide off there), so this is
        // all-fibre or no fibre.
        self.ray(rec, wi, self.hair)
    }
}

/// The share a mixed fibre vertex's continuation ray carries: the fibres'
/// (`hair`) or the other leaves', with the inverse of its probability and the
/// value it scales to.
#[derive(Clone, Copy, Debug)]
struct Choice {
    hair: bool,
    inv_p: f32,
    value: Vec3A,
}

impl Choice {
    /// The factor on a leaf's share: the chosen side's `1/p`, else 0.
    fn scale(&self, leaf_is_hair: bool) -> f32 {
        if leaf_is_hair == self.hair {
            self.inv_p
        } else {
            0.0
        }
    }
}

/// The interior medium `closures.volume` describes at the evaluated `slots`:
/// coefficients sanitised to finite and non-negative, anisotropy clamped
/// inside (−1, 1), and `None` for vacuum. The one mapping, for a thick
/// surface's interior and a volume-only boundary alike.
pub(crate) fn volume_medium(closures: &Closures, slots: &[Val]) -> Option<Medium> {
    closures.volume.and_then(|v| {
        let g = slots[v.anisotropy as usize].x();
        let m = Medium {
            sigma_a: sanitize(slots[v.absorption as usize].rgb()),
            sigma_s: sanitize(slots[v.scattering as usize].rgb()),
            g: if g.is_finite() {
                g.clamp(-0.999, 0.999)
            } else {
                0.0
            },
        };
        (m.sigma_t_max() > 1e-6).then_some(m)
    })
}

/// Finite and non-negative, per channel.
fn sanitize(v: Vec3A) -> Vec3A {
    let f = |x: f32| if x.is_finite() { x.max(0.0) } else { 0.0 };
    Vec3A::new(f(v.x), f(v.y), f(v.z))
}

/// A `vector2` alpha (or a broadcast `float`), floored.
fn alphas(v: Val) -> (f32, f32) {
    let ax = v.v[0];
    let ay = if v.arity >= 2 { v.v[1] } else { v.v[0] };
    let f = |a: f32| {
        if a.is_finite() {
            a.clamp(MIN_ALPHA, 1.0)
        } else {
            1.0
        }
    };
    (f(ax), f(ay))
}

/// Builds a leaf's lobe and frame at the vertex, and returns its layering
/// filter: BSDL's `filter_o(ωo)`, what the leaf lets through to a layer's
/// base (0 for an opaque leaf).
fn prepare(leaf: &crust_mtlx::Leaf, iface: Interface, w: &Walk<'_>) -> (Prepared, Vec3A) {
    let s = |i: u32| w.slots[i as usize];
    let n = leaf
        .normal
        .map(|i| s(i).rgb())
        .filter(|n| n.is_finite() && n.length_squared() > 1e-12)
        .map(|n| n.normalize())
        .filter(|n| n.dot(w.rec.normal) > 1e-3)
        .unwrap_or(w.rec.normal);
    let tangent = leaf
        .tangent
        .map(|i| s(i).rgb())
        .filter(|t| t.is_finite())
        .unwrap_or(w.rec.tangent);
    let mut frame = Frame::new(n, tangent);
    if let Some(r) = leaf.rotation {
        frame = frame.rotated(s(r).x());
    }
    let v = frame.to_local(w.v_world);
    let nv = v.z.clamp(1e-6, 1.0);
    let cx = LeafInputs { w, iface, nv, v };
    let (lobe, albedo, filter) = match &leaf.bsdf {
        Bsdf::Diffuse {
            model,
            color,
            roughness,
        } => prepare_diffuse(&cx, model, color, roughness),
        Bsdf::Subsurface {
            color,
            radius,
            anisotropy,
        } => prepare_subsurface(&cx, color, radius, anisotropy),
        Bsdf::Hair { .. } => prepare_hair(&cx, &leaf.bsdf),
        Bsdf::Translucent { color } => prepare_translucent(&cx, color),
        Bsdf::Sheen {
            color,
            roughness,
            mode,
        } => prepare_sheen(&cx, color, roughness, *mode),
        Bsdf::Dielectric {
            tint,
            ior,
            roughness,
            mode,
            thin_film,
            ..
        } => prepare_dielectric(&cx, tint, ior, roughness, mode, thin_film),
        Bsdf::Conductor {
            ior,
            extinction,
            roughness,
            thin_film,
        } => prepare_conductor(&cx, ior, extinction, roughness, thin_film),
        Bsdf::Schlick { .. } => prepare_schlick(&cx, &leaf.bsdf),
    };
    (
        Prepared {
            lobe,
            weight: Vec3A::ZERO,
            frame,
            v,
            select: w.luma.of(albedo).max(0.02),
            albedo,
            category: leaf.bsdf.category(),
            coat: false,
            over_base: false,
        },
        filter,
    )
}

/// A leaf's lobe, the albedo estimate it is selected by (its luminance,
/// floored at 0.02), and its layering filter — what each `prepare_*` builds.
type Built = (Lobe, Vec3A, Vec3A);

/// What every `prepare_*` reads: the walk's slots and hit, the interface a
/// subsurface leaf is entered through, and the view direction in the leaf's
/// frame with its clamped cosine.
struct LeafInputs<'a> {
    w: &'a Walk<'a>,
    iface: Interface,
    nv: f32,
    v: Vec3A,
}

impl LeafInputs<'_> {
    fn s(&self, i: u32) -> Val {
        self.w.slots[i as usize]
    }

    fn rgb(&self, i: u32) -> Vec3A {
        sanitize(self.s(i).rgb())
    }

    /// A thin film's `(thickness, ior)`, when it has any thickness.
    fn film(&self, tf: &Option<crust_mtlx::ThinFilm>) -> Option<(f32, f32)> {
        tf.map(|t| (self.s(t.thickness).x().max(0.0), self.s(t.ior).x().max(1.0)))
            .filter(|(d, _)| *d > 0.0)
    }

    /// Whether the ray met the back face: BSDL's `backfacing`.
    fn back(&self) -> bool {
        !self.w.rec.front_face
    }
}

/// [`prepare`] for a `Diffuse` leaf: opaque, selected by its colour (BSDL's
/// `albedo_impl`).
#[inline(always)]
fn prepare_diffuse(
    cx: &LeafInputs<'_>,
    model: &DiffuseModel,
    color: &u32,
    roughness: &u32,
) -> Built {
    let color = cx.rgb(*color);
    let r = cx.s(*roughness).x().clamp(0.0, 1.0);
    (
        Lobe::Diffuse {
            model: *model,
            color,
            roughness: r,
        },
        color,
        Vec3A::ZERO,
    )
}

/// [`prepare`] for a `Subsurface` leaf.
#[inline(always)]
fn prepare_subsurface(cx: &LeafInputs<'_>, color: &u32, radius: &u32, anisotropy: &u32) -> Built {
    let color = cx.rgb(*color).min(Vec3A::ONE);
    // `radius` is a vector3; a float broadcasts.
    let r = cx.s(*radius);
    let radius = sanitize(if r.arity >= 3 {
        r.rgb()
    } else {
        Vec3A::splat(r.x())
    });
    let lobe = if radius.max_element() > 0.0 {
        let anisotropy = cx.s(*anisotropy).x();
        Lobe::Subsurface {
            color,
            radius,
            anisotropy: if anisotropy.is_finite() {
                anisotropy.clamp(-0.99, 0.99)
            } else {
                0.0
            },
            ior: cx.iface.ior,
            alpha: cx.iface.alpha,
        }
    } else {
        // A zero mean free path exits where it entered: a diffuse in
        // the subsurface colour, which is also what MaterialX's GLSL
        // renders it as.
        Lobe::Diffuse {
            model: DiffuseModel::OrenNayar,
            color,
            roughness: 0.0,
        }
    };
    (lobe, color, Vec3A::ZERO)
}

/// [`prepare`] for a `Hair` leaf.
#[inline(always)]
fn prepare_hair(cx: &LeafInputs<'_>, bsdf: &Bsdf) -> Built {
    let Bsdf::Hair {
        tint_r,
        tint_tt,
        tint_trt,
        ior,
        roughness_r,
        roughness_tt,
        roughness_trt,
        cuticle_angle,
        absorption,
    } = bsdf
    else {
        unreachable!("prepare_hair is handed a Hair leaf")
    };
    // A `vector2` (variance, scale); a `float` broadcasts.
    let pair = |i: u32| {
        let r = cx.s(i);
        if r.arity >= 2 {
            (r.v[0], r.v[1])
        } else {
            (r.x(), r.x())
        }
    };
    let hair = hair::Hair::new(
        &hair::HairParams {
            tint: [cx.rgb(*tint_r), cx.rgb(*tint_tt), cx.rgb(*tint_trt)],
            ior: cx.s(*ior).x(),
            roughness: [
                pair(*roughness_r),
                pair(*roughness_tt),
                pair(*roughness_trt),
            ],
            cuticle_angle: cx.s(*cuticle_angle).x(),
            absorption: cx.rgb(*absorption),
        },
        cx.v.normalize_or_zero(),
        |c| cx.w.luma.of(c),
    );
    let albedo = hair.albedo();
    // Opaque over a base, as testrender takes every lobe it has no filter
    // for (genosl has no fibre).
    (Lobe::Hair(hair), albedo, Vec3A::ZERO)
}

/// [`prepare`] for a `Translucent` leaf.
#[inline(always)]
fn prepare_translucent(cx: &LeafInputs<'_>, color: &u32) -> Built {
    let color = cx.rgb(*color);
    (Lobe::Translucent { color }, color, Vec3A::ZERO)
}

/// [`prepare`] for a `Sheen` leaf: BSDL's `SheenLobe`.
///
/// Its directional albedo `E(cosθo)` is the mode's own — MaterialX's rational
/// fit for `conty_kulla`, the LTC table's `R` for `zeltner` — and its filter
/// is `1 − min(E · max(color), 1)`. A back face is not shaded and lets
/// everything through.
#[inline(always)]
fn prepare_sheen(cx: &LeafInputs<'_>, color: &u32, roughness: &u32, mode: SheenMode) -> Built {
    let color = cx.rgb(*color);
    let r = cx.s(*roughness).x().clamp(0.0, 1.0);
    let (alpha, e, zeltner) = match mode {
        SheenMode::ContyKulla => {
            let alpha = r.max(bsdl::CONTY_KULLA_MIN_ROUGHNESS);
            (alpha, bsdl::conty_kulla_sheen_albedo(alpha, cx.nv), None)
        }
        SheenMode::Zeltner => {
            let alpha = r.sqrt().max(bsdl::ZELTNER_MIN_ROUGHNESS);
            let lobe = ZeltnerSheen::new(alpha, cx.nv);
            (alpha, lobe.albedo(), Some(lobe))
        }
    };
    let (color, emiss) = if cx.back() {
        (Vec3A::ZERO, 1.0)
    } else {
        (color, bsdl::sheen_emiss(e, color))
    };
    (
        Lobe::Sheen {
            color,
            roughness: alpha,
            zeltner,
        },
        color * (1.0 - emiss),
        Vec3A::splat(emiss),
    )
}

/// The energy terms BSDL's `DielectricLobe` and `SchlickLobe` read at the
/// view: Turquin's `E_ms` and the filter for a leaf that only reflects, the
/// renormalisation for one that refracts, and the albedo it is selected by.
/// `r` is BSDL's roughness, `eta` the ray-facing relative IOR.
fn interface_energy(
    cx: &LeafInputs<'_>,
    r: f32,
    eta: f32,
    refl_tint: Vec3A,
    refr_tint: Vec3A,
) -> (f32, f32, Vec3A, Vec3A) {
    let fi = bsdl::table_index(eta);
    let refl = || TabulatedEnergyCurve::new(bsdl::REFL_FRONT, r, fi).emiss_eval(cx.nv);
    if refr_tint.max_element() > 0.0 {
        let both = if cx.back() {
            bsdl::BOTH_BACK
        } else {
            bsdl::BOTH_FRONT
        };
        // A thin wall transmits straight through, losing nothing to
        // shadowing: BSDL's renormalisation is the rough refraction's.
        let renorm = if cx.w.thin_walled {
            1.0
        } else {
            1.0 / (1.0 - TabulatedEnergyCurve::new(both, r, fi).emiss_eval(cx.nv)).max(0.01)
        };
        // BSDL's `albedo_impl` is 1 for a refracting lobe; split by the
        // reflected share, so a tint steers the selection.
        let e_r = 1.0 - refl();
        let albedo = refl_tint * e_r + refr_tint * (1.0 - e_r);
        (0.0, renorm, Vec3A::ZERO, albedo)
    } else {
        let e_ms = TabulatedEnergyCurve::new(bsdl::GGX, r, 0.0).emiss_eval(cx.nv);
        // BSDL computes the filter only for a lobe that reflects something:
        // a black one is opaque.
        let filter = if refl_tint.max_element() > 0.0 {
            refl()
        } else {
            0.0
        };
        (e_ms, 1.0, Vec3A::splat(filter), refl_tint * (1.0 - filter))
    }
}

/// [`prepare`] for a `Dielectric` leaf: BSDL's `DielectricLobe`.
#[inline(always)]
fn prepare_dielectric(
    cx: &LeafInputs<'_>,
    tint: &u32,
    ior: &u32,
    roughness: &u32,
    mode: &ScatterMode,
    thin_film: &Option<crust_mtlx::ThinFilm>,
) -> Built {
    let ior = cx.s(*ior).x();
    let ior = if ior.is_finite() && ior > 0.0 {
        ior.clamp(bsdl::IOR_MIN, bsdl::IOR_MAX)
    } else {
        1.5
    };
    let (rx, ry) = alphas(cx.s(*roughness));
    let (dist, r) = GGXDist::from_alphas(rx, ry);
    let tint = cx.rgb(*tint);
    // genosl's split: `T` reflects nothing, `R` transmits nothing.
    let refl_tint = if *mode == ScatterMode::T {
        Vec3A::ZERO
    } else {
        tint
    };
    let refr_tint = if *mode == ScatterMode::R {
        Vec3A::ZERO
    } else {
        tint
    };
    // In the ray-facing frame: entering the interior from the front,
    // leaving it from the back.
    let eta = bsdl::dielectric_eta(ior, cx.back());
    let fresnel = Fresnel {
        model: FresnelModel::Dielectric { ior: eta },
        thin_film: cx.film(thin_film),
    };
    let refr = if mode.transmits() {
        refr_tint
    } else {
        Vec3A::ZERO
    };
    let (e_ms, renorm, filter, albedo) = interface_energy(cx, r, eta, refl_tint, refr);
    (
        Lobe::Specular {
            fresnel,
            refl_tint,
            refr_tint,
            ax: dist.ax,
            ay: dist.ay,
            mode: *mode,
            eta,
            e_ms,
            renorm,
            thin_walled: cx.w.thin_walled,
        },
        albedo,
        filter,
    )
}

/// [`prepare`] for a `Conductor` leaf: BSDL's `ConductorLobe`, opaque.
#[inline(always)]
fn prepare_conductor(
    cx: &LeafInputs<'_>,
    ior: &u32,
    extinction: &u32,
    roughness: &u32,
    thin_film: &Option<crust_mtlx::ThinFilm>,
) -> Built {
    let (rx, ry) = alphas(cx.s(*roughness));
    let (dist, r) = GGXDist::from_alphas(rx, ry);
    let fresnel = Fresnel {
        model: FresnelModel::Conductor {
            n: cx.rgb(*ior),
            k: cx.rgb(*extinction),
        },
        thin_film: cx.film(thin_film),
    };
    let e_ms = TabulatedEnergyCurve::new(bsdl::GGX, r, 0.0).emiss_eval(cx.nv);
    // BSDL's `albedo_impl` is a rough fit to the average Fresnel, for
    // selection; this is the compensated lobe's albedo toward this view,
    // `E_ss·F·(1 + F·E_ms/E_ss)` with `E_ss = 1 − E_ms`.
    let f = fresnel.eval(cx.nv);
    let albedo = f * (1.0 - e_ms) + f * f * e_ms;
    (
        Lobe::Specular {
            fresnel,
            refl_tint: Vec3A::ONE,
            refr_tint: Vec3A::ZERO,
            ax: dist.ax,
            ay: dist.ay,
            mode: ScatterMode::R,
            eta: 1.0,
            e_ms,
            renorm: 1.0,
            thin_walled: false,
        },
        albedo,
        Vec3A::ZERO,
    )
}

/// [`prepare`] for a `Schlick` leaf: BSDL's `SchlickLobe`, plus the F82
/// tint BSDL does not apply.
#[inline(always)]
fn prepare_schlick(cx: &LeafInputs<'_>, bsdf: &Bsdf) -> Built {
    let Bsdf::Schlick {
        color0,
        color82,
        color90,
        exponent,
        roughness,
        mode,
        thin_film,
    } = bsdf
    else {
        unreachable!("prepare_schlick is handed a Schlick leaf")
    };
    let (rx, ry) = alphas(cx.s(*roughness));
    let (dist, r) = GGXDist::from_alphas(rx, ry);
    let f0 = cx.rgb(*color0);
    // The refraction IOR, and the tables' index, from the average F0.
    let eta = bsdl::dielectric_eta(bsdl::schlick_ior(f0), cx.back());
    let fresnel = Fresnel {
        model: FresnelModel::Schlick {
            f0: f0.min(Vec3A::ONE),
            f82: cx.rgb(*color82),
            f90: cx.rgb(*color90).min(Vec3A::ONE),
            exponent: cx.s(*exponent).x().max(0.0),
            tir_cos: bsdl::schlick_tir_cos(eta),
        },
        thin_film: cx.film(thin_film),
    };
    let refl_tint = if *mode == ScatterMode::T {
        Vec3A::ZERO
    } else {
        Vec3A::ONE
    };
    let refr_tint = if mode.transmits() {
        Vec3A::ONE
    } else {
        Vec3A::ZERO
    };
    let (e_ms, renorm, filter, albedo) = interface_energy(cx, r, eta, refl_tint, refr_tint);
    (
        Lobe::Specular {
            fresnel,
            refl_tint,
            refr_tint,
            ax: dist.ax,
            ay: dist.ay,
            mode: *mode,
            eta,
            e_ms,
            renorm,
            thin_walled: cx.w.thin_walled,
        },
        albedo,
        filter,
    )
}

/// BSDL's dielectric reflection filter `1 − E_R(cosθo)` — the
/// `DielectricReflFront` table — at BSDL roughness `r` (`α = r²`) and IOR
/// `ior` (or its inverse, which reads the same row).
pub fn dielectric_refl_filter(cos_o: f32, r: f32, ior: f32) -> f32 {
    TabulatedEnergyCurve::new(bsdl::REFL_FRONT, r, bsdl::table_index(ior))
        .emiss_eval(cos_o.max(0.0))
}

enum LobeSample {
    Continuous {
        dir: Vec3A,
        spread: f32,
    },
    Delta {
        dir: Vec3A,
        value: Vec3A,
    },
    /// The direction into a random walk.
    Subsurface {
        dir: Vec3A,
    },
}

/// The view a leaf's lobes are evaluated and sampled toward: `v` lifted to
/// at least 1e-6 above the leaf's plane, which a normal map can tilt it under.
fn lifted(v: Vec3A) -> Vec3A {
    Vec3A::new(v.x, v.y, v.z.max(1e-6)).normalize()
}

/// `(f, pdf)` of a lobe for local `v`, `l`: the BSDF value without the cosine,
/// and the lobe's own continuous sampling density.
fn eval_lobe(lobe: &Lobe, v: Vec3A, l: Vec3A) -> (Vec3A, f32) {
    match *lobe {
        Lobe::Diffuse {
            model,
            color,
            roughness,
        } => {
            if l.z <= 0.0 {
                return (Vec3A::ZERO, 0.0);
            }
            let v = lifted(v);
            let f = match model {
                DiffuseModel::Eon => bsdl::oren_nayar(color, roughness, true, v, l),
                DiffuseModel::OrenNayar => bsdl::oren_nayar(color, roughness, false, v, l),
                DiffuseModel::Burley => bsdl::burley(color, roughness, v, l),
            };
            (f, l.z * FRAC_1_PI)
        }
        Lobe::Translucent { color } => {
            if l.z >= 0.0 {
                return (Vec3A::ZERO, 0.0);
            }
            (color * FRAC_1_PI, -l.z * FRAC_1_PI)
        }
        // Light reaches a walk only through its entry, never toward a
        // direction: no value and no continuous density.
        Lobe::Subsurface { .. } => (Vec3A::ZERO, 0.0),
        Lobe::Hair(ref h) => h.eval(l),
        Lobe::Sheen {
            color,
            zeltner: Some(lobe),
            ..
        } => {
            if l.z <= 0.0 {
                return (Vec3A::ZERO, 0.0);
            }
            // The LTC carries the cosine; the value here goes without it.
            let d = lobe.density(v, l);
            (color * (lobe.albedo() * d / l.z), d)
        }
        Lobe::Sheen {
            color,
            roughness,
            zeltner: None,
        } => {
            if l.z <= 0.0 {
                return (Vec3A::ZERO, 0.0);
            }
            let f = bsdl::conty_kulla_sheen(roughness, lifted(v), l);
            (color * f, l.z * FRAC_1_PI)
        }
        Lobe::Specular {
            fresnel,
            refl_tint,
            refr_tint,
            ax,
            ay,
            mode,
            eta,
            e_ms,
            renorm,
            thin_walled,
        } => {
            let v = lifted(v);
            let dist = GGXDist { ax, ay };
            let fresnel = |c: f32| fresnel.eval(c);
            if l.z > 0.0 {
                if !mode.reflects() {
                    return (Vec3A::ZERO, 0.0);
                }
                let h = (v + l).normalize_or_zero();
                if h.z <= 0.0 {
                    return (Vec3A::ZERO, 0.0);
                }
                // BSDL: Turquin's compensation when the lobe only reflects,
                // the renormalised single scattering when it also refracts.
                let value = if mode.transmits() {
                    refl_tint * renorm * bsdl::eval_dielectric_reflection(&dist, fresnel, v, l)
                } else {
                    refl_tint * bsdl::eval_turquin_microms_reflection(&dist, fresnel, e_ms, v, l)
                };
                let mut pdf = pdf_vndf_ggx_aniso_local(v, h, ax, ay);
                if mode == ScatterMode::RT {
                    pdf *= scalar(fresnel(v.dot(h).clamp(1e-6, 1.0)));
                }
                (value, pdf)
            } else if mode.transmits() && !thin_walled && l.z < 0.0 {
                let (p_h, vh) = refraction_pdf(v, l, eta, ax, ay);
                if p_h <= 0.0 {
                    return (Vec3A::ZERO, 0.0);
                }
                let value = refr_tint
                    * renorm
                    * bsdl::eval_dielectric_refraction(&dist, fresnel, eta, v, l);
                // `RT` samples refraction with probability `1 − F`.
                let pdf = match mode {
                    ScatterMode::RT => p_h * (1.0 - scalar(fresnel(vh))),
                    _ => p_h,
                };
                (value, pdf)
            } else {
                (Vec3A::ZERO, 0.0)
            }
        }
    }
}

/// A Fresnel colour as the scalar probability of reflecting.
fn scalar(f: Vec3A) -> f32 {
    ((f.x + f.y + f.z) / 3.0).clamp(0.0, 1.0)
}

/// The density crust's VNDF sampler draws the refracted direction `l` with
/// (`h` from the VNDF, `l` refracted through it), and `v·h` at that half
/// vector; `(0, 0)` where `l` is no refraction of `v`. `eta` is `η_t / η_i`.
fn refraction_pdf(v: Vec3A, l: Vec3A, eta: f32, ax: f32, ay: f32) -> (f32, f32) {
    let mut h = -(v + l * eta);
    if h.length_squared() < 1e-12 {
        return (0.0, 0.0);
    }
    h = h.normalize();
    if h.z < 0.0 {
        h = -h;
    }
    let vh = v.dot(h);
    let lh = l.dot(h);
    if vh <= 1e-6 || lh >= -1e-6 {
        return (0.0, 0.0);
    }
    let denom = vh + eta * lh;
    let denom2 = denom * denom;
    if denom2 < 1e-10 {
        return (0.0, 0.0);
    }
    let jacobian = eta * eta * -lh / denom2;
    (pdf_vndf_h_aniso_local(v, h, ax, ay) * jacobian, vh)
}

/// Samples a direction from one lobe.
fn sample_lobe(lobe: &Lobe, v: Vec3A, uv: [f32; 2], u: f32) -> Option<LobeSample> {
    match *lobe {
        Lobe::Diffuse { .. } | Lobe::Sheen { zeltner: None, .. } => Some(LobeSample::Continuous {
            dir: cosine_hemisphere(uv),
            spread: crate::RayCone::MAX_SPREAD,
        }),
        Lobe::Sheen {
            zeltner: Some(lobe),
            ..
        } => lobe.sample(v, uv).map(|dir| LobeSample::Continuous {
            dir,
            spread: crate::RayCone::MAX_SPREAD,
        }),
        Lobe::Translucent { .. } => {
            let d = cosine_hemisphere(uv);
            Some(LobeSample::Continuous {
                dir: Vec3A::new(d.x, d.y, -d.z),
                spread: crate::RayCone::MAX_SPREAD,
            })
        }
        Lobe::Hair(ref h) => h.sample(uv, u).map(|dir| LobeSample::Continuous {
            dir,
            spread: h.spread().min(crate::RayCone::MAX_SPREAD),
        }),
        Lobe::Subsurface { ior, alpha, .. } => {
            subsurface_entry(v, ior, alpha, uv).map(|dir| LobeSample::Subsurface { dir })
        }
        Lobe::Specular {
            fresnel,
            refr_tint,
            ax,
            ay,
            mode,
            eta,
            thin_walled,
            ..
        } => {
            let spread = (ax + ay).min(crate::RayCone::MAX_SPREAD);
            let v = lifted(v);
            let h = sample_vndf_ggx_aniso_local(v, ax, ay, uv);
            let vh = v.dot(h);
            if vh <= 1e-6 {
                return None;
            }
            let reflect = match mode {
                ScatterMode::R => true,
                ScatterMode::T => false,
                // The same film-aware Fresnel `eval_lobe`'s pdf uses.
                ScatterMode::RT => u < scalar(fresnel.eval(vh)),
            };
            if reflect {
                let l = 2.0 * vh * h - v;
                return (l.z > 0.0).then_some(LobeSample::Continuous { dir: l, spread });
            }
            if thin_walled {
                // Straight through: a thin sheet's two refractions cancel.
                // Both modes pay `(1 − F)`: `RT` by the selection above
                // (so the estimator divides it out), `T` in the value.
                let value = match mode {
                    ScatterMode::RT => refr_tint,
                    _ => refr_tint * (1.0 - scalar(fresnel.eval(vh))),
                };
                return Some(LobeSample::Delta { dir: -v, value });
            }
            let eta_rel = 1.0 / eta;
            let sin2_t = eta_rel * eta_rel * (1.0 - vh * vh);
            if sin2_t >= 1.0 {
                return None;
            }
            let cos_t = (1.0 - sin2_t).sqrt();
            let l = (-v * eta_rel + h * (eta_rel * vh - cos_t)).normalize();
            (l.z < -1e-6).then_some(LobeSample::Continuous { dir: l, spread })
        }
    }
}

/// The entry direction of a random walk, in the leaf's frame — Typhoon's
/// `Bsdf::SampleSubsurfaceEntry`: refraction through the interface at
/// `ior` (never below 1, so there is no total internal reflection going in),
/// about the normal when smooth and about a GGX VNDF microfacet normal
/// otherwise. Only a direction: the interface's energy is the layer's
/// throughput above the leaf, already in its weight.
fn subsurface_entry(v: Vec3A, ior: f32, alpha: f32, uv: [f32; 2]) -> Option<Vec3A> {
    if v.z <= 0.0 {
        return None;
    }
    let eta = 1.0 / ior.max(1.0);
    let h = if alpha < 1e-3 {
        Vec3A::Z
    } else {
        sample_vndf_ggx_aniso_local(v.normalize(), alpha, alpha, uv)
    };
    let cos_i = v.dot(h);
    let sin2_t = eta * eta * (1.0 - cos_i * cos_i);
    let l = if sin2_t >= 1.0 {
        -v
    } else {
        let cos_t = (1.0 - sin2_t).sqrt();
        -v * eta + h * (eta * cos_i - cos_t)
    };
    (l.z < 0.0).then(|| l.normalize())
}

#[cfg(test)]
mod tests;
