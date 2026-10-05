//! The MaterialX closure-tree evaluator.
//!
//! A MaterialX material compiles (`crust-mtlx`) to a tree of BSDF leaves
//! combined by `layer`, `mix`, `add` and `multiply`, with MaterialX's own
//! semantics: `layer(top, base) = top + base · T_top(ωo)`. The throughput
//! `T_top` depends on the outgoing direction alone, and ωo is fixed at a path
//! vertex, so there the tree is *exactly* a weighted sum of its leaves:
//!
//! - `mix` scales its branches by `m` and `1 − m`,
//! - `multiply` scales its input (and passes its throughput through),
//! - `add` concatenates, with throughput `max(T_a + T_b − 1, 0)`,
//! - `layer` scales every base leaf by the top's throughput.
//!
//! [`ResolvedClosure::resolve`] walks the tree once per vertex and keeps that
//! weighted list inline — no allocation, no second walk. Every BSDF query at
//! the vertex (the scatter, NEE's and guiding's `eval`) then sums a handful of
//! leaves, each with its own roughness, Fresnel, normal and tangent. Nothing
//! is pooled: that is the difference from the reduction this replaced, which
//! averaged every leaf onto one OpenPBR parameter set.
//!
//! The leaves follow MaterialX's GLSL reference (`libraries/pbrlib/genglsl`,
//! ported in [`mx`]): its Fresnel models, Turquin energy compensation on the
//! microfacet lobes, EON / Oren–Nayar / Burley diffuse and Imageworks sheen.
//! The one table that is not MaterialX's is the dielectric reflection
//! throughput — BSDL's tabulated filter ([`bsdl_tables`]), what NVIDIA
//! Typhoon uses by default.

mod bsdl_tables;
pub mod hair;
pub mod mx;

use glam::Vec3A;
use std::cell::RefCell;
use std::f32::consts::FRAC_1_PI;
use utils::cosine_hemisphere;

use crate::PathSampler;
use crate::hittable::HitRecord;
use crate::material::ScatterSample;
use crate::material::brdf::{
    ggx_d_aniso, ggx_g2_smith_aniso, pdf_vndf_ggx_aniso_local, pdf_vndf_h_aniso_local,
    sample_vndf_ggx_aniso_local, tangent_frame,
};
use crate::medium::Medium;
use crate::ray::Ray;
use crate::subsurface::SubsurfaceEntry;
use crust_mtlx::{Bsdf, Closure, Closures, DiffuseModel, NodeId, ScatterMode, Val};
use mx::{Fresnel, FresnelModel};

/// How many leaves a resolved closure holds inline.
///
/// The largest built-in expansion is seven (`standard_surface`: diffuse,
/// subsurface, sheen, transmission, dielectric, metal, coat). A document whose
/// tree has more leaves than this is refused at load, so the list never
/// silently drops one.
pub const MAX_LEAVES: usize = 8;

/// The GGX alpha floor. MaterialX clamps at `M_FLOAT_EPS`; crust's GGX
/// helpers need a finite distribution, and a mirror this sharp is already
/// indistinguishable from a delta at any pixel footprint.
const MIN_ALPHA: f32 = 1e-4;

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
    /// `generalized_schlick_bsdf`.
    Specular {
        fresnel: Fresnel,
        tint: Vec3A,
        ax: f32,
        ay: f32,
        mode: ScatterMode,
        /// Relative IOR across the interface in the ray-facing frame
        /// (`η_t / η_i`), for the refracted half of a transmitting leaf.
        eta: f32,
        /// A thin-walled surface transmits straight through (a delta lobe).
        thin_walled: bool,
    },
    Sheen {
        color: Vec3A,
        roughness: f32,
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
    /// The MaterialX category, for probes.
    pub category: &'static str,
    /// A reflecting interface layered over another reflecting interface: the
    /// coat, for light path expressions (`'coat'` rather than `'specular'`).
    /// MaterialX leaves carry no component name, so the tree's shape is
    /// what tells — see `ResolvedClosure::walk`.
    pub coat: bool,
}

