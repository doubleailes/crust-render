//! The Embree-shaped public API: attach [`Geometry`] objects to a
//! [`SceneBuilder`], `commit()` into an immutable [`Scene`], query with
//! `intersect` / `occluded`.

use crate::aabb::AABB;
use crate::bvh::Bvh;
use crate::prim::{
    CubicCurvePrim, CurvePrim, InstancePrim, PrimHit, PrimNode, SpherePrim, TrianglePrim,
    transformed_aabb,
};
use crate::ray::{MASK_ALL, Ray};
use glam::{Affine3A, Vec3A};
use std::sync::Arc;

/// One round (sphere-swept) curve segment: a cone frustum tangent to the
/// spheres `(p0, r0)` and `(p1, r1)`, with spherical caps.
#[derive(Clone, Copy, Debug)]
pub struct CurveSegment {
    pub p0: Vec3A,
    pub p1: Vec3A,
    pub r0: f32,
    pub r1: f32,
}

/// Top-level primitives of a committed [`Scene`], split by kind. See
/// [`Scene::primitive_breakdown`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PrimitiveBreakdown {
    pub triangles: usize,
    pub spheres: usize,
    pub curve_segments: usize,
    pub cubic_curve_spans: usize,
    pub instances: usize,
}

/// One authored cubic curve span — its own Bézier control points and end
/// radii, intersected analytically (`crate::curve::cubic_curve_intersect`)
/// rather than pre-flattened into several [`CurveSegment`]s. Round, same
/// as `RoundCurves` — this only changes how a span is stored and
/// intersected, not the surface it represents.
#[derive(Clone, Copy, Debug)]
pub struct CubicCurveSegment {
    pub cp: [Vec3A; 4],
    pub r0: f32,
    pub r1: f32,
}

/// A geometry to attach to a scene. The variants mirror Embree's geometry
/// types (the subset crust needs): triangle meshes, analytic spheres,
/// round curves, and instances of another committed scene. Instances nest:
/// an instanced scene may itself contain instances, and transforms,
/// normals and ray masks compose correctly through every level.
pub enum Geometry {
    TriangleMesh {
        vertices: Vec<Vec3A>,
        indices: Vec<[u32; 3]>,
        /// Optional per-vertex shading normals; hits interpolate them by
        /// the barycentrics (`SmoothTriangle` semantics).
        normals: Option<Vec<Vec3A>>,
    },
    Sphere {
        center: Vec3A,
        radius: f32,
    },
    RoundCurves {
        segments: Vec<CurveSegment>,
    },
    /// Cubic curve spans, intersected as true curves instead of being
    /// flattened to `RoundCurves` polylines — see [`CubicCurveSegment`].
    CubicCurves {
        segments: Vec<CubicCurveSegment>,
    },
    /// Another committed scene placed by `transform` (local-to-world).
    /// With `transform_end`, the placement interpolates linearly (per
    /// matrix element) at the ray's shutter time — transform motion blur.
    /// `transform` must be invertible.
    Instance {
        scene: Arc<Scene>,
        transform: Affine3A,
        /// Boxed because it's `None` for the overwhelming majority of
        /// instances (only motion-blurred placements set it): inline it
        /// and every `Geometry` value — the enum is sized by its largest
        /// variant — pays an extra 64 bytes it never uses. A scene with
        /// millions of `PointInstancer` placements makes that the
        /// dominant cost of the whole geometry table.
        transform_end: Option<Box<Affine3A>>,
    },
}

/// An intersection: distance, ray-facing normal (flipped to oppose the
/// ray, with `front_face` recording the original orientation), the hit
/// barycentrics where meaningful, and the IDs that let the application
/// map the hit back to its own data (materials, lights, …).
///
/// For hits inside an [`Geometry::Instance`], `geom_id` is the
/// *instance's* id in the queried scene and `prim_id` the primitive index
/// within the instanced scene — the application maps per top-level
/// geometry.
#[derive(Clone, Copy, Debug)]
pub struct RayHit {
    pub t: f32,
    pub normal: Vec3A,
    pub front_face: bool,
    pub u: f32,
    pub v: f32,
    pub geom_id: u32,
    pub prim_id: u32,
}

