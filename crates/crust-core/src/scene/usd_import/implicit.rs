//! The implicit shapes — `Cube`, `Plane`, `Cylinder`, `Cylinder_1`, `Cone`,
//! `Capsule` and `Capsule_1` — tessellated into the meshes OpenUSD's
//! `pxr/imaging/geomUtil` generators produce, which is what Storm draws.
//!
//! The generators below transcribe GeomUtil's `_GeneratePointsImpl`,
//! `_GenerateNormalsImpl` and `GenerateTopology`: the same vertex order, the
//! cap rings split from the side, the seam at +X before the frame, the
//! `rightHanded` winding, and the `axis` frame of
//! `UsdImagingGprimAdapter::GetImplicitBasis`. `tests/geomutil_oracle.rs`
//! replays the meshes GeomUtil itself produced for a list of cases
//! (`scripts/geomutil_oracle.py`) through them, point for point.
//!
//! The result is a finished surface: the `catmullClark` scheme GeomUtil puts
//! on a curved shape's topology is not applied. Curved shapes carry GeomUtil's
//! smooth vertex normals; `Cube` and `Plane`, whose GeomUtil normals are
//! `uniform` and `constant`, carry none and shade faceted. See "Implicit
//! shapes" in `openspec/specs/usd-scene-import/design.md`.

use crate::warning;
use std::collections::BTreeMap;
use std::f64::consts::{FRAC_PI_2, PI, TAU};

use glam::Vec3;
use openusd::gf::Vec3f;
use openusd::usd::Prim;
use openusd_schemas::geom::{
    Axis as UsdAxis, Capsule, Capsule_1, Capsule_1Schema, CapsuleSchema, Cone, ConeSchema, Cube,
    CubeSchema, Cylinder, Cylinder_1, Cylinder_1Schema, CylinderSchema, Plane, PlaneSchema,
};
use tracing::debug;

use super::adaptive::{Aabb, Cull, ScreenProjection};
use super::attrs::attr_f64;
use super::materials::BoundMaterial;
use super::mesh::{MeshPlace, MeshSource};
use super::time::eval_time;

/// An implicit shape's spine (`Cylinder`, `Cone`, `Capsule`) or normal
/// (`Plane`) axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    X,
    Y,
    Z,
}

impl Axis {
    /// GeomUtil's point `(x, y, z)` in the shape's frame: `x·u + y·v + z·spine`
    /// for the rows `u`, `v`, `spine` of `GetImplicitBasis(axis)` — a cyclic
    /// permutation of the coordinates, so it rounds nothing.
    fn frame(self, [x, y, z]: [f64; 3]) -> [f64; 3] {
        match self {
            // u = Y, v = Z, spine = X.
            Axis::X => [z, x, y],
            // u = Z, v = X, spine = Y.
            Axis::Y => [y, z, x],
            // u = X, v = Y, spine = Z.
            Axis::Z => [x, y, z],
        }
    }
}

impl From<UsdAxis> for Axis {
    fn from(a: UsdAxis) -> Self {
        match a {
            UsdAxis::X => Axis::X,
            UsdAxis::Y => Axis::Y,
            UsdAxis::Z => Axis::Z,
        }
    }
}

/// One generated mesh, in the shape's local frame.
#[derive(Clone, Debug, PartialEq)]
pub struct Tessellation {
    pub points: Vec<[f32; 3]>,
    pub counts: Vec<i32>,
    pub indices: Vec<i32>,
    /// One smooth normal per point for a curved shape (GeomUtil's `vertex`
    /// normals); `None` for the cube and the plane, which shade faceted.
    pub normals: Option<Vec<[f32; 3]>>,
}

/// GeomUtil's `minNumRadial`: fewer radial segments generate nothing there.
pub const MIN_RADIAL: usize = 3;
/// GeomUtil's capsule `minNumCapAxial`.
pub const MIN_CAP_AXIAL: usize = 1;

/// Points (or normals) written through an optional frame, computed in `f64`
/// and stored as `f32`.
struct Writer {
    axis: Option<Axis>,
    out: Vec<[f32; 3]>,
}

impl Writer {
    fn new(axis: Option<Axis>, capacity: usize) -> Self {
        Writer {
            axis,
            out: Vec::with_capacity(capacity),
        }
    }

    fn write(&mut self, p: [f64; 3]) {
        let [x, y, z] = self.axis.map_or(p, |a| a.frame(p));
        self.out.push([x as f32, y as f32, z as f32]);
    }

    /// `ring` scaled by `scale` in XY, at height `z` (`_PointWriter::WriteArc`).
    fn arc(&mut self, scale: f64, ring: &[[f64; 2]], z: f64) {
        for &[x, y] in ring {
            self.write([scale * x, scale * y, z]);
        }
    }

    fn repeat(&mut self, p: [f64; 3], n: usize) {
        for _ in 0..n {
            self.write(p);
        }
    }
}

/// `_GenerateUnitArcXY` for a closed sweep: `n` points of the unit circle,
/// the first at +X, counter-clockwise seen from +Z.
fn unit_ring(n: usize) -> Vec<[f64; 2]> {
    (0..n)
        .map(|i| {
            let a = (i as f64 / n as f64) * TAU;
            [a.cos(), a.sin()]
        })
        .collect()
}

/// `_CapStyle`: whether a triangle fan closes an end of the tube, and whether
/// it shares its ring with the adjacent quad strip.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Cap {
    None,
    SharedEdge,
    SeparateEdge,
}

