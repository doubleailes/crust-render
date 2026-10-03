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

use glam::Vec3A;

use crate::buffer::Buffer;

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
            AovSource::St => 2,
            AovSource::Alpha
            | AovSource::Depth
            | AovSource::Distance
            | AovSource::SampleCount
            | AovSource::Variance => 1,
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
            AovSource::Alpha
            | AovSource::Depth
            | AovSource::Distance
            | AovSource::SampleCount
            | AovSource::Variance => ChannelKind::Scalar,
        }
    }

    /// The accumulation a var gets unless it authors one: data that must
    /// never be blended across an edge is closest, the rest filtered.
    pub fn default_accumulation(self) -> Accumulation {
        match self {
            AovSource::Depth | AovSource::Distance | AovSource::P | AovSource::Peye => {
                Accumulation::Closest
            }
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
}

impl AovVar {
    /// Whether this var adds the beauty's alpha as a fourth channel: a
    /// `color4*` beauty or light path expression.
    pub fn with_alpha(&self) -> bool {
        matches!(self.source, AovSource::Color | AovSource::Lpe) && self.components == 4
    }

    /// The film slot holding this var's per-sample accumulation, if any.
    /// `lpes` is the render's expression list ([`AovLayout::lpes`]).
    fn slot_key(&self, lpes: &[String]) -> Option<SlotKey> {
        let lpe = match &self.expression {
            Some(e) if self.source == AovSource::Lpe => lpes.iter().position(|x| x == e)? as u16,
            _ => NO_LPE,
        };
        self.source.needs_slot().then(|| SlotKey {
            source: self.source,
            accumulation: self.accumulation,
            clear_bits: self.clear.to_bits(),
            lpe,
            raw: self.raw,
        })
    }
}

/// A slot that holds no light path expression.
const NO_LPE: u16 = u16::MAX;

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

    fn vars(&self) -> impl Iterator<Item = &AovVar> {
        self.products.iter().flat_map(|p| &p.vars)
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
}

impl SlotKey {
    pub(crate) fn clear(&self) -> f32 {
        f32::from_bits(self.clear_bits)
    }

    /// A filtered slot whose clear value cannot be averaged — depth and
    /// distance clear to `+inf`. A miss times a filter weight is `±inf`
    /// (Mitchell's lobes are signed), and `inf − inf` is NaN, so such a slot
    /// averages only the samples that hit something, and keeps the clear
    /// value where none did.
    fn hits_only(&self) -> bool {
        self.accumulation == Accumulation::Filtered && !self.clear().is_finite()
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
    /// The compiled expressions and event symbols, built by the renderer
    /// (it needs the lights' tags) when `lpes` or `albedo` ask for routing.
    pub(crate) route: Option<std::sync::Arc<crate::tracer::RouteCtx>>,
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
        let lpes = layout.lpes.clone();
        let mut add = |key: SlotKey| {
            if !layout.slots.contains(&key) {
                layout.slots.push(key);
            }
        };
        for var in request.vars() {
            if let Some(key) = var.slot_key(&lpes) {
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
        layout
    }
}

/// The alpha a `color4*` beauty carries: filtered coverage, cleared to 0.
const ALPHA_OF_BEAUTY: SlotKey = SlotKey {
    source: AovSource::Alpha,
    accumulation: Accumulation::Filtered,
    clear_bits: 0,
    lpe: NO_LPE,
    raw: false,
};

/// What one camera sample carries for the AOVs besides its first hit: the
/// light each expression selected, and the albedo. Empty and unused unless
/// the render asks for them.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SampleExtras<'a> {
    pub(crate) lpe: &'a [Vec3A],
    pub(crate) albedo: Vec3A,
    /// The diffuse filter of the camera ray's first hit (0 off a surface).
    pub(crate) diffuse_filter: Vec3A,
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
    Surface {
        p: Vec3A,
        /// The shading normal (after bump and normal map), facing the ray.
        n: Vec3A,
        uv: Option<(f32, f32)>,
    },
}

/// The camera's frame, for the camera-space sources. Camera space is the
/// USD camera's: `u` right, `v` up, looking down `−w`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CameraFrame {
    pub(crate) origin: Vec3A,
    pub(crate) u: Vec3A,
    pub(crate) v: Vec3A,
    pub(crate) w: Vec3A,
}

impl CameraFrame {
    /// `d`, a world-space offset or direction, in camera space.
    fn eye(&self, d: Vec3A) -> Vec3A {
        Vec3A::new(d.dot(self.u), d.dot(self.v), d.dot(self.w))
    }

    /// Camera-space depth of `p`: positive in front of the camera.
    fn depth(&self, p: Vec3A) -> f32 {
        -(p - self.origin).dot(self.w)
    }
}

/// One sample's value for `key`'s source, written into `out` (as many
/// components as the source has). Where the sample saw nothing the source
/// describes, the slot's clear value.
fn sample_value(
    key: &SlotKey,
    hit: &FirstHit,
    extras: &SampleExtras,
    cam: &CameraFrame,
    out: &mut [f32; 3],
) {
    let clear = key.clear();
    *out = [clear; 3];
    let put3 = |out: &mut [f32; 3], v: Vec3A| *out = [v.x, v.y, v.z];
    match (key.source, hit) {
        (AovSource::Lpe, _) if key.raw => {
            // Raw light: this sample's light over this sample's diffuse
            // colour, channel by channel, 0 where the colour is black.
            let v = extras.lpe[key.lpe as usize];
            let f = extras.diffuse_filter;
            let raw = |v: f32, f: f32| if f >= RAW_FILTER_FLOOR { v / f } else { 0.0 };
            *out = [raw(v.x, f.x), raw(v.y, f.y), raw(v.z, f.z)];
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
        (AovSource::Normal, FirstHit::Surface { n, .. }) => put3(out, *n),
        (AovSource::Neye, FirstHit::Surface { n, .. }) => put3(out, cam.eye(*n)),
        (
            AovSource::St,
            FirstHit::Surface {
                uv: Some((u, v)), ..
            },
        ) => {
            out[0] = *u;
            out[1] = *v;
        }
        _ => {}
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
}

impl SlotPlanes {
    fn new(slot: &SlotKey, pixels: usize) -> Self {
        let comps = slot.source.components();
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
            },
            Accumulation::Closest => SlotPlanes {
                values: vec![slot.clear(); pixels * comps],
                key: vec![f32::INFINITY; pixels],
                in_box: vec![false; pixels],
                hit_weight: Vec::new(),
                hits: Vec::new(),
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
                .map(|s| SlotPlanes::new(s, pixels))
                .collect(),
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
        let (p, cam) = (self.pixel, &self.cam);
        let in_box = (0.0..1.0).contains(&fx) && (0.0..1.0).contains(&fy);
        let mut v = [0.0f32; 3];
        for (slot, planes) in self.slots.iter().zip(&mut self.planes) {
            let comps = slot.source.components();
            match slot.accumulation {
                Accumulation::Filtered => {
                    sample_value(slot, hit, extras, cam, &mut v);
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
                        sample_value(slot, hit, extras, cam, &mut v);
                        planes.values[p * comps..(p + 1) * comps].copy_from_slice(&v[..comps]);
                        planes.key[p] = key;
                        planes.in_box[p] = in_box;
                    }
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

/// The AOVs of a render: full-frame planes beside the beauty [`Buffer`], in
/// the same pixel order (row `y` is the buffer's row `y`, bottom-up).
#[derive(Debug, Clone)]
pub struct AovFilm {
    width: usize,
    height: usize,
    slots: Vec<FilmSlot>,
    /// Samples per pixel, when a var asks for it.
    sample_count: Option<Vec<f32>>,
    /// Variance of the pixel's luminance mean, when a var asks for it.
    variance: Option<Vec<f32>>,
    /// The render's light path expressions ([`AovLayout::lpes`]), to find
    /// an LPE var's slot.
    lpes: Vec<String>,
}

impl AovFilm {
    pub(crate) fn new(layout: &AovLayout, width: usize, height: usize) -> Self {
        let pixels = width * height;
        AovFilm {
            width,
            height,
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
        }
    }

    pub fn dimensions(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    /// Copies a unit's pixel `p` into frame pixel `(x, y)`, resolving the
    /// filtered sums with the beauty's own estimator: `Σ wᵢ·vᵢ / Σ wᵢ`, or
    /// the plain mean where the weights cancel to nothing (see
    /// `PixelState::estimate`). A [`SlotKey::hits_only`] slot divides by
    /// the weight (or count) of its hits instead, and keeps its clear value
    /// where nothing was hit.
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
        let q = y * self.width + x;
        for (slot, src) in self.slots.iter_mut().zip(&unit.planes) {
            let comps = slot.key.source.components();
            let dst = &mut slot.planes;
            match slot.key.accumulation {
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
    /// `Σ (wₖ/total)² · varₖ`.
    ///
    /// A pass with no weight (its variance could not be estimated, as at
    /// 1 spp) adds nothing to the variance, and nothing non-finite to a
    /// filtered plane: `0 · inf` is NaN, where the beauty, finite, gets 0.
    /// A [`SlotKey::hits_only`] slot takes the weighted mean of the passes
    /// whose pixel hit something, and keeps its clear value where none did.
    pub(crate) fn blend(films: Vec<AovFilm>, weights: &[f64], total: f64) -> AovFilm {
        let first = films.first().expect("at least one pass");
        let mut out = AovFilm {
            width: first.width,
            height: first.height,
            slots: first.slots.clone(),
            sample_count: first.sample_count.as_ref().map(|n| vec![0.0; n.len()]),
            variance: first.variance.as_ref().map(|v| vec![0.0; v.len()]),
            lpes: first.lpes.clone(),
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
                    vec![0.0; out.width * out.height]
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
                    Accumulation::Filtered if dst.key.hits_only() => {
                        if share == 0.0 {
                            continue;
                        }
                        let comps = dst.key.source.components();
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
                        let comps = dst.key.source.components();
                        for q in 0..out.width * out.height {
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
            let comps = slot.key.source.components();
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
        let (w, h) = (self.width, self.height);
        let top_down = |f: &dyn Fn(usize) -> f32| -> Vec<f32> {
            (0..w * h).map(|i| f((h - 1 - i / w) * w + i % w)).collect()
        };
        match var.source {
            AovSource::Color => {
                let mut out: Vec<Vec<f32>> = (0..3)
                    .map(|c| top_down(&|q| beauty.get_pixel(q % w, q / w)[c]))
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
                let slot = self.slot(&var.slot_key(&self.lpes).expect("a slotted source"));
                let comps = var.source.components();
                (0..comps)
                    .map(|c| top_down(&|q| slot.planes.values[q * comps + c]))
                    .collect()
            }
        }
    }
}

impl AovFilm {
    /// The film of a render that asks for no AOV: no planes at all.
    pub fn empty(width: usize, height: usize) -> Self {
        AovFilm::new(&AovLayout::default(), width, height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: SampleExtras<'static> = SampleExtras {
        lpe: &[],
        albedo: Vec3A::ZERO,
        diffuse_filter: Vec3A::ZERO,
    };

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
            }],
        };
        let layout = AovLayout::new(&request);
        let cam = CameraFrame {
            origin: Vec3A::ZERO,
            u: Vec3A::X,
            v: Vec3A::Y,
            w: Vec3A::Z,
        };
        let at = |z: f32| FirstHit::Volume {
            p: Vec3A::new(0.0, 0.0, -z),
        };
        let mut unit = UnitAov::new(&layout, cam, 1);
        // Outside the box and nearer: ignored once an in-box sample exists.
        unit.add(&at(1.0), &NONE, -0.2, 0.5, 1.0);
        unit.add(&at(10.0), &NONE, 0.5, 0.5, 1.0);
        unit.add(&at(2.0), &NONE, 0.25, 0.75, 1.0);
        unit.add(&at(5.0), &NONE, 0.75, 0.25, 1.0);
        let mut film = AovFilm::new(&layout, 1, 1);
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
            }],
        };
        let layout = AovLayout::new(&request);
        let cam = CameraFrame {
            origin: Vec3A::ZERO,
            u: Vec3A::X,
            v: Vec3A::Y,
            w: Vec3A::Z,
        };
        let mut unit = UnitAov::new(&layout, cam, 1);
        let near = FirstHit::Volume {
            p: Vec3A::new(0.0, 0.0, -1.0),
        };
        let far = FirstHit::Volume {
            p: Vec3A::new(0.0, 0.0, -7.0),
        };
        unit.add(&near, &NONE, 1.6, 0.5, 1.0);
        unit.add(&far, &NONE, 1.2, 0.5, 1.0);
        let mut film = AovFilm::new(&layout, 1, 1);
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
            }],
        };
        let layout = AovLayout::new(&request);
        let cam = CameraFrame {
            origin: Vec3A::ZERO,
            u: Vec3A::X,
            v: Vec3A::Y,
            w: Vec3A::Z,
        };
        let surface = FirstHit::Surface {
            p: Vec3A::new(0.0, 0.0, -1.0),
            n: Vec3A::Z,
            uv: None,
        };
        let mut unit = UnitAov::new(&layout, cam, 1);
        unit.add(&surface, &NONE, 0.5, 0.5, 3.0);
        unit.add(&FirstHit::Escaped, &NONE, 0.5, 0.5, 1.0);
        let mut film = AovFilm::new(&layout, 1, 1);
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
            }],
        };
        let layout = AovLayout::new(&request);
        let cam = CameraFrame {
            origin: Vec3A::ZERO,
            u: Vec3A::X,
            v: Vec3A::Y,
            w: Vec3A::Z,
        };
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
        let mut film = AovFilm::new(&layout, 2, 1);
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
            }],
        };
        let layout = AovLayout::new(&request);
        let cam = CameraFrame {
            origin: Vec3A::ZERO,
            u: Vec3A::X,
            v: Vec3A::Y,
            w: Vec3A::Z,
        };
        let unit = UnitAov::new(&layout, cam, 1);
        let mut one_spp = AovFilm::new(&layout, 1, 1);
        one_spp.store(&unit, 0, 0, 0, 1.0, 1, f64::INFINITY);
        let mut trained = AovFilm::new(&layout, 1, 1);
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
            }],
        };
        assert!(!request.needs_film());
        beauty.components = 4;
        request.products[0].vars[0] = beauty;
        assert!(request.needs_film(), "color4f needs the alpha slot");
        assert!(!AovRequest::default().needs_film());
    }
}
