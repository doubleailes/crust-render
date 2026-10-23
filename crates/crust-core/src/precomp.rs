//! The precomp: a recipe, authored on the render settings, that rebuilds a
//! comp-driven look from a render's AOVs inside the renderer. See
//! `openspec/specs/precomp-filters/`.
//!
//! Two stages, both read off the stage at import (`usd_import/precomp.rs`):
//!
//! - **Sample filters** (`crust:sampleFilters`) are MaterialX value graphs
//!   compiled by crust-mtlx ([`crust_mtlx::compile_value_graph`]), evaluated
//!   on every camera sample from that sample's own AOV values, at the
//!   per-sample seam the AOVs already have (`UnitAov::add`). Their outputs
//!   accumulate with the beauty's pixel-filter weights, so a recombination is
//!   antialiased exactly as the beauty is.
//! - **Display filters** (`crust:displayFilters`) are a fixed set of image
//!   operations ([`Operation`]) run on the exposed, accumulated image, in list
//!   order, by [`run`] — after the render, and on every snapshot read when a
//!   displayed output is authored. Each output pixel is a sum taken in a fixed
//!   order over pixels that do not depend on how the rows were scheduled, so
//!   the result is bitwise independent of the thread count and of tiles
//!   versus scanlines.
//!
//! Filters only observe: no filter writes into the beauty sample, the beauty
//! buffer or the samples a pixel takes. Their outputs are new channels
//! (`AovSource::Filter`), and [`Precomp::displayed_var`] names the one a host
//! shows as *the* image.

use std::path::PathBuf;
use std::sync::Arc;

use crust_mtlx::{Program, ShadeCtx, Val};
use glam::Vec3A;
use rayon::prelude::*;

use crate::aov::{AovFilm, AovVar};
use crate::buffer::Buffer;

/// The JIT build of a program, or a stand-in type when there is none.
#[cfg(feature = "jit")]
type Jit = crust_jit::JitProgram;
#[cfg(not(feature = "jit"))]
type Jit = std::convert::Infallible;

/// A render's precomp: its sample and display filters, and the output a host
/// shows. Built once at import, shared read-only by every worker.
pub struct Precomp {
    pub(crate) sample: Vec<SampleFilter>,
    pub(crate) display: Vec<DisplayFilter>,
    /// The RenderVar `crust:display` names, when it names one.
    pub(crate) shown: Option<AovVar>,
    /// Where each sample filter's outputs start in the flat output list
    /// (filter `f`'s output `o` is `offsets[f] + o`).
    offsets: Vec<usize>,
}

impl std::fmt::Debug for Precomp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Precomp")
            .field("sample_filters", &self.sample_filter_paths())
            .field("display_filters", &self.display_filter_paths())
            .field("display", &self.display_name())
            .finish()
    }
}

/// Two precomps are equal when they read the same filters, in the same
/// order, and show the same output: what the stage authored, not the
/// compiled programs (which have no equality of their own).
impl PartialEq for Precomp {
    fn eq(&self, other: &Self) -> bool {
        self.sample_filter_paths() == other.sample_filter_paths()
            && self.display_filter_paths() == other.display_filter_paths()
            && self.display_name() == other.display_name()
    }
}

impl Precomp {
    pub(crate) fn new(
        sample: Vec<SampleFilter>,
        display: Vec<DisplayFilter>,
        shown: Option<AovVar>,
    ) -> Self {
        let mut offsets = Vec::with_capacity(sample.len());
        let mut n = 0;
        for f in &sample {
            offsets.push(n);
            n += f.outputs.len();
        }
        Precomp {
            sample,
            display,
            shown,
            offsets,
        }
    }

    /// The sample filters' prim paths, in evaluation order.
    pub fn sample_filter_paths(&self) -> Vec<String> {
        self.sample.iter().map(|f| f.path.clone()).collect()
    }

    /// The display filters' prim paths, in evaluation order.
    pub fn display_filter_paths(&self) -> Vec<String> {
        self.display.iter().map(|f| f.path.clone()).collect()
    }

    /// The displayed output's name: the RenderVar `crust:display` names, or
    /// `beauty` when it names none.
    pub fn display_name(&self) -> &str {
        self.shown.as_ref().map_or("beauty", |v| v.name.as_str())
    }

    /// The RenderVar a host shows as the image, when `crust:display` names
    /// one; `None` shows the beauty.
    pub fn displayed_var(&self) -> Option<&AovVar> {
        self.shown.as_ref()
    }

    /// Whether the precomp has a filter to run at all.
    pub fn has_filters(&self) -> bool {
        !self.sample.is_empty() || !self.display.is_empty()
    }