/// `_GenerateCappedQuadTopology` for a closed sweep: a fan around a bottom
/// pole, `strips` quad strips, and a fan around a top pole.
fn capped_quad_topology(n: usize, strips: usize, bottom: Cap, top: Cap) -> (Vec<i32>, Vec<i32>) {
    let fans = usize::from(bottom != Cap::None) + usize::from(top != Cap::None);
    let mut counts = Vec::with_capacity(n * (strips + fans));
    let mut indices = Vec::with_capacity(n * (4 * strips + 3 * fans));
    let i = |v: usize| v as i32;
    let mut pt = 0;
    if bottom != Cap::None {
        let pole = pt;
        pt += 1;
        for r in 0..n {
            counts.push(3);
            indices.extend([i(pt + (r + 1) % n), i(pt + r), i(pole)]);
        }
        if bottom == Cap::SeparateEdge {
            pt += n;
        }
    }
    for _ in 0..strips {
        for r in 0..n {
            counts.push(4);
            indices.extend([
                i(pt + r),
                i(pt + (r + 1) % n),
                i(pt + (r + 1) % n + n),
                i(pt + r + n),
            ]);
        }
        pt += n;
    }
    if top != Cap::None {
        if strips > 0 && top == Cap::SeparateEdge {
            pt += n;
        }
        let pole = pt + n;
        for r in 0..n {
            counts.push(3);
            indices.extend([i(pt + r), i(pt + (r + 1) % n), i(pole)]);
        }
    }
    (counts, indices)
}

/// `GeomUtilCuboidMeshGenerator` for a `size`-sided cube: eight shared
/// corners, six quads. Storm places it with no frame.
pub fn cuboid(size: f64) -> Tessellation {
    let h = 0.5 * size;
    let mut w = Writer::new(None, 8);
    for p in [
        [h, h, h],
        [-h, h, h],
        [-h, -h, h],
        [h, -h, h],
        [-h, -h, -h],
        [-h, h, -h],
        [h, h, -h],
        [h, -h, -h],
    ] {
        w.write(p);
    }
    Tessellation {
        points: w.out,
        counts: vec![4; 6],
        indices: vec![
            0, 1, 2, 3, 4, 5, 6, 7, 0, 6, 5, 1, 4, 7, 3, 2, 0, 3, 7, 6, 4, 2, 1, 5,
        ],
        normals: None,
    }
}

/// `GeomUtilPlaneMeshGenerator`: one `width` × `length` quad facing +`axis`.
pub fn plane(width: f64, length: f64, axis: Axis) -> Tessellation {
    let (x, y) = (0.5 * width, 0.5 * length);
    let mut w = Writer::new(Some(axis), 4);
    for p in [[x, y, 0.0], [-x, y, 0.0], [-x, -y, 0.0], [x, -y, 0.0]] {
        w.write(p);
    }
    Tessellation {
        points: w.out,
        counts: vec![4],
        indices: vec![0, 1, 2, 3],
        normals: None,
    }
}

/// The unit normal of a side whose radius goes from `r_bottom` to `r_top`
/// over `height`, as `(scale of the ring, latitude)`: `cos` and `sin` of the
/// side's tilt, computed without trigonometry as GeomUtil does.
fn side_tilt(r_bottom: f64, r_top: f64, height: f64) -> (f64, f64) {
    if height != 0.0 {
        let slope = (r_bottom - r_top) / height;
        let scale = 1.0 / (1.0 + slope * slope).sqrt();
        (scale, slope * scale)
    } else {
        // GeomUtil's "something sensible" for a degenerate side.
        (0.0, if r_bottom >= r_top { 1.0 } else { -1.0 })
    }
}

/// `GeomUtilCylinderMeshGenerator`, of `n` radial segments: a bottom fan, one
/// quad strip and a top fan, each fan on a ring of its own so the side's
/// smooth normals do not bend round the rims. `Cylinder` passes equal radii.
pub fn cylinder(n: usize, r_bottom: f64, r_top: f64, height: f64, axis: Axis) -> Tessellation {
    let n = n.max(MIN_RADIAL);
    let ring = unit_ring(n);
    let z = 0.5 * height;
    let len = 4 * n + 2;
    let mut p = Writer::new(Some(axis), len);
    p.write([0.0, 0.0, -z]);
    p.arc(r_bottom, &ring, -z);
    p.arc(r_bottom, &ring, -z);
    p.arc(r_top, &ring, z);
    p.arc(r_top, &ring, z);
    p.write([0.0, 0.0, z]);

    let (scale, latitude) = side_tilt(r_bottom, r_top, height);
    let mut nr = Writer::new(Some(axis), len);
    nr.repeat([0.0, 0.0, -1.0], 1 + n);
    nr.arc(scale, &ring, latitude);
    nr.arc(scale, &ring, latitude);
    nr.repeat([0.0, 0.0, 1.0], n + 1);

    let (counts, indices) = capped_quad_topology(n, 1, Cap::SeparateEdge, Cap::SeparateEdge);
    Tessellation {
        points: p.out,
        counts,
        indices,
        normals: Some(nr.out),
    }
}

/// `GeomUtilConeMeshGenerator`, of `n` radial segments: a bottom fan and one
/// strip of quads that close on a ring of `n` coincident apex points — not a
/// fan, so the side's normals stay smooth round the apex rather than
/// averaging to the spine. Not the `r_top = 0` case of [`cylinder`], which has
/// a top fan as well.
pub fn cone(n: usize, radius: f64, height: f64, axis: Axis) -> Tessellation {
    let n = n.max(MIN_RADIAL);
    let ring = unit_ring(n);
    let z = 0.5 * height;
    let len = 3 * n + 1;
    let mut p = Writer::new(Some(axis), len);
    p.write([0.0, 0.0, -z]);
    p.arc(radius, &ring, -z);
    p.arc(radius, &ring, -z);
    p.repeat([0.0, 0.0, z], n);

    let (scale, latitude) = if height != 0.0 {
        side_tilt(radius, 0.0, height)
    } else {
        (0.0, 1.0)
    };
    let mut nr = Writer::new(Some(axis), len);
    nr.repeat([0.0, 0.0, -1.0], 1 + n);
    nr.arc(scale, &ring, latitude);
    nr.arc(scale, &ring, latitude);

    let (counts, indices) = capped_quad_topology(n, 1, Cap::SeparateEdge, Cap::None);
    Tessellation {
        points: p.out,
        counts,
        indices,
        normals: Some(nr.out),
    }
}

