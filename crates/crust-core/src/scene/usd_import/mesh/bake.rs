//! Placing meshes: a direct prim's placement recorded during the traversal,
//! then baked flat into world space or instanced once every placement count
//! is known.

use std::sync::Arc;

use crust_rt::Geometry;
use glam::{Affine3A, Mat4 as GMat4, Vec3, Vec3A};
use openusd::usd::Prim;
use openusd_schemas::geom::Mesh as UsdMesh;
use tracing::{debug, warn};

use crate::material::Material;
use crate::rt_world::WorldBuilder;
use crate::scene::subdiv;

use super::super::attrs::{prim_motion_translate, prim_ray_mask};
use super::arena::MeshArena;
use super::faces::{check_face_count, remap_refined_faces, triangulate};
use super::source::{MeshPlace, mesh_source};

/// A direct mesh prim whose geometry is recorded but not yet attached.
pub(in crate::scene::usd_import) struct MeshPlacement {
    /// Reserved during traversal, so ids keep their traversal order.
    pub(super) geom_id: u32,
    /// Index into [`MeshArena::slots`].
    pub(super) slot: u32,
    pub(super) l2w: Affine3A,
    pub(super) motion: Option<Vec3>,
}

pub(in crate::scene::usd_import) fn emit_mesh(
    world: &mut WorldBuilder,
    prim: &Prim,
    mesh: &UsdMesh,
    world_xf: GMat4,
    material: Arc<dyn Material>,
    meshes: &mut MeshArena,
    pending: &mut Vec<MeshPlacement>,
) {
    let want_faces = material.face_texture().is_some();
    let Some(src) = mesh_source(
        prim,
        mesh,
        material.as_ref(),
        &mut meshes.subdiv,
        MeshPlace::World(&world_xf),
    ) else {
        debug!(
            "Mesh at {} missing points / faceVertexCounts / faceVertexIndices — skipped",
            prim.path()
        );
        return;
    };

    let mask = prim_ray_mask(prim);
    let motion = prim_motion_translate(prim);

    // Non-invertible placements (a zero scale axis) cannot be instanced —
    // bake the degenerate transform into world-space triangles as before.
    if world_xf.determinant().abs() < 1e-12 {
        warn!(
            "Mesh at {} has a non-invertible transform — baking instead of instancing",
            prim.path()
        );
        if motion.is_some() {
            warn!(
                "Mesh at {}: crust:motion:translate is ignored on baked (non-invertible) geometry",
                prim.path()
            );
        }
        let verts: Vec<[f32; 3]> = src
            .points
            .iter()
            .map(|p| {
                world_xf
                    .transform_point3(Vec3::new(p.x, p.y, p.z))
                    .to_array()
            })
            .collect();
        check_face_count(prim, src.base_face_count, material.as_ref());
        match triangulate(
            &src.counts,
            &src.indices,
            verts.len(),
            want_faces,
            src.uvs.as_ref(),
        ) {
            Some((tris, faces, uvs)) => {
                // A singular transform has no inverse-transpose to push the
                // prototype's normals through, but the smooth normals of the
                // *transformed* mesh are still well-defined — recompute them.
                let normals = src
                    .normals
                    .is_some()
                    .then(|| subdiv::smooth_normals(&verts, &src.counts, &src.indices));
                let faces = match (&src.subdiv_faces, faces) {
                    (Some(sub), Some(map)) => Some(remap_refined_faces(map, sub)),
                    (_, faces) => faces,
                };
                // Already world-space here, so the density is too and the
                // placement scale stays at its default 1.0. This arm does not
                // go through `MeshArena::intern`, so it is the one other
                // place densities are built.
                let uvs = uvs.map(|mut m| {
                    m.build_density(&verts, &tris);
                    Arc::new(m)
                });
                let faces = faces.map(|mut m| {
                    m.build_density(&verts, &tris);
                    Arc::new(m)
                });
                let geom_id = world.attach_masked(
                    Geometry::TriangleMesh {
                        vertices: verts,
                        indices: tris,
                        normals,
                    },
                    material,
                    mask,
                );
                // This path bakes world-space vertices directly without going
                // through `bake_indices`, so the winding — and with it the
                // barycentric order — is whatever the transform produced.
                if let Some(map) = faces {
                    world.set_face_map(geom_id, map, false);
                }
                if let Some(map) = uvs {
                    world.set_uv_map(geom_id, map, false);
                }
            }
            None => debug!("Mesh at {} produced no triangles", prim.path()),
        }
        return;
    }

    // Record the placement rather than attaching it. Whether this mesh is
    // better placed by an instance or baked into world-space triangles
    // depends on how many times it is placed in total, which is not known
    // until the whole stage has been walked — so claim the `geom_id` now (it
    // must keep its traversal order) and decide in `flush_meshes`.
    let Some(slot) = meshes.intern(prim, src, &material) else {
        return;
    };
    meshes.slots[slot as usize].n_place += 1;

    let geom_id = world.reserve_slot(material, mask);
    pending.push(MeshPlacement {
        geom_id,
        slot,
        l2w: Affine3A::from_mat4(world_xf),
        motion,
    });
}

