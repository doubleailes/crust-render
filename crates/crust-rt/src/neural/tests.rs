//! The proxy's contracts: the cut, the labels, eligibility, exactness off the
//! trained mask and for closest hit, determinism across thread counts, and
//! memory.

use std::sync::Arc;

use glam::{Affine3A, Vec3A};
use openqmc::pcg::Rng;

use super::cut::{CutSet, leaves_under};
use super::train::{Draw, Surfaces};
use super::*;
use crate::ray::{MASK_ALL, MASK_CAMERA, MASK_INDIRECT, RayMask};
use crate::{CommitOptions, CurveSegment, Geometry, SceneBuilder};

/// A UV sphere of `2 · segs · rings` triangles.
fn sphere(center: Vec3A, radius: f32, segs: usize, rings: usize) -> Geometry {
    let mut vertices = Vec::new();
    for r in 0..=rings {
        let phi = r as f32 / rings as f32 * std::f32::consts::PI;
        for s in 0..=segs {
            let th = s as f32 / segs as f32 * std::f32::consts::TAU;
            let p =
                center + radius * Vec3A::new(phi.sin() * th.cos(), phi.cos(), phi.sin() * th.sin());
            vertices.push(p.to_array());
        }
    }
    let row = (segs + 1) as u32;
    let mut indices = Vec::new();
    for r in 0..rings as u32 {
        for s in 0..segs as u32 {
            let (a, b) = (r * row + s, r * row + s + 1);
            let (c, d) = ((r + 1) * row + s + 1, (r + 1) * row + s);
            indices.push([a, b, c]);
            indices.push([a, c, d]);
        }
    }
    Geometry::TriangleMesh {
        vertices,
        indices,
        normals: None,
    }
}

/// Eight spheres, 8 · 2 · 48 · 24 = 18 432 triangles: enough for a cut of
/// several lanes at the default K.
fn spheres(b: &mut SceneBuilder) {
    for k in 0..8 {
        let c = Vec3A::new((k & 1) as f32, ((k >> 1) & 1) as f32, (k >> 2) as f32) * 2.5;
        b.attach(sphere(c, 1.0, 48, 24));
    }
}

/// A request small enough to train in a debug test.
fn quick(min_triangles: usize) -> ProxyRequest {
    ProxyRequest {
        min_triangles,
        mask: MASK_SHADOW,
        steps: 8,
        cut: ProxyCut::Size,
    }
}

fn with_proxy(req: ProxyRequest) -> CommitOptions {
    CommitOptions {
        occlusion_proxy: Some(req),
        ..CommitOptions::default()
    }
}

fn mesh_scene(req: Option<ProxyRequest>) -> Scene {
    let mut b = SceneBuilder::new();
    spheres(&mut b);
    b.commit_with(CommitOptions {
        occlusion_proxy: req,
        ..CommitOptions::default()
    })
}

fn rays(n: usize, seed: u32) -> Vec<Ray> {
    let mut rng = Rng::new(seed);
    (0..n)
        .map(|_| {
            let o = Vec3A::new(rng.next_f32(), rng.next_f32(), rng.next_f32()) * 8.0
                - Vec3A::splat(2.75);
            let d = Vec3A::new(rng.next_f32(), rng.next_f32(), rng.next_f32()) - Vec3A::splat(0.5);
            Ray::new(o, d)
        })
        .collect()
}

#[test]
fn the_cut_covers_every_leaf_once_and_is_reproducible() {
    let scene = mesh_scene(None);
    let bvh = scene.bvh();
    let refs = |s: &Scene| instanced_triangles(s).unwrap_or(0);
    for k in [64, 512, cut::SIZE_CUT_REFS, 1 << 30] {
        let a = CutSet::by_size(bvh, k, &refs);
        let b = CutSet::by_size(bvh, k, &refs);
        assert_eq!(a.lanes(), b.lanes(), "K = {k}");
        let mut seen = vec![0u32; bvh.leaf_count()];
        for &(n, l) in a.lanes() {
            let mut under = Vec::new();
            leaves_under(bvh, n, l as usize, &mut under);
            for leaf in under {
                seen[leaf as usize] += 1;
            }
        }
        assert!(seen.iter().all(|&c| c == 1), "K = {k}: {seen:?}");
        // And a split is a cut too.
        let split = a.split(bvh, &(0..a.len()).step_by(2).collect::<Vec<_>>());
        let mut seen = vec![0u32; bvh.leaf_count()];
        for &(n, l) in split.lanes() {
            let mut under = Vec::new();
            leaves_under(bvh, n, l as usize, &mut under);
            for leaf in under {
                seen[leaf as usize] += 1;
            }
        }
        assert!(seen.iter().all(|&c| c == 1), "split of K = {k}");
    }
    let size = CutSet::by_size(bvh, cut::SIZE_CUT_REFS, &refs);
    assert!(size.len() > 1, "the fixture is cut into several lanes");
}