/// `_ComputeNumBottomCapAxial`: of the `2·m` cap rings, the bottom cap's
/// share, in proportion to the part of a sphere it covers (rounded half to
/// even, as `GfRound`'s `rint` does), leaving the top at least
/// [`MIN_CAP_AXIAL`].
fn bottom_cap_rings(m: usize, latitude_range: f64) -> usize {
    let share = (2.0 * m as f64 * (FRAC_PI_2 + latitude_range) / PI).round_ties_even();
    // A negative share saturates to 0 in the cast, then clamps up.
    (share as usize).clamp(MIN_CAP_AXIAL, 2 * m - MIN_CAP_AXIAL)
}

/// A capsule's two spherical caps: their centres along the spine, their
/// radii, and the latitude where they meet the side.
struct Caps {
    offset0: f64,
    offset1: f64,
    radius0: f64,
    radius1: f64,
    latitude_range: f64,
}

impl Caps {
    /// GeomUtil's caps for a side from `r_bottom` to `r_top` over `height`:
    /// with unequal radii each cap is tangent to the sloping side, so its
    /// sphere is larger than the side's radius and its centre moves along
    /// the spine.
    fn of(r_bottom: f64, r_top: f64, height: f64) -> Caps {
        let (mut offset0, mut offset1) = (0.0, 0.0);
        let (mut radius0, mut radius1) = (r_bottom, r_top);
        let mut latitude_range = 0.0;
        if r_bottom != r_top && height != 0.0 {
            let slope = (r_bottom - r_top) / height;
            offset0 = -(slope * r_bottom);
            offset1 = -(slope * r_top);
            radius0 = (r_bottom * r_bottom + offset0 * offset0).sqrt();
            radius1 = (r_top * r_top + offset1 * offset1).sqrt();
            latitude_range = slope.atan();
        }
        Caps {
            offset0: offset0 - 0.5 * height,
            offset1: offset1 + 0.5 * height,
            radius0,
            radius1,
            latitude_range,
        }
    }
}

/// `GeomUtilCapsuleMeshGenerator`, of `n` radial segments and `m` axial
/// divisions per cap: a cylinder (or truncated cone) closed by two spherical
/// caps, all on shared rings. With unequal radii the caps are tangent to the
/// side, so their spheres are larger than the radii and the `2·m` rings are
/// split between them by how much of a sphere each covers.
pub fn capsule(
    n: usize,
    m: usize,
    r_bottom: f64,
    r_top: f64,
    height: f64,
    axis: Axis,
) -> Tessellation {
    let n = n.max(MIN_RADIAL);
    let m = m.max(MIN_CAP_AXIAL);
    let ring = unit_ring(n);
    let Caps {
        offset0,
        offset1,
        radius0,
        radius1,
        latitude_range,
    } = Caps::of(r_bottom, r_top, height);
    let rings0 = bottom_cap_rings(m, latitude_range);
    let rings1 = 2 * m - rings0;
    let lerp = |t: f64, a: f64, b: f64| (1.0 - t) * a + t * b;
    // Latitudes (-π/2, range] on the bottom cap, [range, π/2) on the top.
    let bottom = (1..=rings0).map(|k| lerp(k as f64 / rings0 as f64, -FRAC_PI_2, latitude_range));
    let top = (0..rings1).map(|k| lerp(k as f64 / rings1 as f64, latitude_range, FRAC_PI_2));

    let len = 2 * m * n + 2;
    let mut p = Writer::new(Some(axis), len);
    let mut nr = Writer::new(Some(axis), len);
    p.write([0.0, 0.0, offset0 - radius0]);
    nr.write([0.0, 0.0, -1.0]);
    for lat in bottom {
        let (s, c) = lat.sin_cos();
        p.arc(radius0 * c, &ring, offset0 + radius0 * s);
        nr.arc(c, &ring, s);
    }
    for lat in top {
        let (s, c) = lat.sin_cos();
        p.arc(radius1 * c, &ring, offset1 + radius1 * s);
        nr.arc(c, &ring, s);
    }
    p.write([0.0, 0.0, offset1 + radius1]);
    nr.write([0.0, 0.0, 1.0]);

    let (counts, indices) =
        capped_quad_topology(n, 2 * (m - 1) + 1, Cap::SharedEdge, Cap::SharedEdge);
    Tessellation {
        points: p.out,
        counts,
        indices,
        normals: Some(nr.out),
    }
}

/// An implicit shape prim's authored parameters, read at the render's time
/// code with the schema fallbacks, grouped by the generator that tessellates
/// it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Shape {
    /// `Cube`.
    Cuboid { size: f64 },
    /// `Plane`.
    Plane { width: f64, length: f64, axis: Axis },
    /// `Cylinder` (equal radii) and `Cylinder_1`.
    Cylinder {
        r_bottom: f64,
        r_top: f64,
        height: f64,
        axis: Axis,
    },
    /// `Cone`.
    Cone {
        radius: f64,
        height: f64,
        axis: Axis,
    },
    /// `Capsule` (equal radii) and `Capsule_1`.
    Capsule {
        r_bottom: f64,
        r_top: f64,
        height: f64,
        axis: Axis,
    },
}

/// `attr` at the render's time code, else the schema fallback `or`.
fn length_or(attr: &openusd::usd::Attribute, or: f64) -> f64 {
    attr_f64(attr).unwrap_or(or)
}

