//! AOVs (arbitrary output variables): what a stage's `RenderProduct`s ask
//! for.
//!
//! The request comes from USD's `UsdRender` schema — settings → `products` →
//! `orderedVars` — resolved at import (`usd_import/settings.rs`). This module
//! owns the *vocabulary*: which `sourceName`s crust honours, what each one
//! means (space, units, clear value) and how it is accumulated over a pixel's
//! samples. The schema standardises the plumbing but not the names; the
//! canonical names are Hydra's `HdAovTokens` where Hydra has one, with
//! RenderMan, Arnold, Karma and Blender names as aliases. See
//! `openspec/specs/aovs/`.
//!
//! The film that answers a request is `AovFilm` (`tracer/film.rs`), filled
//! by the renderer: the importer builds requests and never needs it.

/// What a RenderVar computes: one row of the canonical table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AovSource {
    /// The beauty, exactly the image written when no product is authored.
    Color,
    /// Filtered geometric coverage of the primary ray.
    Alpha,
    /// Camera-space depth: distance from the camera plane along the view
    /// axis, in scene units. Not Hydra/Storm's clip-space depth.
    Depth,
    /// Euclidean distance from the camera position to the first hit.
    Distance,
    /// World-space first-hit position.
    P,
    /// Camera-space first-hit position (USD camera: −Z forward, +Y up).
    Peye,
    /// World-space shading normal, facing the camera ray.
    Normal,
    /// The shading normal in camera space.
    Neye,
    /// The first hit's `st`, as the material sees it.
    St,
    /// The samples the pixel took.
    SampleCount,
    /// Variance of the pixel's luminance mean — adaptive sampling's
    /// stopping statistic.
    Variance,
    /// The light a light path expression selects (`sourceType = "lpe"`;
    /// the expression is the var's [`AovVar::expression`]).
    Lpe,
    /// The albedo at the first non-delta hit, for denoisers.
    Albedo,
    /// The colour of the diffuse lobes of the surface the camera ray hits —
    /// what the raw light AOVs divide by.
    DiffuseFilter,
    /// The first hit's forward 2D screen-space motion over the shutter, in
    /// pixels: `u` right, `v` up (Nuke's conventions, Arnold's raw
    /// `motionvector`). Zero on static geometry, in volumes and on escapes.
    MotionVector,
}

/// `(name, source)` for every canonical name and alias a `raw` var may ask
/// for. Matched case-sensitively, as Arnold and RenderMan do.
const RAW_NAMES: &[(&str, AovSource)] = &[
    ("color", AovSource::Color),
    ("Ci", AovSource::Color),
    ("C", AovSource::Color),
    ("RGBA", AovSource::Color),
    ("beauty", AovSource::Color),
    ("HdrColor", AovSource::Color),
    ("Combined", AovSource::Color),
    ("alpha", AovSource::Alpha),
    ("a", AovSource::Alpha),
    ("A", AovSource::Alpha),
    ("opacity", AovSource::Alpha),
    ("depth", AovSource::Depth),
    ("cameraDepth", AovSource::Depth),
    ("z", AovSource::Depth),
    ("Z", AovSource::Depth),
    ("Depth", AovSource::Depth),
    ("distance", AovSource::Distance),
    ("DistanceToCameraSD", AovSource::Distance),
    ("P", AovSource::P),
    ("Pworld", AovSource::P),
    ("__Pworld", AovSource::P),
    ("Position", AovSource::P),
    ("Peye", AovSource::Peye),
    ("Pcam", AovSource::Peye),
    ("__Pcam", AovSource::Peye),
    ("normal", AovSource::Normal),
    ("N", AovSource::Normal),
    ("Nworld", AovSource::Normal),
    ("__Nworld", AovSource::Normal),
    ("Normal", AovSource::Normal),
    ("Neye", AovSource::Neye),
    ("Nn", AovSource::Neye),
    ("primvars:st", AovSource::St),
    ("st", AovSource::St),
    ("uv", AovSource::St),
    ("UV", AovSource::St),
    ("sampleCount", AovSource::SampleCount),
    ("__sampleCount", AovSource::SampleCount),
    ("variance", AovSource::Variance),
    ("crust:variance", AovSource::Variance),
    ("albedo", AovSource::Albedo),
    ("DiffuseAlbedoSD", AovSource::Albedo),
    ("diffuse_albedo", AovSource::DiffuseFilter),
    ("DiffuseFilter", AovSource::DiffuseFilter),
    ("diffuseFilter", AovSource::DiffuseFilter),
    // No aliases: the other renderers' 2D motion names (`velocity`,
    // `Vector`, `motionFore`) each encode the vector their own way, and an
    // alias would be a plausible but wrong channel.
    ("motionvector", AovSource::MotionVector),
];

