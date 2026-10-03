//! Mesh import tests: baking against instancing, face tables, the
//! subdivision policy.

mod bake_tests {
    use std::sync::Arc;

    use crust_rt::{Geometry, SceneBuilder as RtSceneBuilder};
    use glam::{Affine3A, Vec3A};

    use crate::material::{Material, OpenPBR};
    use crate::rt_world::WorldBuilder;

    use super::super::arena::MeshGeom;
    use super::super::bake::{bake_indices, bake_normals, bake_verts};

    /// A unit quad in the z = 0 plane, wound counter-clockwise seen from +z.
    fn quad() -> MeshGeom {
        MeshGeom {
            verts: vec![
                [-1.0, -1.0, 0.0],
                [1.0, -1.0, 0.0],
                [1.0, 1.0, 0.0],
                [-1.0, 1.0, 0.0],
            ],
            tris: vec![[0, 1, 2], [0, 2, 3]],
            normals: None,
        }
    }

    /// Places `geom` by `l2w` two ways — baked into world-space triangles,
    /// and as an instance of the local-space mesh — and returns what a ray
    /// down -z sees of each: `(t, front_face, normal.z)`.
    fn baked_vs_instanced(l2w: Affine3A) -> ((f32, bool, f32), (f32, bool, f32)) {
        let mat = || -> Arc<dyn Material> { Arc::new(OpenPBR::diffuse(Vec3A::splat(0.5))) };
        let geom = quad();

        let mut baked = WorldBuilder::new();
        baked.attach(
            Geometry::TriangleMesh {
                vertices: bake_verts(&geom.verts, &l2w),
                indices: bake_indices(geom.tris.clone(), &l2w),
                normals: None,
            },
            mat(),
        );
        let baked = baked.commit();

        let mut inner = RtSceneBuilder::new();
        inner.attach(Geometry::TriangleMesh {
            vertices: geom.verts.clone(),
            indices: geom.tris.clone(),
            normals: None,
        });
        let mut inst = WorldBuilder::new();
        inst.attach(
            Geometry::Instance {
                scene: Arc::new(inner.commit()),
                transform: l2w,
                transform_end: None,
            },
            mat(),
        );
        let inst = inst.commit();

        let ray = crate::ray::Ray::new(Vec3A::new(0.0, 0.0, 5.0), Vec3A::new(0.0, 0.0, -1.0));
        let probe = |w: &crate::rt_world::World| {
            let h = w.intersect(&ray, 1e-4, f32::MAX).expect("the quad is hit");
            (h.rec.t, h.rec.front_face, h.rec.normal.z)
        };
        (probe(&baked), probe(&inst))
    }

    #[test]
    fn baking_matches_instancing_for_an_ordinary_transform() {
        let l2w = Affine3A::from_scale_rotation_translation(
            glam::Vec3::new(2.0, 1.5, 1.0),
            glam::Quat::from_rotation_z(0.7),
            glam::Vec3::new(0.3, -0.2, 0.0),
        );
        let (baked, inst) = baked_vs_instanced(l2w);
        assert_eq!(baked, inst, "baked {baked:?} vs instanced {inst:?}");
    }

    /// The regression this guards: for `det(M) < 0` the world-space vertices
    /// wind the opposite way round, so a geometric normal derived from them
    /// points *against* the one the instanced path maps out through the
    /// inverse transpose. Without the compensating index swap in
    /// [`bake_indices`], `front_face` inverts — which silently flips which
    /// side of a refractive interface a ray believes it is on.
    #[test]
    fn baking_a_mirrored_transform_keeps_the_original_orientation() {
        // Negative x scale: a mirror, det < 0.
        let l2w = Affine3A::from_scale(glam::Vec3::new(-1.0, 1.0, 1.0));
        assert!(l2w.matrix3.determinant() < 0.0, "this test needs a mirror");

        let (baked, inst) = baked_vs_instanced(l2w);
        assert_eq!(
            baked, inst,
            "mirrored: baked {baked:?} vs instanced {inst:?}"
        );
        // And state the expected value outright, so the test still means
        // something if both paths ever break together.
        assert!(baked.1, "a ray down -z hits the front of a +z-facing quad");
        assert!(baked.2 > 0.0, "the ray-facing normal points back up +z");
    }

