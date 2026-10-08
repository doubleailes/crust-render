//! What a render is asked to do: [`RenderSettings`] and the MIS
//! [`SamplingStrategy`].

use super::PixelRect;
use crate::LightSelection;
use crate::filter::PixelFilter;
use crate::pdf::PdfSolidAngle;

/// The indirect clamp a render gets unless the stage (`crust:indirectClamp`)
/// or the host (`--indirect-clamp`) says otherwise — see
/// [`RenderSettings::with_indirect_clamp`]. On by default, as in production
/// renderers, because a path tracer's worst fireflies are indirect: a diffuse
/// bounce finding a tiny, very hot light. 10 is far above any ordinarily lit
/// surface, so it removes outliers rather than shading; `0` restores the
/// unbiased estimator.
pub const DEFAULT_INDIRECT_CLAMP: f32 = 10.0;

/// The light samples per vertex a render takes unless the stage
/// (`crust:lightSamples` / `crust:lightSamplesIndirect`) or the host
/// (`--light-samples` / `--light-samples-indirect`) says otherwise — see
/// [`RenderSettings::with_light_samples`]. One: the measured defaults
/// (`openspec/specs/lighting/design.md`, "Several light samples per vertex").
pub const DEFAULT_LIGHT_SAMPLES: u32 = 1;

/// The most light samples a vertex may take. A count multiplies the shadow
/// rays at every vertex it applies to, so a mistyped `1000000000` would be a
/// render that never finishes; anything above this is clamped (the importer
/// warns, the CLI refuses it).
pub const MAX_LIGHT_SAMPLES: u32 = 1024;

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
    // The pixels the render traces, in image space (top-left origin); the
    // whole frame unless `with_region` says otherwise. Always non-empty and
    // inside `width` × `height`.
    pub(super) region: PixelRect,
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
    // Path guiding (opt-in via `crust:pathGuiding`; see `with_guiding`).
    pub(super) guiding: bool,
    pub(super) guiding_train_iterations: u32,
    pub(super) guiding_prob: f32,
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
    // How many light samples NEE takes at the camera vertex and at every
    // later surface or volume vertex (see `with_light_samples`;
    // `crust:lightSamples` / `--light-samples` and
    // `crust:lightSamplesIndirect` / `--light-samples-indirect`). At least 1.
    pub(super) light_samples: u32,
    pub(super) light_samples_indirect: u32,
    // Whether camera rays sample the shutter at all (see `with_motion_blur`;
    // `disableMotionBlur` / `instantaneousShutter` on the render settings).
    // Off, every ray is traced at shutter open and moving geometry renders
    // sharp at its authored position; its motion stays in the scene.
    pub(super) motion_blur: bool,
}
/// The settings a stage that authors none renders with: 640×360 at 128 spp,
/// paths up to 32 vertices, adaptive sampling stopping no earlier than 32
/// samples at a 5% relative standard error, frame 0. Every other field is
/// changed by name through a `with_*` builder, so no call site passes a row
/// of bare numbers whose order only the signature knows.
impl Default for RenderSettings {
    fn default() -> Self {
        RenderSettings {
            samples_per_pixel: 128,
            max_depth: 32,
            width: 640,
            height: 360,
            region: PixelRect::full(640, 360),
            min_samples_per_pixel: 32,
            variance_threshold: 0.05,
            adaptive_neighbour_tolerance: DEFAULT_ADAPTIVE_NEIGHBOUR_TOLERANCE,
            frame: 0,
            guiding: false,
            guiding_train_iterations: 4,
            guiding_prob: 0.5,
            sampling_strategy: SamplingStrategy::default(),
            pixel_filter: PixelFilter::default(),
            light_selection: LightSelection::default(),
            indirect_clamp: Some(DEFAULT_INDIRECT_CLAMP),
            light_samples: DEFAULT_LIGHT_SAMPLES,
            light_samples_indirect: DEFAULT_LIGHT_SAMPLES,
            motion_blur: true,
        }
    }
}

impl RenderSettings {
    /// Set the image resolution, in pixels. The region becomes the whole
    /// new frame: a region is in the frame's pixels, so one chosen for
    /// another resolution means nothing here.
    pub fn with_resolution(mut self, width: usize, height: usize) -> Self {
        self.width = width;
        self.height = height;
        self.region = PixelRect::full(width, height);
        self
    }

