//! Session renders (design D6): progressive, cancellable renders of the
//! session's scene on a thread of their own (tracing on the rayon pool), so
//! neither the session thread nor the protocol waits for one.
//!
//! A tool starts a render on the session thread, then waits on the protocol
//! side for at most its budget, and answers with the latest snapshot. The
//! render keeps refining after that, until it completes, an edit or a new
//! render cancels it, or `cancel` does. The latest [`RETAINED`] renders are
//! kept, their images and AOVs, for `snapshot`, `probe` and `diff`.

use crate::{OutputColor, tone_map};
use base64::Engine;
use crust_core::Precision;
use crust_core::color::{PREVIEW_DISPLAY, PREVIEW_VIEW, Space};
use crust_core::compare::{Channel, Planes};
use crust_core::{
    AovFilm, AovRequest, Buffer, RayStats, RenderControl, RenderOutcome, RenderSettings, Renderer,
};
use exr::prelude::f16;
use serde_json::{Value, json};
use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How many renders a session keeps.
pub const RETAINED: usize = 8;

/// The long side of a render's image as a tool returns it, at most.
pub const MAX_IMAGE_SIDE: usize = 1024;

/// The samples every pixel has taken when `render` answers by default: the
/// first stage of the first sweep that reads as an image (design D1 of
/// `mcp-first-pixel`; 1 spp is mostly noise).
pub const FIRST_IMAGE_SPP: u32 = 4;

/// How often a wait for the first image looks at the render's progress,
/// which has no notification of its own (design D2).
const FIRST_IMAGE_POLL: Duration = Duration::from_millis(10);

/// What a render left when it returned.
pub struct Finished {
    pub buffer: Buffer,
    pub film: Option<AovFilm>,
    pub rays: RayStats,
    pub outcome: RenderOutcome,
    pub seconds: f64,
}

/// One session render.
pub struct Job {
    pub id: u64,
    pub control: RenderControl,
    /// The settings it renders with: the scene's, with the call's region and
    /// samples.
    pub settings: RenderSettings,
    /// The products whose AOVs it gathers: the stage's, as `crust render`
    /// requests them.
    pub aovs: Arc<AovRequest>,
    pub working_space: Space,
    started: Instant,
    /// The progress callback's `(done, total)`.
    done: AtomicU64,
    total: AtomicU64,
    finished: Mutex<Option<Arc<Finished>>>,
    signal: tokio::sync::watch::Sender<bool>,
}

impl Job {
    /// What it left, once it returned.
    pub fn finished(&self) -> Option<Arc<Finished>> {
        self.finished
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// `rendering`, `done` (every pixel took its samples) or `cancelled`.
    pub fn status(&self) -> &'static str {
        match self.finished().map(|f| f.outcome) {
            None => "rendering",
            Some(RenderOutcome::Completed) => "done",
            Some(RenderOutcome::Cancelled) => "cancelled",
        }
    }

    /// Waits for the render to return, for at most `budget`.
    pub async fn wait(&self, budget: Duration) {
        let mut returned = self.signal.subscribe();
        let _ = tokio::time::timeout(budget, returned.wait_for(|r| *r)).await;
    }

    /// Waits, for at most `budget`, until every pixel has taken
    /// [`FIRST_IMAGE_SPP`] samples or the render returned. A render of that
    /// many samples or fewer waits for its return: its first image is the
    /// whole render, and answering on its last stage would race the return.
    pub async fn wait_for_image(&self, budget: Duration) {
        if self.settings.samples_per_pixel() <= FIRST_IMAGE_SPP {
            return self.wait(budget).await;
        }
        let returned = self.signal.subscribe();
        let first_image = async {
            // The maximum, not the pass's own count: a guided render starts
            // each training pass over, and a poll between passes would miss
            // the 4-spp image the previous one left.
            while !*returned.borrow() && self.control.max_samples_reached() < FIRST_IMAGE_SPP {
                tokio::time::sleep(FIRST_IMAGE_POLL).await;
            }
        };
        let _ = tokio::time::timeout(budget, first_image).await;
    }

    /// The image so far: the final beauty once returned, else the latest
    /// snapshot (`None` before the first).
    pub fn image(&self) -> Option<Buffer> {
        match self.finished() {
            Some(f) => Some(f.buffer.clone()),
            None => self.control.snapshot().map(|(_, b)| b),
        }
    }

    /// The fraction of the render's work done, 0 to 1.
    pub fn progress(&self) -> f64 {
        if self.finished().is_some() {
            return 1.0;
        }
        let total = self.total.load(Ordering::Relaxed);
        if total == 0 {
            0.0
        } else {
            self.done.load(Ordering::Relaxed) as f64 / total as f64
        }
    }

