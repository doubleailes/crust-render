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

/// A disk is hit inside its radius and nowhere else, from either side,
/// and `front_face` names the side its `normal` points to.
#[test]
fn disk_is_flat_round_and_knows_its_front() {
    let mut b = SceneBuilder::new();
    let id = b.attach(Geometry::Disk {
        center: Vec3A::new(0.0, 0.0, 2.0),
        normal: Vec3A::new(0.0, 0.0, -3.0), // unnormalised on purpose
        radius: 1.0,
    });
    let s = b.commit();

    let from_front = s
        .intersect(
            &Ray::new(Vec3A::new(0.5, 0.5, 0.0), Vec3A::Z),
            0.0,
            f32::INFINITY,
        )
        .expect("inside the radius");
    assert_eq!(from_front.geom_id, id);
    assert!((from_front.t - 2.0).abs() < 1e-6);
    assert!(
        from_front.front_face,
        "the ray arrives on the -Z (front) side"
    );
    assert!(from_front.normal.abs_diff_eq(-Vec3A::Z, 1e-6));

    let from_back = s
        .intersect(
            &Ray::new(Vec3A::new(0.0, 0.0, 5.0), -Vec3A::Z),
            0.0,
            f32::INFINITY,
        )
        .expect("a disk is visible from behind too");
    assert!(!from_back.front_face);

    // Just outside the radius (0.72² + 0.72² > 1) misses; parallel misses.
    assert!(
        s.intersect(
            &Ray::new(Vec3A::new(0.72, 0.72, 0.0), Vec3A::Z),
            0.0,
            f32::INFINITY
        )
        .is_none()
    );
    assert!(
        s.intersect(
            &Ray::new(Vec3A::new(-3.0, 0.0, 2.0), Vec3A::X),
            0.0,
            f32::INFINITY
        )
        .is_none()
    );

    // Bounds are exact in the plane and padded across it.
    let bb = s.bounds().unwrap();
    assert!((bb.maximum.x - 1.0).abs() < 1e-6 && (bb.minimum.y + 1.0).abs() < 1e-6);
    assert!(bb.maximum.z > bb.minimum.z);
}

/// Degenerate or non-finite disks and cylinders are skipped at commit
/// rather than handed to the BVH, where one infinite bound would poison
/// every box above it.
#[test]
fn invalid_disks_and_cylinders_are_skipped() {
    let mut b = SceneBuilder::new();
    for g in [
        Geometry::Disk {
            center: Vec3A::ZERO,
            normal: Vec3A::ZERO,
            radius: 1.0,
        },
        Geometry::Disk {
            center: Vec3A::ZERO,
            normal: Vec3A::Z,
            radius: -1.0,
        },
        Geometry::Disk {
            center: Vec3A::ZERO,
            normal: Vec3A::Z,
            radius: f32::INFINITY,
        },
        Geometry::Disk {
            center: Vec3A::splat(f32::NAN),
            normal: Vec3A::Z,
            radius: 1.0,
        },
        Geometry::Cylinder {
            p0: Vec3A::ZERO,
            p1: Vec3A::ZERO,
            radius: 1.0,
        },
        Geometry::Cylinder {
            p0: Vec3A::ZERO,
            p1: Vec3A::X,
            radius: 0.0,
        },
        Geometry::Cylinder {
            p0: Vec3A::ZERO,
            p1: Vec3A::splat(f32::INFINITY),
            radius: 1.0,
        },
    ] {
        b.attach(g);
    }
    let s = b.commit();
    assert_eq!(
        s.primitive_breakdown().disks + s.primitive_breakdown().cylinders,
        0
    );
    assert!(s.bounds().is_none());
}

