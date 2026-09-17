//! `WorldBuilder` / `World` — the kernel-to-material bridge — plus the
//! per-triangle side tables (`FaceMap`, `UvMap`) and the material types
//! through the `Material` trait.

use crust_core::rt::Geometry;
use crust_core::{
    Emissive, FaceMap, FanSlice, HitRecord, MASK_CAMERA, MASK_INDIRECT, MASK_SHADOW, Material,
    OpenPBR, PathSampler, Ray, UvMap, Vec3A, WorldBuilder, materialx,
};
use std::sync::Arc;

fn sphere(center: Vec3A, radius: f32) -> Geometry {
    Geometry::Sphere { center, radius }
}

/// The unit quad `[0,1]²` at z = 0 as the importer would emit it.
fn quad_verts() -> Vec<Vec3A> {
    vec![
        Vec3A::new(0.0, 0.0, 0.0),
        Vec3A::new(1.0, 0.0, 0.0),
        Vec3A::new(1.0, 1.0, 0.0),
        Vec3A::new(0.0, 1.0, 0.0),
    ]
}

fn quad_tris() -> Vec<[u32; 3]> {
    vec![[0, 1, 2], [0, 2, 3]]
}

fn quad() -> Geometry {
    Geometry::TriangleMesh {
        vertices: quad_verts(),
        indices: quad_tris(),
        normals: None,
    }
}

fn emissive(r: f32) -> Arc<dyn Material> {
    Arc::new(Emissive::new(Vec3A::new(r, 0.0, 0.0)))
}

fn approx(a: f32, b: f32, tol: f32) -> bool {
    (a - b).abs() <= tol
}

// ---------------------------------------------------------------------------
// WorldBuilder / World
// ---------------------------------------------------------------------------

#[test]
fn ids_are_dense_and_index_the_material_table() {
    let mut b = WorldBuilder::new();
    assert_eq!(b.count(), 0);
    let a = b.attach(sphere(Vec3A::new(-3.0, 0.0, 0.0), 1.0), emissive(1.0));
    let c = b.attach(sphere(Vec3A::new(3.0, 0.0, 0.0), 1.0), emissive(2.0));
    assert_eq!((a, c), (0, 1));
    assert_eq!(b.count(), 2);
    let world = b.commit();
    assert_eq!(world.count(), 2);
    assert_eq!(world.material(0).emitted().x, 1.0);
    assert_eq!(world.material(1).emitted().x, 2.0);
}

#[test]
fn intersect_resolves_the_hit_material_and_geometry() {
    let mut b = WorldBuilder::new();
    b.attach(sphere(Vec3A::new(-3.0, 0.0, 0.0), 1.0), emissive(1.0));
    let right = b.attach(sphere(Vec3A::new(3.0, 0.0, 0.0), 1.0), emissive(2.0));
    let world = b.commit();
    let ray = Ray::new(Vec3A::new(3.0, 0.0, -5.0), Vec3A::Z);
    let hit = world
        .intersect(&ray, 1e-3, 100.0)
        .expect("hit the right sphere");
    assert_eq!(hit.geom_id, right);
    assert_eq!(hit.prim_id, 0);
    assert_eq!(hit.mat.emitted().x, 2.0);
    assert!(approx(hit.rec.t, 4.0, 1e-5));
    assert!(hit.rec.p.abs_diff_eq(ray.at(hit.rec.t), 1e-6));
    assert!(hit.rec.normal.abs_diff_eq(-Vec3A::Z, 1e-5));
    assert!(hit.rec.front_face);
    assert_eq!(
        hit.rec.face_id,
        HitRecord::NO_FACE,
        "no face table was installed"
    );
    assert!(!hit.rec.has_uv);
    assert_eq!(hit.rec.tangent, Vec3A::ZERO);
    assert!(
        world
            .intersect(&Ray::new(Vec3A::new(0.0, 0.0, -5.0), Vec3A::Z), 1e-3, 100.0)
            .is_none()
    );
}

#[test]
fn occluded_is_the_boolean_view_of_intersect() {
    let mut b = WorldBuilder::new();
    b.attach(sphere(Vec3A::ZERO, 1.0), emissive(1.0));
    let world = b.commit();
    let ray = Ray::new(Vec3A::new(0.0, 0.0, -5.0), Vec3A::Z);
    assert!(world.occluded(&ray, 1e-3, 100.0));
    assert!(!world.occluded(&ray, 1e-3, 3.0));
    assert!(!world.occluded(&Ray::new(Vec3A::new(0.0, 5.0, -5.0), Vec3A::Z), 1e-3, 100.0));
}