/// `axis` at the render's time code, unauthored (or blocked) reading the
/// schema fallback, `Z`.
fn axis_of(attr: &openusd::usd::Attribute) -> Axis {
    attr.get_at::<UsdAxis>(eval_time())
        .ok()
        .flatten()
        .unwrap_or_default()
        .into()
}

impl Shape {
    /// `prim` as an implicit shape, or `None` when it is none of the seven.
    ///
    /// Every prim the walks do not read otherwise comes through here — each
    /// transform, material and shader — so one `Gprim` test turns those away
    /// before the seven schema tests.
    pub(super) fn read(prim: &Prim) -> Option<Shape> {
        if !prim.is_a("Gprim").unwrap_or(false) {
            return None;
        }
        let prim = || prim.clone();
        if let Ok(Some(s)) = Cube::from_prim(prim()) {
            return Some(Shape::Cuboid {
                size: length_or(&s.size_attr(), 2.0),
            });
        }
        if let Ok(Some(s)) = Plane::from_prim(prim()) {
            return Some(Shape::Plane {
                width: length_or(&s.width_attr(), 2.0),
                length: length_or(&s.length_attr(), 2.0),
                axis: axis_of(&s.axis_attr()),
            });
        }
        if let Ok(Some(s)) = Cylinder::from_prim(prim()) {
            let radius = length_or(&s.radius_attr(), 1.0);
            return Some(Shape::Cylinder {
                r_bottom: radius,
                r_top: radius,
                height: length_or(&s.height_attr(), 2.0),
                axis: axis_of(&s.axis_attr()),
            });
        }
        if let Ok(Some(s)) = Cylinder_1::from_prim(prim()) {
            return Some(Shape::Cylinder {
                r_bottom: length_or(&s.radius_bottom_attr(), 1.0),
                r_top: length_or(&s.radius_top_attr(), 1.0),
                height: length_or(&s.height_attr(), 2.0),
                axis: axis_of(&s.axis_attr()),
            });
        }
        if let Ok(Some(s)) = Cone::from_prim(prim()) {
            return Some(Shape::Cone {
                radius: length_or(&s.radius_attr(), 1.0),
                height: length_or(&s.height_attr(), 2.0),
                axis: axis_of(&s.axis_attr()),
            });
        }
        if let Ok(Some(s)) = Capsule::from_prim(prim()) {
            let radius = length_or(&s.radius_attr(), 0.5);
            return Some(Shape::Capsule {
                r_bottom: radius,
                r_top: radius,
                height: length_or(&s.height_attr(), 1.0),
                axis: axis_of(&s.axis_attr()),
            });
        }
        if let Ok(Some(s)) = Capsule_1::from_prim(prim()) {
            return Some(Shape::Capsule {
                r_bottom: length_or(&s.radius_bottom_attr(), 0.5),
                r_top: length_or(&s.radius_top_attr(), 0.5),
                height: length_or(&s.height_attr(), 1.0),
                axis: axis_of(&s.axis_attr()),
            });
        }
        None
    }

    /// The largest radius of a curved shape, which its radial count is chosen
    /// for; `None` for the cube and the plane, which have no radial count.
    pub(super) fn r_max(&self) -> Option<f64> {
        match *self {
            Shape::Cuboid { .. } | Shape::Plane { .. } => None,
            Shape::Cylinder {
                r_bottom, r_top, ..
            }
            | Shape::Capsule {
                r_bottom, r_top, ..
            } => Some(r_bottom.abs().max(r_top.abs())),
            Shape::Cone { radius, .. } => Some(radius.abs()),
        }
    }

    /// The mesh at `n` radial segments (ignored by the cube and the plane),
    /// a capsule's caps taking [`cap_rings`]`(n)` divisions each.
    pub(super) fn tessellate(&self, n: usize) -> Tessellation {
        match *self {
            Shape::Cuboid { size } => cuboid(size),
            Shape::Plane {
                width,
                length,
                axis,
            } => plane(width, length, axis),
            Shape::Cylinder {
                r_bottom,
                r_top,
                height,
                axis,
            } => cylinder(n, r_bottom, r_top, height, axis),
            Shape::Cone {
                radius,
                height,
                axis,
            } => cone(n, radius, height, axis),
            Shape::Capsule {
                r_bottom,
                r_top,
                height,
                axis,
            } => capsule(n, cap_rings(n), r_bottom, r_top, height, axis),
        }
    }
}

impl Shape {
    /// The schema the shape was read from, for the log.
    fn family(&self) -> &'static str {
        match self {
            Shape::Cuboid { .. } => "Cube",
            Shape::Plane { .. } => "Plane",
            Shape::Cylinder { .. } => "Cylinder",
            Shape::Cone { .. } => "Cone",
            Shape::Capsule { .. } => "Capsule",
        }
    }

    /// The box around the shape's true surface in its local frame, whatever
    /// it is tessellated to: what its size on screen is measured from.
    pub(super) fn local_bounds(&self) -> Aabb {
        // In GeomUtil's frame, before `axis`: (x, y) round the spine, z along it.
        let (half_x, half_y, z0, z1, axis) = match *self {
            Shape::Cuboid { size } => {
                let h = 0.5 * size.abs();
                (h, h, -h, h, Axis::Z)
            }
            Shape::Plane {
                width,
                length,
                axis,
            } => (0.5 * width.abs(), 0.5 * length.abs(), 0.0, 0.0, axis),
            Shape::Cylinder {
                r_bottom,
                r_top,
                height,
                axis,
            } => {
                let r = r_bottom.abs().max(r_top.abs());
                let h = 0.5 * height.abs();
                (r, r, -h, h, axis)
            }
            Shape::Cone {
                radius,
                height,
                axis,
            } => {
                let h = 0.5 * height.abs();
                (radius.abs(), radius.abs(), -h, h, axis)
            }
            Shape::Capsule {
                r_bottom,
                r_top,
                height,
                axis,
            } => {
                // A cap whose sphere reaches past the side's radius (unequal
                // radii) bulges to its sphere's radius at its equator.
                let c = Caps::of(r_bottom, r_top, height);
                let r = c.radius0.abs().max(c.radius1.abs());
                let ends = [
                    c.offset0 - c.radius0,
                    c.offset0 + c.radius0,
                    c.offset1 - c.radius1,
                    c.offset1 + c.radius1,
                ];
                let z0 = ends.iter().copied().fold(f64::INFINITY, f64::min);
                let z1 = ends.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                (r, r, z0, z1, axis)
            }
        };
        // `frame` permutes coordinates, so it carries a box's corners to
        // its corners.
        let [ax, ay, az] = axis.frame([-half_x, -half_y, z0]);
        let [bx, by, bz] = axis.frame([half_x, half_y, z1]);
        Aabb {
            min: Vec3::new(ax as f32, ay as f32, az as f32),
            max: Vec3::new(bx as f32, by as f32, bz as f32),
        }
    }
}