/// An open tube: the wall is hit from outside with an outward normal,
/// from inside as a back face, and the ends are open.
#[test]
fn cylinder_is_an_open_tube() {
    let mut b = SceneBuilder::new();
    b.attach(Geometry::Cylinder {
        p0: Vec3A::new(-1.0, 0.0, 0.0),
        p1: Vec3A::new(1.0, 0.0, 0.0),
        radius: 0.5,
    });
    let s = b.commit();

    let outside = s
        .intersect(
            &Ray::new(Vec3A::new(0.3, 0.0, -4.0), Vec3A::Z),
            0.0,
            f32::INFINITY,
        )
        .expect("the wall");
    assert!((outside.t - 3.5).abs() < 1e-5);
    assert!(outside.front_face);
    assert!(outside.normal.abs_diff_eq(-Vec3A::Z, 1e-5));

    let inside = s
        .intersect(
            &Ray::new(Vec3A::new(0.3, 0.0, 0.0), Vec3A::Y),
            0.0,
            f32::INFINITY,
        )
        .expect("the wall, from within");
    assert!((inside.t - 0.5).abs() < 1e-5);
    assert!(!inside.front_face, "the inside of a tube is its back face");

    // Down the axis there are no caps to hit; past the end, no wall.
    assert!(
        s.intersect(
            &Ray::new(Vec3A::new(-5.0, 0.1, 0.0), Vec3A::X),
            0.0,
            f32::INFINITY
        )
        .is_none()
    );
    assert!(
        s.intersect(
            &Ray::new(Vec3A::new(1.2, 0.0, -4.0), Vec3A::Z),
            0.0,
            f32::INFINITY
        )
        .is_none()
    );
    // An oblique ray entering past the end exits through the wall.
    let oblique = s
        .intersect(
            &Ray::new(Vec3A::new(1.5, 0.0, 0.0), Vec3A::new(-1.0, 0.0, 1.0)),
            0.0,
            f32::INFINITY,
        )
        .expect("exits through the wall");
    assert!(!oblique.front_face);

    let bb = s.bounds().unwrap();
    assert!(bb.minimum.abs_diff_eq(Vec3A::new(-1.0, -0.5, -0.5), 1e-6));
    assert!(bb.maximum.abs_diff_eq(Vec3A::new(1.0, 0.5, 0.5), 1e-6));
    assert_eq!(s.primitive_breakdown().cylinders, 1);
}

/// Placed through an instance, a disk under a non-uniform scale is an
/// ellipse — the path the importer takes for a squashed `DiskLight`.
#[test]
fn instanced_disk_becomes_an_ellipse() {
    let mut inner = SceneBuilder::new();
    inner.attach(Geometry::Disk {
        center: Vec3A::ZERO,
        normal: -Vec3A::Z,
        radius: 1.0,
    });
    let inner = Arc::new(inner.commit());
    let mut b = SceneBuilder::new();
    b.attach(Geometry::Instance {
        scene: inner,
        transform: Affine3A::from_scale(glam::Vec3::new(3.0, 1.0, 1.0)),
        transform_end: None,
    });
    let s = b.commit();
    let hit = |x: f32| s.intersect(&Ray::new(Vec3A::new(x, 0.0, -1.0), Vec3A::Z), 0.0, 10.0);
    assert!(hit(2.9).is_some_and(|h| h.front_face));
    assert!(hit(3.1).is_none());
}

