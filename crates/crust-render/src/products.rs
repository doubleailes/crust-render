//! Writing a render's `RenderProduct`s: one single-part, scanline,
//! ZIP16-compressed EXR per product, one layer of named channels per var.
//!
//! Channel names follow OpenEXR's `<layer>.<component>` convention and the
//! ASWF Color Interop rule that only colour gets `R/G/B`: colour vars are
//! `<layer>.R/.G/.B[/.A]`, vectors `<layer>.X/.Y/.Z`, UVs `<layer>.U/.V`,
//! motion vectors `<layer>.u/.v` (lowercase, Nuke's `forward.u/.v`), and a
//! scalar is one channel named after its layer. The product's first beauty
//! var is written bare (`R/G/B[/A]`) so every viewer shows it as the image.
//!
//! Scanline, not the `exr` crate's default tiling: tinyexr crashes on crust's
//! tiled files (`docs/material_fidelity.md`). The no-products render does not
//! come through here at all — it keeps `write_rgb_file`, byte for byte.

use crust_core::stamp::{STAMP_PREFIX, SamplingStamp, StampValue};
use crust_core::{
    Accumulation, AovFilm, AovProduct, AovSource, AovVar, Buffer, ChannelKind, Precision,
    RenderPass,
};
use exr::meta::attribute::{AttributeValue, IntegerBounds};
use exr::prelude::{
    AnyChannel, AnyChannels, Blocks, Compression, Encoding, FlatSamples, Image, Layer,
    LayerAttributes, LineOrder, Text, Vec2, WritableImage, f16,
};
use std::io;
use std::path::Path;
use tracing::warn;

/// Where a render's pixels sit in its frame, as an EXR says it: the data
/// window's origin (the layer position) and the display window. The data
/// window is the render's region, in image space (top-left origin, rows
/// down, as EXR counts them); the display window is the whole frame. A
/// full-frame render gets the `(0, 0)` position and the `0..w × 0..h`
/// display window the `exr` crate writes by default, so its file does not
/// change by a byte.
pub fn exr_windows(beauty: &Buffer) -> (Vec2<i32>, IntegerBounds) {
    let (frame_w, frame_h) = beauty.frame_size();
    let region = beauty.region();
    (
        Vec2(region.x0 as i32, region.y0 as i32),
        IntegerBounds::new((0, 0), (frame_w, frame_h)),
    )
}

/// The header attribute every EXR of an interrupted render carries
/// (`image-output`), as a string: `"interrupted"`. A completed render writes
/// none, so its headers are exactly those it wrote before renders could be
/// interrupted.
pub const RENDER_STATUS: &str = "crust:renderStatus";

/// Marks a layer's header as an interrupted render's ([`RENDER_STATUS`]).
/// The one function both writers call, after [`stamp`].
pub fn mark_interrupted(attributes: &mut LayerAttributes) {
    attributes.other.insert(
        Text::from(RENDER_STATUS),
        AttributeValue::Text(Text::from("interrupted")),
    );
}

/// Writes `stamp` into a layer's header, each attribute typed as the stamp
/// types it (`int`, `v2i`, `float`, `double`, `string`) and replacing any
/// attribute of the same name already there. The one function both writers
/// call, the single beauty EXR and each RenderProduct.
pub fn stamp(attributes: &mut LayerAttributes, stamp: &SamplingStamp) {
    for (name, value) in stamp.attributes() {
        let value = match value {
            StampValue::Int(v) => AttributeValue::I32(v),
            StampValue::Int2(a, b) => AttributeValue::IntVec2(Vec2(a, b)),
            StampValue::Float(v) => AttributeValue::F32(v),
            StampValue::Double(v) => AttributeValue::F64(v),
            StampValue::Text(v) => match Text::new_or_none(&v) {
                Some(t) => AttributeValue::Text(t),
                // A value EXR text cannot hold (a camera path outside
                // Latin-1): left out rather than written wrong, and said, as
                // `crust diff` can then no longer tell two of them apart.
                None => {
                    warn!(
                        "{name} = {v:?} cannot be written as EXR text; left out of the \
                         header, so crust diff cannot compare it"
                    );
                    continue;
                }
            },
        };
        attributes.other.insert(Text::from(name), value);
    }
}

/// The Color Interop ID for a working space without one in the config.
/// The file still says what it can: its chromaticities, when known.
const UNKNOWN_INTEROP_ID: &str = "unknown";

/// The channel names `var` writes, one per plane `AovFilm::var_channels`
/// returns. `bare` makes the layer prefix empty — the product's beauty.
pub fn channel_names(var: &AovVar, bare: bool) -> Vec<String> {
    let layer = match &var.channel_prefix {
        Some(prefix) => prefix.clone(),
        None if bare => String::new(),
        None => var.name.clone(),
    };
    let components: &[&str] = match var.channel_kind() {
        ChannelKind::Color if var.with_alpha() => &["R", "G", "B", "A"],
        ChannelKind::Color => &["R", "G", "B"],
        ChannelKind::Vector => &["X", "Y", "Z"],
        ChannelKind::Uv => &["U", "V"],
        ChannelKind::Motion => &["u", "v"],
        ChannelKind::Scalar => {
            return vec![if layer.is_empty() {
                var.name.clone()
            } else {
                layer
            }];
        }
    };
    components
        .iter()
        .map(|c| {
            if layer.is_empty() {
                (*c).to_owned()
            } else {
                format!("{layer}.{c}")
            }
        })
        .collect()
}

/// `plane` as `precision` samples. A UINT channel holds integers; a negative
/// one (the `-1` "no ID" clear value) is written as its two's-complement
/// bit pattern.
fn samples(plane: Vec<f32>, precision: Precision) -> FlatSamples {
    match precision {
        Precision::Half => FlatSamples::F16(plane.into_iter().map(f16::from_f32).collect()),
        Precision::Float => FlatSamples::F32(plane),
        Precision::Uint => FlatSamples::U32(plane.into_iter().map(|v| v as i32 as u32).collect()),
    }
}

/// The layout of a product's file: its channels' names, in var order, with
/// the var each comes from. A var whose names collide with an earlier one's
/// is refused (warned) rather than written over it.
pub fn product_channels(product: &AovProduct) -> Vec<(&AovVar, Vec<String>)> {
    let beauty = product.beauty().map(|v| v.prim_path.as_str());
    let mut taken: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for var in &product.vars {
        let bare = Some(var.prim_path.as_str()) == beauty;
        let names = channel_names(var, bare);
        if let Some(clash) = names.iter().find(|n| taken.contains(n)) {
            warn!(
                "{}: channel {clash:?} is already written by another var of {}; skipped",
                var.prim_path, product.prim_path
            );
            continue;
        }
        taken.extend(names.iter().cloned());
        out.push((var, names));
    }
    out
}

/// What [`expand_frame_vars`] saw in a path that its caller turns into a
/// coded warning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathIssue {
    /// A `$NAME` / `${NAME}` that is not a frame variable, as written; it is
    /// left in the path (`product.unknown_variable`).
    Unknown(String),
    /// An integer frame variable rounded a fractional frame
    /// (`product.subframe_frame`).
    Subframe,
}

/// A frame variable: `F`, `F2`–`F9` (zero-padded to that width) or `FF`.
#[derive(Clone, Copy)]
enum FrameVar {
    Int(usize),
    Float,
}

impl FrameVar {
    fn parse(name: &str) -> Option<Self> {
        match name {
            "F" => Some(FrameVar::Int(0)),
            "FF" => Some(FrameVar::Float),
            _ => match name.as_bytes() {
                [b'F', d @ b'2'..=b'9'] => Some(FrameVar::Int(usize::from(d - b'0'))),
                _ => None,
            },
        }
    }