/// The raw light sources: a name, and the light path expression whose
/// paths it divides by the diffuse filter. V-Ray's names are aliases.
const RAW_LIGHT: &[(&str, &str)] = &[
    ("rawLight", "C<RD>[LO]"),
    ("RawLighting", "C<RD>[LO]"),
    ("rawLighting", "C<RD>[LO]"),
    ("rawGI", "C<RD>.+[LO]"),
    ("RawGI", "C<RD>.+[LO]"),
    ("rawTotalLight", "C<RD>.*[LO]"),
    ("RawTotalLighting", "C<RD>.*[LO]"),
];

/// The expression a raw light source (`rawLight` …) stands for, if `name` is
/// one: its paths, divided per sample by the diffuse filter.
pub fn raw_light_expression(name: &str) -> Option<&'static str> {
    RAW_LIGHT.iter().find(|(n, _)| *n == name).map(|(_, e)| *e)
}

/// Below this, a diffuse filter channel counts as black: a raw sample's
/// channel is 0 there rather than light divided by almost nothing.
pub const RAW_FILTER_FLOOR: f32 = 1e-4;

/// Names a later phase of the AOV work will define, refused today with a
/// reason that says so rather than "unknown".
const LATER_NAMES: &[&str] = &[
    "Ng",
    "primId",
    "id",
    "ID",
    "Object Index",
    "instanceId",
    "id2",
    "elementId",
    "faceindex",
    "crypto_object",
    "crypto_material",
    "crypto_asset",
    "CryptoObject",
    "CryptoMaterial",
    "CryptoAsset",
];

/// How a source is laid out as channels — which component suffixes it gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelKind {
    /// `R`, `G`, `B` (and `A`): colour-managed.
    Color,
    /// `X`, `Y`, `Z`: data, never colour-managed.
    Vector,
    /// `U`, `V`.
    Uv,
    /// `u`, `v`, lowercase: Nuke's names for its `forward` / `backward`
    /// layers' channels, so a var named `forward` lands on that layer.
    Motion,
    /// One channel named after the layer.
    Scalar,
}

impl AovSource {
    /// The source a `raw` `sourceName` names, through its aliases.
    pub fn from_raw(name: &str) -> Option<Self> {
        RAW_NAMES.iter().find(|(n, _)| *n == name).map(|(_, s)| *s)
    }

    /// Whether `name` is a source a later phase of the AOV work defines.
    pub fn is_planned(name: &str) -> bool {
        LATER_NAMES.contains(&name)
    }

