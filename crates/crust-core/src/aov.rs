//! AOVs (arbitrary output variables): what a stage's `RenderProduct`s ask
//! for, and the film that answers them.
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
//! The film is [`AovFilm`]: full-frame planes beside the beauty
//! [`Buffer`](crate::Buffer), filled by [`Renderer::render_with_aovs`]
//! (crate::Renderer::render_with_aovs). AOVs only *observe* the camera
//! samples the beauty takes — they draw no random number and change no
//! weight — so the beauty is bit-identical with or without them.

use glam::{Vec3, Vec3A};

use crate::buffer::Buffer;
use crate::camera::CameraFrame;
pub use crate::identity::NameKey;
use crate::identity::{HitIdentity, NO_ID, NO_NAME, NameTable};
use crate::matte::{FilmMatte, MatteSample, SPILL, SPILL_NAME, UnitMatte};
use crate::region::PixelRect;
use crate::rt_world::World;

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
    /// A stable integer per prim: the MurmurHash3 of the path of the prim
    /// the hit geometry came from — the instance or instancer prim for
    /// instanced geometry, as light linking judges receivers.
    PrimId,
    /// The MurmurHash3 of the hit geometry's bound material's path.
    MaterialId,
    /// The MurmurHash3 of the path of the hit prim's asset: its nearest
    /// ancestor (or itself) of `kind` `component` or `assembly`.
    AssetId,
    /// The hit instance's index in its `PointInstancer`'s arrays.
    InstanceId,
    /// The authored (pre-triangulation) face index of the hit mesh.
    ElementId,
    /// A primvar's value at the first hit (`sourceType = "primvar"`; the
    /// primvar is the var's [`AovVar::primvar`]).
    Primvar,
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
    ("primId", AovSource::PrimId),
    ("id", AovSource::PrimId),
    ("ID", AovSource::PrimId),
    ("Object Index", AovSource::PrimId),
    // Crust's own: Hydra names no material or asset id.
    ("materialId", AovSource::MaterialId),
    ("assetId", AovSource::AssetId),
    ("instanceId", AovSource::InstanceId),
    ("id2", AovSource::InstanceId),
    ("elementId", AovSource::ElementId),
    ("faceindex", AovSource::ElementId),
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
const LATER_NAMES: &[&str] = &["Ng"];

/// Cryptomatte's layer names. Crust writes ID mattes as OpenEXRId instead
/// (a `deepRaster` product), so these are refused with a reason that says
/// so, never written.
const CRYPTOMATTE_NAMES: &[&str] = &[
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

    /// Whether `name` is one of Cryptomatte's layers, which crust does not
    /// write: its ID mattes are OpenEXRId.
    pub fn is_cryptomatte(name: &str) -> bool {
        CRYPTOMATTE_NAMES.contains(&name)
    }

    /// Whether the source is an identity: an integer naming what the first
    /// hit lies on, closest-sample and written as UINT. Its film slot holds
    /// the integer's bit pattern, which nothing ever does arithmetic on.
    pub fn is_identity(self) -> bool {
        matches!(
            self,
            AovSource::PrimId
                | AovSource::MaterialId
                | AovSource::AssetId
                | AovSource::InstanceId
                | AovSource::ElementId
        )
    }

    /// What the source's id names, for the three that hash a name — and
    /// that name an ID matte's ids.
    pub fn name_key(self) -> Option<NameKey> {
        match self {
            AovSource::PrimId => Some(NameKey::Prim),
            AovSource::MaterialId => Some(NameKey::Material),
            AovSource::AssetId => Some(NameKey::Asset),
            _ => None,
        }
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
            AovSource::PrimId => "primId",
            AovSource::MaterialId => "materialId",
            AovSource::AssetId => "assetId",
            AovSource::InstanceId => "instanceId",
            AovSource::ElementId => "elementId",
            AovSource::Primvar => "primvar",
        }
    }

    /// How many components the source has. The beauty's alpha is a
    /// separate source, so `color` is 3 even when written as `color4f`. A
    /// primvar has as many as its var's type ([`AovVar::components`]); 1
    /// stands for it here.
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
            | AovSource::Variance
            | AovSource::PrimId
            | AovSource::MaterialId
            | AovSource::AssetId
            | AovSource::InstanceId
            | AovSource::ElementId
            | AovSource::Primvar => 1,
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
            | AovSource::MotionVector
            | AovSource::PrimId
            | AovSource::MaterialId
            | AovSource::AssetId
            | AovSource::InstanceId
            | AovSource::ElementId
            | AovSource::Primvar => 0,
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
            | AovSource::Variance
            | AovSource::PrimId
            | AovSource::MaterialId
            | AovSource::AssetId
            | AovSource::InstanceId
            | AovSource::ElementId => ChannelKind::Scalar,
            // A primvar's layout is its var's type's ([`PrimvarSource`]):
            // scalar until a type says otherwise.
            AovSource::Primvar => ChannelKind::Scalar,
        }
    }

    /// The accumulation a var gets unless it authors one: data that must
    /// never be blended across an edge is closest, the rest filtered. A
    /// motion vector blended at an edge would have VectorBlur smear the
    /// foreground into the background (Arnold users pair `motionvector`
    /// with `closest_filter` for the same reason).
    ///
    /// An identity is closest too: a blend of two ids is no id at all.
    pub fn default_accumulation(self) -> Accumulation {
        match self {
            AovSource::Depth
            | AovSource::Distance
            | AovSource::P
            | AovSource::Peye
            | AovSource::MotionVector => Accumulation::Closest,
            s if s.is_identity() => Accumulation::Closest,
            _ => Accumulation::Filtered,
        }
    }

    /// Whether an authored accumulation mode applies to this source at all.
    /// The beauty, `sampleCount` and `variance` are per-pixel quantities the
    /// film already has; they have no per-sample value to accumulate. An
    /// identity is always closest: its slot holds an integer's bits, which a
    /// filter would average into a number that names nothing.
    pub fn accepts_accumulation(self) -> bool {
        !matches!(
            self,
            AovSource::Color | AovSource::SampleCount | AovSource::Variance
        ) && !self.is_identity()
    }

    /// What a pixel holds where no sample saw the quantity (the primary ray
    /// escaped): `+inf` for the two distances, so a `zmin` composite treats
    /// the background as infinitely far, `-1` ("no id", written as
    /// `0xFFFFFFFF`) for an identity, and 0 for everything else.
    pub fn default_clear(self) -> f32 {
        match self {
            AovSource::Depth | AovSource::Distance => f32::INFINITY,
            s if s.is_identity() => -1.0,
            _ => 0.0,
        }
    }

    /// Whether the film needs a per-sample accumulator for this source. The
    /// beauty is the [`Buffer`]; the sample count and variance are read off
    /// the pixel's own state.
    fn needs_slot(self) -> bool {
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
    /// The primvar of an [`AovSource::Primvar`] var; `None` for every other
    /// source.
    pub primvar: Option<PrimvarSource>,
}

/// One way the render reads a primvar: its name, how many components a var
/// writes of it, and whether as a colour (taken into the working space).
/// Two vars reading one primvar the same way share its values.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PrimvarRead {
    pub name: String,
    pub components: usize,
    pub color: bool,
}

/// The primvar a `sourceType = "primvar"` var reads, and how its channels
/// are laid out.
#[derive(Debug, Clone, PartialEq)]
pub struct PrimvarSource {
    /// The primvar's name, without the `primvars:` namespace.
    pub name: String,
    /// The channels' suffixes, from the var's type's role: a colour is
    /// `R/G/B[/A]` and taken into the working space, a texture coordinate
    /// `U/V[/W]`, any other vector `X/Y/Z[/W]`, a scalar one channel.
    pub kind: ChannelKind,
}

impl AovVar {
    /// Whether this var adds the beauty's alpha as a fourth channel: a
    /// `color4*` beauty or light path expression.
    pub fn with_alpha(&self) -> bool {
        matches!(self.source, AovSource::Color | AovSource::Lpe) && self.components == 4
    }

    /// How the var's channels are laid out: its source's, except that an
    /// expression's variance is one scalar and a primvar's is its type's.
    pub fn channel_kind(&self) -> ChannelKind {
        if self.is_lpe_variance() {
            ChannelKind::Scalar
        } else if let Some(p) = &self.primvar {
            p.kind
        } else {
            self.source.channel_kind()
        }
    }

    /// Whether the var's planes hold an integer's bit pattern rather than a
    /// number ([`AovSource::is_identity`]): a writer takes them with
    /// `f32::to_bits`.
    pub fn holds_bits(&self) -> bool {
        self.source.is_identity()
    }

    /// How the var reads its primvar, for a primvar var.
    pub fn primvar_read(&self) -> Option<PrimvarRead> {
        let p = self.primvar.as_ref()?;
        Some(PrimvarRead {
            name: p.name.clone(),
            components: self.components,
            color: p.kind == ChannelKind::Color,
        })
    }

    fn is_lpe_variance(&self) -> bool {
        self.variance && self.source == AovSource::Lpe
    }

