//! Coded import warnings: the vocabulary, the scoped collector the import
//! records into, and the macros every import-time warning is raised through.
//!
//! Each warning the USD import raises has a [`WarningCode`] — a stable
//! `<domain>.<cause>` string, one per *cause* rather than per call site — and
//! one fixed [`WarningKind`]. The codes are public: they appear in the log
//! (`WARN [light.degenerate_shape] RectLight at …`), on
//! [`Scene::warnings`](crate::Scene::warnings), and in every report that
//! carries warnings. Adding a code is compatible; renaming or removing one,
//! or changing its kind, is a breaking change to every such report. Every
//! code is listed in the user documentation's warnings reference
//! (`site/content/docs/reference/warnings.md`), which a test keeps in step
//! with [`WarningCode::ALL`].
//!
//! # Collecting
//!
//! `load_scene` enters a [`WarningScope`] on the importing thread. While it is
//! active, [`warning!`](crate::warning) logs per the code's [`LogPolicy`] and
//! also records the occurrence: one [`Warning`] per code, every occurrence
//! counted, the first [`MAX_PRIMS`] distinct prims kept, in first-fire order.
//! Outside a scope the macros only log.
//!
//! The collector is a thread-local, like the import's evaluation time
//! (`usd_import/time.rs`), and sound for the same reason: every import-time
//! warning is raised on the importing thread. A warning raised from a rayon
//! task during an import is logged but not recorded; in a debug build it
//! panics instead, so the mistake is found by the tests rather than by a
//! report that silently misses it.
//!
//! # The three macros
//!
//! - [`warning!`](crate::warning)`(Code, at = prim, "fmt", args…)` — the
//!   ordinary case: counts, keeps the prim, logs per policy. `at = …` is
//!   optional; a stage-level warning counts without a prim.
//! - [`record_warning!`](crate::record_warning)`(Code, at = prim, "fmt", …)` —
//!   counts and keeps the prim without logging, for an occurrence whose cause
//!   was already logged (a memoized asset failure). The message is formatted
//!   only when the record has none yet.
//! - [`cause_warning!`](crate::cause_warning)`(Code, "fmt", …)` — logs the
//!   coded line and supplies the record's message without counting: the
//!   asset loader explaining *why* a file failed, once per file, while the
//!   core counts each reference to it.

use std::cell::{Cell, RefCell};
use std::fmt::{self, Display};
use std::marker::PhantomData;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The most distinct prims a record keeps. Enough to locate a pattern; the
/// count gives its scale.
pub const MAX_PRIMS: usize = 16;

/// What a warning says happened to something authored.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WarningKind {
    /// An invalid authored value (non-finite, out of range, unknown token,
    /// wrong type) was replaced by a fallback.
    Refused,
    /// A valid authored value is rendered differently from what it asks for.
    Approximated,
    /// Something authored (a prim, an asset, an output channel) contributes
    /// nothing; a constant or default may stand in for it.
    Skipped,
}

impl WarningKind {
    /// Every kind, in the order reports total them.
    pub const ALL: [WarningKind; 3] = [
        WarningKind::Refused,
        WarningKind::Approximated,
        WarningKind::Skipped,
    ];

    /// The kind as reports and the reference page spell it.
    pub fn as_str(self) -> &'static str {
        match self {
            WarningKind::Refused => "refused",
            WarningKind::Approximated => "approximated",
            WarningKind::Skipped => "skipped",
        }
    }
}

impl Display for WarningKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How often a code is logged. Recording is unaffected: every occurrence is
/// counted either way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogPolicy {
    /// Every occurrence is logged.
    Each,
    /// The first occurrence in an import is logged, the rest only recorded —
    /// for a cause that fires per mesh and would otherwise scale the log with
    /// the scene. Outside an import, once per thread.
    Once,
}

/// Declares the vocabulary: one line per code. Generates [`WarningCode`],
/// its accessors and [`WarningCode::ALL`].
macro_rules! warning_codes {
    ($($(#[$meta:meta])* $variant:ident => $code:literal, $kind:ident, $policy:ident, $doc:literal;)+) => {
        /// A stable import-warning code: one per cause. See the module
        /// documentation for the compatibility rules.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum WarningCode {
            $($(#[$meta])* #[doc = $doc] $variant,)+
        }

        impl WarningCode {
            /// Every code, in table order.
            pub const ALL: &'static [WarningCode] = &[$(WarningCode::$variant,)+];

            /// The code's `<domain>.<cause>` string.
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(WarningCode::$variant => $code,)+
                }
            }

            /// The code's one fixed kind.
            pub const fn kind(self) -> WarningKind {
                match self {
                    $(WarningCode::$variant => WarningKind::$kind,)+
                }
            }

            /// How often the code is logged.
            pub const fn log_policy(self) -> LogPolicy {
                match self {
                    $(WarningCode::$variant => LogPolicy::$policy,)+
                }
            }

            /// One line on what the code means.
            pub const fn doc(self) -> &'static str {
                match self {
                    $(WarningCode::$variant => $doc,)+
                }
            }
        }
    };
}

