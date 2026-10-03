use crate::ray::Ray;
use glam::Vec3A;

/// The `HitRecord` struct stores the geometry of a ray-surface
/// intersection as the *materials* consume it: the intersection point,
/// the ray-facing surface normal, the ray parameter, and the facing flag.
/// Intersection itself happens in the `crust-rt` kernel (which reports
/// ID-based [`crust_rt::RayHit`]s); [`crate::World`] converts kernel hits
/// into `HitRecord`s and looks up the material by `geom_id`.
#[derive(Clone, Copy)]
pub struct HitRecord {
    /// The point of intersection.
    pub p: Vec3A,
    /// The surface normal at the intersection point.
    pub normal: Vec3A,
    /// The parameter `t` along the ray where the intersection occurs.
    pub t: f32,
    /// Indicates whether the ray hit the front face of the surface.
    pub front_face: bool,
    /// The *source* (pre-triangulation) mesh face the hit lies on, and where
    /// on it — `None` when the hit carries no face identity: every primitive
    /// that is not a triangulated polygon mesh, and every mesh whose material
    /// asked for no per-face texture. One `Option` for both, so a position
    /// can never be read off a hit that has no face.
    pub face: Option<FaceHit>,
    /// Interpolated `primvars:st` texture coordinates, the chart a *UV*
    /// texture indexes — unrelated to [`FaceHit::uv`], which is Ptex's per-face
    /// parameterisation. Deliberately **not** wrapped into `[0, 1]`: a UDIM
    /// set addresses its tiles by the integer part, so clamping here would
    /// collapse fourteen 4K tiles onto one.
    ///
    /// `None` for geometry carrying no `st` primvar, or whose material asks
    /// for no UV texture. An `Option` rather than a sentinel: the origin of
    /// the chart is a perfectly ordinary texel.
    pub uv: Option<(f32, f32)>,
    /// World-space surface tangent along increasing `u`, already
    /// orthogonalised against `normal` and unit length. The frame a
    /// tangent-space normal map is expressed in; the bitangent is
    /// `normal × tangent`.
    ///
    /// `Vec3A::ZERO` when no tangent is known, which a material must read as
    /// "shade with the geometric normal" rather than as a degenerate frame.
    /// The importer can only build one for *baked* (single-placement)
    /// geometry — see [`crate::tangent_of`].
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
    /// Width of the ray's texture footprint at this hit, in the face's own
    /// `[0, 1]²` — the filter width a Ptex texture should read [`FaceHit::uv`]
    /// with. `0.0` carries the same meaning as `uv_width`, and it is `0.0`
    /// whenever `face` is `None`.
    pub face_width: f32,
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
}

/// Hand-written rather than derived so that it states every field's "no
/// data" value — `face` is `None`, not face 0, which is a perfectly valid
/// face and would make an unmapped hit silently sample the first face of
/// some texture.
impl Default for HitRecord {
    fn default() -> Self {
        HitRecord {
            p: Vec3A::ZERO,
            normal: Vec3A::ZERO,
            t: 0.0,
            front_face: false,
            face: None,
            uv: None,
            tangent: Vec3A::ZERO,
            uv_width: 0.0,
            face_width: 0.0,
        }
    }
}

impl HitRecord {
    /// Creates a new, default `HitRecord`.
    pub fn new() -> HitRecord {
        Default::default()
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