    /// Shading normals through the same two placements: `bake_normals` must
    /// be the exact matrix the kernel's instance path applies (the inverse
    /// transpose), or a mesh shades differently depending on whether the
    /// importer happened to bake or instance it — including under a mirror,
    /// where a plain rotation of the normal would come out backwards.
    #[test]
    fn baked_shading_normals_match_the_instanced_path() {
        // Tilted shading normals, deliberately not the geometric one.
        let tilt = Vec3A::new(0.3, -0.2, 1.0).normalize().to_array();
        let normals = vec![tilt; 4];
        let geom = quad();
        let mat = || -> Arc<dyn Material> { Arc::new(OpenPBR::diffuse(Vec3A::splat(0.5))) };
        let ray = crate::ray::Ray::new(Vec3A::new(0.0, 0.0, 5.0), Vec3A::new(0.0, 0.0, -1.0));
        let probe = |w: &crate::rt_world::World| {
            let h = w.intersect(&ray, 1e-4, f32::MAX).expect("the quad is hit");
            (h.rec.front_face, h.rec.normal)
        };

        for l2w in [
            Affine3A::from_scale_rotation_translation(
                glam::Vec3::new(2.0, 1.5, 1.0),
                glam::Quat::from_rotation_z(0.7),
                glam::Vec3::new(0.3, -0.2, 0.0),
            ),
            // The mirror is the case that breaks naive normal transforms.
            Affine3A::from_scale(glam::Vec3::new(-1.0, 1.0, 1.0)),
        ] {
            let mut baked = WorldBuilder::new();
            baked.attach(
                Geometry::TriangleMesh {
                    vertices: bake_verts(&geom.verts, &l2w),
                    indices: bake_indices(geom.tris.clone(), &l2w),
                    normals: Some(bake_normals(&normals, &l2w)),
                },
                mat(),
            );
            let baked = baked.commit();

            let mut inner = RtSceneBuilder::new();
            inner.attach(Geometry::TriangleMesh {
                vertices: geom.verts.clone(),
                indices: geom.tris.clone(),
                normals: Some(normals.clone()),
            });
            let mut inst = WorldBuilder::new();
            inst.attach(
                Geometry::Instance {
                    scene: Arc::new(inner.commit()),
                    transform: l2w,
                    transform_end: None,
                },
                mat(),
            );
            let inst = inst.commit();

            let (b_front, b_n) = probe(&baked);
            let (i_front, i_n) = probe(&inst);
            assert_eq!(b_front, i_front, "front_face split under {l2w:?}");
            assert!(
                b_n.abs_diff_eq(i_n, 1e-6),
                "normals split under {l2w:?}: baked {b_n:?} vs instanced {i_n:?}"
            );
        }
    }

    /// Without the swap the test above would pass for the wrong reason if
    /// `bake_indices` were a no-op and the kernel happened to agree, so pin
    /// the swap itself.
    #[test]
    fn bake_indices_swaps_winding_only_when_mirrored() {
        let tris = vec![[0u32, 1, 2]];
        let plain = Affine3A::from_scale(glam::Vec3::new(2.0, 3.0, 4.0));
        assert_eq!(bake_indices(tris.clone(), &plain), vec![[0, 1, 2]]);

        let mirror = Affine3A::from_scale(glam::Vec3::new(-2.0, 3.0, 4.0));
        assert_eq!(bake_indices(tris, &mirror), vec![[0, 2, 1]]);
    }
}

mod face_table_tests {
    use openusd::gf::Vec3f;

    use crate::rt_world::{FaceMap, FanSlice};
    use crate::scene::subdiv;

    use super::super::faces::{remap_subdivided_faces, remap_tessellated_faces, triangulate};

