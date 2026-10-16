//! The seam the engine decodes assets through: [`AssetLoader`], which the
//! host implements (`crust_assets::FileAssets`), and [`NoAssets`], the host
//! that decodes nothing.

use crate::environment::EnvironmentMap;
use crate::warning;

/// How the engine asks its host to decode an image.
///
/// The seam exists so `crust-core` stays free of image-format
/// dependencies — the CLI already links `exr` and `image`, so decoding
/// belongs there. It is also the natural place to grow general texture
/// support.
pub trait AssetLoader: Send + Sync {
    /// Decodes a lat-long environment map. `path` has already been resolved
    /// against the USD layer's directory. `None` — for an unreadable file,
    /// an unsupported format, or a host that does not decode at all — is
    /// not an error: the caller falls back.
    ///
    /// `space` is the file's colour space and the working space to bring it
    /// to: usually `auto` (an 8-bit image is sRGB, a float one is already in
    /// the working space), or what the texture attribute's `colorSpace`
    /// metadatum names. The map comes back as linear light in the working
    /// space.
    fn load_environment(
        &self,
        path: &std::path::Path,
        space: crate::ColorSpace,
    ) -> Option<EnvironmentMap>;

    /// Opens a UV-addressed texture and returns something that can sample it.
    ///
    /// `path` has already been resolved against the authoring layer's
    /// directory and may still contain a tile token — **`<UDIM>`** or
    /// **`<UVTILE>`**, MaterialX's two spellings of the same grid. Expanding
    /// either into the tile set on disk is the host's job, since which tiles
    /// exist is a filesystem question and the cheapest place to decide how
    /// many of them to hold in memory. `space` says how to decode the stored values — the
    /// file does not say, and getting it wrong is silent (see
    /// [`crate::ColorSpace`]).
    ///
    /// Like [`Self::load_ptex`], the host keeps the pixels and hands back a
    /// sampler. `None` means the material falls back to the constant value
    /// authored beside the texture in the MaterialX graph. Defaulted, like
    /// `load_ptex`, so hosts that decode neither need no extra ceremony.
    fn load_texture(
        &self,
        path: &std::path::Path,
        space: crate::ColorSpace,
    ) -> Option<std::sync::Arc<dyn crate::Texture2D>> {
        let _ = space;
        warning!(
            AssetUnsupportedByHost,
            "Asset loader does not decode UV textures: {} ignored — the input \
             falls back to its constant value.",
            path.display()
        );
        None
    }

    /// Opens a Ptex file and returns something that can sample it.
    ///
    /// Unlike [`Self::load_environment`], the host keeps ownership of the
    /// pixels and hands back a sampler — see [`crate::PtexTexture`] for why
    /// per-face textures cannot cross this seam as a pixel buffer. `path` has
    /// already been resolved against the USD layer's directory. `None` means
    /// the surface falls back to its constant `baseColor`, which is authored
    /// alongside the texture in every Ptex material the Moana island ships,
    /// so the fallback is a plausible flat colour rather than a black hole.
    ///
    /// `space` says how to decode the stored samples, exactly as for
    /// [`Self::load_texture`]: a colour map is display-encoded
    /// ([`crate::ColorSpace::GAMMA22`], the island's convention) while a
    /// displacement map is data ([`crate::ColorSpace::RAW`]). The file does
    /// not say which it is, and decoding a height map by 2.2 would bend every
    /// offset toward zero.
    ///
    /// Defaulted to `None` so a host that only cares about environment maps —
    /// and [`NoAssets`] — needs no extra ceremony.
    fn load_ptex(
        &self,
        path: &std::path::Path,
        space: crate::ColorSpace,
    ) -> Option<std::sync::Arc<dyn crate::PtexTexture>> {
        let _ = space;
        warning!(
            AssetUnsupportedByHost,
            "Asset loader does not decode Ptex: {} ignored — the surface falls \
             back to its constant baseColor.",
            path.display()
        );
        None
    }

    /// Decodes a light's colour map (`RectLight`'s `inputs:texture:file`) to
    /// linear float RGB — the decode [`Self::load_environment`] does, without
    /// the lat-long importance sampling. `None` means the light emits its
    /// uniform colour.
    ///
    /// `space` is as for [`Self::load_environment`].
    fn load_light_texture(
        &self,
        path: &std::path::Path,
        space: crate::ColorSpace,
    ) -> Option<std::sync::Arc<crate::LightTexture>> {
        let _ = space;
        warning!(
            AssetUnsupportedByHost,
            "Asset loader does not decode light textures: {} ignored — the light \
             emits its uniform colour.",
            path.display()
        );
        None
    }

    /// Decodes an IES photometric profile (`inputs:shaping:ies:file`).
    ///
    /// `path` is resolved as for the other loaders. `None` drops the IES term
    /// from the light's shaping — the rest of it (focus, cone) still applies —
    /// so the light renders unshaped by the profile rather than not at all.
    fn load_ies(&self, path: &std::path::Path) -> Option<std::sync::Arc<crate::IesProfile>> {
        warning!(
            AssetUnsupportedByHost,
            "Asset loader does not decode IES profiles: {} ignored — the light \
             renders without it.",
            path.display()
        );
        None
    }
}

/// The default host: decodes nothing. `Scene::from_usd` uses it, so a
/// caller that does not care about textures needs no extra ceremony.
pub struct NoAssets;

impl AssetLoader for NoAssets {
    fn load_environment(
        &self,
        path: &std::path::Path,
        _space: crate::ColorSpace,
    ) -> Option<EnvironmentMap> {
        warning!(
            AssetUnsupportedByHost,
            "No asset loader: environment map {} ignored — the dome falls back \
             to its uniform colour. Use Scene::from_usd_with_assets to supply one.",
            path.display()
        );
        None
    }
}