#[test]
fn masks_gate_world_queries() {
    let mut b = WorldBuilder::new();
    b.attach_masked(
        sphere(Vec3A::ZERO, 1.0),
        emissive(1.0),
        MASK_SHADOW | MASK_INDIRECT,
    );
    let world = b.commit();
    let ray = Ray::new(Vec3A::new(0.0, 0.0, -5.0), Vec3A::Z);
    assert!(
        world
            .intersect(&ray.clone().with_mask(MASK_CAMERA), 1e-3, 100.0)
            .is_none()
    );
    assert!(
        world
            .intersect(&ray.clone().with_mask(MASK_INDIRECT), 1e-3, 100.0)
            .is_some()
    );
    assert!(world.occluded(&ray.clone().with_mask(MASK_SHADOW), 1e-3, 100.0));
    assert!(!world.occluded(&ray.with_mask(MASK_CAMERA), 1e-3, 100.0));
}

#[test]
fn reserved_slots_bind_their_material_before_their_geometry() {
    let mut b = WorldBuilder::new();
    let first = b.attach(sphere(Vec3A::new(-3.0, 0.0, 0.0), 1.0), emissive(1.0));
    let slot = b.reserve_slot(emissive(5.0), MASK_SHADOW | MASK_INDIRECT | MASK_CAMERA);
    let last = b.attach(sphere(Vec3A::new(3.0, 0.0, 0.0), 1.0), emissive(2.0));
    assert_eq!((first, slot, last), (0, 1, 2));
    b.set_geometry(slot, sphere(Vec3A::ZERO, 1.0));
    let world = b.commit();
    assert_eq!(world.count(), 3);
    assert_eq!(world.primitive_count(), 3);
    let hit = world
        .intersect(&Ray::new(Vec3A::new(0.0, 0.0, -5.0), Vec3A::Z), 1e-3, 100.0)
        .unwrap();
    assert_eq!(hit.geom_id, slot);
    assert_eq!(hit.mat.emitted().x, 5.0);
}

#[test]
fn an_unfilled_slot_is_invisible_but_keeps_its_material() {
    let mut b = WorldBuilder::new();
    let slot = b.reserve_slot(emissive(9.0), MASK_CAMERA);
    b.attach(sphere(Vec3A::new(0.0, 0.0, 3.0), 1.0), emissive(1.0));
    let world = b.commit();
    assert_eq!(world.count(), 2);
    assert_eq!(world.primitive_count(), 1);
    assert_eq!(world.material(slot).emitted().x, 9.0);
    let hit = world
        .intersect(&Ray::new(Vec3A::new(0.0, 0.0, -5.0), Vec3A::Z), 1e-3, 100.0)
        .unwrap();
    assert_eq!(hit.geom_id, 1);
}

#[test]
fn reserve_is_only_a_capacity_hint() {
    let mut b = WorldBuilder::new();
    b.reserve(10_000);
    assert_eq!(b.count(), 0);
    b.attach(sphere(Vec3A::ZERO, 1.0), emissive(1.0));
    assert_eq!(b.count(), 1);
    assert_eq!(b.commit().count(), 1);
}

#[test]
fn reporting_helpers_delegate_to_the_kernel() {
    let mut b = WorldBuilder::new();
    b.attach(quad(), emissive(1.0));
    b.attach(sphere(Vec3A::new(5.0, 0.0, 0.0), 1.0), emissive(1.0));
    let world = b.commit();
    assert_eq!(world.primitive_count(), 3);
    let br = world.primitive_breakdown();
    assert_eq!(br.triangles, 2);
    assert_eq!(br.spheres, 1);
    assert_eq!(world.unique_primitive_breakdown(), br);
    assert!(!world.has_motion());
    assert!(world.memory_footprint().total() > 0);
    let bb = world.bounds().unwrap();
    assert!(bb.minimum.x <= 0.0 && bb.maximum.x >= 6.0);
    let (n, diag, mean, max) = world.primitive_extents();
    assert_eq!(n, 3);
    assert!(diag > 0.0 && mean > 0.0 && max >= mean);
}

#[test]
fn an_empty_world_has_no_bounds() {
    let world = WorldBuilder::new().commit();
    assert_eq!(world.count(), 0);
    assert!(world.bounds().is_none());
    assert!(
        world
            .intersect(&Ray::new(Vec3A::ZERO, Vec3A::Z), 1e-3, 100.0)
            .is_none()
    );
}

#[test]
fn default_builder_matches_new() {
    let a = WorldBuilder::default().commit();
    let b = WorldBuilder::new().commit();
    assert_eq!(a.count(), b.count());
}

// ---------------------------------------------------------------------------
// FaceMap
// ---------------------------------------------------------------------------

fn quad_face_map() -> FaceMap {
    FaceMap {
        faces: vec![0, 0],
        slices: vec![FanSlice::QuadLower, FanSlice::QuadUpper],
        uvs: None,
    }
}

