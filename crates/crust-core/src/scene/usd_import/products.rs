//! `RenderSettings.products` → `RenderProduct` → `orderedVars` → `RenderVar`:
//! which files a render writes and which AOVs go in them ([`AovRequest`]).
//!
//! A small resolver over the typed `openusd-schemas` views rather than
//! `openusd_schemas::render::compute_render_spec`, which takes no time code:
//! `productName` is *varying* precisely so a shot can time-sample it per
//! frame, and every attribute here is read at the render's time code like
//! every other one crust reads. It follows `UsdRenderComputeSpec` step by
//! step otherwise — a product starts from the settings' base attributes and
//! overrides only what it authors, vars keep `orderedVars` order, a var
//! targeted twice in one product is used once — and a test pins the two to
//! the same answer on a time-invariant stage.
//!
//! The vocabulary (what a `sourceName` means) is [`crate::aov`]'s. Everything
//! refused here — a deep product, a mismatched camera, an unknown source, a
//! type that cannot hold its source — is refused with one `WARN` naming the
//! prim, and gets no file or channel rather than a black one that looks
//! valid.

use crate::warning;
use std::collections::HashMap;

use openusd::sdf;
use openusd::usd::{Prim, Stage};
use openusd_schemas::render::{
    Product as RenderProduct, ProductSchema, Settings as UsdRenderSettings, SettingsBaseSchema,
    SettingsSchema, Var as RenderVar,
};
use tracing::debug;

use crate::aov::{Accumulation, AovProduct, AovRequest, AovSource, AovVar, Precision};
use crate::tracer::PixelRect;

use super::attrs::{custom_bool, custom_token, decode_bool, decode_number, prim_value, value_at};
use super::prim_at;
use super::settings::render_settings_path;

const DRIVER_PARAMETERS: &str = "driver:parameters:";

/// The stage's products, and the camera, resolution, data window and
/// motion-blur switch the render takes from the first of them.
#[derive(Debug)]
pub(super) struct RenderProducts {
    pub(super) request: AovRequest,
    /// The first product's resolved camera — the settings' own when the
    /// product authors none.
    pub(super) camera: Option<sdf::Path>,
    /// The first product's resolved resolution, when the settings or the
    /// product author one.
    pub(super) resolution: Option<(usize, usize)>,
    /// The first product's resolved `dataWindowNDC` (`xmin, ymin, xmax,
    /// ymax`, bottom-left origin), when the settings or the product author
    /// one; [`region_from_ndc`] turns it into the render's region.
    pub(super) data_window: Option<[f32; 4]>,
    /// Whether moving geometry is motion blurred: `false` when the first
    /// product (else the settings prim) authors `disableMotionBlur = true`
    /// or its deprecated synonym `instantaneousShutter = true`. The stage's
    /// motion stays in the scene either way (`RenderSettings::with_motion_blur`).
    pub(super) motion_blur: bool,
}

/// Hand-written for one field: a stage that authors nothing blurs.
impl Default for RenderProducts {
    fn default() -> Self {
        RenderProducts {
            request: AovRequest::default(),
            camera: None,
            resolution: None,
            data_window: None,
            motion_blur: true,
        }
    }
}

/// The base attributes a product inherits from the settings and may
/// override — the ones crust renders with. The other `RenderSettingsBase`
/// attributes are not honoured (see [`warn_unhonoured`]).
#[derive(Debug, Clone, PartialEq)]
struct Base {
    camera: Option<sdf::Path>,
    resolution: Option<(usize, usize)>,
    data_window: Option<[f32; 4]>,
    /// `disableMotionBlur` and `instantaneousShutter`, each resolved on its
    /// own: a product that authors one of them inherits the other.
    disable_motion_blur: bool,
    instantaneous_shutter: bool,
}

fn read_resolution(view: &impl SettingsBaseSchema) -> Option<(usize, usize)> {
    let v = value_at(&view.resolution_attr())?.try_as_vec_2i()?;
    (v.x > 0 && v.y > 0).then_some((v.x as usize, v.y as usize))
}

fn read_data_window(view: &impl SettingsBaseSchema) -> Option<[f32; 4]> {
    let v = value_at(&view.data_window_ndc_attr())?.try_as_vec_4f()?;
    Some([v.x, v.y, v.z, v.w])
}