impl Prepared {
    /// The event this leaf contributes toward a direction on the reflecting
    /// side, or (`transmitted`) the far side. Total over [`Lobe`].
    pub fn event(&self, transmitted: bool) -> crate::lpe::LobeEvent {
        use crate::lpe::{LobeEvent, LobeLabel, Scatter, microfacet_scatter};
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
                tint,
                ax,
                ay,
                mode,
                eta,
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
                    "{mode:?} {f}{film} tint {} alpha ({ax:.4} {ay:.4}) eta {eta:.4}{wall}",
                    c(tint)
                )
            }
            Lobe::Sheen { color, roughness } => {
                format!("sheen color {} roughness {roughness:.3}", c(color))
            }
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
    category: "",
    coat: false,
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
    rec: &'a HitRecord,
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
            self.medium = closures.volume.and_then(|v| {
                let m = Medium {
                    sigma_a: sanitize(slots[v.absorption as usize].rgb()),
                    sigma_s: sanitize(slots[v.scattering as usize].rgb()),
                    g: slots[v.anisotropy as usize].x().clamp(-0.999, 0.999),
                };
                (m.sigma_t_max() > 1e-6).then_some(m)
            });
        }
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
    /// returns its throughput toward ωo.
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
                let (mut p, albedo) = prepare(leaf, iface, w);
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
                // MaterialX: `1 − E·weight` for a leaf that lets light through
                // to a base (dielectric, generalized Schlick, sheen), and 0 for
                // an opaque one (diffuse, conductor, subsurface, translucent)
                // whatever its weight.
                match albedo {
                    Some(e) => (Vec3A::ONE - e * own).clamp(Vec3A::ZERO, Vec3A::ONE),
                    None => Vec3A::ZERO,
                }
            }
            Closure::Layer { top, base } => {
                let first = self.len;
                let t_top = self.walk(cl, *top, weight, iface, w);
                let middle = self.len;
                // A dielectric over the base is what a random walk below it
                // is entered through.
                let under = interface_of(cl, *top, w.slots).unwrap_or(iface);
                let t_base = self.walk(cl, *base, weight * t_top, under, w);
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
                t_top * t_base
            }
            Closure::Mix { fg, bg, mix } => {
                let m = w.slots[*mix as usize].x().clamp(0.0, 1.0);
                let t_fg = self.walk(cl, *fg, weight * m, iface, w);
                let t_bg = self.walk(cl, *bg, weight * (1.0 - m), iface, w);
                t_bg.lerp(t_fg, m)
            }
            Closure::Add { a, b } => {
                let ta = self.walk(cl, *a, weight, iface, w);
                let tb = self.walk(cl, *b, weight, iface, w);
                (ta + tb - Vec3A::ONE).max(Vec3A::ZERO)
            }
            Closure::Multiply { input, weight: k } => {
                let k = sanitize(w.slots[*k as usize].rgb()).min(Vec3A::ONE);
                self.walk(cl, *input, weight * k, iface, w)
            }
            // A pruned branch: nothing to shade, and it lets everything through.
            Closure::Empty => Vec3A::ONE,
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
    pub fn eval_lobes(&self, wi: Vec3A, out: &mut crate::lpe::LobeSplit) -> bool {
        self.eval_lobes_at(wi.normalize(), out)
    }

    /// [`ResolvedClosure::eval_lobes`] toward an already normalised `wi`,
    /// normalised no further — the terms `scatter`'s own `eval_pdf(world)`
    /// sums for a continuous sample, so they sum to its value bit for bit.
    fn eval_lobes_at(&self, wi: Vec3A, out: &mut crate::lpe::LobeSplit) -> bool {
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
        sampler: PathSampler,
        out: &mut crate::lpe::LobeSplit,
    ) -> Option<ScatterSample> {
        out.clear();
        let (sample, choice) = self.scatter_choosing(r_in, rec, sampler)?;
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
    pub fn delta_event(&self, sample: &ScatterSample) -> crate::lpe::LobeEvent {
        use crate::lpe::{LobeEvent, LobeLabel, Scatter};
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
                    tint,
                    mode,
                    ..
                } => {
                    if mode.reflects() {
                        tint * fresnel.eval(1.0)
                    } else {
                        tint
                    }
                }
            };
            sum += leaf.weight * tint;
        }
        sum.clamp(Vec3A::ZERO, Vec3A::ONE)
    }

    /// [`crate::Material::eval`].
    pub fn eval(&self, _r_in: &Ray, _rec: &HitRecord, wi: Vec3A) -> Option<(Vec3A, f32)> {
        if self.len == 0 {
            return None;
        }
        let (value, pdf) = self.eval_pdf(wi.normalize());
        Some((value, pdf.max(1e-4)))
    }

    /// [`crate::Material::scatter_importance`].
    pub fn scatter(
        &self,
        r_in: &Ray,
        rec: &HitRecord,
        sampler: PathSampler,
    ) -> Option<ScatterSample> {
        self.scatter_choosing(r_in, rec, sampler).map(|(s, _)| s)
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
        sampler: PathSampler,
    ) -> Option<(ScatterSample, Option<Choice>)> {
        if self.len == 0 {
            return None;
        }
        // One 4D block: `s[0]` picks the leaf, `s[1..3]` the direction and
        // `s[3]` a leaf's own discrete choice (reflect or refract).
        let s = sampler.draw_sample_f32::<4>();
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

/// Builds a leaf's lobe and frame at the vertex, and returns the directional
/// albedo `E(ωo)` its throughput is `1 − E·weight` of — `None` for an opaque
/// leaf, whose throughput is 0.
fn prepare(leaf: &crust_mtlx::Leaf, iface: Interface, w: &Walk<'_>) -> (Prepared, Option<Vec3A>) {
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
    let (lobe, select, throughput) = match &leaf.bsdf {
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
            color, roughness, ..
        } => prepare_sheen(&cx, color, roughness),
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
            select,
            category: leaf.bsdf.category(),
            coat: false,
        },
        throughput,
    )
}