#[test]
fn face_map_resolves_quad_slices_into_ptex_space() {
    let m = quad_face_map();
    // Lower fan half (v0, v1, v2): uv = (u + v, v).
    let (f, u, v) = m.resolve(0, 0.5, 0.25, false).unwrap();
    assert_eq!(f, 0);
    assert!(approx(u, 0.75, 1e-6) && approx(v, 0.25, 1e-6));
    // Upper fan half (v0, v2, v3): uv = (u, u + v).
    let (f, u, v) = m.resolve(1, 0.25, 0.5, false).unwrap();
    assert_eq!(f, 0);
    assert!(approx(u, 0.25, 1e-6) && approx(v, 0.75, 1e-6));
    // Out of range triangles have no face.
    assert!(m.resolve(2, 0.1, 0.1, false).is_none());
}

#[test]
fn face_map_triangle_slice_is_the_identity_and_ngons_are_unmappable() {
    let m = FaceMap {
        faces: vec![3, 4],
        slices: vec![FanSlice::Triangle, FanSlice::Unmappable],
        uvs: None,
    };
    let (f, u, v) = m.resolve(0, 0.2, 0.3, false).unwrap();
    assert_eq!(f, 3);
    assert!(approx(u, 0.2, 1e-6) && approx(v, 0.3, 1e-6));
    assert!(m.resolve(1, 0.2, 0.3, false).is_none());
}

#[test]
fn face_map_swap_exchanges_barycentrics() {
    let m = quad_face_map();
    let straight = m.resolve(0, 0.6, 0.1, false).unwrap();
    let swapped = m.resolve(0, 0.1, 0.6, true).unwrap();
    assert_eq!(straight, swapped);
}

#[test]
fn face_map_clamps_to_the_unit_square() {
    let m = quad_face_map();
    let (_, u, v) = m.resolve(0, 0.9, 0.9, false).unwrap();
    assert!(u <= 1.0 && v <= 1.0);
    let (_, u, v) = m.resolve(1, -0.1, -0.1, false).unwrap();
    assert!(u >= 0.0 && v >= 0.0);
}

#[test]
fn face_map_explicit_corner_uvs_interpolate() {
    // A refined triangle covering the sub-rectangle [0.5,1]×[0,0.5] of its
    // cage face, as a subdivided mesh would carry it.
    let m = FaceMap {
        faces: vec![7],
        slices: vec![FanSlice::Triangle],
        uvs: Some(vec![[[0.5, 0.0], [1.0, 0.0], [1.0, 0.5]]]),
    };
    let (f, u, v) = m.resolve(0, 0.0, 0.0, false).unwrap();
    assert_eq!(f, 7);
    assert!(approx(u, 0.5, 1e-6) && approx(v, 0.0, 1e-6));
    let (_, u, v) = m.resolve(0, 1.0, 0.0, false).unwrap();
    assert!(approx(u, 1.0, 1e-6) && approx(v, 0.0, 1e-6));
    let (_, u, v) = m.resolve(0, 0.0, 1.0, false).unwrap();
    assert!(approx(u, 1.0, 1e-6) && approx(v, 0.5, 1e-6));
    // Centroid.
    let (_, u, v) = m.resolve(0, 1.0 / 3.0, 1.0 / 3.0, false).unwrap();
    assert!(approx(u, 2.5 / 3.0, 1e-5) && approx(v, 0.5 / 3.0, 1e-5));
    // Even with explicit UVs an unmappable slice is declined.
    let n = FaceMap {
        faces: vec![7],
        slices: vec![FanSlice::Unmappable],
        uvs: Some(vec![[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]]),
    };
    assert!(n.resolve(0, 0.2, 0.2, false).is_none());
}

#[test]
fn world_hits_carry_ptex_face_coordinates() {
    let mut b = WorldBuilder::new();
    let id = b.attach(quad(), emissive(1.0));
    b.set_face_map(id, Arc::new(quad_face_map()), false);
    let world = b.commit();
    // For this quad the Ptex parameterisation equals the hit's (x, y).
    for (x, y) in [
        (0.75, 0.25),
        (0.25, 0.75),
        (0.5, 0.5),
        (0.1, 0.05),
        (0.9, 0.95),
    ] {
        let hit = world
            .intersect(&Ray::new(Vec3A::new(x, y, -1.0), Vec3A::Z), 1e-3, 10.0)
            .unwrap();
        assert_eq!(hit.rec.face_id, 0);
        assert!(
            approx(hit.rec.face_uv.0, x, 1e-4),
            "({x},{y}) -> {:?}",
            hit.rec.face_uv
        );
        assert!(
            approx(hit.rec.face_uv.1, y, 1e-4),
            "({x},{y}) -> {:?}",
            hit.rec.face_uv
        );
    }
}