warning_codes! {
    // Render settings
    SettingsInvalidValue => "settings.invalid_value", Refused, Each,
        "A crust:* render setting has a value outside its domain; its default is used.";
    SettingsLightSamplesClamped => "settings.light_samples_clamped", Approximated, Each,
        "A light-sample count is above the most crust takes; the maximum is used.";
    TimeOutsideRange => "time.outside_range", Approximated, Each,
        "The frame lies outside the stage's time range; animated attributes hold their nearest sample.";
    ColorWorkingSpaceRefused => "color.working_space_refused", Refused, Each,
        "renderingColorSpace names no usable working space; the render is in lin_rec709.";
    ColorUnknownSpace => "color.unknown_space", Refused, Each,
        "A colour space is not defined by the OCIO config; the value is used as stored.";
    ColorNoConversion => "color.no_conversion", Approximated, Each,
        "Two colour spaces have no conversion between them; values are used as stored.";
    ColorNoLuminance => "color.no_luminance", Approximated, Each,
        "The working space has no RGB to XYZ matrix; colours are weighed by Rec.709 luminance.";

    // Camera
    CameraNotACamera => "camera.not_a_camera", Refused, Each,
        "RenderSettings.camera names a prim that is not a camera; the first camera met is used.";
    CameraMissing => "camera.missing", Skipped, Each,
        "The stage authors no camera; the procedural default camera is used.";
    CameraInvalidExposure => "camera.invalid_exposure", Refused, Each,
        "The render camera's exposure attributes give a scale that is not finite or not positive; the image is not scaled.";
    CameraUnreadable => "camera.unreadable", Skipped, Each,
        "A camera prim could not be built into a camera.";

    // Products, AOVs and light path expressions
    ProductNotARenderProduct => "product.not_a_render_product", Skipped, Each,
        "RenderSettings.products targets a prim that is not a RenderProduct.";
    ProductUnsupportedType => "product.unsupported_type", Skipped, Each,
        "A RenderProduct's productType is not raster; no file is written for it.";
    ProductCameraMismatch => "product.camera_mismatch", Skipped, Each,
        "A product renders through another camera or resolution than the first; no file is written for it.";
    ProductMotionBlurMismatch => "product.motion_blur_mismatch", Approximated, Each,
        "A product asks for other motion blur than the first; its file is written with the first's.";
    ProductRegionMismatch => "product.region_mismatch", Approximated, Each,
        "A product asks for another dataWindowNDC than the first; its file is written over the first's.";
    ProductUnhonouredAttribute => "product.unhonoured_attribute", Approximated, Each,
        "A product authors a setting crust does not honour; it renders without it.";
    ProductInvalidDataWindow => "product.invalid_data_window", Refused, Each,
        "dataWindowNDC is not finite or selects no pixel; the full frame is rendered.";
    ProductDataWindowClipped => "product.data_window_clipped", Approximated, Each,
        "dataWindowNDC reaches outside the frame; overscan is not supported, so it is clipped.";
    ProductNoName => "product.no_name", Skipped, Each,
        "A RenderProduct authors no productName; nothing is written for it.";
    ProductNoWritableVars => "product.no_writable_vars", Skipped, Each,
        "A RenderProduct has no RenderVar crust can write; nothing is written for it.";
    ProductSharedPath => "product.shared_path", Skipped, Each,
        "A RenderProduct writes the path an earlier one already writes; nothing is written for it.";
    AovNotARenderVar => "aov.not_a_render_var", Skipped, Each,
        "orderedVars targets a prim that is not a RenderVar.";
    AovUnsupportedSource => "aov.unsupported_source", Skipped, Each,
        "A RenderVar's source is one crust does not produce yet (planned, primvar, intrinsic); no channel is written.";
    AovUnknownSource => "aov.unknown_source", Skipped, Each,
        "A RenderVar names an unknown source or sourceType; no channel is written.";
    AovTypeMismatch => "aov.type_mismatch", Skipped, Each,
        "A RenderVar's data type is not numeric or cannot hold its source; no channel is written.";
    AovUnknownAccumulation => "aov.unknown_accumulation", Refused, Each,
        "A RenderVar's accumulation token is not supported; the source's default is used.";
    AovAccumulationIgnored => "aov.accumulation_ignored", Approximated, Each,
        "A per-pixel source cannot be accumulated as authored; its default accumulation is used.";
    AovInvalidVariance => "aov.invalid_variance", Skipped, Each,
        "crust:aov:variance is authored where it cannot apply; no channel is written.";
    AovRawIgnored => "aov.raw_ignored", Approximated, Each,
        "crust:aov:raw is authored on a source that is not a light path expression; it is ignored.";
    AovInvalidRaw => "aov.invalid_raw", Skipped, Each,
        "crust:aov:raw needs an expression whose every path starts with a diffuse reflection; no channel is written.";
    LpeInvalid => "lpe.invalid", Skipped, Each,
        "A light path expression does not parse or compile; no channel is written.";

    // Transforms
    XformUnknownOp => "xform.unknown_op", Approximated, Each,
        "xformOpOrder lists an op that is not a UsdGeomXformOp kind; it reads as identity.";
    XformUncomposable => "xform.uncomposable", Refused, Each,
        "An xformOp stack could not be composed; the local transform is identity.";
    XformUnknownType => "xform.unknown_type", Skipped, Once,
        "A prim of a type the schema registry does not know authors xformOps; it is not Xformable, so they are ignored.";
    XformMotionVectorUnsupported => "xform.motion_vector_unsupported", Approximated, Each,
        "Geometry moves other than by a translation; it is motion blurred but the motionvector AOV reads zero.";

    // Meshes, subdivision, displacement
    MeshNonInvertibleTransform => "mesh.non_invertible_transform", Approximated, Each,
        "A mesh's transform is not invertible; it is baked instead of instanced.";
    MeshMotionIgnored => "mesh.motion_ignored", Skipped, Each,
        "crust:motion:translate on baked (non-invertible) geometry is ignored.";
    MeshInvalidUvs => "mesh.invalid_uvs", Skipped, Each,
        "A subdivided mesh's texture coordinates do not index cleanly; it renders without them.";
    MeshPtexFaceMismatch => "mesh.ptex_face_mismatch", Approximated, Each,
        "A mesh's face count differs from its per-face texture's; its shading is wrong.";
    MeshDisplacedAtCage => "mesh.displaced_at_cage", Approximated, Once,
        "A displaced mesh is not refined, so only its cage vertices move.";
    MeshPtexDisplacedAtCage => "mesh.ptex_displaced_at_cage", Approximated, Once,
        "A subdivisionScheme = none mesh with Ptex displacement and non-quad faces is displaced at its cage.";
    MeshDisplacementExceedsBound => "mesh.displacement_exceeds_bound", Approximated, Each,
        "Displacement reaches past crust:displacementBound; it is applied unclamped.";
    SubdivInvalidSetting => "subdiv.invalid_setting", Refused, Each,
        "A subdivision level is negative or an edge length is not a positive pixel length; it is clamped or ignored.";
    SubdivLevelClamped => "subdiv.level_clamped", Approximated, Each,
        "A subdivision level is above the most crust refines to; it is clamped.";
    SubdivAdaptiveNeedsCamera => "subdiv.adaptive_needs_camera", Approximated, Each,
        "Adaptive subdivision has no render camera to measure from; the uniform level is used.";
    SubdivLegacyLevel => "subdiv.legacy_level", Skipped, Once,
        "The per-prim crust:subdivisionLevel is no longer read.";
    SubdivLoopNeedsTriangles => "subdiv.loop_needs_triangles", Approximated, Each,
        "subdivisionScheme = loop on a mesh with non-triangle faces; the base cage is rendered.";
    SubdivLoopPtex => "subdiv.loop_ptex", Approximated, Each,
        "subdivisionScheme = loop with a per-face texture cannot keep its face ids; the base cage is rendered.";
    SubdivFailed => "subdiv.failed", Approximated, Each,
        "Subdivision or per-face tessellation failed; the base cage is rendered.";
    DisplacementVectorIgnored => "displacement.vector_ignored", Approximated, Each,
        "Vector displacement is not applied.";
    DisplacementUnevaluated => "displacement.unevaluated", Skipped, Each,
        "The displacement amount is driven by a shader crust does not evaluate; the surface is not displaced.";

    // Curves and volumes
    CurvesUnsupportedBasis => "curves.unsupported_basis", Skipped, Each,
        "BasisCurves uses a basis crust does not support.";
    CurvesInvalidCounts => "curves.invalid_counts", Skipped, Each,
        "curveVertexCounts overruns the points; the remaining curves are skipped.";
    CurvesNonInvertibleTransform => "curves.non_invertible_transform", Skipped, Each,
        "BasisCurves has a non-invertible transform.";
    VolumeUnknownType => "volume.unknown_type", Skipped, Each,
        "crust:volume:type is not homogeneous, smoke or grid.";
    VolumeInvalidGrid => "volume.invalid_grid", Skipped, Each,
        "A grid volume's gridDims and gridData are missing or do not match.";
    VolumeInPrototype => "volume.in_prototype", Skipped, Each,
        "A volume inside an instance prototype; volumes cannot be instanced.";

    // Instancing
    InstancingInstanceableWithoutPrototype => "instancing.instanceable_without_prototype", Approximated, Each,
        "An instanceable prim has no prototype; it is imported directly.";
    InstancingNestingTooDeep => "instancing.nesting_too_deep", Skipped, Each,
        "A prototype nests instances past the supported depth; the deeper levels are not expanded.";
    InstancingEmptyPrototype => "instancing.empty_prototype", Skipped, Each,
        "A prototype contributed no geometry.";
    InstancingNoPrototypes => "instancing.no_prototypes", Skipped, Each,
        "A PointInstancer has no prototypes targets.";
    InstancingNoProtoIndices => "instancing.no_proto_indices", Skipped, Each,
        "A PointInstancer has no protoIndices.";
    InstancingMissingPositions => "instancing.missing_positions", Skipped, Each,
        "A PointInstancer has fewer positions than protoIndices; the extra instances are skipped.";
    InstancingProtoIndexOutOfRange => "instancing.proto_index_out_of_range", Skipped, Each,
        "A PointInstancer's protoIndices entry names no prototype; that instance is skipped.";

    // Lights and light linking
    LightNonFiniteInput => "light.non_finite_input", Refused, Each,
        "A light input is not finite; its fallback is used.";
    LightUnsupportedMultiplier => "light.unsupported_multiplier", Approximated, Each,
        "A per-lobe light multiplier (diffuse, specular) is not supported; the light contributes at 1.0.";
    LightDegenerateShape => "light.degenerate_shape", Skipped, Each,
        "A light's size or transform collapses its shape; the light is skipped.";
    LightUnsupportedTextureFormat => "light.unsupported_texture_format", Skipped, Each,
        "A DomeLight's texture:format is not latlong; it emits its uniform colour.";
    LightMapUnreadable => "light.map_unreadable", Skipped, Each,
        "A light's or dome's texture could not be loaded; it emits its uniform colour.";
    IesUnreadable => "ies.unreadable", Skipped, Each,
        "An IES profile could not be loaded; the light renders without it.";
    LightLinkMembershipExpression => "light_link.membership_expression", Approximated, Each,
        "A light-link collection authors membershipExpression, which is not read; it includes every prim.";
    LightLinkUnreadableCollection => "light_link.unreadable_collection", Refused, Each,
        "A light-link collection cannot be read; it includes every prim.";
    LightLinkTargetInInstance => "light_link.target_in_instance", Approximated, Each,
        "A light-link collection targets a prim inside an instance; membership is judged on the instance.";
    LightLinkNestedNotComposed => "light_link.nested_not_composed", Skipped, Each,
        "A nested collection lies outside the streamed chunk that reads it; it contributes nothing.";
    LightLinkTooManyClasses => "light_link.too_many_classes", Skipped, Each,
        "The scene needs more light-link classes than crust encodes; light links are ignored.";
    LightLinkRayMaskRewritten => "light_link.ray_mask_rewritten", Approximated, Each,
        "Shadow linking rewrites the authored crust:rayMask bits 3-31.";
    LightLinkShadowLinkUnencodable => "light_link.shadow_link_unencodable", Approximated, Each,
        "A shadowLink collection cannot be encoded; the light is shadowed by every occluder.";

    // Materials
    MaterialFallbackDefault => "material.fallback_default", Skipped, Each,
        "A material cannot be resolved to a shader crust reads; the default grey OpenPBR is used.";
    MaterialVolumeIgnored => "material.volume_ignored", Skipped, Each,
        "A MaterialX volume terminal beside a non-MaterialX surface is ignored.";
    MtlxIncomplete => "mtlx.incomplete", Approximated, Each,
        "Part of a MaterialX network is not represented; those inputs fall back to their defaults.";
    MtlxUnusable => "mtlx.unusable", Skipped, Each,
        "A MaterialX document or network cannot be used; the material falls back.";
    PreviewMultiplePrimvars => "preview.multiple_primvars", Approximated, Each,
        "A UsdPreviewSurface's textures read several primvars; the first is read for all.";
    PreviewUnsupportedConnection => "preview.unsupported_connection", Skipped, Each,
        "A UsdPreviewSurface input connects to something other than a UsdUVTexture output; its constant is used.";
    PreviewTextureAlpha => "preview.texture_alpha", Approximated, Each,
        "A UsdPreviewSurface input reads texture alpha, which reads 1.0.";
    PreviewTextureWithoutFile => "preview.texture_without_file", Skipped, Each,
        "A UsdUVTexture has no inputs:file; the input keeps its constant.";
    PreviewUnreadSt => "preview.unread_st", Approximated, Each,
        "A UsdUVTexture's st is driven by a shader crust does not read; the mesh chart is used unchanged.";

    // Assets
    AssetUnsupportedByHost => "asset.unsupported_by_host", Skipped, Each,
        "The host's asset loader does not decode this type of asset.";
    TextureUnreadable => "texture.unreadable", Skipped, Each,
        "A texture could not be loaded; the input reads its fallback.";
    TextureUdimTileMissing => "texture.udim_tile_missing", Skipped, Each,
        "A tile of a UDIM set does not decode; the set is used without it.";
    TextureTxStale => "texture.tx_stale", Approximated, Each,
        "A .tx is older than its source and is used anyway.";
    TextureTxConvertFailed => "texture.tx_convert_failed", Approximated, Each,
        "--auto-tx could not convert a texture; its source is read instead.";
    TextureStreamFallback => "texture.stream_fallback", Approximated, Each,
        "A texture could not be streamed and is preloaded instead.";
}