/// Accumulates geometries, then builds the acceleration structure once in
/// [`SceneBuilder::commit`] (Embree's `rtcCommitScene`).
#[derive(Default)]
pub struct SceneBuilder {
    geoms: Vec<(Geometry, u32)>,
}

impl SceneBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Attaches a geometry visible to every ray category; returns its
    /// `geom_id` (dense, starting at 0 — usable as a table index).
    pub fn attach(&mut self, geometry: Geometry) -> u32 {
        self.attach_masked(geometry, MASK_ALL)
    }

    /// Attaches a geometry visible only to ray categories in `mask`.
    pub fn attach_masked(&mut self, geometry: Geometry, mask: u32) -> u32 {
        self.geoms.push((geometry, mask));
        (self.geoms.len() - 1) as u32
    }

    /// Reserves capacity for `additional` more geometries. Purely a
    /// performance/memory hint — callers that know an upcoming batch size
    /// (a `PointInstancer` with N placements, say) should use it: without
    /// it, growing a multi-million-entry `Vec` by repeated doubling both
    /// re-copies everything so far at each doubling and can leave up to
    /// ~2x the final size over-allocated.
    pub fn reserve(&mut self, additional: usize) {
        self.geoms.reserve(additional);
    }

    /// Number of geometries attached so far.
    pub fn count(&self) -> usize {
        self.geoms.len()
    }

    /// Expands every geometry into primitives and builds the BVH.
    pub fn commit(self) -> Scene {
        let n_geoms = self.geoms.len() as u32;
        // Size the primitive table exactly before filling it. A dense
        // instancer attaches millions of geometries, and growing this
        // vector by doubling re-copies every `PrimNode` written so far at
        // each step — hundreds of megabytes of memcpy, and the page faults
        // that come with it, for a count that is free to compute.
        let n_prims: usize = self
            .geoms
            .iter()
            .map(|(geom, _)| match geom {
                Geometry::TriangleMesh { indices, .. } => indices.len(),
                Geometry::RoundCurves { segments } => segments.len(),
                Geometry::CubicCurves { segments } => segments.len(),
                Geometry::Sphere { .. } | Geometry::Instance { .. } => 1,
            })
            .sum();
        let mut prims: Vec<PrimNode> = Vec::with_capacity(n_prims);
        for (geom_id, (geom, mask)) in self.geoms.into_iter().enumerate() {
            let geom_id = geom_id as u32;
            match geom {
                Geometry::TriangleMesh {
                    vertices,
                    indices,
                    normals,
                } => {
                    for (prim_id, tri) in indices.into_iter().enumerate() {
                        let [i0, i1, i2] = tri;
                        let (i0, i1, i2) = (i0 as usize, i1 as usize, i2 as usize);
                        if i0 >= vertices.len() || i1 >= vertices.len() || i2 >= vertices.len() {
                            continue;
                        }
                        let tri_normals = normals.as_ref().and_then(|ns| {
                            (i0 < ns.len() && i1 < ns.len() && i2 < ns.len())
                                .then(|| [ns[i0], ns[i1], ns[i2]])
                        });
                        prims.push(PrimNode::Triangle(TrianglePrim {
                            v0: vertices[i0],
                            v1: vertices[i1],
                            v2: vertices[i2],
                            normals: tri_normals,
                            geom_id,
                            prim_id: prim_id as u32,
                            mask,
                        }));
                    }
                }
                Geometry::Sphere { center, radius } => {
                    prims.push(PrimNode::Sphere(SpherePrim {
                        center,
                        radius,
                        geom_id,
                        mask,
                    }));
                }
                Geometry::RoundCurves { segments } => {
                    prims.reserve(segments.len());
                    for (prim_id, s) in segments.into_iter().enumerate() {
                        prims.push(PrimNode::Curve(CurvePrim {
                            p0: s.p0,
                            p1: s.p1,
                            r0: s.r0,
                            r1: s.r1,
                            geom_id,
                            prim_id: prim_id as u32,
                            mask,
                        }));
                    }
                }
                Geometry::CubicCurves { segments } => {
                    prims.reserve(segments.len());
                    for (prim_id, s) in segments.into_iter().enumerate() {
                        prims.push(PrimNode::CubicCurve(Box::new(CubicCurvePrim {
                            cp: s.cp,
                            r0: s.r0,
                            r1: s.r1,
                            geom_id,
                            prim_id: prim_id as u32,
                            mask,
                        })));
                    }
                }
                Geometry::Instance {
                    scene,
                    transform,
                    transform_end,
                } => {
                    let Some(inner_bounds) = scene.bounds() else {
                        continue; // empty instanced scene
                    };
                    let w2l = transform.inverse();
                    let bounds = match &transform_end {
                        Some(end) => AABB::surrounding_box(
                            transformed_aabb(&inner_bounds, &transform),
                            transformed_aabb(&inner_bounds, end),
                        ),
                        None => transformed_aabb(&inner_bounds, &transform),
                    };
                    prims.push(PrimNode::Instance(Box::new(InstancePrim {
                        scene,
                        l2w: transform,
                        normal_mat: w2l.matrix3.transpose(),
                        w2l,
                        l2w_end: transform_end.map(|end| *end),
                        bounds,
                        geom_id,
                        mask,
                    })));
                }
            }
        }
        Scene {
            bvh: Bvh::new(prims),
            n_geoms,
        }
    }
}