/// A leaf's lobe, its selection weight (floored at 0.02), and the directional
/// albedo its throughput is `1 − E·weight` of (`None`: opaque) — what each
/// `prepare_*` builds.
type Built = (Lobe, f32, Option<Vec3A>);

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
}

/// [`prepare`] for a `Diffuse` leaf.
#[inline(always)]
fn prepare_diffuse(
    cx: &LeafInputs<'_>,
    model: &DiffuseModel,
    color: &u32,
    roughness: &u32,
) -> Built {
    let color = cx.rgb(*color);
    let r = cx.s(*roughness).x().clamp(0.0, 1.0);
    let albedo = match model {
        DiffuseModel::Eon => mx::eon_dir_albedo(cx.nv, r, color),
        DiffuseModel::OrenNayar => color * mx::oren_nayar_dir_albedo(cx.nv, r),
        DiffuseModel::Burley => color * mx::burley_dir_albedo(cx.nv, r),
    };
    (
        Lobe::Diffuse {
            model: *model,
            color,
            roughness: r,
        },
        cx.w.luma.of(albedo).max(0.02),
        None,
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
    (lobe, cx.w.luma.of(color).max(0.02), None)
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
    // Over a base, a fibre passes on what it does not scatter.
    (
        Lobe::Hair(hair),
        cx.w.luma.of(albedo).max(0.02),
        Some(albedo),
    )
}

/// [`prepare`] for a `Translucent` leaf.
#[inline(always)]
fn prepare_translucent(cx: &LeafInputs<'_>, color: &u32) -> Built {
    let color = cx.rgb(*color);
    (
        Lobe::Translucent { color },
        cx.w.luma.of(color).max(0.02),
        None,
    )
}

/// [`prepare`] for a `Sheen` leaf.
#[inline(always)]
fn prepare_sheen(cx: &LeafInputs<'_>, color: &u32, roughness: &u32) -> Built {
    let color = cx.rgb(*color);
    let r = cx.s(*roughness).x().clamp(0.0, 1.0);
    let e = mx::imageworks_sheen_dir_albedo(cx.nv, r);
    (
        Lobe::Sheen {
            color,
            roughness: r,
        },
        (cx.w.luma.of(color) * e).max(0.02),
        Some(Vec3A::splat(e)),
    )
}

/// [`prepare`] for a `Dielectric` leaf.
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
        ior
    } else {
        1.5
    };
    let (ax, ay) = alphas(cx.s(*roughness));
    let tint = cx.rgb(*tint);
    // In the ray-facing frame: entering the interior from the front,
    // leaving it from the back.
    let eta = if cx.w.rec.front_face { ior } else { 1.0 / ior };
    let fresnel = Fresnel {
        model: FresnelModel::Dielectric { ior: eta },
        thin_film: cx.film(thin_film),
    };
    let avg = mx::average_alpha(ax, ay);
    // MaterialX's throughput for every dielectric mode: `1 − E_R·w`,
    // the reflection albedo alone. BSDL's table where it applies (no
    // film); MaterialX's Fresnel-weighted fit with a film.
    let e_r = if fresnel.thin_film.is_some() {
        let f = fresnel.eval(cx.nv);
        fresnel.dir_albedo(cx.nv, avg) * mx::ggx_energy_compensation(cx.nv, avg, f)
    } else {
        Vec3A::splat(1.0 - dielectric_refl_filter(cx.nv, avg.sqrt(), ior))
    };
    let e = cx.w.luma.of(e_r);
    let select = match mode {
        ScatterMode::R => e,
        ScatterMode::T => (1.0 - e) * cx.w.luma.of(tint),
        ScatterMode::RT => e + (1.0 - e) * cx.w.luma.of(tint),
    };
    (
        Lobe::Specular {
            fresnel,
            tint,
            ax,
            ay,
            mode: *mode,
            eta,
            thin_walled: cx.w.thin_walled,
        },
        select.max(0.02),
        Some(e_r),
    )
}

