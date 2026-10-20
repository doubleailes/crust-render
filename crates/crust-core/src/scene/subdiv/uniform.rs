//! Uniform refinement: the cage refined to one level and snapped to the limit.

use opensubdiv_rs::far::{
    FVarChannelDescriptor, PrimvarRefiner, TopologyDescriptor, TopologyRefinerFactory,
    UniformOptions,
};
use opensubdiv_rs::sdc;
use openusd::gf::Vec3f;

use super::normals::smooth_normals;
use super::topology::{expand_crease_runs, ptex_fvar_channel, validate_cage, validate_corners};
use super::{
    Lanes, PrimvarChannel, RefinedChannel, SubdivError, SubdivFaces, SubdivRequest, SubdivScheme,
    SubdividedMesh,
};

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
    let smooths_corners =
        |c: &sdc::FVarLinearInterpolation| *c == sdc::FVarLinearInterpolation::None;
    if req.want_face_uvs
        && (req
            .uvs
            .is_some_and(|c| c.face_varying && smooths_corners(&c.linear))
            || req
                .extra
                .iter()
                .any(|c| c.face_varying && smooths_corners(&c.linear)))
    {
        let faces = subdivide(
            points,
            counts,
            indices,
            &SubdivRequest {
                uvs: None,
                extra: &[],
                want_base_face: false,
                ..*req
            },
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
    // Every channel to refine: the chart first, widened to four lanes, then
    // the extra primvars. One code path refines them all.
    let widened = req.uvs.as_ref().map(|c| c.widened());
    let mut channels_in: Vec<PrimvarChannel> = Vec::with_capacity(1 + req.extra.len());
    if let (Some(c), Some(values)) = (req.uvs, widened.as_deref()) {
        channels_in.push(PrimvarChannel {
            values,
            indices: c.indices,
            face_varying: c.face_varying,
            linear: c.linear,
        });
    }
    channels_in.extend_from_slice(req.extra);

    // The face-varying rule is one per refiner, not per channel — in USD it
    // is a property of the mesh, so every channel carries the same one; the
    // chart's wins. The synthetic Ptex channel must refine bilinearly, and
    // does under every rule but `none` (handled above) — each of its values
    // is private to one face, so every one of its edges is a face-varying
    // boundary, and its data is affine. Without a face-varying channel,
    // `All`.
    let first_fvar = channels_in.iter().find(|c| c.face_varying);
    let options = sdc::Options::default()
        .with_vtx_boundary_interpolation(req.boundary)
        .with_fvar_linear_interpolation(
            first_fvar.map_or(sdc::FVarLinearInterpolation::All, |c| c.linear),
        );

    // Loop cannot refine quads at all, and the Ptex channel only makes sense
    // for the quad-split schemes.
    let want_uvs = req.want_face_uvs && req.scheme != SubdivScheme::Loop;
    let (fvar_uvs, fvar_indices) = if want_uvs {
        ptex_fvar_channel(&counts_us)
    } else {
        (Vec::new(), Vec::new())
    };
    // A face-varying channel's per-face-vertex indices are its topology; its
    // values seed the refinement.
    let channel_indices: Vec<Vec<u32>> = channels_in
        .iter()
        .map(|c| {
            if c.face_varying {
                (0..indices.len())
                    .map(|fv| c.value_index(fv) as u32)
                    .collect()
            } else {
                Vec::new()
            }
        })
        .collect();
    let mut channels = Vec::with_capacity(1 + channels_in.len());
    if want_uvs {
        channels.push(FVarChannelDescriptor::new(fvar_uvs.len(), &fvar_indices));
    }
    // Each channel's refiner FVar channel, `None` for a vertex one.
    let fvar_channel: Vec<Option<usize>> = channels_in
        .iter()
        .zip(&channel_indices)
        .map(|(c, idx)| {
            c.face_varying.then(|| {
                channels.push(FVarChannelDescriptor::new(c.values.len(), idx));
                channels.len() - 1
            })
        })
        .collect();

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

    // Base-cage face per refined face: compose the one-step child-to-parent
    // maps from the last refinement down to level 0.
    let base_face: Option<Vec<u32>> = (want_uvs || req.want_base_face).then(|| {
        let mut base_face: Vec<u32> = (0..n_faces as u32).collect();
        for l in (1..=level).rev() {
            let refinement = refiner.refinement(l);
            for f in &mut base_face {
                *f = refinement.child_face_parent_face(*f as usize);
            }
        }
        base_face
    });

    let faces = want_uvs.then(|| {
        let base_face: Vec<Option<u32>> = base_face
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|&f| (counts[f as usize] == 4).then_some(f))
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

    let mut refined_channels = channels_in
        .iter()
        .zip(&fvar_channel)
        .map(|(c, ch)| match *ch {
            Some(ch) => {
                // Face-varying: refine the values level by level, snap
                // them to the limit, then read each refined face's
                // entries.
                let mut values = c.values.to_vec();
                for l in 1..=level {
                    let mut refined = vec![[0.0f32; 4]; refiner.level(l).num_fvar_values(ch)];
                    primvar.interpolate_face_varying(l, ch, &values, &mut refined);
                    values = refined;
                }
                let mut limit_values = vec![[0.0f32; 4]; values.len()];
                primvar.limit_face_varying(ch, &values, &mut limit_values);
                let mut fv_indices = Vec::with_capacity(out_indices.len());
                for f in 0..n_faces {
                    fv_indices.extend(last.face_fvar_values(f, ch).iter().map(|&v| v as i32));
                }
                RefinedChannel {
                    values: limit_values,
                    indices: Some(fv_indices),
                    face_varying: true,
                }
            }
            None => {
                // Vertex: one value per point, refined and limited
                // exactly like the positions.
                let mut values: Vec<Lanes> = (0..points.len())
                    .map(|p| c.values[c.value_index(p)])
                    .collect();
                for l in 1..=level {
                    let mut refined = vec![[0.0f32; 4]; refiner.level(l).num_vertices()];
                    primvar.interpolate(l, &values, &mut refined);
                    values = refined;
                }
                let mut limit_values = vec![[0.0f32; 4]; values.len()];
                primvar.limit(&values, &mut limit_values);
                RefinedChannel {
                    values: limit_values,
                    indices: None,
                    face_varying: false,
                }
            }
        })
        .collect::<Vec<_>>()
        .into_iter();
    let uvs = req.uvs.is_some().then(|| {
        refined_channels
            .next()
            .expect("the chart is channel 0")
            .narrowed()
    });
    let extra: Vec<RefinedChannel> = refined_channels.collect();

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
        extra,
        base_face: base_face.filter(|_| req.want_base_face),
    })
}