impl WarningCode {
    fn index(self) -> usize {
        self as usize
    }
}

impl Display for WarningCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl serde::Serialize for WarningCode {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

/// One code's record from an import: every occurrence counted, the first
/// [`MAX_PRIMS`] distinct prims kept.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Warning {
    pub code: WarningCode,
    pub kind: WarningKind,
    /// How many times the code fired.
    pub count: u64,
    /// The distinct prims it fired on, in first-occurrence order, at most
    /// [`MAX_PRIMS`]. Empty for a stage-level warning.
    pub prims: Vec<String>,
    /// The first occurrence's text, without the `[code]` prefix.
    pub message: String,
}

/// The records of one scope, in first-fire order, and where each code's
/// record sits.
#[derive(Default)]
struct Collector {
    records: Vec<Warning>,
    slot: Vec<Option<usize>>,
    /// Messages a loader explained before any reference counted them
    /// ([`cause_warning!`](crate::cause_warning)).
    pending: Vec<(WarningCode, String)>,
}

impl Collector {
    fn record_mut(&mut self, code: WarningCode) -> Option<&mut Warning> {
        let i = (*self.slot.get(code.index())?)?;
        Some(&mut self.records[i])
    }
}

thread_local! {
    static COLLECTOR: RefCell<Option<Collector>> = const { RefCell::new(None) };
    /// The [`LogPolicy::Once`] flags outside any scope.
    static LOGGED_OUTSIDE: RefCell<Vec<bool>> = const { RefCell::new(Vec::new()) };
    /// Whether this thread has a scope, read without borrowing the collector.
    static IN_SCOPE: Cell<bool> = const { Cell::new(false) };
    /// How many [`cause_warning!`](crate::cause_warning)s this thread has
    /// raised, scope or not ([`causes_raised`]).
    static CAUSES: Cell<u64> = const { Cell::new(0) };
}