fn read_camera(view: &impl SettingsBaseSchema) -> Option<sdf::Path> {
    view.camera_rel().targets().ok()?.into_iter().next()
}

impl Base {
    /// Nothing authored anywhere: the first camera, the default resolution,
    /// motion blur on.
    fn unauthored() -> Base {
        Base {
            camera: None,
            resolution: None,
            data_window: None,
            disable_motion_blur: false,
            instantaneous_shutter: false,
        }
    }

    /// `view`'s authored opinions over `fallback`.
    fn resolve(view: &impl SettingsBaseSchema, fallback: &Base) -> Base {
        Base {
            camera: read_camera(view).or_else(|| fallback.camera.clone()),
            resolution: read_resolution(view).or(fallback.resolution),
            data_window: read_data_window(view).or(fallback.data_window),
            disable_motion_blur: value_at(&view.disable_motion_blur_attr())
                .and_then(decode_bool)
                .unwrap_or(fallback.disable_motion_blur),
            instantaneous_shutter: value_at(&view.instantaneous_shutter_attr())
                .and_then(decode_bool)
                .unwrap_or(fallback.instantaneous_shutter),
        }
    }

    /// Whether the render blurs moving geometry: either flag turns it off.
    fn motion_blur(&self) -> bool {
        !(self.disable_motion_blur || self.instantaneous_shutter)
    }

    /// Whether two products can be rendered by one render: the same camera
    /// and resolution. The shutter flags and the data window are not part
    /// of it — the render follows the first product's, and a later one that
    /// differs is warned about, not refused.
    fn same_render(&self, other: &Base) -> bool {
        self.camera == other.camera && self.resolution == other.resolution
    }
}

