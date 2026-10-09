//! `crust mcp`: a Model Context Protocol server over stdin/stdout, for an
//! agent that keeps one USD stage open, edits it through composition and
//! renders it (the `mcp-session` capability).
//!
//! stdout carries only protocol messages, so `main` sends the log to stderr
//! and nothing here draws a progress bar. tokio carries the protocol and
//! nothing else, on one thread: the session's stage lives on a thread of its
//! own ([`session`]), and renders on the rayon pool.

mod edit;
mod layer;
mod query;
mod render;
mod scene;
mod session;
#[cfg(test)]
mod tests;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, ContentBlock, Implementation, InitializeResult, ServerCapabilities,
};
use rmcp::{ErrorData, ServiceExt, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use session::{Session, SessionGone};
use std::process::ExitCode;
use std::time::Duration;
use tracing::{debug, error};

/// What a client is told about the server when it initialises.
const INSTRUCTIONS: &str = "crust renders USD stages. A session edits one stage through an \
override layer that sublayers it, and renders the result. Call open_session first, with \
absolute paths; every edit is an opinion in the override layer, saved and re-imported at \
once. Renders return within their time budget and keep refining: use snapshot to see more.";

/// The server's protocol side: each tool sends its command to the session
/// thread and answers with what comes back.
#[derive(Clone)]
struct Server {
    session: Session,
}

/// A tool's answer: a JSON result, or the message of a refusal — which the
/// client sees as a tool error (`isError`), not a protocol one.
type Answer = Result<serde_json::Value, String>;

fn reply(answer: Result<Answer, SessionGone>) -> Result<CallToolResult, ErrorData> {
    match answer {
        Ok(Ok(value)) => Ok(CallToolResult::structured(value)),
        Ok(Err(message)) => Ok(CallToolResult::error(vec![ContentBlock::text(message)])),
        Err(gone) => Err(ErrorData::internal_error(gone.to_string(), None)),
    }
}

/// A render's answer, or the tool error for a render id the session does
/// not keep.
fn job_answer(
    job: Result<Result<std::sync::Arc<render::Job>, String>, SessionGone>,
) -> Result<CallToolResult, ErrorData> {
    match job {
        Ok(Ok(job)) => Ok(render::answer(&job, serde_json::json!({}))),
        Ok(Err(message)) => Ok(CallToolResult::error(vec![ContentBlock::text(message)])),
        Err(gone) => Err(ErrorData::internal_error(gone.to_string(), None)),
    }
}

#[derive(Deserialize, JsonSchema)]
struct OpenSession {
    /// The USD stage to work on (.usda, .usdc or .usdz). It is never written.
    input: String,
    /// The override layer to write (a new .usda), or an existing one of the
    /// same input to resume. It sublayers the input and holds every edit.
    output: String,
}

#[derive(Deserialize, JsonSchema)]
struct Query {
    /// A prim path (`/World/lights/key`) or an attribute path
    /// (`/World/lights/key.inputs:exposure`).
    path: String,
}

#[derive(Deserialize, JsonSchema)]
struct SetAttribute {
    /// The attribute, as `/prim/path.name` (`/World/lights/key.inputs:exposure`).
    path: String,
    /// The value, as JSON shaped like the attribute's type: a number for
    /// `float`, `[r, g, b]` for `color3f`, a string for `token` or `asset`
    /// (a relative asset path is read from the input stage's directory), an
    /// array for an array type.
    value: serde_json::Value,
    /// The USD type, needed only when no layer declares the attribute (for
    /// a schema attribute nothing authors yet, such as a light's
    /// `inputs:exposure`): `float`, `color3f`, `asset`, …
    #[serde(rename = "type", default)]
    ty: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct SetVariant {
    /// The prim holding the variant set.
    prim: String,
    /// The variant set (`lod`, `modelingVariant`, …).
    variant_set: String,
    /// The variant to select.
    variant: String,
}

#[derive(Deserialize, JsonSchema)]
struct SetActive {
    /// The prim to activate or deactivate.
    prim: String,
    /// `false` removes the prim and its descendants from the stage.
    active: bool,
}

#[derive(Deserialize, JsonSchema)]
struct BindMaterial {
    /// The prim to bind (a mesh, or an ancestor whose descendants inherit it).
    prim: String,
    /// The Material prim to bind it to.
    material: String,
}

#[derive(Deserialize, JsonSchema)]
struct AuthorUsda {
    /// USDA text of prim specs (`over`, `def`, `class`), merged into the
    /// override layer as one edit: each field it authors replaces the
    /// layer's, sibling prims and properties are kept. The `#usda 1.0`
    /// header may be left out. `subLayers` is refused.
    text: String,
}

#[derive(Deserialize, JsonSchema)]
struct Render {
    /// Render only this rectangle of the frame: `[x0, y0, x1, y1]` in pixels
    /// from the top-left corner, x1 and y1 excluded, as `crust render
    /// --region`. Each pixel renders exactly as in the full frame.
    #[serde(default)]
    region: Option<[usize; 4]>,
    /// Samples per pixel, as `crust render -s`. Defaults to the stage's.
    #[serde(default)]
    spp: Option<u32>,
    /// Return after at most this many seconds (default 10) with the latest
    /// image; the render goes on refining.
    #[serde(default)]
    budget_s: Option<f64>,
}

#[derive(Deserialize, JsonSchema)]
struct Probe {
    /// The id `render` returned.
    render_id: u64,
    /// The pixel's column, from the frame's left edge.
    x: usize,
    /// The pixel's row, from the frame's top edge.
    y: usize,
}

#[derive(Deserialize, JsonSchema)]
struct Diff {
    /// The reference render's id.
    render_a: u64,
    /// The id of the render compared with it.
    render_b: u64,
}

#[derive(Deserialize, JsonSchema)]
struct RenderId {
    /// The id `render` returned.
    render_id: u64,
}

/// How long `render` waits by default before answering.
const DEFAULT_BUDGET_S: f64 = 10.0;

#[tool_router]
impl Server {
    /// Opens a session: creates the override layer `output` on `input` (or
    /// resumes it when it already is one) and imports it as `crust render
    /// <output>` would. Closes any session already open.
    #[tool]
    async fn open_session(
        &self,
        Parameters(p): Parameters<OpenSession>,
    ) -> Result<CallToolResult, ErrorData> {
        reply(
            self.session
                .call(move |state| state.open_session(&p.input, &p.output))
                .await,
        )
    }

    /// What the session's stage composes at a prim (type, active, children,
    /// variant sets, authored attributes and relationships, and the layers
    /// that author it) or at an attribute (type, value at the default time,
    /// and the layers that author it, strongest first: `source` is the one
    /// the value comes from, or `fallback`).
    #[tool]
    async fn query(&self, Parameters(p): Parameters<Query>) -> Result<CallToolResult, ErrorData> {
        reply(
            self.session
                .call(move |state| query::query(&state.session()?.stage, &p.path))
                .await,
        )
    }

    /// Sets an attribute: authors its value as an opinion in the override
    /// layer (an `over` where the prim is defined elsewhere), saves the layer
    /// and re-imports it. The value takes the attribute's declared type; one
    /// that does not convert is refused, nothing authored. The result says
    /// which warning codes the import gained or lost, and how long it took.
    #[tool]
    async fn set_attribute(
        &self,
        Parameters(p): Parameters<SetAttribute>,
    ) -> Result<CallToolResult, ErrorData> {
        reply(
            self.session
                .call(move |state| {
                    state
                        .session()?
                        .set_attribute(&p.path, &p.value, p.ty.as_deref())
                })
                .await,
        )
    }

    /// Selects a variant: authors the variant selection on the prim in the
    /// override layer, saves it and re-imports.
    #[tool]
    async fn set_variant(
        &self,
        Parameters(p): Parameters<SetVariant>,
    ) -> Result<CallToolResult, ErrorData> {
        reply(
            self.session
                .call(move |state| {
                    state
                        .session()?
                        .set_variant(&p.prim, &p.variant_set, &p.variant)
                })
                .await,
        )
    }

    /// Activates or deactivates a prim (its `active` metadata) in the
    /// override layer, saves it and re-imports.
    #[tool]
    async fn set_active(
        &self,
        Parameters(p): Parameters<SetActive>,
    ) -> Result<CallToolResult, ErrorData> {
        reply(
            self.session
                .call(move |state| state.session()?.set_active(&p.prim, p.active))
                .await,
        )
    }

    /// Binds a material: authors `material:binding` (and MaterialBindingAPI)
    /// on the prim in the override layer, saves it and re-imports.
    #[tool]
    async fn bind_material(
        &self,
        Parameters(p): Parameters<BindMaterial>,
    ) -> Result<CallToolResult, ErrorData> {
        reply(
            self.session
                .call(move |state| state.session()?.bind_material(&p.prim, &p.material))
                .await,
        )
    }

    /// Authors a USDA snippet of `over` / `def` / `class` prim specs into the
    /// override layer as one edit: one save, one re-import, one undo step.
    /// Use it for several opinions at once, or for new prims (a light, a
    /// material). Text that does not parse, or that authors `subLayers`, is
    /// refused with the layer unchanged.
    #[tool]
    async fn author_usda(
        &self,
        Parameters(p): Parameters<AuthorUsda>,
    ) -> Result<CallToolResult, ErrorData> {
        reply(
            self.session
                .call(move |state| state.session()?.author_usda(&p.text))
                .await,
        )
    }

    /// Restores the override layer to its content before the last edit, saves
    /// it and re-imports. History lasts for the server process: after a
    /// resume, earlier sessions' edits are out of reach.
    #[tool]
    async fn undo(&self) -> Result<CallToolResult, ErrorData> {
        reply(self.session.call(|state| state.session()?.undo()).await)
    }

    /// Starts a progressive render of the session's scene and returns within
    /// `budget_s` with the latest image (PNG, at most 1024 pixels on its long
    /// side, tone-mapped as `crust render`'s preview), the samples per pixel
    /// reached, whether it is done, and its id. The render keeps refining
    /// after the call returns, until it completes, `cancel` stops it, or an
    /// edit or a new render cancels it. The session keeps its 8 latest
    /// renders for `snapshot`, `probe` and `diff`.
    #[tool]
    async fn render(&self, Parameters(p): Parameters<Render>) -> Result<CallToolResult, ErrorData> {
        // The budget counts from the call, so starting the render (a new
        // light selection) is inside it.
        let asked = std::time::Instant::now();
        let budget_s = p.budget_s.unwrap_or(DEFAULT_BUDGET_S);
        let Some(budget) = Duration::try_from_secs_f64(budget_s)
            .ok()
            .filter(|b| !b.is_zero())
        else {
            return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                "budget_s {budget_s} is not a positive number of seconds"
            ))]));
        };
        let started = self
            .session
            .call(move |state| state.session()?.start_render(p.region, p.spp))
            .await;
        match started {
            Ok(Ok((job, evicted))) => {
                job.wait(budget.saturating_sub(asked.elapsed())).await;
                Ok(render::answer(
                    &job,
                    serde_json::json!({ "evicted": evicted }),
                ))
            }
            Ok(Err(message)) => Ok(CallToolResult::error(vec![ContentBlock::text(message)])),
            Err(gone) => Err(ErrorData::internal_error(gone.to_string(), None)),
        }
    }

    /// The latest image and progress of a render, without waiting.
    #[tool]
    async fn snapshot(
        &self,
        Parameters(p): Parameters<RenderId>,
    ) -> Result<CallToolResult, ErrorData> {
        job_answer(
            self.session
                .call(move |state| state.session()?.renders.get(p.render_id))
                .await,
        )
    }

    /// Stops a render and keeps what it has done: its image, AOVs and samples
    /// stay available to `snapshot`, `probe` and `diff`.
    #[tool]
    async fn cancel(
        &self,
        Parameters(p): Parameters<RenderId>,
    ) -> Result<CallToolResult, ErrorData> {
        job_answer(
            self.session
                .call(move |state| state.session()?.renders.cancel(p.render_id))
                .await,
        )
    }

    /// A pixel's values in a render, in its linear working space: the beauty,
    /// and every AOV channel of the stage's RenderProducts once the render
    /// has returned. `prim` and `material` (what the camera ray hit there)
    /// are null until crust keeps a table from hits to prims.
    #[tool]
    async fn probe(&self, Parameters(p): Parameters<Probe>) -> Result<CallToolResult, ErrorData> {
        reply(
            self.session
                .call(move |state| {
                    let job = state.session()?.renders.get(p.render_id)?;
                    render::probe(&job, p.x, p.y)
                })
                .await,
        )
    }

    /// Compares two renders as `crust diff` compares two EXRs: whether every
    /// channel is bitwise identical, and error metrics on the beauty against
    /// `render_a`. Refused for renders with different samples per pixel or
    /// regions, and for a render still running.
    #[tool]
    async fn diff(&self, Parameters(p): Parameters<Diff>) -> Result<CallToolResult, ErrorData> {
        reply(
            self.session
                .call(move |state| {
                    let renders = &state.session()?.renders;
                    let (a, b) = (renders.get(p.render_a)?, renders.get(p.render_b)?);
                    render::diff(&a, &b)
                })
                .await,
        )
    }

    /// The session's result: renders the saved override layer exactly as
    /// `crust render <output>` would, and lists the files written. Every
    /// RenderProduct the stage authors is written, its productName resolved
    /// against the layer's directory; a stage without products writes
    /// `<output stem>.exr` and its PNG preview beside the layer. Stops any
    /// session render first. It blocks the session until the render is done.
    #[tool]
    async fn render_final(&self) -> Result<CallToolResult, ErrorData> {
        reply(
            self.session
                .call(|state| state.session()?.render_final())
                .await,
        )
    }

    /// The `crust-check/1` report of the session's current import: what
    /// `crust check -i <output> --json -` reports on the saved layer — the
    /// render it describes, its effective settings, the import's costs and
    /// counts, the findings that need no render, and every warning.
    #[tool]
    async fn check(&self) -> Result<CallToolResult, ErrorData> {
        reply(
            self.session
                .call(|state| {
                    let json = state.session()?.scene.report.to_json();
                    serde_json::from_str(&json).map_err(|e| e.to_string())
                })
                .await,
        )
    }
}

#[tool_handler]
impl rmcp::ServerHandler for Server {
    fn get_info(&self) -> InitializeResult {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(
                Implementation::new("crust", env!("CARGO_PKG_VERSION")).with_title("Crust Render"),
            )
            .with_instructions(INSTRUCTIONS)
    }
}

/// `crust mcp`: serve until the client closes stdin. 0 when it did, 1 when
/// the protocol could not start or the server failed.
pub fn run() -> ExitCode {
    let (session, _thread) = match Session::spawn() {
        Ok(spawned) => spawned,
        Err(e) => {
            error!("cannot start the session thread: {e}");
            return ExitCode::FAILURE;
        }
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            error!("cannot start the MCP runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    runtime.block_on(async {
        let running = match (Server { session }).serve(rmcp::transport::stdio()).await {
            Ok(running) => running,
            Err(e) => {
                error!("MCP initialisation failed: {e}");
                return ExitCode::FAILURE;
            }
        };
        match running.waiting().await {
            Ok(reason) => {
                debug!("MCP server stopped: {reason:?}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                error!("MCP server failed: {e}");
                ExitCode::FAILURE
            }
        }
    })
}