/// How many causes this thread has explained so far. A host load that comes
/// back `None` with this unchanged declined on purpose (a switch turned it
/// off, or the host does not decode that asset) rather than failing to read
/// the file — the importer counts only the failures.
pub(crate) fn causes_raised() -> u64 {
    CAUSES.with(Cell::get)
}

/// Whether this thread is collecting an import's warnings — for a site met
/// far more often outside an import than in one (a cache hit on every
/// decoded texel) that should record only an import's occurrences.
pub(crate) fn in_scope() -> bool {
    IN_SCOPE.with(Cell::get)
}

#[doc(hidden)]
pub fn __caused() {
    CAUSES.with(|c| c.set(c.get() + 1));
}

/// Scopes active on any thread — for the debug check that no recording macro
/// runs on a rayon worker while an import elsewhere expects to collect it.
static ACTIVE_SCOPES: AtomicUsize = AtomicUsize::new(0);

/// Collects the warnings raised on this thread while it lives, and restores
/// whatever collector was there before when it ends — on [`finish`] or on
/// drop, including an early `?` return.
///
/// `!Send`: it restores a thread-local, so it must end on the thread that
/// entered it. The marker makes moving it into a rayon task a compile error.
///
/// [`finish`]: WarningScope::finish
#[must_use = "warnings are collected only while the scope lives"]
pub struct WarningScope {
    previous: Option<Option<Collector>>,
    _not_send: PhantomData<*const ()>,
}

