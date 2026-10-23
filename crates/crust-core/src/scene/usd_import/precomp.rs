//! The render settings' precomp → [`Precomp`]: `crust:sampleFilters`,
//! `crust:displayFilters` and `crust:display` on the `RenderSettings` prim the
//! render reads (after the active pass chose it), and the `crust:filter`
//! RenderVars that write their outputs. See `openspec/specs/precomp-filters/`.
//!
//! Read on the index stage with the products ([`super::products`]), so a
//! full import, an in-place settings update and `crust check` read the same
//! precomp. Every refusal is a `precomp.*` warning and never fails the render:
//! a refused sample filter's outputs read their clear value, a refused display
//! operation passes its input through, and an unbound input reads its default.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use openusd::sdf;
use openusd::usd::{Prim, Stage};
use tracing::debug;

use crate::aov::{AovRequest, AovSource, AovVar, FilterSource, Precision};
use crate::precomp::{
    Bind, DisplayFilter, Field, FilterOutput, Image, MergeMode, Operation, Precomp, SampleFilter,
};
use crate::warning;

use super::assets::asset_path;
use super::attrs::{custom_bool, custom_f32, custom_i32, custom_token, decode_vec3, prim_value};
use super::materials::shader_info_id;
use super::mtlx_network::translate_value_graph;
use super::prim_at;
use super::products::FilterTarget;

/// The name a sample filter input reads the beauty radiance by.
const BEAUTY: &str = "beauty";

/// The precomp the settings prim `settings` authors, resolving the filter
/// vars of `request` (`targets`, by var prim path) into their filters and
/// dropping the ones that cannot be. `None` when the prim authors no filter
/// list and no `crust:display`.
pub(super) fn import_precomp(
    stage: &Stage,
    stage_path: &Path,
    settings: &sdf::Path,
    request: &mut AovRequest,
    targets: &HashMap<String, FilterTarget>,
) -> Option<Arc<Precomp>> {
    let prim = prim_at(stage, settings.clone());
    let list = |name: &str| -> Option<Vec<sdf::Path>> {
        let rel = prim.relationship(name);
        rel.has_authored_targets()
            .unwrap_or(false)
            .then(|| rel.targets().unwrap_or_default())
    };
    let sample_paths = list("crust:sampleFilters");
    let display_paths = list("crust:displayFilters");
    let display_name = custom_token(&prim, "crust:display").filter(|d| !d.is_empty());
    if sample_paths.is_none() && display_paths.is_none() && display_name.is_none() {
        // Filter vars with no filter to read.
        drop_unresolved(request, targets, &[], &[]);
        return None;
    }
    let sample_paths = sample_paths.unwrap_or_default();
    let display_paths = display_paths.unwrap_or_default();

    // The sample filters' graphs first: a var reading one needs its outputs.
    let graphs: Vec<Graph> = sample_paths
        .iter()
        .map(|p| Graph::read(stage, stage_path, p))
        .collect();
    drop_unresolved(request, targets, &graphs, &display_paths);
    let vars = Vars::of(request);

    let mut sample = Vec::with_capacity(graphs.len());
    for (index, graph) in graphs.into_iter().enumerate() {
        sample.push(graph.bind(index as u16, &vars));
    }
    let partial = Precomp::new(sample, Vec::new(), None);
    let mut display: Vec<DisplayFilter> = Vec::with_capacity(display_paths.len());
    for (index, path) in display_paths.iter().enumerate() {
        let filter = read_display(
            stage,
            stage_path,
            path,
            index as u16,
            &display_paths,
            &partial,
            &display,
            &vars,
        );
        display.push(filter);
    }
    let Precomp { sample, .. } = partial;
    check_display_widths(request, &display);

    let vars = Vars::of(request);
    let shown = display_name.and_then(|name| match vars.by_name(&name) {
        Some(v) => Some(v.clone()),
        None => {
            warning!(
                PrecompUnknownDisplay,
                at = settings,
                "{settings}: crust:display = {name:?} names no RenderVar of the render; the \
                 beauty is shown"
            );
            None
        }
    });
    let precomp = Precomp::new(sample, display, shown);
    debug!(
        "Precomp at {settings}: {} sample filter(s), {} display filter(s), displaying {}",
        precomp.sample.len(),
        precomp.display.len(),
        precomp.display_name()
    );
    Some(Arc::new(precomp))
}