#[test]
fn every_label_is_the_exact_occlusion() {
    let scene = mesh_scene(None);
    let refs = |s: &Scene| instanced_triangles(s).unwrap_or(0);
    let cut = CutSet::by_size(scene.bvh(), cut::SIZE_CUT_REFS, &refs);
    let surfaces = Surfaces::of(&scene, MASK_SHADOW, instanced_triangles(&scene).unwrap());
    let draw = Draw::new(&scene, &cut, &surfaces, MASK_SHADOW);
    let segs = draw.many(1, 2, 4096);
    let (mut pos, mut contact) = (0, 0);
    for s in &segs {
        let ray = Ray::new(s.o, s.d).with_mask(MASK_SHADOW);
        assert_eq!(s.label, scene.occluded(&ray, s.t0, s.t1));
        assert!(s.t0 <= s.t1 && s.diag > 0.0);
        pos += s.label as usize;
        contact += s.contact as usize;
    }
    // All three families and both classes are present.
    assert!(pos > 0 && pos < segs.len(), "{pos} of {}", segs.len());
    assert!(
        contact > segs.len() / 3 && contact < 2 * segs.len() / 3,
        "{contact}"
    );
}

#[test]
fn a_scene_with_curves_gets_no_proxy() {
    let mut b = SceneBuilder::new();
    spheres(&mut b);
    b.attach(Geometry::RoundCurves {
        segments: vec![CurveSegment {
            p0: Vec3A::ZERO,
            p1: Vec3A::ONE,
            r0: 0.1,
            r1: 0.1,
        }],
    });
    let scene = b.commit_with(with_proxy(quick(1)));
    assert!(!scene.has_occlusion_proxy());
    assert_eq!(scene.memory_footprint().occlusion_proxies, 0);
}

#[test]
fn a_small_scene_gets_no_proxy() {
    let scene = mesh_scene(Some(quick(1_000_000)));
    assert!(!scene.has_occlusion_proxy());
    assert!(scene.occlusion_proxy().is_none());
}

#[test]
fn a_moving_scene_gets_no_proxy() {
    let mut inner = SceneBuilder::new();
    spheres(&mut inner);
    let inner = Arc::new(inner.commit());
    let mut b = SceneBuilder::new();
    b.attach(Geometry::Instance {
        scene: inner,
        transform: Affine3A::IDENTITY,
        transform_end: Some(Box::new(Affine3A::from_translation(glam::Vec3::X))),
    });
    assert!(!b.commit_with(with_proxy(quick(1))).has_occlusion_proxy());
}

#[test]
fn a_prototype_of_instanced_parts_gets_a_proxy() {
    // Two parts, each under the threshold, placed four times each: only the
    // count through placements crosses it.
    let part = |c: Vec3A| {
        let mut p = SceneBuilder::new();
        p.attach(sphere(c, 1.0, 32, 16));
        Arc::new(p.commit())
    };
    let parts = [part(Vec3A::ZERO), part(Vec3A::X * 2.5)];
    let mut b = SceneBuilder::new();
    for k in 0..8 {
        b.attach(Geometry::Instance {
            scene: Arc::clone(&parts[k % 2]),
            transform: Affine3A::from_translation(glam::Vec3::new(0.0, 0.0, 2.5 * (k / 2) as f32)),
            transform_end: None,
        });
    }
    let per_part = 2 * 32 * 16;
    assert!(per_part * 2 < 8 * per_part);
    let scene = b.commit_with(with_proxy(quick(4 * per_part)));
    assert!(scene.has_occlusion_proxy());
    assert_eq!(scene.occlusion_proxy().unwrap().triangles, 8 * per_part);
}