/// A reserved slot must keep its `geom_id` (so ids stay dense and every
/// later attach is unperturbed) and contribute nothing until filled.
#[test]
fn reserved_slots_keep_ids_dense_and_stay_invisible() {
    let mut b = SceneBuilder::new();
    let a = b.attach(Geometry::Sphere {
        center: Vec3A::new(-5.0, 0.0, 0.0),
        radius: 1.0,
    });
    let placeholder = b.attach(SceneBuilder::empty_geometry());
    let c = b.attach(Geometry::Sphere {
        center: Vec3A::new(5.0, 0.0, 0.0),
        radius: 1.0,
    });
    assert_eq!((a, placeholder, c), (0, 1, 2), "ids stay dense");

    // Never filled in: the two spheres are all there is.
    let scene = b.commit();
    assert_eq!(scene.geometry_count(), 3);
    assert_eq!(scene.primitive_count(), 2);

    // Filling it in later puts real geometry under the id it claimed.
    let mut b = SceneBuilder::new();
    b.attach(Geometry::Sphere {
        center: Vec3A::new(-5.0, 0.0, 0.0),
        radius: 1.0,
    });
    let slot = b.attach(SceneBuilder::empty_geometry());
    b.set_geometry(
        slot,
        Geometry::Sphere {
            center: Vec3A::ZERO,
            radius: 1.0,
        },
    );
    let scene = b.commit();
    assert_eq!(scene.primitive_count(), 2);
    let hit = scene
        .intersect(
            &Ray::new(Vec3A::new(0.0, 0.0, -8.0), Vec3A::Z),
            1e-4,
            f32::MAX,
        )
        .expect("the filled-in sphere is hit");
    assert_eq!(hit.geom_id, slot, "and reports the id it reserved");
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
            [2.0, -1.0, -1.0],
            [2.0, -1.0, 1.0],
            [2.0, 1.0, 1.0],
            [2.0, 1.0, -1.0],
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
        .intersect(
            &Ray::new(Vec3A::new(0.0, 0.0, -5.0), Vec3A::Z),
            0.001,
            100.0,
        )
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
fn motion_flag_reports_a_static_scene_as_static() {
    let mut b = SceneBuilder::new();
    b.attach(Geometry::Sphere {
        center: Vec3A::ZERO,
        radius: 1.0,
    });
    assert!(!b.commit().has_motion());

    // A *static* placement of static geometry is still static.
    let mut b = SceneBuilder::new();
    b.attach(Geometry::Instance {
        scene: unit_sphere_scene(),
        transform: Affine3A::from_translation(glam::Vec3::new(3.0, 0.0, 0.0)),
        transform_end: None,
    });
    assert!(!b.commit().has_motion());
}

/// The way `has_motion` can be wrong that matters: motion authored on an
/// *inner* instance, placed by an outer one that does not itself move.
/// If the flag only looked at its own `transform_end`, the renderer would
/// stop sampling the shutter and silently drop the blur.
#[test]
fn motion_flag_propagates_through_nesting() {
    let leaf = unit_sphere_scene();

    // Level 1: the sphere streaks along X over the shutter.
    let mut mid = SceneBuilder::new();
    mid.attach(Geometry::Instance {
        scene: leaf,
        transform: Affine3A::IDENTITY,
        transform_end: Some(Box::new(Affine3A::from_translation(glam::Vec3::new(
            4.0, 0.0, 0.0,
        )))),
    });
    let mid = Arc::new(mid.commit());
    assert!(mid.has_motion(), "the level that authored the motion");

    // Level 2: a static placement of that moving scene. Still moving.
    let mut root = SceneBuilder::new();
    root.attach(Geometry::Instance {
        scene: Arc::clone(&mid),
        transform: Affine3A::from_translation(glam::Vec3::new(0.0, 7.0, 0.0)),
        transform_end: None,
    });
    assert!(
        root.commit().has_motion(),
        "a static placement of moving geometry still moves"
    );
}

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
            .intersect(
                &Ray::new(Vec3A::new(0.0, 0.0, -8.0), Vec3A::Z),
                0.001,
                100.0
            )
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
        .intersect(
            &Ray::new(Vec3A::new(0.0, -8.0, 0.0), Vec3A::Y),
            0.001,
            100.0,
        )
        .expect("ray along Y must hit the rotated ellipsoid");
    assert!(
        (along_y.t - 6.0).abs() < 1e-3,
        "long axis is not along Y: t = {} (want 6)",
        along_y.t
    );
    let along_x = scene
        .intersect(
            &Ray::new(Vec3A::new(-8.0, 0.0, 0.0), Vec3A::X),
            0.001,
            100.0,
        )
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
    assert!(
        scene
            .intersect(&ray.with_mask(MASK_CAMERA), 0.001, 100.0)
            .is_none()
    );
    assert!(
        scene
            .intersect(&ray.with_mask(MASK_SHADOW), 0.001, 100.0)
            .is_some()
    );
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
    assert!(
        scene
            .intersect(&ray.with_mask(MASK_CAMERA), 0.001, 100.0)
            .is_none()
    );
    assert!(
        scene
            .intersect(&ray.with_mask(MASK_SHADOW), 0.001, 100.0)
            .is_some()
    );
    assert!(!scene.occluded(&ray.with_mask(MASK_CAMERA), 0.001, 100.0));
    assert!(scene.occluded(&ray.with_mask(MASK_SHADOW), 0.001, 100.0));
}