    /// What a tool reports about the render.
    pub fn report(&self) -> Value {
        let spp = self.settings.samples_per_pixel();
        let (w, h) = self.settings.get_dimensions();
        let r = self.settings.region();
        let finished = self.finished();
        // While it renders, the samples every pixel has taken (the last
        // completed stage of the first sweep); once returned, the fewest and
        // most any pixel took.
        let reached = match &finished {
            Some(f) => json!({ "min": f.rays.spp_min, "max": f.rays.spp_max }),
            None => json!(self.control.samples_reached()),
        };
        json!({
            "render_id": self.id,
            "status": self.status(),
            "done": finished.is_some(),
            "progress": self.progress(),
            "spp": spp,
            "spp_reached": reached,
            // Bumped with every update of the image: a newer image has a
            // greater one.
            "generation": self.control.generation(),
            "elapsed_s": finished
                .as_ref()
                .map_or(self.started.elapsed().as_secs_f64(), |f| f.seconds),
            "resolution": [w, h],
            "region": [r.x0, r.y0, r.x1, r.y1],
        })
    }
}

/// The renders of a session: the latest, maybe running, and the ones kept.
#[derive(Default)]
pub struct Renders {
    /// Oldest first; the last is the latest render.
    jobs: VecDeque<Arc<Job>>,
    /// The latest render's thread, until it is joined.
    thread: Option<JoinHandle<()>>,
    next: u64,
}

impl Renders {
    /// Cancels the latest render if it still runs, and joins it. Returns its
    /// id when it was cancelled here.
    pub fn stop(&mut self) -> Option<u64> {
        let thread = self.thread.take()?;
        let job = self.jobs.back()?.clone();
        let cancelled = job.finished().is_none();
        job.control.cancel();
        let _ = thread.join();
        cancelled.then_some(job.id)
    }

    /// Stops the latest render, then starts one of `renderer` with
    /// `settings`. Returns it, and the ids of the renders it evicted.
    pub fn start(
        &mut self,
        renderer: &mut Arc<Renderer>,
        settings: RenderSettings,
        aovs: Arc<AovRequest>,
        working_space: Space,
    ) -> Result<(Arc<Job>, Vec<u64>), String> {
        self.stop();
        let scene = Arc::get_mut(renderer).ok_or("the scene is still held by a render")?;
        // Retuning keeps the light selection (and the `learned` pre-pass the
        // import already ran) when the call changes only the samples or the
        // region; it rebuilds it otherwise.
        scene.retune(settings);
        self.next += 1;
        let (signal, _) = tokio::sync::watch::channel(false);
        let job = Arc::new(Job {
            id: self.next,
            control: RenderControl::new(),
            settings,
            aovs,
            working_space,
            started: Instant::now(),
            done: AtomicU64::new(0),
            total: AtomicU64::new(0),
            finished: Mutex::new(None),
            signal,
        });
        let mut evicted = Vec::new();
        while self.jobs.len() >= RETAINED {
            if let Some(old) = self.jobs.pop_front() {
                evicted.push(old.id);
            }
        }
        self.jobs.push_back(job.clone());
        let (renderer, running) = (renderer.clone(), job.clone());
        let thread = std::thread::Builder::new()
            .name(format!("crust-render-{}", job.id))
            .spawn(move || {
                let job = running;
                let progress = |done: u64, total: u64| {
                    job.total.store(total, Ordering::Relaxed);
                    job.done.store(done, Ordering::Relaxed);
                };
                // The AOVs `crust render` requests for the stage: none without
                // products, so the beauty is the CLI's either way.
                let request = (!job.aovs.products.is_empty()).then_some(&*job.aovs);
                let rendered =
                    renderer.render_with_control(true, Some(&progress), request, &job.control);
                let finished = Finished {
                    buffer: rendered.buffer,
                    film: rendered.film,
                    rays: rendered.rays,
                    outcome: rendered.outcome,
                    seconds: job.started.elapsed().as_secs_f64(),
                };
                *job.finished.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(finished));
                job.signal.send_replace(true);
            })
            .map_err(|e| format!("cannot start the render thread: {e}"))?;
        self.thread = Some(thread);
        Ok((job, evicted))
    }

    /// The render `id`, while it is kept.
    pub fn get(&self, id: u64) -> Result<Arc<Job>, String> {
        self.jobs
            .iter()
            .find(|j| j.id == id)
            .cloned()
            .ok_or_else(|| {
                let kept: Vec<u64> = self.jobs.iter().map(|j| j.id).collect();
                format!("no render {id}: the session keeps renders {kept:?}")
            })
    }

    /// `cancel`: stops render `id` if it is the one running.
    pub fn cancel(&mut self, id: u64) -> Result<Arc<Job>, String> {
        let job = self.get(id)?;
        if self.jobs.back().is_some_and(|j| j.id == id) {
            self.stop();
        }
        Ok(job)
    }
}