    /// The film slot holding this var's per-sample accumulation, if any.
    /// `lpes` and `primvars` are the render's expression and primvar lists
    /// ([`AovLayout::lpes`], [`AovLayout::primvars`]).
    fn slot_key(&self, lpes: &[String], primvars: &[PrimvarRead]) -> Option<SlotKey> {
        let lpe = match &self.expression {
            Some(e) if self.source == AovSource::Lpe => lpes.iter().position(|x| x == e)? as u16,
            _ => NO_LPE,
        };
        let (primvar, width) = match self.primvar_read() {
            Some(read) if self.source == AovSource::Primvar => (
                primvars.iter().position(|x| *x == read)? as u16,
                self.components.clamp(1, 4) as u8,
            ),
            _ => (NO_PRIMVAR, self.source.components() as u8),
        };
        // An identity slot holds its integer's bits, its clear value too.
        let clear_bits = if self.source.is_identity() {
            self.clear as i64 as u32
        } else {
            self.clear.to_bits()
        };
        // A variance is a statistic over every sample: its slot is filtered
        // whatever the var carries. The importer refuses `Closest` on one;
        // an engine-built var asking for it still gets the variance.
        let accumulation = if self.is_lpe_variance() {
            Accumulation::Filtered
        } else {
            self.accumulation
        };
        self.source.needs_slot().then(|| SlotKey {
            source: self.source,
            accumulation,
            clear_bits,
            lpe,
            raw: self.raw,
            variance: self.is_lpe_variance(),
            primvar,
            width,
        })
    }
}

/// A slot that holds no light path expression.
const NO_LPE: u16 = u16::MAX;

/// A slot that holds no primvar.
const NO_PRIMVAR: u16 = u16::MAX;

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
    /// A `deepRaster` product: one OpenEXRId matte, whose ids stand for
    /// what its one identity var names ([`AovProduct::matte`]); a beauty var
    /// adds each id's share of the beauty.
    pub deep: bool,
}

impl AovProduct {
    /// The first var resolving to the beauty — the one written unprefixed
    /// and the source of the PNG preview.
    pub fn beauty(&self) -> Option<&AovVar> {
        self.vars.iter().find(|v| v.source == AovSource::Color)
    }

    /// The ID matte a `deepRaster` product writes: what its ids name, and
    /// whether it carries colour. `None` for a raster product, and for a
    /// deep one with no var that names an id (the importer refuses those).
    pub fn matte(&self) -> Option<MatteKey> {
        if !self.deep {
            return None;
        }
        let names = self.vars.iter().find_map(|v| v.source.name_key())?;
        Some(MatteKey {
            names,
            color: self.beauty().is_some(),
        })
    }
}

/// What one ID matte accumulates: the name each sample's id stands for,
/// and whether its samples carry their share of the beauty (`R/G/B`)
/// besides their coverage (`A`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MatteKey {
    pub names: NameKey,
    pub color: bool,
}

/// Every product a render writes. Empty when the stage authors none, which
/// means "write the single beauty EXR".
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AovRequest {
    pub products: Vec<AovProduct>,
}

impl AovRequest {
    /// Whether any var needs more than the beauty buffer — that is, whether
    /// the render must take the AOV path at all. An ID matte always does.
    pub fn needs_film(&self) -> bool {
        self.vars()
            .any(|v| v.source != AovSource::Color || v.with_alpha())
            || self.mattes().next().is_some()
    }

    /// The vars of the raster products: the ones written as planes.
    fn vars(&self) -> impl Iterator<Item = &AovVar> {
        self.products
            .iter()
            .filter(|p| !p.deep)
            .flat_map(|p| &p.vars)
    }

    /// The ID mattes the `deepRaster` products ask for.
    fn mattes(&self) -> impl Iterator<Item = MatteKey> + '_ {
        self.products.iter().filter_map(AovProduct::matte)
    }

    /// What the render needs to know about the first hit's identity: which
    /// names the flat ids and the mattes read, and whether a var reads the
    /// instance or the element. The importer keeps the tables these need,
    /// and only these.
    pub fn identity(&self) -> IdentityNeeds {
        let mut needs = IdentityNeeds::default();
        for key in self.mattes() {
            needs.names[key.names.index()] = true;
        }
        for v in self.vars() {
            if let Some(key) = v.source.name_key() {
                needs.names[key.index()] = true;
            }
            needs.instance |= v.source == AovSource::InstanceId;
            needs.element |= v.source == AovSource::ElementId;
        }
        needs
    }

    /// The primvars the render's vars read, each way once, in first-use
    /// order: what the importer keeps per geometry ([`AovLayout::primvars`]).
    pub fn primvars(&self) -> Vec<PrimvarRead> {
        let mut out: Vec<PrimvarRead> = Vec::new();
        for read in self.vars().filter_map(AovVar::primvar_read) {
            if !out.contains(&read) {
                out.push(read);
            }
        }
        out
    }
}

/// What the identity AOVs of a render read ([`AovRequest::identity`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IdentityNeeds {
    /// Per [`NameKey`], whether an id or a matte names it.
    pub names: [bool; 3],
    /// Whether a var reads `instanceId`.
    pub instance: bool,
    /// Whether a var reads `elementId`.
    pub element: bool,
}

impl IdentityNeeds {
    /// Whether anything is read at all.
    pub fn any(&self) -> bool {
        self.names.contains(&true) || self.instance || self.element
    }
}

/// What one film slot accumulates. Two vars asking for the same source with
/// the same mode and clear value share a slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct SlotKey {
    pub(crate) source: AovSource,
    pub(crate) accumulation: Accumulation,
    clear_bits: u32,
    /// For an [`AovSource::Lpe`] slot, the expression's index in
    /// [`AovLayout::lpes`] — its bit in the compiled DFA.
    lpe: u16,
    /// A raw light slot: the expression's value over the diffuse filter. The
    /// raw and plain slots of one expression share its DFA bit.
    raw: bool,
    /// An expression's variance slot: per sample, the luminance of the
    /// expression's weighted value goes into the unit's [`VarPlanes`]; the
    /// film holds the variance of the mean. It shares the expression's DFA
    /// bit with the value slot, as raw does.
    variance: bool,
    /// For an [`AovSource::Primvar`] slot, the primvar's index in
    /// [`AovLayout::primvars`].
    primvar: u16,
    /// Components per sample: the source's, or a primvar var's type's.
    width: u8,
}

impl SlotKey {
    pub(crate) fn clear(&self) -> f32 {
        f32::from_bits(self.clear_bits)
    }

    /// The power of the exposure scale this slot's values take: an
    /// expression's variance slot holds the variance of a luminance mean (2),
    /// every other slot its source's.
    fn exposure_power(&self) -> i32 {
        if self.variance {
            2
        } else {
            self.source.exposure_power()
        }
    }

    /// Components per pixel in the film: one for a variance slot, else the
    /// source's (a primvar's, its var's).
    fn components(&self) -> usize {
        if self.variance {
            1
        } else {
            self.width as usize
        }
    }

    /// A filtered slot whose clear value cannot be averaged — depth and
    /// distance clear to `+inf`. A miss times a filter weight is `±inf`
    /// (Mitchell's lobes are signed), and `inf − inf` is NaN, so such a slot
    /// averages only the samples that hit something, and keeps the clear
    /// value where none did.
    fn hits_only(&self) -> bool {
        self.accumulation == Accumulation::Filtered && !self.variance && !self.clear().is_finite()
    }
}

/// The slots a render accumulates, derived once from an [`AovRequest`].
#[derive(Debug, Clone, Default)]
pub(crate) struct AovLayout {
    pub(crate) slots: Vec<SlotKey>,
    pub(crate) sample_count: bool,
    pub(crate) variance: bool,
    /// The distinct light path expressions, in first-use order: expression
    /// `i` is bit `i` of the compiled DFA's accept masks. At most
    /// [`crate::lpe::MAX_EXPRESSIONS`] — the importer refuses the rest.
    pub(crate) lpes: Vec<String>,
    /// Whether a var asks for the albedo.
    pub(crate) albedo: bool,
    /// Whether a var needs the diffuse filter of the camera ray's first
    /// hit: a raw light AOV, or `diffuse_albedo`.
    pub(crate) diffuse_filter: bool,
    /// Whether a var asks for the motion vector, so the integrator looks up
    /// the first hit's motion at all.
    pub(crate) motion: bool,
    /// The working space's luminance weights, which an expression's
    /// variance reduces each sample by — the beauty's (`LightList::luma`).
    pub(crate) luma: utils::Luma,
    /// The compiled expressions and event symbols, built by the renderer
    /// (it needs the lights' tags) when `lpes` or `albedo` ask for routing.
    pub(crate) route: Option<std::sync::Arc<crate::tracer::RouteCtx>>,
    /// The light path inspector's configuration: every camera sample's
    /// records are kept per work unit ([`crate::inspect`]).
    pub(crate) inspect: Option<std::sync::Arc<crate::tracer::inspection::Config>>,
    /// What the identity vars and mattes read of the first hit.
    pub(crate) identity: IdentityNeeds,
    /// The primvars the vars read, in first-use order: a primvar slot's
    /// index, and the order a geometry's [`PrimvarSet`] holds them in.
    ///
    /// [`PrimvarSet`]: crate::identity::PrimvarSet
    pub(crate) primvars: Vec<PrimvarRead>,
    /// The ID mattes the `deepRaster` products write.
    pub(crate) mattes: Vec<MatteKey>,
    /// The world's identity tables, which name a matte's ids; set by the
    /// renderer when a matte or an identity var needs them.
    pub(crate) names: Option<std::sync::Arc<crate::identity::Identity>>,
}