/// Turns the deferred [`MeshPlacement`]s into real geometry, now that every
/// mesh's placement count is final.
///
/// A mesh placed exactly once is baked into world-space triangles in the
/// parent BVH; anything placed more than once keeps one shared kernel scene
/// and an instance per placement.
///
/// Why baking a single placement is worth it: an instance costs every ray
/// that enters its box a transform into local space, a fresh ray/slab setup,
/// and a cold descent into a second tree — and the box the parent BVH sees is
/// the transformed AABB of the inner tree's root AABB, a box of a box, which
/// spatial splits cannot tighten. For geometry that exists in exactly one
/// place that buys nothing at all: there is no sharing to amortise it
/// against. It is also *less* memory, not more, since the inner tree's nodes,
/// leaf table and packets all go away and the triangles exist once either
/// way.
///
/// The decision has to be global — made after every streamed chunk, not
/// per chunk. Per-chunk would make it depend on stage layout (cornellbox
/// streams, so each of its meshes would look like a per-chunk singleton and
/// results would differ under `CRUST_STREAM_IMPORT=0`), and geometry
/// referenced from two elements of a production stage would get one resident
/// copy per element — a memory regression in exactly the case sharing exists
/// for.
///
/// `CRUST_MESH_BAKE=0` forces every placement to instance, i.e. the old
/// behaviour. That is the A/B switch: with it set the output must be
/// bit-identical, which is what separates "the deferral is wrong" from "the
/// baking changed something".
pub(in crate::scene::usd_import) fn flush_meshes(
    world: &mut WorldBuilder,
    meshes: &mut MeshArena,
    pending: Vec<MeshPlacement>,
) {
    let bake_enabled = crate::config().mesh_bake;
    let mut baked = 0usize;
    let mut instanced = 0usize;

    for p in pending {
        let slot = &meshes.slots[p.slot as usize];
        // Bake only when this is the mesh's sole placement, nothing has
        // already made it resident as a kernel scene, and it does not move —
        // a baked mesh has no transform left to interpolate over the shutter.
        let bake =
            bake_enabled && slot.n_place == 1 && slot.committed.is_none() && p.motion.is_none();

        let faces = slot.faces.clone();
        let uvs = slot.uvs.clone();
        let mirrored = p.l2w.matrix3.determinant() < 0.0;
        if bake {
            let geom = meshes.slots[p.slot as usize]
                .local
                .take()
                .expect("an unbaked, uncommitted slot still holds its triangles");
            let verts = bake_verts(&geom.verts, &p.l2w);
            let tris = bake_indices(geom.tris, &p.l2w);
            // The chart is shared as-is with every other placement: the
            // tangent frame, which does belong to this transform, is derived
            // at the hit from the baked vertices the kernel holds, and the
            // densities live in the *local* frame alongside the `FaceMap`,
            // with the placement's own scale recorded separately.
            world.set_geometry(
                p.geom_id,
                Geometry::TriangleMesh {
                    vertices: verts,
                    indices: tris,
                    normals: geom.normals.map(|ns| bake_normals(&ns, &p.l2w)),
                },
            );
            // `bake_indices` swaps a triangle's second and third vertices for a
            // mirroring placement, which exchanges the barycentrics the kernel
            // reports — so the face lookup has to exchange them back.
            if let Some(map) = faces {
                world.set_face_map(p.geom_id, map, mirrored);
            }
            if let Some(map) = uvs {
                world.set_uv_map(p.geom_id, map, mirrored);
            }
            // The vertices were baked into world space but the densities were
            // not: they are shared with (or cloned from) the slot's
            // local-frame tables, so the placement's scale still has to be
            // divided out at lookup.
            world.set_placement_scale(p.geom_id, placement_scale(&p.l2w));
            baked += 1;
        } else {
            let scene = meshes.committed_scene(p.slot);
            let l2w = p.l2w;
            world.set_geometry(
                p.geom_id,
                Geometry::Instance {
                    scene,
                    transform: l2w,
                    transform_end: p
                        .motion
                        .map(|v| Box::new(Affine3A::from_translation(v) * l2w)),
                },
            );
            // An instance keeps the prototype's own winding: the transform is
            // applied to the ray, not to the triangles, so no swap.
            if let Some(map) = faces {
                world.set_face_map(p.geom_id, map, false);
            }
            // Shared as-is: the tangent frame of this placement is derived
            // at the hit through the instance's transform (`World`'s
            // placement record), so there is nothing per placement to hold.
            if let Some(map) = uvs {
                world.set_uv_map(p.geom_id, map, false);
            }
            world.set_placement_scale(p.geom_id, placement_scale(&l2w));
            instanced += 1;
        }
    }

    if baked + instanced > 0 {
        debug!(
            "Direct meshes: {baked} baked flat, {instanced} instanced ({} distinct)",
            meshes.slots.len()
        );
    }
}

