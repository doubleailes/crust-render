//! Scalar displacement, as a bound material defines it.
//!
//! Displacement is consumed **once, at import**, when a mesh is tessellated —
//! never per hit — so it is not part of the [`crate::Material`] contract,
//! whose every method and every `crust-core/tests/resolve.rs` pin is about
//! shading. Material resolution returns one of these *beside* the material,
//! and the importer's displacement pass (`scene/displace.rs`) evaluates it at
//! each unique vertex of the tessellation.
//!
//! It is a closed enum rather than a trait object because the set of sources
//! is the set of authoring paths crust reads, and each one samples a
//! different chart:
//!
//! - [`DisplacementValue::Constant`] — `UsdPreviewSurface.inputs:displacement`
//!   authored as a value.
//! - [`DisplacementValue::Uv`] — the same input through a `UsdUVTexture`,
//!   sampled by the very [`UvInput`] code a shading input uses, so the two
//!   cannot drift on channel, `scale`, `bias` or wrap.
//! - [`DisplacementValue::Ptex`] — RenderMan's `PxrDisplace` over raw Ptex
//!   maps (one, or the product a `PxrBlend` multiply forms), addressed by
//!   cage face.
//! - [`DisplacementValue::Field`] — a program over the vertex — MaterialX's
//!   `displacement` node, compiled to a one-root program.
//!
//! The value is a distance in the mesh's **local** units, applied along the
//! vertex normal before the placement transform.

use crate::PtexRef;
use crate::material::preview_surface::UvInput;
use glam::Vec3A;
use std::sync::Arc;

/// Where a vertex is, in every chart a displacement may read.
///
/// Built once per unique vertex from its *owner* corner — the first face
/// corner that references it — so a vertex shared across a UV seam or a Ptex
/// face boundary reads one value, and the displaced mesh stays watertight.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct VertexCtx {
    /// Texture coordinate at the owner corner, `None` without a chart.
    pub uv: Option<[f32; 2]>,
    /// Footprint diameter in UV units: the tessellation's spacing around the
    /// vertex, so a coarse dicing reads a coarse mip level.
    pub uv_width: f32,
    /// Ptex face and the vertex's coordinate in that face's unit square.
    pub ptex: Option<(u32, [f32; 2])>,
    /// Footprint diameter in face-local units (`1.0` is the whole face).
    pub ptex_width: f32,
    /// Local-space position before displacement.
    pub position: Vec3A,
    /// Unit local-space normal before displacement — the offset direction.
    pub normal: Vec3A,
}

/// A scalar function of a vertex — what a compiled MaterialX displacement
/// graph is to the displacement pass. A trait so `crust-mtlx`'s program and
/// its JIT stay in `materialx.rs`, and so a test can hand in a closure.
pub trait VertexField: Send + Sync {
    fn eval(&self, ctx: &VertexCtx) -> f32;
}

impl<F: Fn(&VertexCtx) -> f32 + Send + Sync> VertexField for F {
    fn eval(&self, ctx: &VertexCtx) -> f32 {
        self(ctx)
    }
}

/// The source of a displacement's value.
#[derive(Clone)]
pub enum DisplacementValue {
    /// The same offset everywhere.
    Constant(f32),
    /// A `UsdUVTexture`: the connected output's channel, after its `scale`
    /// and `bias`.
    Uv(UvInput),
    /// Per-face textures, read raw and multiplied together (one map, or a
    /// `PxrBlend` multiply of several over the same faces), remapped, times
    /// `scale` (`PxrDisplace.dispAmount`). Never empty.
    Ptex {
        maps: Vec<PtexRef>,
        remap: DispRemap,
        scale: f32,
    },
    /// A program over the vertex, times `scale` (MaterialX's `displacement`
    /// node: its `displacement` input times its `scale`).
    Field {
        field: Arc<dyn VertexField>,
        scale: f32,
    },
}

/// RenderMan's `PxrDispTransform` remap of a raw map value `s`, as the Moana
/// island authors it between its displacement Ptex and `PxrDisplace`.
///
/// The three `dispRemapMode`s, with RenderMan's defaults (`dispCenter` 0.5,
/// `dispDepth` and `dispHeight` 1):
/// - `None` (0): `s`;
/// - `Centered` (1): `s − center`, so a map stored around mid-grey moves both
///   ways;
/// - `DepthHeight` (2): `s` below `center` maps linearly onto `[−depth, 0]`,
///   above it onto `[0, height]`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DispRemap {
    None,
    Centered {
        center: f32,
    },
    DepthHeight {
        center: f32,
        depth: f32,
        height: f32,
    },
}