    /// The canonical (Hydra) name.
    pub fn name(self) -> &'static str {
        match self {
            AovSource::Color => "color",
            AovSource::Alpha => "alpha",
            AovSource::Depth => "depth",
            AovSource::Distance => "distance",
            AovSource::P => "P",
            AovSource::Peye => "Peye",
            AovSource::Normal => "normal",
            AovSource::Neye => "Neye",
            AovSource::St => "primvars:st",
            AovSource::SampleCount => "sampleCount",
            AovSource::Variance => "variance",
            AovSource::Lpe => "lpe",
            AovSource::Albedo => "albedo",
            AovSource::DiffuseFilter => "diffuse_albedo",
            AovSource::MotionVector => "motionvector",
        }
    }

    /// How many components the source has. The beauty's alpha is a
    /// separate source, so `color` is 3 even when written as `color4f`.
    pub fn components(self) -> usize {
        match self {
            AovSource::Color
            | AovSource::P
            | AovSource::Peye
            | AovSource::Normal
            | AovSource::Neye
            | AovSource::Lpe
            | AovSource::Albedo
            | AovSource::DiffuseFilter => 3,
            AovSource::St | AovSource::MotionVector => 2,
            AovSource::Alpha
            | AovSource::Depth
            | AovSource::Distance
            | AovSource::SampleCount
            | AovSource::Variance => 1,
        }
    }

    /// The power of the render camera's exposure scale this source's values
    /// are multiplied by: 1 for radiance (the beauty and light path
    /// expressions), 2 for the variance of a luminance mean, 0 for everything
    /// that is not light — geometry, reflectances, counts.
    pub fn exposure_power(self) -> i32 {
        match self {
            AovSource::Color | AovSource::Lpe => 1,
            AovSource::Variance => 2,
            AovSource::Alpha
            | AovSource::Depth
            | AovSource::Distance
            | AovSource::P
            | AovSource::Peye
            | AovSource::Normal
            | AovSource::Neye
            | AovSource::St
            | AovSource::SampleCount
            | AovSource::Albedo
            | AovSource::DiffuseFilter
            | AovSource::MotionVector => 0,
        }
    }

    pub fn channel_kind(self) -> ChannelKind {
        match self {
            AovSource::Color | AovSource::Lpe | AovSource::Albedo | AovSource::DiffuseFilter => {
                ChannelKind::Color
            }
            AovSource::P | AovSource::Peye | AovSource::Normal | AovSource::Neye => {
                ChannelKind::Vector
            }
            AovSource::St => ChannelKind::Uv,
            AovSource::MotionVector => ChannelKind::Motion,
            AovSource::Alpha
            | AovSource::Depth
            | AovSource::Distance
            | AovSource::SampleCount
            | AovSource::Variance => ChannelKind::Scalar,
        }
    }

    /// The accumulation a var gets unless it authors one: data that must
    /// never be blended across an edge is closest, the rest filtered. A
    /// motion vector blended at an edge would have VectorBlur smear the
    /// foreground into the background (Arnold users pair `motionvector`
    /// with `closest_filter` for the same reason).
    pub fn default_accumulation(self) -> Accumulation {
        match self {
            AovSource::Depth
            | AovSource::Distance
            | AovSource::P
            | AovSource::Peye
            | AovSource::MotionVector => Accumulation::Closest,
            _ => Accumulation::Filtered,
        }
    }

    /// Whether an authored accumulation mode applies to this source at all.
    /// The beauty, `sampleCount` and `variance` are per-pixel quantities the
    /// film already has; they have no per-sample value to accumulate.
    pub fn accepts_accumulation(self) -> bool {
        !matches!(
            self,
            AovSource::Color | AovSource::SampleCount | AovSource::Variance
        )
    }

    /// What a pixel holds where no sample saw the quantity (the primary ray
    /// escaped): `+inf` for the two distances, so a `zmin` composite treats
    /// the background as infinitely far, and 0 for everything else.
    pub fn default_clear(self) -> f32 {
        match self {
            AovSource::Depth | AovSource::Distance => f32::INFINITY,
            _ => 0.0,
        }
    }

    /// Whether the film needs a per-sample accumulator for this source. The
    /// beauty is the `Buffer`; the sample count and variance are read off
    /// the pixel's own state.
    pub fn needs_slot(self) -> bool {
        !matches!(
            self,
            AovSource::Color | AovSource::SampleCount | AovSource::Variance
        )
    }
}

/// How an AOV combines a pixel's samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Accumulation {
    /// `Σ wᵢ·vᵢ / Σ wᵢ` with the beauty's own pixel-filter weights.
    Filtered,
    /// The value of the sample nearest the camera among those inside the
    /// pixel's own box (RenderMan `zmin`, Arnold `closest_filter`).
    Closest,
}

/// The sample type a channel is written with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Precision {
    Half,
    Float,
    /// 32-bit unsigned; a negative value is written as its two's-complement
    /// bit pattern (`-1` → `0xFFFFFFFF`).
    Uint,
}