/// A mask replaced after attaching is the one the committed scene
/// filters by, exactly as if it had been attached with it.
#[test]
fn set_mask_replaces_the_attached_mask() {
    let mut b = SceneBuilder::new();
    let id = b.attach(Geometry::Sphere {
        center: Vec3A::ZERO,
        radius: 1.0,
    });
    assert_eq!(b.mask(id), MASK_ALL);
    b.set_mask(id, MASK_CAMERA);
    assert_eq!(b.mask(id), MASK_CAMERA);
    let scene = b.commit();
    let ray = Ray::new(Vec3A::new(0.0, 0.0, -5.0), Vec3A::Z);
    assert!(
        scene
            .intersect(&ray.with_mask(MASK_CAMERA), 0.001, 100.0)
            .is_some()
    );
    assert!(!scene.occluded(&ray.with_mask(MASK_SHADOW), 0.001, 100.0));
}

/// A geometry the commit skips — an invalid disk or cylinder, an instance of
/// an empty scene — still holds its `geom_id`'s slot in the geometry tables,
/// which are indexed by id: a triangle mesh attached after one reads its own
/// vertices and normals, not its neighbour's (or past the end of the table).
#[test]
fn a_skipped_geometry_keeps_its_table_slot() {
    let empty = Arc::new(SceneBuilder::new().commit());
    let mut b = SceneBuilder::new();
    b.attach(Geometry::Disk {
        center: Vec3A::ZERO,
        normal: Vec3A::Z,
        radius: 0.0,
    });
    b.attach(Geometry::Instance {
        scene: empty,
        transform: glam::Affine3A::IDENTITY,
        transform_end: None,
    });
    let mesh = b.attach(Geometry::TriangleMesh {
        vertices: vec![[-1.0, -1.0, 2.0], [1.0, -1.0, 2.0], [0.0, 1.0, 2.0]],
        indices: vec![[0, 1, 2]],
        normals: Some(vec![Vec3A::new(0.5, 0.0, -1.0).normalize().to_array(); 3]),
    });
    let scene = b.commit();
    let hit = scene
        .intersect(
            &Ray::new(Vec3A::new(0.0, -0.5, 0.0), Vec3A::Z),
            0.001,
            100.0,
        )
        .expect("the mesh is hit");
    assert_eq!(hit.geom_id, mesh);
    assert!(
        hit.normal
            .abs_diff_eq(Vec3A::new(0.5, 0.0, -1.0).normalize(), 1e-5),
        "the mesh shaded with another geometry's normals: {:?}",
        hit.normal
    );
}