    /// The variable's text at `frame`, and whether it rounded a subframe.
    fn format(self, frame: f64) -> (String, bool) {
        match self {
            // `round` takes halves away from zero, as Houdini's `round`;
            // the width counts the sign (`-003`), as its `padzero`. Formatted
            // from the `f64` itself: any finite frame is accepted, and a cast
            // to an integer type would saturate, giving two frames one name.
            // `+ 0.0` turns a rounded `-0` (from `-0.4`) into `0`.
            FrameVar::Int(width) => (
                format!("{:0width$.0}", frame.round() + 0.0),
                frame.fract() != 0.0,
            ),
            // The shortest round-trip form, with no trailing `.0`; `-0`
            // reads as `0`.
            FrameVar::Float if frame == 0.0 => ("0".to_owned(), false),
            FrameVar::Float => (format!("{frame}"), false),
        }
    }
}

/// `path` with husk's frame variables expanded at `frame`
/// (`output-path-frame-tokens` design D3, D4): `$F`, `$F2`–`$F9`, `$FF`,
/// their braced forms `${F4}`, and their UDIM-style forms `<F4>`. `\$` is a
/// literal `$`. Any other `$NAME` or `${NAME}` (longest name, as Houdini
/// reads it) is kept as written and reported; any other `$` or `<…>` is
/// literal and reported nothing. Pure, so that the caller decides which prim
/// a warning names.
pub fn expand_frame_vars(path: &str, frame: f64) -> (String, Vec<PathIssue>) {
    let is_start = |c: u8| c.is_ascii_alphabetic() || c == b'_';
    let is_name = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let bytes = path.as_bytes();
    let mut out = String::with_capacity(path.len());
    let mut issues = Vec::new();
    let mut subframe = false;
    let mut expand = |var: FrameVar, out: &mut String| {
        let (text, rounded) = var.format(frame);
        subframe |= rounded;
        out.push_str(&text);
    };
    // Every token boundary below is an ASCII byte, so slicing there stays on
    // a char boundary and non-ASCII text is copied through whole.
    let mut i = 0;
    let mut literal = 0;
    while i < bytes.len() {
        let token = match bytes[i] {
            b'\\' if bytes.get(i + 1) == Some(&b'$') => Some((i + 2, "$".to_owned(), None)),
            b'$' if bytes.get(i + 1) == Some(&b'{') => {
                let inner = i + 2;
                let len = bytes[inner..].iter().take_while(|&&c| is_name(c)).count();
                let end = inner + len;
                (len > 0 && is_start(bytes[inner]) && bytes.get(end) == Some(&b'}'))
                    .then(|| (end + 1, path[inner..end].to_owned(), Some(&path[i..=end])))
            }
            b'$' if bytes.get(i + 1).is_some_and(|&c| is_start(c)) => {
                let len = bytes[i + 1..].iter().take_while(|&&c| is_name(c)).count();
                let end = i + 1 + len;
                Some((end, path[i + 1..end].to_owned(), Some(&path[i..end])))
            }
            b'<' => bytes[i + 1..]
                .iter()
                .position(|&c| c == b'>')
                .map(|n| (i + 2 + n, &path[i + 1..i + 1 + n]))
                .filter(|(_, name)| FrameVar::parse(name).is_some())
                .map(|(next, name)| (next, name.to_owned(), Some(&path[i..next]))),
            _ => None,
        };
        let Some((next, name, written)) = token else {
            i += 1;
            continue;
        };
        out.push_str(&path[literal..i]);
        match (written, FrameVar::parse(&name)) {
            // `\$`: the literal dollar.
            (None, _) => out.push_str(&name),
            (Some(_), Some(var)) => expand(var, &mut out),
            (Some(written), None) => {
                out.push_str(written);
                issues.push(PathIssue::Unknown(written.to_owned()));
            }
        }
        i = next;
        literal = next;
    }
    out.push_str(&path[literal..]);
    if subframe {
        issues.push(PathIssue::Subframe);
    }
    (out, issues)
}

/// A render pass's file pattern, in the pass pattern dialect (`render-passes` design
/// D6, D7): `$l` / `$L` the pass, `$n` the layer, `$o` the RenderVar, `$f`
/// and `$01f`–`$09f` the frame, `$c` / `$C` the camera, `$e` / `$E` the eye,
/// `$x` the extension, `$(NAME)` an environment variable, `$_` before any of
/// them to lower-case it, and `\$` a literal `$`. Parsed once, expanded once
/// per (layer, RenderVar). The frame follows `expand_frame_vars`' rules
/// through the same [`FrameVar`].
#[derive(Debug, Clone, PartialEq)]
pub struct Pattern {
    source: String,
    tokens: Vec<Token>,
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Literal(String),
    Spec {
        spec: Spec,
        lower: bool,
    },
    /// A `$` and a name that is no specifier, kept as written.
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq)]
enum Spec {
    Pass,
    Layer,
    Output,
    Frame(usize),
    Camera,
    EyeNumber,
    EyeName,
    Extension,
    Env(String),
}

/// What a pattern refuses to expand: either would write somewhere other
/// than intended, so the command stops before rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatternError {
    /// `$(NAME)` names an environment variable that is not set (or not
    /// UTF-8): the file would move to the filesystem root.
    UnsetVariable { pattern: String, name: String },
    /// The expanded path's extension is not `.exr`: the EXR would be
    /// written under another format's name.
    NotExr { pattern: String, path: String },
}

impl std::fmt::Display for PatternError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PatternError::UnsetVariable { pattern, name } => write!(
                f,
                "file pattern {pattern:?}: $({name}) names the environment variable {name}, \
                 which is not set"
            ),
            PatternError::NotExr { pattern, path } => {
                let ext = Path::new(path)
                    .extension()
                    .map_or("none".to_owned(), |e| format!(".{}", e.to_string_lossy()));
                write!(
                    f,
                    "file pattern {pattern:?} writes {path:?}, whose extension ({ext}) is not \
                     .exr: crust writes only OpenEXR"
                )
            }
        }
    }
}

/// What one expansion is of: the pass, the camera and the frame, which do
/// not change, and the (layer, RenderVar) that does.
pub struct PatternInputs<'a> {
    pub pass: &'a str,
    pub camera: &'a str,
    pub frame: f64,
    pub layer: &'a str,
    pub output: &'a str,
}