/// Resolves the render settings prim's products. Empty when the stage has no
/// settings prim or the prim authors no `products` — the "write the single
/// beauty EXR" case.
pub(super) fn import_render_products(stage: &Stage) -> RenderProducts {
    let Some(path) = render_settings_path(stage) else {
        return RenderProducts::default();
    };
    let Some(settings) = UsdRenderSettings::get(stage, path.clone()).ok().flatten() else {
        return RenderProducts::default();
    };
    let targets = settings
        .products_rel()
        .forwarded_targets()
        .unwrap_or_default();
    let base = Base::resolve(&settings, &Base::unauthored());
    if targets.is_empty() {
        return RenderProducts {
            request: AovRequest::default(),
            motion_blur: base.motion_blur(),
            camera: base.camera,
            resolution: base.resolution,
            data_window: base.data_window,
        };
    }
    warn_unhonoured(&prim_at(stage, path.clone()));

    let mut out = RenderProducts {
        request: AovRequest::default(),
        camera: base.camera.clone(),
        resolution: base.resolution,
        data_window: base.data_window,
        motion_blur: base.motion_blur(),
    };
    // The render's own base: the first accepted product's.
    let mut render_base: Option<Base> = None;
    // A var shared by several products is resolved, and warned about, once.
    let mut vars: HashMap<sdf::Path, Option<AovVar>> = HashMap::new();
    let mut lpes: Vec<String> = Vec::new();
    // Expressions refused once, so a var shared by products warns once.
    let mut refused: Vec<String> = Vec::new();

    for product_path in targets {
        let Some(product) = RenderProduct::get(stage, product_path.clone())
            .ok()
            .flatten()
        else {
            warning!(
                ProductNotARenderProduct,
                at = product_path,
                "{path}.products targets {product_path}, which is not a RenderProduct; skipped"
            );
            continue;
        };
        let prim = prim_at(stage, product_path.clone());
        let product_type =
            custom_token(&prim, "productType").unwrap_or_else(|| "raster".to_owned());
        if product_type != "raster" {
            warning!(
                ProductUnsupportedType,
                at = product_path,
                "{product_path}: productType {product_type:?} is not supported (only \
                 \"raster\"); no file is written for it"
            );
            continue;
        }
        // The camera's existence is not checked here: this is the index
        // stage, with payloads unloaded, and a shot camera under a payload
        // (the production case) is not composed on it. A camera that really
        // is missing is the traversal's to find, which warns and falls back
        // to the first camera (`CameraChoice::Settings`).
        let resolved = Base::resolve(&product, &base);
        match &render_base {
            None => render_base = Some(resolved.clone()),
            Some(first) if !first.same_render(&resolved) => {
                warning!(
                    ProductCameraMismatch,
                    at = product_path,
                    "{product_path}: renders through {} at {}, but the render is {} at {} \
                     (the first product's); crust renders one camera and resolution per \
                     stage, so no file is written for it",
                    describe_camera(&resolved.camera),
                    describe_resolution(resolved.resolution),
                    describe_camera(&first.camera),
                    describe_resolution(first.resolution),
                );
                continue;
            }
            Some(first) if first.motion_blur() != resolved.motion_blur() => {
                // One shutter per stage, like one camera: the file is still
                // written, with the first product's blur.
                let state = |on: bool| if on { "on" } else { "off" };
                warning!(
                    ProductMotionBlurMismatch,
                    at = product_path,
                    "{product_path}: asks for motion blur {}, but the render has it {} (the \
                     first product's); its file is written with that",
                    state(resolved.motion_blur()),
                    state(first.motion_blur()),
                );
            }
            Some(_) => {}
        }
        // Compared as they render: an unauthored window is the full frame,
        // like an authored `(0, 0, 1, 1)`.
        if let Some(first) = &render_base
            && window_or_full(first.data_window) != window_or_full(resolved.data_window)
        {
            // One region per render, like one shutter: the file is still
            // written, over the first product's region.
            warning!(
                ProductRegionMismatch,
                at = product_path,
                "{product_path}: asks for dataWindowNDC {}, but the render's is {} (the first \
                 product's); its file is written with that",
                describe_window(resolved.data_window),
                describe_window(first.data_window),
            );
        }
        warn_unhonoured(&prim);

        let name = custom_token(&prim, "productName").unwrap_or_default();
        let mut seen = Vec::new();
        let mut product_vars = Vec::new();
        for var_path in product
            .ordered_vars_rel()
            .forwarded_targets()
            .unwrap_or_default()
        {
            if seen.contains(&var_path) {
                debug!("{product_path}: {var_path} is targeted twice; using it once");
                continue;
            }
            seen.push(var_path.clone());
            let var = vars
                .entry(var_path.clone())
                .or_insert_with(|| resolve_var(stage, &var_path))
                .clone();
            // One DFA holds every expression of the render, each accepting
            // as one bit of a u64 and all sharing a bounded number of
            // states: an expression is accepted only if the render's set
            // still compiles with it, so the renderer's compile cannot fail.
            if let Some(v) = &var
                && let Some(e) = &v.expression
                && !lpes.contains(e)
            {
                if refused.contains(e) {
                    continue;
                }
                let mut set: Vec<&str> = lpes.iter().map(String::as_str).collect();
                set.push(e);
                if let Err(err) = crate::lpe::Lpe::compile(&set) {
                    warning!(
                        LpeInvalid,
                        at = var_path,
                        "{var_path}: light path expression {e:?} refused: {err}"
                    );
                    refused.push(e.clone());
                    continue;
                }
                lpes.push(e.clone());
            }
            product_vars.extend(var);
        }
        let attributes = driver_attributes(&prim);
        debug!(
            "{product_path}: {:?} with {} var(s): {}",
            name,
            product_vars.len(),
            product_vars
                .iter()
                .map(|v| format!("{} = {}", v.name, v.source.name()))
                .collect::<Vec<_>>()
                .join(", ")
        );
        out.request.products.push(AovProduct {
            prim_path: product_path.to_string(),
            name,
            vars: product_vars,
            attributes,
        });
    }
    if let Some(first) = render_base {
        out.motion_blur = first.motion_blur();
        out.camera = first.camera;
        out.resolution = first.resolution;
        out.data_window = first.data_window;
    }
    out
}

/// `UsdRender`'s fallback `dataWindowNDC`: the whole frame.
const FULL_WINDOW: [f32; 4] = [0.0, 0.0, 1.0, 1.0];

/// The window a render uses: the authored one, else the full frame.
fn window_or_full(window: Option<[f32; 4]>) -> [f32; 4] {
    window.unwrap_or(FULL_WINDOW)
}