/// A committed, immutable scene. Queries are `&self` and thread-safe.
pub struct Scene {
    bvh: Bvh,
    n_geoms: u32,
}

impl Scene {
    /// Closest hit in `(t_min, t_max)`, or `None` (Embree's
    /// `rtcIntersect1`).
    pub fn intersect(&self, ray: &Ray, t_min: f32, t_max: f32) -> Option<RayHit> {
        let hit = self.bvh.hit(ray, t_min, t_max)?;
        let front_face = ray.dir.dot(hit.outward) < 0.0;
        Some(RayHit {
            t: hit.t,
            normal: if front_face { hit.outward } else { -hit.outward },
            front_face,
            u: hit.u,
            v: hit.v,
            geom_id: hit.geom_id,
            prim_id: hit.prim_id,
        })
    }

    /// Does the ray hit *anything* in `(t_min, t_max)`? Early-exit
    /// traversal — the shadow-ray fast path (Embree's `rtcOccluded1`).
    pub fn occluded(&self, ray: &Ray, t_min: f32, t_max: f32) -> bool {
        self.bvh.hit_any(ray, t_min, t_max)
    }

    /// World bounds of everything in the scene; `None` when empty.
    pub fn bounds(&self) -> Option<AABB> {
        self.bvh.bounds()
    }

    /// Number of attached geometries (`geom_id`s are `0..count`).
    pub fn geometry_count(&self) -> u32 {
        self.n_geoms
    }

    /// Number of primitives the geometries expanded into.
    pub fn primitive_count(&self) -> usize {
        self.bvh.prim_count()
    }

    /// Top-level primitives split by kind, for reporting. Instances count
    /// as one primitive each and are *not* descended into — the instanced
    /// scene's own contents are its own `Scene`'s business, and a
    /// prototype shared by a thousand placements would otherwise be
    /// counted a thousand times.
    pub fn primitive_breakdown(&self) -> PrimitiveBreakdown {
        self.bvh.primitive_breakdown()
    }

