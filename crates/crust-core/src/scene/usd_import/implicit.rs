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

use std::f64::consts::{FRAC_PI_2, PI, TAU};

use openusd::usd::{Prim, Stage};
use openusd_schemas::geom::{
    Axis as UsdAxis, Capsule, Capsule_1, Capsule_1Schema, CapsuleSchema, Cone, ConeSchema, Cube,
    CubeSchema, Cylinder, Cylinder_1, Cylinder_1Schema, CylinderSchema, Plane, PlaneSchema,
};

use super::attrs::attr_f64;
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
    offset0 -= 0.5 * height;
    offset1 += 0.5 * height;
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
    pub(super) fn read(stage: &Stage, prim: &Prim) -> Option<Shape> {
        let path = || prim.path().clone();
        if let Ok(Some(s)) = Cube::get(stage, path()) {
            return Some(Shape::Cuboid {
                size: length_or(&s.size_attr(), 2.0),
            });
        }
        if let Ok(Some(s)) = Plane::get(stage, path()) {
            return Some(Shape::Plane {
                width: length_or(&s.width_attr(), 2.0),
                length: length_or(&s.length_attr(), 2.0),
                axis: axis_of(&s.axis_attr()),
            });
        }
        if let Ok(Some(s)) = Cylinder::get(stage, path()) {
            let radius = length_or(&s.radius_attr(), 1.0);
            return Some(Shape::Cylinder {
                r_bottom: radius,
                r_top: radius,
                height: length_or(&s.height_attr(), 2.0),
                axis: axis_of(&s.axis_attr()),
            });
        }
        if let Ok(Some(s)) = Cylinder_1::get(stage, path()) {
            return Some(Shape::Cylinder {
                r_bottom: length_or(&s.radius_bottom_attr(), 1.0),
                r_top: length_or(&s.radius_top_attr(), 1.0),
                height: length_or(&s.height_attr(), 2.0),
                axis: axis_of(&s.axis_attr()),
            });
        }
        if let Ok(Some(s)) = Cone::get(stage, path()) {
            return Some(Shape::Cone {
                radius: length_or(&s.radius_attr(), 1.0),
                height: length_or(&s.height_attr(), 2.0),
                axis: axis_of(&s.axis_attr()),
            });
        }
        if let Ok(Some(s)) = Capsule::get(stage, path()) {
            let radius = length_or(&s.radius_attr(), 0.5);
            return Some(Shape::Capsule {
                r_bottom: radius,
                r_top: radius,
                height: length_or(&s.height_attr(), 1.0),
                axis: axis_of(&s.axis_attr()),
            });
        }
        if let Ok(Some(s)) = Capsule_1::get(stage, path()) {
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

/// A capsule's axial divisions per cap at `n` radial segments: a quarter as
/// many, rounded up, at least two — so a cap's rings are about as far apart
/// along its meridian as its segments are round its equator.
pub(super) fn cap_rings(n: usize) -> usize {
    n.div_ceil(4).max(2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::usd_import::{prim_at, stage_builder};
    use openusd::sdf;

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
            .map(|p| (p.path().to_string(), Shape::read(&stage, &p)))
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
}