fn describe_window(window: Option<[f32; 4]>) -> String {
    let [x0, y0, x1, y1] = window_or_full(window);
    format!("({x0}, {y0}, {x1}, {y1})")
}

/// The render region a `dataWindowNDC` selects on a `width` × `height`
/// frame: the pixels whose centres lie inside the window, `xmin ≤ (x + ½)/W
/// < xmax` and `ymin ≤ 1 − (y + ½)/H < ymax` — NDC's `y` grows upwards from
/// the bottom-left, the region's downwards from the top-left. `None` is the
/// full frame.
///
/// A window reaching outside [0, 1] (overscan) is clipped to the frame, with
/// one warning; one that selects no pixel — a window wholly outside the frame
/// included — or is not finite, is refused with one warning and the full
/// frame renders. No window gets two.
pub(super) fn region_from_ndc(window: [f32; 4], width: usize, height: usize) -> Option<PixelRect> {
    let text = describe_window(Some(window));
    if window == FULL_WINDOW {
        return None;
    }
    if !window.iter().all(|c| c.is_finite()) {
        warning!(
            ProductInvalidDataWindow,
            "dataWindowNDC = {text} is not finite; rendering the full frame"
        );
        return None;
    }
    let [xmin, ymin, xmax, ymax] = window.map(f64::from);
    let (w, h) = (width as f64, height as f64);
    // The first pixel whose centre is at or past `c` (for the `≤` bounds),
    // and the first whose centre is past `c` (for the strict ones), each
    // clamped to the frame.
    let at_or_past = |c: f64, n: f64| (c * n - 0.5).ceil().clamp(0.0, n) as usize;
    let past = |c: f64, n: f64| ((c * n - 0.5).floor() + 1.0).clamp(0.0, n) as usize;
    let region = PixelRect::new(
        at_or_past(xmin, w),
        past(1.0 - ymax, h),
        at_or_past(xmax, w),
        past(1.0 - ymin, h),
    );
    if region.is_empty() {
        warning!(
            ProductInvalidDataWindow,
            "dataWindowNDC = {text} selects no pixel of the {width}x{height} frame; rendering the full frame"
        );
        return None;
    }
    // Only once the clipped window is known to select something: a window
    // wholly outside the frame is the empty case above, not a clipped one.
    if window.iter().any(|c| !(0.0..=1.0).contains(c)) {
        warning!(
            ProductDataWindowClipped,
            "dataWindowNDC = {text} reaches outside the frame; overscan is not supported, \
             so it is clipped to it"
        );
    }
    Some(region)
}

fn describe_camera(camera: &Option<sdf::Path>) -> String {
    camera
        .as_ref()
        .map_or_else(|| "the first camera".to_owned(), ToString::to_string)
}

fn describe_resolution(resolution: Option<(usize, usize)>) -> String {
    resolution.map_or_else(
        || "the default resolution".to_owned(),
        |(w, h)| format!("{w}x{h}"),
    )
}

/// `RenderSettingsBase` attributes crust does not honour, warned about only
/// when authored with a value that would change the image — Houdini authors
/// every one of them at its fallback, and those need no word.
/// `disableMotionBlur`, `instantaneousShutter` and `dataWindowNDC` are
/// honoured ([`Base`]), so they are not here.
fn warn_unhonoured(prim: &Prim) {
    let mut ignored = Vec::new();
    let value = |name: &str| prim_value(prim, name);
    if let Some(sdf::Value::Float(a)) = value("pixelAspectRatio")
        && a != 1.0
    {
        ignored.push(format!("pixelAspectRatio = {a}"));
    }
    if custom_bool(prim, "disableDepthOfField") == Some(true) {
        ignored.push("disableDepthOfField = true".to_owned());
    }
    if !ignored.is_empty() {
        warning!(
            ProductUnhonouredAttribute,
            at = prim.path(),
            "{}: {} not honoured; rendering without it",
            prim.path(),
            ignored.join(", ")
        );
    }
}