/// Every var of the render, by layer name: the first var of that name wins,
/// in product order.
struct Vars<'a> {
    vars: Vec<&'a AovVar>,
}

impl<'a> Vars<'a> {
    fn of(request: &'a AovRequest) -> Self {
        Vars {
            vars: request.products.iter().flat_map(|p| &p.vars).collect(),
        }
    }

    fn by_name(&self, name: &str) -> Option<&'a AovVar> {
        self.vars.iter().copied().find(|v| v.name == name)
    }
}

/// A sample filter's graph, translated and compiled, before its inputs are
/// bound.
struct Graph {
    path: String,
    program: Option<crust_mtlx::Program>,
    outputs: Vec<FilterOutput>,
}

impl Graph {
    fn read(stage: &Stage, stage_path: &Path, path: &sdf::Path) -> Graph {
        let prim = prim_at(stage, path.clone());
        let network = translate_value_graph(stage, stage_path, &prim);
        let roots: Vec<(&str, Option<&str>)> = network
            .outputs
            .iter()
            .map(|(_, node, output)| (node.as_str(), output.as_deref()))
            .collect();
        let compiled = crust_mtlx::compile_value_graph(&network.doc, &roots);
        let outputs: Vec<FilterOutput> = network
            .outputs
            .iter()
            .zip(&compiled.outputs)
            .map(|((name, _, _), (slot, width))| FilterOutput {
                name: name.clone(),
                slot: *slot,
                width: *width,
            })
            .collect();
        let mut why: Vec<String> = network.reported;
        why.extend(compiled.refused.iter().cloned());
        if outputs.is_empty() {
            why.push("the graph has no output".into());
        }
        let program = if why.is_empty() {
            debug!(
                "Sample filter {path}: {} op(s), {} output(s), reads [{}]",
                compiled.program.ops.len(),
                outputs.len(),
                compiled.program.primvars.join(", ")
            );
            Some(compiled.program)
        } else {
            warning!(
                PrecompUnsupportedNode,
                at = path,
                "{path}: a sample filter cannot evaluate this graph ({}); its outputs read their \
                 clear value",
                why.join("; ")
            );
            None
        };
        Graph {
            path: path.to_string(),
            program,
            outputs,
        }
    }

    /// The filter, its channels bound to what they read: `beauty`, a var's
    /// per-sample value, or an earlier filter's output (a var reading it).
    fn bind(self, index: u16, vars: &Vars<'_>) -> SampleFilter {
        let path = self.path;
        let binds = self
            .program
            .as_ref()
            .map(|p| {
                p.primvars
                    .iter()
                    .map(|name| bind_input(&path, name, index, vars))
                    .collect()
            })
            .unwrap_or_default();
        SampleFilter::new(path, self.program, self.outputs, binds)
    }
}

/// What a sample filter's interface input `name` reads.
fn bind_input(path: &str, name: &str, index: u16, vars: &Vars<'_>) -> Bind {
    if name == BEAUTY && vars.by_name(name).is_none() {
        return Bind::Beauty;
    }
    let Some(var) = vars.by_name(name) else {
        warning!(
            PrecompUnboundInput,
            at = path,
            "{path}: input {name:?} names no RenderVar of the render; it reads its default"
        );
        return Bind::Default;
    };
    match var.source {
        AovSource::Color => Bind::Beauty,
        AovSource::Variance | AovSource::SampleCount => unbindable(path, name, var),
        AovSource::Lpe if var.variance => unbindable(path, name, var),
        AovSource::Filter(f) if f.display || f.filter >= index => {
            warning!(
                PrecompCycle,
                at = path,
                "{path}: input {name:?} reads {}, a filter that runs after this one; it reads its \
                 default",
                var.prim_path
            );
            Bind::Default
        }
        AovSource::Filter(f) => Bind::Sample {
            filter: f.filter,
            output: f.output,
        },
        _ => Bind::Var(var.clone()),
    }
}

