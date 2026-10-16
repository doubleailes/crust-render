//! Uniform refinement: the cage refined to one level and snapped to the limit.

use opensubdiv_rs::far::{
    FVarChannelDescriptor, PrimvarRefiner, TopologyDescriptor, TopologyRefinerFactory,
    UniformOptions,
};
use opensubdiv_rs::sdc;
use openusd::gf::Vec3f;

use super::normals::smooth_normals;
use super::topology::{expand_crease_runs, ptex_fvar_channel, validate_cage, validate_corners};
use super::{RefinedUvs, SubdivError, SubdivFaces, SubdivRequest, SubdivScheme, SubdividedMesh};

/// Uniformly refines the cage to `req.level` and snaps the result to the
/// limit surface. See the module docs for the shape of the answer.
pub(crate) fn subdivide(
    points: &[Vec3f],
    counts: &[i32],
    indices: &[i32],
    req: &SubdivRequest,
) -> Result<SubdividedMesh, SubdivError> {
    // `none` is the one face-varying rule that smooths face-varying corners,
    // which would slide the Ptex channel sharing its refiner off its
    // sub-faces. Refine the face table on its own, chartless, and the rest
    // without it — twice the refinement, for a material that reads Ptex *and*
    // a `none` chart. (Pinned by `ptex_channel_is_invariant_under_every_fvar_rule`.)
    if req.want_face_uvs
        && req
            .uvs
            .is_some_and(|c| c.face_varying && c.linear == sdc::FVarLinearInterpolation::None)
    {
        let faces = subdivide(
            points,
            counts,
            indices,
            &SubdivRequest { uvs: None, ..*req },
        )?
        .faces;
        let mut out = subdivide(
            points,
            counts,
            indices,
            &SubdivRequest {
                want_face_uvs: false,
                ..*req
            },
        )?;
        out.faces = faces;
        return Ok(out);
    }
    let (counts_us, indices_u32) = validate_cage(points.len(), counts, indices)?;
    let (crease_pairs, crease_weights) = expand_crease_runs(
        req.crease_indices,
        req.crease_lengths,
        req.crease_sharpnesses,
    )?;
    let corners = validate_corners(req.corner_indices, req.corner_sharpnesses)?;

    let scheme = match req.scheme {
        SubdivScheme::CatmullClark => sdc::SchemeType::Catmark,
        SubdivScheme::Bilinear => sdc::SchemeType::Bilinear,
        SubdivScheme::Loop => sdc::SchemeType::Loop,
    };
    // The face-varying rule is one per refiner, not per channel, so the
    // authored chart's rule wins. The synthetic Ptex channel must refine
    // bilinearly, and does under every rule but `none` (handled above) —
    // each of its values is private to one face, so every one of its edges
    // is a face-varying boundary, and its data is affine. Without a chart,
    // `All`.
    let chart = req.uvs.as_ref().filter(|c| c.face_varying);
    let options = sdc::Options::default()
        .with_vtx_boundary_interpolation(req.boundary)
        .with_fvar_linear_interpolation(
            chart.map_or(sdc::FVarLinearInterpolation::All, |c| c.linear),
        );

    // Loop cannot refine quads at all, and the Ptex channel only makes sense
    // for the quad-split schemes.
    let want_uvs = req.want_face_uvs && req.scheme != SubdivScheme::Loop;
    let (fvar_uvs, fvar_indices) = if want_uvs {
        ptex_fvar_channel(&counts_us)
    } else {
        (Vec::new(), Vec::new())
    };
    // A face-varying chart's per-face-vertex indices are its channel's
    // topology; its values seed the refinement.
    let chart_indices: Vec<u32> = match chart {
        Some(c) => (0..indices.len())
            .map(|fv| c.value_index(fv) as u32)
            .collect(),
        None => Vec::new(),
    };
    let mut channels = Vec::with_capacity(2);
    if want_uvs {
        channels.push(FVarChannelDescriptor::new(fvar_uvs.len(), &fvar_indices));
    }
    let chart_channel = chart.map(|c| {
        channels.push(FVarChannelDescriptor::new(c.values.len(), &chart_indices));
        channels.len() - 1
    });

    let mut descriptor = TopologyDescriptor::new(points.len(), &counts_us, &indices_u32)
        .with_creases(&crease_pairs, &crease_weights)
        .with_corners(&corners.0, &corners.1);
    if !channels.is_empty() {
        descriptor = descriptor.with_fvar_channels(&channels);
    }

    let mut refiner =
        TopologyRefinerFactory::create(descriptor, scheme, options).map_err(SubdivError::Refine)?;
    let level = req.level as usize;
    refiner.refine_uniform(UniformOptions::new(level));

    // Positions: interpolate level by level, then snap the last level to the
    // limit surface. (For Bilinear the limit is the refined mesh itself;
    // limit_level handles that uniformly.)
    let primvar = PrimvarRefiner::new(&refiner);
    let mut verts: Vec<[f32; 3]> = points.iter().map(|p| [p.x, p.y, p.z]).collect();
    for l in 1..=level {
        let mut refined = vec![[0.0f32; 3]; refiner.level(l).num_vertices()];
        primvar.interpolate(l, &verts, &mut refined);
        verts = refined;
    }
    let mut limit = vec![[0.0f32; 3]; verts.len()];
    primvar.limit(&verts, &mut limit);

    // Topology of the last level, back in the importer's array shapes.
    let last = refiner.level(level);
    let n_faces = last.num_faces();
    let mut out_counts = Vec::with_capacity(n_faces);
    let mut out_indices = Vec::with_capacity(last.num_face_vertices_total());
    for f in 0..n_faces {
        let fv = last.face_vertices(f);
        out_counts.push(fv.len() as i32);
        out_indices.extend(fv.iter().map(|&v| v as i32));
    }

    let faces = want_uvs.then(|| {
        // Base-cage face per refined face: compose the one-step
        // child-to-parent maps from the last refinement down to level 0.
        let mut base_face: Vec<u32> = (0..n_faces as u32).collect();
        for l in (1..=level).rev() {
            let refinement = refiner.refinement(l);
            for f in &mut base_face {
                *f = refinement.child_face_parent_face(*f as usize);
            }
        }
        let base_face: Vec<Option<u32>> = base_face
            .into_iter()
            .map(|f| (counts[f as usize] == 4).then_some(f))
            .collect();

        // Sub-face corner UVs: refine the synthetic channel the same way the
        // positions were refined, then read each face's four values.
        let mut uvs = fvar_uvs.clone();
        for l in 1..=level {
            let mut refined = vec![[0.0f32; 2]; refiner.level(l).num_fvar_values(0)];
            primvar.interpolate_face_varying(l, 0, &uvs, &mut refined);
            uvs = refined;
        }
        let corner_uvs = (0..n_faces)
            .map(|f| {
                let fv = last.face_fvar_values(f, 0);
                debug_assert_eq!(fv.len(), 4, "quad-split schemes only refine into quads");
                [
                    uvs[fv[0] as usize],
                    uvs[fv[1] as usize],
                    uvs[fv[2] as usize],
                    uvs[fv[3] as usize],
                ]
            })
            .collect();
        SubdivFaces {
            base_face,
            corner_uvs,
        }
    });

    let uvs = req.uvs.as_ref().map(|c| match chart_channel {
        Some(ch) => {
            // Face-varying: refine the values level by level, snap them to
            // the limit, then read each refined face's entries.
            let mut values = c.values.to_vec();
            for l in 1..=level {
                let mut refined = vec![[0.0f32; 2]; refiner.level(l).num_fvar_values(ch)];
                primvar.interpolate_face_varying(l, ch, &values, &mut refined);
                values = refined;
            }
            let mut limit_uvs = vec![[0.0f32; 2]; values.len()];
            primvar.limit_face_varying(ch, &values, &mut limit_uvs);
            let mut fv_indices = Vec::with_capacity(out_indices.len());
            for f in 0..n_faces {
                fv_indices.extend(last.face_fvar_values(f, ch).iter().map(|&v| v as i32));
            }
            RefinedUvs {
                values: limit_uvs,
                indices: Some(fv_indices),
                face_varying: true,
            }
        }
        None => {
            // Vertex: one value per point, refined and limited exactly like
            // the positions.
            let mut values: Vec<[f32; 2]> = (0..points.len())
                .map(|p| c.values[c.value_index(p)])
                .collect();
            for l in 1..=level {
                let mut refined = vec![[0.0f32; 2]; refiner.level(l).num_vertices()];
                primvar.interpolate(l, &values, &mut refined);
                values = refined;
            }
            let mut limit_uvs = vec![[0.0f32; 2]; values.len()];
            primvar.limit(&values, &mut limit_uvs);
            RefinedUvs {
                values: limit_uvs,
                indices: None,
                face_varying: false,
            }
        }
    });

    // Everything that needed the refiner is extracted; drop it — every
    // level's topology, a ×4/3 of the last — before the result's own copies
    // of the last level are made, so the two never coexist. This is the
    // third of the transient the kernel design record costed. (`primvar`
    // only borrows it; its last use is above.)
    drop(refiner);

    let normals = smooth_normals(&limit, &out_counts, &out_indices);
    let points: Vec<Vec3f> = limit
        .into_iter()
        .map(|p| Vec3f::from([p[0], p[1], p[2]]))
        .collect();

    Ok(SubdividedMesh {
        points,
        counts: out_counts,
        indices: out_indices,
        normals,
        faces,
        uvs,
    })
}