#[test]
fn another_mask_is_exact_and_closest_hit_ignores_the_proxy() {
    let exact = mesh_scene(None);
    let proxied = mesh_scene(Some(quick(1)));
    assert!(proxied.has_occlusion_proxy());
    for (i, r) in rays(4000, 17).into_iter().enumerate() {
        // No shadow bit: exact, whatever geometry it would see.
        for mask in [MASK_CAMERA, MASK_INDIRECT] {
            let r = r.with_mask(mask);
            assert_eq!(
                proxied.occluded(&r, 1e-3, f32::INFINITY),
                exact.occluded(&r, 1e-3, f32::INFINITY),
                "ray {i}, mask {mask:?}"
            );
        }
        for mask in [MASK_SHADOW, MASK_CAMERA] {
            let r = r.with_mask(mask);
            let (a, b) = (
                proxied.intersect(&r, 1e-3, f32::INFINITY),
                exact.intersect(&r, 1e-3, f32::INFINITY),
            );
            match (a, b) {
                (None, None) => {}
                (Some(a), Some(b)) => {
                    assert_eq!(a.t.to_bits(), b.t.to_bits());
                    assert_eq!(a.u.to_bits(), b.u.to_bits());
                    assert_eq!(a.v.to_bits(), b.v.to_bits());
                    assert_eq!(
                        a.normal.to_array().map(f32::to_bits),
                        b.normal.to_array().map(f32::to_bits)
                    );
                    assert_eq!(
                        (a.geom_id, a.prim_id, a.placement),
                        (b.geom_id, b.prim_id, b.placement)
                    );
                }
                _ => panic!("ray {i}: one scene hit and the other did not"),
            }
        }
    }
}

#[test]
fn a_segment_that_misses_the_bounds_is_not_occluded() {
    let proxied = mesh_scene(Some(quick(1)));
    let b = proxied.bounds().unwrap();
    let mut rng = Rng::new(5);
    for _ in 0..2000 {
        // Rays above the box, parallel to its top, and rays pointing away.
        let o = Vec3A::new(
            rng.next_f32() * 10.0 - 5.0,
            b.maximum.y + 0.5,
            rng.next_f32() * 10.0 - 5.0,
        );
        let d = Vec3A::new(
            rng.next_f32() - 0.5,
            rng.next_f32() * 0.5,
            rng.next_f32() - 0.5,
        );
        let r = Ray::new(o, d).with_mask(MASK_SHADOW);
        assert!(!proxied.occluded(&r, 1e-3, f32::INFINITY));
    }
}

#[test]
fn training_at_one_and_eight_threads_gives_the_same_proxy() {
    let at = |threads: usize| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap()
            .install(|| mesh_scene(Some(quick(1))))
    };
    let (one, eight) = (at(1), at(8));
    let (p1, p8) = (one.proxy().unwrap(), eight.proxy().unwrap());
    let bits = |v: &[f32]| v.iter().map(|x| x.to_bits()).collect::<Vec<_>>();
    assert_eq!(bits(&p1.model.grid.params), bits(&p8.model.grid.params));
    assert_eq!(bits(&p1.model.mlp), bits(&p8.model.mlp));
    assert_eq!(p1.cut.lanes(), p8.cut.lanes());
    for r in rays(2000, 23) {
        let r = r.with_mask(MASK_SHADOW);
        assert_eq!(
            one.occluded(&r, 1e-3, f32::INFINITY),
            eight.occluded(&r, 1e-3, f32::INFINITY)
        );
    }
}

#[test]
fn the_refined_cut_is_reproducible() {
    let req = ProxyRequest {
        cut: ProxyCut::Refined,
        ..quick(1)
    };
    let (a, b) = (mesh_scene(Some(req)), mesh_scene(Some(req)));
    let (pa, pb) = (a.proxy().unwrap(), b.proxy().unwrap());
    assert_eq!(pa.cut.lanes(), pb.cut.lanes());
    assert_eq!(pa.report.steps, 3 * (req.steps / 4) + req.steps);
}

#[test]
fn a_proxied_prototype_placed_many_times_is_counted_once() {
    let proto = Arc::new(mesh_scene(Some(quick(1))));
    let bytes = proto.memory_footprint().occlusion_proxies;
    assert!(bytes >= grid::LEVELS * grid::TABLE * grid::FEATURES * 4);
    assert_eq!(bytes, proto.occlusion_proxy().unwrap().bytes);
    let mut b = SceneBuilder::new();
    for k in 0..1000 {
        b.attach(Geometry::Instance {
            scene: Arc::clone(&proto),
            transform: Affine3A::from_translation(glam::Vec3::new(10.0 * k as f32, 0.0, 0.0)),
            transform_end: None,
        });
    }
    let top = b.commit();
    let f = top.memory_footprint();
    assert_eq!(f.occlusion_proxies, bytes);
    assert!(f.total() >= bytes);
}

