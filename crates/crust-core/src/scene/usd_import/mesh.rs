//! `UsdGeomMesh` → kernel triangles: interning distinct meshes, the deferred
//! instance-vs-bake decision, triangulation and the per-triangle side tables
//! (Ptex faces, UVs, texture density).

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use crust_rt::{Geometry, Scene as RtScene, SceneBuilder as RtSceneBuilder};
use glam::{Affine3A, Mat4 as GMat4, Vec3, Vec3A};
use openusd::gf::Vec3f;
use openusd::sdf;
use openusd::usd::Prim;
use openusd_schemas::geom::{
    FaceVaryingLinearInterpolation, InterpolateBoundary, Mesh as UsdMesh, PointBased,
    SubdivisionScheme,
};
use rayon::prelude::*;
use tracing::{debug, warn};

use crate::material::Material;
use crate::rt_world::{FaceMap, FanSlice, SubFace, UvMap, WorldBuilder};
use crate::scene::subdiv;

use super::adaptive::{self, Aabb, ScreenRate};
use super::attrs::{custom_i32, prim_motion_translate, prim_ray_mask};
use super::time::eval_time;

/// Identity of an imported mesh's shared geometry: a content hash of the
/// authored points/counts/indices **and texture chart**, plus the (memoized,
/// so pointer-comparable) material. Prims agreeing on all of it share one
/// local-space triangle BVH.
///
/// The chart belongs in the identity because a [`MeshSlot`] owns the `UvMap`
/// that *every* placement of it shades through: the table is built once, from
/// whichever prim interned the slot first, and shared by `Arc` thereafter. Two
/// prims with the same points, topology and material but different
/// `primvars:st` — the same panel charted into two UDIM tiles, the idiom
/// `samples/materialx_basic.usda` is built around — would otherwise collide,
/// and the second would silently shade through the first's chart. The failure
/// is not visible as an error: it is a texture that reads plausibly and is
/// simply the wrong tile.
///
/// The chart is folded in only when one was read, which is exactly when the
/// bound material reports `uses_uv()`. An untextured stage hashes nothing
/// extra and dedupes as before.
#[derive(PartialEq, Eq, Hash)]
pub(super) struct MeshKey {
    pub(super) geo_hash: u64,
    pub(super) n_points: usize,
    pub(super) n_indices: usize,
    /// Authored UV values, or `None` when the mesh carries no chart. A
    /// discriminator beside the hash, like the two counts above.
    pub(super) n_uvs: Option<usize>,
    /// Whether the mesh shades with smooth normals. The normals themselves
    /// follow from the points and topology already hashed, but whether there
    /// are any does not: at level 0 a subdivision cage and a `none` cage share
    /// every array yet shade smooth and faceted — without this, whichever
    /// interned first would shade both.
    pub(super) smooth: bool,
    pub(super) material: usize,
}

impl MeshKey {
    pub(super) fn new(src: &MeshSource, material: &Arc<dyn Material>) -> Self {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for p in &src.points {
            p.x.to_bits().hash(&mut h);
            p.y.to_bits().hash(&mut h);
            p.z.to_bits().hash(&mut h);
        }
        src.counts.hash(&mut h);
        src.indices.hash(&mut h);
        // All three parts of the chart: the values, the indirection, and the
        // interpolation that decides which index addresses them. Two meshes
        // agreeing on the values but not on `face_varying` produce different
        // tables from the same array.
        match &src.uvs {
            Some(uv) => {
                1u8.hash(&mut h);
                for c in &uv.values {
                    c[0].to_bits().hash(&mut h);
                    c[1].to_bits().hash(&mut h);
                }
                uv.indices.hash(&mut h);
                uv.face_varying.hash(&mut h);
            }
            None => 0u8.hash(&mut h),
        }
        MeshKey {
            geo_hash: h.finish(),
            n_points: src.points.len(),
            n_indices: src.indices.len(),
            n_uvs: src.uvs.as_ref().map(|uv| uv.values.len()),
            smooth: src.normals.is_some(),
            material: Arc::as_ptr(material) as *const u8 as usize,
        }
    }
}

/// Local-space triangles of one distinct mesh, held only while that mesh
/// might still be baked flat into the parent BVH rather than instanced.
pub(super) struct MeshGeom {
    pub(super) verts: Vec<[f32; 3]>,
    pub(super) tris: Vec<[u32; 3]>,
    /// Smooth shading normals, parallel to `verts` — only subdivided meshes
    /// carry them.
    pub(super) normals: Option<Vec<[f32; 3]>>,
}

/// What the importer knows about one distinct mesh (one [`MeshKey`]).
pub(super) struct MeshSlot {
    /// The triangles, kept until the instance-vs-bake decision is made.
    /// Dropped as soon as `committed` is set: a committed slot is already
    /// resident as a kernel scene, so every placement of it instances and
    /// baking would only add a second copy.
    pub(super) local: Option<MeshGeom>,
    /// Triangle-to-source-face table, for a mesh whose material samples a
    /// per-face texture. Lives here rather than on [`MeshGeom`] because it has
    /// to outlive both endings — baking *and* committing drop `local`, but
    /// every placement still needs to resolve face ids at render time. Shared
    /// by `Arc` across the placements of one distinct mesh.
    pub(super) faces: Option<Arc<FaceMap>>,
    /// The mesh's texture chart, for a mesh whose material reads it. Lives
    /// beside `faces` and for the same reason: it has to outlive `local`,
    /// which both baking and committing drop. Shared by every placement —
    /// the tangent frame, which is per placement, is derived at the hit
    /// from the kernel's vertices rather than held here.
    pub(super) uvs: Option<Arc<UvMap>>,
    /// Set once some path needed this mesh as a real kernel scene — which
    /// prototypes always do, since an instance is the only way to place one.
    pub(super) committed: Option<Arc<RtScene>>,
    /// Instanceable direct placements recorded so far. World-space-baked
    /// placements (a non-invertible transform) are not counted: they never
    /// reference the slot again.
    pub(super) n_place: u32,
}

