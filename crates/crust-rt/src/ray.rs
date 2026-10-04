use glam::Vec3A;

/// Ray visibility categories, Embree-mask style: a ray carries the bit of
/// the category it belongs to, geometry carries a mask of the categories
/// it is visible to, and an intersection only counts when the two
/// [`sees`](RayMask::sees) each other. Geometry and rays both default to
/// [`MASK_ALL`], so masking is zero-cost until something opts in.
///
/// A newtype over the `u32` bits (`repr(transparent)`, so it costs nothing)
/// rather than a bare `u32`, so a mask cannot be passed where a geometry or
/// primitive id is expected, or the other way round.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RayMask(pub u32);

impl RayMask {
    /// Visible to nothing.
    pub const NONE: RayMask = RayMask(0);
    /// Primary rays from the camera.
    pub const CAMERA: RayMask = RayMask(1 << 0);
    /// Shadow rays toward a light.
    pub const SHADOW: RayMask = RayMask(1 << 1);
    /// Bounce rays after the first vertex.
    pub const INDIRECT: RayMask = RayMask(1 << 2);
    /// Every category.
    pub const ALL: RayMask = RayMask(u32::MAX);

    /// Whether a ray of mask `self` sees geometry of mask `other` (or the
    /// other way round — it is symmetric): they share a category.
    #[inline(always)]
    pub const fn sees(self, other: RayMask) -> bool {
        self.0 & other.0 != 0
    }
}

impl std::ops::BitOr for RayMask {
    type Output = RayMask;

    #[inline(always)]
    fn bitor(self, o: RayMask) -> RayMask {
        RayMask(self.0 | o.0)
    }
}

impl std::ops::BitAnd for RayMask {
    type Output = RayMask;

    #[inline(always)]
    fn bitand(self, o: RayMask) -> RayMask {
        RayMask(self.0 & o.0)
    }
}

impl std::ops::BitOrAssign for RayMask {
    #[inline(always)]
    fn bitor_assign(&mut self, o: RayMask) {
        self.0 |= o.0;
    }
}

impl std::ops::BitAndAssign for RayMask {
    #[inline(always)]
    fn bitand_assign(&mut self, o: RayMask) {
        self.0 &= o.0;
    }
}

/// [`RayMask::CAMERA`].
pub const MASK_CAMERA: RayMask = RayMask::CAMERA;
/// [`RayMask::SHADOW`].
pub const MASK_SHADOW: RayMask = RayMask::SHADOW;
/// [`RayMask::INDIRECT`].
pub const MASK_INDIRECT: RayMask = RayMask::INDIRECT;
/// [`RayMask::ALL`].
pub const MASK_ALL: RayMask = RayMask::ALL;

/// A ray: origin, (unnormalized) direction, shutter `time` in `[0, 1)` for
/// motion blur, the visibility category `mask`, and whether the ray passes
/// out of curve tubes unseen. Plain `Copy` data — renderer-side state like
/// the participating medium a path is inside belongs to the caller, not the
/// kernel.
#[derive(Clone, Copy, Debug)]
pub struct Ray {
    pub origin: Vec3A,
    pub dir: Vec3A,
    pub time: f32,
    pub mask: RayMask,
    /// Skip every curve hit at which the ray *leaves* a tube (its direction
    /// along the outward normal), keeping entries and every other kind of
    /// geometry. For rays leaving a fibre-scattering vertex, whose model
    /// already accounts for the light's path through its own strand: a ray
    /// starting outside any other tube must enter it before it can leave
    /// it, so this can only ever hide the strand the ray started in. Sits
    /// in the padding after `mask`, so the ray is no larger for it.
    pub ignore_curve_exits: bool,
}

impl Default for Ray {
    fn default() -> Self {
        Ray::new(Vec3A::ZERO, Vec3A::ZERO)
    }
}

impl Ray {
    /// A ray at shutter time 0, visible to all geometry.
    pub fn new(origin: Vec3A, dir: Vec3A) -> Ray {
        Ray {
            origin,
            dir,
            time: 0.0,
            mask: MASK_ALL,
            ignore_curve_exits: false,
        }
    }

    #[must_use = "returns a new ray; the original is unchanged"]
    pub fn with_time(mut self, time: f32) -> Ray {
        self.time = time;
        self
    }

    #[must_use = "returns a new ray; the original is unchanged"]
    pub fn with_mask(mut self, mask: RayMask) -> Ray {
        self.mask = mask;
        self
    }

    /// Same ray, passing out of curve tubes unseen or not — see
    /// [`Ray::ignore_curve_exits`].
    #[must_use = "returns a new ray; the original is unchanged"]
    pub fn with_curve_exits_ignored(mut self, ignore: bool) -> Ray {
        self.ignore_curve_exits = ignore;
        self
    }

    pub fn at(&self, t: f32) -> Vec3A {
        self.origin + t * self.dir
    }
}