impl AovLayout {
    pub(crate) fn new(request: &AovRequest) -> Self {
        let mut layout = AovLayout::default();
        for var in request.vars() {
            if let Some(e) = &var.expression
                && var.source == AovSource::Lpe
                && !layout.lpes.contains(e)
                && layout.lpes.len() < crate::lpe::MAX_EXPRESSIONS
            {
                layout.lpes.push(e.clone());
            }
        }
        layout.primvars = request.primvars();
        let lpes = layout.lpes.clone();
        let primvars = layout.primvars.clone();
        let mut add = |key: SlotKey| {
            if !layout.slots.contains(&key) {
                layout.slots.push(key);
            }
        };
        for var in request.vars() {
            if let Some(key) = var.slot_key(&lpes, &primvars) {
                add(key);
            }
            if var.with_alpha() {
                add(ALPHA_OF_BEAUTY);
            }
        }
        layout.albedo = request.vars().any(|v| v.source == AovSource::Albedo);
        layout.diffuse_filter = request
            .vars()
            .any(|v| v.raw || v.source == AovSource::DiffuseFilter);
        layout.sample_count = request.vars().any(|v| v.source == AovSource::SampleCount);
        layout.variance = request.vars().any(|v| v.source == AovSource::Variance);
        layout.motion = request.vars().any(|v| v.source == AovSource::MotionVector);
        layout.identity = request.identity();
        for key in request.mattes() {
            if !layout.mattes.contains(&key) {
                layout.mattes.push(key);
            }
        }
        layout
    }

    /// Whether the integrator has to look the first hit up in the world's
    /// identity or primvar tables at all.
    pub(crate) fn reads_hit_attributes(&self) -> bool {
        self.identity.any() || !self.primvars.is_empty()
    }
}

/// The alpha a `color4*` beauty carries: filtered coverage, cleared to 0.
const ALPHA_OF_BEAUTY: SlotKey = SlotKey {
    source: AovSource::Alpha,
    accumulation: Accumulation::Filtered,
    clear_bits: 0,
    lpe: NO_LPE,
    raw: false,
    variance: false,
    primvar: NO_PRIMVAR,
    width: 1,
};

/// What one camera sample carries for the AOVs besides its first hit: the
/// light each expression selected, the albedo, and the shutter time it was
/// traced at. Empty and unused unless the render asks for them.
#[derive(Clone, Copy)]
pub(crate) struct SampleExtras<'a> {
    pub(crate) lpe: &'a [Vec3A],
    pub(crate) albedo: Vec3A,
    /// The diffuse filter of the camera ray's first hit (0 off a surface).
    pub(crate) diffuse_filter: Vec3A,
    /// The sample's shutter time in `[0, 1)`: 0 unless the scene moves and
    /// motion blur is on. The motion vector rebases the hit by it.
    pub(crate) time: f32,
    /// The world the sample was traced in, which the identity and primvar
    /// AOVs look the first hit up in ([`FirstAttrs`]); `None` reads nothing.
    pub(crate) world: Option<&'a World>,
    /// The sample's radiance, unweighted — what a coloured matte splits
    /// among the ids.
    pub(crate) radiance: Vec3A,
}

/// What the camera ray met at the path's first vertex, recorded by the
/// AOV instantiation of the integrator and nothing else.
#[derive(Debug, Clone, Copy)]
pub(crate) enum FirstHit {
    /// The primary ray left the scene (or the path never started).
    Escaped,
    /// A volume scatter: a position, but no surface.
    Volume { p: Vec3A },
    /// A camera-visible surface, after cutout pass-through.
    ///
    /// `n` and `motion` are `Vec3`, not `Vec3A`, so the hit's ids fit in
    /// the bytes their fourth lanes took: `FirstHit` keeps its size, and
    /// [`PathScratch`](crate::tracer) the layout the beauty-only render's
    /// instruction count was measured with ("Identity and primvars" in the
    /// `aovs` design record).
    Surface {
        p: Vec3A,
        /// The shading normal (after bump and normal map), facing the ray.
        n: Vec3,
        uv: Option<(f32, f32)>,
        /// The world-space translation the hit geometry moves through over
        /// the shutter ([`World::motion`]); zero when it does not move, or
        /// when no var asks for the motion vector.
        motion: Vec3,
        /// The hit's geometry and primitive: what the identity and primvar
        /// AOVs look up ([`FirstAttrs`]).
        geom_id: u32,
        prim_id: u32,
    },
}

/// The `motionvector` value: where the point hit at `p` at shutter time
/// `time` moves on screen from shutter open to shutter close, in pixels,
/// `u` right and `v` up.
///
/// The hit is rebased to its shutter-open position `P0 = p − time·v` first
/// (exact: the kernel's interpolation moves every point by `v` at constant
/// speed), so the value does not depend on which shutter time the sample
/// drew, and therefore not on whether the beauty is blurred. The segment
/// `[P0, P0 + v]` is then clipped to camera depth `≥ 1e-3 · max(z0, z1)` —
/// the camera has no near plane, and a projection at or behind depth 0
/// blows up or flips sign — and both ends are projected. The visible hit
/// lies on the segment in front of the camera, so `max(z0, z1) > 0` and at
/// most one end is clipped; the result is finite and points the way the
/// visible part moves. The limit depends only on the path, never on `time`,
/// so clipping keeps the independence from blur.
fn motion_vector(cam: &CameraFrame, p: Vec3A, v: Vec3A, time: f32) -> (f32, f32) {
    if v == Vec3A::ZERO {
        return (0.0, 0.0);
    }
    let p0 = p - time * v;
    let p1 = p0 + v;
    let (z0, z1) = (cam.depth(p0), cam.depth(p1));
    let z_max = z0.max(z1);
    if z_max.is_nan() || z_max <= 0.0 {
        // Nothing of the path is in front of the camera: a degenerate hit
        // (at the origin itself, or non-finite), which has no projection.
        return (0.0, 0.0);
    }
    let z_near = 1e-3 * z_max;
    let clip = |a: Vec3A, za: f32, b: Vec3A, zb: f32| {
        if za >= z_near {
            a
        } else {
            a + (b - a) * ((z_near - za) / (zb - za))
        }
    };
    let (s0, t0) = cam.proj(clip(p0, z0, p1, z1));
    let (s1, t1) = cam.proj(clip(p1, z1, p0, z0));
    ((s1 - s0) * cam.width, (t1 - t0) * cam.height)
}

/// One sample's value for `key`'s source, written into `out` (as many
/// components as the source has). Where the sample saw nothing the source
/// describes, the slot's clear value. An identity's value is its integer's
/// bit pattern.
fn sample_value(
    key: &SlotKey,
    hit: &FirstHit,
    extras: &SampleExtras,
    attrs: &FirstAttrs,
    cam: &CameraFrame,
    out: &mut [f32; 4],
) {
    let clear = key.clear();
    *out = [clear; 4];
    let put3 = |out: &mut [f32; 4], v: Vec3A| out[..3].copy_from_slice(&[v.x, v.y, v.z]);
    match (key.source, hit) {
        (AovSource::Lpe, _) if key.raw => {
            // Raw light: this sample's light over this sample's diffuse
            // colour, channel by channel, 0 where the colour is black.
            let v = extras.lpe[key.lpe as usize];
            let f = extras.diffuse_filter;
            let raw = |v: f32, f: f32| if f >= RAW_FILTER_FLOOR { v / f } else { 0.0 };
            out[..3].copy_from_slice(&[raw(v.x, f.x), raw(v.y, f.y), raw(v.z, f.z)]);
        }
        (AovSource::Lpe, _) => put3(out, extras.lpe[key.lpe as usize]),
        (AovSource::Albedo, _) => put3(out, extras.albedo),
        (AovSource::DiffuseFilter, _) => put3(out, extras.diffuse_filter),
        (AovSource::Alpha, FirstHit::Surface { .. }) => out[0] = 1.0,
        (AovSource::Alpha, _) => out[0] = 0.0,
        (AovSource::Depth, FirstHit::Surface { p, .. } | FirstHit::Volume { p }) => {
            out[0] = cam.depth(*p)
        }
        (AovSource::Distance, FirstHit::Surface { p, .. } | FirstHit::Volume { p }) => {
            out[0] = (*p - cam.origin).length()
        }
        (AovSource::P, FirstHit::Surface { p, .. } | FirstHit::Volume { p }) => put3(out, *p),
        (AovSource::Peye, FirstHit::Surface { p, .. } | FirstHit::Volume { p }) => {
            put3(out, cam.eye(*p - cam.origin))
        }
        (AovSource::Normal, FirstHit::Surface { n, .. }) => put3(out, (*n).into()),
        (AovSource::Neye, FirstHit::Surface { n, .. }) => put3(out, cam.eye((*n).into())),
        (AovSource::MotionVector, FirstHit::Surface { p, motion, .. }) => {
            let (u, v) = motion_vector(cam, *p, (*motion).into(), extras.time);
            out[0] = u;
            out[1] = v;
        }
        (
            AovSource::St,
            FirstHit::Surface {
                uv: Some((u, v)), ..
            },
        ) => {
            out[0] = *u;
            out[1] = *v;
        }
        (
            AovSource::PrimId | AovSource::MaterialId | AovSource::AssetId,
            FirstHit::Surface { .. },
        ) => {
            let k = key.source.name_key().expect("a named identity").index();
            if attrs.identity.names[k] != NO_NAME {
                out[0] = f32::from_bits(attrs.identity.hashes[k]);
            }
        }
        (AovSource::InstanceId, FirstHit::Surface { .. }) if attrs.identity.instance != NO_ID => {
            out[0] = f32::from_bits(attrs.identity.instance);
        }
        (AovSource::ElementId, FirstHit::Surface { .. }) if attrs.identity.element != NO_ID => {
            out[0] = f32::from_bits(attrs.identity.element);
        }
        (AovSource::Primvar, FirstHit::Surface { .. }) => {
            if let Some(Some(v)) = attrs.primvars.get(key.primvar as usize) {
                let n = key.width as usize;
                out[..n].copy_from_slice(&v[..n]);
            }
        }
        _ => {}
    }
}