    /// How many outputs the sample filters have between them.
    pub(crate) fn sample_outputs(&self) -> usize {
        self.sample.iter().map(|f| f.outputs.len()).sum()
    }

    /// The flat index of sample filter `filter`'s output `output`.
    pub(crate) fn flat(&self, filter: u16, output: u16) -> usize {
        self.offsets[filter as usize] + output as usize
    }

    /// Each sample filter output's width, in flat order.
    pub(crate) fn output_widths(&self) -> Vec<u8> {
        self.sample
            .iter()
            .flat_map(|f| f.outputs.iter().map(|o| o.width))
            .collect()
    }

    /// Decodes every `crust:convolve` kernel through `load` — the import
    /// hands it its `AssetLoader`'s light-texture decode: crust-core decodes
    /// no image itself. A kernel `load` declines is `precomp.missing_kernel`,
    /// and its filter passes its input through. Called on the importing
    /// thread, once per import.
    pub fn load_kernels(
        &mut self,
        load: impl Fn(&std::path::Path) -> Option<Arc<crate::LightTexture>>,
    ) {
        for f in &mut self.display {
            if let Operation::Convolve {
                kernel_path: Some(path),
                kernel,
                ..
            } = &mut f.op
            {
                *kernel = load(path).map(|t| Kernel::from_texture(&t));
                if kernel.is_none() {
                    crate::warning!(
                        PrecompMissingKernel,
                        at = f.path,
                        "{}: kernel {} could not be read; the filter passes its input through",
                        f.path,
                        path.display()
                    );
                }
            }
        }
    }

    /// One line per filter, for `crust check`: what each filter is and what
    /// it reads.
    pub fn describe(&self) -> Vec<String> {
        let mut out = Vec::new();
        for f in &self.sample {
            let binds: Vec<String> = f
                .program
                .as_ref()
                .map(|p| {
                    p.primvars
                        .iter()
                        .zip(&f.binds)
                        .map(|(name, b)| format!("{name} = {}", b.describe(self)))
                        .collect()
                })
                .unwrap_or_default();
            out.push(format!(
                "sample {}{}: outputs [{}], inputs [{}]",
                f.path,
                if f.program.is_none() {
                    " (refused)"
                } else {
                    ""
                },
                f.outputs
                    .iter()
                    .map(|o| o.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                binds.join(", ")
            ));
        }
        for f in &self.display {
            out.push(format!(
                "display {}: {}",
                f.path,
                f.op.describe(self, &self.display)
            ));
        }
        out
    }
}

/// One compiled sample filter.
pub(crate) struct SampleFilter {
    pub(crate) path: String,
    /// The graph's program; `None` when it was refused
    /// (`precomp.unsupported_node`), so its outputs keep their clear value.
    pub(crate) program: Option<Program>,
    /// The JIT build of `program`, when built with `jit` and the shader JIT
    /// is on.
    pub(crate) jit: Option<Jit>,
    pub(crate) outputs: Vec<FilterOutput>,
    /// What each of the program's channels ([`Program::primvars`]) reads, in
    /// that order.
    pub(crate) binds: Vec<Bind>,
}

impl SampleFilter {
    /// A filter over `program`, with its JIT build when the shader JIT is on
    /// — the interpreter runs it otherwise, bit for bit the same.
    pub(crate) fn new(
        path: String,
        program: Option<Program>,
        outputs: Vec<FilterOutput>,
        binds: Vec<Bind>,
    ) -> Self {
        #[cfg(feature = "jit")]
        let jit = program.as_ref().and_then(|p| {
            crate::config()
                .shader_jit
                .then(|| match crust_jit::JitProgram::new(p) {
                    Ok(j) => Some(j),
                    Err(e) => {
                        tracing::debug!("{e}; sample filter {path} runs on the interpreter");
                        None
                    }
                })
                .flatten()
        });
        #[cfg(not(feature = "jit"))]
        let jit = None;
        SampleFilter {
            path,
            program,
            jit,
            outputs,
            binds,
        }
    }