    /// Trace only the pixels of `region` — image space, top-left origin,
    /// half-open — clipped to the frame. Everything the resolution drives
    /// (the camera, ray-cone footprints, the per-pixel sample keys) still
    /// sees the full frame, so a pixel of a region renders as it does in
    /// the full frame. A region that is empty once clipped is refused
    /// ([`Error::EmptyRegion`](crate::Error::EmptyRegion)), and the
    /// settings keep their own region.
    ///
    /// Set it after [`with_resolution`](Self::with_resolution), which
    /// resets it.
    pub fn with_region(mut self, region: PixelRect) -> Result<Self, crate::Error> {
        match region.clip_to(self.width, self.height) {
            Some(clipped) => {
                self.region = clipped;
                Ok(self)
            }
            None => Err(crate::Error::EmptyRegion {
                region,
                width: self.width,
                height: self.height,
            }),
        }
    }

    /// The pixels the render traces, in image space (top-left origin): the
    /// whole frame unless [`with_region`](Self::with_region) narrowed it.
    pub fn region(&self) -> PixelRect {
        self.region
    }

    /// Whether the region is the whole frame.
    pub fn is_full_frame(&self) -> bool {
        self.region == PixelRect::full(self.width, self.height)
    }

    /// The region in the tracer's raster space, whose rows grow upwards
    /// from the bottom of the frame (see [`PixelRect`]).
    pub(crate) fn raster_region(&self) -> PixelRect {
        self.region.flip_y(self.height)
    }

    /// Set the longest path, in vertices.
    pub fn with_max_depth(mut self, max_depth: u32) -> Self {
        self.max_depth = max_depth;
        self
    }

    /// Set adaptive sampling: a pixel may stop once it has taken at least
    /// `min_samples_per_pixel` samples and the relative standard error of its
    /// mean is below `variance_threshold` (`0` never stops early).
    pub fn with_adaptive_sampling(
        mut self,
        min_samples_per_pixel: u32,
        variance_threshold: f32,
    ) -> Self {
        self.min_samples_per_pixel = min_samples_per_pixel;
        self.variance_threshold = variance_threshold;
        self
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

    /// Enable (or disable) path guiding with the given number of training
    /// iterations and guide-sampling probability α.
    pub fn with_guiding(mut self, enabled: bool, train_iterations: u32, guide_prob: f32) -> Self {
        self.guiding = enabled;
        self.guiding_train_iterations = train_iterations.max(1);
        self.guiding_prob = guide_prob.clamp(0.1, 0.9);
        self
    }

    /// Whether path guiding is on — see [`RenderSettings::with_guiding`].
    pub fn guiding(&self) -> bool {
        self.guiding
    }

    /// Guiding's training iterations (at least 1).
    pub fn guiding_train_iterations(&self) -> u32 {
        self.guiding_train_iterations
    }

    /// Guiding's guide-sampling probability α.
    pub fn guiding_prob(&self) -> f32 {
        self.guiding_prob
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
    /// [`clamp_indirect`](super::path::clamp_indirect) for what counts as indirect.
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

    /// How many light samples next-event estimation takes per vertex:
    /// `camera` at the first vertex of each path, `indirect` at every later
    /// surface or volume vertex. Each is clamped to 1 ..= [`MAX_LIGHT_SAMPLES`],
    /// and 1 and 1 (the default, [`DEFAULT_LIGHT_SAMPLES`]) is the one-sample
    /// renderer, bit for bit.
    ///
    /// The samples at one vertex stratify the light pick, so a count of N
    /// spreads over the lights close to N times each one's selection
    /// probability, and the bounce side weighs a light it hits against N
    /// times the light density NEE used there (multi-sample MIS). Direct-light
    /// variance falls about as 1/N; the cost is N shadow rays per vertex, so
    /// `indirect` multiplies along the whole path while `camera` is paid once.
    pub fn with_light_samples(mut self, camera: u32, indirect: u32) -> Self {
        self.light_samples = camera.clamp(1, MAX_LIGHT_SAMPLES);
        self.light_samples_indirect = indirect.clamp(1, MAX_LIGHT_SAMPLES);
        self
    }

    /// Light samples per camera vertex (at least 1).
    pub fn light_samples(&self) -> u32 {
        self.light_samples
    }

    /// Light samples per later surface or volume vertex (at least 1).
    pub fn light_samples_indirect(&self) -> u32 {
        self.light_samples_indirect
    }

    /// Whether moving geometry (`crust:motion:translate`) is motion blurred.
    /// On by default. Off (`disableMotionBlur` or `instantaneousShutter` on
    /// the render settings), every camera ray is traced at shutter open: the
    /// beauty and every AOV show moving geometry sharp at its authored
    /// position, no shutter sample is drawn (the other sample dimensions are
    /// the ones a static scene draws), and the motion stays in the scene,
    /// so the `motionvector` AOV is still produced.
    pub fn with_motion_blur(mut self, on: bool) -> Self {
        self.motion_blur = on;
        self
    }

    /// Whether moving geometry is motion blurred — see
    /// [`RenderSettings::with_motion_blur`].
    pub fn motion_blur(&self) -> bool {
        self.motion_blur
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