impl Pattern {
    /// The tokens of `source`. Every token boundary is an ASCII byte, so
    /// non-ASCII text is copied through whole.
    pub fn parse(source: &str) -> Pattern {
        let bytes = source.as_bytes();
        let is_name = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
        let spec_at = |j: usize| -> Option<(Spec, usize)> {
            let single = |s: Spec| Some((s, j + 1));
            match bytes.get(j)? {
                b'l' | b'L' => single(Spec::Pass),
                b'n' => single(Spec::Layer),
                b'o' => single(Spec::Output),
                b'f' => single(Spec::Frame(0)),
                b'c' | b'C' => single(Spec::Camera),
                b'e' => single(Spec::EyeNumber),
                b'E' => single(Spec::EyeName),
                b'x' => single(Spec::Extension),
                b'0' => match (bytes.get(j + 1), bytes.get(j + 2)) {
                    (Some(d @ b'1'..=b'9'), Some(b'f')) => {
                        Some((Spec::Frame(usize::from(d - b'0')), j + 3))
                    }
                    _ => None,
                },
                b'(' => {
                    let close = bytes[j + 1..].iter().position(|&c| c == b')')?;
                    let name = &source[j + 1..j + 1 + close];
                    (!name.is_empty()).then(|| (Spec::Env(name.to_owned()), j + 2 + close))
                }
                _ => None,
            }
        };
        let mut tokens = Vec::new();
        let mut literal = String::new();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'\\' && bytes.get(i + 1) == Some(&b'$') {
                literal.push('$');
                i += 2;
                continue;
            }
            if bytes[i] != b'$' {
                let next = (i + 1..=bytes.len())
                    .find(|&k| k == bytes.len() || bytes[k] == b'$' || bytes[k] == b'\\')
                    .unwrap_or(bytes.len());
                literal.push_str(&source[i..next]);
                i = next;
                continue;
            }
            let lowered = (bytes.get(i + 1) == Some(&b'_'))
                .then(|| spec_at(i + 2))
                .flatten();
            let (token, next) = if let Some((spec, next)) = lowered {
                (Token::Spec { spec, lower: true }, next)
            } else if let Some((spec, next)) = spec_at(i + 1) {
                (Token::Spec { spec, lower: false }, next)
            } else if bytes.get(i + 1).is_some_and(|&c| is_name(c)) {
                let len = bytes[i + 1..].iter().take_while(|&&c| is_name(c)).count();
                let end = i + 1 + len;
                (Token::Unknown(source[i..end].to_owned()), end)
            } else {
                // A lone `$`: literal, and nothing to say about it.
                literal.push('$');
                i += 1;
                continue;
            };
            if !literal.is_empty() {
                tokens.push(Token::Literal(std::mem::take(&mut literal)));
            }
            tokens.push(token);
            i = next;
        }
        if !literal.is_empty() {
            tokens.push(Token::Literal(literal));
        }
        Pattern {
            source: source.to_owned(),
            tokens,
        }
    }

    /// The pattern as written.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Every `$` name that is no specifier, as written, each once.
    pub fn unknown(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for t in &self.tokens {
            if let Token::Unknown(name) = t
                && !out.contains(&name.as_str())
            {
                out.push(name);
            }
        }
        out
    }

    /// Whether a frame specifier rounds `frame`: it is not an integer.
    pub fn rounds(&self, frame: f64) -> bool {
        frame.fract() != 0.0
            && self.tokens.iter().any(|t| {
                matches!(
                    t,
                    Token::Spec {
                        spec: Spec::Frame(_),
                        ..
                    }
                )
            })
    }

    /// The pattern expanded for `inputs`, environment variables read through
    /// `env`. Refused when a variable is unset; the extension is the
    /// caller's to check, since a layer pattern names channels, not files.
    pub fn expand(
        &self,
        inputs: &PatternInputs,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> Result<String, PatternError> {
        let mut out = String::new();
        for t in &self.tokens {
            match t {
                Token::Literal(text) | Token::Unknown(text) => out.push_str(text),
                Token::Spec { spec, lower } => {
                    let value = match spec {
                        Spec::Pass => inputs.pass.to_owned(),
                        Spec::Layer => inputs.layer.to_owned(),
                        Spec::Output => inputs.output.to_owned(),
                        Spec::Frame(width) => FrameVar::Int(*width).format(inputs.frame).0,
                        Spec::Camera => inputs.camera.to_owned(),
                        Spec::EyeNumber => "1".to_owned(),
                        Spec::EyeName => "left".to_owned(),
                        Spec::Extension => "exr".to_owned(),
                        Spec::Env(name) => {
                            env(name).ok_or_else(|| PatternError::UnsetVariable {
                                pattern: self.source.clone(),
                                name: name.clone(),
                            })?
                        }
                    };
                    if *lower {
                        out.push_str(&value.to_lowercase());
                    } else {
                        out.push_str(&value);
                    }
                }
            }
        }
        Ok(out)
    }
}

/// The process environment, as a pattern reads `$(NAME)`: a path input of
/// the command, not a `CRUST_*` behaviour switch (design D6).
pub fn process_env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// The RenderVar a pass writes when its settings name none: the beauty
/// with alpha.
fn default_pass_var(pass: &RenderPass) -> AovVar {
    AovVar {
        prim_path: pass.path.clone(),
        name: "Beauty".to_owned(),
        channel_prefix: None,
        source: AovSource::Color,
        components: 4,
        precision: Precision::Float,
        accumulation: Accumulation::Filtered,
        clear: 0.0,
        expression: None,
        raw: false,
        variance: false,
        layer: None,
    }
}

/// The files a render pass writes: its `pattern` (the pass's own, or `-o`)
/// expanded for every (layer, var) — layer-major, vars in order — the
/// outputs grouped by expanded path, in first-occurrence order, into one
/// synthesized product each. A product holding several outputs names each
/// one's channels with the pass's layer pattern (`channel_prefix`); one
/// holding a single output keeps a product's names. `vars` are the pass's
/// settings' RenderVars ([`pass_vars`]). Unknown specifiers and a rounded
/// subframe are warned (`product.unknown_variable`, `product.subframe_frame`,
/// naming the pass); an unset variable, an extension other than `.exr` and
/// two outputs of one file with one layer name are refused, with the
/// message to stop on.
pub fn pass_products(
    pass: &RenderPass,
    vars: &[AovVar],
    pattern: &str,
    camera: &str,
    frame: f64,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<Vec<AovProduct>, String> {
    let file = Pattern::parse(pattern);
    let layer = Pattern::parse(&pass.layer_pattern);
    for (p, what) in [(&file, "file pattern"), (&layer, "layer pattern")] {
        let unknown = p.unknown();
        if !unknown.is_empty() {
            crust_core::warning!(
                ProductUnknownVariable,
                at = pass.path,
                "{}: {} in the {what} {:?} is not a pattern specifier; left as written",
                pass.path,
                unknown.join(", "),
                p.source()
            );
        }
    }
    if file.rounds(frame) {
        crust_core::warning!(
            ProductSubframeFrame,
            at = pass.path,
            "{}: frame {frame} rounded to {} in the file pattern {:?}",
            pass.path,
            frame.round(),
            file.source()
        );
    }
    let default_var;
    let vars = if vars.is_empty() {
        default_var = [default_pass_var(pass)];
        &default_var[..]
    } else {
        vars
    };
    // A layer's beauty is accumulated as one more light path expression; the
    // render compiles at most `MAX_EXPRESSIONS`.
    let mut expressions: Vec<&str> = vars
        .iter()
        .filter(|v| v.source == AovSource::Lpe)
        .filter_map(|v| v.expression.as_deref())
        .collect();
    if vars.iter().any(|v| v.source == AovSource::Color) {
        expressions.push(crust_core::aov::LAYER_BEAUTY);
    }
    expressions.sort_unstable();
    expressions.dedup();
    if expressions.len() > crust_core::lpe::MAX_EXPRESSIONS {
        return Err(format!(
            "{}: its RenderVars and its layers' beauty need {} light path expressions, more \
             than the {} one render compiles",
            pass.path,
            expressions.len(),
            crust_core::lpe::MAX_EXPRESSIONS
        ));
    }
    // One entry per file, in first-use order.
    let mut files: Vec<PassFile> = Vec::new();
    for (l, name) in pass.layers.iter().enumerate() {
        for var in vars {
            let inputs = PatternInputs {
                pass: &pass.name,
                camera,
                frame,
                layer: name,
                output: &var.name,
            };
            let path = file.expand(&inputs, env).map_err(|e| e.to_string())?;
            if !Path::new(&path)
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("exr"))
            {
                return Err(PatternError::NotExr {
                    pattern: file.source().to_owned(),
                    path,
                }
                .to_string());
            }
            let key: std::path::PathBuf = Path::new(&path).components().collect();
            match files.iter_mut().find(|f| f.key == key) {
                Some(f) => f.outputs.push((l as u16, var)),
                None => files.push(PassFile {
                    path,
                    key,
                    outputs: vec![(l as u16, var)],
                }),
            }
        }
    }
    let mut products = Vec::with_capacity(files.len());
    for PassFile { path, outputs, .. } in files {
        let shared = outputs.len() > 1;
        let mut names: Vec<String> = Vec::new();
        let mut product_vars = Vec::with_capacity(outputs.len());
        for (l, var) in outputs {
            let mut var = var.clone();
            var.layer = Some(l);
            if shared {
                let inputs = PatternInputs {
                    pass: &pass.name,
                    camera,
                    frame,
                    layer: &pass.layers[l as usize],
                    output: &var.name,
                };
                let name = layer.expand(&inputs, env).map_err(|e| e.to_string())?;
                if names.contains(&name) {
                    return Err(format!(
                        "{}: two outputs of {path} are named {name:?} by the layer pattern \
                         {:?}; give it $n or $o",
                        pass.path,
                        layer.source()
                    ));
                }
                names.push(name.clone());
                var.channel_prefix = Some(name);
            }
            product_vars.push(var);
        }
        products.push(AovProduct {
            prim_path: pass.path.clone(),
            name: path,
            vars: product_vars,
            attributes: Vec::new(),
        });
    }
    Ok(products)
}