    /// Evaluates the filter with `inputs` as its channels, into `slots`.
    pub(crate) fn eval(&self, inputs: &[Option<Val>], slots: &mut Vec<Val>) -> bool {
        let Some(program) = &self.program else {
            return false;
        };
        let ctx = ShadeCtx {
            uv: (0.0, 0.0),
            normal: Vec3A::Z,
            tangent: Vec3A::ZERO,
            view: -Vec3A::Z,
            position: Vec3A::ZERO,
            uv_width: 0.0,
            curve: None,
            primvars: inputs,
        };
        match &self.jit {
            #[cfg(feature = "jit")]
            Some(j) => j.eval(&ctx, slots),
            #[cfg(not(feature = "jit"))]
            Some(never) => match *never {},
            None => program.eval(&ctx, slots),
        }
        true
    }
}

/// One output of a sample filter: its name on the graph, its slot in an
/// evaluation, and its width (1 to 4).
#[derive(Debug, Clone)]
pub(crate) struct FilterOutput {
    pub(crate) name: String,
    pub(crate) slot: u32,
    pub(crate) width: u8,
}

/// What one sample-filter channel reads, per camera sample.
#[derive(Debug, Clone)]
pub(crate) enum Bind {
    /// The sample's radiance, after the firefly clamp and before exposure.
    Beauty,
    /// The sample's own value of a RenderVar, whatever its accumulation.
    Var(AovVar),
    /// An earlier sample filter's output, this sample's.
    Sample { filter: u16, output: u16 },
    /// Nothing: the input reads its authored default.
    Default,
}

impl Bind {
    fn describe(&self, pc: &Precomp) -> String {
        match self {
            Bind::Beauty => "beauty".into(),
            Bind::Var(v) => format!("var {}", v.prim_path),
            Bind::Sample { filter, output } => sample_name(pc, *filter, *output),
            Bind::Default => "its default".into(),
        }
    }
}

/// `<graph>.<output>`: how a display filter names a sample-filter output.
fn sample_name(pc: &Precomp, filter: u16, output: u16) -> String {
    let f = &pc.sample[filter as usize];
    let graph = f.path.rsplit('/').next().unwrap_or(&f.path);
    format!("{graph}.{}", f.outputs[output as usize].name)
}

/// One display filter.
pub(crate) struct DisplayFilter {
    pub(crate) path: String,
    pub(crate) op: Operation,
    /// How many channels its output has.
    pub(crate) width: u8,
}

/// An image a display filter reads.
#[derive(Debug, Clone)]
pub(crate) enum Image {
    /// A RenderVar's channels, as the product writes them (exposed).
    Var(AovVar),
    /// A sample filter's output, exposed as radiance.
    Sample { filter: u16, output: u16 },
    /// An earlier display filter's output.
    Display(u16),
    /// The clear value: one channel of zeros.
    Clear,
}

impl Image {
    fn describe(&self, pc: &Precomp, display: &[DisplayFilter]) -> String {
        match self {
            Image::Var(v) => format!("var {}", v.prim_path),
            Image::Sample { filter, output } => sample_name(pc, *filter, *output),
            Image::Display(i) => display[*i as usize].path.clone(),
            Image::Clear => "clear".into(),
        }
    }
}

/// How `crust:vectorBlur` finds each pixel's blur vector, in pixels, `x`
/// right and `y` up (the `motionvector` AOV's convention).
#[derive(Debug, Clone)]
pub(crate) enum Field {
    /// Per pixel, from an image's first two channels.
    Vectors(Image),
    /// One direction for every pixel.
    Direction([f32; 2]),
    /// From the centre (in pixels from the region's bottom-left) through the
    /// pixel, `strength` times the pixel's distance from it: a zoom blur.
    Radial {
        center: Option<[f32; 2]>,
        strength: f32,
    },
}

/// `crust:merge`'s operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MergeMode {
    /// `b + a·(1 − α)`, α from the `alpha` input (1 when unconnected).
    Over,
    Plus,
    Multiply,
    Screen,
}

impl std::str::FromStr for MergeMode {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, ()> {
        Ok(match s {
            "over" => MergeMode::Over,
            "plus" => MergeMode::Plus,
            "multiply" => MergeMode::Multiply,
            "screen" => MergeMode::Screen,
            _ => return Err(()),
        })
    }
}

/// A convolution kernel, decoded: `taps[y * w + x]`, top row first.
#[derive(Debug, Clone)]
pub(crate) struct Kernel {
    pub(crate) w: usize,
    pub(crate) h: usize,
    pub(crate) taps: Vec<[f32; 3]>,
}