/// Distinct meshes seen so far, and the index of each by content.
///
/// Replaces a bare `HashMap<MeshKey, Arc<RtScene>>`: the same content-hash
/// deduplication, but a mesh's *representation* is no longer decided the
/// moment it is first seen.
pub(super) struct MeshArena {
    /// Indexed by the `u32` in `by_key`. Iterate this, never `by_key` — a
    /// `HashMap`'s order is not stable and the build must be deterministic.
    pub(super) slots: Vec<MeshSlot>,
    pub(super) by_key: HashMap<MeshKey, u32>,
    /// How this load refines subdivision surfaces. Here rather than on the
    /// import context because every path that reads a mesh — direct prims
    /// and prototype parts alike — already holds the arena.
    pub(super) subdiv: SubdivPolicy,
}

/// The load-wide subdivision choices [`mesh_source`] applies to each mesh.
pub(super) struct SubdivPolicy {
    /// The one refinement level, resolved by
    /// [`resolve_subdiv_level`](super::attrs::resolve_subdiv_level). In
    /// adaptive mode, the level of shared prototypes.
    pub(super) level: u32,
    /// Adaptive mode: each mesh's level comes from its size on screen
    /// ([`ScreenRate`]) instead of [`SubdivPolicy::level`]. `None` is uniform.
    pub(super) adaptive: Option<ScreenRate>,
    /// Subdivision meshes read, by the level each was refined to: a direct
    /// prim once per placement, a prototype's mesh once per version built.
    pub(super) levels: Vec<u64>,
    /// Meshes tessellated per face, and adaptive meshes that took the
    /// per-mesh level instead (a `loop` mesh, a face-varying chart, or
    /// `CRUST_ADAPTIVE_PER_FACE=0`).
    pub(super) per_face_meshes: u64,
    pub(super) per_face_fallbacks: u64,
    /// Meshes of shared prototypes, refined to [`SubdivPolicy::level`] in
    /// adaptive mode.
    pub(super) shared_meshes: u64,
    /// Cage edges and spokes of per-face meshes by rate, binned by
    /// `ceil(log2(rate))`.
    pub(super) rate_bins: Vec<u64>,
    /// Shapes of the per-face meshes' refined triangles (interior grids,
    /// stitched rings), for the end-of-import DEBUG line.
    pub(super) quality: [[u64; subdiv::QUALITY_BINS]; 2],
    /// `false` under `CRUST_SUBDIV=0`: every mesh renders its faceted cage,
    /// exactly as before subdivision surfaces were read at all — so, unlike
    /// level 0, not even smooth cage normals. The honest "off" side of the A/B.
    enabled: bool,
    /// Whether the retired per-prim `crust:subdivisionLevel` has been warned
    /// about: once per load, since a per-prim warning would scale with the
    /// scene.
    legacy_warned: bool,
}

impl SubdivPolicy {
    pub(super) fn new(level: u32) -> Self {
        SubdivPolicy {
            level,
            adaptive: None,
            levels: Vec::new(),
            per_face_meshes: 0,
            per_face_fallbacks: 0,
            shared_meshes: 0,
            rate_bins: Vec::new(),
            quality: [[0; subdiv::QUALITY_BINS]; 2],
            enabled: crate::config().subdiv,
            legacy_warned: false,
        }
    }

    /// Adaptive subdivision under `rate`, shared prototypes refined to
    /// `shared_level`.
    pub(super) fn adaptive(rate: ScreenRate, shared_level: u32) -> Self {
        SubdivPolicy {
            adaptive: Some(rate),
            ..SubdivPolicy::new(shared_level)
        }
    }

    /// The level a subdivision mesh with this cage is refined to, read for
    /// `place`.
    fn level_for(
        &mut self,
        prim: &Prim,
        points: &[Vec3f],
        counts: &[i32],
        indices: &[i32],
        place: MeshPlace<'_>,
    ) -> u32 {
        let level = match (self.adaptive, place) {
            (None, _) => self.level,
            (Some(_), MeshPlace::Shared) => {
                self.shared_meshes += 1;
                self.level
            }
            (Some(rate), MeshPlace::World(xf)) => {
                let edge = adaptive::mean_edge_length(points, counts, indices);
                let (sigma, distance) = match Aabb::of_points(points) {
                    Some(bounds) => (
                        rate.sigma(xf, &bounds),
                        Some(bounds.transformed(xf).distance_to(rate.eye)),
                    ),
                    None => (0.0, None),
                };
                let level = rate.level(edge, sigma);
                debug!(
                    "Mesh at {}: adaptive level {level} (mean cage edge {edge}, {} px per unit{}, \
                     edge {} px)",
                    prim.path(),
                    sigma,
                    distance.map_or(String::new(), |d| format!(" at distance {d}")),
                    edge * sigma
                );
                level
            }
        };
        let n = level as usize;
        if self.levels.len() <= n {
            self.levels.resize(n + 1, 0);
        }
        self.levels[n] += 1;
        level
    }
}

/// Where [`mesh_source`] reads a mesh for, which in adaptive mode decides how
/// it is refined.
#[derive(Clone, Copy)]
pub(super) enum MeshPlace<'a> {
    /// Unshared geometry — a direct prim, or a part of a prototype placed once
    /// — at its world transform: refined by its size on screen.
    World(&'a GMat4),
    /// A part of a shared prototype: refined to the uniform level.
    Shared,
}

