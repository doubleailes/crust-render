use crate::ray::Ray;
use crate::rt_world::MeshHit;
use crate::strands::CurveHit;
use glam::Vec3A;

/// The `HitRecord` struct stores the geometry of a ray-surface
/// intersection as the *materials* consume it: the intersection point,
/// the ray-facing surface normal, the ray parameter, and the facing flag.
/// Intersection itself happens in the `crust-rt` kernel (which reports
/// ID-based [`crust_rt::RayHit`]s); [`crate::World`] converts kernel hits
/// into `HitRecord`s and looks up the material by `geom_id`.
///
/// `'w` is the [`crate::World`] the hit was found in: a curve hit borrows
/// the strand table its material reads curve primvars from ([`Self::curve`]),
/// and a mesh hit the primvar table its material reads ([`Self::mesh`]).
///
/// 96 bytes, and kept there: [`crate::WorldHit`] wraps one in exactly 128,
/// the most LLVM copies inline, and the integrator moves a `WorldHit` at
/// every vertex. Past it each move is a `memcpy` call — +0.47% of
/// cornellbox's instructions when the curve record was a field beside the
/// face rather than sharing [`HitPart`] with it.
#[derive(Clone, Copy)]
pub struct HitRecord<'w> {
    /// The point of intersection.
    pub p: Vec3A,
    /// The surface normal at the intersection point.
    pub normal: Vec3A,
    /// The parameter `t` along the ray where the intersection occurs.
    pub t: f32,
    /// Indicates whether the ray hit the front face of the surface.
    pub front_face: bool,
    /// What the hit lies on besides the surface itself: the source mesh face
    /// a per-face texture reads, the strand a curve's material reads, or
    /// neither. One enum, since no hit lies on both.
    pub part: HitPart<'w>,
    /// Interpolated `primvars:st` texture coordinates, the chart a *UV*
    /// texture indexes — unrelated to `face_uv`, which is Ptex's per-face
    /// parameterisation. Deliberately **not** wrapped into `[0, 1]`: a UDIM
    /// set addresses its tiles by the integer part, so clamping here would
    /// collapse fourteen 4K tiles onto one.
    ///
    /// `None` for geometry carrying no `st` primvar, or whose material asks
    /// for no UV texture — never a sentinel value, since the origin of the
    /// chart is a perfectly ordinary texel.
    pub uv: Option<(f32, f32)>,
    /// World-space surface tangent along increasing `u`, already
    /// orthogonalised against `normal` and unit length. The frame a
    /// tangent-space normal map is expressed in; the bitangent is
    /// `normal × tangent`.
    ///
    /// `Vec3A::ZERO` when no tangent is known, which a material must read as
    /// "shade with the geometric normal" rather than as a degenerate frame.
    /// The importer can only build one for *baked* (single-placement)
    /// geometry — see [`crate::UvMap::tangents`].
    pub tangent: Vec3A,
    /// Width of the ray's texture footprint at this hit, in the *chart's* UV
    /// units — the filter width a UV texture should read `uv` with.
    ///
    /// `0.0` means "point-sample the finest level": no cone was stamped on
    /// the ray (`CRUST_RAY_CONES=0`), or the geometry carries no density
    /// table to convert the cone's world-space width with. That is exactly
    /// the behaviour that predates mip pyramids, which is what makes the
    /// switch an honest A/B.
    pub uv_width: f32,
}

/// What a hit lies on besides its surface ([`HitRecord::part`]).
#[derive(Clone, Copy, Default)]
pub enum HitPart<'w> {
    /// Nothing more: every primitive that is neither a face-mapped mesh nor
    /// a curve with a strand table.
    #[default]
    None,
    /// The *source* (pre-triangulation) mesh face, and where on it: a mesh
    /// whose material asked for a per-face texture. A position cannot be
    /// read off a hit that has no face.
    Face(FaceHit),
    /// Where on its strand a curve hit lies, when the geometry carries a
    /// strand table (its material reads a curve property, primvar or chart).
    Curve(CurveHit<'w>),
    /// Where on a mesh (or an analytic sphere) a hit lies, when its material
    /// reads one of its primvars ([`crate::PrimvarTable::shading`]).
    Mesh(MeshHit<'w>),
}

/// A hit's Ptex face identity (see [`HitRecord::face`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FaceHit {
    /// The Ptex face id: the kernel reports the index of the *triangle* it
    /// hit, and [`crate::World`] maps that back through the fan triangulation
    /// to the polygon the triangle came from.
    pub id: u32,
    /// Position within the face's own `[0, 1]²` parametric domain, already
    /// mapped out of the hit triangle's barycentrics.
    pub uv: (f32, f32),
    /// Width of the ray's texture footprint at this hit, in the face's own
    /// `[0, 1]²` — the filter width a Ptex texture should read `uv` with.
    /// `0.0` carries the same meaning as [`HitRecord::uv_width`].
    pub width: f32,
}