impl Kernel {
    fn from_texture(t: &crate::LightTexture) -> Self {
        let (w, h) = (t.width(), t.height());
        let taps = (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .map(|(x, y)| {
                t.texel((x as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32)
                    .to_array()
            })
            .collect();
        Kernel { w, h, taps }
    }
}

/// What a display filter does.
#[derive(Debug, Clone)]
pub(crate) enum Operation {
    /// An unknown operation, refused: the input unchanged.
    PassThrough(Image),
    Convolve {
        input: Image,
        kernel_path: Option<PathBuf>,
        kernel: Option<Kernel>,
        /// Output pixels per kernel texel.
        scale: f32,
        normalize: bool,
    },
    Blur {
        input: Image,
        radius: f32,
    },
    VectorBlur {
        input: Image,
        field: Field,
        taps: u32,
    },
    Merge {
        a: Image,
        b: Image,
        alpha: Option<Image>,
        mode: MergeMode,
        mix: f32,
    },
    Grade {
        input: Image,
        lift: [f32; 3],
        gain: [f32; 3],
        gamma: [f32; 3],
        offset: [f32; 3],
    },
}

impl Operation {
    fn describe(&self, pc: &Precomp, display: &[DisplayFilter]) -> String {
        let d = |i: &Image| i.describe(pc, display);
        match self {
            Operation::PassThrough(i) => format!("pass-through of {}", d(i)),
            Operation::Convolve {
                input, kernel_path, ..
            } => format!(
                "crust:convolve of {} by {}",
                d(input),
                kernel_path
                    .as_ref()
                    .map_or("no kernel".into(), |p| p.display().to_string())
            ),
            Operation::Blur { input, radius } => {
                format!("crust:blur of {} (radius {radius})", d(input))
            }
            Operation::VectorBlur { input, field, taps } => format!(
                "crust:vectorBlur of {} along {} ({taps} taps)",
                d(input),
                match field {
                    Field::Vectors(v) => d(v),
                    Field::Direction([x, y]) => format!("({x}, {y})"),
                    Field::Radial { strength, .. } => format!("radial, strength {strength}"),
                }
            ),
            Operation::Merge { a, b, mode, .. } => {
                format!("crust:merge {mode:?} of {} and {}", d(a), d(b))
            }
            Operation::Grade { input, .. } => format!("crust:grade of {}", d(input)),
        }
    }
}

/// A full-frame image: one plane per channel, each `w × h`, top-down rows —
/// the layout [`AovFilm::var_channels`] returns and the EXR writer takes.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Plane {
    pub(crate) w: usize,
    pub(crate) h: usize,
    pub(crate) channels: Vec<Vec<f32>>,
}

impl Plane {
    fn clear(w: usize, h: usize) -> Self {
        Plane {
            w,
            h,
            channels: vec![vec![0.0; w * h]],
        }
    }

    /// Channel `c`, the last one standing in for any past it (a grey image
    /// read as a colour is grey in every channel).
    fn channel(&self, c: usize) -> &[f32] {
        &self.channels[c.min(self.channels.len() - 1)]
    }

    /// The value of channel `c` at `(x, y)`, clamped to the edge.
    #[inline]
    fn at(&self, c: &[f32], x: isize, y: isize) -> f32 {
        let x = x.clamp(0, self.w as isize - 1) as usize;
        let y = y.clamp(0, self.h as isize - 1) as usize;
        c[y * self.w + x]
    }

    /// Bilinear lookup at continuous `(x, y)` (pixel centres at `i + 0.5`),
    /// edges clamped.
    #[inline]
    fn bilinear(&self, c: &[f32], x: f32, y: f32) -> f32 {
        let (fx, fy) = (x - 0.5, y - 0.5);
        let (x0, y0) = (fx.floor(), fy.floor());
        let (tx, ty) = (fx - x0, fy - y0);
        let (x0, y0) = (x0 as isize, y0 as isize);
        let a = self.at(c, x0, y0);
        let b = self.at(c, x0 + 1, y0);
        let d = self.at(c, x0, y0 + 1);
        let e = self.at(c, x0 + 1, y0 + 1);
        let top = a + (b - a) * tx;
        let bottom = d + (e - d) * tx;
        top + (bottom - top) * ty
    }