/// A product's authored `driver:parameters:*` text values (outside the
/// `aov:` namespace, which configures vars), for the EXR header:
/// `driver:parameters:OpenEXR:<key>` as `<key>`, the rest by their name
/// without the prefix (`artist`, `comment`).
fn driver_attributes(prim: &Prim) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = prim
        .authored_property_names()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|name| {
            let name = name.as_str();
            let key = name.strip_prefix(DRIVER_PARAMETERS)?;
            if key.starts_with("aov:") {
                return None;
            }
            let value = custom_token(prim, name)?;
            let key = key.strip_prefix("OpenEXR:").unwrap_or(key);
            Some((key.to_owned(), value))
        })
        .collect();
    out.sort();
    out
}

/// A float attribute authored as any numeric type — Houdini writes
/// `clearValue` as a float, hand-written files as an int.
fn custom_number(prim: &Prim, name: &str) -> Option<f32> {
    prim_value(prim, name).and_then(decode_number)
}

/// Components and precision of an Sdf type name or a Houdini
/// `aov:format`, or `None` for anything else. The whole name must be one of
/// the spellings below — a prefix match would let `floatgarbage` through as
/// a float:
///
/// - `float`, `half`, `double`, `int`, `uint`, alone (one component) or
///   followed by `2`, `3` or `4` (Houdini's `float3`, `half4`, ...);
/// - `color3`/`color4`, `normal3`, `point3`, `vector3`, `texCoord2`/
///   `texCoord3`, followed by `f`, `h` or `d`.
///
/// `int`/`uint` are UINT, `half` and the `h` suffix HALF, everything else
/// FLOAT (a `double` is accumulated and written as f32).
pub(super) fn parse_data_type(name: &str) -> Option<(usize, Precision)> {
    const SCALARS: &[(&str, Precision)] = &[
        ("float", Precision::Float),
        ("half", Precision::Half),
        ("double", Precision::Float),
        ("int", Precision::Uint),
        ("uint", Precision::Uint),
    ];
    const ROLES: &[(&str, &[usize])] = &[
        ("color", &[3, 4]),
        ("normal", &[3]),
        ("point", &[3]),
        ("vector", &[3]),
        ("texCoord", &[2, 3]),
    ];
    let digit = |rest: &str| -> Option<usize> {
        let n = rest.parse::<usize>().ok()?;
        (rest.len() == 1 && (2..=4).contains(&n)).then_some(n)
    };
    for (family, precision) in SCALARS {
        if let Some(rest) = name.strip_prefix(family) {
            if rest.is_empty() {
                return Some((1, *precision));
            }
            return digit(rest).map(|n| (n, *precision));
        }
    }
    for (family, counts) in ROLES {
        if let Some(rest) = name.strip_prefix(family) {
            let mut chars = rest.chars();
            let (Some(n), Some(suffix), None) = (chars.next(), chars.next(), chars.next()) else {
                return None;
            };
            let n = n.to_digit(10)? as usize;
            if !counts.contains(&n) {
                return None;
            }
            let precision = match suffix {
                'f' | 'd' => Precision::Float,
                'h' => Precision::Half,
                _ => return None,
            };
            return Some((n, precision));
        }
    }
    None
}

/// Whether `source` can be written as `components` channels of `precision`.
fn type_fits(source: AovSource, components: usize, precision: Precision) -> bool {
    let components_fit = match source {
        AovSource::Color | AovSource::Lpe => components == 3 || components == 4,
        s => components == s.components(),
    };
    let precision_fits = precision != Precision::Uint || source == AovSource::SampleCount;
    components_fit && precision_fits
}

/// The accumulation a var's authored attributes ask for, first match wins:
/// Hydra's `multiSampled`, then Arnold, Karma and RenderMan filter
/// attributes. `None` when none is authored (or the one authored names a
/// rule crust does not have, which is warned).
fn authored_accumulation(prim: &Prim) -> Option<Accumulation> {
    if let Some(multi) = custom_bool(prim, "driver:parameters:aov:multiSampled") {
        return Some(if multi {
            Accumulation::Filtered
        } else {
            Accumulation::Closest
        });
    }
    if let Some(filter) = custom_token(prim, "arnold:filter") {
        return Some(if filter == "closest_filter" {
            Accumulation::Closest
        } else {
            Accumulation::Filtered
        });
    }
    if let Some(filter) = custom_token(prim, "driver:parameters:aov:filter") {
        let f = filter.trim_start();
        return Some(
            if f.starts_with("[\"closest\"") || f.starts_with("closest") {
                Accumulation::Closest
            } else {
                Accumulation::Filtered
            },
        );
    }
    for name in ["ri:accumulationRule", "ri:displayChannel:filter"] {
        if let Some(rule) = custom_token(prim, name) {
            return match rule.as_str() {
                "zmin" => Some(Accumulation::Closest),
                "filter" | "" => Some(Accumulation::Filtered),
                other => {
                    warning!(
                        AovUnknownAccumulation,
                        at = prim.path(),
                        "{}: {name} = {other:?} is not supported (only \"zmin\" and the \
                         filtered default); using the source's default",
                        prim.path()
                    );
                    None
                }
            };
        }
    }
    None
}