impl WarningScope {
    /// Starts collecting on this thread.
    pub fn enter() -> WarningScope {
        let previous = COLLECTOR.with(|c| c.replace(Some(Collector::default())));
        IN_SCOPE.with(|s| s.set(true));
        ACTIVE_SCOPES.fetch_add(1, Ordering::Relaxed);
        WarningScope {
            previous: Some(previous),
            _not_send: PhantomData,
        }
    }

    /// Stops collecting and returns the records, in first-fire order.
    pub fn finish(mut self) -> Vec<Warning> {
        self.restore().map(|c| c.records).unwrap_or_default()
    }

    fn restore(&mut self) -> Option<Collector> {
        let previous = self.previous.take()?;
        ACTIVE_SCOPES.fetch_sub(1, Ordering::Relaxed);
        IN_SCOPE.with(|s| s.set(previous.is_some()));
        COLLECTOR.with(|c| c.replace(previous))
    }
}

impl Drop for WarningScope {
    fn drop(&mut self) {
        self.restore();
    }
}

/// What a site does after [`__occur`]: whether to log, and whether the
/// record wants its message.
#[doc(hidden)]
#[derive(Clone, Copy)]
pub struct __Emit {
    pub log: bool,
    pub message: bool,
}

#[cfg(debug_assertions)]
fn check_thread() {
    if !IN_SCOPE.with(Cell::get)
        && ACTIVE_SCOPES.load(Ordering::Relaxed) > 0
        && rayon::current_thread_index().is_some()
    {
        panic!(
            "an import warning was raised on a rayon worker while an import was collecting \
             warnings on another thread: it would be logged but not recorded. Raise it on \
             the importing thread (docs/architecture.md § Invariants)"
        );
    }
}

#[cfg(not(debug_assertions))]
#[inline(always)]
fn check_thread() {}