/// The largest error, in pixels, a curved shape's silhouette may have on
/// screen: the sagitta of one radial segment's chord.
pub(super) const EPSILON_PX: f64 = 0.5;
/// The fewest radial segments an unshared curved shape takes: Storm uses 10;
/// 16 keeps reflections of shapes off screen round enough.
pub(super) const N_MIN: usize = 16;
/// The most: a sagitta of 7.5e-5 of the radius, under [`EPSILON_PX`] up to a
/// radius of about 6 600 px.
pub(super) const N_MAX: usize = 256;
/// The count of a shared shape, and of every shape in a load with no camera
/// named before the traversal: a chord error of 0.2% of the radius.
pub(super) const N_FIXED: usize = 48;

/// The smallest radial count, within `N_MIN..=N_MAX`, at which a circle of
/// `r_max` local units seen at `sigma` pixels per unit has a sagitta of at
/// most [`EPSILON_PX`]: `ceil(π / acos(1 − ε/ρ))` for a radius of `ρ` pixels.
/// Off screen (`sigma = 0`) or under `ε` it is `N_MIN`; with the camera
/// inside the shape's box (`sigma = ∞`), `N_MAX`.
pub(super) fn radial_count_at(sigma: f32, r_max: f64) -> usize {
    if sigma == f32::INFINITY {
        return N_MAX;
    }
    let rho = r_max * f64::from(sigma);
    if rho.is_nan() || rho <= EPSILON_PX {
        return N_MIN;
    }
    let n = (PI / (1.0 - EPSILON_PX / rho).acos()).ceil();
    // A float cast saturates, so a huge count lands on the clamp.
    (n as usize).clamp(N_MIN, N_MAX)
}

/// The sagitta, in pixels, of one chord of a circle of `rho` pixels' radius
/// drawn with `n` segments: how far its outline strays inside the circle.
#[cfg(test)]
fn sagitta_px(rho: f64, n: usize) -> f64 {
    rho * (1.0 - (PI / n as f64).cos())
}

/// How this load tessellates implicit shapes: the render camera's projection,
/// when it was named before the traversal, and what was tessellated.
#[derive(Default)]
pub(super) struct ImplicitPolicy {
    /// The camera's projection, which sizes each unshared curved shape;
    /// `None` gives every shape [`N_FIXED`].
    pub(super) projection: Option<ScreenProjection>,
    /// Curved shapes read, by radial count: a direct prim once per
    /// placement, a prototype's shape once per version built.
    pub(super) counts: BTreeMap<usize, u64>,
    /// Shapes read, the cube and the plane included.
    pub(super) shapes: u64,
    /// Triangles generated for them, before any sharing.
    pub(super) triangles: u64,
    /// Whether any count came from the projection: a change of camera or
    /// resolution then changes the geometry (see `live`).
    pub(super) on_screen: bool,
}

impl ImplicitPolicy {
    pub(super) fn new(projection: Option<ScreenProjection>) -> Self {
        ImplicitPolicy {
            projection,
            ..ImplicitPolicy::default()
        }
    }

    /// The radial count of a curved shape whose largest radius is `r_max`
    /// and whose local box is `bounds`, read for `place`: from its size on
    /// screen when it is unshared and the camera is known, else [`N_FIXED`].
    /// `None` when the count does not depend on the camera.
    fn radial_count(
        &self,
        place: MeshPlace<'_>,
        bounds: &Aabb,
        r_max: f64,
    ) -> (usize, Option<f32>) {
        match (&self.projection, place) {
            (Some(p), MeshPlace::World(xf)) => {
                let sigma = p.sigma_culled(xf, bounds, Cull::Pad(0.0));
                (radial_count_at(sigma, r_max), Some(sigma))
            }
            _ => (N_FIXED, None),
        }
    }

    /// `shape`'s mesh read for `place`, as the mesh path consumes it: never
    /// subdivided, with no texture chart, smooth when the shape is curved.
    /// `at` names the prim in the log.
    pub(super) fn source(
        &mut self,
        at: &openusd::sdf::Path,
        shape: &Shape,
        place: MeshPlace<'_>,
    ) -> MeshSource {
        let n = match shape.r_max() {
            Some(r_max) => {
                let (n, sigma) = self.radial_count(place, &shape.local_bounds(), r_max);
                self.on_screen |= sigma.is_some();
                *self.counts.entry(n).or_default() += 1;
                debug!(
                    "{} at {at}: {n} radial segments ({})",
                    shape.family(),
                    sigma.map_or("shared, or no camera".to_owned(), |s| format!(
                        "{s} px per unit"
                    ))
                );
                n
            }
            None => 0,
        };
        let t = shape.tessellate(n);
        self.shapes += 1;
        self.triangles += t.counts.iter().map(|&c| c as u64 - 2).sum::<u64>();
        MeshSource {
            points: t.points.into_iter().map(Vec3f::from).collect(),
            base_face_count: t.counts.len(),
            counts: t.counts,
            indices: t.indices,
            normals: t.normals,
            subdiv_faces: None,
            uvs: None,
            refined: false,
        }
    }
}