#[test]
fn smooth_normals_interpolate() {
    // One triangle with vertex normals fanned outward; the hit normal
    // at an interior point must be a blend, not the face normal.
    let mut b = SceneBuilder::new();
    b.attach(Geometry::TriangleMesh {
        vertices: vec![[-1.0, -1.0, 2.0], [1.0, -1.0, 2.0], [0.0, 1.0, 2.0]],
        indices: vec![[0, 1, 2]],
        normals: Some(vec![
            Vec3A::new(-0.5, 0.0, -1.0).normalize().to_array(),
            Vec3A::new(0.5, 0.0, -1.0).normalize().to_array(),
            Vec3A::new(0.0, 0.5, -1.0).normalize().to_array(),
        ]),
    });
    let scene = b.commit();
    // Straight at the v1 corner region: x > 0 → normal tilts +x.
    let hit = scene
        .intersect(
            &Ray::new(Vec3A::new(0.6, -0.7, 0.0), Vec3A::Z),
            0.001,
            100.0,
        )
        .expect("hit");
    assert!(
        hit.normal.x > 0.1,
        "normal not interpolated: {:?}",
        hit.normal
    );
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
        .intersect(
            &Ray::new(Vec3A::new(1.0, 5.0, 0.0), -Vec3A::Y),
            0.001,
            f32::INFINITY,
        )
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
        vertices: vec![[-1.0, -1.0, 0.0], [1.0, -1.0, 0.0], [0.0, 1.0, 0.0]],
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
        .intersect(
            &Ray::new(Vec3A::new(5.0, 0.0, 0.0), -Vec3A::X),
            0.001,
            f32::INFINITY,
        )
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
        transform_end: Some(Box::new(Affine3A::from_translation(glam::Vec3::new(
            4.0, 0.0, 0.0,
        )))),
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
    let hit = scene
        .intersect(&rh, 0.001, f32::INFINITY)
        .expect("halfway hit");
    assert!((hit.t - 4.0).abs() < 1e-4);
    // The shutter-union bounding box covers both endpoints.
    let bb = scene.bounds().unwrap();
    assert!(bb.minimum.x <= -1.0 && bb.maximum.x >= 5.0);
}

/// A unit sphere at `x` in a one-geometry scene: a stand-in "part".
fn part_at(x: f32) -> Arc<Scene> {
    let mut b = SceneBuilder::new();
    b.attach(Geometry::Sphere {
        center: Vec3A::new(x, 0.0, 0.0),
        radius: 0.5,
    });
    Arc::new(b.commit())
}

fn placed(scene: &Arc<Scene>, z: f32) -> Geometry {
    Geometry::Instance {
        scene: scene.clone(),
        transform: Affine3A::from_translation(glam::Vec3::new(0.0, 0.0, z)),
        transform_end: None,
    }
}

/// The id a ray down +Z at `x` reports, if it hits.
fn id_at(scene: &Scene, x: f32) -> Option<u32> {
    scene
        .intersect(
            &Ray::new(Vec3A::new(x, 0.0, -10.0), Vec3A::Z),
            0.001,
            f32::INFINITY,
        )
        .map(|h| h.geom_id)
}

/// The shape the importer builds for a many-part prototype: parts
/// labelled `As(k)` inside a group, the group placed several times under
/// `Offset(0)` inside a scatter, and the scatter attached at the top
/// level under `Offset(base)`. Every hit must come out as `base + k`,
/// whichever placement it landed in.
#[test]
fn labelled_instances_compose_offsets_through_nesting() {
    let mut group = SceneBuilder::new();
    for k in 0..3u32 {
        // Attached out of order so a part's own id is never its label.
        group.attach_labelled(
            placed(&part_at(k as f32 * 2.0), 0.0),
            MASK_ALL,
            InstanceHitId::As(2 - k),
        );
    }
    let group = Arc::new(group.commit());

    let mut scatter = SceneBuilder::new();
    for z in [0.0, 5.0] {
        scatter.attach_labelled(placed(&group, z), MASK_ALL, InstanceHitId::Offset(0));
    }
    let scatter = Arc::new(scatter.commit());

    let mut top = SceneBuilder::new();
    let other = top.attach(Geometry::Sphere {
        center: Vec3A::new(-5.0, 0.0, 0.0),
        radius: 0.5,
    });
    let base = 10;
    let own = top.attach_labelled(placed(&scatter, 0.0), MASK_ALL, InstanceHitId::Offset(base));
    let top = top.commit();

    assert_eq!(own, 1, "a labelled instance still takes its own slot");
    assert_eq!(id_at(&top, -5.0), Some(other));
    assert_eq!(id_at(&top, 0.0), Some(base + 2));
    assert_eq!(id_at(&top, 2.0), Some(base + 1));
    assert_eq!(id_at(&top, 4.0), Some(base));
    assert_eq!(id_at(&top, 1.0), None);
}

