//! [`LightKind`]: the closed set of lights, dispatched by `match`.

use glam::Vec3A;

use super::{AreaLight, DistantLight, DomeLight, Light, LightSample};
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
    fn power(&self) -> Option<f32> {
        dispatch!(self, l => l.power())
    }
}