/// A mesh's `subdivisionScheme`, unauthored (or blocked) reading the schema
/// fallback, `catmullClark`.
fn subdivision_scheme(mesh: &UsdMesh) -> SubdivisionScheme {
    mesh.subdivision_scheme_attr()
        .get_at::<SubdivisionScheme>(eval_time())
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// A direct mesh prim whose geometry is recorded but not yet attached.
pub(super) struct MeshPlacement {
    /// Reserved during traversal, so ids keep their traversal order.
    pub(super) geom_id: u32,
    /// Index into [`MeshArena::slots`].
    pub(super) slot: u32,
    pub(super) l2w: Affine3A,
    pub(super) motion: Option<Vec3>,
}

impl MeshArena {
    pub(super) fn new(subdiv: SubdivPolicy) -> Self {
        MeshArena {
            slots: Vec::new(),
            by_key: HashMap::new(),
            subdiv,
        }
    }

    /// Interns a mesh by content, returning its slot index. Triangulates on
    /// first sight; `None` if nothing survives triangulation (matching the
    /// old behaviour, which also did not cache a failed mesh).
    ///
    /// Takes the source by value: its normals move into the slot rather than
    /// being copied, and nothing reads a source after it is interned.
    pub(super) fn intern(
        &mut self,
        prim: &Prim,
        src: MeshSource,
        material: &Arc<dyn Material>,
    ) -> Option<u32> {
        let key = MeshKey::new(&src, material);
        if let Some(&slot) = self.by_key.get(&key) {
            debug!(
                "Mesh at {} shares geometry with an earlier prim",
                prim.path()
            );
            return Some(slot);
        }
        let verts: Vec<[f32; 3]> = src.points.iter().map(|p| [p.x, p.y, p.z]).collect();
        let want_faces = material.face_texture().is_some();
        let (tris, faces, uvs) = triangulate(
            &src.counts,
            &src.indices,
            verts.len(),
            want_faces,
            src.uvs.as_ref(),
        )
        .or_else(|| {
            debug!("Mesh at {} produced no triangles", prim.path());
            None
        })?;
        // A subdivided face table numbers *refined* faces; rewrite it to the
        // base-cage ids Ptex actually indexes before anything caches it.
        let faces = match (&src.subdiv_faces, faces) {
            (Some(sub), Some(map)) => Some(remap_refined_faces(map, sub)),
            (_, faces) => faces,
        };
        check_face_count(prim, src.base_face_count, material.as_ref());
        // Texture-footprint densities, built here and only here: this is the
        // one funnel every shared mesh passes through — direct meshes, baked
        // or instanced, and prototype parts alike — and the one place that
        // holds the mesh's *local* vertices, which is the frame the densities
        // have to be in. After the subdiv remap above, so a refined mesh's
        // sub-face UVs are the ones measured.
        let faces = faces.map(|mut m| {
            m.build_density(&verts, &tris);
            Arc::new(m)
        });
        let uvs = uvs.map(|mut m| {
            m.build_density(&verts, &tris);
            Arc::new(m)
        });
        let slot = self.slots.len() as u32;
        self.slots.push(MeshSlot {
            local: Some(MeshGeom {
                verts,
                tris,
                normals: src.normals,
            }),
            faces,
            uvs,
            committed: None,
            n_place: 0,
        });
        self.by_key.insert(key, slot);
        Some(slot)
    }

    /// The slot's geometry as a committed local-space kernel scene, built on
    /// first demand and shared thereafter. Once this is called the slot can
    /// no longer be baked — see [`MeshSlot::local`].
    pub(super) fn committed_scene(&mut self, slot: u32) -> Arc<RtScene> {
        let s = &mut self.slots[slot as usize];
        if let Some(scene) = &s.committed {
            return Arc::clone(scene);
        }
        let geom = s
            .local
            .take()
            .expect("a slot is either still local or already committed");
        let scene = Arc::new(commit_mesh(geom));
        s.committed = Some(Arc::clone(&scene));
        scene
    }

    /// Commits every slot in `slots` not committed yet, in parallel, so the
    /// [`MeshArena::committed_scene`] calls that follow are cache hits.
    ///
    /// A prototype's meshes used to be built one at a time as the walk met
    /// them — 21% of ALab's import on one core. Each build is independent and
    /// deterministic, so building them side by side gives the same scenes.
    pub(super) fn commit_slots(&mut self, slots: &[u32]) {
        let mut todo: Vec<u32> = slots
            .iter()
            .copied()
            .filter(|&k| self.slots[k as usize].committed.is_none())
            .collect();
        todo.sort_unstable();
        todo.dedup();
        if todo.len() < 2 {
            return; // nothing to overlap; `committed_scene` builds it
        }
        let geoms: Vec<(u32, MeshGeom)> = todo
            .into_iter()
            .map(|k| {
                let geom = self.slots[k as usize]
                    .local
                    .take()
                    .expect("an uncommitted slot still holds its geometry");
                (k, geom)
            })
            .collect();
        // Built in batches of bounded total size rather than all at once. A
        // build's transient memory (references, binary nodes) is proportional
        // to its triangles, so an unbounded parallel map lets a prototype of
        // several large meshes hold all their transients together. Batching by
        // triangle count keeps many small meshes fully parallel and a large
        // one alone, as it was when this was sequential.
        let mut geoms = geoms.into_iter().peekable();
        while geoms.peek().is_some() {
            let mut batch = Vec::new();
            let mut tris = 0usize;
            while let Some((_, g)) = geoms.peek() {
                if !batch.is_empty() && tris + g.tris.len() > PARALLEL_COMMIT_TRIS {
                    break;
                }
                tris += g.tris.len();
                batch.push(geoms.next().expect("peeked"));
            }
            let built: Vec<(u32, RtScene)> = batch
                .into_par_iter()
                .map(|(k, geom)| (k, commit_mesh(geom)))
                .collect();
            for (k, scene) in built {
                self.slots[k as usize].committed = Some(Arc::new(scene));
            }
        }
    }
}

/// Triangles `MeshArena::commit_slots` builds side by side at most — a bound
/// on the build transients that overlap. A mesh larger than this is built
/// alone, as every mesh was before the builds went parallel.
const PARALLEL_COMMIT_TRIS: usize = 2_000_000;

/// A local-space mesh as a committed kernel scene.
fn commit_mesh(geom: MeshGeom) -> RtScene {
    let mut b = RtSceneBuilder::new();
    b.attach(Geometry::TriangleMesh {
        vertices: geom.verts,
        indices: geom.tris,
        normals: geom.normals,
    });
    b.commit_with(crate::commit_options())
}

pub(super) fn emit_mesh(
    world: &mut WorldBuilder,
    prim: &Prim,
    mesh: &UsdMesh,
    world_xf: GMat4,
    material: Arc<dyn Material>,
    meshes: &mut MeshArena,
    pending: &mut Vec<MeshPlacement>,
) {
    let want_faces = material.face_texture().is_some();
    let want_uvs = material.uses_uv();
    let Some(src) = mesh_source(
        prim,
        mesh,
        want_faces,
        want_uvs,
        material.uv_primvar(),
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
pub(super) fn flush_meshes(
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
fn bake_verts(verts: &[[f32; 3]], l2w: &Affine3A) -> Vec<[f32; 3]> {
    verts
        .iter()
        .map(|v| l2w.transform_point3a(Vec3A::from_array(*v)).to_array())
        .collect()
}

/// Local-space shading normals into world space: the inverse transpose —
/// exactly the matrix the kernel's instance path applies (`w2l`'s linear part
/// transposed, in `crust-rt`), so a baked placement shades identically to an
/// instanced one, mirrors included.
fn bake_normals(normals: &[[f32; 3]], l2w: &Affine3A) -> Vec<[f32; 3]> {
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
fn bake_indices(mut tris: Vec<[u32; 3]>, l2w: &Affine3A) -> Vec<[u32; 3]> {
    if l2w.matrix3.determinant() < 0.0 {
        for t in &mut tris {
            t.swap(1, 2);
        }
    }
    tris
}

/// Reads a mesh prim's authored arrays. `None` when any of the three
/// required attributes is missing.
pub(super) fn mesh_arrays(mesh: &UsdMesh) -> Option<(Vec<Vec3f>, Vec<i32>, Vec<i32>)> {
    let int_vec = |v: sdf::Value| match v {
        sdf::Value::IntVec(v) => Some(v),
        _ => None,
    };
    let points = match mesh
        .points_attr()
        .get_at::<sdf::Value>(eval_time())
        .ok()
        .flatten()?
    {
        sdf::Value::Vec3fVec(v) => v,
        _ => return None,
    };
    let counts = int_vec(
        mesh.face_vertex_counts_attr()
            .get_at::<sdf::Value>(eval_time())
            .ok()
            .flatten()?,
    )?;
    let indices = int_vec(
        mesh.face_vertex_indices_attr()
            .get_at::<sdf::Value>(eval_time())
            .ok()
            .flatten()?,
    )?;
    Some((points, counts, indices))
}

/// The `st` primvar as authored, before triangulation resolves it.
///
/// USD stores texture coordinates as a value array plus an optional index
/// array, interpolated either per **point** (`vertex`/`varying`) or per
/// **face-vertex** (`faceVarying`). The distinction is not cosmetic: a vertex
/// on a UV seam has one position but two texture coordinates, which only the
/// faceVarying form can express — and it is the form both DPEL assets use.
pub(super) struct UvSource {
    pub(super) values: Vec<[f32; 2]>,
    /// `primvars:st:indices`, when authored. Indexes `values`.
    pub(super) indices: Option<Vec<i32>>,
    /// True for `faceVarying`: the lookup index is the running face-vertex
    /// offset rather than the point index.
    pub(super) face_varying: bool,
}

impl UvSource {
    /// The index into `values` of the coordinate at face-vertex `fv`, whose
    /// point index is `point`; `None` when the source does not resolve it
    /// (a negative or out-of-range entry), which reads as `(0, 0)`.
    pub(super) fn index_at(&self, fv: usize, point: usize) -> Option<u32> {
        let i = if self.face_varying { fv } else { point };
        let i = match &self.indices {
            Some(idx) => match idx.get(i) {
                Some(&v) if v >= 0 => v as usize,
                _ => return None,
            },
            None => i,
        };
        (i < self.values.len()).then_some(i as u32)
    }
}

/// Reads a mesh's texture-coordinate primvar.
///
/// `st` is USD's conventional name and what `UsdPreviewSurface` and MaterialX
/// both assume; `uv` and `st0` are read as fallbacks because exporters differ
/// and an asset with the chart under another name is otherwise silently
/// untextured. The first one that yields values wins. `preferred` — the
/// primvar the bound material's network names ([`Material::uv_primvar`]) — is
/// tried before all of them.
pub(super) fn mesh_uvs(prim: &Prim, preferred: Option<&str>) -> Option<UvSource> {
    let preferred = preferred.map(|p| format!("primvars:{p}"));
    for name in preferred.as_deref().into_iter().chain([
        "primvars:st",
        "primvars:uv",
        "primvars:st0",
        "primvars:UVMap",
    ]) {
        let value = prim
            .attribute(name)
            .get_at::<sdf::Value>(eval_time())
            .ok()
            .flatten();
        let values = match value {
            // `texCoord2f[]` and `float2[]` are the same bits; which one an
            // exporter writes is a matter of taste.
            Some(sdf::Value::Vec2fVec(v)) => v.iter().map(|p| [p.x, p.y]).collect::<Vec<_>>(),
            _ => continue,
        };
        if values.is_empty() {
            continue;
        }
        let indices = match prim
            .attribute(format!("{name}:indices"))
            .get_at::<sdf::Value>(eval_time())
            .ok()
            .flatten()
        {
            Some(sdf::Value::IntVec(v)) => Some(v),
            _ => None,
        };
        // USD's fallback interpolation for a primvar is `constant`, but for
        // `st` in practice it is always authored; treating an unauthored
        // metadatum as faceVarying would mis-index a vertex-interpolated
        // chart, so the authored value decides and `vertex` is the fallback.
        let face_varying = matches!(
            prim.attribute(name)
                .get_metadata::<sdf::Value>("interpolation")
                .ok()
                .flatten(),
            Some(sdf::Value::Token(t)) if t.as_str() == "faceVarying"
        );
        return Some(UvSource {
            values,
            indices,
            face_varying,
        });
    }
    None
}

/// One mesh's geometry as the rest of the importer consumes it — either the
/// authored cage verbatim, or its subdivision-surface refinement when the
/// mesh authors a subdivision scheme (see [`mesh_source`]). Refinement happens *here*, before
/// interning, so every downstream path — direct bake, deferred
/// instance-vs-bake, prototypes — sees it exactly once, and [`MeshKey`]
/// dedupes on the refined arrays (two prims sharing a cage at different
/// levels hash differently, at the same level they still share).
pub(super) struct MeshSource {
    pub(super) points: Vec<Vec3f>,
    pub(super) counts: Vec<i32>,
    pub(super) indices: Vec<i32>,
    /// Smooth shading normals — `Some` iff subdivided (a cage renders
    /// faceted, exactly as before).
    pub(super) normals: Option<Vec<[f32; 3]>>,
    /// Refined-face → base-cage-face mapping, `Some` iff subdivided and the
    /// material wants a face table.
    pub(super) subdiv_faces: Option<RefinedFaces>,
    /// The *authored* cage's face count — what Ptex face ids index, whether
    /// or not the mesh was refined.
    pub(super) base_face_count: usize,
    /// `primvars:st`, `Some` iff the bound material reads texture
    /// coordinates. For a subdivided mesh this is the *refined* chart — the
    /// cage's UVs on refined triangles would stretch every texture across the
    /// patch it came from.
    pub(super) uvs: Option<UvSource>,
}

/// How a refined mesh's triangles map back to the cage faces Ptex addresses.
pub(super) enum RefinedFaces {
    /// Uniform (or per-mesh adaptive) refinement: dyadic cells of cage faces.
    Uniform(subdiv::SubdivFaces),
    /// Per-face tessellation: explicit corners per triangle.
    PerFace(subdiv::TessellatedFaces),
}

/// Reads a mesh prim's arrays and refines them when the mesh is a
/// subdivision surface.
///
/// A mesh is one unless its `subdivisionScheme` is `none` — *including* when
/// the scheme is unauthored, since USD's fallback is `catmullClark`. That is
/// how production assets mark a subdivision surface: ALab's and
/// Kitchen_set's render meshes author neither a scheme nor normals, while
/// ALab's polygonal display proxies author `none` and normals. It is refined
/// to the load's one [`SubdivPolicy::level`], or in adaptive mode to the level
/// its size on screen asks for when read for `place`; at level 0 it renders its
/// cage with smooth normals rather than faceted.
///
/// `None` when the required attributes are missing (matching
/// [`mesh_arrays`]); any subdivision problem warns and degrades to the cage.
pub(super) fn mesh_source(
    prim: &Prim,
    mesh: &UsdMesh,
    want_faces: bool,
    want_uvs: bool,
    uv_primvar: Option<&str>,
    policy: &mut SubdivPolicy,
    place: MeshPlace<'_>,
) -> Option<MeshSource> {
    let (points, counts, indices) = mesh_arrays(mesh)?;
    let base_face_count = counts.len();
    let uvs = want_uvs.then(|| mesh_uvs(prim, uv_primvar)).flatten();
    let cage = |points, counts, indices, uvs| MeshSource {
        points,
        counts,
        indices,
        normals: None,
        subdiv_faces: None,
        base_face_count,
        uvs,
    };

    if !policy.legacy_warned && custom_i32(prim, "crust:subdivisionLevel").is_some() {
        policy.legacy_warned = true;
        warn!(
            "Mesh at {} (and possibly others): the per-prim crust:subdivisionLevel \
             is no longer read — a mesh is subdivided when it authors \
             subdivisionScheme, to the level set by crust:subdivisionLevel on the \
             RenderSettings prim or --subdiv-level",
            prim.path()
        );
    }

    let usd_scheme = subdivision_scheme(mesh);
    if !policy.enabled || usd_scheme == SubdivisionScheme::None {
        return Some(cage(points, counts, indices, uvs));
    }
    // Adaptive mode tessellates unshared meshes per face, but for a `loop`
    // cage, which keeps the per-mesh level: the tessellator cuts quad Ptex
    // faces only.
    let per_face = policy.adaptive.is_some()
        && matches!(place, MeshPlace::World(_))
        && crate::config().adaptive_per_face
        && usd_scheme != SubdivisionScheme::Loop;
    if policy.adaptive.is_some() && matches!(place, MeshPlace::World(_)) && !per_face {
        policy.per_face_fallbacks += 1;
    }
    let level = if per_face {
        policy.adaptive.map_or(0, |r| r.max)
    } else {
        policy.level_for(prim, &points, &counts, &indices, place)
    };
    // Loop refinement builds no face table (Ptex addresses quad sub-faces),
    // so a Ptex lookup on its triangles would read refined face ordinals as
    // cage face ids — a plausible, wrong texture. Keep the cage instead.
    let loop_ptex = usd_scheme == SubdivisionScheme::Loop && want_faces;
    if loop_ptex && level > 0 {
        warn!(
            "Mesh at {}: subdivisionScheme = loop with a per-face (Ptex) texture \
             cannot keep its face ids through refinement — rendering the smooth \
             base cage",
            prim.path()
        );
    }
    if !per_face && (level == 0 || loop_ptex) {
        let normals = subdiv::smooth_cage_normals(&points, &counts, &indices);
        return Some(MeshSource {
            normals,
            ..cage(points, counts, indices, uvs)
        });
    }
    let scheme = match usd_scheme {
        SubdivisionScheme::CatmullClark => subdiv::SubdivScheme::CatmullClark,
        SubdivisionScheme::Bilinear => subdiv::SubdivScheme::Bilinear,
        SubdivisionScheme::Loop => {
            if counts.iter().any(|&c| c != 3) {
                warn!(
                    "Mesh at {}: subdivisionScheme = loop needs an all-triangle \
                     mesh — rendering the base cage",
                    prim.path()
                );
                return Some(cage(points, counts, indices, uvs));
            }
            subdiv::SubdivScheme::Loop
        }
        SubdivisionScheme::None => return Some(cage(points, counts, indices, uvs)),
    };

    let boundary = match mesh
        .interpolate_boundary_attr()
        .get_at::<InterpolateBoundary>(eval_time())
        .ok()
        .flatten()
        .unwrap_or_default()
    {
        InterpolateBoundary::None => opensubdiv_rs::sdc::VtxBoundaryInterpolation::None,
        InterpolateBoundary::EdgeOnly => opensubdiv_rs::sdc::VtxBoundaryInterpolation::EdgeOnly,
        InterpolateBoundary::EdgeAndCorner => {
            opensubdiv_rs::sdc::VtxBoundaryInterpolation::EdgeAndCorner
        }
    };

    let int_array =
        |attr: openusd::usd::Attribute| match attr.get_at::<sdf::Value>(eval_time()).ok().flatten()
        {
            Some(sdf::Value::IntVec(v)) => v,
            _ => Vec::new(),
        };
    let float_array =
        |attr: openusd::usd::Attribute| match attr.get_at::<sdf::Value>(eval_time()).ok().flatten()
        {
            Some(sdf::Value::FloatVec(v)) => v,
            _ => Vec::new(),
        };
    let crease_indices = int_array(mesh.crease_indices_attr());
    let crease_lengths = int_array(mesh.crease_lengths_attr());
    let crease_sharpnesses = float_array(mesh.crease_sharpnesses_attr());
    let corner_indices = int_array(mesh.corner_indices_attr());
    let corner_sharpnesses = float_array(mesh.corner_sharpnesses_attr());

    // The chart the refiner carries. One it cannot index (a negative or
    // out-of-range entry) is dropped rather than refined into garbage: the
    // surface then renders on the material's constant inputs, as every
    // subdivided mesh did before charts were refined.
    let chart = uvs.as_ref().and_then(|uv| {
        let channel = subdiv::UvChannel {
            values: &uv.values,
            indices: uv.indices.as_deref(),
            face_varying: uv.face_varying,
            linear: face_varying_linear(mesh),
        };
        let n_entries = if uv.face_varying {
            indices.len()
        } else {
            points.len()
        };
        if channel.is_well_formed(n_entries) {
            Some(channel)
        } else {
            warn!(
                "Mesh at {}: texture coordinates do not index cleanly into their \
                 values — the subdivided surface renders without them",
                prim.path()
            );
            None
        }
    });

    let req = subdiv::SubdivRequest {
        scheme,
        level,
        boundary,
        crease_indices: &crease_indices,
        crease_lengths: &crease_lengths,
        crease_sharpnesses: &crease_sharpnesses,
        corner_indices: &corner_indices,
        corner_sharpnesses: &corner_sharpnesses,
        want_face_uvs: want_faces,
        uvs: chart,
    };
    if per_face
        && let Some(rate) = policy.adaptive
        && let MeshPlace::World(xf) = place
    {
        let segment = |pts: &[[f32; 3]]| rate.segment_at(xf, pts);
        match subdiv::tessellate_adaptive(&points, &counts, &indices, &req, rate.max, &segment) {
            Ok(t) => {
                policy.per_face_meshes += 1;
                for (into, from) in policy.quality.iter_mut().zip(&t.quality) {
                    for (i, f) in into.iter_mut().zip(from) {
                        *i += f;
                    }
                }
                for (b, &n) in t.rate_bins.iter().enumerate() {
                    if policy.rate_bins.len() <= b {
                        policy.rate_bins.resize(b + 1, 0);
                    }
                    policy.rate_bins[b] += n;
                }
                debug!(
                    "Mesh at {}: tessellated per face ({} Ptex faces -> {} triangles, edge rates {}..={})",
                    prim.path(),
                    t.ptex_faces,
                    t.indices.len() / 3,
                    t.rate_range.0,
                    t.rate_range.1
                );
                let n_tris = t.indices.len() / 3;
                return Some(MeshSource {
                    points: t.points,
                    counts: vec![3; n_tris],
                    indices: t.indices,
                    normals: Some(t.normals),
                    subdiv_faces: t.faces.map(RefinedFaces::PerFace),
                    base_face_count,
                    uvs: match (t.uvs, t.face_varying_uvs) {
                        (Some(values), _) => Some(UvSource {
                            values,
                            indices: None,
                            face_varying: false,
                        }),
                        (None, Some((values, corners))) => Some(UvSource {
                            values,
                            indices: Some(corners),
                            face_varying: true,
                        }),
                        (None, None) => None,
                    },
                });
            }
            Err(e) => {
                warn!(
                    "Mesh at {}: per-face tessellation failed ({e}) — rendering the base cage",
                    prim.path()
                );
                return Some(cage(points, counts, indices, uvs));
            }
        }
    }
    match subdiv::subdivide(&points, &counts, &indices, &req) {
        Ok(refined) => {
            debug!(
                "Mesh at {}: subdivided to level {level} ({} -> {} faces)",
                prim.path(),
                base_face_count,
                refined.counts.len()
            );
            Some(MeshSource {
                points: refined.points,
                counts: refined.counts,
                indices: refined.indices,
                normals: Some(refined.normals),
                subdiv_faces: refined.faces.map(RefinedFaces::Uniform),
                base_face_count,
                uvs: refined.uvs.map(|uv| UvSource {
                    values: uv.values,
                    indices: uv.indices,
                    face_varying: uv.face_varying,
                }),
            })
        }
        Err(e) => {
            warn!(
                "Mesh at {}: subdivision failed ({e}) — rendering the base cage",
                prim.path()
            );
            // The cage fallback recovers the chart: unrefined triangles
            // index it exactly as authored.
            Some(cage(points, counts, indices, uvs))
        }
    }
}

/// The mesh's `faceVaryingLinearInterpolation`, mapped one to one. Unauthored
/// is USD's fallback, `cornersPlus1` — not OpenSubdiv's own default,
/// `cornersOnly`.
fn face_varying_linear(mesh: &UsdMesh) -> opensubdiv_rs::sdc::FVarLinearInterpolation {
    use opensubdiv_rs::sdc::FVarLinearInterpolation as Osd;
    match mesh
        .face_varying_linear_interpolation_attr()
        .get_at::<FaceVaryingLinearInterpolation>(eval_time())
        .ok()
        .flatten()
        .unwrap_or_default()
    {
        FaceVaryingLinearInterpolation::None => Osd::None,
        FaceVaryingLinearInterpolation::CornersOnly => Osd::CornersOnly,
        FaceVaryingLinearInterpolation::CornersPlus1 => Osd::CornersPlus1,
        FaceVaryingLinearInterpolation::CornersPlus2 => Osd::CornersPlus2,
        FaceVaryingLinearInterpolation::Boundaries => Osd::Boundaries,
        FaceVaryingLinearInterpolation::All => Osd::All,
    }
}

/// [`remap_subdivided_faces`] or [`remap_tessellated_faces`], by how the mesh
/// was refined.
fn remap_refined_faces(map: FaceMap, faces: &RefinedFaces) -> FaceMap {
    match faces {
        RefinedFaces::Uniform(sub) => remap_subdivided_faces(map, sub),
        RefinedFaces::PerFace(per_face) => remap_tessellated_faces(map, per_face),
    }
}

/// Rewrites a [`FaceMap`] built by triangulating a per-face tessellation —
/// all triangles, so `faces` numbers them — into cage face ids plus each
/// triangle's explicit corners. A triangle of an `n`-gon is unmappable, as
/// under uniform refinement.
fn remap_tessellated_faces(map: FaceMap, t: &subdiv::TessellatedFaces) -> FaceMap {
    let n = map.faces.len();
    let mut faces = Vec::with_capacity(n);
    let mut slices = Vec::with_capacity(n);
    let mut corners = Vec::with_capacity(n);
    for &tri in &map.faces {
        let tri = tri as usize;
        match t.base_face.get(tri).copied().flatten() {
            Some(base) => {
                faces.push(base);
                slices.push(FanSlice::Triangle);
                corners.push(t.corner_uvs[tri]);
            }
            None => {
                faces.push(u32::MAX);
                slices.push(FanSlice::Unmappable);
                corners.push([[0.0; 2]; 3]);
            }
        }
    }
    FaceMap {
        faces,
        slices,
        sub: None,
        corners: Some(corners),
        density: Vec::new(),
    }
}

/// Rewrites a [`FaceMap`] built by triangulating *refined* quads — whose
/// `faces` therefore number refined faces — into base-cage face ids plus
/// explicit sub-face UVs. The fan of a refined quad is `k=1 -> (v0,v1,v2)`
/// (`QuadLower`) and `k=2 -> (v0,v2,v3)` (`QuadUpper`), so each triangle
/// takes the matching three of its face's four corner UVs.
fn remap_subdivided_faces(map: FaceMap, sub: &subdiv::SubdivFaces) -> FaceMap {
    let n = map.faces.len();
    let mut faces = Vec::with_capacity(n);
    let mut slices = Vec::with_capacity(n);
    let mut subs = Vec::with_capacity(n);
    for (&refined, &slice) in map.faces.iter().zip(&map.slices) {
        // A refined face with no Ptex-addressable ancestor (its cage face
        // was not a quad), or — never seen, since the refiner halves
        // dyadics — one whose corners are not a dyadic cell: unmappable.
        let cell = sub.base_face[refined as usize].and_then(|base| {
            SubFace::from_corners(&sub.corner_uvs[refined as usize]).map(|c| (base, c))
        });
        match cell {
            Some((base, cell)) => {
                faces.push(base);
                slices.push(slice);
                subs.push(cell);
            }
            None => {
                // The face table's own sentinel: there the layout is the point.
                faces.push(u32::MAX);
                slices.push(FanSlice::Unmappable);
                subs.push(SubFace::default());
            }
        }
    }
    FaceMap {
        faces,
        slices,
        sub: Some(subs),
        corners: None,
        density: Vec::new(),
    }
}

/// The uniform scale a placement applies, as the geometric mean of its three
/// axis scales.
///
/// Both side tables' densities are in the mesh's local frame, so this is what
/// converts a world-space texture footprint into that frame. `cbrt` of the
/// determinant is the mean because the determinant is the product of the
/// three scales; a non-uniform placement is therefore filtered by their mean,
/// which is all an isotropic cone could have used anyway.
pub(super) fn placement_scale(l2w: &Affine3A) -> f32 {
    let det = l2w.matrix3.determinant().abs();
    if det > 0.0 { det.cbrt() } else { 1.0 }
}

/// Warns when a per-face texture's face count disagrees with the mesh's.
///
/// A Ptex face id *is* a mesh face index, so the two counts must match
/// exactly. When they do not, the texture belongs to different geometry and
/// every lookup is quietly wrong — the render still completes, and still looks
/// like a plausible rock, which is precisely what makes it worth a warning.
/// `n_base_faces` is the *authored cage's* face count
/// ([`MeshSource::base_face_count`]) — never the refined count: subdivision
/// changes the mesh's face count but Ptex ids keep indexing the cage.
fn check_face_count(prim: &Prim, n_base_faces: usize, material: &dyn Material) {
    let Some(tex) = material.face_texture() else {
        return;
    };
    if tex.num_faces() != n_base_faces {
        warn!(
            "Mesh at {} has {} faces but its per-face texture has {} — \
             the texture does not match this geometry, so shading will be wrong",
            prim.path(),
            n_base_faces,
            tex.num_faces()
        );
    }
}

/// Fan-triangulates the faces into an index-triple list; `None` if
/// nothing survives.
///
/// With `want_faces`, also returns the table mapping each emitted triangle
/// back to the source face it was cut from — what a per-face (Ptex) texture
/// needs, since its face ids index `counts`, not the triangles. The two
/// outputs are index-parallel by construction: every `push` to one pushes to
/// the other in the same statement, so the skip paths cannot desynchronise
/// them.
///
/// The triangle list, plus the per-face and per-triangle-UV side tables
/// when the material asked for them.
type Triangulated = (Vec<[u32; 3]>, Option<FaceMap>, Option<UvMap>);

fn triangulate(
    counts: &[i32],
    indices: &[i32],
    n_verts: usize,
    want_faces: bool,
    uv_src: Option<&UvSource>,
) -> Option<Triangulated> {
    let mut tris: Vec<[u32; 3]> = Vec::new();
    let mut faces: Vec<u32> = Vec::new();
    let mut slices: Vec<FanSlice> = Vec::new();
    let mut corners: Vec<[u32; 3]> = Vec::new();
    // The chart's values plus one `(0, 0)` at the end for every corner the
    // source cannot index (`UvSource::at`'s answer), so a corner is an index
    // and never a copied value.
    let fallback = uv_src.map_or(0, |src| src.values.len() as u32);
    let mut offset = 0usize;
    for (face, &fc) in counts.iter().enumerate() {
        let fc = fc as usize;
        if fc < 3 || offset + fc > indices.len() {
            offset += fc;
            continue;
        }
        for k in 1..(fc - 1) {
            let i0 = indices[offset];
            let i1 = indices[offset + k];
            let i2 = indices[offset + k + 1];
            if i0 < 0 || i1 < 0 || i2 < 0 {
                continue;
            }
            let (i0, i1, i2) = (i0 as u32, i1 as u32, i2 as u32);
            if i0 as usize >= n_verts || i1 as usize >= n_verts || i2 as usize >= n_verts {
                continue;
            }
            tris.push([i0, i1, i2]);
            // Pushed in the same statement sequence as the triangle, so the
            // skip paths above cannot desynchronise the tables from it —
            // the same discipline `faces`/`slices` rely on.
            if let Some(src) = uv_src {
                // A faceVarying chart is indexed by the *face-vertex* slot,
                // which is why the fan's offsets are carried through rather
                // than just the point indices: two faces meeting at a seam
                // share `i0` but not its texture coordinate.
                corners.push([
                    src.index_at(offset, i0 as usize).unwrap_or(fallback),
                    src.index_at(offset + k, i1 as usize).unwrap_or(fallback),
                    src.index_at(offset + k + 1, i2 as usize)
                        .unwrap_or(fallback),
                ]);
            }
            if want_faces {
                faces.push(face as u32);
                // Ptex defines quad and triangle faces only, so a larger
                // polygon has no addressable texture — mark it rather than
                // inventing a parameterisation for it.
                slices.push(match (fc, k) {
                    (3, _) => FanSlice::Triangle,
                    (4, 1) => FanSlice::QuadLower,
                    (4, 2) => FanSlice::QuadUpper,
                    _ => FanSlice::Unmappable,
                });
            }
        }
        offset += fc;
    }
    if tris.is_empty() {
        return None;
    }
    // `then_some` rather than `then`: the value is two already-built vectors
    // being moved, so there is no work for a closure to defer.
    let map = want_faces.then_some(FaceMap {
        faces,
        slices,
        sub: None,
        corners: None,
        density: Vec::new(),
    });
    // The densities are left empty: they want the mesh's *local* vertices,
    // which this function does not receive, so the callers that hold them
    // fill them in (`MeshArena::intern`, and the non-invertible bake above).
    let uv_map = uv_src.map(|src| {
        let mut values = src.values.clone();
        values.push([0.0, 0.0]);
        UvMap {
            values,
            corners,
            density: Vec::new(),
        }
    });
    Some((tris, map, uv_map))
}

#[cfg(test)]
mod bake_tests {
    use super::*;
    use crate::material::OpenPBR;

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

#[cfg(test)]
mod face_table_tests {
    use super::*;

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

#[cfg(test)]
mod subdiv_policy_tests {
    use super::*;
    use openusd::usd::Stage;

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
            let prim = super::super::prim_at(&stage, p.clone());
            let mesh = UsdMesh::get(&stage, p).unwrap().expect("a mesh");
            let src = mesh_source(
                &prim,
                &mesh,
                false,
                false,
                None,
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