/// The pixels of a finished render as `crust render` writes them, read
/// back: one [`Planes`] per file, keyed by the product's prim (`None` for
/// the beauty EXR of a stage without products). Each value is rounded as
/// its file stores it (a half channel to `f16`, an id channel to `u32`), so
/// a session render and the CLI's files compare bit for bit.
pub fn planes(
    finished: &Finished,
    aovs: &AovRequest,
    source: &str,
) -> Vec<(Option<String>, Planes)> {
    let stamp = crust_core::compare::Stamp::default();
    let film = match &finished.film {
        Some(film) if !aovs.products.is_empty() => film,
        _ => {
            let (w, h) = finished.buffer.size();
            let mut rgb = [Vec::new(), Vec::new(), Vec::new()];
            for y in 0..h {
                for x in 0..w {
                    let (r, g, b) = finished.buffer.get_rgb(x, y);
                    rgb[0].push(r);
                    rgb[1].push(g);
                    rgb[2].push(b);
                }
            }
            let channels = ["R", "G", "B"]
                .into_iter()
                .zip(rgb)
                .map(|(n, values)| {
                    (
                        n.to_owned(),
                        Channel {
                            size: (w, h),
                            values,
                        },
                    )
                })
                .collect();
            return vec![(
                None,
                Planes {
                    source: source.to_owned(),
                    width: w,
                    height: h,
                    channels,
                    stamp,
                },
            )];
        }
    };
    let size = film.dimensions();
    aovs.products
        .iter()
        .map(|product| {
            let mut channels = BTreeMap::new();
            for (var, names) in crate::products::product_channels(product) {
                for (name, plane) in names
                    .into_iter()
                    .zip(film.var_channels(&finished.buffer, var))
                {
                    let values = match var.precision {
                        Precision::Half => plane
                            .into_iter()
                            .map(|v| f16::from_f32(v).to_f32())
                            .collect(),
                        Precision::Float => plane,
                        Precision::Uint => {
                            plane.into_iter().map(|v| v as i32 as u32 as f32).collect()
                        }
                    };
                    channels.insert(name, Channel { size, values });
                }
            }
            let planes = Planes {
                source: format!("{source} {}", product.prim_path),
                width: size.0,
                height: size.1,
                channels,
                stamp: stamp.clone(),
            };
            (Some(product.prim_path.clone()), planes)
        })
        .collect()
}

/// `image`, box-filtered down to at most [`MAX_IMAGE_SIDE`] on its long side
/// in linear, tone-mapped through the CLI's default preview view, as PNG.
pub fn png(image: &Buffer, working_space: Space) -> Result<Vec<u8>, String> {
    let (w, h) = image.size();
    let f = w.max(h).div_ceil(MAX_IMAGE_SIDE).max(1);
    let (ow, oh) = (w.div_ceil(f), h.div_ceil(f));
    let mut rgb = Vec::with_capacity(ow * oh * 3);
    for oy in 0..oh {
        for ox in 0..ow {
            let (mut sum, mut n) = ([0.0f32; 3], 0.0f32);
            for y in oy * f..((oy + 1) * f).min(h) {
                for x in ox * f..((ox + 1) * f).min(w) {
                    let (r, g, b) = image.get_rgb(x, y);
                    sum[0] += r;
                    sum[1] += g;
                    sum[2] += b;
                    n += 1.0;
                }
            }
            rgb.extend(sum.map(|c| c / n));
        }
    }
    let color = OutputColor::new(working_space, PREVIEW_DISPLAY, PREVIEW_VIEW)?;
    let bytes = tone_map(&mut rgb, &color);
    let img = image::RgbImage::from_raw(ow as u32, oh as u32, bytes).ok_or("a malformed image")?;
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    Ok(out.into_inner())
}

/// A render's answer: its report as JSON, and its image as PNG content.
pub fn answer(job: &Job, extra: Value) -> rmcp::model::CallToolResult {
    use rmcp::model::{CallToolResult, ContentBlock};
    let mut report = job.report();
    if let (Value::Object(r), Value::Object(e)) = (&mut report, extra) {
        r.extend(e);
    }
    let mut content = Vec::new();
    match job.image().map(|b| png(&b, job.working_space)) {
        Some(Ok(bytes)) => {
            let data = base64::engine::general_purpose::STANDARD.encode(bytes);
            content.push(ContentBlock::image(data, "image/png"));
        }
        Some(Err(e)) => report["image_error"] = e.into(),
        None => report["image"] = "none yet: no unit has finished its first stage".into(),
    }
    content.push(ContentBlock::text(report.to_string()));
    let mut result = CallToolResult::success(content);
    result.structured_content = Some(report);
    result
}