/// Hand-written rather than derived so that it states every field's "no
/// data" value — `part` is `None`, not face 0, which is a perfectly valid
/// face and would make an unmapped hit silently sample the first face of
/// some texture.
impl Default for HitRecord<'_> {
    fn default() -> Self {
        HitRecord {
            p: Vec3A::ZERO,
            normal: Vec3A::ZERO,
            t: 0.0,
            front_face: false,
            part: HitPart::None,
            uv: None,
            tangent: Vec3A::ZERO,
            uv_width: 0.0,
        }
    }
}

impl<'w> HitRecord<'w> {
    /// Creates a new, default `HitRecord`.
    pub fn new() -> HitRecord<'w> {
        Default::default()
    }

    /// The source mesh face the hit lies on, when it carries one.
    #[inline]
    pub fn face(&self) -> Option<&FaceHit> {
        match &self.part {
            HitPart::Face(f) => Some(f),
            _ => None,
        }
    }

    /// Where on its strand the hit lies, when it is a curve hit carrying one.
    #[inline]
    pub fn curve(&self) -> Option<&CurveHit<'w>> {
        match &self.part {
            HitPart::Curve(c) => Some(c),
            _ => None,
        }
    }

    /// Where on a mesh the hit lies, when its material reads the mesh's
    /// primvars.
    #[inline]
    pub fn mesh(&self) -> Option<&MeshHit<'w>> {
        match &self.part {
            HitPart::Mesh(m) => Some(m),
            _ => None,
        }
    }

    /// The record without its strand ([`HitRecord::curve`]) or its mesh
    /// primvars ([`HitRecord::mesh`]), so it can outlive the world it was
    /// found in: for a hit kept only for its geometry (a data AOV's first
    /// hit, a random walk's exit, which the exit Lambertian shades), never
    /// for one a curve's or a mesh's own material shades.
    pub fn detached(self) -> HitRecord<'static> {
        HitRecord {
            p: self.p,
            normal: self.normal,
            t: self.t,
            front_face: self.front_face,
            part: match self.part {
                HitPart::Face(f) => HitPart::Face(f),
                HitPart::None | HitPart::Curve(_) | HitPart::Mesh(_) => HitPart::None,
            },
            uv: self.uv,
            tangent: self.tangent,
            uv_width: self.uv_width,
        }
    }

    /// Sets the surface normal and determines whether the ray hit the front face.
    ///
    /// # Parameters
    /// - `r`: The ray that intersects the object.
    /// - `outward_normal`: The outward-facing normal of the surface.
    ///
    /// This method adjusts the normal to always point against the ray's direction
    /// and sets the `front_face` flag accordingly.
    pub fn set_face_normal(&mut self, r: &Ray, outward_normal: Vec3A) {
        self.front_face = r.direction().dot(outward_normal) < 0.0;
        self.normal = if self.front_face {
            outward_normal
        } else {
            -outward_normal
        };
    }
}

#[cfg(test)]
mod tests {
    /// A `WorldHit` is moved at every path vertex; past 128 bytes LLVM
    /// copies it with a `memcpy` call instead of inline (see [`HitRecord`]).
    #[test]
    #[cfg(target_pointer_width = "64")]
    fn a_world_hit_stays_an_inline_copy() {
        assert_eq!(std::mem::size_of::<super::HitRecord>(), 96);
        assert_eq!(std::mem::size_of::<crate::WorldHit>(), 128);
    }
}