/// Local-space vertices into world space.
pub(super) fn bake_verts(verts: &[[f32; 3]], l2w: &Affine3A) -> Vec<[f32; 3]> {
    verts
        .iter()
        .map(|v| l2w.transform_point3a(Vec3A::from_array(*v)).to_array())
        .collect()
}

/// Local-space shading normals into world space: the inverse transpose —
/// exactly the matrix the kernel's instance path applies (`w2l`'s linear part
/// transposed, in `crust-rt`), so a baked placement shades identically to an
/// instanced one, mirrors included.
pub(super) fn bake_normals(normals: &[[f32; 3]], l2w: &Affine3A) -> Vec<[f32; 3]> {
    let m = l2w.matrix3.inverse().transpose();
    normals
        .iter()
        .map(|n| (m * Vec3A::from_array(*n)).normalize().to_array())
        .collect()
}

/// Triangle winding for baked geometry, flipped under a mirroring transform.
///
/// The instanced path derives its geometric normal inside the prototype and
/// maps it out through the inverse transpose. Baking derives it from the
/// world-space vertices instead, and for `det(M) < 0` those disagree in sign:
/// `(p1−p0)×(p2−p0) = det(M)·(M⁻¹)ᵀn`. Left unhandled, every mirrored prim
/// would render inside-out — `front_face` inverted, which flips which side of
/// a refractive interface the ray thinks it is on. Swapping two indices
/// restores the original orientation.
///
/// (This also swaps the roles of the barycentric `u`/`v` a hit reports.
/// Nothing reads them today — `HitRecord` carries no UVs — but whoever adds
/// texture coordinates needs to know.)
pub(super) fn bake_indices(mut tris: Vec<[u32; 3]>, l2w: &Affine3A) -> Vec<[u32; 3]> {
    if l2w.matrix3.determinant() < 0.0 {
        for t in &mut tris {
            t.swap(1, 2);
        }
    }
    tris
}

/// The uniform scale a placement applies, as the geometric mean of its three
/// axis scales.
///
/// Both side tables' densities are in the mesh's local frame, so this is what
/// converts a world-space texture footprint into that frame. `cbrt` of the
/// determinant is the mean because the determinant is the product of the
/// three scales; a non-uniform placement is therefore filtered by their mean,
/// which is all an isotropic cone could have used anyway.
pub(in crate::scene::usd_import) fn placement_scale(l2w: &Affine3A) -> f32 {
    let det = l2w.matrix3.determinant().abs();
    if det > 0.0 { det.cbrt() } else { 1.0 }
}