/// One file of a pass, while its outputs are grouped: the path as expanded,
/// the lexical key it is compared by, and its (layer, var) outputs.
struct PassFile<'a> {
    path: String,
    key: std::path::PathBuf,
    outputs: Vec<(u16, &'a AovVar)>,
}

/// The RenderVars a pass writes: every var of its settings' products,
/// each prim once, in product order.
pub fn pass_vars(products: &[AovProduct]) -> Vec<AovVar> {
    let mut out: Vec<AovVar> = Vec::new();
    for var in products.iter().flat_map(|p| &p.vars) {
        if !out.iter().any(|v| v.prim_path == var.prim_path) {
            out.push(var.clone());
        }
    }
    out
}

/// The layer whose beauty a pass file's preview shows: the file's one
/// layer, when it has a beauty var. `None` for a file of several layers, or
/// without a beauty.
pub fn preview_layer(product: &AovProduct) -> Option<&AovVar> {
    let beauty = product.beauty()?;
    let layer = beauty.layer;
    product
        .vars
        .iter()
        .all(|v| v.layer == layer)
        .then_some(beauty)
}

/// Refuses (with a warning) every product whose path is one an earlier
/// product already writes, keeping the first: the later write would replace
/// the earlier file and silently lose its channels. Paths are compared
/// lexically, component by component (`a/./b.exr` is `a/b.exr`), since the
/// files need not exist yet.
pub fn refuse_shared_paths(products: &mut Vec<AovProduct>) {
    let mut seen: Vec<(std::path::PathBuf, String)> = Vec::new();
    products.retain(|p| {
        let path: std::path::PathBuf = Path::new(&p.name).components().collect();
        if let Some((_, first)) = seen.iter().find(|(q, _)| *q == path) {
            crust_core::warning!(
                ProductSharedPath,
                at = p.prim_path,
                "{} writes {}, as {first} already does; nothing written for it",
                p.prim_path,
                path.display()
            );
            return false;
        }
        seen.push((path, p.prim_path.clone()));
        true
    });
}