/// Why `probe` names no prim: the table from a hit to its prim path is not
/// built yet.
const NO_PRIM_TABLE: &str = "crust keeps no table from a hit to its prim yet \
(change add-identity-aovs-openexrid), so the prim and its material are not known";

/// `probe`: the render's values at pixel (`x`, `y`) of the frame, in the
/// working space: the beauty, and once the render has returned every channel
/// of every product it gathered.
pub fn probe(job: &Job, x: usize, y: usize) -> Result<Value, String> {
    let r = job.settings.region();
    if !(r.x0..r.x1).contains(&x) || !(r.y0..r.y1).contains(&y) {
        return Err(format!(
            "({x}, {y}) is outside render {}'s region [{}, {}, {}, {}]",
            job.id, r.x0, r.y0, r.x1, r.y1
        ));
    }
    let (lx, ly) = (x - r.x0, y - r.y0);
    let image = job
        .image()
        .ok_or("the render has no image yet: no unit has finished its first stage")?;
    let (red, green, blue) = image.get_rgb(lx, ly);
    let mut aovs = serde_json::Map::new();
    let finished = job.finished();
    if let Some(f) = &finished
        && let Some(film) = &f.film
    {
        for product in &job.aovs.products {
            let mut values = serde_json::Map::new();
            for (var, names) in crate::products::product_channels(product) {
                // One pixel, not the planes: a probe reads one value each.
                let pixel = film.var_pixel(&f.buffer, var, lx, ly);
                for (name, value) in names.into_iter().zip(pixel) {
                    values.insert(name, json!(value));
                }
            }
            aovs.insert(product.prim_path.clone(), Value::Object(values));
        }
    }
    Ok(json!({
        "render_id": job.id,
        "status": job.status(),
        "x": x,
        "y": y,
        "beauty": [red, green, blue],
        "aovs": aovs,
        "aovs_note": if finished.is_none() {
            Some("the render is still running: its AOVs are gathered when it returns")
        } else if job.aovs.products.is_empty() {
            Some("the stage authors no RenderProduct, so the render gathered no AOV")
        } else {
            None
        },
        "prim": Value::Null,
        "material": Value::Null,
        "prim_note": NO_PRIM_TABLE,
    }))
}

/// One side of a `diff`: the beauty (`R`, `G`, `B`), then every product's
/// channels as its file holds them, named `<product>.<channel>`.
fn diff_planes(job: &Job, finished: &Finished) -> Planes {
    let source = format!("render {}", job.id);
    let mut all = planes(finished, &AovRequest::default(), &source)
        .pop()
        .map(|(_, p)| p)
        .expect("the beauty");
    if !job.aovs.products.is_empty() {
        for (product, p) in planes(finished, &job.aovs, &source) {
            let product = product.unwrap_or_default();
            for (name, channel) in p.channels {
                all.channels.insert(format!("{product}.{name}"), channel);
            }
        }
    }
    all
}

/// `diff`: two renders compared as `crust diff` compares two files — every
/// channel bitwise, error metrics on the beauty — refused unless both have
/// returned with the same samples per pixel and region.
pub fn diff(a: &Job, b: &Job) -> Result<Value, String> {
    let (Some(fa), Some(fb)) = (a.finished(), b.finished()) else {
        return Err("a render still running cannot be compared: wait for it, or cancel it".into());
    };
    let (sa, sb) = (
        a.settings.samples_per_pixel(),
        b.settings.samples_per_pixel(),
    );
    if sa != sb {
        return Err(format!(
            "render {} has {sa} spp and render {} {sb}: comparing them would be misleading",
            a.id, b.id
        ));
    }
    if a.settings.region() != b.settings.region() {
        return Err(format!(
            "renders {} and {} cover different regions: comparing them would be misleading",
            a.id, b.id
        ));
    }
    let report = crust_core::compare::compare(&diff_planes(a, &fa), &diff_planes(b, &fb));
    let mut value: Value = serde_json::from_str(&report.to_json()).map_err(|e| e.to_string())?;
    if let Value::Object(map) = &mut value {
        // The renders carry no file stamp; their settings were checked above.
        map.remove("comparability");
        map.insert("render_a".into(), json!(a.id));
        map.insert("render_b".into(), json!(b.id));
        map.insert("status_a".into(), json!(a.status()));
        map.insert("status_b".into(), json!(b.status()));
    }
    Ok(value)
}