/// [`prepare`] for a `Conductor` leaf.
#[inline(always)]
fn prepare_conductor(
    cx: &LeafInputs<'_>,
    ior: &u32,
    extinction: &u32,
    roughness: &u32,
    thin_film: &Option<crust_mtlx::ThinFilm>,
) -> Built {
    let (ax, ay) = alphas(cx.s(*roughness));
    let fresnel = Fresnel {
        model: FresnelModel::Conductor {
            n: cx.rgb(*ior),
            k: cx.rgb(*extinction),
        },
        thin_film: cx.film(thin_film),
    };
    let avg = mx::average_alpha(ax, ay);
    let e = fresnel.dir_albedo(cx.nv, avg)
        * mx::ggx_energy_compensation(cx.nv, avg, fresnel.eval(cx.nv));
    (
        Lobe::Specular {
            fresnel,
            tint: Vec3A::ONE,
            ax,
            ay,
            mode: ScatterMode::R,
            eta: 1.0,
            thin_walled: false,
        },
        cx.w.luma.of(e).max(0.02),
        // MaterialX: a conductor is opaque.
        None,
    )
}

/// [`prepare`] for a `Schlick` leaf.
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
    let (ax, ay) = alphas(cx.s(*roughness));
    let f0 = cx.rgb(*color0);
    let fresnel = Fresnel {
        model: FresnelModel::Schlick {
            f0,
            f82: cx.rgb(*color82),
            f90: cx.rgb(*color90),
            exponent: cx.s(*exponent).x().max(0.0),
        },
        thin_film: cx.film(thin_film),
    };
    let avg = mx::average_alpha(ax, ay);
    let e = fresnel.dir_albedo(cx.nv, avg)
        * mx::ggx_energy_compensation(cx.nv, avg, fresnel.eval(cx.nv));
    let e_avg = (e.x + e.y + e.z) / 3.0;
    let eta = mx::f0_to_ior(Vec3A::splat((f0.x + f0.y + f0.z) / 3.0)).x;
    (
        Lobe::Specular {
            fresnel,
            tint: Vec3A::ONE,
            ax,
            ay,
            mode: *mode,
            eta: if cx.w.rec.front_face { eta } else { 1.0 / eta },
            thin_walled: cx.w.thin_walled,
        },
        e_avg.max(0.02),
        Some(Vec3A::splat(e_avg)),
    )
}