fn unbindable(path: &str, name: &str, var: &AovVar) -> Bind {
    warning!(
        PrecompUnbindableInput,
        at = path,
        "{path}: input {name:?} reads {}, which has no per-sample value; it reads its default",
        var.prim_path
    );
    Bind::Default
}

/// Resolves every filter var into the filter and output it names, and drops
/// (warned) the ones naming a prim in neither list, an output the filter does
/// not have, or more channels than a sample filter's output holds.
fn drop_unresolved(
    request: &mut AovRequest,
    targets: &HashMap<String, FilterTarget>,
    graphs: &[Graph],
    display: &[sdf::Path],
) {
    for product in &mut request.products {
        product.vars.retain_mut(|var| {
            let AovSource::Filter(placeholder) = var.source else {
                return true;
            };
            let Some(target) = targets.get(&var.prim_path) else {
                return false;
            };
            match resolve_target(var, placeholder, target, graphs, display) {
                Some(source) => {
                    var.source = AovSource::Filter(source);
                    true
                }
                None => false,
            }
        });
    }
}

fn resolve_target(
    var: &AovVar,
    placeholder: FilterSource,
    target: &FilterTarget,
    graphs: &[Graph],
    display: &[sdf::Path],
) -> Option<FilterSource> {
    let path = &var.prim_path;
    let filter = &target.filter;
    if let Some(i) = graphs.iter().position(|g| g.path == filter.as_str()) {
        let g = &graphs[i];
        let output = match &target.output {
            Some(name) => g.outputs.iter().position(|o| &o.name == name),
            None if g.outputs.len() == 1 => Some(0),
            None => None,
        };
        let Some(output) = output else {
            let names: Vec<&str> = g.outputs.iter().map(|o| o.name.as_str()).collect();
            warning!(
                PrecompUnknownFilter,
                at = path,
                "{path}: crust:filter:output {:?} is not one of {filter}'s outputs [{}]; no \
                 channel written",
                target.output.as_deref().unwrap_or("(unauthored)"),
                names.join(", ")
            );
            return None;
        };
        let width = g.outputs[output].width as usize;
        if width != 1 && var.components > width {
            warning!(
                AovTypeMismatch,
                at = path,
                "{path}: {} channels cannot hold {filter}'s {width}-channel output {:?}; no \
                 channel written",
                var.components,
                g.outputs[output].name
            );
            return None;
        }
        return Some(FilterSource {
            display: false,
            filter: i as u16,
            output: output as u16,
            ..placeholder
        });
    }
    if let Some(i) = display.iter().position(|p| p == filter) {
        if target.output.as_deref().is_some_and(|o| o != "out") {
            warning!(
                PrecompUnknownFilter,
                at = path,
                "{path}: a display filter has one output, \"out\", not {:?}; no channel written",
                target.output.as_deref().unwrap_or_default()
            );
            return None;
        }
        return Some(FilterSource {
            display: true,
            filter: i as u16,
            output: 0,
            // An image operation's output is exposed already.
            radiance: false,
            ..placeholder
        });
    }
    warning!(
        PrecompUnknownFilter,
        at = path,
        "{path}: crust:filter targets {filter}, which is in neither crust:sampleFilters nor \
         crust:displayFilters; no channel written"
    );
    None
}

/// Drops (warned) the display filter vars asking for more channels than
/// their filter's output has.
fn check_display_widths(request: &mut AovRequest, display: &[DisplayFilter]) {
    for product in &mut request.products {
        product.vars.retain(|var| match var.source {
            AovSource::Filter(f) if f.display => {
                let width = display[f.filter as usize].width as usize;
                let fits = width == 1 || var.components <= width;
                if !fits {
                    warning!(
                        AovTypeMismatch,
                        at = var.prim_path,
                        "{}: {} channels cannot hold {}'s {width}-channel output; no channel \
                         written",
                        var.prim_path,
                        var.components,
                        display[f.filter as usize].path
                    );
                }
                fits
            }
            _ => true,
        });
    }
}

/// The beauty as a display filter reads it when no var is named `beauty`.
fn beauty_var() -> AovVar {
    AovVar {
        prim_path: BEAUTY.into(),
        name: BEAUTY.into(),
        channel_prefix: None,
        source: AovSource::Color,
        components: 3,
        precision: Precision::Float,
        accumulation: crate::aov::Accumulation::Filtered,
        clear: 0.0,
        expression: None,
        raw: false,
        variance: false,
    }
}