    /// A plane of `channels` channels, each row computed by `row(c, y, out)`
    /// — rows in parallel, each pixel in a fixed order, so the result does
    /// not depend on the thread count.
    fn build(
        w: usize,
        h: usize,
        channels: usize,
        row: impl Fn(usize, usize, &mut [f32]) + Sync,
    ) -> Plane {
        let channels = (0..channels)
            .map(|c| {
                let mut out = vec![0.0f32; w * h];
                out.par_chunks_mut(w.max(1))
                    .enumerate()
                    .for_each(|(y, r)| row(c, y, r));
                out
            })
            .collect();
        Plane { w, h, channels }
    }
}

/// Runs the display chain of `pc` over `film`, beside the exposed `beauty`
/// rendered with it: each display filter in list order, its output kept in
/// the film for the vars that read it ([`AovFilm::var_channels`]) and for
/// later filters. The film must already carry the camera's exposure
/// ([`AovFilm::apply_exposure`]).
pub(crate) fn run(pc: &Precomp, beauty: &Buffer, film: &mut AovFilm) {
    let start = std::time::Instant::now();
    film.clear_display();
    for f in &pc.display {
        let out = apply(pc, &f.op, beauty, film);
        film.push_display(out);
    }
    film.set_display_seconds(start.elapsed().as_secs_f64());
}

/// The image `image` names, as a plane over the film's region.
fn read(pc: &Precomp, image: &Image, beauty: &Buffer, film: &AovFilm) -> Plane {
    let (w, h) = film.dimensions();
    match image {
        Image::Var(v) => Plane {
            w,
            h,
            channels: film.var_channels(beauty, v),
        },
        Image::Sample { filter, output } => Plane {
            w,
            h,
            channels: film.sample_output_channels(pc.flat(*filter, *output), true),
        },
        Image::Display(i) => film.display_plane(*i as usize).clone(),
        Image::Clear => Plane::clear(w, h),
    }
}

/// One display filter's output.
fn apply(pc: &Precomp, op: &Operation, beauty: &Buffer, film: &AovFilm) -> Plane {
    let get = |i: &Image| read(pc, i, beauty, film);
    match op {
        Operation::PassThrough(i) => get(i),
        Operation::Convolve {
            input,
            kernel,
            scale,
            normalize,
            ..
        } => {
            let src = get(input);
            match kernel {
                Some(k) => convolve(&src, k, *scale, *normalize),
                None => src,
            }
        }
        Operation::Blur { input, radius } => blur(&get(input), *radius),
        Operation::VectorBlur { input, field, taps } => {
            let src = get(input);
            let vectors = match field {
                Field::Vectors(v) => Some(get(v)),
                _ => None,
            };
            vector_blur(&src, field, vectors.as_ref(), *taps)
        }
        Operation::Merge {
            a,
            b,
            alpha,
            mode,
            mix,
        } => {
            let alpha = alpha.as_ref().map(get);
            merge(&get(a), &get(b), alpha.as_ref(), *mode, *mix)
        }
        Operation::Grade {
            input,
            lift,
            gain,
            gamma,
            offset,
        } => grade(&get(input), *lift, *gain, *gamma, *offset),
    }
}

/// Convolves every channel with the kernel's matching channel (its last for
/// any past three), each texel covering `scale` × `scale` output pixels
/// (nearest), centred on the pixel. Normalised per channel to sum 1 unless
/// `normalize` is off or the channel sums to 0. Edges clamp.
pub(crate) fn convolve(src: &Plane, k: &Kernel, scale: f32, normalize: bool) -> Plane {
    let s = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let fw = ((k.w as f32 * s).round() as usize).max(1);
    let fh = ((k.h as f32 * s).round() as usize).max(1);
    // The footprint's taps per kernel channel, row-major, top row first.
    let weights: Vec<Vec<f32>> = (0..3)
        .map(|c| {
            let mut t: Vec<f32> = (0..fh)
                .flat_map(|j| (0..fw).map(move |i| (i, j)))
                .map(|(i, j)| {
                    let kx = (i * k.w / fw).min(k.w - 1);
                    let ky = (j * k.h / fh).min(k.h - 1);
                    k.taps[ky * k.w + kx][c]
                })
                .collect();
            let sum: f32 = t.iter().sum();
            if normalize && sum != 0.0 {
                t.iter_mut().for_each(|x| *x /= sum);
            }
            t
        })
        .collect();
    let (cx, cy) = ((fw / 2) as isize, (fh / 2) as isize);
    Plane::build(src.w, src.h, src.channels.len(), |c, y, out| {
        let ch = src.channel(c);
        let wt = &weights[c.min(2)];
        for (x, o) in out.iter_mut().enumerate() {
            let mut acc = 0.0f32;
            for j in 0..fh {
                for i in 0..fw {
                    let v = src.at(
                        ch,
                        x as isize + i as isize - cx,
                        y as isize + j as isize - cy,
                    );
                    acc += wt[j * fw + i] * v;
                }
            }
            *o = acc;
        }
    })
}

/// A separable gaussian of standard deviation `radius / 3`, truncated at
/// `±ceil(radius)` pixels and normalised; edges clamp. A radius of 0 or less
/// is the identity.
pub(crate) fn blur(src: &Plane, radius: f32) -> Plane {
    if !(radius.is_finite() && radius > 0.0) {
        return src.clone();
    }
    let n = radius.ceil() as isize;
    let sigma = radius / 3.0;
    let mut taps: Vec<f32> = (-n..=n)
        .map(|i| (-0.5 * (i as f32 / sigma).powi(2)).exp())
        .collect();
    let sum: f32 = taps.iter().sum();
    taps.iter_mut().for_each(|t| *t /= sum);
    let horizontal = Plane::build(src.w, src.h, src.channels.len(), |c, y, out| {
        let ch = src.channel(c);
        for (x, o) in out.iter_mut().enumerate() {
            let mut acc = 0.0f32;
            for (k, t) in taps.iter().enumerate() {
                acc += t * src.at(ch, x as isize + k as isize - n, y as isize);
            }
            *o = acc;
        }
    });
    Plane::build(src.w, src.h, src.channels.len(), |c, y, out| {
        let ch = &horizontal.channels[c];
        for (x, o) in out.iter_mut().enumerate() {
            let mut acc = 0.0f32;
            for (k, t) in taps.iter().enumerate() {
                acc += t * horizontal.at(ch, x as isize, y as isize + k as isize - n);
            }
            *o = acc;
        }
    })
}

/// The mean of `taps` bilinear lookups along each pixel's blur vector,
/// centred on the pixel (from `−v/2` to `+v/2`). A pixel whose vector is zero
/// keeps its value exactly.
pub(crate) fn vector_blur(src: &Plane, field: &Field, vectors: Option<&Plane>, taps: u32) -> Plane {
    let taps = taps.max(1);
    let (w, h) = (src.w, src.h);
    let center = match field {
        Field::Radial { center, .. } => center.unwrap_or([w as f32 * 0.5, h as f32 * 0.5]),
        _ => [0.0, 0.0],
    };
    // The blur vector at top-down pixel (x, y), in pixels, x right, y up.
    let vector = |x: usize, y: usize| -> [f32; 2] {
        match field {
            Field::Vectors(_) => {
                let v = vectors.expect("a vector field reads its image");
                let i = y * w + x;
                [v.channel(0)[i], v.channel(1)[i]]
            }
            Field::Direction(d) => *d,
            Field::Radial { strength, .. } => {
                // Pixel centre, y up from the bottom row.
                let px = x as f32 + 0.5;
                let py = (h - 1 - y) as f32 + 0.5;
                [(px - center[0]) * strength, (py - center[1]) * strength]
            }
        }
    };
    Plane::build(w, h, src.channels.len(), |c, y, out| {
        let ch = src.channel(c);
        for (x, o) in out.iter_mut().enumerate() {
            let [vx, vy] = vector(x, y);
            if vx == 0.0 && vy == 0.0 || !(vx.is_finite() && vy.is_finite()) {
                *o = ch[y * w + x];
                continue;
            }
            let (cx, cy) = (x as f32 + 0.5, y as f32 + 0.5);
            let mut acc = 0.0f32;
            for k in 0..taps {
                let t = (k as f32 + 0.5) / taps as f32 - 0.5;
                // Image rows run down: `y` up is a smaller row.
                acc += src.bilinear(ch, cx + t * vx, cy - t * vy);
            }
            *o = acc / taps as f32;
        }
    })
}

/// A per-pixel composite of `a` and `b`, faded from `a` by `mix` (1 is the
/// composite exactly).
pub(crate) fn merge(
    a: &Plane,
    b: &Plane,
    alpha: Option<&Plane>,
    mode: MergeMode,
    mix: f32,
) -> Plane {
    let n = a.channels.len().max(b.channels.len());
    Plane::build(a.w, a.h, n, |c, y, out| {
        let (ca, cb) = (a.channel(c), b.channel(c));
        let al = alpha.map(|p| p.channel(0));
        for (x, o) in out.iter_mut().enumerate() {
            let i = y * a.w + x;
            let (va, vb) = (ca[i], cb[i]);
            let r = match mode {
                MergeMode::Over => {
                    let al = al.map_or(1.0, |p| p[i]);
                    vb + va * (1.0 - al)
                }
                MergeMode::Plus => va + vb,
                MergeMode::Multiply => va * vb,
                MergeMode::Screen => va + vb - va * vb,
            };
            *o = if mix == 1.0 { r } else { va + (r - va) * mix };
        }
    })
}

/// Nuke's `Grade` with its black and white points at 0 and 1:
/// `(gain − lift)·x + offset + lift`, raised to `1/gamma` where positive.
/// The defaults (lift 0, gain 1, gamma 1, offset 0) are the identity, bit for
/// bit.
pub(crate) fn grade(
    src: &Plane,
    lift: [f32; 3],
    gain: [f32; 3],
    gamma: [f32; 3],
    offset: [f32; 3],
) -> Plane {
    Plane::build(src.w, src.h, src.channels.len(), |c, y, out| {
        let ch = src.channel(c);
        let k = c.min(2);
        let a = gain[k] - lift[k];
        let b = offset[k] + lift[k];
        let g = gamma[k];
        for (x, o) in out.iter_mut().enumerate() {
            let mut v = a * ch[y * src.w + x] + b;
            if g != 1.0 && g > 0.0 && v > 0.0 {
                v = v.powf(1.0 / g);
            }
            *o = v;
        }
    })
}

/// The image a host shows for a render with `pc`: the displayed output when
/// one is authored, as colour (a one-channel output as grey, a two-channel
/// one as red and green), over `beauty`'s region; `None` shows the beauty.
/// `film` must have had [`run`] over it.
pub fn displayed(pc: Option<&Precomp>, beauty: &Buffer, film: &AovFilm) -> Option<Buffer> {
    let var = pc?.displayed_var()?;
    let channels = film.var_channels(beauty, var);
    let (w, h) = beauty.size();
    let mut out = beauty.black_like();
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let at = |c: usize| channels.get(c).map_or(0.0, |p| p[i]);
            let rgb = match channels.len() {
                1 => Vec3A::splat(at(0)),
                _ => Vec3A::new(at(0), at(1), at(2)),
            };
            out.set_rgb(x, y, rgb);
        }
    }
    Some(out)
}

