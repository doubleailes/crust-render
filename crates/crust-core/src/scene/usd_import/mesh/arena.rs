//! Distinct meshes, interned by content: each one's triangles until the
//! instance-vs-bake decision, then its committed kernel scene.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use crust_rt::{Geometry, Scene as RtScene, SceneBuilder as RtSceneBuilder};
use openusd::usd::Prim;
use rayon::prelude::*;
use tracing::debug;

use crate::material::Material;
use crate::rt_world::{FaceMap, UvMap};

use super::faces::{check_face_count, remap_refined_faces, triangulate};
use super::source::{MeshSource, SubdivPolicy};

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
pub(in crate::scene::usd_import) struct MeshSlot {
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
    pub(in crate::scene::usd_import) faces: Option<Arc<FaceMap>>,
    /// The mesh's texture chart, for a mesh whose material reads it. Lives
    /// beside `faces` and for the same reason: it has to outlive `local`,
    /// which both baking and committing drop. Shared by every placement —
    /// the tangent frame, which is per placement, is derived at the hit
    /// from the kernel's vertices rather than held here.
    pub(in crate::scene::usd_import) uvs: Option<Arc<UvMap>>,
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
pub(in crate::scene::usd_import) struct MeshArena {
    /// Indexed by the `u32` in `by_key`. Iterate this, never `by_key` — a
    /// `HashMap`'s order is not stable and the build must be deterministic.
    pub(in crate::scene::usd_import) slots: Vec<MeshSlot>,
    pub(super) by_key: HashMap<MeshKey, u32>,
    /// How this load refines subdivision surfaces. Here rather than on the
    /// import context because every path that reads a mesh — direct prims
    /// and prototype parts alike — already holds the arena.
    pub(in crate::scene::usd_import) subdiv: SubdivPolicy,
}

impl MeshArena {
    pub(in crate::scene::usd_import) fn new(subdiv: SubdivPolicy) -> Self {
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
    pub(in crate::scene::usd_import) fn intern(
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
    pub(in crate::scene::usd_import) fn committed_scene(&mut self, slot: u32) -> Arc<RtScene> {
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
    pub(in crate::scene::usd_import) fn commit_slots(&mut self, slots: &[u32]) {
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