/// Display filter `index` at `path`.
#[allow(clippy::too_many_arguments)]
fn read_display(
    stage: &Stage,
    stage_path: &Path,
    path: &sdf::Path,
    index: u16,
    paths: &[sdf::Path],
    precomp: &Precomp,
    earlier: &[DisplayFilter],
    vars: &Vars<'_>,
) -> DisplayFilter {
    let prim = prim_at(stage, path.clone());
    let shader = openusd_schemas::shade::Shader::get(stage, path.clone())
        .ok()
        .flatten();
    let id = shader.as_ref().and_then(shader_info_id).unwrap_or_default();
    let names = ImageNames {
        path,
        index,
        paths,
        precomp,
        earlier,
        vars,
    };
    let image = |input: &str| names.image(&prim, input);
    let op = match id.as_str() {
        "crust:convolve" => Operation::Convolve {
            input: image("in"),
            kernel_path: asset_path(&prim.attribute("inputs:kernel"), stage_path),
            kernel: None,
            scale: custom_f32(&prim, "inputs:kernelScale").unwrap_or(1.0),
            normalize: input_bool(&prim, "inputs:normalize").unwrap_or(true),
        },
        "crust:blur" => {
            let radius = custom_f32(&prim, "inputs:radius").unwrap_or(1.0);
            if radius.is_nan() || radius > crate::precomp::MAX_BLUR_RADIUS {
                warning!(
                    PrecompInvalidValue,
                    at = path,
                    "{path}: crust:blur radius {radius} is past {} pixels; the filter passes its \
                     input through",
                    crate::precomp::MAX_BLUR_RADIUS
                );
            }
            Operation::Blur {
                input: image("in"),
                radius,
            }
        }
        "crust:vectorBlur" => {
            let field = if authored(&prim, "inputs:motion") {
                Field::Vectors(image("motion"))
            } else if let Some(d) = float2(&prim, "inputs:direction") {
                Field::Direction(d)
            } else {
                Field::Radial {
                    center: float2(&prim, "inputs:center"),
                    strength: custom_f32(&prim, "inputs:strength").unwrap_or(0.0),
                }
            };
            Operation::VectorBlur {
                input: image("in"),
                field,
                taps: custom_i32(&prim, "inputs:taps").unwrap_or(8).clamp(1, 256) as u32,
            }
        }
        "crust:merge" => {
            let token = custom_token(&prim, "inputs:operation").unwrap_or_else(|| "over".into());
            let mode = token.parse::<MergeMode>().unwrap_or_else(|()| {
                warning!(
                    SettingsInvalidValue,
                    at = path,
                    "{path}: crust:merge operation {token:?} is not over, plus, multiply or \
                     screen; using over"
                );
                MergeMode::Over
            });
            Operation::Merge {
                a: image("a"),
                b: image("b"),
                alpha: authored(&prim, "inputs:alpha").then(|| image("alpha")),
                mode,
                mix: custom_f32(&prim, "inputs:mix").unwrap_or(1.0),
            }
        }
        "crust:grade" => Operation::Grade {
            input: image("in"),
            lift: color(&prim, "inputs:lift").unwrap_or([0.0; 3]),
            gain: color(&prim, "inputs:gain").unwrap_or([1.0; 3]),
            gamma: color(&prim, "inputs:gamma").unwrap_or([1.0; 3]),
            offset: color(&prim, "inputs:offset").unwrap_or([0.0; 3]),
        },
        other => {
            warning!(
                PrecompUnknownOperation,
                at = path,
                "{path}: {} is not a display operation (crust:convolve, crust:blur, \
                 crust:vectorBlur, crust:merge, crust:grade); the filter passes its input through",
                if other.is_empty() {
                    "a shader with no info:id".to_string()
                } else {
                    format!("info:id {other:?}")
                }
            );
            Operation::PassThrough(image("in"))
        }
    };
    let width = match &op {
        Operation::Merge { a, b, .. } => names.width(a).max(names.width(b)),
        Operation::PassThrough(i)
        | Operation::Convolve { input: i, .. }
        | Operation::Blur { input: i, .. }
        | Operation::VectorBlur { input: i, .. }
        | Operation::Grade { input: i, .. } => names.width(i),
    };
    DisplayFilter {
        path: path.to_string(),
        op,
        width,
    }
}