/// One accepted RenderVar: a layer of channels in its product's EXR.
#[derive(Debug, Clone, PartialEq)]
pub struct AovVar {
    /// The RenderVar prim, for messages.
    pub prim_path: String,
    /// The layer name: `driver:parameters:aov:name`, else the prim name.
    pub name: String,
    /// An authored `driver:parameters:aov:channel_prefix`, which replaces the
    /// layer prefix of every channel.
    pub channel_prefix: Option<String>,
    pub source: AovSource,
    /// Components as authored by the type: `components()` of the source,
    /// or 4 for a `color4*` beauty (which adds alpha).
    pub components: usize,
    pub precision: Precision,
    pub accumulation: Accumulation,
    pub clear: f32,
    /// The light path expression of an [`AovSource::Lpe`] var, without any
    /// `lpe:` prefix; `None` for every other source.
    pub expression: Option<String>,
    /// A raw light AOV: an [`AovSource::Lpe`] var whose value is divided,
    /// per camera sample, by that sample's diffuse filter (`rawLight` …, or
    /// `crust:aov:raw`). Its expression starts with a diffuse reflection.
    pub raw: bool,
    /// A light path expression's variance (`crust:aov:variance`): an
    /// [`AovSource::Lpe`] var, filtered, whose one scalar channel is the
    /// per-pixel variance of the expression's luminance mean — the
    /// `variance` source's estimator over the expression's (raw, if `raw`)
    /// share of each sample. `components` is 1.
    pub variance: bool,
}

impl AovVar {
    /// Whether this var adds the beauty's alpha as a fourth channel: a
    /// `color4*` beauty or light path expression.
    pub fn with_alpha(&self) -> bool {
        matches!(self.source, AovSource::Color | AovSource::Lpe) && self.components == 4
    }

    /// How the var's channels are laid out: its source's, except that an
    /// expression's variance is one scalar.
    pub fn channel_kind(&self) -> ChannelKind {
        if self.is_lpe_variance() {
            ChannelKind::Scalar
        } else {
            self.source.channel_kind()
        }
    }

    pub fn is_lpe_variance(&self) -> bool {
        self.variance && self.source == AovSource::Lpe
    }
}

/// One accepted `raster` RenderProduct.
#[derive(Debug, Clone, PartialEq)]
pub struct AovProduct {
    /// The RenderProduct prim, for messages.
    pub prim_path: String,
    /// `productName` at the render's time code: the file to write.
    pub name: String,
    /// Accepted vars, in `orderedVars` order.
    pub vars: Vec<AovVar>,
    /// Authored `driver:parameters:*` text values to copy into the header,
    /// as `(name without the prefix, value)`.
    pub attributes: Vec<(String, String)>,
}

impl AovProduct {
    /// The first var resolving to the beauty — the one written unprefixed
    /// and the source of the PNG preview.
    pub fn beauty(&self) -> Option<&AovVar> {
        self.vars.iter().find(|v| v.source == AovSource::Color)
    }
}

/// Every product a render writes. Empty when the stage authors none, which
/// means "write the single beauty EXR".
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AovRequest {
    pub products: Vec<AovProduct>,
}

impl AovRequest {
    /// Whether any var needs more than the beauty buffer — that is, whether
    /// the render must take the AOV path at all.
    pub fn needs_film(&self) -> bool {
        self.vars()
            .any(|v| v.source != AovSource::Color || v.with_alpha())
    }

    pub fn vars(&self) -> impl Iterator<Item = &AovVar> {
        self.products.iter().flat_map(|p| &p.vars)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_alias_resolves_and_canonical_names_round_trip() {
        for (name, source) in RAW_NAMES {
            assert_eq!(AovSource::from_raw(name), Some(*source), "{name}");
            assert_eq!(AovSource::from_raw(source.name()), Some(*source));
        }
        assert_eq!(AovSource::from_raw("diffuse_direct"), None);
        assert_eq!(AovSource::from_raw("Z"), Some(AovSource::Depth));
        // Case-sensitive: `n` is no alias.
        assert_eq!(AovSource::from_raw("n"), None);
    }
}