/// What the identity and primvar AOVs read of a sample's first hit: the
/// names the world's [`Identity`](crate::identity::Identity) gives it, its
/// authored face, and the render's primvars there. Read by the unit, once
/// per sample, from the ids [`FirstHit::Surface`] carries — never by the
/// integrator, so the beauty-only render's path scratch is unchanged.
#[derive(Debug, Clone, Default)]
struct FirstAttrs {
    /// Whether any var reads them.
    on: bool,
    identity: HitIdentity,
    /// Each of [`AovLayout::primvars`] at the hit, `None` where the hit
    /// surface does not author it.
    primvars: Vec<Option<[f32; 4]>>,
}

impl FirstAttrs {
    fn new(on: bool, primvars: usize) -> Self {
        FirstAttrs {
            on,
            identity: HitIdentity::NONE,
            primvars: vec![None; primvars],
        }
    }

    /// Looks `hit` up in `world`. The barycentrics are solved only when a
    /// primvar spreads over the face's corners
    /// ([`World::barycentrics`]).
    fn read(&mut self, world: &World, hit: &FirstHit) {
        self.identity = HitIdentity::NONE;
        self.primvars.fill(None);
        let FirstHit::Surface {
            p,
            geom_id,
            prim_id,
            ..
        } = *hit
        else {
            return;
        };
        if let Some(names) = world.identity() {
            self.identity = names.lookup(geom_id);
        }
        let Some(geom) = world.aov_geometry(geom_id) else {
            return;
        };
        self.identity.element = geom.element(prim_id);
        let Some(set) = geom.primvars.as_deref() else {
            return;
        };
        let bary = if geom.needs_barycentrics() {
            world.barycentrics(p, geom_id, prim_id)
        } else {
            None
        };
        let at = geom.locate(prim_id, bary);
        for (i, value) in self.primvars.iter_mut().enumerate() {
            *value = set.eval(i, at);
        }
    }
}

/// One slot's per-pixel state over a work unit (or, gathered, the frame).
#[derive(Debug, Clone)]
struct SlotPlanes {
    /// `pixels × components`: the weighted sum (filtered) or the chosen
    /// sample's value (closest).
    values: Vec<f32>,
    /// Closest only, per pixel: the chosen sample's rank key — its camera
    /// depth when `in_box`, its squared distance to the pixel centre when
    /// not — and whether any sample inside the pixel's box was seen.
    key: Vec<f32>,
    in_box: Vec<bool>,
    /// [`SlotKey::hits_only`] slots only, per pixel: the filter weight and
    /// the count of the samples that hit.
    hit_weight: Vec<f32>,
    hits: Vec<u32>,
    /// A unit's variance slot only: the luminance moments its samples
    /// accumulate, in place of `values` (which such a unit leaves empty).
    /// The film's planes hold the resolved variance in `values` instead, so
    /// the finished frame costs no more than a scalar AOV — but every unit
    /// of a pass is built before it renders and kept until the film is
    /// assembled, so while a pass runs the moments cover its whole region.
    var: Option<VarPlanes>,
}

/// Per pixel, the sum and sum of squares of a variance slot's per-sample
/// luminance, in f64 like the beauty's `PixelState`. The sample count is
/// the pixel's own `taken`: every sample lands in every slot, zero or not.
#[derive(Debug, Clone)]
struct VarPlanes {
    sum: Vec<f64>,
    sq: Vec<f64>,
}

impl SlotPlanes {
    fn new(slot: &SlotKey, pixels: usize) -> Self {
        let comps = slot.components();
        let hits_only = slot.hits_only();
        match slot.accumulation {
            Accumulation::Filtered => SlotPlanes {
                values: vec![0.0; pixels * comps],
                key: Vec::new(),
                in_box: Vec::new(),
                hit_weight: if hits_only {
                    vec![0.0; pixels]
                } else {
                    Vec::new()
                },
                hits: if hits_only {
                    vec![0; pixels]
                } else {
                    Vec::new()
                },
                var: None,
            },
            Accumulation::Closest => SlotPlanes {
                values: vec![slot.clear(); pixels * comps],
                key: vec![f32::INFINITY; pixels],
                in_box: vec![false; pixels],
                hit_weight: Vec::new(),
                hits: Vec::new(),
                var: None,
            },
        }
    }

    /// The closest rule: a sample inside the pixel's box beats any outside
    /// it, and among equals the smaller key wins; a tie keeps the earlier
    /// sample, so the choice depends only on the pixel's own sample order.
    fn closer(&self, p: usize, in_box: bool, key: f32) -> bool {
        match (in_box, self.in_box[p]) {
            (true, false) => true,
            (false, true) => false,
            _ => key < self.key[p],
        }
    }
}

/// Per-sample accumulators for one work unit, beside its `PixelState`s.
///
/// Carries everything a sample needs to land in it — the slots, the camera
/// frame, and which of the unit's pixels is being sampled — so the
/// integrator's per-pixel entry point keeps the signature it has without
/// AOVs, and the beauty-only instantiation is the code it always was.
pub(crate) struct UnitAov {
    slots: Vec<SlotKey>,
    cam: CameraFrame,
    /// The unit's pixel the next samples belong to, set before each pixel
    /// is advanced.
    pub(crate) pixel: usize,
    planes: Vec<SlotPlanes>,
    luma: utils::Luma,
    /// The ID mattes' per-pixel entries.
    mattes: Vec<UnitMatte>,
    /// What the identity, primvar and matte vars read of the current
    /// sample's first hit.
    attrs: FirstAttrs,
}

impl UnitAov {
    pub(crate) fn new(layout: &AovLayout, cam: CameraFrame, pixels: usize) -> Self {
        UnitAov {
            slots: layout.slots.clone(),
            cam,
            pixel: 0,
            planes: layout
                .slots
                .iter()
                .map(|s| {
                    if s.variance {
                        SlotPlanes {
                            values: Vec::new(),
                            key: Vec::new(),
                            in_box: Vec::new(),
                            hit_weight: Vec::new(),
                            hits: Vec::new(),
                            var: Some(VarPlanes {
                                sum: vec![0.0; pixels],
                                sq: vec![0.0; pixels],
                            }),
                        }
                    } else {
                        SlotPlanes::new(s, pixels)
                    }
                })
                .collect(),
            luma: layout.luma,
            mattes: layout
                .mattes
                .iter()
                .map(|&k| UnitMatte::new(k, pixels))
                .collect(),
            attrs: FirstAttrs::new(layout.reads_hit_attributes(), layout.primvars.len()),
        }
    }

