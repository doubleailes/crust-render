//! Densities with their measure in the type: [`PdfSolidAngle`] and
//! [`InvPdfArea`].
//!
//! Every pdf used to be a bare `f32` whose measure lived in a doc comment,
//! and MIS combines two of them: a light's density and a bounce's, which are
//! only comparable in the same measure. `light/area.rs` records what that
//! costs when it goes wrong — an `1e-4` added to the area → solid-angle
//! conversion made the result depend on the scene's units. The newtypes are
//! `#[repr(transparent)]` and their methods are the `f32` operations they
//! replace, so they compile to the same code.

/// A density over directions, per steradian — the measure MIS weights are
/// computed in ([`SamplingStrategy`](crate::SamplingStrategy)'s
/// `light_weight` / `bounce_weight` take nothing else).
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub struct PdfSolidAngle(f32);

impl PdfSolidAngle {
    /// A density a light sampler reports: `None` unless finite and positive.
    ///
    /// That is the "non-finite density is refused, never replaced by a finite
    /// stand-in" rule as a constructor: a sample whose density is not a real
    /// number is refused (NEE never delivers it), and the bounce side, asking
    /// for the same point or direction, gets the same `None` and takes the
    /// emission whole.
    #[inline]
    pub fn new(pdf: f32) -> Option<Self> {
        (pdf.is_finite() && pdf > 0.0).then_some(Self(pdf))
    }

    /// A value already known to be a solid-angle density, taken as it is: a
    /// BSDF, phase or guide pdf (each floored by its producer), or a light
    /// density whose bounds its producer states beside the call. Not
    /// validated — use [`PdfSolidAngle::new`] where a refusal is meant.
    #[inline]
    pub const fn from_measure(pdf: f32) -> Self {
        Self(pdf)
    }

    /// The density, per steradian.
    #[inline]
    pub const fn get(self) -> f32 {
        self.0
    }

    /// `f32::max` on the density: the floor MIS puts under a light density
    /// before dividing by it.
    #[inline]
    #[must_use]
    pub fn max(self, floor: f32) -> Self {
        Self(self.0.max(floor))
    }
}

impl std::fmt::Display for PdfSolidAngle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// The reciprocal of an area density — the surface area one sample stands
/// for, in square scene units. For a shape sampled uniformly by area it is
/// the area itself (see [`LightShape::inv_pdf_area`](crate::LightShape)).
///
/// Kept as the reciprocal because that is what the one conversion
/// ([`InvPdfArea::to_solid_angle`]) divides by, bit for bit.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub struct InvPdfArea(f32);

impl InvPdfArea {
    #[inline]
    pub const fn new(area_per_sample: f32) -> Self {
        Self(area_per_sample)
    }

    /// The area per sample.
    #[inline]
    pub const fn get(self) -> f32 {
        self.0
    }

    /// The one area → solid-angle conversion, `d² / (|cos θ_light| · A)`, for
    /// a point `dist2` away seen at `cos_light` (unsigned: the Jacobian is the
    /// same from either side). `None` edge-on (`cos_light = 0`) or wherever
    /// the result is not a finite, positive density: refused, never a finite
    /// stand-in. Nothing is added to the denominator — that made the answer
    /// depend on the scene's units.
    #[inline]
    pub fn to_solid_angle(self, dist2: f32, cos_light: f32) -> Option<PdfSolidAngle> {
        let pdf = dist2 / (cos_light * self.0);
        if cos_light > 0.0 {
            PdfSolidAngle::new(pdf)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_density_that_is_not_a_real_positive_number_is_refused() {
        for bad in [0.0, -1.0, f32::INFINITY, f32::NAN] {
            assert_eq!(PdfSolidAngle::new(bad), None, "{bad}");
        }
        assert_eq!(PdfSolidAngle::new(2.5).map(PdfSolidAngle::get), Some(2.5));
    }

    #[test]
    fn the_area_conversion_refuses_edge_on_and_adds_nothing() {
        let a = InvPdfArea::new(0.5);
        assert_eq!(a.to_solid_angle(4.0, 0.0), None);
        assert_eq!(a.to_solid_angle(0.0, 1.0), None);
        // Exactly `d² / (cos · A)`: no epsilon in the denominator.
        let p = a.to_solid_angle(4.0, 0.25).unwrap().get();
        assert_eq!(p.to_bits(), (4.0f32 / (0.25 * 0.5)).to_bits());
    }
}