/// BSDL's dielectric reflection filter `1 − E_R(cosθo)` at perceptual
/// roughness `r` (`α = r²`) and IOR `ior`, interpolated as BSDL's
/// `TabulatedEnergyCurve` does: bilinear in (IOR, roughness), piecewise
/// linear in the cosine.
pub fn dielectric_refl_filter(cos_o: f32, r: f32, ior: f32) -> f32 {
    use bsdl_tables::{DIELECTRIC_REFL_FRONT as T, NC, NF, NR};
    const IOR_MIN: f32 = 1.001;
    const IOR_MAX: f32 = 5.0;
    let eta = if ior < 1.0 { 1.0 / ior } else { ior };
    let eta = eta.clamp(IOR_MIN, IOR_MAX);
    let fi = ((eta - IOR_MIN) / (IOR_MAX - IOR_MIN)).sqrt() * (NF - 1) as f32;
    let fa = (fi as usize).min(NF - 1);
    let fb = (fa + 1).min(NF - 1);
    let ff = fi - fa as f32;
    let ri = r.clamp(0.0, 1.0) * (NR - 1) as f32;
    let ra = (ri as usize).min(NR - 1);
    let rb = (ra + 1).min(NR - 1);
    let rf = ri - ra as f32;
    let lerp = |t: f32, a: f32, b: f32| a + (b - a) * t;
    let e = |c: usize| {
        lerp(
            ff,
            lerp(rf, T[(fa * NR + ra) * NC + c], T[(fa * NR + rb) * NC + c]),
            lerp(rf, T[(fb * NR + ra) * NC + c], T[(fb * NR + rb) * NC + c]),
        )
    };
    let cosine = |i: usize| (i as f32 / (NC - 1) as f32).max(1e-6);
    let c = cos_o.max(0.0);
    if c <= cosine(0) {
        return e(0);
    }
    for i in 1..NC {
        let c1 = cosine(i);
        if c < c1 {
            let c0 = cosine(i - 1);
            return lerp((c - c0) / (c1 - c0), e(i - 1), e(i));
        }
    }
    e(NC - 1)
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

/// `(f, pdf)` of a lobe for local `v`, `l`: the BSDF value without the cosine,
/// and the lobe's own continuous sampling density.
fn eval_lobe(lobe: &Lobe, v: Vec3A, l: Vec3A) -> (Vec3A, f32) {
    let nv = v.z.clamp(1e-6, 1.0);
    match *lobe {
        Lobe::Diffuse {
            model,
            color,
            roughness,
        } => {
            if l.z <= 0.0 {
                return (Vec3A::ZERO, 0.0);
            }
            let nl = l.z;
            let f = match model {
                DiffuseModel::Eon => mx::eon(nv, nl, l.dot(v), roughness, color),
                DiffuseModel::OrenNayar => color * mx::oren_nayar(nv, nl, l.dot(v), roughness),
                DiffuseModel::Burley => {
                    let h = (v + l).normalize_or_zero();
                    color * mx::burley(nv, nl, l.dot(h), roughness)
                }
            };
            (f * FRAC_1_PI, nl * FRAC_1_PI)
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
        Lobe::Sheen { color, roughness } => {
            if l.z <= 0.0 {
                return (Vec3A::ZERO, 0.0);
            }
            let h = (v + l).normalize_or_zero();
            let f = mx::imageworks_sheen(l.z, nv, h.z.max(0.0), roughness);
            (color * f, l.z * FRAC_1_PI)
        }
        Lobe::Specular {
            fresnel,
            tint,
            ax,
            ay,
            mode,
            eta,
            thin_walled,
        } => {
            let avg = mx::average_alpha(ax, ay);
            let transmit_continuous = mode.transmits() && !thin_walled;
            if l.z > 0.0 {
                if !mode.reflects() {
                    return (Vec3A::ZERO, 0.0);
                }
                let h = (v + l).normalize_or_zero();
                if h.z <= 0.0 {
                    return (Vec3A::ZERO, 0.0);
                }
                let vh = v.dot(h).clamp(1e-6, 1.0);
                let d = ggx_d_aniso(h.z, h.x, h.y, ax, ay);
                let g = ggx_g2_smith_aniso(nv, v.x, v.y, l.z, l.x, l.y, ax, ay);
                let f = fresnel.eval(vh);
                let comp = mx::ggx_energy_compensation(nv, avg, f);
                let value = tint * f * comp * (d * g / (4.0 * nv * l.z.max(1e-6)));
                let mut pdf = pdf_vndf_ggx_aniso_local(v, h, ax, ay);
                if mode == ScatterMode::RT {
                    pdf *= scalar(f);
                }
                (value, pdf)
            } else if transmit_continuous && l.z < 0.0 {
                let (btdf, p_h, vh) = refraction(v, l, eta, ax, ay);
                if btdf <= 0.0 {
                    return (Vec3A::ZERO, 0.0);
                }
                // Both modes pay the interface's `(1 − F)` (MaterialX GLSL's
                // `mx_surface_transmission`, OSL, BSDL and Typhoon alike —
                // the layer above attenuates only by its own throughput);
                // `RT` also samples refraction with that probability.
                let t = 1.0 - scalar(fresnel.eval(vh));
                let pdf = match mode {
                    ScatterMode::RT => p_h * t,
                    _ => p_h,
                };
                (tint * (btdf * t), pdf)
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

/// Walter et al. 2007's refraction BTDF without its Fresnel factor — the
/// `(1 − F)` is the caller's, film-aware Fresnel — plus the
/// VNDF sampling density of `l` and `v·h` at the refraction half vector.
/// `eta` is `η_t / η_i`.
fn refraction(v: Vec3A, l: Vec3A, eta: f32, ax: f32, ay: f32) -> (f32, f32, f32) {
    let (eta_i, eta_t) = (1.0, eta);
    let mut h = -(v * eta_i + l * eta_t);
    if h.length_squared() < 1e-12 {
        return (0.0, 0.0, 0.0);
    }
    h = h.normalize();
    if h.z < 0.0 {
        h = -h;
    }
    let vh = v.dot(h);
    let lh = l.dot(h);
    if vh <= 1e-6 || lh >= -1e-6 {
        return (0.0, 0.0, 0.0);
    }
    let nv = v.z.max(1e-6);
    let nl = (-l.z).max(1e-6);
    let d = ggx_d_aniso(h.z.max(1e-6), h.x, h.y, ax, ay);
    let g = ggx_g2_smith_aniso(nv, v.x, v.y, nl, l.x, l.y, ax, ay);
    let denom = eta_i * vh + eta_t * lh;
    let denom2 = denom * denom;
    if denom2 < 1e-10 {
        return (0.0, 0.0, 0.0);
    }
    let btdf = (vh * -lh) / (nv * nl) * (eta_t * eta_t * d * g / denom2);
    let p_h = pdf_vndf_h_aniso_local(v, h, ax, ay);
    let jacobian = eta_t * eta_t * -lh / denom2;
    (btdf.max(0.0), p_h * jacobian, vh)
}

/// Samples a direction from one lobe.
fn sample_lobe(lobe: &Lobe, v: Vec3A, uv: [f32; 2], u: f32) -> Option<LobeSample> {
    match *lobe {
        Lobe::Diffuse { .. } | Lobe::Sheen { .. } => Some(LobeSample::Continuous {
            dir: cosine_hemisphere(uv),
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
            tint,
            ax,
            ay,
            mode,
            eta,
            thin_walled,
        } => {
            let spread = (ax + ay).min(crate::RayCone::MAX_SPREAD);
            let v = Vec3A::new(v.x, v.y, v.z.max(1e-6)).normalize();
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
                    ScatterMode::RT => tint,
                    _ => tint * (1.0 - scalar(fresnel.eval(vh))),
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