    /// Folds one camera sample into the current pixel. `(fx, fy)` is the
    /// sample's film offset from the pixel's corner (the box is `[0, 1)²`),
    /// `weight` the beauty's filter weight for it.
    pub(crate) fn add(
        &mut self,
        hit: &FirstHit,
        extras: &SampleExtras,
        fx: f32,
        fy: f32,
        weight: f32,
    ) {
        if self.attrs.on
            && let Some(world) = extras.world
        {
            self.attrs.read(world, hit);
        }
        let (p, cam, attrs) = (self.pixel, &self.cam, &self.attrs);
        let in_box = (0.0..1.0).contains(&fx) && (0.0..1.0).contains(&fy);
        let mut v = [0.0f32; 4];
        for (slot, planes) in self.slots.iter().zip(&mut self.planes) {
            let comps = slot.components();
            match slot.accumulation {
                Accumulation::Filtered if slot.variance => {
                    // The beauty's own reduction (`advance_pixel`): the
                    // luminance of the weighted colour, as f64. For
                    // `C.*[LO]` the colour is the beauty's bit for bit, so
                    // the moments are too.
                    sample_value(slot, hit, extras, attrs, cam, &mut v);
                    let x = self.luma.of(Vec3A::new(v[0], v[1], v[2]) * weight) as f64;
                    let var = planes.var.as_mut().expect("a unit's variance slot");
                    var.sum[p] += x;
                    var.sq[p] += x * x;
                }
                Accumulation::Filtered => {
                    sample_value(slot, hit, extras, attrs, cam, &mut v);
                    if slot.hits_only() {
                        // A sample that saw the quantity has a finite value;
                        // a miss has the (non-finite) clear value.
                        if !v[..comps].iter().all(|x| x.is_finite()) {
                            continue;
                        }
                        planes.hit_weight[p] += weight;
                        planes.hits[p] += 1;
                    }
                    for (c, x) in v[..comps].iter().enumerate() {
                        planes.values[p * comps + c] += weight * x;
                    }
                }
                Accumulation::Closest => {
                    let key = if in_box {
                        match hit {
                            FirstHit::Surface { p, .. } | FirstHit::Volume { p } => cam.depth(*p),
                            FirstHit::Escaped => f32::INFINITY,
                        }
                    } else {
                        let (dx, dy) = (fx - 0.5, fy - 0.5);
                        dx * dx + dy * dy
                    };
                    if planes.closer(p, in_box, key) {
                        sample_value(slot, hit, extras, attrs, cam, &mut v);
                        planes.values[p * comps..(p + 1) * comps].copy_from_slice(&v[..comps]);
                        planes.key[p] = key;
                        planes.in_box[p] = in_box;
                    }
                }
            }
        }
        // A matte counts the surfaces the camera saw first; an escape or a
        // volume scatter covers no name, so its weight is the background's.
        if let FirstHit::Surface { p: at, .. } = hit {
            for matte in &mut self.mattes {
                let name = attrs.identity.names[matte.key().names.index()];
                if name != NO_NAME {
                    matte.add(p, name, cam.depth(*at), weight, extras.radiance);
                }
            }
        }
    }
}

/// One slot's full-frame planes.
#[derive(Debug, Clone)]
struct FilmSlot {
    key: SlotKey,
    planes: SlotPlanes,
}

/// The AOVs of a render: planes beside the beauty [`Buffer`], covering the
/// same pixels — the render's region — in the same order (row `y` is the
/// buffer's row `y`, bottom-up).
#[derive(Debug, Clone)]
pub struct AovFilm {
    /// The pixels the planes hold, in raster space (rows bottom-up); every
    /// plane is indexed through it ([`PixelRect::index`]).
    rect: PixelRect,
    slots: Vec<FilmSlot>,
    /// Samples per pixel, when a var asks for it.
    sample_count: Option<Vec<f32>>,
    /// Variance of the pixel's luminance mean, when a var asks for it.
    variance: Option<Vec<f32>>,
    /// The render's light path expressions ([`AovLayout::lpes`]), to find
    /// an LPE var's slot.
    lpes: Vec<String>,
    /// The render's primvars ([`AovLayout::primvars`]), to find a primvar
    /// var's slot.
    primvars: Vec<PrimvarRead>,
    /// The ID mattes, one per [`MatteKey`] the products ask for.
    mattes: Vec<FilmMatte>,
    /// The world's identity tables, which name the mattes' ids.
    names: Option<std::sync::Arc<crate::identity::Identity>>,
}

impl AovFilm {
    /// A film over `rect`, a rectangle of the frame in raster space.
    pub(crate) fn new(layout: &AovLayout, rect: PixelRect) -> Self {
        let pixels = rect.area();
        AovFilm {
            rect,
            slots: layout
                .slots
                .iter()
                .map(|key| FilmSlot {
                    key: *key,
                    planes: SlotPlanes::new(key, pixels),
                })
                .collect(),
            sample_count: layout.sample_count.then(|| vec![0.0; pixels]),
            variance: layout.variance.then(|| vec![0.0; pixels]),
            lpes: layout.lpes.clone(),
            primvars: layout.primvars.clone(),
            mattes: layout
                .mattes
                .iter()
                .map(|&k| FilmMatte::new(k, pixels))
                .collect(),
            names: layout.names.clone(),
        }
    }

    /// The size of the planes: the render region's, which is the frame's
    /// unless the render was given a region.
    pub fn dimensions(&self) -> (usize, usize) {
        (self.rect.width(), self.rect.height())
    }