impl DispRemap {
    /// The remapped value.
    pub fn apply(self, s: f32) -> f32 {
        match self {
            DispRemap::None => s,
            DispRemap::Centered { center } => s - center,
            DispRemap::DepthHeight {
                center,
                depth,
                height,
            } => {
                if s < center {
                    if center > 0.0 {
                        depth * (s - center) / center
                    } else {
                        0.0
                    }
                } else if center < 1.0 {
                    height * (s - center) / (1.0 - center)
                } else {
                    0.0
                }
            }
        }
    }

    /// The largest `|value|` a map in `[0, 1]` can remap to — what bounds an
    /// 8-bit map's offset.
    pub fn unit_extent(self) -> f32 {
        [0.0f32, 1.0]
            .into_iter()
            .map(|s| self.apply(s).abs())
            .fold(0.0, f32::max)
    }
}

/// A material's scalar displacement and what is known of its extent.
#[derive(Clone)]
pub struct Displacement {
    pub value: DisplacementValue,
    /// The largest `|offset|` the displacement can produce, in local units:
    /// exact for a constant, otherwise `crust:displacementBound` when
    /// authored. Adaptive dicing grows its culling boxes by it; `None` turns
    /// the frustum test off for the mesh, so displaced geometry pushed into
    /// view is never under-diced.
    pub bound: Option<f32>,
}

impl std::fmt::Debug for Displacement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match &self.value {
            DisplacementValue::Constant(c) => format!("constant {c}"),
            DisplacementValue::Uv(_) => "UV texture".to_string(),
            DisplacementValue::Ptex { maps, remap, scale } => {
                format!("{maps:?} {remap:?} x {scale}")
            }
            DisplacementValue::Field { scale, .. } => format!("program x {scale}"),
        };
        write!(f, "Displacement({kind}, bound {:?})", self.bound)
    }
}

impl Displacement {
    /// A displacement with the bound that follows from its value alone:
    /// `|c|` for a constant, unknown otherwise.
    pub fn new(value: DisplacementValue) -> Self {
        let bound = match value {
            DisplacementValue::Constant(c) => Some(c.abs()),
            _ => None,
        };
        Displacement { value, bound }
    }

    /// With an authored `crust:displacementBound`. A constant keeps its exact
    /// bound: an authored one can only be looser.
    pub fn with_authored_bound(mut self, bound: Option<f32>) -> Self {
        if !matches!(self.value, DisplacementValue::Constant(_))
            && let Some(b) = bound.filter(|b| b.is_finite())
        {
            self.bound = Some(b.abs());
        }
        self
    }

    /// Whether evaluating this needs each vertex's texture coordinate.
    pub fn needs_uv(&self) -> bool {
        matches!(
            self.value,
            DisplacementValue::Uv(_) | DisplacementValue::Field { .. }
        )
    }

    /// Whether evaluating this needs each vertex's Ptex face coordinate.
    pub fn needs_ptex(&self) -> bool {
        matches!(self.value, DisplacementValue::Ptex { .. })
    }

    /// Whether the value is the same at every vertex.
    pub fn is_constant(&self) -> bool {
        matches!(self.value, DisplacementValue::Constant(_))
    }