#[test]
fn a_mirrored_placement_swaps_the_face_parameterisation() {
    // Bake the quad mirrored in X (x → 1 − x) with its winding fixed by an
    // index swap, exactly as the importer does, and mark the table swapped.
    let verts: Vec<Vec3A> = quad_verts()
        .into_iter()
        .map(|p| Vec3A::new(1.0 - p.x, p.y, p.z))
        .collect();
    let tris: Vec<[u32; 3]> = quad_tris().into_iter().map(|[a, b, c]| [a, c, b]).collect();
    let mut b = WorldBuilder::new();
    let id = b.attach(
        Geometry::TriangleMesh {
            vertices: verts,
            indices: tris,
            normals: None,
        },
        emissive(1.0),
    );
    b.set_face_map(id, Arc::new(quad_face_map()), true);
    let world = b.commit();
    // The point at world x = 0.25 is the original quad's x = 0.75.
    let hit = world
        .intersect(
            &Ray::new(Vec3A::new(0.25, 0.25, -1.0), Vec3A::Z),
            1e-3,
            10.0,
        )
        .unwrap();
    assert_eq!(hit.rec.face_id, 0);
    assert!(
        approx(hit.rec.face_uv.0, 0.75, 1e-4),
        "{:?}",
        hit.rec.face_uv
    );
    assert!(
        approx(hit.rec.face_uv.1, 0.25, 1e-4),
        "{:?}",
        hit.rec.face_uv
    );
}

// ---------------------------------------------------------------------------
// UvMap
// ---------------------------------------------------------------------------