    /// Per-face tessellation's face table end to end: a cube tessellated at
    /// mixed rates, triangulated and remapped. Every triangle resolves into
    /// the cage face it was cut from, its corners to their own Ptex
    /// coordinates, and two triangles sharing an edge inside a face resolve
    /// its midpoint to the same place — the texture is continuous across the
    /// tessellation.
    #[test]
    fn tessellated_face_table_resolves_to_patch_coordinates() {
        let points: Vec<Vec3f> = [
            [-1.0, -1.0, 1.0],
            [1.0, -1.0, 1.0],
            [1.0, 1.0, 1.0],
            [-1.0, 1.0, 1.0],
            [-1.0, -1.0, -1.0],
            [1.0, -1.0, -1.0],
            [1.0, 1.0, -1.0],
            [-1.0, 1.0, -1.0],
        ]
        .map(Vec3f::from)
        .to_vec();
        let counts = [4; 6];
        let indices = [
            0, 1, 2, 3, 5, 4, 7, 6, 4, 0, 3, 7, 1, 5, 6, 2, 3, 2, 6, 7, 4, 5, 1, 0,
        ];
        let req = subdiv::SubdivRequest {
            scheme: subdiv::SubdivScheme::CatmullClark,
            level: 0,
            boundary: opensubdiv_rs::sdc::VtxBoundaryInterpolation::EdgeAndCorner,
            crease_indices: &[],
            crease_lengths: &[],
            crease_sharpnesses: &[],
            corner_indices: &[],
            corner_sharpnesses: &[],
            want_face_uvs: true,
            uvs: None,
        };
        // Rates 1 to 6, varying by edge.
        let segment =
            |p: &[[f32; 3]]| ((p[0][0] + 2.0 * p[1][1] + 3.0 * p[0][2]).abs() * 2.1) % 6.0;
        let t = subdiv::tessellate_adaptive(&points, &counts, &indices, &req, 3, &segment).unwrap();
        let per_face = t.faces.as_ref().unwrap();
        let tri_counts = vec![3; t.indices.len() / 3];
        let (tris, map, _) =
            triangulate(&tri_counts, &t.indices, t.points.len(), true, None).unwrap();
        let map = remap_tessellated_faces(map.unwrap(), per_face);
        assert_eq!(map.faces.len(), tris.len());
        // Corners: barycentric (0,0), (1,0), (0,1) are the triangle's own
        // corners, in its original order.
        let mut by_edge: std::collections::HashMap<(u32, u32), (u32, [f32; 2])> =
            std::collections::HashMap::new();
        for (i, tri) in tris.iter().enumerate() {
            let want = per_face.corner_uvs[i];
            let face = per_face.base_face[i].expect("cube faces are quads");
            for (k, (bu, bv)) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0)].into_iter().enumerate() {
                let (f, u, v) = map.resolve(i as u32, bu, bv, false).unwrap();
                assert_eq!(f, face);
                assert!((u - want[k][0]).abs() < 1e-6 && (v - want[k][1]).abs() < 1e-6);
            }
            // Each edge's midpoint, as this triangle resolves it.
            for (a, b, bary) in [(0, 1, (0.5, 0.0)), (1, 2, (0.5, 0.5)), (2, 0, (0.0, 0.5))] {
                let (f, u, v) = map.resolve(i as u32, bary.0, bary.1, false).unwrap();
                let key = (tri[a].min(tri[b]), tri[a].max(tri[b]));
                if let Some(&(f2, uv2)) = by_edge.get(&key) {
                    if f2 == f {
                        assert!(
                            (uv2[0] - u).abs() < 1e-6 && (uv2[1] - v).abs() < 1e-6,
                            "an edge inside face {f} resolves to two places"
                        );
                    }
                } else {
                    by_edge.insert(key, (f, [u, v]));
                }
            }
        }
    }

    /// The remap end to end: subdivide one textured quad, triangulate the
    /// refinement, remap — every triangle must resolve into the *base* face,
    /// and the refined corners must land on their sub-rectangle of it.
    #[test]
    fn subdivided_face_table_resolves_into_the_base_face() {
        let points = vec![
            Vec3f::from([0.0, 0.0, 0.0]),
            Vec3f::from([1.0, 0.0, 0.0]),
            Vec3f::from([1.0, 1.0, 0.0]),
            Vec3f::from([0.0, 1.0, 0.0]),
        ];
        let counts = [4];
        let indices = [0, 1, 2, 3];
        let req = subdiv::SubdivRequest {
            scheme: subdiv::SubdivScheme::CatmullClark,
            level: 1,
            boundary: opensubdiv_rs::sdc::VtxBoundaryInterpolation::EdgeAndCorner,
            crease_indices: &[],
            crease_lengths: &[],
            crease_sharpnesses: &[],
            corner_indices: &[],
            corner_sharpnesses: &[],
            want_face_uvs: true,
            uvs: None,
        };
        let refined = subdiv::subdivide(&points, &counts, &indices, &req).unwrap();
        let sub = refined.faces.as_ref().unwrap();
        let (tris, map, _) = triangulate(
            &refined.counts,
            &refined.indices,
            refined.points.len(),
            true,
            None,
        )
        .unwrap();
        let map = remap_subdivided_faces(map.unwrap(), sub);

        assert_eq!(tris.len(), 8, "4 child quads, 2 triangles each");
        let subs = map.sub.as_ref().expect("subdivided tables carry sub-faces");
        assert_eq!(map.faces.len(), tris.len());
        assert_eq!(subs.len(), tris.len());
        assert!(map.faces.iter().all(|&f| f == 0), "one base face only");
        // The eight-byte cell reproduces the refined channel's corners
        // exactly: a cell's corners are dyadic and the channel halved.
        for (t, (&refined, cell)) in map.faces.iter().zip(subs).enumerate() {
            let _ = refined;
            let want = sub.corner_uvs[t / 2];
            assert_eq!(cell.corners(), want, "triangle {t}");
        }
        let uvs: Vec<[[f32; 2]; 3]> = (0..tris.len())
            .map(|t| {
                let [c0, c1, c2, c3] = subs[t].corners();
                if t % 2 == 0 {
                    [c0, c1, c2]
                } else {
                    [c0, c2, c3]
                }
            })
            .collect();

        // Each triangle's interior resolves inside its child's quadrant of
        // the base face — quadrants are half-open squares of side 0.5.
        for (i, tri_uvs) in uvs.iter().enumerate() {
            let (got_face, u, v) = map
                .resolve(i as u32, 1.0 / 3.0, 1.0 / 3.0, false)
                .expect("every child of a quad resolves");
            assert_eq!(got_face, 0);
            let centroid_u = tri_uvs.iter().map(|c| c[0]).sum::<f32>() / 3.0;
            let centroid_v = tri_uvs.iter().map(|c| c[1]).sum::<f32>() / 3.0;
            assert!((u - centroid_u).abs() < 1e-6);
            assert!((v - centroid_v).abs() < 1e-6);
            assert!((0.0..=1.0).contains(&u) && (0.0..=1.0).contains(&v));
        }

        // The whole refinement still covers the base face: some corner of
        // some triangle touches each of the four Ptex corners.
        for corner in [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]] {
            assert!(
                uvs.iter()
                    .flatten()
                    .any(|c| (c[0] - corner[0]).abs() < 1e-6 && (c[1] - corner[1]).abs() < 1e-6),
                "no triangle corner reaches base corner {corner:?}"
            );
        }
    }

    /// Children of a non-quad cage face must decline the lookup — same
    /// contract as an unsubdivided n-gon.
    #[test]
    fn subdivided_ngon_descendants_are_unmappable() {
        let map = FaceMap {
            faces: vec![0, 1],
            slices: vec![FanSlice::QuadLower, FanSlice::QuadUpper],
            sub: None,
            corners: None,
            density: Vec::new(),
        };
        let sub = subdiv::SubdivFaces {
            base_face: vec![None, None],
            corner_uvs: vec![[[0.0; 2]; 4]; 2],
        };
        let map = remap_subdivided_faces(map, &sub);
        assert!(map.slices.iter().all(|&s| s == FanSlice::Unmappable));
        assert_eq!(map.resolve(0, 0.2, 0.2, false), None);
        assert_eq!(map.resolve(1, 0.2, 0.2, false), None);
    }

    /// The triangle list and the face table must stay index-parallel, because
    /// the kernel's `prim_id` indexes one to look up the other.
    #[test]
    fn quad_mesh_face_table_is_parallel_to_triangles() {
        // Three quads, 4 verts each, sharing a vertex pool of 12.
        let counts = [4, 4, 4];
        let indices: Vec<i32> = (0..12).collect();
        let (tris, map, _) = triangulate(&counts, &indices, 12, true, None).unwrap();
        let map = map.unwrap();

        assert_eq!(tris.len(), 6, "a quad fans into two triangles");
        assert_eq!(map.faces.len(), tris.len());
        assert_eq!(map.slices.len(), tris.len());
        assert_eq!(map.faces, vec![0, 0, 1, 1, 2, 2]);
        assert_eq!(
            map.slices,
            vec![
                FanSlice::QuadLower,
                FanSlice::QuadUpper,
                FanSlice::QuadLower,
                FanSlice::QuadUpper,
                FanSlice::QuadLower,
                FanSlice::QuadUpper,
            ]
        );
        // The fan is anchored at each face's first vertex.
        assert_eq!(tris[2], [4, 5, 6]);
        assert_eq!(tris[3], [4, 6, 7]);
    }

    /// A face the importer drops must not consume a face id, or every triangle
    /// after it addresses the wrong texture face — the failure mode that looks
    /// like plausible-but-wrong shading rather than an obvious break.
    #[test]
    fn skipped_faces_do_not_shift_later_face_ids() {
        // A degenerate 2-gon between two quads: skipped, but still numbered.
        let counts = [4, 2, 4];
        let indices: Vec<i32> = (0..10).collect();
        let (tris, map, _) = triangulate(&counts, &indices, 10, true, None).unwrap();
        let map = map.unwrap();
        assert_eq!(tris.len(), 4);
        // Face 1 contributed nothing; face 2 keeps its own index.
        assert_eq!(map.faces, vec![0, 0, 2, 2]);
    }

    /// Ptex has no n-gon faces, so those triangles must be marked unmappable
    /// rather than given a made-up parameterisation.
    #[test]
    fn ngons_and_triangles_get_their_own_slices() {
        let counts = [3, 5];
        let indices: Vec<i32> = (0..8).collect();
        let (_, map, _) = triangulate(&counts, &indices, 8, true, None).unwrap();
        let map = map.unwrap();
        assert_eq!(map.slices[0], FanSlice::Triangle);
        // A pentagon fans into three triangles, none of them addressable.
        assert_eq!(
            &map.slices[1..],
            &[
                FanSlice::Unmappable,
                FanSlice::Unmappable,
                FanSlice::Unmappable
            ]
        );
    }

    /// No table unless a material asks for one: the common case is an
    /// untextured stage, which should allocate nothing.
    #[test]
    fn face_table_is_not_built_unless_requested() {
        let counts = [4];
        let indices = [0, 1, 2, 3];
        let (tris, map, _) = triangulate(&counts, &indices, 4, false, None).unwrap();
        assert_eq!(tris.len(), 2);
        assert!(map.is_none());
    }
}