/// Counts one occurrence of `code` on `prim`. `log` is whether the site logs
/// at all (`false` for [`record_warning!`](crate::record_warning)).
#[doc(hidden)]
pub fn __occur(code: WarningCode, prim: Option<&dyn Display>, log: bool) -> __Emit {
    check_thread();
    let recorded = COLLECTOR.with(|c| {
        let mut c = c.borrow_mut();
        let c = c.as_mut()?;
        let first = c.record_mut(code).is_none();
        if first {
            if c.slot.len() <= code.index() {
                c.slot.resize(code.index() + 1, None);
            }
            c.slot[code.index()] = Some(c.records.len());
            let message = c
                .pending
                .iter()
                .position(|(p, _)| *p == code)
                .map(|i| c.pending.swap_remove(i).1)
                .unwrap_or_default();
            c.records.push(Warning {
                code,
                kind: code.kind(),
                count: 0,
                prims: Vec::new(),
                message,
            });
        }
        let record = c.record_mut(code).expect("inserted above");
        record.count += 1;
        if let Some(prim) = prim
            && record.prims.len() < MAX_PRIMS
        {
            let prim = prim.to_string();
            if !record.prims.contains(&prim) {
                record.prims.push(prim);
            }
        }
        Some((first, record.message.is_empty()))
    });
    let log = log
        && match code.log_policy() {
            LogPolicy::Each => true,
            LogPolicy::Once => match recorded {
                Some((first, _)) => first,
                None => LOGGED_OUTSIDE.with(|l| {
                    let mut l = l.borrow_mut();
                    if l.len() <= code.index() {
                        l.resize(code.index() + 1, false);
                    }
                    !std::mem::replace(&mut l[code.index()], true)
                }),
            },
        };
    __Emit {
        log,
        message: recorded.is_some_and(|(_, empty)| empty),
    }
}

/// Gives `code`'s record its message, if it has none: on the record when it
/// exists, else held until the first occurrence creates it.
#[doc(hidden)]
pub fn __set_message(code: WarningCode, message: String) {
    COLLECTOR.with(|c| {
        let mut c = c.borrow_mut();
        let Some(c) = c.as_mut() else { return };
        if let Some(r) = c.record_mut(code) {
            if r.message.is_empty() {
                r.message = message;
            }
        } else if !c.pending.iter().any(|(p, _)| *p == code) {
            c.pending.push((code, message));
        }
    });
}

/// Whether [`cause_warning!`](crate::cause_warning) has a record or a pending
/// slot to give a message to — so it formats only when one wants it.
#[doc(hidden)]
pub fn __wants_message(code: WarningCode) -> bool {
    COLLECTOR.with(|c| {
        let c = c.borrow();
        let Some(c) = c.as_ref() else { return false };
        match c.slot.get(code.index()).copied().flatten() {
            Some(i) => c.records[i].message.is_empty(),
            None => !c.pending.iter().any(|(p, _)| *p == code),
        }
    })
}

#[doc(hidden)]
pub use tracing as __tracing;

/// Raises a coded import warning: counts it, keeps the prim, and logs
/// `[code] message` per the code's [`LogPolicy`].
///
/// ```ignore
/// warning!(LightDegenerateShape, at = prim.path(), "RectLight at {}: … skipped", prim.path());
/// warning!(TimeOutsideRange, "Frame {time} is outside …");
/// ```
#[macro_export]
macro_rules! warning {
    ($code:ident, at = $prim:expr, $($fmt:tt)+) => {{
        let __prim = &$prim;
        $crate::__warning_emit!($code, Some(__prim as &dyn ::std::fmt::Display), true, $($fmt)+)
    }};
    ($code:ident, $($fmt:tt)+) => {
        $crate::__warning_emit!($code, None, true, $($fmt)+)
    };
}

/// Counts a coded warning and keeps its prim without logging it — for an
/// occurrence whose cause has already been logged (see
/// [`cause_warning!`](crate::cause_warning)). The message is formatted only
/// when the record has none yet.
#[macro_export]
macro_rules! record_warning {
    ($code:ident, at = $prim:expr, $($fmt:tt)+) => {{
        let __prim = &$prim;
        $crate::__warning_emit!($code, Some(__prim as &dyn ::std::fmt::Display), false, $($fmt)+)
    }};
    ($code:ident, $($fmt:tt)+) => {
        $crate::__warning_emit!($code, None, false, $($fmt)+)
    };
}

/// Logs a coded warning's cause and gives the record its message, without
/// counting: the occurrences are counted where the failure is met (with
/// [`record_warning!`](crate::record_warning)).
#[macro_export]
macro_rules! cause_warning {
    ($code:ident, $($fmt:tt)+) => {{
        let __code = $crate::warnings::WarningCode::$code;
        let __message = ::std::format!($($fmt)+);
        $crate::warnings::__tracing::warn!("[{}] {}", __code.as_str(), __message);
        $crate::warnings::__caused();
        if $crate::warnings::__wants_message(__code) {
            $crate::warnings::__set_message(__code, __message);
        }
    }};
}