fn quad_uv_map() -> UvMap {
    UvMap {
        uvs: vec![
            [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0]],
            [[0.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
        ],
        tangents: Vec::new(),
    }
}

#[test]
fn uv_map_interpolates_corners_and_defaults_tangent_to_zero() {
    let m = quad_uv_map();
    let ((u, v), t) = m.resolve(0, 0.5, 0.25, false).unwrap();
    assert!(approx(u, 0.75, 1e-6) && approx(v, 0.25, 1e-6));
    assert_eq!(t, Vec3A::ZERO);
    let ((u, v), _) = m.resolve(1, 0.25, 0.5, false).unwrap();
    assert!(approx(u, 0.25, 1e-6) && approx(v, 0.75, 1e-6));
    assert!(m.resolve(5, 0.1, 0.1, false).is_none());
    // Not clamped: a UDIM chart addresses tiles by the integer part.
    let m = UvMap {
        uvs: vec![[[2.0, 1.0], [3.0, 1.0], [3.0, 2.0]]],
        tangents: Vec::new(),
    };
    let ((u, v), _) = m.resolve(0, 0.5, 0.5, false).unwrap();
    assert!(approx(u, 3.0, 1e-6) && approx(v, 1.5, 1e-6));
}

#[test]
fn uv_map_swap_restores_original_vertex_order() {
    let m = quad_uv_map();
    let a = m.resolve(0, 0.6, 0.1, false).unwrap();
    let b = m.resolve(0, 0.1, 0.6, true).unwrap();
    assert_eq!(a.0, b.0);
}

#[test]
fn build_tangents_follows_increasing_u() {
    let mut m = quad_uv_map();
    m.build_tangents(&quad_verts(), &quad_tris());
    assert_eq!(m.tangents.len(), 2);
    for t in &m.tangents {
        assert!(t.abs_diff_eq(Vec3A::X, 1e-5), "{t}");
    }
    // A chart rotated 90°: u now grows along +Y.
    let mut r = UvMap {
        uvs: vec![
            [[0.0, 0.0], [0.0, 1.0], [1.0, 1.0]],
            [[0.0, 0.0], [1.0, 1.0], [1.0, 0.0]],
        ],
        tangents: Vec::new(),
    };
    r.build_tangents(&quad_verts(), &quad_tris());
    for t in &r.tangents {
        assert!(t.abs_diff_eq(Vec3A::Y, 1e-5), "{t}");
    }
}

#[test]
fn build_tangents_gives_zero_for_degenerate_charts() {
    let mut m = UvMap {
        uvs: vec![
            [[0.3, 0.3], [0.3, 0.3], [0.3, 0.3]],
            [[0.0, 0.0], [1.0, 0.0], [2.0, 0.0]],
        ],
        tangents: Vec::new(),
    };
    m.build_tangents(&quad_verts(), &quad_tris());
    assert_eq!(m.tangents, vec![Vec3A::ZERO, Vec3A::ZERO]);
    // A triangle with no UV entry at all also gets zero, never a panic.
    let mut short = UvMap {
        uvs: vec![[[0.0, 0.0], [1.0, 0.0], [1.0, 1.0]]],
        tangents: Vec::new(),
    };
    short.build_tangents(&quad_verts(), &quad_tris());
    assert_eq!(short.tangents.len(), 2);
    assert!(short.tangents[0].abs_diff_eq(Vec3A::X, 1e-5));
    assert_eq!(short.tangents[1], Vec3A::ZERO);
}

#[test]
fn build_tangents_is_unit_length_on_a_scaled_mesh() {
    let verts: Vec<Vec3A> = quad_verts().into_iter().map(|p| p * 7.0).collect();
    let mut m = quad_uv_map();
    m.build_tangents(&verts, &quad_tris());
    for t in &m.tangents {
        assert!(approx(t.length(), 1.0, 1e-5));
    }
    // Rebuilding replaces rather than appends.
    m.build_tangents(&verts, &quad_tris());
    assert_eq!(m.tangents.len(), 2);
}

#[test]
fn world_hits_carry_uv_and_tangent_from_the_uv_map() {
    let mut m = quad_uv_map();
    m.build_tangents(&quad_verts(), &quad_tris());
    let mut b = WorldBuilder::new();
    let id = b.attach(quad(), emissive(1.0));
    b.set_uv_map(id, Arc::new(m), false);
    let world = b.commit();
    for (x, y) in [(0.75, 0.25), (0.25, 0.75), (0.5, 0.5)] {
        let hit = world
            .intersect(&Ray::new(Vec3A::new(x, y, -1.0), Vec3A::Z), 1e-3, 10.0)
            .unwrap();
        assert!(hit.rec.has_uv);
        assert!(
            approx(hit.rec.uv.0, x, 1e-4) && approx(hit.rec.uv.1, y, 1e-4),
            "{:?}",
            hit.rec.uv
        );
        assert!(hit.rec.tangent.abs_diff_eq(Vec3A::X, 1e-5));
        // The face table was not installed, so Ptex fields stay unset.
        assert_eq!(hit.rec.face_id, HitRecord::NO_FACE);
    }
}

#[test]
fn a_geometry_may_carry_both_side_tables() {
    let mut b = WorldBuilder::new();
    let id = b.attach(quad(), emissive(1.0));
    b.set_face_map(id, Arc::new(quad_face_map()), false);
    b.set_uv_map(id, Arc::new(quad_uv_map()), false);
    let world = b.commit();
    let hit = world
        .intersect(&Ray::new(Vec3A::new(0.6, 0.2, -1.0), Vec3A::Z), 1e-3, 10.0)
        .unwrap();
    assert_eq!(hit.rec.face_id, 0);
    assert!(hit.rec.has_uv);
    assert!(approx(hit.rec.face_uv.0, hit.rec.uv.0, 1e-5));
    assert!(approx(hit.rec.face_uv.1, hit.rec.uv.1, 1e-5));
}

#[test]
fn side_tables_are_per_geometry() {
    let mut b = WorldBuilder::new();
    let textured = b.attach(quad(), emissive(1.0));
    let plain = b.attach(
        Geometry::TriangleMesh {
            vertices: quad_verts()
                .into_iter()
                .map(|p| p + Vec3A::new(2.0, 0.0, 0.0))
                .collect(),
            indices: quad_tris(),
            normals: None,
        },
        emissive(2.0),
    );
    b.set_face_map(textured, Arc::new(quad_face_map()), false);
    let world = b.commit();
    let a = world
        .intersect(&Ray::new(Vec3A::new(0.5, 0.5, -1.0), Vec3A::Z), 1e-3, 10.0)
        .unwrap();
    assert_eq!(a.rec.face_id, 0);
    let p = world
        .intersect(&Ray::new(Vec3A::new(2.5, 0.5, -1.0), Vec3A::Z), 1e-3, 10.0)
        .unwrap();
    assert_eq!(p.geom_id, plain);
    assert_eq!(p.rec.face_id, HitRecord::NO_FACE);
}

// ---------------------------------------------------------------------------
// Materials
// ---------------------------------------------------------------------------

#[test]
fn openpbr_presets_set_the_expected_lobes() {
    let d = OpenPBR::diffuse(Vec3A::new(0.2, 0.3, 0.4));
    assert_eq!(d.base_color, Vec3A::new(0.2, 0.3, 0.4));
    assert_eq!(d.specular_weight, 0.0);
    assert_eq!(d.base_metalness, 0.0);
    assert_eq!(d.transmission_weight, 0.0);

    let m = OpenPBR::metal(Vec3A::ONE, 0.3);
    assert_eq!(m.base_metalness, 1.0);
    assert_eq!(m.specular_roughness, 0.3);
    assert_eq!(
        OpenPBR::metal(Vec3A::ONE, 4.0).specular_roughness,
        1.0,
        "roughness clamps"
    );
    assert_eq!(OpenPBR::metal(Vec3A::ONE, -1.0).specular_roughness, 0.0);

    let g = OpenPBR::glass(1.7);
    assert_eq!(g.transmission_weight, 1.0);
    assert_eq!(g.specular_ior, 1.7);

    let gl = OpenPBR::glossy(Vec3A::ONE, 0.0, 2.0);
    assert_eq!(gl.specular_roughness, 0.05, "glossy floors the roughness");
    assert_eq!(gl.base_metalness, 1.0, "metalness clamps to one");
    assert_eq!(OpenPBR::glossy(Vec3A::ONE, 0.5, -1.0).base_metalness, 0.0);
}

#[test]
fn openpbr_defaults_follow_the_spec() {
    let o = OpenPBR::default();
    assert_eq!(o.base_weight, 1.0);
    assert_eq!(o.specular_ior, 1.5);
    assert_eq!(o.specular_roughness, 0.3);
    assert_eq!(o.coat_weight, 0.0);
    assert_eq!(o.coat_ior, 1.6);
    assert_eq!(o.fuzz_weight, 0.0);
    assert_eq!(o.emission_luminance, 0.0);
    assert_eq!(o.geometry_opacity, 1.0);
    assert!(!o.geometry_thin_walled);
    assert!(o.base_color_ptex.is_none());
    assert_eq!(o.emitted(), Vec3A::ZERO);
    assert!(o.face_texture().is_none());
    assert!(!o.uses_uv());
}

#[test]
fn openpbr_emission_is_colour_times_luminance() {
    let o = OpenPBR {
        emission_luminance: 2.0,
        emission_color: Vec3A::new(1.0, 0.5, 0.0),
        ..OpenPBR::default()
    };
    assert_eq!(o.emitted(), Vec3A::new(2.0, 1.0, 0.0));
    // Without a coat the directional emission is the isotropic one.
    assert_eq!(o.emitted_directional(1.0), o.emitted());
    assert_eq!(o.emitted_directional(0.2), o.emitted());
}

#[test]
fn a_coat_attenuates_emission_more_at_grazing_angles() {
    let o = OpenPBR {
        emission_luminance: 1.0,
        coat_weight: 1.0,
        ..OpenPBR::default()
    };
    let head_on = o.emitted_directional(1.0);
    let grazing = o.emitted_directional(0.1);
    assert!(head_on.x <= 1.0 + 1e-6, "a coat never amplifies: {head_on}");
    assert!(head_on.x > 0.0);
    assert!(
        grazing.x < head_on.x,
        "grazing {grazing} vs head-on {head_on}"
    );
}

#[test]
fn emissive_material_emits_and_never_scatters() {
    let e = Emissive::new(Vec3A::new(3.0, 2.0, 1.0));
    assert_eq!(e.color(), Vec3A::new(3.0, 2.0, 1.0));
    assert_eq!(e.emitted(), Vec3A::new(3.0, 2.0, 1.0));
    assert_eq!(e.emitted_directional(0.3), e.emitted());
    let rec = HitRecord {
        p: Vec3A::ZERO,
        normal: Vec3A::Z,
        t: 1.0,
        front_face: true,
        ..HitRecord::default()
    };
    let r_in = Ray::new(Vec3A::Z, -Vec3A::Z);
    assert!(
        e.scatter_importance(&r_in, &rec, PathSampler::new(0, 0, 0, 0))
            .is_none()
    );
    assert!(e.eval(&r_in, &rec, Vec3A::Z).is_none());
    assert!(e.face_texture().is_none());
    assert!(!e.uses_uv());
    // The default continuation ray starts at the hit point.
    let ray = e.make_ray(&rec, Vec3A::X);
    assert_eq!(ray.origin(), rec.p);
    assert_eq!(ray.direction(), Vec3A::X);
    let _ = format!("{e:?}");
}

fn upward_hit() -> (Ray, HitRecord) {
    let rec = HitRecord {
        p: Vec3A::ZERO,
        normal: Vec3A::Z,
        t: 1.0,
        front_face: true,
        ..HitRecord::default()
    };
    (
        Ray::new(
            Vec3A::new(0.3, 0.2, 1.0),
            Vec3A::new(-0.3, -0.2, -1.0).normalize(),
        ),
        rec,
    )
}

#[test]
fn diffuse_scatter_samples_stay_in_the_upper_hemisphere() {
    let m = OpenPBR::diffuse(Vec3A::splat(0.7));
    let (r_in, rec) = upward_hit();
    for i in 0..256 {
        let s = m
            .scatter_importance(&r_in, &rec, PathSampler::new(0, 0, 0, i))
            .expect("a diffuse surface always scatters");
        assert!(
            s.ray.direction().dot(rec.normal) > 0.0,
            "sample {i} went below the surface"
        );
        assert!(s.ray.origin().abs_diff_eq(rec.p, 1e-3));
        assert!(s.pdf > 0.0 && s.pdf.is_finite());
        assert!(s.value.min_element() >= 0.0 && s.value.is_finite());
        assert!(!s.delta, "diffuse has no delta lobe");
        assert!(s.ray.medium().is_none());
    }
}

#[test]
fn diffuse_eval_is_consistent_with_its_samples() {
    let m = OpenPBR::diffuse(Vec3A::splat(0.7));
    let (r_in, rec) = upward_hit();
    for i in 0..64 {
        let s = m
            .scatter_importance(&r_in, &rec, PathSampler::new(0, 0, 0, i))
            .unwrap();
        let (value, pdf) = m
            .eval(&r_in, &rec, s.ray.direction().normalize())
            .expect("continuous");
        assert!(
            approx(pdf, s.pdf, 1e-3 * s.pdf.max(1.0)),
            "pdf {pdf} vs sampled {}",
            s.pdf
        );
        assert!(
            value.abs_diff_eq(s.value, 1e-3 * s.value.max_element().max(1.0)),
            "{value} vs {}",
            s.value
        );
    }
}

#[test]
fn eval_below_the_horizon_is_zero_but_still_some() {
    let m = OpenPBR::diffuse(Vec3A::splat(0.7));
    let (r_in, rec) = upward_hit();
    let (value, pdf) = m
        .eval(&r_in, &rec, -Vec3A::Z)
        .expect("eval availability never depends on wi");
    assert_eq!(value, Vec3A::ZERO);
    assert!(pdf >= 0.0 && pdf.is_finite());
}

/// `E[value / pdf]` over the material's own samples: the directional albedo.
fn directional_albedo(m: &OpenPBR) -> Vec3A {
    let (r_in, rec) = upward_hit();
    let n = 4096;
    let mut sum = Vec3A::ZERO;
    for i in 0..n {
        let s = m
            .scatter_importance(&r_in, &rec, PathSampler::new(0, 0, 0, i))
            .unwrap();
        sum += s.value / s.pdf;
    }
    sum / n as f32
}

#[test]
fn diffuse_furnace_returns_the_albedo() {
    // With the dielectric interface switched off (IOR 1 → F0 = 0) the
    // diffuse slab's directional albedo is exactly its base colour.
    let albedo = Vec3A::new(0.2, 0.5, 0.8);
    let m = OpenPBR {
        specular_ior: 1.0,
        ..OpenPBR::diffuse(albedo)
    };
    let mean = directional_albedo(&m);
    assert!(mean.abs_diff_eq(albedo, 0.01), "{mean} vs {albedo}");
}

#[test]
fn diffuse_under_a_dielectric_interface_loses_the_average_fresnel() {
    // crust couples the diffuse slab to the dielectric above it with a
    // flat `1 − F_avg` (documented in the alignment record), and applies
    // it from the IOR alone — `specular_weight = 0` does not switch it off.
    // At IOR 1.5, F0 = 0.04, so the albedo comes back at 96%.
    let albedo = Vec3A::new(0.2, 0.5, 0.8);
    let m = OpenPBR::diffuse(albedo);
    let mean = directional_albedo(&m);
    assert!(
        mean.abs_diff_eq(albedo * 0.96, 0.01),
        "{mean} vs {}",
        albedo * 0.96
    );
}

#[test]
fn metal_reflects_around_the_mirror_direction() {
    let m = OpenPBR::metal(Vec3A::ONE, 0.05);
    let (r_in, rec) = upward_hit();
    let d = r_in.direction();
    let mirror = (d - 2.0 * d.dot(rec.normal) * rec.normal).normalize();
    let mut close = 0;
    for i in 0..128 {
        let s = m
            .scatter_importance(&r_in, &rec, PathSampler::new(0, 0, 0, i))
            .unwrap();
        if s.ray.direction().normalize().dot(mirror) > 0.95 {
            close += 1;
        }
    }
    assert!(
        close > 100,
        "only {close} of 128 samples near the mirror direction"
    );
}

#[test]
fn glass_transmission_enters_the_interior() {
    let m = OpenPBR {
        transmission_depth: 1.0,
        transmission_color: Vec3A::splat(0.5),
        ..OpenPBR::glass(1.5)
    };
    let (r_in, rec) = upward_hit();
    let mut refracted = 0;
    for i in 0..128 {
        let s = m
            .scatter_importance(&r_in, &rec, PathSampler::new(0, 0, 0, i))
            .unwrap();
        if s.ray.direction().dot(rec.normal) < 0.0 {
            refracted += 1;
            assert!(
                s.ray.medium().is_some(),
                "a refracted ray carries the interior medium"
            );
        } else {
            assert!(s.ray.medium().is_none(), "a reflected ray stays in vacuum");
        }
    }
    assert!(
        refracted > 64,
        "most rays refract through clear glass: {refracted}/128"
    );
    // `make_ray` mirrors that decision for an external direction.
    let inward = m.make_ray(&rec, Vec3A::new(0.1, 0.0, -1.0).normalize());
    assert!(inward.medium().is_some());
    let outward = m.make_ray(&rec, Vec3A::new(0.1, 0.0, 1.0).normalize());
    assert!(outward.medium().is_none());
}

#[test]
fn zero_depth_glass_carries_no_medium() {
    let m = OpenPBR::glass(1.5);
    let (_, rec) = upward_hit();
    let inward = m.make_ray(&rec, Vec3A::new(0.1, 0.0, -1.0).normalize());
    assert!(
        inward.medium().is_none(),
        "an inert interior is not tracked"
    );
}

#[test]
fn materials_are_object_safe_and_shareable() {
    let mats: Vec<Arc<dyn Material>> = vec![
        Arc::new(OpenPBR::default()),
        Arc::new(Emissive::new(Vec3A::ONE)),
        Arc::new(OpenPBR::glass(1.5)),
    ];
    let mut b = WorldBuilder::new();
    for (i, m) in mats.iter().enumerate() {
        b.attach(
            sphere(Vec3A::new(i as f32 * 3.0, 0.0, 0.0), 1.0),
            Arc::clone(m),
        );
    }
    let world = b.commit();
    assert_eq!(world.material(1).emitted(), Vec3A::ONE);
    assert_eq!(world.material(0).emitted(), Vec3A::ZERO);
}

// ---------------------------------------------------------------------------
// MaterialX adapter
// ---------------------------------------------------------------------------

fn sample_mtlx() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("samples")
        .join("materialx_basic.mtlx")
}