/// `bound` without its displacement, which an implicit shape does not apply:
/// it has no chart to displace along, and a warning names the prim.
pub(super) fn undisplaced(prim: &Prim, bound: BoundMaterial) -> BoundMaterial {
    if bound.displacement.is_some() {
        warning!(
            GeomImplicitDisplacement,
            at = prim.path(),
            "{} is an implicit shape: its material's displacement is not applied",
            prim.path()
        );
    }
    BoundMaterial {
        material: bound.material,
        displacement: None,
    }
}

/// A capsule's axial divisions per cap at `n` radial segments: a quarter as
/// many, rounded up, at least two — so a cap's rings are about as far apart
/// along its meridian as its segments are round its equator.
pub(super) fn cap_rings(n: usize) -> usize {
    n.div_ceil(4).max(2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::usd_import::adaptive::Frustum;
    use crate::scene::usd_import::{ValidOptions, prim_at, read_index, stage_builder};
    use glam::Mat4 as GMat4;
    use openusd::sdf;
    use openusd::usd::InitialLoadSet;

    /// The shapes `body` defines under `/W`, read from a stage on disk.
    fn read_all(name: &str, body: &str) -> Vec<(String, Option<Shape>)> {
        let dir = std::env::temp_dir().join("crust_implicit_tests");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join(format!("{name}.usda"));
        std::fs::write(&path, format!("#usda 1.0\ndef Xform \"W\"\n{{\n{body}}}\n"))
            .expect("write stage");
        let stage = stage_builder()
            .open(path.to_str().unwrap())
            .expect("stage opens");
        let root = prim_at(&stage, sdf::path("/W").unwrap());
        root.children()
            .expect("children")
            .into_iter()
            .map(|p| (p.path().to_string(), Shape::read(&p)))
            .collect()
    }

    #[test]
    fn every_schema_reads_its_fallbacks() {
        let shapes = read_all(
            "fallbacks",
            r#"    def Cube "Cube" {}
    def Plane "Plane" {}
    def Cylinder "Cylinder" {}
    def Cylinder_1 "Cylinder_1" {}
    def Cone "Cone" {}
    def Capsule "Capsule" {}
    def Capsule_1 "Capsule_1" {}
    def Xform "Xform" {}
"#,
        );
        let z = Axis::Z;
        let want = [
            ("/W/Cube", Some(Shape::Cuboid { size: 2.0 })),
            (
                "/W/Plane",
                Some(Shape::Plane {
                    width: 2.0,
                    length: 2.0,
                    axis: z,
                }),
            ),
            (
                "/W/Cylinder",
                Some(Shape::Cylinder {
                    r_bottom: 1.0,
                    r_top: 1.0,
                    height: 2.0,
                    axis: z,
                }),
            ),
            (
                "/W/Cylinder_1",
                Some(Shape::Cylinder {
                    r_bottom: 1.0,
                    r_top: 1.0,
                    height: 2.0,
                    axis: z,
                }),
            ),
            (
                "/W/Cone",
                Some(Shape::Cone {
                    radius: 1.0,
                    height: 2.0,
                    axis: z,
                }),
            ),
            (
                "/W/Capsule",
                Some(Shape::Capsule {
                    r_bottom: 0.5,
                    r_top: 0.5,
                    height: 1.0,
                    axis: z,
                }),
            ),
            (
                "/W/Capsule_1",
                Some(Shape::Capsule {
                    r_bottom: 0.5,
                    r_top: 0.5,
                    height: 1.0,
                    axis: z,
                }),
            ),
            ("/W/Xform", None),
        ];
        for (got, (path, shape)) in shapes.iter().zip(want) {
            assert_eq!(got.0, path);
            assert_eq!(got.1, shape, "{path}");
        }
    }

    #[test]
    fn authored_values_are_read_in_either_precision() {
        let shapes = read_all(
            "authored",
            r#"    def Cylinder "Cylinder" { double radius = 0.25  float height = 3  token axis = "X" }
    def Cylinder_1 "Cone1" { double radiusBottom = 1  double radiusTop = 0  double height = 4 }
    def Cone "Cone" { double radius = 2  token axis = "Y" }
    def Capsule_1 "Capsule_1" { float radiusBottom = 0.75  double radiusTop = 0.25 }
    def Cube "Cube" { float size = 0.5 }
    def Plane "Plane" { double width = 3  double length = 1  token axis = "X" }
"#,
        );
        let got: Vec<Option<Shape>> = shapes.into_iter().map(|(_, s)| s).collect();
        assert_eq!(
            got,
            [
                // A `Cylinder` has one radius: both ends take it.
                Some(Shape::Cylinder {
                    r_bottom: 0.25,
                    r_top: 0.25,
                    height: 3.0,
                    axis: Axis::X,
                }),
                // A `Cylinder_1` with radiusTop 0 is a cone, tessellated as a
                // cylinder.
                Some(Shape::Cylinder {
                    r_bottom: 1.0,
                    r_top: 0.0,
                    height: 4.0,
                    axis: Axis::Z,
                }),
                Some(Shape::Cone {
                    radius: 2.0,
                    height: 2.0,
                    axis: Axis::Y,
                }),
                Some(Shape::Capsule {
                    r_bottom: 0.75,
                    r_top: 0.25,
                    height: 1.0,
                    axis: Axis::Z,
                }),
                Some(Shape::Cuboid { size: 0.5 }),
                Some(Shape::Plane {
                    width: 3.0,
                    length: 1.0,
                    axis: Axis::X,
                }),
            ]
        );
    }

    #[test]
    fn counts_follow_geomutil() {
        assert_eq!(cylinder(8, 1.0, 1.0, 2.0, Axis::Z).points.len(), 34);
        assert_eq!(cone(8, 1.0, 2.0, Axis::Z).points.len(), 25);
        let c = capsule(10, 4, 0.5, 0.5, 1.0, Axis::Z);
        assert_eq!(c.points.len(), 2 * 4 * 10 + 2);
        assert_eq!(c.counts.len(), 10 * (2 * 3 + 1) + 2 * 10);
        // Each topology indexes exactly its points.
        for t in [
            cuboid(2.0),
            plane(2.0, 2.0, Axis::Y),
            cylinder(5, 1.0, 0.3, 1.0, Axis::X),
            cone(5, 1.0, 1.0, Axis::Y),
            capsule(7, 3, 1.0, 0.2, 2.0, Axis::Z),
        ] {
            let max = *t.indices.iter().max().unwrap() as usize;
            assert_eq!(max + 1, t.points.len());
            let corners: i32 = t.counts.iter().sum();
            assert_eq!(corners as usize, t.indices.len());
            if let Some(n) = &t.normals {
                assert_eq!(n.len(), t.points.len());
            }
        }
    }

    #[test]
    fn caps_take_a_quarter_of_the_radial_count() {
        assert_eq!(cap_rings(3), 2);
        assert_eq!(cap_rings(8), 2);
        assert_eq!(cap_rings(16), 4);
        assert_eq!(cap_rings(17), 5);
        assert_eq!(cap_rings(48), 12);
    }

    /// The projection [`read_index`] resolves for a stage, and the warnings
    /// the read raised.
    fn projection_of(name: &str, text: &str) -> (Option<ScreenProjection>, Vec<crate::Warning>) {
        let dir = std::env::temp_dir().join("crust_implicit_tests");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join(format!("{name}.usda"));
        std::fs::write(&path, text).expect("write stage");
        let path_str = path.to_str().unwrap();
        let scope = crate::WarningScope::enter();
        let index = stage_builder()
            .load(InitialLoadSet::LoadNone)
            .open(path_str)
            .expect("stage opens");
        let options = crate::UsdImportOptions::default();
        let valid = ValidOptions::check(&options).expect("valid options");
        let read = read_index(&index, &path, path_str, &options, &valid).expect("index read");
        assert!(read.subdiv.adaptive.is_none(), "no edge length: uniform");
        (read.projection, scope.finish())
    }

    /// A camera named by `RenderSettings.camera` gives a projection with
    /// no edge length set; no named camera gives none, silently.
    #[test]
    fn a_named_camera_gives_a_projection_without_adaptive_subdivision() {
        let camera = r#"def Camera "cam"
{
    double3 xformOp:translate = (0, 0, 10)
    uniform token[] xformOpOrder = ["xformOp:translate"]
}
"#;
        let (named, warnings) = projection_of(
            "named_camera",
            &format!(
                r#"#usda 1.0
(
    renderSettingsPrimPath = "/Render/settings"
)
{camera}
def Scope "Render"
{{
    def RenderSettings "settings"
    {{
        rel camera = </cam>
        int2 resolution = (200, 100)
    }}
}}
"#
            ),
        );
        let p = named.expect("a projection");
        assert!(
            p.eye.abs_diff_eq(Vec3::new(0.0, 0.0, 10.0), 1e-6),
            "{:?}",
            p.eye
        );
        assert!(p.f_px > 0.0 && p.f_px.is_finite());
        assert!(warnings.is_empty(), "{warnings:#?}");

        let (unnamed, warnings) = projection_of(
            "unnamed_camera",
            &format!(
                "#usda 1.0
{camera}"
            ),
        );
        assert!(
            unnamed.is_none(),
            "the first camera is not known before the traversal"
        );
        assert!(warnings.is_empty(), "{warnings:#?}");
    }

    /// A camera at the origin looking down −Z, 1000 px per unit at unit
    /// distance, with or without its ±45° view pyramid.
    fn projection(frustum: bool) -> ScreenProjection {
        ScreenProjection {
            eye: Vec3::ZERO,
            f_px: 1000.0,
            frustum: frustum.then(|| Frustum::new(Vec3::ZERO, Vec3::NEG_Z, Vec3::Y, 1.0, 1.0)),
        }
    }

    /// The radial count of a unit cylinder placed at `at`.
    fn count_at(policy: &ImplicitPolicy, at: Vec3) -> usize {
        let shape = Shape::Cylinder {
            r_bottom: 1.0,
            r_top: 1.0,
            height: 2.0,
            axis: Axis::Z,
        };
        let xf = GMat4::from_translation(at);
        policy
            .radial_count(MeshPlace::World(&xf), &shape.local_bounds(), 1.0)
            .0
    }

    /// "Larger on screen means more segments": the count never rises
    /// with distance, and does fall over the range it is not clamped in.
    #[test]
    fn the_count_falls_with_distance() {
        let policy = ImplicitPolicy::new(Some(projection(true)));
        // ρ = 1000 / (d − 1) px for the unit radius: N_MAX inside d ≈ 1.15,
        // N_MIN beyond d ≈ 39.
        let counts: Vec<usize> = [1.05, 1.2, 1.5, 2.0, 3.0, 5.0, 10.0, 20.0, 50.0, 100.0]
            .iter()
            .map(|&d| count_at(&policy, Vec3::new(0.0, 0.0, -d)))
            .collect();
        assert!(counts.windows(2).all(|w| w[0] >= w[1]), "{counts:?}");
        assert_eq!(
            (counts[0], counts[counts.len() - 1]),
            (N_MAX, N_MIN),
            "{counts:?}"
        );
        assert!(counts.iter().any(|&n| N_MIN < n && n < N_MAX), "{counts:?}");
        // Ten times closer: at least as many, and more where not clamped.
        let (near, far) = (
            count_at(&policy, Vec3::new(0.0, 0.0, -2.0)),
            count_at(&policy, Vec3::new(0.0, 0.0, -20.0)),
        );
        assert!(near > far, "{near} vs {far}");
    }

    /// Off screen (σ = 0) the count is `N_MIN`; with the camera inside
    /// the shape's box (σ = ∞) it is `N_MAX`.
    #[test]
    fn the_count_is_clamped_off_screen_and_inside() {
        let policy = ImplicitPolicy::new(Some(projection(true)));
        assert_eq!(
            count_at(&policy, Vec3::new(0.0, 0.0, 30.0)),
            N_MIN,
            "behind"
        );
        assert_eq!(
            count_at(&policy, Vec3::new(0.0, 0.0, -1.1)),
            N_MAX,
            "very close"
        );
        assert_eq!(count_at(&policy, Vec3::ZERO), N_MAX, "inside");
        assert_eq!(radial_count_at(0.0, 1.0), N_MIN);
        assert_eq!(radial_count_at(f32::INFINITY, 1.0), N_MAX);
        assert_eq!(radial_count_at(f32::INFINITY, 0.0), N_MAX);
        assert_eq!(radial_count_at(1e30, 1.0), N_MAX, "a huge count saturates");
    }

    /// "Off screen with the frustum test off": under
    /// `CRUST_ADAPTIVE_FRUSTUM=0` the projection has no view pyramid
    /// (`subdiv_policy`), and a shape behind the camera is sized by its
    /// distance like one in front of it.
    #[test]
    fn without_the_frustum_test_off_screen_is_sized_by_distance() {
        let policy = ImplicitPolicy::new(Some(projection(false)));
        let behind = count_at(&policy, Vec3::new(0.0, 0.0, 5.0));
        let ahead = count_at(&policy, Vec3::new(0.0, 0.0, -5.0));
        assert_eq!(behind, ahead);
        assert!(behind > N_MIN, "{behind}");
    }

    /// A shared placement, and every placement with no projection, take
    /// `N_FIXED`, and do not mark the load as sized on screen.
    #[test]
    fn shared_and_cameraless_shapes_take_the_fixed_count() {
        let shape = Shape::Capsule {
            r_bottom: 1.0,
            r_top: 1.0,
            height: 2.0,
            axis: Axis::Y,
        };
        let near = GMat4::from_translation(Vec3::new(0.0, 0.0, -3.0));
        let bounds = shape.local_bounds();
        let with = ImplicitPolicy::new(Some(projection(true)));
        assert_eq!(
            with.radial_count(MeshPlace::Shared, &bounds, 1.0),
            (N_FIXED, None)
        );
        let without = ImplicitPolicy::new(None);
        assert_eq!(
            without.radial_count(MeshPlace::World(&near), &bounds, 1.0),
            (N_FIXED, None)
        );
        let mut shared = ImplicitPolicy::new(Some(projection(true)));
        let at = sdf::path("/W/Pill").unwrap();
        let src = shared.source(&at, &shape, MeshPlace::Shared);
        assert!(!shared.on_screen);
        // A capsule's caps follow the radial count: N_FIXED / 4 rings each.
        assert_eq!(src.points.len(), 2 * cap_rings(N_FIXED) * N_FIXED + 2);
        let mut placed = ImplicitPolicy::new(Some(projection(true)));
        placed.source(&at, &shape, MeshPlace::World(&near));
        assert!(placed.on_screen);
    }

    /// The chosen count keeps the silhouette's sagitta within `ε`
    /// pixels, and one segment fewer would not — unless clamped.
    #[test]
    fn the_count_is_the_smallest_within_the_budget() {
        let mut px = 0.25_f32;
        while px < 1e5 {
            let n = radial_count_at(px, 1.0);
            let rho = f64::from(px);
            if n < N_MAX {
                assert!(sagitta_px(rho, n) <= EPSILON_PX + 1e-9, "ρ {rho}: {n}");
            }
            if n > N_MIN && n < N_MAX {
                assert!(sagitta_px(rho, n - 1) > EPSILON_PX, "ρ {rho}: {n} - 1");
            }
            px *= 1.37;
        }
    }

    /// The local box holds the shape's surface on every axis: the mesh at a
    /// fine count lies inside it and touches each face.
    #[test]
    fn local_bounds_hold_the_surface() {
        for shape in [
            Shape::Cuboid { size: 2.0 },
            Shape::Plane {
                width: 3.0,
                length: 1.0,
                axis: Axis::X,
            },
            Shape::Cylinder {
                r_bottom: 1.0,
                r_top: 0.25,
                height: 2.0,
                axis: Axis::Y,
            },
            Shape::Cone {
                radius: 0.5,
                height: 3.0,
                axis: Axis::X,
            },
            Shape::Capsule {
                r_bottom: 1.0,
                r_top: 0.25,
                height: 1.0,
                axis: Axis::Z,
            },
            Shape::Capsule {
                r_bottom: 0.2,
                r_top: 0.8,
                height: 0.5,
                axis: Axis::Y,
            },
        ] {
            let b = shape.local_bounds();
            let t = shape.tessellate(N_MAX);
            let mesh = Aabb::of_arrays(&t.points).unwrap();
            assert!(
                mesh.min.cmpge(b.min - 1e-5).all() && mesh.max.cmple(b.max + 1e-5).all(),
                "{shape:?}: {mesh:?} outside {b:?}"
            );
            assert!(
                mesh.min.abs_diff_eq(b.min, 1e-3) && mesh.max.abs_diff_eq(b.max, 1e-3),
                "{shape:?}: {mesh:?} short of {b:?}"
            );
        }
    }
}