    /// Primitives actually resident in memory: like
    /// [`Scene::primitive_breakdown`], but descending into instanced
    /// scenes, counting each distinct prototype **once** however many
    /// placements reference it.
    ///
    /// This is the count that tracks memory. `primitive_breakdown` says
    /// what the top-level BVH traverses; this says what is stored. For an
    /// instance-heavy scene the two differ enormously, and the gap is the
    /// whole benefit of instancing.
    pub fn unique_primitive_breakdown(&self) -> PrimitiveBreakdown {
        let mut visited = std::collections::HashSet::new();
        let mut acc = PrimitiveBreakdown::default();
        self.accumulate_unique_into(&mut visited, &mut acc);
        acc
    }

    pub(crate) fn accumulate_unique_into(
        &self,
        visited: &mut std::collections::HashSet<usize>,
        acc: &mut PrimitiveBreakdown,
    ) {
        self.bvh.accumulate_unique(visited, acc);
    }

    /// Internal closest-hit that keeps the *outward* (unoriented) normal,
    /// so instance transforms can map it without re-deriving orientation.
    pub(crate) fn intersect_outward(&self, ray: &Ray, t_min: f32, t_max: f32) -> Option<PrimHit> {
        self.bvh.hit(ray, t_min, t_max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ray::{MASK_CAMERA, MASK_SHADOW};
    use glam::Mat4;

    fn unit_sphere_scene() -> Arc<Scene> {
        let mut b = SceneBuilder::new();
        b.attach(Geometry::Sphere {
            center: Vec3A::ZERO,
            radius: 1.0,
        });
        Arc::new(b.commit())
    }

    #[test]
    fn ids_map_back_to_geometries() {
        let mut b = SceneBuilder::new();
        let ball = b.attach(Geometry::Sphere {
            center: Vec3A::new(-3.0, 0.0, 0.0),
            radius: 1.0,
        });
        let quad = b.attach(Geometry::TriangleMesh {
            vertices: vec![
                Vec3A::new(2.0, -1.0, -1.0),
                Vec3A::new(2.0, -1.0, 1.0),
                Vec3A::new(2.0, 1.0, 1.0),
                Vec3A::new(2.0, 1.0, -1.0),
            ],
            indices: vec![[0, 1, 2], [0, 2, 3]],
            normals: None,
        });
        let scene = b.commit();
        assert_eq!(scene.geometry_count(), 2);
        assert_eq!(scene.primitive_count(), 3);

        let hit_ball = scene
            .intersect(
                &Ray::new(Vec3A::new(-3.0, 0.0, -5.0), Vec3A::Z),
                0.001,
                f32::INFINITY,
            )
            .expect("ball hit");
        assert_eq!(hit_ball.geom_id, ball);

        // Aim at the second triangle of the quad (upper-left half).
        let hit_quad = scene
            .intersect(
                &Ray::new(Vec3A::new(0.0, 0.5, -0.5), Vec3A::X),
                0.001,
                f32::INFINITY,
            )
            .expect("quad hit");
        assert_eq!(hit_quad.geom_id, quad);
        assert_eq!(hit_quad.prim_id, 1);
    }

    #[test]
    fn front_face_semantics_match_ray_side() {
        let scene = unit_sphere_scene();
        let outside = scene
            .intersect(&Ray::new(Vec3A::new(0.0, 0.0, -5.0), Vec3A::Z), 0.001, 100.0)
            .expect("outside hit");
        assert!(outside.front_face);
        assert!(outside.normal.abs_diff_eq(-Vec3A::Z, 1e-4));

        // From inside the sphere the normal flips toward the origin.
        let inside = scene
            .intersect(&Ray::new(Vec3A::ZERO, Vec3A::Z), 0.001, 100.0)
            .expect("inside hit");
        assert!(!inside.front_face);
        assert!(inside.normal.abs_diff_eq(-Vec3A::Z, 1e-4));
    }

    /// Does the kernel already nest instances? An `Instance` holds an
    /// `Arc<Scene>`, and nothing stops that scene from containing
    /// instances of its own — this asks whether the recursion actually
    /// works end to end, or only looks like it should.
    #[test]
    fn instances_nest() {
        // Level 0: a unit sphere at the origin.
        let leaf = unit_sphere_scene();

        // Level 1: two spheres, at local x = ±2.
        let mut mid = SceneBuilder::new();
        for x in [-2.0f32, 2.0] {
            mid.attach(Geometry::Instance {
                scene: Arc::clone(&leaf),
                transform: Affine3A::from_translation(glam::Vec3::new(x, 0.0, 0.0)),
                transform_end: None,
            });
        }
        let mid = Arc::new(mid.commit());

        // Level 2: two copies of that pair, at world y = ±5. Four spheres
        // in total, from one copy of the sphere's geometry.
        let mut root = SceneBuilder::new();
        for y in [-5.0f32, 5.0] {
            root.attach(Geometry::Instance {
                scene: Arc::clone(&mid),
                transform: Affine3A::from_translation(glam::Vec3::new(0.0, y, 0.0)),
                transform_end: None,
            });
        }
        let scene = root.commit();

        // Two top-level primitives hold four spheres.
        assert_eq!(scene.primitive_count(), 2);

        for (x, y) in [(-2.0f32, -5.0f32), (2.0, -5.0), (-2.0, 5.0), (2.0, 5.0)] {
            let ray = Ray::new(Vec3A::new(x, y, -8.0), Vec3A::Z);
            let hit = scene
                .intersect(&ray, 0.001, 100.0)
                .unwrap_or_else(|| panic!("nested sphere at ({x}, {y}) was missed"));
            assert!(
                (hit.t - 7.0).abs() < 1e-3,
                "nested sphere at ({x}, {y}): t = {} (want 7)",
                hit.t
            );
            assert!(
                hit.normal.abs_diff_eq(-Vec3A::Z, 1e-4),
                "nested normal wrong at ({x}, {y}): {:?}",
                hit.normal
            );
            assert!(scene.occluded(&ray, 0.001, 100.0));
        }

        // And nothing where the spheres are not.
        assert!(
            scene
                .intersect(&Ray::new(Vec3A::new(0.0, 0.0, -8.0), Vec3A::Z), 0.001, 100.0)
                .is_none(),
            "hit between the nested spheres"
        );
    }

    /// The reporting counts: the top-level view sees instances, the unique
    /// view descends but counts a shared prototype only once — the whole
    /// point being that four placed spheres cost one sphere of memory.
    #[test]
    fn unique_breakdown_counts_shared_prototypes_once() {
        let leaf = unit_sphere_scene();
        let mut mid = SceneBuilder::new();
        for x in [-2.0f32, 2.0] {
            mid.attach(Geometry::Instance {
                scene: Arc::clone(&leaf),
                transform: Affine3A::from_translation(glam::Vec3::new(x, 0.0, 0.0)),
                transform_end: None,
            });
        }
        let mid = Arc::new(mid.commit());
        let mut root = SceneBuilder::new();
        for y in [-5.0f32, 5.0] {
            root.attach(Geometry::Instance {
                scene: Arc::clone(&mid),
                transform: Affine3A::from_translation(glam::Vec3::new(0.0, y, 0.0)),
                transform_end: None,
            });
        }
        let scene = root.commit();

        // Top level: the two outer placements, no spheres visible yet.
        let top = scene.primitive_breakdown();
        assert_eq!(top.instances, 2);
        assert_eq!(top.spheres, 0);

        // Unique: descends both levels, but `mid` is one Arc shared by two
        // placements and `leaf` one Arc shared by two more — so exactly one
        // sphere is resident, reached through 2 + 2 instance primitives.
        let unique = scene.unique_primitive_breakdown();
        assert_eq!(unique.spheres, 1, "shared prototype counted more than once");
        assert_eq!(unique.instances, 4);
    }

    /// Nested instances must compose transforms in the right order, and
    /// map normals back through both levels. A rotation at the outer level
    /// and a non-uniform scale at the inner level do not commute, so this
    /// fails loudly if the composition is inverted.
    #[test]
    fn nested_instances_compose_transforms_and_normals() {
        // Inner: a unit sphere squashed to an ellipsoid by the mid level.
        let leaf = unit_sphere_scene();
        let mut mid = SceneBuilder::new();
        mid.attach(Geometry::Instance {
            scene: leaf,
            // 2x along local X only.
            transform: Affine3A::from_scale(glam::Vec3::new(2.0, 1.0, 1.0)),
            transform_end: None,
        });
        let mid = Arc::new(mid.commit());

        // Outer: rotate that ellipsoid 90 degrees about Z, so its long
        // axis ends up along world Y.
        let mut root = SceneBuilder::new();
        root.attach(Geometry::Instance {
            scene: mid,
            transform: Affine3A::from_rotation_z(std::f32::consts::FRAC_PI_2),
            transform_end: None,
        });
        let scene = root.commit();

        // Long axis is now Y: a ray down the Y axis meets the surface at
        // |y| = 2, while one down X meets it at |x| = 1.
        let along_y = scene
            .intersect(&Ray::new(Vec3A::new(0.0, -8.0, 0.0), Vec3A::Y), 0.001, 100.0)
            .expect("ray along Y must hit the rotated ellipsoid");
        assert!(
            (along_y.t - 6.0).abs() < 1e-3,
            "long axis is not along Y: t = {} (want 6)",
            along_y.t
        );
        let along_x = scene
            .intersect(&Ray::new(Vec3A::new(-8.0, 0.0, 0.0), Vec3A::X), 0.001, 100.0)
            .expect("ray along X must hit the rotated ellipsoid");
        assert!(
            (along_x.t - 7.0).abs() < 1e-3,
            "short axis is not along X: t = {} (want 7)",
            along_x.t
        );

        // The normal at the Y pole points back down -Y; an inverse
        // transpose applied at only one level would tilt it.
        assert!(
            along_y.normal.abs_diff_eq(-Vec3A::Y, 1e-4),
            "nested normal not mapped through both levels: {:?}",
            along_y.normal
        );
    }

    /// A ray mask must gate at every level of nesting: hiding the outer
    /// instance hides everything beneath it.
    #[test]
    fn nested_instances_respect_masks_at_each_level() {
        let leaf = unit_sphere_scene();
        let mut mid = SceneBuilder::new();
        mid.attach_masked(
            Geometry::Instance {
                scene: leaf,
                transform: Affine3A::IDENTITY,
                transform_end: None,
            },
            MASK_SHADOW,
        );
        let mid = Arc::new(mid.commit());

        let mut root = SceneBuilder::new();
        root.attach_masked(
            Geometry::Instance {
                scene: mid,
                transform: Affine3A::IDENTITY,
                transform_end: None,
            },
            MASK_SHADOW | MASK_CAMERA,
        );
        let scene = root.commit();

        let ray = Ray::new(Vec3A::new(0.0, 0.0, -5.0), Vec3A::Z);
        // The inner level only admits shadow rays, so a camera ray is
        // rejected there even though the outer level would allow it.
        assert!(scene.intersect(&ray.with_mask(MASK_CAMERA), 0.001, 100.0).is_none());
        assert!(scene.intersect(&ray.with_mask(MASK_SHADOW), 0.001, 100.0).is_some());
    }

    #[test]
    fn masks_filter_by_ray_category() {
        let mut b = SceneBuilder::new();
        b.attach_masked(
            Geometry::Sphere {
                center: Vec3A::ZERO,
                radius: 1.0,
            },
            MASK_SHADOW,
        );
        let scene = b.commit();
        let ray = Ray::new(Vec3A::new(0.0, 0.0, -5.0), Vec3A::Z);
        assert!(scene.intersect(&ray.with_mask(MASK_CAMERA), 0.001, 100.0).is_none());
        assert!(scene.intersect(&ray.with_mask(MASK_SHADOW), 0.001, 100.0).is_some());
        assert!(!scene.occluded(&ray.with_mask(MASK_CAMERA), 0.001, 100.0));
        assert!(scene.occluded(&ray.with_mask(MASK_SHADOW), 0.001, 100.0));
    }

    #[test]
    fn smooth_normals_interpolate() {
        // One triangle with vertex normals fanned outward; the hit normal
        // at an interior point must be a blend, not the face normal.
        let mut b = SceneBuilder::new();
        b.attach(Geometry::TriangleMesh {
            vertices: vec![
                Vec3A::new(-1.0, -1.0, 2.0),
                Vec3A::new(1.0, -1.0, 2.0),
                Vec3A::new(0.0, 1.0, 2.0),
            ],
            indices: vec![[0, 1, 2]],
            normals: Some(vec![
                Vec3A::new(-0.5, 0.0, -1.0).normalize(),
                Vec3A::new(0.5, 0.0, -1.0).normalize(),
                Vec3A::new(0.0, 0.5, -1.0).normalize(),
            ]),
        });
        let scene = b.commit();
        // Straight at the v1 corner region: x > 0 → normal tilts +x.
        let hit = scene
            .intersect(&Ray::new(Vec3A::new(0.6, -0.7, 0.0), Vec3A::Z), 0.001, 100.0)
            .expect("hit");
        assert!(hit.normal.x > 0.1, "normal not interpolated: {:?}", hit.normal);
        assert!(hit.normal.z < 0.0);
    }

    #[test]
    fn translated_instance_matches_baked() {
        let mut b = SceneBuilder::new();
        b.attach(Geometry::Instance {
            scene: unit_sphere_scene(),
            transform: Affine3A::from_translation(glam::Vec3::new(3.0, 0.0, 0.0)),
            transform_end: None,
        });
        let scene = b.commit();
        let ray = Ray::new(Vec3A::new(3.0, 0.0, -5.0), Vec3A::Z);
        let hit = scene.intersect(&ray, 0.001, f32::INFINITY).expect("hit");
        assert!((hit.t - 4.0).abs() < 1e-4);
        assert!(hit.normal.abs_diff_eq(-Vec3A::Z, 1e-4));
        assert!(scene.occluded(&ray, 0.001, f32::INFINITY));
        assert!(!scene.occluded(&ray, 0.001, 3.9));
    }

    #[test]
    fn nonuniform_scale_transforms_normals_correctly() {
        // Sphere scaled 2x in X: probe an oblique point where the naive
        // (non inverse-transpose) normal mapping would be wrong.
        let mut b = SceneBuilder::new();
        b.attach(Geometry::Instance {
            scene: unit_sphere_scene(),
            transform: Affine3A::from_scale(glam::Vec3::new(2.0, 1.0, 1.0)),
            transform_end: None,
        });
        let scene = b.commit();
        // Hit the ellipsoid straight down above x=1 (local x=0.5).
        let hit = scene
            .intersect(&Ray::new(Vec3A::new(1.0, 5.0, 0.0), -Vec3A::Y), 0.001, f32::INFINITY)
            .expect("hit");
        // Implicit ellipsoid (x/2)^2 + y^2 + z^2 = 1: gradient at
        // (1, sqrt(3)/2, 0) is proportional to (0.5, sqrt(3), 0).
        let expected = Vec3A::new(0.5, 3.0f32.sqrt(), 0.0).normalize();
        assert!(
            hit.normal.abs_diff_eq(expected, 1e-3),
            "normal {:?} != expected {:?}",
            hit.normal,
            expected
        );
    }

    #[test]
    fn rotated_instance_hits_where_baked_triangle_would() {
        let mut inner = SceneBuilder::new();
        inner.attach(Geometry::TriangleMesh {
            vertices: vec![
                Vec3A::new(-1.0, -1.0, 0.0),
                Vec3A::new(1.0, -1.0, 0.0),
                Vec3A::new(0.0, 1.0, 0.0),
            ],
            indices: vec![[0, 1, 2]],
            normals: None,
        });
        let mut b = SceneBuilder::new();
        b.attach(Geometry::Instance {
            scene: Arc::new(inner.commit()),
            transform: Affine3A::from_mat4(
                Mat4::from_rotation_y(std::f32::consts::FRAC_PI_2)
                    * Mat4::from_translation(glam::Vec3::new(0.0, 0.0, 2.0)),
            ),
            transform_end: None,
        });
        let scene = b.commit();
        // Local (0,0,2) maps to world (2,0,0); triangle now faces +X.
        let hit = scene
            .intersect(&Ray::new(Vec3A::new(5.0, 0.0, 0.0), -Vec3A::X), 0.001, f32::INFINITY)
            .expect("hit");
        assert!((hit.t - 3.0).abs() < 1e-4);
        assert!(hit.normal.abs_diff_eq(Vec3A::X, 1e-4));
    }

    #[test]
    fn motion_blur_interpolates_position() {
        let mut b = SceneBuilder::new();
        b.attach(Geometry::Instance {
            scene: unit_sphere_scene(),
            transform: Affine3A::IDENTITY,
            transform_end: Some(Box::new(Affine3A::from_translation(glam::Vec3::new(4.0, 0.0, 0.0)))),
        });
        let scene = b.commit();
        // At time 0 the sphere is at the origin...
        let r0 = Ray::new(Vec3A::new(0.0, 0.0, -5.0), Vec3A::Z).with_time(0.0);
        assert!(scene.intersect(&r0, 0.001, f32::INFINITY).is_some());
        // ...at time 1 it has moved to x=4...
        let r1 = Ray::new(Vec3A::new(4.0, 0.0, -5.0), Vec3A::Z).with_time(1.0);
        assert!(scene.intersect(&r1, 0.001, f32::INFINITY).is_some());
        let r1_origin = Ray::new(Vec3A::new(0.0, 0.0, -5.0), Vec3A::Z).with_time(1.0);
        assert!(scene.intersect(&r1_origin, 0.001, f32::INFINITY).is_none());
        // ...and at time 0.5 it is halfway.
        let rh = Ray::new(Vec3A::new(2.0, 0.0, -5.0), Vec3A::Z).with_time(0.5);
        let hit = scene.intersect(&rh, 0.001, f32::INFINITY).expect("halfway hit");
        assert!((hit.t - 4.0).abs() < 1e-4);
        // The shutter-union bounding box covers both endpoints.
        let bb = scene.bounds().unwrap();
        assert!(bb.minimum.x <= -1.0 && bb.maximum.x >= 5.0);
    }

    #[test]
    fn instance_hits_report_instance_geom_id_and_inner_prim_id() {
        let mut inner = SceneBuilder::new();
        inner.attach(Geometry::TriangleMesh {
            vertices: vec![
                Vec3A::new(-1.0, -1.0, 0.0),
                Vec3A::new(1.0, -1.0, 0.0),
                Vec3A::new(1.0, 1.0, 0.0),
                Vec3A::new(-1.0, 1.0, 0.0),
            ],
            indices: vec![[0, 1, 2], [0, 2, 3]],
            normals: None,
        });
        let inner = Arc::new(inner.commit());

        let mut b = SceneBuilder::new();
        let _floor = b.attach(Geometry::Sphere {
            center: Vec3A::new(0.0, -100.0, 0.0),
            radius: 1.0,
        });
        let inst = b.attach(Geometry::Instance {
            scene: inner,
            transform: Affine3A::from_translation(glam::Vec3::new(0.0, 0.0, 5.0)),
            transform_end: None,
        });
        let scene = b.commit();
        // Upper-left region → second triangle of the instanced mesh.
        let hit = scene
            .intersect(&Ray::new(Vec3A::new(-0.5, 0.5, 0.0), Vec3A::Z), 0.001, f32::INFINITY)
            .expect("hit");
        assert_eq!(hit.geom_id, inst);
        assert_eq!(hit.prim_id, 1);
    }
}