#[test]
fn material_node_of_takes_the_path_leaf() {
    assert_eq!(
        materialx::material_node_of("/MaterialX/Materials/surfacematerial_x"),
        Some("surfacematerial_x")
    );
    assert_eq!(materialx::material_node_of("plain"), Some("plain"));
    assert_eq!(materialx::material_node_of("/a/b/"), None);
    assert_eq!(materialx::material_node_of(""), None);
}

#[test]
fn loading_the_sample_builds_a_material_that_shades() {
    let loaded =
        materialx::load(&sample_mtlx(), Some("mtlx_ceramic"), &|_, _| None).expect("loads");
    assert_eq!(loaded.material.name, "mtlx_ceramic");
    assert_eq!(loaded.textures, 0);
    assert!(loaded.unsupported.is_empty());
    assert!(loaded.summary.contains("mtlx_ceramic"));
    assert!(loaded.summary.contains("lobes"));
    assert!(
        loaded.material.uses_uv(),
        "the graph reads texture coordinates"
    );

    let (r_in, rec) = upward_hit();
    let params = loaded.material.probe(&r_in, &rec);
    assert!(params.base_color.is_finite());
    assert!(params.base_color.min_element() >= 0.0 && params.base_color.max_element() <= 1.0);
    assert!(
        (params.specular_ior - 1.48).abs() < 1e-3,
        "the glaze IOR reaches OpenPBR: {}",
        params.specular_ior
    );
    assert_eq!(
        params.base_metalness, 0.0,
        "no conductor lobe in the ceramic"
    );

    let s = loaded
        .material
        .scatter_importance(&r_in, &rec, PathSampler::new(0, 0, 0, 1))
        .expect("scatters");
    assert!(s.pdf > 0.0);
    assert!(loaded.material.eval(&r_in, &rec, Vec3A::Z).is_some());
    assert_eq!(loaded.material.emitted(), Vec3A::ZERO);
}

#[test]
fn the_sample_metal_reduces_to_a_metal_lobe() {
    let loaded = materialx::load(&sample_mtlx(), Some("mtlx_metal"), &|_, _| None).expect("loads");
    let (r_in, rec) = upward_hit();
    let params = loaded.material.probe(&r_in, &rec);
    assert!(
        params.base_metalness > 0.0 && params.base_metalness <= 1.0,
        "{}",
        params.base_metalness
    );
    assert!(params.base_color.max_element() > 0.0);
}

#[test]
fn a_missing_mtlx_material_is_an_error() {
    let err = materialx::load(&sample_mtlx(), Some("nothing_here"), &|_, _| None)
        .err()
        .expect("error");
    assert!(err.to_string().contains("nothing_here"));
    assert!(materialx::load(std::path::Path::new("/no/such.mtlx"), None, &|_, _| None).is_err());
}