/// What a display filter's image inputs can name.
struct ImageNames<'a> {
    path: &'a sdf::Path,
    index: u16,
    paths: &'a [sdf::Path],
    precomp: &'a Precomp,
    earlier: &'a [DisplayFilter],
    vars: &'a Vars<'a>,
}

impl ImageNames<'_> {
    /// The image `token inputs:<input>` names: a RenderVar, a sample-filter
    /// output (`<graph>.<output>`), an earlier display filter (its prim
    /// name), or `beauty`.
    fn image(&self, prim: &Prim, input: &str) -> Image {
        let path = self.path;
        let Some(name) = custom_token(prim, &format!("inputs:{input}")).filter(|n| !n.is_empty())
        else {
            warning!(
                PrecompUnboundInput,
                at = path,
                "{path}: input {input:?} names nothing; it reads its clear value"
            );
            return Image::Clear;
        };
        if let Some(var) = self.vars.by_name(&name) {
            return match var.source {
                AovSource::Filter(f) if f.display => self.display(f.filter as usize, input, &name),
                _ => Image::Var(var.clone()),
            };
        }
        if name == BEAUTY {
            return Image::Var(beauty_var());
        }
        if let Some((graph, output)) = name.split_once('.') {
            for (fi, f) in self.precomp.sample.iter().enumerate() {
                if f.path.rsplit('/').next() == Some(graph)
                    && let Some(o) = f.outputs.iter().position(|o| o.name == output)
                {
                    return Image::Sample {
                        filter: fi as u16,
                        output: o as u16,
                    };
                }
            }
        }
        if let Some(i) = self
            .paths
            .iter()
            .position(|p| p.name().is_some_and(|n| n == name))
        {
            return self.display(i, input, &name);
        }
        warning!(
            PrecompUnboundInput,
            at = path,
            "{path}: input {input:?} = {name:?} names no RenderVar, sample-filter output or \
             display filter; it reads its clear value"
        );
        Image::Clear
    }

    /// Display filter `i`'s output, which only a later filter may read.
    fn display(&self, i: usize, input: &str, name: &str) -> Image {
        if i < self.index as usize {
            return Image::Display(i as u16);
        }
        let path = self.path;
        warning!(
            PrecompCycle,
            at = path,
            "{path}: input {input:?} = {name:?} reads a display filter that runs at or after \
             this one; it reads its clear value"
        );
        Image::Clear
    }

    /// How many channels `image` has.
    fn width(&self, image: &Image) -> u8 {
        match image {
            Image::Var(v) => v.components.clamp(1, 4) as u8,
            Image::Sample { filter, output } => {
                self.precomp.sample[*filter as usize].outputs[*output as usize].width
            }
            Image::Display(i) => self.earlier[*i as usize].width,
            Image::Clear => 1,
        }
    }
}

fn authored(prim: &Prim, name: &str) -> bool {
    prim_value(prim, name).is_some()
}

/// A bool input, authored as a bool or an int.
fn input_bool(prim: &Prim, name: &str) -> Option<bool> {
    custom_bool(prim, name).or_else(|| custom_i32(prim, name).map(|i| i != 0))
}

/// A `float2` (or `double2`) input.
fn float2(prim: &Prim, name: &str) -> Option<[f32; 2]> {
    match prim_value(prim, name)? {
        sdf::Value::Vec2f(v) => Some([v.x, v.y]),
        sdf::Value::Vec2d(v) => Some([v.x as f32, v.y as f32]),
        _ => None,
    }
}

/// A `color3f` input, or a float in every channel.
fn color(prim: &Prim, name: &str) -> Option<[f32; 3]> {
    let v = prim_value(prim, name)?;
    decode_vec3(v.clone())
        .map(|c| c.to_array())
        .or_else(|| super::attrs::decode_f32(v).map(|f| [f; 3]))
}