#[test]
fn labels_and_exact_queries_ignore_nested_proxies() {
    // Parts that hold proxies of their own, grouped into a proxied scene.
    let part = Arc::new(mesh_scene(Some(quick(1))));
    assert!(part.has_occlusion_proxy());
    let plain = Arc::new(mesh_scene(None));
    let group = |p: &Arc<Scene>, req| {
        let mut b = SceneBuilder::new();
        for k in 0..3 {
            b.attach(Geometry::Instance {
                scene: Arc::clone(p),
                transform: Affine3A::from_translation(glam::Vec3::new(9.0 * k as f32, 0.0, 0.0)),
                transform_end: None,
            });
        }
        b.commit_with(CommitOptions {
            occlusion_proxy: req,
            ..CommitOptions::default()
        })
    };
    let proxied = group(&part, Some(quick(1)));
    let exact = group(&plain, None);
    assert!(proxied.has_occlusion_proxy());
    // `occluded_exact` sees through every proxy, nested ones included.
    let mut rng = Rng::new(31);
    for _ in 0..3000 {
        let o = Vec3A::new(
            rng.next_f32() * 30.0 - 5.0,
            rng.next_f32() * 8.0 - 3.0,
            rng.next_f32() * 8.0 - 3.0,
        );
        let d = Vec3A::new(
            rng.next_f32() - 0.5,
            rng.next_f32() - 0.5,
            rng.next_f32() - 0.5,
        );
        let r = Ray::new(o, d).with_mask(MASK_SHADOW);
        assert_eq!(
            proxied.occluded_exact(&r, 1e-3, f32::INFINITY),
            exact.occluded(&r, 1e-3, f32::INFINITY)
        );
    }
    // And so do the training labels.
    let refs = |s: &Scene| instanced_triangles(s).unwrap_or(0);
    let cut = CutSet::by_size(proxied.bvh(), cut::SIZE_CUT_REFS, &refs);
    let surfaces = Surfaces::of(
        &proxied,
        MASK_SHADOW,
        instanced_triangles(&proxied).unwrap(),
    );
    let draw = Draw::new(&proxied, &cut, &surfaces, MASK_SHADOW);
    for s in draw.many(3, 4, 2048) {
        let ray = Ray::new(s.o, s.d).with_mask(MASK_SHADOW);
        assert_eq!(s.label, exact.occluded(&ray, s.t0, s.t1));
    }
}

#[test]
fn a_mask_that_sees_the_same_geometry_is_proxied_and_one_that_does_not_is_exact() {
    // Everything inside is visible to every ray: a shadow ray with class
    // bits on top (a shadow-linked light's) sees exactly what a plain one
    // sees, so the proxy answers it, bit for bit as it answers the plain one.
    let proxied = mesh_scene(Some(quick(1)));
    let p = proxied.proxy().unwrap();
    let linked = MASK_SHADOW | RayMask(1 << 5) | RayMask(1 << 31);
    assert!(p.answers(MASK_SHADOW) && p.answers(linked) && p.answers(MASK_ALL));
    assert!(!p.answers(MASK_CAMERA) && !p.answers(MASK_INDIRECT));
    for r in rays(3000, 41) {
        assert_eq!(
            proxied.occluded(&r.with_mask(linked), 1e-3, f32::INFINITY),
            proxied.occluded(&r.with_mask(MASK_SHADOW), 1e-3, f32::INFINITY)
        );
    }

    // A sphere inside that only camera rays see: a mask that sees it where
    // the shadow mask does not would see other geometry, so it is exact.
    let build = |req| {
        let mut b = SceneBuilder::new();
        spheres(&mut b);
        b.attach_masked(
            sphere(Vec3A::new(1.25, 1.25, 1.25), 0.8, 24, 12),
            MASK_CAMERA,
        );
        b.commit_with(CommitOptions {
            occlusion_proxy: req,
            ..CommitOptions::default()
        })
    };
    let (proxied, exact) = (build(Some(quick(1))), build(None));
    let p = proxied.proxy().unwrap();
    let wide = MASK_SHADOW | MASK_CAMERA;
    assert!(p.answers(MASK_SHADOW) && !p.answers(wide));
    for r in rays(3000, 43) {
        let r = r.with_mask(wide);
        assert_eq!(
            proxied.occluded(&r, 1e-3, f32::INFINITY),
            exact.occluded(&r, 1e-3, f32::INFINITY)
        );
    }
}
