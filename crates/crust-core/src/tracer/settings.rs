//! What a render is asked to do: [`RenderSettings`] and the MIS
//! [`SamplingStrategy`].

use crate::LightSelection;
use crate::filter::PixelFilter;
use crate::guiding::GuidingConfig;
use crate::pdf::PdfSolidAngle;

/// The indirect clamp a render gets unless the stage (`crust:indirectClamp`)
/// or the host (`--indirect-clamp`) says otherwise — see
/// [`RenderSettings::with_indirect_clamp`]. On by default, as in production
/// renderers, because a path tracer's worst fireflies are indirect: a diffuse
/// bounce finding a tiny, very hot light. 10 is far above any ordinarily lit
/// surface, so it removes outliers rather than shading; `0` restores the
/// unbiased estimator.
pub const DEFAULT_INDIRECT_CLAMP: f32 = 10.0;

/// The adaptive neighbour tolerance a render gets unless the stage
/// (`crust:adaptiveNeighbourTolerance`) says otherwise — see
/// [`RenderSettings::with_adaptive_neighbour_tolerance`]. One index unit:
/// a pixel waits for a cross neighbour that is more than one threshold of
/// relative error behind it.
pub const DEFAULT_ADAPTIVE_NEIGHBOUR_TOLERANCE: f32 = 1.0;

/// How the integrator combines its two direct-lighting strategies — light
/// sampling (NEE) and BSDF/phase sampling — into one estimate. The two MIS
/// variants weight each strategy's samples with a Veach heuristic; the
/// single-strategy variants disable one side entirely and exist to
/// visualize what each strategy contributes and where it fails (the classic
/// Veach comparison — `samples/veach_mis.usda` is the matching scene).
///
/// Every variant keeps `light_weight + bounce_weight = 1` for a light both
/// strategies can reach, so emission is counted exactly once and all four
/// estimators are unbiased — they differ only in variance. Lights only BSDF
/// sampling can reach (delta lobes, emissive geometry outside the light
/// list) keep full bounce weight under every strategy, `LightOnly`
/// included, because zeroing them would lose their energy entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SamplingStrategy {
    /// β=2 power-heuristic MIS — the renderer's historical default.
    #[default]
    PowerMis,
    /// Balance-heuristic MIS.
    BalanceMis,
    /// Light sampling only: NEE at full weight, bounce-hit emission dropped
    /// (for lights NEE could have sampled).
    LightOnly,
    /// BSDF sampling only: no shadow rays, bounce-hit emission at full
    /// weight.
    BsdfOnly,
}

crate::names::named!(
    SamplingStrategy,
    "sampling strategy",
    [
        (
            SamplingStrategy::PowerMis,
            "power",
            "β=2 power-heuristic MIS (default)"
        ),
        (
            SamplingStrategy::BalanceMis,
            "balance",
            "Balance-heuristic MIS"
        ),
        (
            SamplingStrategy::LightOnly,
            "light",
            "Light sampling (NEE) only"
        ),
        (SamplingStrategy::BsdfOnly, "bsdf", "BSDF sampling only"),
    ],
    aliases[("mis", SamplingStrategy::PowerMis)]
);

impl SamplingStrategy {
    /// Does this strategy trace NEE shadow rays at all?
    pub fn samples_lights(self) -> bool {
        !matches!(self, SamplingStrategy::BsdfOnly)
    }

    /// Weight of a light-sampled (NEE) contribution, given the competing
    /// bounce strategy's density toward the same direction.
    ///
    /// Both densities are [`PdfSolidAngle`]s: a light's and a bounce's pdf
    /// are only comparable in the same measure.
    pub fn light_weight(self, light_pdf: PdfSolidAngle, bounce_pdf: PdfSolidAngle) -> f32 {
        let (light_pdf, bounce_pdf) = (light_pdf.get(), bounce_pdf.get());
        match self {
            SamplingStrategy::PowerMis => utils::power_heuristic(light_pdf, bounce_pdf),
            SamplingStrategy::BalanceMis => utils::balance_heuristic(light_pdf, bounce_pdf),
            SamplingStrategy::LightOnly => 1.0,
            SamplingStrategy::BsdfOnly => 0.0,
        }
    }

