//! A mesh's authored arrays and texture chart, and its subdivision-surface
//! refinement under the load's [`SubdivPolicy`]: what [`mesh_source`] hands
//! the rest of the importer.

use glam::Mat4 as GMat4;
use openusd::gf::Vec3f;
use openusd::sdf;
use openusd::usd::Prim;
use openusd_schemas::geom::{
    FaceVaryingLinearInterpolation, InterpolateBoundary, Mesh as UsdMesh, PointBased,
    SubdivisionScheme,
};
use tracing::{debug, warn};

use crate::scene::subdiv;

use super::super::adaptive::{self, Aabb, ScreenRate};
use super::super::attrs::{custom_i32, decode_f32s, decode_i32s, decode_vec3fs, value_at};
use super::super::time::eval_time;

/// The load-wide subdivision choices [`mesh_source`] applies to each mesh.
pub(in crate::scene::usd_import) struct SubdivPolicy {
    /// The one refinement level, resolved by
    /// [`resolve_subdiv_level`](super::attrs::resolve_subdiv_level). In
    /// adaptive mode, the level of shared prototypes.
    pub(in crate::scene::usd_import) level: u32,
    /// Adaptive mode: each mesh's level comes from its size on screen
    /// ([`ScreenRate`]) instead of [`SubdivPolicy::level`]. `None` is uniform.
    pub(in crate::scene::usd_import) adaptive: Option<ScreenRate>,
    /// Subdivision meshes read, by the level each was refined to: a direct
    /// prim once per placement, a prototype's mesh once per version built.
    pub(in crate::scene::usd_import) levels: Vec<u64>,
    /// Meshes tessellated per face, and adaptive meshes that took the
    /// per-mesh level instead (a `loop` mesh, a face-varying chart, or
    /// `CRUST_ADAPTIVE_PER_FACE=0`).
    pub(in crate::scene::usd_import) per_face_meshes: u64,
    pub(in crate::scene::usd_import) per_face_fallbacks: u64,
    /// Meshes of shared prototypes, refined to [`SubdivPolicy::level`] in
    /// adaptive mode.
    pub(in crate::scene::usd_import) shared_meshes: u64,
    /// Cage edges and spokes of per-face meshes by rate, binned by
    /// `ceil(log2(rate))`.
    pub(in crate::scene::usd_import) rate_bins: Vec<u64>,
    /// Shapes of the per-face meshes' refined triangles (interior grids,
    /// stitched rings), for the end-of-import DEBUG line.
    pub(in crate::scene::usd_import) quality: [[u64; subdiv::QUALITY_BINS]; 2],
    /// `false` under `CRUST_SUBDIV=0`: every mesh renders its faceted cage,
    /// exactly as before subdivision surfaces were read at all — so, unlike
    /// level 0, not even smooth cage normals. The honest "off" side of the A/B.
    enabled: bool,
    /// Whether the retired per-prim `crust:subdivisionLevel` has been warned
    /// about: once per load, since a per-prim warning would scale with the
    /// scene.
    pub(super) legacy_warned: bool,
}

impl SubdivPolicy {
    pub(in crate::scene::usd_import) fn new(level: u32) -> Self {
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
    pub(in crate::scene::usd_import) fn adaptive(rate: ScreenRate, shared_level: u32) -> Self {
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
pub(in crate::scene::usd_import) enum MeshPlace<'a> {
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

/// Reads a mesh prim's authored arrays. `None` when any of the three
/// required attributes is missing.
pub(super) fn mesh_arrays(mesh: &UsdMesh) -> Option<(Vec<Vec3f>, Vec<i32>, Vec<i32>)> {
    let points = decode_vec3fs(value_at(&mesh.points_attr())?)?;
    let counts = decode_i32s(value_at(&mesh.face_vertex_counts_attr())?)?;
    let indices = decode_i32s(value_at(&mesh.face_vertex_indices_attr())?)?;
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
        let values = match value_at(&prim.attribute(name)) {
            // `texCoord2f[]` and `float2[]` are the same bits; which one an
            // exporter writes is a matter of taste.
            Some(sdf::Value::Vec2fVec(v)) => v.iter().map(|p| [p.x, p.y]).collect::<Vec<_>>(),
            _ => continue,
        };
        if values.is_empty() {
            continue;
        }
        let indices = value_at(&prim.attribute(format!("{name}:indices"))).and_then(decode_i32s);
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
pub(in crate::scene::usd_import) struct MeshSource {
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
pub(in crate::scene::usd_import) fn mesh_source(
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
        |attr: openusd::usd::Attribute| value_at(&attr).and_then(decode_i32s).unwrap_or_default();
    let float_array =
        |attr: openusd::usd::Attribute| value_at(&attr).and_then(decode_f32s).unwrap_or_default();
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