    /// The offset at one vertex, in local units. A non-finite value (a NaN
    /// texel, a division by zero in a graph) is no offset rather than a
    /// vertex sent to infinity.
    pub fn eval(&self, ctx: &VertexCtx) -> f32 {
        let d = match &self.value {
            DisplacementValue::Constant(c) => *c,
            DisplacementValue::Uv(input) => {
                let uv = ctx.uv.map(|[u, v]| (u, v));
                input.scalar(input.sample_at(uv, ctx.uv_width))
            }
            DisplacementValue::Ptex { maps, remap, scale } => match ctx.ptex {
                Some((face, [u, v])) => {
                    let s: f32 = maps
                        .iter()
                        .map(|m| m.eval(face, u, v, ctx.ptex_width).x)
                        .product();
                    remap.apply(s) * scale
                }
                None => 0.0,
            },
            DisplacementValue::Field { field, scale } => field.eval(ctx) * scale,
        };
        if d.is_finite() { d } else { 0.0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::material::preview_surface::{TexOutput, Wrap};
    use crate::{PtexTexture, Texture2D, TextureRef};

    /// `u + 10 v` in every channel, recording nothing — enough to tell which
    /// coordinate and channel was read.
    struct Ramp;
    impl Texture2D for Ramp {
        fn eval(&self, u: f32, v: f32, _width: f32) -> [f32; 4] {
            [u + 10.0 * v, 2.0 * (u + 10.0 * v), 0.25, 1.0]
        }
    }

    /// Face id plus `u`, so the face and coordinate are both visible.
    struct FaceRamp;
    impl PtexTexture for FaceRamp {
        fn eval(&self, face: u32, u: f32, _v: f32, _width: f32) -> Vec3A {
            Vec3A::splat(face as f32 + u)
        }
        fn num_faces(&self) -> usize {
            4
        }
    }

    fn ctx() -> VertexCtx {
        VertexCtx {
            uv: Some([0.25, 0.5]),
            ptex: Some((2, [0.5, 0.75])),
            position: Vec3A::new(1.0, 2.0, 3.0),
            normal: Vec3A::Y,
            ..VertexCtx::default()
        }
    }

    #[test]
    fn a_constant_is_its_value_and_its_bound() {
        let d = Displacement::new(DisplacementValue::Constant(-0.1));
        assert_eq!(d.eval(&ctx()), -0.1);
        assert_eq!(d.bound, Some(0.1));
        // An authored bound cannot loosen an exact one.
        assert_eq!(d.with_authored_bound(Some(5.0)).bound, Some(0.1));
    }

    #[test]
    fn a_uv_texture_reads_its_channel_scale_and_bias() {
        let input = UvInput {
            tex: Some(TextureRef(Arc::new(Ramp))),
            output: TexOutput::G,
            scale: [1.0, 0.2, 1.0, 1.0],
            bias: [0.0, -0.1, 0.0, 0.0],
            fallback: [0.0; 4],
            wrap: [Wrap::Repeat; 2],
            tiled: false,
        };
        let d = Displacement::new(DisplacementValue::Uv(input));
        // g = 2 (0.25 + 10 * 0.5) = 10.5; 0.2 * 10.5 - 0.1 = 2.0
        assert_eq!(d.eval(&ctx()), 0.2 * 10.5 - 0.1);
        assert_eq!(d.bound, None);
        assert_eq!(d.with_authored_bound(Some(-3.0)).bound, Some(3.0));
    }

    #[test]
    fn a_ptex_map_reads_the_owner_face_times_its_amount() {
        let d = Displacement::new(DisplacementValue::Ptex {
            maps: vec![PtexRef(Arc::new(FaceRamp))],
            remap: DispRemap::None,
            scale: 2.0,
        });
        assert_eq!(d.eval(&ctx()), 2.0 * 2.5);
        // No face, no offset.
        let none = VertexCtx {
            ptex: None,
            ..ctx()
        };
        assert_eq!(d.eval(&none), 0.0);
    }

    #[test]
    fn the_renderman_remaps_follow_pxr_disp_transform() {
        assert_eq!(DispRemap::None.apply(0.25), 0.25);
        assert_eq!(DispRemap::Centered { center: 0.5 }.apply(0.25), -0.25);
        let dh = DispRemap::DepthHeight {
            center: 0.5,
            depth: 0.35,
            height: 0.7,
        };
        assert_eq!(dh.apply(0.5), 0.0);
        assert_eq!(dh.apply(0.0), -0.35);
        assert_eq!(dh.apply(1.0), 0.7);
        assert_eq!(dh.apply(0.25), -0.175);
        assert_eq!(dh.unit_extent(), 0.7);
        let d = Displacement::new(DisplacementValue::Ptex {
            maps: vec![PtexRef(Arc::new(FaceRamp))],
            remap: DispRemap::Centered { center: 2.0 },
            scale: 2.0,
        });
        // face 2, u = 0.5: (2.5 - 2.0) * 2
        assert_eq!(d.eval(&ctx()), 1.0);
    }

    /// Several maps multiply, as a `PxrBlend` multiply of two Ptex does.
    #[test]
    fn ptex_maps_multiply() {
        let d = Displacement::new(DisplacementValue::Ptex {
            maps: vec![PtexRef(Arc::new(FaceRamp)), PtexRef(Arc::new(FaceRamp))],
            remap: DispRemap::None,
            scale: 2.0,
        });
        // face 2, u = 0.5: 2.5 · 2.5 · 2
        assert_eq!(d.eval(&ctx()), 12.5);
    }

    #[test]
    fn a_field_sees_the_vertex_and_is_scaled() {
        let d = Displacement::new(DisplacementValue::Field {
            field: Arc::new(|c: &VertexCtx| c.position.y + c.uv.unwrap()[0]),
            scale: 0.5,
        });
        assert_eq!(d.eval(&ctx()), 0.5 * 2.25);
    }

    #[test]
    fn a_non_finite_value_is_no_offset() {
        let d = Displacement::new(DisplacementValue::Field {
            field: Arc::new(|_: &VertexCtx| f32::NAN),
            scale: 1.0,
        });
        assert_eq!(d.eval(&ctx()), 0.0);
    }
}