#[doc(hidden)]
#[macro_export]
macro_rules! __warning_emit {
    ($code:ident, $prim:expr, $log:expr, $($fmt:tt)+) => {{
        let __code = $crate::warnings::WarningCode::$code;
        let __emit = $crate::warnings::__occur(__code, $prim, $log);
        if __emit.log || __emit.message {
            let __message = ::std::format!($($fmt)+);
            if __emit.log {
                $crate::warnings::__tracing::warn!("[{}] {}", __code.as_str(), __message);
            }
            if __emit.message {
                $crate::warnings::__set_message(__code, __message);
            }
        }
    }};
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// The WARN lines a closure logs, through a subscriber local to it.
    fn logged(f: impl FnOnce()) -> Vec<String> {
        #[derive(Clone, Default)]
        struct Lines(Arc<Mutex<Vec<String>>>);
        struct Visit<'a>(&'a mut String);
        impl tracing::field::Visit for Visit<'_> {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
                if field.name() == "message" {
                    *self.0 = format!("{value:?}");
                }
            }
        }
        impl tracing::Subscriber for Lines {
            fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
                true
            }
            fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
                tracing::span::Id::from_u64(1)
            }
            fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
            fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
            fn event(&self, event: &tracing::Event<'_>) {
                if *event.metadata().level() == tracing::Level::WARN {
                    let mut line = String::new();
                    event.record(&mut Visit(&mut line));
                    self.0.lock().unwrap().push(line);
                }
            }
            fn enter(&self, _: &tracing::span::Id) {}
            fn exit(&self, _: &tracing::span::Id) {}
        }
        let lines = Lines::default();
        tracing::subscriber::with_default(lines.clone(), f);
        lines.0.lock().unwrap().clone()
    }

    #[test]
    fn codes_are_well_formed_and_unique() {
        let mut seen = std::collections::HashSet::new();
        for code in WarningCode::ALL {
            let s = code.as_str();
            let (domain, cause) = s.split_once('.').expect("<domain>.<cause>");
            let word =
                |w: &str| !w.is_empty() && w.chars().all(|c| c.is_ascii_lowercase() || c == '_');
            assert!(word(domain) && word(cause), "{s} is not [a-z_]+.[a-z_]+");
            assert!(seen.insert(s), "{s} is listed twice");
            assert!(!code.doc().is_empty(), "{s} has no documentation");
        }
        // `ALL` is in declaration order, which `index` relies on.
        for (i, code) in WarningCode::ALL.iter().enumerate() {
            assert_eq!(code.index(), i);
        }
    }

    /// The rows of the warnings reference page: `(code, kind)` from each
    /// table line that starts with a backquoted code.
    fn reference_rows(page: &str) -> Vec<(String, String)> {
        page.lines()
            .filter_map(|l| l.strip_prefix("| `"))
            .map(|l| {
                let mut cells = l.split('|');
                let code = cells
                    .next()
                    .unwrap()
                    .trim()
                    .trim_end_matches('`')
                    .to_owned();
                let kind = cells.next().unwrap_or_default().trim().to_owned();
                (code, kind)
            })
            .collect()
    }

    /// The user documentation lists exactly the codes crust can raise, each
    /// with its kind — so a code cannot be added without documenting it.
    #[test]
    fn the_reference_page_lists_every_code() {
        let page = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../site/content/docs/reference/warnings.md"
        ))
        .expect("site/content/docs/reference/warnings.md");
        let documented = reference_rows(&page);
        let expected: Vec<(String, String)> = WarningCode::ALL
            .iter()
            .map(|c| (c.as_str().to_owned(), c.kind().as_str().to_owned()))
            .collect();
        for row in &expected {
            assert!(
                documented.contains(row),
                "{row:?} is not on the reference page"
            );
        }
        for row in &documented {
            assert!(
                expected.contains(row),
                "the reference page lists {row:?}, which crust cannot raise"
            );
        }
        assert_eq!(documented.len(), expected.len(), "a code is listed twice");
    }

    #[test]
    fn reference_rows_detect_a_missing_code() {
        let page = "| `light.degenerate_shape` | skipped | x | y |\n";
        assert_eq!(
            reference_rows(page),
            [("light.degenerate_shape".to_owned(), "skipped".to_owned())]
        );
        assert!(reference_rows("no table here").is_empty());
    }

    #[test]
    fn counts_every_occurrence_and_caps_prims() {
        let scope = WarningScope::enter();
        let lines = logged(|| {
            for i in 0..20 {
                warning!(
                    LightDegenerateShape,
                    at = format!("/l{i}"),
                    "light {i} skipped"
                );
            }
            // A repeated prim is counted, not listed twice.
            warning!(LightDegenerateShape, at = "/l0", "again");
        });
        let records = scope.finish();
        assert_eq!(records.len(), 1);
        let r = &records[0];
        assert_eq!(r.code, WarningCode::LightDegenerateShape);
        assert_eq!(r.kind, WarningKind::Skipped);
        assert_eq!(r.count, 21);
        assert_eq!(r.prims.len(), MAX_PRIMS);
        assert_eq!(r.prims[0], "/l0");
        assert_eq!(r.prims[15], "/l15");
        assert_eq!(r.message, "light 0 skipped");
        assert_eq!(lines.len(), 21, "Each logs every occurrence");
        assert_eq!(lines[0], "[light.degenerate_shape] light 0 skipped");
    }

    #[test]
    fn records_in_first_fire_order_and_stage_level_has_no_prims() {
        let scope = WarningScope::enter();
        warning!(TimeOutsideRange, "frame 9");
        warning!(LightNonFiniteInput, at = "/a", "a");
        warning!(TimeOutsideRange, "frame 9 again");
        let records = scope.finish();
        let codes: Vec<_> = records.iter().map(|r| r.code).collect();
        assert_eq!(
            codes,
            [
                WarningCode::TimeOutsideRange,
                WarningCode::LightNonFiniteInput
            ]
        );
        assert_eq!(records[0].count, 2);
        assert!(records[0].prims.is_empty());
        assert_eq!(records[0].message, "frame 9");
    }

    #[test]
    fn once_logs_once_but_counts_all() {
        let scope = WarningScope::enter();
        let lines = logged(|| {
            for i in 0..3 {
                warning!(MeshDisplacedAtCage, at = format!("/m{i}"), "mesh {i}");
            }
        });
        let records = scope.finish();
        assert_eq!(lines, ["[mesh.displaced_at_cage] mesh 0"]);
        assert_eq!(records[0].count, 3);
        assert_eq!(records[0].prims, ["/m0", "/m1", "/m2"]);
        // A new scope is a new import: it logs again.
        let scope = WarningScope::enter();
        let lines = logged(|| warning!(MeshDisplacedAtCage, at = "/m", "again"));
        drop(scope);
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn once_outside_a_scope_logs_once_per_thread() {
        std::thread::spawn(|| {
            let lines = logged(|| {
                for _ in 0..3 {
                    warning!(SubdivLegacyLevel, at = "/m", "legacy");
                }
            });
            assert_eq!(lines.len(), 1);
        })
        .join()
        .unwrap();
    }

    #[test]
    fn nested_scope_restores_the_outer_one() {
        let outer = WarningScope::enter();
        warning!(TimeOutsideRange, "outer");
        {
            let inner = WarningScope::enter();
            warning!(CameraMissing, "inner");
            let inner = inner.finish();
            assert_eq!(inner.len(), 1);
            assert_eq!(inner[0].code, WarningCode::CameraMissing);
        }
        warning!(TimeOutsideRange, "outer again");
        let outer = outer.finish();
        assert_eq!(
            outer.len(),
            1,
            "the inner scope's warning is not the outer's"
        );
        assert_eq!(outer[0].count, 2);
    }

    #[test]
    fn a_dropped_scope_leaves_no_collector() {
        fn early_return() -> Result<(), ()> {
            let _scope = WarningScope::enter();
            warning!(TimeOutsideRange, "inside");
            Err(())
        }
        assert!(early_return().is_err());
        // No collector: nothing is recorded, and a fresh scope starts empty.
        warning!(TimeOutsideRange, "outside");
        assert!(COLLECTOR.with(|c| c.borrow().is_none()));
        assert!(WarningScope::enter().finish().is_empty());
    }

    #[test]
    fn cause_sets_the_message_without_counting() {
        let scope = WarningScope::enter();
        let lines = logged(|| {
            // The loader explains first, then each reference counts.
            cause_warning!(TextureUnreadable, "a.png: not found");
            for m in ["/m1", "/m2", "/m3"] {
                record_warning!(TextureUnreadable, at = m, "a.png not loadable");
            }
            // A second file's cause does not replace the first message.
            cause_warning!(TextureUnreadable, "b.png: corrupt");
        });
        let records = scope.finish();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].count, 3);
        assert_eq!(records[0].prims, ["/m1", "/m2", "/m3"]);
        assert_eq!(records[0].message, "a.png: not found");
        assert_eq!(
            lines,
            [
                "[texture.unreadable] a.png: not found",
                "[texture.unreadable] b.png: corrupt"
            ]
        );
        // A cause with no occurrence leaves no record.
        let scope = WarningScope::enter();
        cause_warning!(IesUnreadable, "x.ies");
        assert!(scope.finish().is_empty());
    }

    #[test]
    fn record_without_cause_formats_its_own_message() {
        let scope = WarningScope::enter();
        let lines = logged(|| record_warning!(TextureUnreadable, at = "/m", "fallback text"));
        let records = scope.finish();
        assert!(lines.is_empty());
        assert_eq!(records[0].message, "fallback text");
    }

    #[test]
    fn serializes_codes_and_kinds_as_strings() {
        let w = Warning {
            code: WarningCode::LightDegenerateShape,
            kind: WarningKind::Skipped,
            count: 2,
            prims: vec!["/a".into()],
            message: "m".into(),
        };
        assert_eq!(
            serde_json::to_string(&w).unwrap(),
            r#"{"code":"light.degenerate_shape","kind":"skipped","count":2,"prims":["/a"],"message":"m"}"#
        );
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "rayon worker")]
    fn recording_from_a_rayon_worker_during_an_import_panics() {
        let _scope = WarningScope::enter();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        let result = pool
            .install(|| std::panic::catch_unwind(|| warning!(TimeOutsideRange, "from a worker")));
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }
}
