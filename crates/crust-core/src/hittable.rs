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
    /// Index of the *source* (pre-triangulation) mesh face, or
    /// [`HitRecord::NO_FACE`] when the hit carries no face identity — every
    /// primitive that is not a triangulated polygon mesh, and every mesh whose
    /// material asked for no per-face texture.
    ///
    /// This is a Ptex face id: the kernel reports the index of the *triangle*
    /// it hit, and [`crate::World`] maps that back through the fan
    /// triangulation to the polygon the triangle came from.
    pub face_id: u32,
    /// Position within `face_id`'s own `[0, 1]²` parametric domain, already
    /// mapped out of the hit triangle's barycentrics. Meaningless — and left
    /// at zero — when `face_id` is [`HitRecord::NO_FACE`].
    pub face_uv: (f32, f32),
}

/// Hand-written rather than derived so `face_id` defaults to
/// [`HitRecord::NO_FACE`] and not to `0`, which is a perfectly valid face and
/// would make an unmapped hit silently sample the first face of some texture.
impl Default for HitRecord {
    fn default() -> Self {
        HitRecord {
            p: Vec3A::ZERO,
            normal: Vec3A::ZERO,
            t: 0.0,
            front_face: false,
            face_id: HitRecord::NO_FACE,
            face_uv: (0.0, 0.0),
        }
    }
}

impl HitRecord {
    /// `face_id` sentinel for a hit with no per-face texture identity.
    pub const NO_FACE: u32 = u32::MAX;

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