mod subdiv_policy_tests {
    use glam::{Mat4 as GMat4, Vec3A};
    use openusd::sdf;
    use openusd::usd::Stage;
    use openusd_schemas::geom::Mesh as UsdMesh;

    use crate::material::OpenPBR;

    use super::super::source::{MeshPlace, SubdivPolicy, mesh_source};

    /// The retired per-prim `crust:subdivisionLevel` warns once per load,
    /// however many prims author it, and never picks the level.
    #[test]
    fn the_legacy_per_prim_level_warns_once_and_is_not_the_level() {
        let dir = std::env::temp_dir().join("crust_subdiv_policy_tests");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("legacy_level.usda");
        let quad = |name: &str| {
            format!(
                r#"    def Mesh "{name}"
    {{
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(0, 0, 0), (1, 0, 0), (1, 1, 0), (0, 1, 0)]
        int crust:subdivisionLevel = 2
    }}
"#
            )
        };
        std::fs::write(
            &path,
            format!(
                "#usda 1.0\ndef Xform \"W\"\n{{\n{}{}}}\n",
                quad("A"),
                quad("B")
            ),
        )
        .expect("write stage");
        let stage = Stage::builder()
            .open(path.to_str().unwrap())
            .expect("stage opens");
        let mut policy = SubdivPolicy::new(2);
        for (n, name) in ["/W/A", "/W/B"].into_iter().enumerate() {
            let p = sdf::path(name).unwrap();
            let prim = crate::scene::usd_import::prim_at(&stage, p.clone());
            let mesh = UsdMesh::get(&stage, p).unwrap().expect("a mesh");
            let src = mesh_source(
                &prim,
                &mesh,
                &OpenPBR::diffuse(Vec3A::splat(0.5)),
                &mut policy,
                MeshPlace::World(&GMat4::IDENTITY),
            )
            .unwrap();
            // The fallback scheme at the policy's level 2, not the prim's.
            assert_eq!(src.counts.len(), 16, "{name}: refined at the load's level");
            // Set by the first prim, so the second finds the warning spent.
            assert!(policy.legacy_warned, "after prim {n}");
        }
    }
}