/// Writes `product` to `path`, creating its parent directories. Returns the
/// channel names written, in file (alphabetical) order.
///
/// The colour channels are tagged with the working space's ASWF Color
/// Interop ID (`colorInteropID`, `docs/color_management.md`) and, off
/// Rec.709, its chromaticities. An `interrupted` render's product says so
/// ([`RENDER_STATUS`]).
pub fn write_product(
    path: &Path,
    product: &AovProduct,
    beauty: &Buffer,
    film: &AovFilm,
    color: &super::OutputColor,
    sampling: &SamplingStamp,
    interrupted: bool,
) -> io::Result<Vec<String>> {
    let interop = crust_core::color::interop_id(color.working);
    let interop = interop.as_deref().unwrap_or(UNKNOWN_INTEROP_ID);
    let (width, height) = film.dimensions();
    let (position, display_window) = exr_windows(beauty);
    let mut channels = Vec::new();
    for (var, names) in product_channels(product) {
        for (name, plane) in names.iter().zip(film.var_channels(beauty, var)) {
            let name = Text::new_or_none(name).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("channel name {name:?} is not valid in an EXR"),
                )
            })?;
            channels.push(AnyChannel::new(name, samples(plane, var.precision)));
        }
    }
    if channels.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "no channel to write",
        ));
    }
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    // Sorted, because EXR stores channels alphabetically and a reader finds
    // them by name — an unsorted list is a malformed file.
    let channels = AnyChannels::sort(channels.into_iter().collect());
    let names = channels.list.iter().map(|c| c.name.to_string()).collect();

    let mut attributes = LayerAttributes {
        software_name: Text::new_or_none(concat!("crust-render ", env!("CARGO_PKG_VERSION"))),
        layer_position: position,
        ..LayerAttributes::default()
    };
    let mut text = vec![("colorInteropID", interop)];
    for (key, value) in &product.attributes {
        match key.as_str() {
            // Standard attributes `exr` exposes as typed fields.
            "comments" | "comment" => attributes.comments = Text::new_or_none(value),
            "owner" => attributes.owner = Text::new_or_none(value),
            // Describes the pixels crust wrote, so only crust may set it.
            "colorInteropID" => warn!(
                "{}: driver:parameters colorInteropID = {value:?} is not copied: the colour \
                 channels are {interop}",
                product.prim_path
            ),
            // How the pixels were sampled: crust's own record, below.
            k if k.starts_with(STAMP_PREFIX) => warn!(
                "{}: driver:parameters {k} = {value:?} is not copied: crust stamps its \
                 own {STAMP_PREFIX}* attributes",
                product.prim_path
            ),
            k if exr::meta::header::standard_names::ALL.contains(&k.as_bytes()) => {
                warn!(
                    "{}: driver:parameters {k:?} names a standard EXR attribute crust sets \
                     itself; not copied",
                    product.prim_path
                );
            }
            k => text.push((k, value)),
        }
    }
    for (key, value) in text {
        if let (Some(k), Some(v)) = (Text::new_or_none(key), Text::new_or_none(value)) {
            attributes.other.insert(k, AttributeValue::Text(v));
        }
    }
    stamp(&mut attributes, sampling);
    if interrupted {
        mark_interrupted(&mut attributes);
    }

    let layer = Layer::new(
        (width, height),
        attributes,
        Encoding {
            compression: Compression::ZIP16,
            blocks: Blocks::ScanLines,
            line_order: LineOrder::Increasing,
        },
        channels,
    );
    let mut image = Image::from_layer(layer);
    image.attributes.display_window = display_window;
    image.attributes.chromaticities = color.exr_chromaticities();
    image
        .write()
        .to_file(path)
        .map_err(|e| io::Error::other(format!("{}: {e}", path.display())))?;
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crust_core::{Accumulation, AovSource};
    use exr::prelude::{ReadChannels, ReadLayers, read};

    fn var(name: &str, source: AovSource) -> AovVar {
        AovVar {
            prim_path: format!("/Render/Vars/{name}"),
            name: name.to_owned(),
            channel_prefix: None,
            source,
            components: source.components(),
            precision: Precision::Float,
            accumulation: Accumulation::Filtered,
            clear: source.default_clear(),
            expression: None,
            raw: false,
            variance: false,
            layer: None,
        }
    }

    fn product(vars: Vec<AovVar>) -> AovProduct {
        AovProduct {
            prim_path: "/Render/p".into(),
            name: "p.exr".into(),
            vars,
            attributes: vec![
                ("artist".into(), "someone".into()),
                ("colorInteropID".into(), "srgb_rec709_display".into()),
                ("comments".into(), "a note".into()),
            ],
        }
    }

    /// A 16 spp render's stamp.
    fn sampling() -> SamplingStamp {
        let (_, settings) = crust_core::get_settings();
        SamplingStamp::new(
            &settings.with_samples_per_pixel(16),
            &crust_core::RayStats::default(),
            None,
            None,
        )
    }

    fn names(p: &AovProduct) -> Vec<String> {
        product_channels(p)
            .into_iter()
            .flat_map(|(_, names)| names)
            .collect()
    }

    #[test]
    fn channels_follow_the_layer_dot_component_convention() {
        let mut beauty = var("beauty", AovSource::Color);
        beauty.components = 4;
        let p = product(vec![
            beauty,
            var("Z", AovSource::Depth),
            var("N", AovSource::Normal),
            var("st", AovSource::St),
            var("diffuse", AovSource::Color),
        ]);
        // The beauty bare, scalars named after their layer, data in X/Y/Z or
        // U/V, a second colour var prefixed.
        assert_eq!(
            names(&p),
            [
                "R",
                "G",
                "B",
                "A",
                "Z",
                "N.X",
                "N.Y",
                "N.Z",
                "st.U",
                "st.V",
                "diffuse.R",
                "diffuse.G",
                "diffuse.B"
            ]
        );
    }

    /// Motion vectors take Nuke's lowercase `u` / `v`, so a var named
    /// `forward` lands on Nuke's built-in `forward` layer; the UV source
    /// keeps its `U` / `V`.
    #[test]
    fn motion_vectors_write_lowercase_u_v() {
        let p = product(vec![
            var("forward", AovSource::MotionVector),
            var("st", AovSource::St),
        ]);
        assert_eq!(names(&p), ["forward.u", "forward.v", "st.U", "st.V"]);
        let mut prefixed = var("mv", AovSource::MotionVector);
        prefixed.channel_prefix = Some("backward".into());
        assert_eq!(
            names(&product(vec![prefixed])),
            ["backward.u", "backward.v"]
        );
    }

    #[test]
    fn a_channel_prefix_replaces_the_layer_and_a_clash_is_refused() {
        let mut beauty = var("beauty", AovSource::Color);
        beauty.channel_prefix = Some("rgba".into());
        let mut p_world = var("P", AovSource::P);
        p_world.channel_prefix = Some("Pw".into());
        let clash = var("Z", AovSource::Depth);
        let p = product(vec![beauty, p_world, var("Z", AovSource::Depth), clash]);
        assert_eq!(
            names(&p),
            ["rgba.R", "rgba.G", "rgba.B", "Pw.X", "Pw.Y", "Pw.Z", "Z"]
        );
    }

    #[test]
    fn a_product_is_written_scanline_zip_with_its_header() {
        let (w, h) = (3, 2);
        let mut beauty = Buffer::new(w, h);
        beauty.set_pixel(0, h - 1, crust_core::Vec3A::new(1.0, 2.0, 3.0));
        let film = crust_core::AovFilm::empty(w, h);
        let dir = std::env::temp_dir().join("crust_render_products_test/nested");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("p.exr");
        // A beauty-only product comes out of an empty film; the parent
        // directories do not exist yet.
        let p = product(vec![var("beauty", AovSource::Color)]);
        let rec709 = crate::tests::rec709();
        let written =
            write_product(&path, &p, &beauty, &film, &rec709, &sampling(), false).expect("written");
        assert_eq!(written, ["B", "G", "R"]);
        let image = read()
            .no_deep_data()
            .largest_resolution_level()
            .all_channels()
            .first_valid_layer()
            .all_attributes()
            .from_file(&path)
            .expect("reads back");
        let layer = &image.layer_data;
        assert_eq!(layer.encoding.blocks, Blocks::ScanLines);
        assert_eq!(layer.encoding.compression, Compression::ZIP16);
        assert_eq!(
            layer.attributes.other.get(&Text::from("colorInteropID")),
            Some(&AttributeValue::Text(Text::from("lin_rec709_scene")))
        );
        assert_eq!(
            layer.attributes.other.get(&Text::from("artist")),
            Some(&AttributeValue::Text(Text::from("someone")))
        );
        assert_eq!(layer.attributes.comments, Some(Text::from("a note")));
        // An authored colorInteropID does not replace crust's own: the
        // pixels are linear Rec.709 whatever the product says.
        assert_eq!(
            layer.attributes.other.get(&Text::from("colorInteropID")),
            Some(&AttributeValue::Text(Text::from("lin_rec709_scene")))
        );
        // The stamp carries the exposure scale the channels were written at.
        assert_eq!(
            layer
                .attributes
                .other
                .get(&Text::from("crust:exposureScale")),
            Some(&AttributeValue::F32(1.0))
        );
        // Top-down rows: the buffer's top row (y = h - 1) is the file's first.
        let r = &layer.channel_data.list[2];
        assert_eq!(r.name, Text::from("R"));
        assert_eq!(r.sample_data.value_by_flat_index(0).to_f32(), 1.0);
    }

    /// The `exr` crate's API for a crop (task 4.1 of `add-render-region`):
    /// the layer position is the data window's origin, the image's display
    /// window is the frame, and both read back as written.
    #[test]
    fn exr_writes_a_data_window_inside_the_display_window() {
        use exr::prelude::*;
        let path = std::env::temp_dir().join("crust_render_exr_windows.exr");
        let channels = SpecificChannels::rgb(|Vec2(x, _y)| (x as f32, 0.0f32, 0.0f32));
        let mut image = Image::from_channels((64, 32), channels);
        image.layer_data.attributes.layer_position = Vec2(100, 50);
        image.attributes.display_window = IntegerBounds::new((0, 0), (640, 360));
        image.write().to_file(&path).expect("written");
        let back = read_all_data_from_file(&path).expect("reads back");
        assert_eq!(
            back.attributes.display_window,
            IntegerBounds::new((0, 0), (640, 360))
        );
        let layer = &back.layer_data[0];
        assert_eq!(layer.attributes.layer_position, Vec2(100, 50));
        assert_eq!(layer.size, Vec2(64, 32));
    }

    /// A product of a cropped render: the display window is the frame, the
    /// data window the region (`(100, 50)–(163, 113)` inclusive, as EXR
    /// states it), and the pixels the region's, top-down.
    #[test]
    fn a_cropped_product_records_both_windows() {
        let region = crust_core::PixelRect::new(100, 50, 164, 114);
        let (fw, fh) = (640, 360);
        let mut beauty = Buffer::with_region(fw, fh, region);
        // The region's top-left pixel: image row 50 is raster row fh - 1 - 50.
        beauty.set_pixel(100, fh - 1 - 50, crust_core::Vec3A::new(1.0, 2.0, 3.0));
        let film = crust_core::AovFilm::empty(region.width(), region.height());
        let path = std::env::temp_dir().join("crust_render_cropped_product.exr");
        let p = product(vec![var("beauty", AovSource::Color)]);
        write_product(
            &path,
            &p,
            &beauty,
            &film,
            &crate::tests::rec709(),
            &sampling(),
            false,
        )
        .expect("written");
        let image = read()
            .no_deep_data()
            .largest_resolution_level()
            .all_channels()
            .first_valid_layer()
            .all_attributes()
            .from_file(&path)
            .expect("reads back");
        assert_eq!(
            image.attributes.display_window,
            IntegerBounds::new((0, 0), (fw, fh))
        );
        let layer = &image.layer_data;
        let data_window = layer.absolute_bounds();
        assert_eq!(data_window.position, Vec2(100, 50));
        assert_eq!(data_window.max(), Vec2(163, 113));
        let r = &layer.channel_data.list[2];
        assert_eq!(r.sample_data.value_by_flat_index(0).to_f32(), 1.0);
    }

    /// The log lines a test's code emits, captured.
    #[derive(Clone, Default)]
    struct Log(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl io::Write for Log {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Log {
        /// A log and a subscriber writing into it, for `with_default`.
        fn capture() -> (Log, impl tracing::Subscriber) {
            let log = Log::default();
            let writer = log.clone();
            let subscriber = tracing_subscriber::fmt()
                .with_writer(move || writer.clone())
                .with_ansi(false)
                .finish();
            (log, subscriber)
        }

        fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    /// A camera path EXR text cannot hold is left out of the header, said
    /// with a warning naming it, and the rest of the stamp is written.
    #[test]
    fn a_stamp_value_exr_cannot_hold_is_left_out_with_a_warning() {
        let (_, settings) = crust_core::get_settings();
        let sampling = SamplingStamp::new(
            &settings,
            &crust_core::RayStats::default(),
            Some("/cams/カメラ"),
            None,
        );
        let mut attributes = LayerAttributes::default();
        let (log, subscriber) = Log::capture();
        tracing::subscriber::with_default(subscriber, || stamp(&mut attributes, &sampling));
        assert!(!attributes.other.contains_key(&Text::from("crust:camera")));
        assert!(attributes.other.contains_key(&Text::from("crust:spp")));
        let log = log.text();
        assert!(
            log.contains("WARN") && log.contains("crust:camera"),
            "{log}"
        );
    }

    /// An interrupted render's product says so in its header; a completed
    /// one's carries no `crust:renderStatus` at all.
    #[test]
    fn an_interrupted_product_is_marked() {
        let (w, h) = (2, 2);
        let beauty = Buffer::new(w, h);
        let film = crust_core::AovFilm::empty(w, h);
        let p = product(vec![var("beauty", AovSource::Color)]);
        for interrupted in [false, true] {
            let path = std::env::temp_dir().join(format!("crust_render_status_{interrupted}.exr"));
            write_product(
                &path,
                &p,
                &beauty,
                &film,
                &crate::tests::rec709(),
                &sampling(),
                interrupted,
            )
            .expect("written");
            let image = read()
                .no_deep_data()
                .largest_resolution_level()
                .all_channels()
                .first_valid_layer()
                .all_attributes()
                .from_file(&path)
                .expect("reads back");
            let status = image
                .layer_data
                .attributes
                .other
                .get(&Text::from(RENDER_STATUS))
                .cloned();
            let expected = interrupted.then(|| AttributeValue::Text(Text::from("interrupted")));
            assert_eq!(status, expected, "interrupted {interrupted}");
        }
    }

    /// A product may not author crust's own stamp: the written value is the
    /// render's, and the authored one is warned about, naming the product.
    #[test]
    fn an_authored_stamp_attribute_is_replaced_and_warned_about() {
        let (log, subscriber) = Log::capture();
        let (w, h) = (2, 2);
        let beauty = Buffer::new(w, h);
        let film = crust_core::AovFilm::empty(w, h);
        let path = std::env::temp_dir().join("crust_render_authored_stamp.exr");
        let mut p = product(vec![var("beauty", AovSource::Color)]);
        p.attributes.push(("crust:spp".into(), "1024".into()));
        tracing::subscriber::with_default(subscriber, || {
            write_product(
                &path,
                &p,
                &beauty,
                &film,
                &crate::tests::rec709(),
                &sampling(),
                false,
            )
            .expect("written")
        });
        let image = read()
            .no_deep_data()
            .largest_resolution_level()
            .all_channels()
            .first_valid_layer()
            .all_attributes()
            .from_file(&path)
            .expect("reads back");
        let other = &image.layer_data.attributes.other;
        assert_eq!(
            other.get(&Text::from("crust:spp")),
            Some(&AttributeValue::I32(16))
        );
        assert_eq!(
            other.get(&Text::from("crust:sppTaken")),
            Some(&AttributeValue::IntVec2(Vec2(16, 16)))
        );
        let log = log.text();
        assert!(
            log.contains("WARN") && log.contains("/Render/p") && log.contains("crust:spp"),
            "{log}"
        );
    }

    #[test]
    fn a_product_sharing_an_earlier_path_is_refused() {
        let named = |prim: &str, name: &str| AovProduct {
            prim_path: prim.into(),
            name: name.into(),
            ..product(vec![var("beauty", AovSource::Color)])
        };
        let mut products = vec![
            named("/a", "out/a.exr"),
            named("/b", "out/./a.exr"),
            named("/c", "out/c.exr"),
        ];
        refuse_shared_paths(&mut products);
        let kept: Vec<_> = products.iter().map(|p| p.prim_path.as_str()).collect();
        assert_eq!(kept, ["/a", "/c"]);
    }

    #[test]
    fn samples_take_the_requested_precision() {
        assert!(matches!(
            samples(vec![0.5], Precision::Half),
            FlatSamples::F16(v) if v == [f16::from_f32(0.5)]
        ));
        assert!(matches!(
            samples(vec![0.5], Precision::Float),
            FlatSamples::F32(v) if v == [0.5]
        ));
        // `-1` ("no ID") is the all-ones bit pattern.
        assert!(matches!(
            samples(vec![-1.0, 7.0], Precision::Uint),
            FlatSamples::U32(v) if v == [0xFFFF_FFFF, 7]
        ));
    }

    fn expand(path: &str, frame: f64) -> (String, Vec<PathIssue>) {
        expand_frame_vars(path, frame)
    }

    fn unknown(name: &str) -> PathIssue {
        PathIssue::Unknown(name.to_owned())
    }

    #[test]
    fn a_padded_frame() {
        assert_eq!(
            expand("renders/beauty.$F4.exr", 12.0),
            ("renders/beauty.0012.exr".to_owned(), vec![])
        );
    }

    #[test]
    fn unpadded_float_and_braced_forms() {
        assert_eq!(expand("a.$F.${F3}.$FF.exr", 12.0).0, "a.12.012.12.exr");
    }

    #[test]
    fn udim_style_forms_are_the_same_variables() {
        assert_eq!(
            expand("renders/beauty.<F4>.exr", 12.0),
            ("renders/beauty.0012.exr".to_owned(), vec![])
        );
        assert_eq!(expand("raw.<F>.<FF>.exr", 12.0).0, "raw.12.12.exr");
    }

    #[test]
    fn other_angle_bracket_tokens_are_left_alone() {
        assert_eq!(
            expand("a.<UDIM>.<F4>.exr", 2.0),
            ("a.<UDIM>.0002.exr".to_owned(), vec![])
        );
        assert_eq!(expand("a<F4.exr", 2.0), ("a<F4.exr".to_owned(), vec![]));
        assert_eq!(expand("a.<F1>.exr", 2.0), ("a.<F1>.exr".to_owned(), vec![]));
    }

    #[test]
    fn a_negative_frame_counts_its_sign_in_the_width() {
        assert_eq!(expand("a.$F4.exr", -3.0).0, "a.-003.exr");
        assert_eq!(expand("a.$FF.exr", -3.0).0, "a.-3.exr");
    }

    #[test]
    fn frames_beyond_any_integer_type_keep_their_digits() {
        // 2^63 and -(2^63 + 2048): just past `i64`'s range on each side.
        assert_eq!(expand("$F", 9223372036854775808.0).0, "9223372036854775808");
        assert_eq!(
            expand("$F", -9223372036854777856.0).0,
            "-9223372036854777856"
        );
        assert_eq!(expand("$F4", 1e20).0, "100000000000000000000");
    }

    #[test]
    fn a_frame_rounding_to_zero_has_no_sign() {
        assert_eq!(expand("$F2", -0.4).0, "00");
        assert_eq!(expand("$F", -0.0).0, "0");
    }

    #[test]
    fn an_escaped_dollar() {
        assert_eq!(
            expand(r"cost\$F4.exr", 1.0),
            ("cost$F4.exr".to_owned(), vec![])
        );
    }

    #[test]
    fn a_subframe_rounds_integer_variables_and_is_reported() {
        assert_eq!(
            expand("a.$F4.exr", 12.5),
            ("a.0013.exr".to_owned(), vec![PathIssue::Subframe])
        );
        assert_eq!(
            expand("a.<F4>.exr", 12.5),
            ("a.0013.exr".to_owned(), vec![PathIssue::Subframe])
        );
        // Halves round away from zero on both sides.
        assert_eq!(expand("a.$F.exr", -12.5).0, "a.-13.exr");
        // Reported once per path, however many variables rounded.
        assert_eq!(expand("$F.$F4", 1.5).1, vec![PathIssue::Subframe]);
    }

    #[test]
    fn a_float_frame_keeps_its_fraction_silently() {
        assert_eq!(expand("a.$FF.exr", 12.5), ("a.12.5.exr".to_owned(), vec![]));
        assert_eq!(
            expand("a.<FF>.exr", 12.5),
            ("a.12.5.exr".to_owned(), vec![])
        );
        assert_eq!(expand("a.$FF.exr", -0.0).0, "a.0.exr");
    }

    #[test]
    fn unknown_names_are_kept_as_written_and_reported() {
        assert_eq!(
            expand("$SHOT/beauty.$F4.exr", 2.0),
            ("$SHOT/beauty.0002.exr".to_owned(), vec![unknown("$SHOT")])
        );
        assert_eq!(
            expand("${HIP}/a.exr", 2.0),
            ("${HIP}/a.exr".to_owned(), vec![unknown("${HIP}")])
        );
        // The longest name, as Houdini reads it: not `$F4` then `x`.
        assert_eq!(
            expand("a.$F4x.exr", 12.0),
            ("a.$F4x.exr".to_owned(), vec![unknown("$F4x")])
        );
    }

    #[test]
    fn a_dollar_with_no_name_is_literal() {
        assert_eq!(expand("price$.exr", 1.0), ("price$.exr".to_owned(), vec![]));
        assert_eq!(expand("a$1.exr", 1.0), ("a$1.exr".to_owned(), vec![]));
        assert_eq!(expand("a$", 1.0), ("a$".to_owned(), vec![]));
    }

    #[test]
    fn an_unclosed_brace_is_literal() {
        assert_eq!(expand("a.${F4.exr", 1.0), ("a.${F4.exr".to_owned(), vec![]));
        assert_eq!(expand("a.${}.exr", 1.0), ("a.${}.exr".to_owned(), vec![]));
    }

    #[test]
    fn a_windows_path_keeps_its_backslashes() {
        assert_eq!(
            expand(r"C:\renders\beauty.$F4.exr", 7.0).0,
            r"C:\renders\beauty.0007.exr"
        );
    }

    #[test]
    fn a_path_without_variables_is_unchanged() {
        for path in ["out.exr", "renders/é/beauty.0001.exr", "", "a\\b", "x>y"] {
            assert_eq!(expand(path, 3.0), (path.to_owned(), vec![]), "{path:?}");
        }
    }

    #[test]
    fn non_ascii_text_around_variables_survives() {
        assert_eq!(expand("é.$F2.ü<F>.exr", 4.0).0, "é.04.ü4.exr");
    }

    // --- Render pass file patterns (`render-passes` design D6, D7).

    fn inputs<'a>(frame: f64, layer: &'a str, output: &'a str) -> PatternInputs<'a> {
        PatternInputs {
            pass: "render",
            camera: "shotCam",
            frame,
            layer,
            output,
        }
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn pat(pattern: &str, frame: f64) -> String {
        Pattern::parse(pattern)
            .expand(&inputs(frame, "hero", "Diffuse"), &no_env)
            .unwrap()
    }

    #[test]
    fn every_pattern_specifier() {
        assert_eq!(
            pat("$l|$L|$n|$o|$f|$04f|$c|$C|$e|$E|$x", 3.0),
            "render|render|hero|Diffuse|3|0003|shotCam|shotCam|1|left|exr"
        );
        // Each specifier lower-cased by `$_`.
        assert_eq!(
            pat("$_l|$_n|$_o|$_c|$_E|$_04f|$_x", 3.0),
            "render|hero|diffuse|shotcam|left|0003|exr"
        );
        assert_eq!(pat("$l_$_o.$x", 1.0), "render_diffuse.exr");
    }

    #[test]
    fn a_pattern_with_no_dollar_is_unchanged() {
        for p in ["", "out.exr", "a/b c/ü.exr"] {
            assert_eq!(pat(p, 7.0), p);
            assert!(Pattern::parse(p).unknown().is_empty());
        }
    }

    #[test]
    fn environment_variables_in_a_pattern() {
        let env = |n: &str| (n == "IMAGES").then(|| "/tmp/images".to_owned());
        let pattern = Pattern::parse("$(IMAGES)/$l.$x");
        assert_eq!(
            pattern
                .expand(&inputs(1.0, "hero", "Beauty"), &env)
                .unwrap(),
            "/tmp/images/render.exr"
        );
        assert_eq!(
            pattern.expand(&inputs(1.0, "hero", "Beauty"), &no_env),
            Err(PatternError::UnsetVariable {
                pattern: "$(IMAGES)/$l.$x".into(),
                name: "IMAGES".into()
            })
        );
        let lowered = Pattern::parse("$_(SHOT).$x");
        let shot = |_: &str| Some("SH010".to_owned());
        assert_eq!(
            lowered.expand(&inputs(1.0, "a", "b"), &shot).unwrap(),
            "sh010.exr"
        );
    }

    #[test]
    fn frame_specifiers_in_a_pattern() {
        assert_eq!(pat("$l.$05f.$x", 12.0), "render.00012.exr");
        // Without -f the caller passes frame 1, as for a product path.
        assert_eq!(pat("$l.$04f.$x", 1.0), "render.0001.exr");
        assert_eq!(pat("$l.$04f.$x", -3.0), "render.-003.exr");
        assert_eq!(pat("$l.$f.$x", 1001.0), "render.1001.exr");
        // A subframe rounds halves away from zero, as `$F4` does.
        assert_eq!(pat("$l.$04f.$x", 12.5), "render.0013.exr");
        assert_eq!(pat("$l.$04f.$x", -12.5), "render.-013.exr");
        assert!(Pattern::parse("$l.$04f.$x").rounds(12.5));
        assert!(!Pattern::parse("$l.$04f.$x").rounds(12.0));
        assert!(!Pattern::parse("$l.$x").rounds(12.5));
    }

    #[test]
    fn unknown_specifiers_are_left_as_written() {
        assert_eq!(pat("$l_$q.$x", 1.0), "render_$q.exr");
        assert_eq!(Pattern::parse("$l_$q.$x").unknown(), ["$q"]);
        assert_eq!(pat("$SHOT/$l.$x", 1.0), "$SHOT/render.exr");
        assert_eq!(Pattern::parse("$SHOT/$l.$x").unknown(), ["$SHOT"]);
        // husk's frame variables are not this dialect's.
        assert_eq!(pat("$l.$F4.$x", 4.0), "render.$F4.exr");
        assert_eq!(Pattern::parse("$l.$F4.$x").unknown(), ["$F4"]);
        // A lone dollar is literal and unremarkable.
        assert_eq!(pat("$l_price$.$x", 1.0), "render_price$.exr");
        assert!(Pattern::parse("$l_price$.$x").unknown().is_empty());
        // `\$` is a literal dollar.
        assert_eq!(pat(r"cost\$n.$x", 1.0), "cost$n.exr");
        assert!(Pattern::parse(r"cost\$n.$x").unknown().is_empty());
    }

    fn pass(layers: &[&str], layer_pattern: &str) -> RenderPass {
        RenderPass {
            path: "/Render/Passes/render".into(),
            name: "render".into(),
            render_source: Some("/Render/settings".into()),
            layers: layers.iter().map(|l| (*l).to_owned()).collect(),
            file_pattern: crust_core::render_pass::DEFAULT_FILE_PATTERN.into(),
            layer_pattern: layer_pattern.into(),
        }
    }

    fn beauty_and_diffuse() -> Vec<AovVar> {
        let mut beauty = var("Beauty", AovSource::Color);
        beauty.components = 4;
        vec![beauty, var("Diffuse", AovSource::Albedo)]
    }

    fn files(pattern: &str, layer_pattern: &str) -> Result<Vec<AovProduct>, String> {
        pass_products(
            &pass(&["hero", "default"], layer_pattern),
            &beauty_and_diffuse(),
            pattern,
            "shotCam",
            3.0,
            &no_env,
        )
    }

    fn file_names(products: &[AovProduct]) -> Vec<&str> {
        products.iter().map(|p| p.name.as_str()).collect()
    }

    fn channels(product: &AovProduct) -> Vec<String> {
        product_channels(product)
            .into_iter()
            .flat_map(|(_, n)| n)
            .collect()
    }

    #[test]
    fn the_default_pattern_writes_one_file_per_layer_and_output() {
        let p = files(crust_core::render_pass::DEFAULT_FILE_PATTERN, "$n_$o").unwrap();
        assert_eq!(
            file_names(&p),
            [
                "render_hero_Beauty.0003.exr",
                "render_hero_Diffuse.0003.exr",
                "render_default_Beauty.0003.exr",
                "render_default_Diffuse.0003.exr",
            ]
        );
        // One output per file: a product's names, the beauty bare.
        assert_eq!(channels(&p[0]), ["R", "G", "B", "A"]);
        assert_eq!(p[0].vars[0].layer, Some(0));
        assert_eq!(p[3].vars[0].layer, Some(1));
        assert!(preview_layer(&p[0]).is_some());
        assert!(preview_layer(&p[1]).is_none(), "no beauty");
    }

    #[test]
    fn no_o_puts_a_layers_outputs_in_one_file() {
        let p = files("$l_$n.$x", "$n_$o").unwrap();
        assert_eq!(file_names(&p), ["render_hero.exr", "render_default.exr"]);
        assert_eq!(
            channels(&p[0]),
            [
                "hero_Beauty.R",
                "hero_Beauty.G",
                "hero_Beauty.B",
                "hero_Beauty.A",
                "hero_Diffuse.R",
                "hero_Diffuse.G",
                "hero_Diffuse.B",
            ]
        );
        // One layer and its beauty: a preview.
        assert!(preview_layer(&p[0]).is_some());
    }

    #[test]
    fn neither_n_nor_o_puts_everything_in_one_file() {
        let p = files("$l.$x", "$n_$o").unwrap();
        assert_eq!(file_names(&p), ["render.exr"]);
        let c = channels(&p[0]);
        for name in [
            "hero_Beauty.R",
            "hero_Beauty.A",
            "hero_Diffuse.R",
            "default_Beauty.R",
            "default_Diffuse.B",
        ] {
            assert!(c.iter().any(|n| n == name), "{name} in {c:?}");
        }
        assert_eq!(c.len(), 14);
        // Several layers: no preview.
        assert!(preview_layer(&p[0]).is_none());
        // Ordered by layer, then by var.
        let order: Vec<(Option<u16>, &str)> = p[0]
            .vars
            .iter()
            .map(|v| (v.layer, v.name.as_str()))
            .collect();
        assert_eq!(
            order,
            [
                (Some(0), "Beauty"),
                (Some(0), "Diffuse"),
                (Some(1), "Beauty"),
                (Some(1), "Diffuse")
            ]
        );
    }

    #[test]
    fn an_authored_layer_pattern() {
        let p = files("$l_$n.$x", "$_n.$_o").unwrap();
        let c = channels(&p[0]);
        assert!(c.iter().any(|n| n == "hero.diffuse.R"), "{c:?}");
        assert!(c.iter().any(|n| n == "hero.beauty.A"), "{c:?}");
    }

    #[test]
    fn two_outputs_with_one_layer_name_are_refused() {
        let e = files("$l_$n.$x", "$n").unwrap_err();
        assert!(
            e.contains("render_hero.exr") && e.contains("\"hero\""),
            "{e}"
        );
    }

    #[test]
    fn patterns_that_would_write_elsewhere_are_refused() {
        let e = files("$l_$n_$o.png", "$n_$o").unwrap_err();
        assert!(e.contains(".png"), "{e}");
        let e = files("$l_$n_$o", "$n_$o").unwrap_err();
        assert!(e.contains("none"), "{e}");
        let e = files("$(IMAGES)/$l.$x", "$n_$o").unwrap_err();
        assert!(e.contains("IMAGES"), "{e}");
        // Case does not matter.
        assert!(files("$l_$n_$o.EXR", "$n_$o").is_ok());
    }

    #[test]
    fn a_pass_without_vars_writes_its_beauty() {
        let p = pass_products(
            &pass(&["default"], "$n_$o"),
            &[],
            "$l_$n_$o.$x",
            "cam",
            1.0,
            &no_env,
        )
        .unwrap();
        assert_eq!(file_names(&p), ["render_default_Beauty.exr"]);
        assert_eq!(channels(&p[0]), ["R", "G", "B", "A"]);
    }

    #[test]
    fn pattern_warnings_name_the_pass() {
        let scope = crust_core::WarningScope::enter();
        let p = files("$l_$q.$04f.$x", "$n_$o").unwrap();
        assert_eq!(p[0].name, "render_$q.0003.exr");
        let _ = pass_products(
            &pass(&["default"], "$n_$o"),
            &[],
            "$l.$04f.$x",
            "cam",
            12.5,
            &no_env,
        )
        .unwrap();
        let warnings = scope.finish();
        let unknown = warnings
            .iter()
            .find(|w| w.code == crust_core::WarningCode::ProductUnknownVariable)
            .expect("an unknown-variable warning");
        assert!(unknown.message.contains("$q"), "{}", unknown.message);
        assert_eq!(unknown.prims, ["/Render/Passes/render"]);
        let subframe = warnings
            .iter()
            .find(|w| w.code == crust_core::WarningCode::ProductSubframeFrame)
            .expect("a subframe warning");
        assert!(subframe.message.contains("12.5"), "{}", subframe.message);
        assert_eq!(subframe.count, 1);
        // A lone dollar raises nothing.
        let scope = crust_core::WarningScope::enter();
        let _ = files("$l_price$.$x", "$n_$o");
        assert!(scope.finish().is_empty());
    }

    #[test]
    fn a_pass_whose_layers_need_too_many_expressions_is_refused() {
        let mut vars = beauty_and_diffuse();
        let lpe = |i: usize| {
            let mut v = var(&format!("L{i}"), AovSource::Lpe);
            v.expression = Some(format!("C<RD>.{{{i}}}[LO]"));
            v
        };
        vars.extend((0..crust_core::lpe::MAX_EXPRESSIONS - 1).map(lpe));
        let p = pass(&["default"], "$n_$o");
        assert!(pass_products(&p, &vars, "$l_$o.$x", "cam", 1.0, &no_env).is_ok());
        vars.push(lpe(999));
        let e = pass_products(&p, &vars, "$l_$o.$x", "cam", 1.0, &no_env).unwrap_err();
        assert!(e.contains("65"), "{e}");
    }

    #[test]
    fn pass_vars_keep_each_prim_once_in_product_order() {
        let a = var("A", AovSource::Albedo);
        let b = var("B", AovSource::Depth);
        let vars = pass_vars(&[product(vec![a.clone(), b.clone()]), product(vec![b, a])]);
        let names: Vec<&str> = vars.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, ["A", "B"]);
    }
}