/// What a snapshot needs to show the displayed output while the render runs:
/// the film the render's units publish their AOVs into, the precomp to run
/// over it, and the exposure to apply first ([`crate::RenderControl`]).
pub(crate) struct Snapshot {
    pub(crate) film: AovFilm,
    pub(crate) precomp: Arc<Precomp>,
    pub(crate) exposure: f32,
}

impl Snapshot {
    /// The displayed output over the exposed `beauty` and this film's
    /// state: the display chain run over a copy, so a blur reads unfinished
    /// units as their clear value.
    pub(crate) fn show(&self, beauty: &Buffer) -> Buffer {
        let mut film = self.film.clone();
        if self.exposure != 1.0 {
            film.apply_exposure(self.exposure);
        }
        run(&self.precomp, beauty, &mut film);
        displayed(Some(&self.precomp), beauty, &film).unwrap_or_else(|| beauty.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plane(w: usize, h: usize, f: impl Fn(usize, usize) -> f32) -> Plane {
        Plane {
            w,
            h,
            channels: vec![(0..w * h).map(|i| f(i % w, i / w)).collect()],
        }
    }

    #[test]
    fn grade_defaults_are_the_identity_and_the_formula_is_nukes() {
        let src = plane(3, 2, |x, y| x as f32 * 0.7 - y as f32 * 0.3);
        let id = grade(&src, [0.0; 3], [1.0; 3], [1.0; 3], [0.0; 3]);
        assert_eq!(id, src);
        let g = grade(&src, [0.1; 3], [2.0; 3], [2.0; 3], [0.05; 3]);
        for (o, x) in g.channels[0].iter().zip(&src.channels[0]) {
            let v = 1.9 * x + 0.15;
            let want = if v > 0.0 { v.powf(0.5) } else { v };
            assert!((o - want).abs() < 1e-6, "{o} vs {want}");
        }
    }

    #[test]
    fn merge_operations_match_their_formulas() {
        let a = plane(2, 1, |x, _| [0.25, 0.5][x]);
        let b = plane(2, 1, |x, _| [0.5, 1.0][x]);
        let al = plane(2, 1, |x, _| [0.5, 0.0][x]);
        let at = |p: &Plane| p.channels[0].clone();
        assert_eq!(
            at(&merge(&a, &b, None, MergeMode::Plus, 1.0)),
            vec![0.75, 1.5]
        );
        assert_eq!(
            at(&merge(&a, &b, None, MergeMode::Multiply, 1.0)),
            vec![0.125, 0.5]
        );
        assert_eq!(
            at(&merge(&a, &b, None, MergeMode::Screen, 1.0)),
            vec![0.625, 1.0]
        );
        assert_eq!(
            at(&merge(&a, &b, Some(&al), MergeMode::Over, 1.0)),
            vec![0.625, 1.5]
        );
        assert_eq!(
            at(&merge(&a, &b, None, MergeMode::Over, 1.0)),
            vec![0.5, 1.0]
        );
        // Half way from `a` to the sum.
        assert_eq!(
            at(&merge(&a, &b, None, MergeMode::Plus, 0.5)),
            vec![0.5, 1.0]
        );
    }

    #[test]
    fn blur_matches_a_hand_computed_gaussian_and_keeps_a_constant() {
        // An impulse in the middle of a 7x1 row: the row of taps, which sum
        // to 1.
        let src = plane(7, 1, |x, _| if x == 3 { 1.0 } else { 0.0 });
        let out = blur(&src, 1.5);
        let sigma = 0.5f32;
        let raw: Vec<f32> = (-2..=2)
            .map(|i: i32| (-0.5 * (i as f32 / sigma).powi(2)).exp())
            .collect();
        let sum: f32 = raw.iter().sum();
        for (k, r) in raw.iter().enumerate() {
            let got = out.channels[0][1 + k];
            assert!((got - r / sum).abs() < 1e-6, "tap {k}: {got}");
        }
        assert_eq!(out.channels[0][0], 0.0);
        let flat = plane(5, 4, |_, _| 0.75);
        for v in &blur(&flat, 2.0).channels[0] {
            assert!((v - 0.75).abs() < 1e-6);
        }
        assert_eq!(blur(&src, 0.0), src);
    }

    #[test]
    fn convolve_with_a_box_kernel_averages_and_edges_clamp() {
        let k = Kernel {
            w: 3,
            h: 1,
            taps: vec![[1.0; 3]; 3],
        };
        let src = plane(4, 1, |x, _| x as f32);
        let out = convolve(&src, &k, 1.0, true);
        let want = [
            (0.0 + 0.0 + 1.0) / 3.0,
            (0.0 + 1.0 + 2.0) / 3.0,
            (1.0 + 2.0 + 3.0) / 3.0,
            (2.0 + 3.0 + 3.0) / 3.0,
        ];
        for (o, w) in out.channels[0].iter().zip(want) {
            assert!((o - w).abs() < 1e-6, "{o} vs {w}");
        }
        // Unnormalised: the sum.
        let sum = convolve(&src, &k, 1.0, false);
        assert!((sum.channels[0][1] - 3.0).abs() < 1e-6);
    }

    #[test]
    fn vector_blur_leaves_static_pixels_and_smears_moving_ones() {
        let src = plane(8, 1, |x, _| if x == 4 { 1.0 } else { 0.0 });
        // Zero motion everywhere: the image, bit for bit.
        let still = plane(8, 1, |_, _| 0.0);
        let vectors = Plane {
            w: 8,
            h: 1,
            channels: vec![still.channels[0].clone(), still.channels[0].clone()],
        };
        let field = Field::Vectors(Image::Clear);
        assert_eq!(vector_blur(&src, &field, Some(&vectors), 8), src);
        // Four pixels right: the impulse spreads over its neighbours, and
        // its energy is kept away from the edges.
        let out = vector_blur(&src, &Field::Direction([4.0, 0.0]), None, 16);
        let total: f32 = out.channels[0].iter().sum();
        assert!((total - 1.0).abs() < 1e-5, "{total}");
        assert!(out.channels[0][4] < 1.0 && out.channels[0][3] > 0.0);
        assert!(out.channels[0][5] > 0.0);
    }

    #[test]
    fn a_radial_blur_leaves_the_centre_alone() {
        let src = plane(5, 5, |x, y| (x * 5 + y) as f32);
        let out = vector_blur(
            &src,
            &Field::Radial {
                center: None,
                strength: 0.3,
            },
            None,
            8,
        );
        // The middle pixel's centre is the image centre: no vector.
        assert_eq!(out.channels[0][12], src.channels[0][12]);
        assert_ne!(out.channels[0][0], src.channels[0][0]);
    }

    #[test]
    fn display_operations_are_independent_of_the_thread_count() {
        let src = plane(37, 23, |x, y| ((x * 31 + y * 17) % 13) as f32 * 0.1);
        let k = Kernel {
            w: 3,
            h: 3,
            taps: (0..9).map(|i| [i as f32, 1.0, 0.5]).collect(),
        };
        let all = |p: &Plane| {
            vec![
                blur(p, 2.5),
                convolve(p, &k, 1.5, true),
                vector_blur(p, &Field::Direction([3.0, -2.0]), None, 7),
                vector_blur(
                    p,
                    &Field::Radial {
                        center: Some([4.0, 9.0]),
                        strength: 0.2,
                    },
                    None,
                    5,
                ),
            ]
        };
        let one = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap()
            .install(|| all(&src));
        let many = rayon::ThreadPoolBuilder::new()
            .num_threads(4)
            .build()
            .unwrap()
            .install(|| all(&src));
        for (a, b) in one.iter().zip(&many) {
            let bits =
                |p: &Plane| -> Vec<u32> { p.channels[0].iter().map(|v| v.to_bits()).collect() };
            assert_eq!(bits(a), bits(b));
        }
    }
}