/// One RenderVar as a channel layer, or `None` (warned) when crust cannot
/// honour it.
fn resolve_var(stage: &Stage, path: &sdf::Path) -> Option<AovVar> {
    if RenderVar::get(stage, path.clone()).ok().flatten().is_none() {
        warning!(
            AovNotARenderVar,
            at = path,
            "orderedVars targets {path}, which is not a RenderVar; skipped"
        );
        return None;
    }
    let prim = prim_at(stage, path.clone());
    let prim_name = path.name().unwrap_or_default().to_owned();
    let name = custom_token(&prim, "driver:parameters:aov:name")
        .filter(|n| !n.is_empty())
        .unwrap_or(prim_name);
    let channel_prefix = custom_token(&prim, "driver:parameters:aov:channel_prefix")
        .or_else(|| custom_token(&prim, "driver:parameters:aov:husk:channel_prefix"));

    let source_type = custom_token(&prim, "sourceType").unwrap_or_else(|| "raw".to_owned());
    let source_name = custom_token(&prim, "sourceName").unwrap_or_default();
    let mut expression = None;
    let mut raw = false;
    let raw_authored = custom_bool(&prim, "crust:aov:raw") == Some(true);
    let variance = custom_bool(&prim, "crust:aov:variance") == Some(true);
    if variance && source_type != "lpe" {
        warning!(
            AovInvalidVariance,
            at = path,
            "{path}: crust:aov:variance applies to light path expressions (sourceType \
             \"lpe\"), not sourceType {source_type:?}; no channel written"
        );
        return None;
    }
    let source = match source_type.as_str() {
        "raw" => {
            let lookup = if source_name.is_empty() {
                &name
            } else {
                &source_name
            };
            if raw_authored {
                warning!(
                    AovRawIgnored,
                    at = path,
                    "{path}: crust:aov:raw applies to light path expressions \
                     (sourceType \"lpe\"); ignored on {lookup:?}"
                );
            }
            if let Some(expr) = crate::aov::raw_light_expression(lookup) {
                // `rawLight` …: a fixed expression, divided by the diffuse
                // filter. It starts with a diffuse reflection by construction.
                expression = Some(expr.to_owned());
                raw = true;
                AovSource::Lpe
            } else {
                match AovSource::from_raw(lookup) {
                    Some(s) => s,
                    None if AovSource::is_planned(lookup) => {
                        warning!(
                            AovUnsupportedSource,
                            at = path,
                            "{path}: source {lookup:?} is not supported yet; no channel written"
                        );
                        return None;
                    }
                    None => {
                        warning!(
                            AovUnknownSource,
                            at = path,
                            "{path}: unknown raw source {lookup:?}; no channel written (crust's \
                         AOV names are listed in the user documentation, usd/aovs)"
                        );
                        return None;
                    }
                }
            }
        }
        "lpe" => {
            let expr = crate::lpe::strip_prefix(&source_name);
            if let Err(e) = crate::lpe::validate(expr) {
                warning!(
                    LpeInvalid,
                    at = path,
                    "{path}: light path expression {expr:?}: {e}; no channel written"
                );
                return None;
            }
            if raw_authored {
                // Raw light divides by the diffuse colour of the camera's
                // first hit, which only means something when every path the
                // expression accepts starts by reflecting off it diffusely.
                let lpe = match crate::lpe::Lpe::compile(&[expr]) {
                    Ok(lpe) => lpe,
                    Err(e) => {
                        warning!(
                            LpeInvalid,
                            at = path,
                            "{path}: light path expression {expr:?}: {e}; no channel written"
                        );
                        return None;
                    }
                };
                if !lpe.starts_with_diffuse_reflection(0) {
                    warning!(
                        AovInvalidRaw,
                        at = path,
                        "{path}: crust:aov:raw needs an expression whose every path starts \
                         with a diffuse reflection (C<RD>…); {expr:?} does not, so no channel \
                         written"
                    );
                    return None;
                }
                raw = true;
            }
            expression = Some(expr.to_owned());
            AovSource::Lpe
        }
        "primvar" => {
            warning!(
                AovUnsupportedSource,
                at = path,
                "{path}: primvar source {source_name:?} is not supported yet; no channel written"
            );
            return None;
        }
        "intrinsic" => {
            warning!(
                AovUnsupportedSource,
                at = path,
                "{path}: sourceType \"intrinsic\" is unimplemented in UsdRender itself; no \
                 channel written"
            );
            return None;
        }
        other => {
            warning!(
                AovUnknownSource,
                at = path,
                "{path}: unknown sourceType {other:?}; no channel written"
            );
            return None;
        }
    };

    // An expression's variance is one scalar: unauthored, it is a float.
    let type_name = custom_token(&prim, "driver:parameters:aov:format")
        .or_else(|| custom_token(&prim, "dataType"))
        .unwrap_or_else(|| if variance { "float" } else { "color3f" }.to_owned());
    let Some((components, precision)) = parse_data_type(&type_name) else {
        warning!(
            AovTypeMismatch,
            at = path,
            "{path}: data type {type_name:?} is not a numeric type; no channel written"
        );
        return None;
    };
    if variance && (components != 1 || precision == Precision::Uint) {
        warning!(
            AovInvalidVariance,
            at = path,
            "{path}: the variance of {:?} is one float channel and cannot be written as \
             {type_name:?}; no channel written",
            expression.as_deref().unwrap_or_default()
        );
        return None;
    }
    if !variance && !type_fits(source, components, precision) {
        warning!(
            AovTypeMismatch,
            at = path,
            "{path}: {} cannot be written as {type_name:?}; no channel written",
            source.name()
        );
        return None;
    }

    let default = source.default_accumulation();
    let authored = authored_accumulation(&prim);
    if variance && authored == Some(Accumulation::Closest) {
        // One sample's value per pixel has no sample variance.
        warning!(
            AovInvalidVariance,
            at = path,
            "{path}: crust:aov:variance needs every sample, so it cannot be accumulated \
             as Closest; no channel written"
        );
        return None;
    }
    let accumulation = match authored {
        Some(mode) if !source.accepts_accumulation() && mode != default => {
            warning!(
                AovAccumulationIgnored,
                at = path,
                "{path}: {} is a per-pixel quantity and cannot be accumulated as {mode:?}; \
                 using {default:?}",
                source.name()
            );
            default
        }
        Some(mode) => mode,
        None => default,
    };
    let clear = custom_number(&prim, "driver:parameters:aov:clearValue")
        .unwrap_or_else(|| source.default_clear());

    Some(AovVar {
        prim_path: path.to_string(),
        name,
        channel_prefix,
        source,
        components,
        precision,
        accumulation,
        clear,
        expression,
        raw,
        variance,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_types_parse_whole_names_only() {
        let ok = [
            ("float", (1, Precision::Float)),
            ("half", (1, Precision::Half)),
            ("int", (1, Precision::Uint)),
            ("double", (1, Precision::Float)),
            ("float3", (3, Precision::Float)),
            ("half4", (4, Precision::Half)),
            ("color3f", (3, Precision::Float)),
            ("color4h", (4, Precision::Half)),
            ("normal3f", (3, Precision::Float)),
            ("point3d", (3, Precision::Float)),
            ("vector3h", (3, Precision::Half)),
            ("texCoord2f", (2, Precision::Float)),
        ];
        for (name, want) in ok {
            assert_eq!(parse_data_type(name), Some(want), "{name}");
        }
        for name in [
            "floatgarbage",
            "float5",
            "float1",
            "float33",
            "color3",
            "color3x",
            "color2f",
            "normal4f",
            "texCoord2fz",
            "float[]",
            "string",
            "token",
            "",
        ] {
            assert_eq!(parse_data_type(name), None, "{name}");
        }
    }
}