    /// Copies a unit's pixel `p` into frame pixel `(x, y)` (raster space,
    /// inside the film's region), resolving the
    /// filtered sums with the beauty's own estimator: `Σ wᵢ·vᵢ / Σ wᵢ`, or
    /// the plain mean where the weights cancel to nothing (see
    /// `PixelState::estimate`). A [`SlotKey::hits_only`] slot divides by
    /// the weight (or count) of its hits instead, and keeps its clear value
    /// where nothing was hit. A pixel that took no sample — only a cancelled
    /// render leaves one — keeps every plane at its clear value, where the
    /// estimators would divide `0 / 0`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn store(
        &mut self,
        unit: &UnitAov,
        p: usize,
        x: usize,
        y: usize,
        weight_sum: f32,
        taken: u32,
        variance: f64,
    ) {
        let q = self.rect.index(x, y);
        for (matte, src) in self.mattes.iter_mut().zip(&unit.mattes) {
            matte.store(src, p, q, weight_sum, taken);
        }
        if taken == 0 {
            for slot in &mut self.slots {
                let comps = slot.key.components();
                slot.planes.values[q * comps..(q + 1) * comps].fill(slot.key.clear());
            }
            // The sample count and the variance planes are created at 0.
            return;
        }
        for (slot, src) in self.slots.iter_mut().zip(&unit.planes) {
            let comps = slot.key.components();
            let dst = &mut slot.planes;
            match slot.key.accumulation {
                Accumulation::Filtered if slot.key.variance => {
                    let var = src.var.as_ref().expect("a unit's variance slot");
                    dst.values[q] = crate::buffer::var_of_mean(var.sum[p], var.sq[p], taken) as f32;
                }
                Accumulation::Filtered if slot.key.hits_only() => {
                    let (w, n) = (src.hit_weight[p], src.hits[p]);
                    for c in 0..comps {
                        let sum = src.values[p * comps + c];
                        dst.values[q * comps + c] = if n == 0 {
                            slot.key.clear()
                        } else if w > 0.0 {
                            sum / w
                        } else {
                            sum / n as f32
                        };
                    }
                }
                Accumulation::Filtered => {
                    for c in 0..comps {
                        let sum = src.values[p * comps + c];
                        dst.values[q * comps + c] = if weight_sum > 0.0 {
                            sum / weight_sum
                        } else {
                            sum / taken as f32
                        };
                    }
                }
                Accumulation::Closest => {
                    dst.values[q * comps..(q + 1) * comps]
                        .copy_from_slice(&src.values[p * comps..(p + 1) * comps]);
                    dst.key[q] = src.key[p];
                    dst.in_box[q] = src.in_box[p];
                }
            }
        }
        if let Some(n) = &mut self.sample_count {
            n[q] = taken as f32;
        }
        if let Some(v) = &mut self.variance {
            v[q] = variance as f32;
        }
    }

    /// Blends the films of independent passes with the beauty's own
    /// inverse-variance weights (`weights[k] / total`, applied in pass order
    /// exactly as the beauty blend applies them), so a filtered AOV of a
    /// guided render is the same linear combination of passes as the
    /// beauty. Closest slots take the closest sample across passes, the
    /// sample count sums, and the variance of the blended mean is
    /// `Σ (wₖ/total)² · varₖ` — for the `variance` source and an
    /// expression's variance alike, in the same arithmetic.
    ///
    /// A pass with no weight (its variance could not be estimated, as at
    /// 1 spp) adds nothing to the variance, and nothing non-finite to a
    /// filtered plane: `0 · inf` is NaN, where the beauty, finite, gets 0.
    /// A [`SlotKey::hits_only`] slot takes the weighted mean of the passes
    /// whose pixel hit something, and keeps its clear value where none did.
    pub(crate) fn blend(films: Vec<AovFilm>, weights: &[f64], total: f64) -> AovFilm {
        let first = films.first().expect("at least one pass");
        let mut out = AovFilm {
            rect: first.rect,
            slots: first.slots.clone(),
            sample_count: first.sample_count.as_ref().map(|n| vec![0.0; n.len()]),
            variance: first.variance.as_ref().map(|v| vec![0.0; v.len()]),
            lpes: first.lpes.clone(),
            primvars: first.primvars.clone(),
            mattes: (0..first.mattes.len())
                .map(|m| {
                    let passes: Vec<&FilmMatte> = films.iter().map(|f| &f.mattes[m]).collect();
                    FilmMatte::blend(&passes, weights, total)
                })
                .collect(),
            names: first.names.clone(),
        };
        for slot in &mut out.slots {
            if slot.key.accumulation == Accumulation::Filtered {
                slot.planes.values.fill(0.0);
            }
        }
        // Per hits-only slot, per pixel: the share of the passes that hit.
        let mut hit_share: Vec<Vec<f64>> = out
            .slots
            .iter()
            .map(|s| {
                if s.key.hits_only() {
                    vec![0.0; out.rect.area()]
                } else {
                    Vec::new()
                }
            })
            .collect();
        for (film, w) in films.iter().zip(weights) {
            let share = *w / total;
            for ((dst, src), hit_share) in out.slots.iter_mut().zip(&film.slots).zip(&mut hit_share)
            {
                match dst.key.accumulation {
                    Accumulation::Filtered if dst.key.variance => {
                        if share > 0.0 {
                            let share = share * share;
                            for (d, s) in dst.planes.values.iter_mut().zip(&src.planes.values) {
                                *d = (*d as f64 + share * *s as f64) as f32;
                            }
                        }
                    }
                    Accumulation::Filtered if dst.key.hits_only() => {
                        if share == 0.0 {
                            continue;
                        }
                        let comps = dst.key.components();
                        for (q, h) in hit_share.iter_mut().enumerate() {
                            let v = &src.planes.values[q * comps..(q + 1) * comps];
                            if v.iter().all(|x| x.is_finite()) {
                                *h += share;
                                for (d, s) in dst.planes.values[q * comps..(q + 1) * comps]
                                    .iter_mut()
                                    .zip(v)
                                {
                                    *d += s * share as f32;
                                }
                            }
                        }
                    }
                    Accumulation::Filtered => {
                        for (d, s) in dst.planes.values.iter_mut().zip(&src.planes.values) {
                            if share == 0.0 && !s.is_finite() {
                                continue;
                            }
                            *d += s * share as f32;
                        }
                    }
                    Accumulation::Closest => {
                        let comps = dst.key.components();
                        for q in 0..out.rect.area() {
                            if dst
                                .planes
                                .closer(q, src.planes.in_box[q], src.planes.key[q])
                            {
                                dst.planes.values[q * comps..(q + 1) * comps].copy_from_slice(
                                    &src.planes.values[q * comps..(q + 1) * comps],
                                );
                                dst.planes.key[q] = src.planes.key[q];
                                dst.planes.in_box[q] = src.planes.in_box[q];
                            }
                        }
                    }
                }
            }
            if let (Some(d), Some(s)) = (&mut out.sample_count, &film.sample_count) {
                d.iter_mut().zip(s).for_each(|(d, s)| *d += s);
            }
            if let (Some(d), Some(s)) = (&mut out.variance, &film.variance)
                && share > 0.0
            {
                let share = share * share;
                d.iter_mut()
                    .zip(s)
                    .for_each(|(d, s)| *d = (*d as f64 + share * *s as f64) as f32);
            }
        }
        for (slot, hit_share) in out.slots.iter_mut().zip(&hit_share) {
            if !slot.key.hits_only() {
                continue;
            }
            let comps = slot.key.components();
            for (q, h) in hit_share.iter().enumerate() {
                for d in &mut slot.planes.values[q * comps..(q + 1) * comps] {
                    *d = if *h > 0.0 {
                        (*d as f64 / *h) as f32
                    } else {
                        slot.key.clear()
                    };
                }
            }
        }
        out
    }

    fn slot(&self, key: &SlotKey) -> &FilmSlot {
        self.slots
            .iter()
            .find(|s| s.key == *key)
            .expect("every requested var has a slot in the film it was rendered with")
    }

    /// The channels `var` writes, one plane per component, each in
    /// top-down row order (the EXR's): `R, G, B[, A]` for the beauty, else
    /// the source's components. `beauty` is the buffer rendered with this
    /// film.
    pub fn var_channels(&self, beauty: &Buffer, var: &AovVar) -> Vec<Vec<f32>> {
        let (w, h) = self.dimensions();
        let top_down = |f: &dyn Fn(usize) -> f32| -> Vec<f32> {
            (0..w * h).map(|i| f((h - 1 - i / w) * w + i % w)).collect()
        };
        match var.source {
            AovSource::Color => {
                // `get_rgb` is already top-down over the same region.
                let mut out: Vec<Vec<f32>> = (0..3)
                    .map(|c| {
                        (0..w * h)
                            .map(|i| {
                                let (r, g, b) = beauty.get_rgb(i % w, i / w);
                                [r, g, b][c]
                            })
                            .collect()
                    })
                    .collect();
                if var.with_alpha() {
                    let alpha = self.slot(&ALPHA_OF_BEAUTY);
                    out.push(top_down(&|q| alpha.planes.values[q]));
                }
                out
            }
            AovSource::SampleCount => {
                let n = self.sample_count.as_ref().expect("laid out");
                vec![top_down(&|q| n[q])]
            }
            AovSource::Variance => {
                let v = self.variance.as_ref().expect("laid out");
                vec![top_down(&|q| v[q])]
            }
            _ => {
                let key = var
                    .slot_key(&self.lpes, &self.primvars)
                    .expect("a slotted source");
                let slot = self.slot(&key);
                let comps = key.components();
                (0..comps)
                    .map(|c| top_down(&|q| slot.planes.values[q * comps + c]))
                    .collect()
            }
        }
    }

    /// One pixel of [`var_channels`](Self::var_channels): the value of each
    /// of `var`'s channels at (`x`, `y`) of the region, top-down, without
    /// building the planes. Pinned equal to `var_channels` at every pixel.
    pub fn var_pixel(&self, beauty: &Buffer, var: &AovVar, x: usize, y: usize) -> Vec<f32> {
        let (w, h) = self.dimensions();
        // The bottom-up plane index of a top-down pixel, as `var_channels`'
        // `top_down` maps it.
        let q = (h - 1 - y) * w + x;
        match var.source {
            AovSource::Color => {
                let (r, g, b) = beauty.get_rgb(x, y);
                let mut out = vec![r, g, b];
                if var.with_alpha() {
                    out.push(self.slot(&ALPHA_OF_BEAUTY).planes.values[q]);
                }
                out
            }
            AovSource::SampleCount => vec![self.sample_count.as_ref().expect("laid out")[q]],
            AovSource::Variance => vec![self.variance.as_ref().expect("laid out")[q]],
            _ => {
                let key = var
                    .slot_key(&self.lpes, &self.primvars)
                    .expect("a slotted source");
                let slot = self.slot(&key);
                let comps = key.components();
                (0..comps)
                    .map(|c| slot.planes.values[q * comps + c])
                    .collect()
            }
        }
    }
}

impl AovFilm {
    /// Multiplies every plane by the power of `scale` its slot takes
    /// ([`AovSource::exposure_power`]): radiance by `scale`, variances by
    /// its square, the rest not at all. Every value a channel is read from is
    /// linear in a slot's `values`, so scaling them scales the channel and
    /// leaves the filter weights and hit counts alone.
    pub(crate) fn apply_exposure(&mut self, scale: f32) {
        for slot in &mut self.slots {
            let power = slot.key.exposure_power();
            if power > 0 {
                let factor = scale.powi(power);
                slot.planes.values.iter_mut().for_each(|v| *v *= factor);
            }
        }
        if let Some(variance) = &mut self.variance {
            let factor = scale.powi(AovSource::Variance.exposure_power());
            variance.iter_mut().for_each(|v| *v *= factor);
        }
        for matte in &mut self.mattes {
            matte.apply_exposure(scale);
        }
    }

    /// The ID matte `product` writes, if it is a `deepRaster` product the
    /// render was asked for: each pixel's samples and the names they
    /// stand for.
    pub fn matte(&self, product: &AovProduct) -> Option<MatteView<'_>> {
        let key = product.matte()?;
        let matte = self.mattes.iter().find(|m| m.key() == key)?;
        Some(MatteView {
            matte,
            names: self.names.as_deref().map(|n| n.names(key.names)),
            width: self.rect.width(),
            height: self.rect.height(),
            rect: self.rect,
        })
    }

    /// The film of a render that asks for no AOV: no planes at all.
    pub fn empty(width: usize, height: usize) -> Self {
        AovFilm::new(&AovLayout::default(), PixelRect::full(width, height))
    }
}

/// One ID matte of a film, as its writer reads it: per pixel of the render's
/// region, its samples front to back, and the name each sample's id stands
/// for.
pub struct MatteView<'a> {
    matte: &'a FilmMatte,
    names: Option<&'a NameTable>,
    width: usize,
    height: usize,
    rect: PixelRect,
}

