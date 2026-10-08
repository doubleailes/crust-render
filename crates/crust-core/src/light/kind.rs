//! [`LightKind`]: the closed set of lights, dispatched by `match`.

use glam::Vec3A;

use super::{AreaLight, DistantLight, DomeLight, FoundAlong, Light, LightSample};
use crate::pdf::PdfSolidAngle;

/// One light of the scene: every [`Light`] the renderer has, as an enum
/// rather than an `Arc<dyn Light>`.
///
/// The three implementors are all in this module and nothing outside the
/// crate implements `Light`, so the set is closed; as an enum, NEE's
/// `sample_li` and the bounce side's `pdf_at_point` / `escaped` are static
/// calls the optimiser can inline — together with [`AreaShape`](super::AreaShape),
/// the whole light sample is statically dispatched. `Light` stays the
/// contract each variant implements; the enum is only the dispatch, the move
/// `crust_rt`'s `PrimNode` made.
#[derive(Clone)]
pub enum LightKind {
    Area(AreaLight),
    Distant(DistantLight),
    Dome(DomeLight),
}

impl From<AreaLight> for LightKind {
    fn from(l: AreaLight) -> Self {
        LightKind::Area(l)
    }
}

impl From<DistantLight> for LightKind {
    fn from(l: DistantLight) -> Self {
        LightKind::Distant(l)
    }
}

impl From<DomeLight> for LightKind {
    fn from(l: DomeLight) -> Self {
        LightKind::Dome(l)
    }
}

impl LightKind {
    /// What a ray from `from` along the unit `dir` finds of this light,
    /// visibility aside: for an area light each point where it meets the
    /// light's surface, nearest first; for a distant light the cone, at
    /// infinity, when `dir` is inside it. The radiance and density are the
    /// ones NEE uses for the same connection ([`Light::sample_li`],
    /// [`Light::pdf_at_point`], [`Light::escaped`]). A dome answers nothing:
    /// every direction reaches it, and its bounce side is the escaping ray.
    ///
    /// The bounce-side estimate of a shadow-linked light (the "link twin" in
    /// `tracer/path.rs`) asks this, then tests visibility the way NEE does.
    #[inline]
    pub fn found_along(&self, from: Vec3A, dir: Vec3A, mut f: impl FnMut(FoundAlong)) {
        match self {
            LightKind::Area(l) => l.found_along(from, dir, f),
            LightKind::Distant(l) => {
                if let Some((radiance, pdf)) = l.escaped(from, dir) {
                    f(FoundAlong {
                        distance: f32::INFINITY,
                        radiance,
                        pdf,
                    });
                }
            }
            LightKind::Dome(_) => {}
        }
    }

    /// Whether this is a dome light.
    #[inline]
    pub fn is_dome(&self) -> bool {
        matches!(self, LightKind::Dome(_))
    }
}

/// Forwards one [`Light`] method to the variant.
macro_rules! dispatch {
    ($self:ident, $l:ident => $call:expr) => {
        match $self {
            LightKind::Area($l) => $call,
            LightKind::Distant($l) => $call,
            LightKind::Dome($l) => $call,
        }
    };
}

impl Light for LightKind {
    #[inline(always)]
    fn kind(&self) -> &'static str {
        dispatch!(self, l => l.kind())
    }

    #[inline(always)]
    fn sample_li(&self, from: Vec3A, u: f32, v: f32) -> Option<LightSample> {
        dispatch!(self, l => l.sample_li(from, u, v))
    }

    #[inline(always)]
    fn pdf_at_point(&self, from: Vec3A, light_point: Vec3A) -> Option<PdfSolidAngle> {
        dispatch!(self, l => l.pdf_at_point(from, light_point))
    }

    #[inline(always)]
    fn escaped(&self, from: Vec3A, direction: Vec3A) -> Option<(Vec3A, Option<PdfSolidAngle>)> {
        dispatch!(self, l => l.escaped(from, direction))
    }

    #[inline(always)]
    fn at_infinity(&self) -> bool {
        dispatch!(self, l => l.at_infinity())
    }

    #[inline(always)]
    fn geom_id(&self) -> Option<u32> {
        dispatch!(self, l => l.geom_id())
    }

    #[inline(always)]
    fn power(&self, luma: utils::Luma) -> Option<f32> {
        dispatch!(self, l => l.power(luma))
    }
}