/// `As` overrides the inner id outright, and `Own` still reports the
/// instance's slot even when what it places forwards.
#[test]
fn as_and_own_labels_override_inner_ids() {
    let mut inner = SceneBuilder::new();
    inner.attach_labelled(
        placed(&part_at(0.0), 0.0),
        MASK_ALL,
        InstanceHitId::Offset(7),
    );
    let inner = Arc::new(inner.commit());

    let mut b = SceneBuilder::new();
    let own = b.attach(placed(&inner, 0.0));
    let scene = b.commit();
    assert_eq!(id_at(&scene, 0.0), Some(own));

    let mut b = SceneBuilder::new();
    b.attach_labelled(placed(&inner, 0.0), MASK_ALL, InstanceHitId::As(42));
    assert_eq!(id_at(&b.commit(), 0.0), Some(42));
}

/// An offset that could carry an inner id past the id space is refused
/// at commit: on the hit path it would wrap onto an unrelated id, and a
/// host would shade the hit with that geometry's material.
#[test]
#[should_panic(expected = "overflows the geom_id space")]
fn an_offset_that_could_overflow_is_refused_at_commit() {
    let mut inner = SceneBuilder::new();
    inner.attach_labelled(placed(&part_at(0.0), 0.0), MASK_ALL, InstanceHitId::As(10));
    let inner = Arc::new(inner.commit());
    let mut b = SceneBuilder::new();
    b.attach_labelled(
        placed(&inner, 0.0),
        MASK_ALL,
        InstanceHitId::Offset(u32::MAX - 5),
    );
    let _ = b.commit();
}

/// The bound is exact enough to accept the largest offset that fits.
#[test]
fn the_largest_offset_that_fits_is_accepted() {
    let mut inner = SceneBuilder::new();
    inner.attach_labelled(placed(&part_at(0.0), 0.0), MASK_ALL, InstanceHitId::As(10));
    let inner = Arc::new(inner.commit());
    let mut b = SceneBuilder::new();
    let base = u32::MAX - 11;
    b.attach_labelled(placed(&inner, 0.0), MASK_ALL, InstanceHitId::Offset(base));
    assert_eq!(id_at(&b.commit(), 0.0), Some(base + 10));
}

#[test]
#[should_panic(expected = "only an instance can relabel")]
fn only_instances_take_labels() {
    SceneBuilder::new().attach_labelled(
        Geometry::Sphere {
            center: Vec3A::ZERO,
            radius: 1.0,
        },
        MASK_ALL,
        InstanceHitId::As(3),
    );
}

/// The forwarding field sits in padding: an island-scale scene holds
/// tens of millions of `InstancePrim`s, so a byte here is gigabytes.
#[test]
fn instance_prim_is_not_grown_by_forwarding() {
    assert_eq!(std::mem::size_of::<InstancePrim>(), 96);
}

#[test]
fn instance_hits_report_instance_geom_id_and_inner_prim_id() {
    let mut inner = SceneBuilder::new();
    inner.attach(Geometry::TriangleMesh {
        vertices: vec![
            [-1.0, -1.0, 0.0],
            [1.0, -1.0, 0.0],
            [1.0, 1.0, 0.0],
            [-1.0, 1.0, 0.0],
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
        .intersect(
            &Ray::new(Vec3A::new(-0.5, 0.5, 0.0), Vec3A::Z),
            0.001,
            f32::INFINITY,
        )
        .expect("hit");
    assert_eq!(hit.geom_id, inst);
    assert_eq!(hit.prim_id, 1);
}