impl MatteView<'_> {
    /// What the matte's ids name and whether it carries colour.
    pub fn key(&self) -> MatteKey {
        self.matte.key()
    }

    /// The size of the region the matte covers.
    pub fn dimensions(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    /// The samples of pixel `(x, y)` of the region, `y` top-down (an EXR's
    /// row order), front to back.
    pub fn pixel(&self, x: usize, y: usize) -> &[MatteSample] {
        let q = self.rect.index(self.rect.x0 + x, self.rect.y1 - 1 - y);
        self.matte.pixel(q)
    }

    /// The name a sample's id stands for: a prim, material or asset path,
    /// or [`SPILL_NAME`] for the names a crowded pixel had no room for.
    pub fn name(&self, id: u32) -> &str {
        if id == SPILL {
            return SPILL_NAME;
        }
        self.names.and_then(|n| n.name(id)).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: SampleExtras<'static> = SampleExtras {
        lpe: &[],
        albedo: Vec3A::ZERO,
        diffuse_filter: Vec3A::ZERO,
        time: 0.0,
        world: None,
        radiance: Vec3A::ZERO,
    };

    /// A camera at the origin looking down −Z with a 90° square frame one
    /// unit away, on a 100×100 image: one unit of `x` at depth 1 is 50 px.
    fn frame() -> CameraFrame {
        CameraFrame {
            origin: Vec3A::ZERO,
            u: Vec3A::X,
            v: Vec3A::Y,
            w: Vec3A::Z,
            lower_left: Vec3A::new(-1.0, -1.0, -1.0),
            horizontal: Vec3A::new(2.0, 0.0, 0.0),
            vertical: Vec3A::new(0.0, 2.0, 0.0),
            width: 100.0,
            height: 100.0,
        }
    }

    fn var(source: AovSource, accumulation: Accumulation) -> AovVar {
        AovVar {
            prim_path: "/Render/Vars/x".into(),
            name: "x".into(),
            channel_prefix: None,
            source,
            components: source.components(),
            precision: Precision::Float,
            accumulation,
            clear: source.default_clear(),
            expression: None,
            raw: false,
            variance: false,
            primvar: None,
        }
    }

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

    #[test]
    fn closest_prefers_in_box_samples_then_the_nearest() {
        let request = AovRequest {
            products: vec![AovProduct {
                prim_path: "/p".into(),
                name: "a.exr".into(),
                vars: vec![var(AovSource::Depth, Accumulation::Closest)],
                attributes: Vec::new(),
                deep: false,
            }],
        };
        let layout = AovLayout::new(&request);
        let cam = frame();
        let at = |z: f32| FirstHit::Volume {
            p: Vec3A::new(0.0, 0.0, -z),
        };
        let mut unit = UnitAov::new(&layout, cam, 1);
        // Outside the box and nearer: ignored once an in-box sample exists.
        unit.add(&at(1.0), &NONE, -0.2, 0.5, 1.0);
        unit.add(&at(10.0), &NONE, 0.5, 0.5, 1.0);
        unit.add(&at(2.0), &NONE, 0.25, 0.75, 1.0);
        unit.add(&at(5.0), &NONE, 0.75, 0.25, 1.0);
        let mut film = AovFilm::new(&layout, PixelRect::full(1, 1));
        film.store(&unit, 0, 0, 0, 4.0, 4, 0.0);
        let depth = film.var_channels(&Buffer::new(1, 1), &request.products[0].vars[0]);
        // Never a blend of 2, 5 and 10.
        assert_eq!(depth, vec![vec![2.0]]);
    }

    #[test]
    fn closest_falls_back_to_the_sample_nearest_the_centre() {
        let v = var(AovSource::Depth, Accumulation::Closest);
        let request = AovRequest {
            products: vec![AovProduct {
                prim_path: "/p".into(),
                name: "a.exr".into(),
                vars: vec![v.clone()],
                attributes: Vec::new(),
                deep: false,
            }],
        };
        let layout = AovLayout::new(&request);
        let cam = frame();
        let mut unit = UnitAov::new(&layout, cam, 1);
        let near = FirstHit::Volume {
            p: Vec3A::new(0.0, 0.0, -1.0),
        };
        let far = FirstHit::Volume {
            p: Vec3A::new(0.0, 0.0, -7.0),
        };
        unit.add(&near, &NONE, 1.6, 0.5, 1.0);
        unit.add(&far, &NONE, 1.2, 0.5, 1.0);
        let mut film = AovFilm::new(&layout, PixelRect::full(1, 1));
        film.store(&unit, 0, 0, 0, 2.0, 2, 0.0);
        assert_eq!(film.var_channels(&Buffer::new(1, 1), &v), vec![vec![7.0]]);
    }

    #[test]
    fn filtered_uses_the_beauty_weights_and_escapes_clear() {
        let v = var(AovSource::Alpha, Accumulation::Filtered);
        let request = AovRequest {
            products: vec![AovProduct {
                prim_path: "/p".into(),
                name: "a.exr".into(),
                vars: vec![v.clone()],
                attributes: Vec::new(),
                deep: false,
            }],
        };
        let layout = AovLayout::new(&request);
        let cam = frame();
        let surface = FirstHit::Surface {
            p: Vec3A::new(0.0, 0.0, -1.0),
            n: Vec3::Z,
            uv: None,
            motion: Vec3::ZERO,
            geom_id: 0,
            prim_id: 0,
        };
        let mut unit = UnitAov::new(&layout, cam, 1);
        unit.add(&surface, &NONE, 0.5, 0.5, 3.0);
        unit.add(&FirstHit::Escaped, &NONE, 0.5, 0.5, 1.0);
        let mut film = AovFilm::new(&layout, PixelRect::full(1, 1));
        film.store(&unit, 0, 0, 0, 4.0, 2, 0.0);
        assert_eq!(film.var_channels(&Buffer::new(1, 1), &v), vec![vec![0.75]]);
    }

    #[test]
    fn filtered_depth_with_signed_weights_never_goes_nan() {
        let mut v = var(AovSource::Depth, Accumulation::Filtered);
        v.clear = f32::INFINITY;
        let request = AovRequest {
            products: vec![AovProduct {
                prim_path: "/p".into(),
                name: "a.exr".into(),
                vars: vec![v.clone()],
                attributes: Vec::new(),
                deep: false,
            }],
        };
        let layout = AovLayout::new(&request);
        let cam = frame();
        let at = |z: f32| FirstHit::Volume {
            p: Vec3A::new(0.0, 0.0, -z),
        };
        // Two pixels: one mixing hits and misses under Mitchell-like signed
        // weights, one that misses with weights of both signs.
        let mut unit = UnitAov::new(&layout, cam, 2);
        unit.pixel = 0;
        unit.add(&at(2.0), &NONE, 0.5, 0.5, 1.5);
        unit.add(&at(4.0), &NONE, 0.5, 0.5, 0.5);
        unit.add(&FirstHit::Escaped, &NONE, 0.5, 0.5, -0.25);
        unit.pixel = 1;
        unit.add(&FirstHit::Escaped, &NONE, 0.5, 0.5, 1.2);
        unit.add(&FirstHit::Escaped, &NONE, 0.5, 0.5, -0.2);
        let mut film = AovFilm::new(&layout, PixelRect::full(2, 1));
        film.store(&unit, 0, 0, 0, 1.75, 3, 0.0);
        film.store(&unit, 1, 1, 0, 1.0, 2, 0.0);
        let depth = &film.var_channels(&Buffer::new(2, 1), &v)[0];
        // The weighted mean of the hits alone; the miss-only pixel clears.
        assert_eq!(depth, &vec![2.5, f32::INFINITY]);

        // Blended with a zero-weight pass (a 1 spp pass whose variance
        // could not be estimated), nothing goes NaN either.
        let other = film.clone();
        let blended = AovFilm::blend(vec![film, other], &[0.0, 1.0], 1.0);
        assert_eq!(
            &blended.var_channels(&Buffer::new(2, 1), &v)[0],
            &vec![2.5, f32::INFINITY]
        );
    }

    #[test]
    fn a_zero_weight_pass_leaves_the_variance_finite() {
        let v = var(AovSource::Variance, Accumulation::Filtered);
        let request = AovRequest {
            products: vec![AovProduct {
                prim_path: "/p".into(),
                name: "a.exr".into(),
                vars: vec![v.clone()],
                attributes: Vec::new(),
                deep: false,
            }],
        };
        let layout = AovLayout::new(&request);
        let cam = frame();
        let unit = UnitAov::new(&layout, cam, 1);
        let mut one_spp = AovFilm::new(&layout, PixelRect::full(1, 1));
        one_spp.store(&unit, 0, 0, 0, 1.0, 1, f64::INFINITY);
        let mut trained = AovFilm::new(&layout, PixelRect::full(1, 1));
        trained.store(&unit, 0, 0, 0, 4.0, 4, 0.5);
        let blended = AovFilm::blend(vec![one_spp, trained], &[0.0, 2.0], 2.0);
        assert_eq!(
            blended.var_channels(&Buffer::new(1, 1), &v),
            vec![vec![0.5]]
        );
    }

    #[test]
    fn a_beauty_only_request_needs_no_film() {
        let mut beauty = var(AovSource::Color, Accumulation::Filtered);
        let mut request = AovRequest {
            products: vec![AovProduct {
                prim_path: "/p".into(),
                name: "a.exr".into(),
                vars: vec![beauty.clone()],
                attributes: Vec::new(),
                deep: false,
            }],
        };
        assert!(!request.needs_film());
        beauty.components = 4;
        request.products[0].vars[0] = beauty;
        assert!(request.needs_film(), "color4f needs the alpha slot");
        assert!(!AovRequest::default().needs_film());
    }

    #[test]
    fn motion_vector_resolves_with_two_closest_components_and_no_aliases() {
        assert_eq!(
            AovSource::from_raw("motionvector"),
            Some(AovSource::MotionVector)
        );
        assert_eq!(AovSource::MotionVector.name(), "motionvector");
        assert_eq!(AovSource::MotionVector.components(), 2);
        assert_eq!(AovSource::MotionVector.channel_kind(), ChannelKind::Motion);
        assert_eq!(
            AovSource::MotionVector.default_accumulation(),
            Accumulation::Closest
        );
        assert_eq!(AovSource::MotionVector.default_clear(), 0.0);
        assert!(AovSource::MotionVector.accepts_accumulation());
        // Other renderers' names stay unknown, not aliases.
        for name in [
            "velocity",
            "Vector",
            "motionFore",
            "motionBack",
            "MotionVector",
        ] {
            assert_eq!(AovSource::from_raw(name), None, "{name}");
            assert!(!AovSource::is_planned(name), "{name}");
        }
        let layout = AovLayout::new(&AovRequest {
            products: vec![AovProduct {
                prim_path: "/p".into(),
                name: "a.exr".into(),
                vars: vec![var(AovSource::MotionVector, Accumulation::Closest)],
                attributes: Vec::new(),
                deep: false,
            }],
        });
        assert!(layout.motion);
        assert!(!AovLayout::new(&AovRequest::default()).motion);
    }

    /// `proj` is `Camera::get_ray`'s inverse at the lens centre: a point at
    /// any depth along the ray through `(s, t)` projects back to `(s, t)`.
    #[test]
    fn proj_inverts_get_ray_at_any_depth() {
        let cameras = [
            crate::Camera::new(
                Vec3A::new(0.0, 0.0, 5.0),
                Vec3A::ZERO,
                Vec3A::Y,
                60.0,
                16.0 / 9.0,
                0.0,
                5.0,
            ),
            // Off-axis, tilted up vector, a focus distance that is not 1.
            crate::Camera::new(
                Vec3A::new(3.0, 2.0, -4.0),
                Vec3A::new(-1.0, 0.5, 2.0),
                Vec3A::new(0.2, 1.0, 0.1),
                35.0,
                1.5,
                0.0,
                7.5,
            ),
        ];
        for camera in &cameras {
            let cam = camera.frame(320, 180);
            for &s in &[0.0, 0.13, 0.5, 0.87, 1.0] {
                for &t in &[0.0, 0.21, 0.5, 0.79, 1.0] {
                    let ray = camera.get_ray(s, t, [0.5, 0.5], 0.0);
                    for &k in &[0.05, 0.5, 1.0, 3.0, 20.0] {
                        let (ps, pt) = cam.proj(ray.at(k));
                        assert!(
                            (ps - s).abs() < 1e-5 && (pt - t).abs() < 1e-5,
                            "({s}, {t}) at {k}: ({ps}, {pt})"
                        );
                    }
                }
            }
        }
    }

    /// The same point of a moving object, hit at different shutter times,
    /// gives one vector: the hit is rebased to shutter open first.
    #[test]
    fn the_motion_vector_does_not_depend_on_the_shutter_time() {
        let cam = frame();
        let p0 = Vec3A::new(-0.4, 0.2, -2.0);
        let v = Vec3A::new(0.6, -0.3, 0.5);
        let at_open = motion_vector(&cam, p0, v, 0.0);
        for time in [0.3, 0.9] {
            let (u, w) = motion_vector(&cam, p0 + time * v, v, time);
            assert!(
                (u - at_open.0).abs() < 1e-4 && (w - at_open.1).abs() < 1e-4,
                "time {time}: ({u}, {w}) vs {at_open:?}"
            );
        }
        // Rightwards and upwards motion is positive `u` and `v`; one unit
        // of `x` at depth 2 is 25 px here.
        let (u, w) = motion_vector(
            &cam,
            Vec3A::new(0.0, 0.0, -2.0),
            Vec3A::new(1.0, 0.5, 0.0),
            0.0,
        );
        assert!(
            (u - 25.0).abs() < 1e-3 && (w - 12.5).abs() < 1e-3,
            "({u}, {w})"
        );
        assert_eq!(motion_vector(&cam, p0, Vec3A::ZERO, 0.4), (0.0, 0.0));
    }

    /// A path crossing the camera plane, in either direction, is measured
    /// over its part in front of the camera: finite, along the visible
    /// motion.
    #[test]
    fn a_path_crossing_the_camera_plane_gives_a_finite_vector_along_the_visible_motion() {
        let cam = frame();
        // Through the camera: from 2 in front, rightwards, to 2 behind.
        let (u, v) = motion_vector(
            &cam,
            Vec3A::new(0.0, 0.0, -2.0),
            Vec3A::new(1.0, 0.0, 4.0),
            0.0,
        );
        assert!(u.is_finite() && v.is_finite(), "({u}, {v})");
        assert!(u > 0.0 && v.abs() < 1e-3, "({u}, {v})");
        // Arriving from behind, leftwards, hit at time 0.9 (in front).
        let p0 = Vec3A::new(1.0, 0.0, 2.0);
        let v3 = Vec3A::new(-1.0, 0.0, -4.0);
        let (u, v) = motion_vector(&cam, p0 + 0.9 * v3, v3, 0.9);
        assert!(u.is_finite() && v.is_finite(), "({u}, {v})");
        assert!(u < 0.0 && v.abs() < 1e-3, "({u}, {v})");
    }

    /// An expression's variance is a statistic over every sample, so its
    /// slot is filtered whatever accumulation the var carries: an
    /// engine-built var asking for `Closest` (the importer refuses it) still
    /// gets the variance, never the nearest sample's red channel.
    #[test]
    fn a_variance_var_is_filtered_whatever_its_accumulation() {
        let mut v = var(AovSource::Lpe, Accumulation::Closest);
        v.expression = Some("C.*[LO]".into());
        v.components = 1;
        v.variance = true;
        let request = AovRequest {
            products: vec![AovProduct {
                prim_path: "/p".into(),
                name: "a.exr".into(),
                vars: vec![v.clone()],
                attributes: Vec::new(),
                deep: false,
            }],
        };
        let layout = AovLayout::new(&request);
        let mut unit = UnitAov::new(&layout, frame(), 1);
        let hit = FirstHit::Surface {
            p: Vec3A::new(0.0, 0.0, -1.0),
            n: Vec3::Z,
            uv: None,
            motion: Vec3::ZERO,
            geom_id: 0,
            prim_id: 0,
        };
        let samples = [Vec3A::new(1.0, 0.0, 0.0), Vec3A::new(3.0, 0.0, 0.0)];
        for lpe in &samples {
            let extras = SampleExtras {
                lpe: std::slice::from_ref(lpe),
                ..NONE
            };
            unit.add(&hit, &extras, 0.5, 0.5, 1.0);
        }
        let mut film = AovFilm::new(&layout, PixelRect::full(1, 1));
        film.store(&unit, 0, 0, 0, 2.0, 2, 0.0);
        let (x0, x1) = (
            layout.luma.of(samples[0]) as f64,
            layout.luma.of(samples[1]) as f64,
        );
        let want = crate::buffer::var_of_mean(x0 + x1, x0 * x0 + x1 * x1, 2) as f32;
        assert!(want > 0.0);
        assert_eq!(film.var_channels(&Buffer::new(1, 1), &v), vec![vec![want]]);
    }

    /// Only a surface has a motion vector; a volume scatter and an escape
    /// hold the clear value.
    #[test]
    fn volumes_and_escapes_take_the_motion_vectors_clear_value() {
        let mut v = var(AovSource::MotionVector, Accumulation::Closest);
        v.clear = 7.0;
        let key = v.slot_key(&[], &[]).expect("slotted");
        let cam = frame();
        let mut out = [0.0f32; 4];
        sample_value(
            &key,
            &FirstHit::Volume {
                p: Vec3A::new(0.0, 0.0, -1.0),
            },
            &NONE,
            &FirstAttrs::default(),
            &cam,
            &mut out,
        );
        assert_eq!(out[..2], [7.0, 7.0]);
        sample_value(
            &key,
            &FirstHit::Escaped,
            &NONE,
            &FirstAttrs::default(),
            &cam,
            &mut out,
        );
        assert_eq!(out[..2], [7.0, 7.0]);
        let moving = FirstHit::Surface {
            p: Vec3A::new(0.0, 0.0, -2.0),
            n: Vec3::Z,
            uv: None,
            motion: Vec3::new(1.0, 0.0, 0.0),
            geom_id: 0,
            prim_id: 0,
        };
        sample_value(&key, &moving, &NONE, &FirstAttrs::default(), &cam, &mut out);
        assert!(
            (out[0] - 25.0).abs() < 1e-3 && out[1].abs() < 1e-3,
            "{out:?}"
        );
        let still = FirstHit::Surface {
            p: Vec3A::new(0.0, 0.0, -2.0),
            n: Vec3::Z,
            uv: None,
            motion: Vec3::ZERO,
            geom_id: 0,
            prim_id: 0,
        };
        sample_value(&key, &still, &NONE, &FirstAttrs::default(), &cam, &mut out);
        assert_eq!(out[..2], [0.0, 0.0]);
    }
}