    /// Weight of a contribution only one technique can produce: bounce-hit
    /// emission NEE could not have delivered (a delta or non-evaluable
    /// previous vertex, a light it never picks, a point its light refuses to
    /// sample, emissive geometry with no light-list entry), or NEE-free
    /// escapes. With nothing competing there is nothing to partition, so
    /// every strategy takes it whole — `LightOnly` included, which would
    /// otherwise lose light that NEE cannot reach.
    pub fn unopposed_weight(self) -> f32 {
        match self {
            SamplingStrategy::PowerMis
            | SamplingStrategy::BalanceMis
            | SamplingStrategy::LightOnly
            | SamplingStrategy::BsdfOnly => 1.0,
        }
    }

    /// Weight of bounce-hit emission on a light that NEE could also have
    /// sampled with density `light_pdf`. Mirror of [`Self::light_weight`]:
    /// for every strategy the two weights sum to one.
    pub fn bounce_weight(self, bounce_pdf: PdfSolidAngle, light_pdf: PdfSolidAngle) -> f32 {
        let (bounce_pdf, light_pdf) = (bounce_pdf.get(), light_pdf.get());
        match self {
            SamplingStrategy::PowerMis => utils::power_heuristic(bounce_pdf, light_pdf),
            SamplingStrategy::BalanceMis => utils::balance_heuristic(bounce_pdf, light_pdf),
            SamplingStrategy::LightOnly => 0.0,
            SamplingStrategy::BsdfOnly => 1.0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RenderSettings {
    pub(super) samples_per_pixel: u32,
    pub(super) max_depth: u32,
    pub(super) width: usize,
    pub(super) height: usize,
    // Adaptive sampling: a pixel may stop early once it has taken at least
    // `min_samples_per_pixel` samples and the relative standard error of its
    // mean drops below `variance_threshold` (0 disables early stopping).
    pub(super) min_samples_per_pixel: u32,
    pub(super) variance_threshold: f32,
    // How far a still-sampling cross neighbour's convergence index may sit
    // above a pixel's own before it holds the pixel back (see
    // `with_adaptive_neighbour_tolerance`). Negative: no comparison.
    pub(super) adaptive_neighbour_tolerance: f32,
    pub(super) frame: isize,
    // Path guiding: `None` unless opted in (`crust:pathGuiding`; see
    // `with_guiding`).
    pub(super) guiding: Option<GuidingConfig>,
    // MIS strategy (see `SamplingStrategy`; `crust:samplingStrategy` /
    // `--strategy`).
    pub(super) sampling_strategy: SamplingStrategy,
    // Pixel reconstruction filter (see `PixelFilter`; `crust:pixelFilter` /
    // `--filter`). Applied by filter importance sampling in `render_pixel`.
    pub(super) pixel_filter: PixelFilter,
    // How NEE picks a light (see `LightSelection`; `crust:lightSelection` /
    // `--light-selection`). Applied once, in `Renderer::new`.
    pub(super) light_selection: LightSelection,
    // Firefly clamp on each camera sample's indirect light (see
    // `with_indirect_clamp`; `crust:indirectClamp` / `--indirect-clamp`).
    // `DEFAULT_INDIRECT_CLAMP` unless overridden; `None` is off (an authored
    // 0). Validated at construction: `Some` is always finite and positive.
    pub(super) indirect_clamp: Option<f32>,
}
impl RenderSettings {
    pub fn new(
        samples_per_pixel: u32,
        max_depth: u32,
        width: usize,
        height: usize,
        min_samples_per_pixel: u32,
        variance_threshold: f32,
        frame: isize,
    ) -> Self {
        RenderSettings {
            samples_per_pixel,
            max_depth,
            width,
            height,
            min_samples_per_pixel,
            variance_threshold,
            adaptive_neighbour_tolerance: DEFAULT_ADAPTIVE_NEIGHBOUR_TOLERANCE,
            frame,
            guiding: None,
            sampling_strategy: SamplingStrategy::default(),
            pixel_filter: PixelFilter::default(),
            light_selection: LightSelection::default(),
            indirect_clamp: Some(DEFAULT_INDIRECT_CLAMP),
        }
    }

    /// Override the samples-per-pixel count (e.g. from a CLI flag). Clamped to >= 1.
    pub fn with_samples_per_pixel(mut self, spp: u32) -> Self {
        self.samples_per_pixel = spp.max(1);
        self
    }

    /// Override the sampler's frame seed (`crust:frame`) — the frame index
    /// OpenQMC decorrelates a render's sample patterns by.
    pub fn with_frame(mut self, frame: isize) -> Self {
        self.frame = frame;
        self
    }

    pub fn frame(&self) -> isize {
        self.frame
    }

    /// Enable path guiding with `config`, or disable it with `None`. At least
    /// one training iteration is run, and the guide-sampling probability α is
    /// clamped to `[0.1, 0.9]` so neither side of the mixture starves.
    pub fn with_guiding(mut self, config: Option<GuidingConfig>) -> Self {
        self.guiding = config.map(|c| GuidingConfig {
            train_iterations: c.train_iterations.max(1),
            guide_prob: c.guide_prob.clamp(0.1, 0.9),
            ..c
        });
        self
    }

    /// The guiding configuration in effect, `None` when guiding is off.
    pub fn guiding(&self) -> Option<GuidingConfig> {
        self.guiding
    }

    /// Select how light sampling and BSDF sampling combine — see
    /// [`SamplingStrategy`].
    pub fn with_sampling_strategy(mut self, strategy: SamplingStrategy) -> Self {
        self.sampling_strategy = strategy;
        self
    }

    pub fn sampling_strategy(&self) -> SamplingStrategy {
        self.sampling_strategy
    }

    /// Select the pixel reconstruction filter — see [`PixelFilter`].
    pub fn with_pixel_filter(mut self, filter: PixelFilter) -> Self {
        self.pixel_filter = filter;
        self
    }

    pub fn pixel_filter(&self) -> PixelFilter {
        self.pixel_filter
    }

    /// Select how NEE picks a light — see [`LightSelection`].
    pub fn with_light_selection(mut self, selection: LightSelection) -> Self {
        self.light_selection = selection;
        self
    }

    pub fn light_selection(&self) -> LightSelection {
        self.light_selection
    }

    /// Clamp each camera sample's **indirect** light to at most `limit` in
    /// its largest channel, scaling the colour down whole so the hue
    /// survives. Defaults to [`DEFAULT_INDIRECT_CLAMP`]; `0`, a negative or a
    /// non-finite value disables it, which is the unbiased estimator. See
    /// `clamp_indirect` (in `tracer/path.rs`) for what counts as indirect.
    ///
    /// Biased on purpose — energy is removed exactly where it is rare and
    /// bright — and the standard trade in production renderers (Cycles'
    /// *Clamp Indirect*, Arnold's `indirect_sample_clamp`): a diffuse
    /// bounce that happens to find a tiny, very hot light or a sharp
    /// highlight is a firefly that would take thousands of samples to
    /// average out. Direct light is never touched, so lights, their direct
    /// illumination and their MIS-weighted bounce hits keep full energy.
    pub fn with_indirect_clamp(mut self, limit: f32) -> Self {
        self.indirect_clamp = (limit.is_finite() && limit > 0.0).then_some(limit);
        self
    }

    /// The indirect clamp in effect — finite and positive — or `None` when it
    /// is off.
    pub fn indirect_clamp(&self) -> Option<f32> {
        self.indirect_clamp
    }

    /// How much less converged a still-sampling cross neighbour (up, down,
    /// left, right) may be before it holds a pixel back, in units of the
    /// convergence index (`relative standard error / variance_threshold`,
    /// so `1` means "one threshold of relative error"). Defaults to
    /// [`DEFAULT_ADAPTIVE_NEIGHBOUR_TOLERANCE`]. A negative value skips the
    /// comparison, and each pixel then stops exactly as it would alone —
    /// the A/B side. A non-finite value keeps the default.
    pub fn with_adaptive_neighbour_tolerance(mut self, tolerance: f32) -> Self {
        if tolerance.is_finite() {
            self.adaptive_neighbour_tolerance = tolerance;
        }
        self
    }

    pub fn adaptive_neighbour_tolerance(&self) -> f32 {
        self.adaptive_neighbour_tolerance
    }

    pub fn min_samples_per_pixel(&self) -> u32 {
        self.min_samples_per_pixel
    }

    pub fn variance_threshold(&self) -> f32 {
        self.variance_threshold
    }

    pub fn get_dimensions(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    pub fn samples_per_pixel(&self) -> u32 {
        self.samples_per_pixel
    }

    pub fn max_depth(&self) -> u32 {
        self.max_depth
    }
}
