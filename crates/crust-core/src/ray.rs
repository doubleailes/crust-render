use crate::medium::Medium;
use glam::Vec3A;
use std::sync::Arc;

pub use crust_rt::{MASK_ALL, MASK_CAMERA, MASK_INDIRECT, MASK_SHADOW};

/// The ray's texture-filtering footprint, as a cone about its axis.
///
/// This is the renderer's answer to "how much texture does this ray cover",
/// which a mip pyramid needs and a single bilinear tap does not. Following
/// Akenine-Möller et al.'s ray cones rather than full ray differentials: a
/// path tracer spawns one ray at a time, so the four extra rays differentials
/// want have nowhere to come from, whereas two floats ride along for free.
///
/// `width` is the cone's **diameter perpendicular to the ray**, in world
/// units, at the ray's origin; `spread` is how much that diameter grows per
/// world unit travelled. A default cone (both zero) is a pencil ray, which
/// every texture reads as "point-sample the finest level" — i.e. exactly the
/// behaviour that predates this struct.
///
/// Two invariants are easy to break and expensive to debug:
///
/// 1. `width` is the cross-section **perpendicular to the ray**, never the
///    footprint spread across a surface. Grazing incidence stretches the
///    latter by `1/|cos θ|`, and that factor belongs only on the way out to a
///    texture width — folding it back in here would compound it at every
///    bounce (five grazing hits is 3125×) and every deep texture would read
///    its 1×1 level.
/// 2. `spread` only ever grows. A bounce adds the scattered lobe's angular
///    width to it, so a path that has been through a diffuse surface can
///    never afterwards sharpen.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct RayCone {
    /// Footprint diameter perpendicular to the ray, in world units, at the origin.
    pub width: f32,
    /// Growth of `width` per world unit travelled.
    pub spread: f32,
}

impl RayCone {
    /// The cone's diameter after travelling `distance` world units.
    #[inline]
    pub fn width_at(&self, distance: f32) -> f32 {
        self.width + self.spread * distance
    }

    /// The cone a bounce leaves behind: it starts at the width it arrived
    /// with and widens by the scattered lobe's own angular spread.
    ///
    /// `spread` saturates at [`RayCone::MAX_SPREAD`]: past a full radian the
    /// footprint already covers the coarsest mip level of any texture, and
    /// letting it run away turns into an infinity a few bounces later.
    #[inline]
    pub fn scattered(&self, width_at_hit: f32, lobe_spread: f32) -> RayCone {
        RayCone {
            width: width_at_hit,
            spread: (self.spread + lobe_spread.max(0.0)).min(Self::MAX_SPREAD),
        }
    }

    /// Widest spread a cone is allowed to carry (radians, small-angle).
    pub const MAX_SPREAD: f32 = 1.0;
}

/// The renderer's ray: the kernel ray (origin, direction, shutter time,
/// visibility mask — see [`crust_rt::Ray`]) plus the renderer-side state
/// the kernel deliberately does not know about: the participating
/// `medium` the ray is currently travelling through, used by transmissive
/// OpenPBR materials so the tracer can apply Beer-Lambert attenuation
/// between surface hits.
#[derive(Default, Clone)]
pub struct Ray {
    rt: crust_rt::Ray,
    medium: Option<Arc<Medium>>,
    cone: RayCone,
}

impl Ray {
    /// Creates a new `Ray` with the specified origin and direction, in
    /// vacuum (no medium), at shutter time 0, visible to all geometry.
    pub fn new(origin: Vec3A, direction: Vec3A) -> Ray {
        Ray {
            rt: crust_rt::Ray::new(origin, direction),
            medium: None,
            cone: RayCone::default(),
        }
    }

    /// Creates a new `Ray` travelling through the given medium. Use this on
    /// a refraction that enters a transmissive volume.
    pub fn new_in_medium(origin: Vec3A, direction: Vec3A, medium: Arc<Medium>) -> Ray {
        Ray {
            rt: crust_rt::Ray::new(origin, direction),
            medium: Some(medium),
            cone: RayCone::default(),
        }
    }

    /// Same ray with the shutter time replaced.
    pub fn with_time(mut self, time: f32) -> Ray {
        self.rt.time = time;
        self
    }

    /// Same ray with the visibility mask replaced.
    pub fn with_mask(mut self, mask: u32) -> Ray {
        self.rt.mask = mask;
        self
    }

    /// Same ray with the texture-filtering footprint replaced.
    ///
    /// Materials build scattered rays with no path context, so — like the
    /// shutter time and the visibility mask — the cone is stamped on by the
    /// tracer once the ray comes back.
    pub fn with_cone(mut self, cone: RayCone) -> Ray {
        self.cone = cone;
        self
    }

    /// The kernel view of this ray — what `crust_rt` queries take.
    pub fn rt(&self) -> &crust_rt::Ray {
        &self.rt
    }

    pub fn origin(&self) -> Vec3A {
        self.rt.origin
    }

    pub fn direction(&self) -> Vec3A {
        self.rt.dir
    }

    pub fn medium(&self) -> Option<&Arc<Medium>> {
        self.medium.as_ref()
    }

    pub fn time(&self) -> f32 {
        self.rt.time
    }

    pub fn mask(&self) -> u32 {
        self.rt.mask
    }

    pub fn cone(&self) -> RayCone {
        self.cone
    }

    pub fn at(&self, t: f32) -> Vec3A {
        self.rt.at(t)
    }
}
