# Rust Code Assessment Report — Crust Render

**Assessed revision:** `70a7f38` (workspace version 0.4.0) · **Date:** 2026-10-03
**Scope:** the 7 workspace crates under `crates/` (~56k lines of production Rust, ~26k lines of test code), CI under `.github/workflows/`, manifests and tooling.
**Method:** I read the manifests, CI and source, and I ran these tools locally on rustc/clippy 1.97.0:

- `cargo clippy` with the CI gate, again with `clippy::pedantic` + `clippy::nursery`, and again with `missing_docs`
- `cargo fmt --check`
- `cargo test --workspace`
- `cargo llvm-cov` and `cargo audit`
- `cargo tree -d`

The standard used is the enterprise Rust checklist modelled on VGV (layered crates, typed errors, pedantic lints, ≥90% coverage, supply-chain gates). The report focuses on risks and gives no numeric scores.

> **Reading guide.** This is an unusually disciplined codebase for its size. Its `unsafe` is confined to one crate, its fallbacks are documented, its comments carry measurements, and it has 1,200+ tests. Most findings are therefore not "this code is sloppy". They fall into three other groups:
> - places where the project's own conventions diverge from enterprise Rust conventions (`Option` over `Result`, prose docs over rustdoc contracts, no lock file);
> - process gates that exist only as scripts a human must remember to run;
> - a small number of real correctness and robustness defects, called out as such.
>
> Each section ends with the action it implies.

---

## 1) Architecture & Crate Structure: Scalability Challenges

### Workspace & Modularity

The crate split is **sound at the top level**, and better than most projects this size:

| crate | role | depends on |
|---|---|---|
| `crust-rt` | intersection kernel (SBVH → BVH4) | glam, rayon only |
| `crust-mtlx` | MaterialX reader / shading-program compiler | glam, roxmltree only |
| `crust-jit` | Cranelift JIT for `crust-mtlx` programs | crust-mtlx, cranelift |
| `utils` | stateless math | glam |
| `crust-core` | engine: USD import, integrator, materials, lights, volumes, guiding, stats | all of the above |
| `crust-assets` | every file decoder and texture cache | crust-core (implements its `AssetLoader`) |
| `crust-render` | CLI binary | crust-core, crust-assets |

There are no cycles. The leaf crates (`crust-rt`, `crust-mtlx`) are kept free of renderer types. `crust-assets → crust-core` is a real dependency inversion: `AssetLoader` is defined in `crates/crust-core/src/scene.rs:184` and implemented in `crates/crust-assets/src/lib.rs:742`, so the engine has no image, EXR or Ptex dependency.

The problems are **inside `crust-core`**, which holds 36.5k of the 56k production lines and is the de-facto monolith:

- **USD import is a third of the engine.** `scene/usd_import/` plus `scene/subdiv.rs` is about 11.4k lines (31% of crust-core). One module tree mixes USD reading, tessellation policy, instancing, light linking, material binding and time evaluation. A `crust-usd` crate boundary would let the import be tested, versioned and (eventually) parallelised independently of the integrator. Today, any change to the importer recompiles and relinks the whole engine.
- **God functions rather than god structs.**
  - `trace_path` is about 660 lines (`crates/crust-core/src/tracer/path.rs:748-1408`). The entire integrator is one function: surface NEE, BSDF bounce, volume tracking, guiding and Russian roulette.
  - `load_scene` is about 335 lines (`usd_import/mod.rs:528-862`).
  - The CLI `main()` is about 335 lines (`crates/crust-render/src/main.rs:343-677`).
  - `path.rs` has no unit tests of its own, so `trace_path` can be tested only through whole renders. This is the single largest barrier to refactoring the integrator safely.
- **Public surface is wide and leaks internals.** crust-core has about 560 `pub` items against 55 `pub(crate)`, about 89 names re-exported from `lib.rs`, and zero `#[doc(hidden)]`. Leaks include:
  - `pub use material::*` (`lib.rs:93`), which also exports the `closure`, `materialx` and `preview_surface` modules wholesale (`material/mod.rs:9-15`);
  - `pub mod subsurface`, which exposes random-walk internals (`subsurface.rs:190`, `:295`);
  - `pub mod profile`, which is process-global state (see below);
  - `ray_color` (`tracer/path.rs:57`), public only so tests and benches can reach it;
  - about 20 stats types (`lib.rs:101-104`);
  - `Scene` (`scene.rs:14-25`) and `Renderer` (`tracer/mod.rs:104-114`), which have all-`pub` fields, so no invariant can be enforced on them.

  Most of this surface exists to serve the 19 integration-test files. In a published crate every one of these items is a SemVer commitment. This **complicates any internal refactor**, because changing a field type is formally a breaking change.
- **Asset concerns leak back into the engine.** Asset-only switches (Ptex and texture cache budgets, `PtexMipSpace`) live in crust-core's `Config` (`config.rs:172-215`). Asset-only stats types (`PtexCacheStats`, `TextureCacheStats`, `stats.rs:399`, `:477`) live there too. Adding a new decoder backend therefore touches the engine crate.
- **Duplicated dispatch chains.** The prim-type dispatch (`if let Ok(Some(x)) = Schema::get(..)`) appears in `usd_import/mod.rs:357-402` and again in `instancing.rs:186-291`. A new geometry type must be added to both.

**Cost of extension:**

| change | files touched | notes |
|---|---|---|
| new `Material` | 2-3 | Open trait (`material/material.rs:53`); good. |
| new BSDF lobe | 4-5 (about 15 match arms) | Closed enums on both sides of the crate boundary. `Lobe` is matched at `closure/mod.rs:167, 206, 735, 774, 797, 1004, 1032, 1134`; crust-mtlx `Bsdf` at `bsdf.rs:167, 239, 841` and `surface.rs:381, 821, 952, 1295`. |
| new light type | 6-7 | One `dispatch!` macro (`light/kind.rs:44-50`) contains the match, but the USD side has hard-coded schema lists (see below). |
| new integrator | — | No seam: `trace_path` is called directly (`tracer/mod.rs:787`). `SamplingStrategy` only changes MIS weighting. |

**Latent defect from a closed list.** `resets_xform_stack_at` (`usd_import/xform.rs:217-236`) and `local_matrix_via_openusd` (`:184-212`) each test six hard-coded schemas: Xform, Mesh, Sphere, Camera, SphereLight and RectLight. For any other prim type (DiskLight, CylinderLight, DistantLight, DomeLight, BasisCurves, PointInstancer) `!resetXformStack!` appears to be ignored. The fallback for those types is the identity matrix. The function is called at `mod.rs:189`, `:291` and `instancing.rs:390`. This is exactly the failure mode closed lists produce: the list is correct for the types that existed when it was written and silently wrong for every type added since. *Verify with a fixture before fixing.*

### Dependency Injection

**Strengths:**

- `AssetLoader` is a real seam with a documented contract: `None` means "fall back".
- `Material` and `Light` are traits.
- `Config` is parsed in exactly one place, and `Config::from_lookup` (`config.rs:251`) makes parsing testable without touching the environment.
- crust-assets accepts an injected config (`FileAssets::with_config`, `lib.rs:301`), and its tests A/B both sides of a switch in-process (`crust-assets/tests/auto_tx.rs:63,68`).

**Risks:**

- **crust-core reads the global config deep inside the import.**
  - `static CONFIG: LazyLock<Config>` (`config.rs:393`) is read through `config()` at 21 production call sites.
  - Ten of those are in crust-core: `usd_import/mesh.rs:202, 621, 964`, `attrs.rs:102`, `mod.rs:144, 875, 925`, `materialx.rs:336, 344` and `tracer/path.rs:104`.
  - The switches `subdiv`, `mesh_bake`, `stream_import`, `adaptive_*`, `mtlx_opt`, `shader_jit` and `ray_cones` are not part of `UsdImportOptions`.
  - `SubdivPolicy` caches the flag (`mesh.rs:202`), yet `attrs.rs:102` and `mod.rs:875` read the global again.
  - **This prevents in-process testing of the "off" side of every engine switch.** The tests at `attrs.rs:290` and `:310` begin with `if !crate::config().subdiv { return; }`, so under `CRUST_SUBDIV=0` they pass vacuously.
- **Process-global mutable state.**
  - `profile::ENABLED: AtomicBool` and `GLOBAL: Mutex<Option<Tree>>` (`profile.rs:160-161`) are shared by every render in the process. `set_enabled` wipes everyone's recording, and `take()` drains a process-wide tree.
  - `tests/profile.rs:1-3` acknowledges this: the test needs a binary of its own.
  - `MICRO_BYTES` (`crust-assets/src/ptex_stream.rs:265`) is a process-wide counter that mixes renders.
  - A host that renders two scenes concurrently, such as a Python binding or a render-farm worker, gets profiles and budgets that interfere with each other.
- **Thread-local import time.** `EvalTimeScope` (`usd_import/time.rs:7-45`) is well done: it is scoped, restored on drop, `!Send`, and its reasoning is documented. Still, it is the reason the importer cannot be parallelised, as `CLAUDE.md` itself notes. It is a hidden parameter on every attribute read.
- **No `Integrator`, `Film`/output, `Sampler` or `SceneImporter` trait.** The sampler is a type alias (`lib.rs:56`). None of these can be mocked, so the only way to test the integrator is to render.

**Action:** move every engine switch into an options struct passed down from `Renderer`/`UsdImportOptions`, and keep `config()` only at the CLI edge. Make `profile` a value owned by the render (or at least keyed per render). Shrink the crust-core surface with `pub(crate)` plus a `#[doc(hidden)] pub mod __test_support` for what the integration tests need. Consider splitting `crust-usd` out of `crust-core`.

---

## 2) Error Handling: Debugging & Reliability Risk

*VGV principle: descriptive exceptions, and documenting when calls may throw.*

### What is good

- **Typed errors, with no `anyhow`.**
  - crust-core has `Error` (`crates/crust-core/src/error.rs:7`), which implements `source()` and keeps the openusd error whole.
  - crust-assets has `AssetError` (`crates/crust-assets/src/error.rs:15`), with a source per decoder and a `path()` accessor.
  - crust-mtlx has `MtlxError` (`parse.rs:109`).
- **Very few panics in production code.** With inline `#[cfg(test)]` modules excluded, there are 12 `.unwrap()`, 36 `.expect()`, 1 `panic!` and 6 `unreachable!` in about 44k lines. Nearly all `expect`s state an invariant ("level 0 always exists", "lc > 0 implies bounds"). The one input-driven `unwrap` (`crust-assets/src/ies.rs:213,220`) is protected by the `n_h == 0` check at `ies.rs:92-96`.
- **The CLI never calls `process::exit`.** `main() -> ExitCode` returns `FAILURE` after an `error!` (`main.rs:359-362, 396-399, 548-551, 557-560`).

### Risks

- **The library's contract is "degrade to `None`", and that hides real failures.** The only fallible public entry points are `Scene::from_usd*` (`scene.rs:67, 80, 102, 119`). Once the stage is open, every failure becomes a fallback, often logged only at `DEBUG` (the default level is `info`, `main.rs:49`). The most consequential cases, all in production code:

  | # | location | what is lost |
  |---|---|---|
  | 1 | `usd_import/mod.rs:215, 309, 427` | `if let Ok(children) = prim.children()`: a composition error silently drops a **whole subtree**. |
  | 2 | `usd_import/mod.rs:161-167` | An error in `subtree_roots` becomes `Vec::new()`, silently disabling streaming import. |
  | 3 | `usd_import/mod.rs:357-402`, `instancing.rs:186-270` | `if let Ok(Some(_)) = Schema::get(..)`: a read **error** is indistinguishable from "not this type", so the prim is ignored. |
  | 4 | `usd_import/mesh.rs:474-486` | A mesh whose points or indices fail to read is skipped with a **`debug!`**, so geometry vanishes with no visible message. |
  | 5 | `usd_import/xform.rs:79-87` | `_ => return Some(GMat4::IDENTITY)` covers both "no `xformOpOrder` authored" and **"`xformOpOrder` read failed"**, so an object is silently placed at the origin. |
  | 6 | `usd_import/instancing.rs:814` | A positions read error becomes `unwrap_or_default()`, so the instancer has zero instances. |
  | 7 | `usd_import/materials.rs:102-107` | A `compute_bound_material` error gives the default material with no warning. |
  | 8 | `crust-assets/src/ptex_stream.rs:610, 669` | Tile I/O errors mid-render become the fallback colour with no log and **no counter**. The tiled cache counts the same errors (`tiled/cache.rs:571, 582`). |
  | 9 | `crust-assets/src/ies.rs:25-26` | `fs::read(path).ok()?`: the caller logs "Could not load" with no reason (`lib.rs:872`). `parse_ies` (`:39`) returns `Option`, so a malformed IES file and a missing one look the same. |
  | 10 | `usd_import/light_links.rs:124` | `expansion_rule().unwrap_or_default()` quietly changes light-linking semantics on a read error. |

  For an offline renderer, "the render finished but a subtree, a mesh or a transform is silently missing" is the worst outcome. It costs a farm night and a human to spot. `CLAUDE.md` says `WARN` means "something authored was refused, approximated or skipped". Items 1-8 break that rule.

- **There is no error report.** The import has no way to return "succeeded with N warnings" to a host. A `Diagnostics` collector, a `Vec<ImportIssue>` returned beside the `Scene`, would let a pipeline fail a render on a missing subtree without making every error fatal.
- **Uneven error types.**
  - `MtlxError` implements `Display` but **not `std::error::Error`** (`crust-mtlx/src/parse.rs:109-127`), so it cannot be `?`-boxed or chained.
  - `JitError(String)` (`crust-jit/src/lib.rs:94`) is stringly typed.
  - Three `FromStr` impls use `type Err = ()` (`config.rs`), which makes `"gathered" | "indexed" | "auto"` parse errors unreportable except by the caller re-deriving the valid set.
  - `MtlxError::Unsupported(String)` and `AssetError::Unusable { reason: String }` carry prose where a variant would let callers react.
- **No `# Errors` sections.** There are zero `# Errors` sections in the workspace. Clippy's `missing_errors_doc` reports 23 documented `Result`-returning functions without one, for example `crust-assets/src/ptex_stream.rs:373, 378, 397`, `ptex_texture.rs:137, 147` and `tiled/exr_write.rs:42`.
- **Panics in `extern "C"` callbacks abort.** `panic = "abort"` is set in release (`Cargo.toml:85`), and `host_apply`/`host_texture` are `extern "C"`, which aborts on unwind in edition 2024. So a panic inside texture I/O called from JIT code takes the process down with no error path. The `cache.rs:27-30` "nothing here may panic" contract is therefore load-bearing, and only code review enforces it.
- **No `#[non_exhaustive]`.** None of the public error enums has it (there are 0 uses in the workspace), so adding a variant to `Error`, `AssetError` or `MtlxError` is a breaking change.

**Action:**
- Introduce an import diagnostics channel.
- Split "absent" from "failed" in every `if let Ok(Some(..))` and `_ =>` arm of the importer.
- Raise skipped-geometry logs to `WARN`, keeping the per-prim detail at `DEBUG` with a `WARN` summary count.
- Implement `std::error::Error` for `MtlxError`, give `FromStr` a real error type, and mark the public error enums `#[non_exhaustive]`.
- Add `# Errors` sections and enable `clippy::missing_errors_doc`.

---

## 3) Type Safety & Correctness: Maintenance Deficits

### Newtype Patterns

Strengths:
- `PdfSolidAngle::new` refuses non-finite densities on both the sample and the pdf side, structurally.
- `NonZeroUsize`/`NonZeroU64` are used for budgets in `Config`.
- `Resolution::new` enforces the emission-before-`into_resolved` order through the type system, as `CLAUDE.md` describes.

Gaps:

- **Primitive obsession at the numeric seams.** Clippy pedantic reports, in production code alone:
  - 164 + 146 possibly truncating casts;
  - 145 + 75 + 41 + 36 precision-losing casts;
  - 116 sign-losing casts;
  - 82 casts that `From` could express infallibly.

  Most sit in index arithmetic (`u32` primitive ids, `usize` slots, `u8` arities and levels). Examples are `Val { arity: arity as u8 }` (`crust-jit/src/lib.rs:296`) and `remap[slot as usize]` (`crust-mtlx/src/lib.rs:89`). Each cast is individually correct today. Together they are 600+ places where a widened id space (for example Moana-scale instance counts past `u32`) would wrap silently instead of failing to compile. Newtypes such as `GeomId(u32)`, `SlotIdx(u16)` and `MipLevel(u8)`, with checked constructors, would make the reachable range part of the type.
- **Units are mostly unnamed.** Radiance, solid-angle pdf, area pdf and the three colour spaces are `f32`/`Vec3`. `CLAUDE.md` rule "every colour input states its colour space" is enforced by documentation, not by types. A `Linear<Rec709>` vs `Encoded` wrapper on texture outputs would turn the "wrong albedo decode renders plausibly" class of bug, which the project explicitly worries about, into a compile error.
- **251 strict float equality comparisons** (72 of them in production code) are flagged by `float_cmp`. Many are intentional, because bit-identity is a project invariant. They should be marked so, with `#[expect(clippy::float_cmp, reason = "...")]`, so that the unintentional ones become visible.

### Enum & Pattern Matching

- Matching is generally exhaustive. There is one `_ => {}` in production code, and catch-all arms are rare.
- **Exhaustiveness is defeated by error-conflating wildcards** in the importer, such as `_ => return Some(IDENTITY)` (`xform.rs:86`). These are the risky catch-alls: they swallow the `Err` arm, not a new enum variant.
- **`#[non_exhaustive]` is used nowhere** (0 uses across 38 public enums). See section 2.
- **A doc defect.** The rustdoc block written for `PtexMipSpace` (`crates/crust-core/src/config.rs:24-51`) is attached to `TriPackets` instead: two `///` blocks run together before `enum TriPackets` at `:58`. As a result `PtexMipSpace` (`:103`) is undocumented, and `TriPackets` shows the Ptex text in rustdoc. `missing_docs` reports it.

### Unsafe

The posture is strong:
- `#![forbid(unsafe_code)]` on crust-rt, crust-mtlx, crust-assets, utils and crust-render;
- `#![deny(unsafe_code)]` on crust-core, for a `#[cfg(test)]` `GlobalAlloc` (`scene/subdiv.rs:1974`);
- `#![deny(unsafe_code)]` on crust-jit;
- no `unsafe impl Send/Sync` anywhere.

The crust-jit catalogue:

| site | operation | assessment |
|---|---|---|
| `crust-jit/src/lib.rs:87` | `JITModule::free_memory` in `Drop` | Sound. `&mut self` excludes concurrent `eval`. |
| `:221` | `transmute::<*const u8, Shade>` | Sound given Cranelift's ABI. The SAFETY comment is good. |
| `:270` | `host_apply`: raw-pointer derefs, `from_raw_parts` | Sound for its only caller, but see below. |
| `:292` | `host_texture`: raw-pointer derefs | Same. |
| **`:248` (not counted)** | `(self.func)(slots.as_mut_ptr(), ctx)`, a call into generated machine code | **Executed from safe code.** |

**Soundness-convention defect.**
- `type Shade = extern "C" fn(*mut Val, *const ShadeCtx)` (`:55`) is a *safe* function-pointer type. So the call into JIT-generated code at `:248` needs no `unsafe` block, and the crate header's "four audited blocks" claim (`:33`) undercounts its own unsafety.
- `host_apply` (`:262`) and `host_texture` (`:279`) are **safe** `extern "C" fn`s that dereference raw pointer arguments. By Rust's soundness rules, a safe function that causes UB for some arguments is unsound: `host_apply(std::ptr::null(), ..)` from safe code is UB. They are private, so this is not exploitable from outside the crate.
- The fix is mechanical. Declare `type Shade = unsafe extern "C" fn(..)` and `unsafe extern "C" fn host_apply/host_texture`, and put the `:248` call in an `unsafe {}` block with its own `// SAFETY:` comment. The registration at `:138-139` is unchanged.
- No Miri or sanitizer run covers this crate. Miri cannot execute JIT code, but ASan or Valgrind on `tests/jit.rs` can.

---

## 4) Async & Concurrency: Safety & Performance Risk

### Tokio Patterns

There is **no async code at all**: no tokio, no `async fn`, no `.await`. That is correct for a CPU-bound offline renderer. The async-specific criteria (task tracking, blocking-in-runtime, network timeouts) do not apply. What does apply is **cancellation and shutdown**, and that is missing:

- **A render cannot be cancelled.**
  - `ProgressCallback = &(dyn Fn(u64, u64) + Sync)` returns `()` (`tracer/mod.rs:35`).
  - There is no stop flag and no signal handling.
  - A library host (DCC plugin, service, interactive viewer) cannot stop a render except by killing the process.
  - The CLI's Ctrl-C discards all work: output is written only after `render_with_stats` returns (`main.rs:524` then `:546-561`).
- **Outputs are written non-atomically.** The EXR (`main.rs:546`) and PNG (`:277`) go straight to the final path, so an interruption mid-write leaves a truncated file that a farm pipeline may pick up. The `.tx` writer already uses temp-file plus rename (`crust-assets/src/tiled/make.rs:213-235`), and the same pattern should apply here.
- **No thread-count control.** There is no `--threads` and no `ThreadPoolBuilder`; only `RAYON_NUM_THREADS` sets the count. The Ptex microcache reserve is sized from `available_parallelism()` (`ptex_stream.rs:221-224`), and the `--auto-tx` pool uses its own `std::thread::scope` sizing (`crust-assets/src/lib.rs:478-503`). Two thread-sizing policies disagree whenever `RAYON_NUM_THREADS` differs from the core count, as it does under a farm slot limit. When they disagree, the memory accounting is wrong.

### Shared State

**Strengths:**

- **The render loop is data-parallel with no shared mutable state.** It runs `par_iter_mut().for_each_init` over tiles or rows (`tracer/mod.rs:502-593`), and gathers the results in a fixed order. Output is therefore bit-identical regardless of schedule, and a bitwise test pins that.
- **The tile cache is carefully engineered.**
  - Lookups go through a per-thread set-associative microcache, then 64 shard mutexes, then disk.
  - Eviction uses `try_lock`, so render threads never wait on it (`tiled/cache.rs:661-664`).
  - The shard guard is dropped before `make_room` to avoid self-deadlock (`:619-635`).
  - Striped counters fixed measured cache-line bouncing (`:58-65`).
- **Poisoned locks are recovered via `into_inner`** almost everywhere (`cache.rs:705-710`, `profile.rs:171`, `tracer/mod.rs:542`). The only `.lock().unwrap()` calls are in the feature-gated `crust-rt/src/bvh/stats.rs:85-114`.
- **All 57 atomic sites use `Relaxed`.** That is justified: they are counters read after a rayon join, which provides the happens-before.

**Risks:**

- **Lock order is not documented as a hierarchy.**
  - `make_room` takes `sweeping` then each shard (`cache.rs:662, 673`).
  - `counters()` takes `files` then each `seen` (`:475-481`).
  - `FileAssets` holds `ptex` while calling `set_budget`, which takes the upstream reader's mutex (`lib.rs:964-968`).

  There is no cycle today, but the only guard is a local comment (`:619-623`). A `// LOCK ORDER: files > seen > readers > shard; sweeping > shard` header, or `parking_lot`'s deadlock detector in tests, would stop the next editor from introducing an inversion.
- **Check-then-act race (TOCTOU) in Ptex admission.** The "room for another stream" check releases the lock (`crust-assets/src/lib.rs:902-905`), and the push re-acquires it (`:964`), so concurrent loads can exceed `max_streams`. It is latent only because the import is single-threaded. Parallelising the import, which is the obvious next performance step, would activate it.
- **The `lock()` helper documents a branch that cannot happen.** In `tiled/cache.rs:531-533`, the docs say it returns `None` "on a poisoned lock", but it recovers poisoning and never returns `None`. Every `?` on it is dead code that implies an error path that doesn't exist.
- **Microcache tiles outlive their cache.** The per-thread `MICRO` thread-local (`tiled/cache.rs:740-751`) holds `Arc<Tile>`s outside the budget: up to 1.5 MiB per thread, so about 108 MiB at 72 threads. These live on persistent rayon workers, so in a long-lived host they stay alive after the `TileCache` is dropped. That is a cross-render memory leak for any non-CLI embedding. Ptex's microcache, by contrast, is charged to the budget (`ptex_stream.rs:226-252`).

**Action:**
- Add a cancellation token (`&AtomicBool` or a callback returning `ControlFlow`) checked once per tile.
- Have the CLI catch SIGINT/SIGTERM, flush the partial image, and write outputs via temp-file plus rename.
- Add `--threads` that builds the rayon pool and feeds the same number to every sizing heuristic.
- Document the lock hierarchy.
- Make the Ptex admission check-and-push a single critical section.
- Key the microcache by cache generation, or clear it when the cache is dropped.

---

## 5) Testing: Barrier to Expansion & Refactoring

*VGV principle: 100% coverage from the start (the Rust target here is ≥90%).*

### Coverage

The suite is **large and healthy**: 1,202 tests pass, 0 fail, 4 are ignored (`cargo test --workspace`, 42 test binaries).

| crate | integration (`tests/`) | inline (`src/`) |
|---|---|---|
| crust-core | 467 (19 files) | 289 |
| crust-assets | 49 | 82 |
| crust-rt | 60 | 70 |
| crust-mtlx | 94 | 38 |
| utils | 30 | 0 |
| crust-jit | 5 | 0 |
| crust-render | 0 | 22 |

**Measured coverage** (`cargo llvm-cov --workspace`): **89.97% of lines, 90.08% of regions and 91.58% of functions**, which meets the ≥90% target on regions and functions and falls just short on lines. There are 3,050 missed lines out of 30,399. Two caveats:
- `llvm-cov` also counts inline `#[cfg(test)]` modules, which inflates the figure slightly.
- **Nothing enforces the figure.** There is no coverage job in CI, so it can only drift.

The headline number hides where the gaps are:

| crate | line coverage |
|---|---|
| utils | 100.0% |
| crust-rt | 97.6% |
| crust-mtlx | 93.9% |
| crust-jit | 93.1% |
| crust-core | 89.3% |
| crust-assets | 86.0% |
| **crust-render (CLI)** | **68.3%** |

The least-covered files are the **error and fallback paths**, the very code that section 2 finds most consequential:

| file | lines | covered |
|---|---|---|
| `crust-assets/src/uv_texture/decode.rs` | 96 | 40.6% |
| `crust-core/src/error.rs` | 26 | 42.3% |
| `crust-assets/src/lib.rs` (the `FileAssets` seam) | 478 | 57.1% |
| `crust-core/src/scene/usd_import/xform.rs` | 151 | **64.9%** (the hard-coded schema fallbacks of section 1) |
| `crust-assets/src/error.rs` | 47 | 66.0% |
| `crust-core/src/stats.rs` | 912 | 68.0% |
| `crust-render/src/main.rs` | 556 | 68.4% |
| `crust-assets/src/ies.rs` | 224 | 75.5% |
| `crust-core/src/scene/usd_import/materials.rs` | 438 | 84.0% |
| `crust-core/src/scene/usd_import/instancing.rs` | 681 | 85.3% |

The happy paths are covered thoroughly. The ways input can fail are not.

**Untested or under-gated critical paths:**

- **The CLI has no end-to-end test.** `fn main()` is about 335 lines of orchestration (`main.rs:343-677`): settings overrides, stats assembly, the BVH descent report and output writing. Its 22 inline tests cover argument parsing, tone mapping and PNG writing. There is no `CARGO_BIN_EXE_crust-render` or `assert_cmd` test that runs the binary on `samples/cornellbox.usda` and checks the exit code and output files.
- **No golden-image regression in CI.** `scripts/check_images.sh` (record/check at 16 spp) is the project's own definition of "did the image change", but it runs only when a human remembers to. For a renderer whose bugs "render as something plausible", this is the most important missing gate.
- **Path-guiding unbiasedness is never gated.** `guided_render_is_unbiased` and `guided_render_with_tiles_smoke` (`crust-core/tests/guiding.rs:51, 64`) are `#[ignore]` ("renders frames") and CI never runs `--ignored` for them. CI runs only the subdivision memory probe that way (`rust.yml`).
- **`trace_path` has no unit tests** (`tracer/path.rs`, 1,497 lines, none inline). The integrator's correctness rests on whole-render statistical tests.
- **The JIT's unsafe code has 5 integration tests.** All compare against the interpreter bitwise, which is a good oracle, but there is no sanitizer run.

### Quality

**Strengths:**

- Test names read as sentences (average 41 characters; none start with `test_`). For example:
  - `a_garbage_file_is_an_error_not_a_panic` (`crust-core/tests/usd_inline.rs:2099`)
  - `streamed_and_preloaded_agree_texel_for_texel` (`crust-assets/tests/ptex_stream.rs:105`)
  - `homogeneous_transmittance_is_exact_beer_lambert` (`crust-core/tests/volumes.rs:464`)
- Every `#[ignore]` has a reason string, and both `#[should_panic]`s use `expected =`.
- Randomness is seeded.
- Statistical tests are checked against analytic results, and an OSL oracle (`crust-mtlx/tests/osl_oracle.rs`) pins MaterialX nodes to the reference implementation.
- No test mutates environment variables.
- The process-global profiler is isolated in its own test binary.

**Risks:**

- **Silent skips that count as passes.** `crust-jit/tests/jit.rs:97-109` and `crust-mtlx/tests/optimize.rs:114-126` check gitignored downloaded assets with `if p.exists() { check_file(&p); }`. When the file is missing, which is always the case in CI, they report **ok** having tested nothing. The same applies to `attrs.rs:290, 310` under `CRUST_SUBDIV=0`. Use `#[ignore = "needs downloaded assets"]` or an explicit skip message so the report tells the truth.
- **Fixed temp paths.**
  - There are 56 `std::env::temp_dir()` uses in tests, and only 6 are scoped by `process::id()`. Examples: `crust-render/src/main.rs:926`, `crust-core/tests/usd_inline.rs:89`, `usd_scene.rs:170, 782`, `crust-assets/tests/decoders.rs:22, 374-502`.
  - Several call `remove_dir_all` first (`tiled/cache.rs:833`, `tiled/make.rs:247`, `uv_texture/tests.rs:21`).
  - Two concurrent `cargo test` runs on one host can therefore delete each other's fixtures, for example two worktrees or a shared self-hosted runner.
  - The PID-scoped pattern already exists (`material/materialx/tests.rs:15`). Use it everywhere, or use `tempfile`.
- **A test name that overstates its test.** `a_budget_is_read_from_the_environment_and_a_bad_one_falls_back` (`crust-assets/src/ptex_stream.rs:833`) asserts a constant. Its own comment says "No env mutation".
- **Copy-pasted helpers.** There is no `tests/common/`, so `repo()`, `samples()`, `scratch()` and `write_stage()` are duplicated per file (`jit.rs:36`, `optimize.rs:34`, `decoders.rs:13-22`, `usd_inline.rs:88`).
- **Some terse names** in older modules: `packet_sizes` (`crust-rt/src/bvh/tests.rs:420`), `sheen_nonneg`, `fresnel_dielectric_sanity` (`openpbr/tests.rs:329, 296`), `instances_nest`.

### Property & Fuzz Testing

There is **none**: no `proptest`, `quickcheck`, `arbitrary` or `cargo-fuzz`. This is the biggest testing gap for a program whose job is to parse files authored elsewhere. Parsers of external input:

| parser | input |
|---|---|
| `crust-assets/src/ies.rs` | IES photometric text |
| `crust-mtlx/src/parse.rs` | MaterialX XML |
| `crust-assets/src/tiled/{read,exr_read}.rs` | tiled TIFF/EXR headers, tile offsets |
| `crust-assets/src/image_file.rs` | TIFF patching (`declare_unspecified_extra_sample_as_alpha` rewrites bytes in place) |
| USD import (`openusd` + `usd_import/`) | `.usda`/`.usdc`/`.usdz` |

`image_file.rs:30,37` calls `no_limits()` on the image decoder, justified as "trusted assets". A corrupt or hostile texture can therefore exhaust memory, which should be a deliberate, fuzzed decision.

The kernel is also a natural property-testing target. "Tri4 packets ≡ scalar triangles", "BVH hit ≡ linear scan" and "JIT ≡ interpreter" are already written as example-based tests. Expressing them as `proptest` properties would explore degenerate geometry (zero-area, NaN, denormal coordinates) mechanically.

**Action:**
- Add `cargo llvm-cov` to CI with a ratchet (fail below the current 89.9%, and raise the floor as gaps close).
- Run `check_images.sh check` against committed goldens in CI at 16 spp.
- Add a binary smoke test.
- Run the ignored guiding tests on a schedule.
- Add `cargo-fuzz` targets for IES, MaterialX, tiled-TIFF headers and the TIFF patcher.
- Add `proptest` for the kernel equivalence properties.

---

## 6) CI/CD & Tooling: Process Gaps

*VGV principle: automated gates before merge.*

### Pipelines

What exists is **good**:

- `rust.yml` runs fmt, clippy (`--all-targets -D warnings`) and test in parallel on a **pinned toolchain** (`RUST_VERSION: "1.98.1"`), plus the `--ignored` allocation probe.
- `nightly.yml` runs a pinned nightly (gating) plus the latest nightly (`continue-on-error`, daily cron), and the nightly-only `bvh8` feature.
- `docs.yml` builds the Zola site and checks its links.
- Third-party actions are pinned by commit SHA (`Swatinem/rust-cache@6323deb…`, `shalzz/zola-deploy-action@90cd842…`).
- `concurrency` cancels superseded runs, and the default permission is `contents: read`.

What is missing, against the VGV-equivalent gate list:

| gate | status |
|---|---|
| `cargo fmt --check` | ✅ |
| `cargo clippy -D warnings` | ✅ (default lint set only) |
| `cargo test --workspace` | ✅ |
| coverage with threshold | ❌ none (measured 89.97% lines locally; nothing enforces it) |
| `cargo audit` / `cargo deny` | ❌ none |
| golden-image regression | ❌ manual script only |
| benchmarks / perf regression | ❌ criterion benches only compiled, never run |
| MSRV check | ❌ no `rust-version` in any manifest |
| doc build (`cargo doc -D warnings`) | ❌ broken intra-doc links are not gated |
| Miri / sanitizers | ❌ |

- **The toolchain pin lives only in CI.** There is no `rust-toolchain.toml`, so contributors build with whatever `stable` they have, and the gate does not reproduce locally. **Observed during this audit:** on rustc 1.97.0, `cargo clippy --workspace --all-targets -- -D warnings` **fails**:

  ```
  error: manual implementation of `Option::zip`
    --> crates/crust-core/src/scene/usd_import/mesh.rs:1252
  ```

  CI on 1.98.1 presumably passes. A contributor following `CLAUDE.md` literally on a slightly older stable gets a red gate on code they didn't touch. A `rust-toolchain.toml` with `channel = "1.98.1"` fixes this in one line.
- **Supply-chain risk in the release job.**
  - `actions/checkout@v4` is tag-pinned, not SHA-pinned, unlike the other actions.
  - `rust-build/rust-build.action@v1.4.5` is tag-pinned and runs with `contents: write` and `GITHUB_TOKEN`.
  - A moved tag could publish tampered release binaries.
  - `dependabot.yml` covers only `cargo`, not `github-actions`.

### Conventions

- **Conventional Commits: not followed.** Of the 50 most recent commits, 1 uses a type prefix (`docs: add an Architecture section…`). The rest use change-name prefixes (`compact-triangle-storage: tick 4.1`), free-form imperatives (`Address review: tighten the far-light backoff…`), or the GitHub UI default (`Update Cargo.toml`, `cd11376`, which is the commit tagged `0.4.0`). The change-name prefixes are useful within the openspec workflow, but no changelog generator or semver-bump tool can consume them.
- **SemVer: shape only.**
  - Tags `0.1.0` → `0.4.0` exist with GitHub releases, but have no release notes.
  - No CHANGELOG exists.
  - `utils` is versioned independently at `0.1.0` while the rest of the workspace is `0.4.0` (`crates/utils/Cargo.toml:3`).
  - The crate name `utils` is generic enough to collide if the workspace is ever published.
- **No CONTRIBUTING.md and no SECURITY.md.** `CLAUDE.md` is effectively the contributor guide, and it is excellent, but it is aimed at an AI assistant. Humans won't look for it there.

### Tooling

| file | present? | note |
|---|---|---|
| `rustfmt.toml` | ❌ | default style; acceptable |
| `clippy.toml` / `[workspace.lints]` | ❌ | No workspace-level lint policy. Every crate inherits only the default set, and `forbid(unsafe_code)` is repeated per crate instead of declared once. |
| `deny.toml` | ❌ | No license, advisory, ban or source policy. Two git dependencies are unaudited. |
| `rust-toolchain.toml` | ❌ | see above |
| `Cargo.lock` committed | ❌ **gitignored** (`.gitignore:3`) | see section 8 |
| pre-commit hooks | ❌ | |
| spell check | ❌ | Docs volume is high (10k+ doc-comment lines plus `docs/` and `site/`), so `typos` would be cheap. |
| `.cargo/config.toml` | ✅ | Only a commented-out `x86-64-v3` opt-in, documented. |

**Action:** commit `Cargo.lock` and `rust-toolchain.toml`; add `[workspace.lints]`, `deny.toml` and a `cargo deny check` job; pin `actions/checkout` and the release action by SHA and add the `github-actions` ecosystem to Dependabot; add CHANGELOG, CONTRIBUTING and SECURITY files; adopt Conventional Commits, or at least a `type:` prefix alongside the change name.

---

## 7) API Design & Documentation: Onboarding Cost

### Public API

- **`#[must_use]` coverage is thin.** There are 27 annotations in the workspace. Pedantic clippy reports 295 methods and 71 functions that "could have a `#[must_use]` attribute", and 20 methods returning `Self` without one. In a builder-heavy kernel API (`crust-rt` scene construction), dropping a returned value is a silent no-op.
- **Construction is through plain structs with all-`pub` fields** (`Scene`, `Renderer`, `RenderSettings`), not builders. This is fast to write and maximally fragile under SemVer. Adding a field breaks every struct-literal construction downstream.
- **Visibility does not say what the API is.** Most of crust-core's 22 modules are private but declare their items `pub` and rely on `lib.rs` re-exports. Clippy's `redundant_pub_crate` also flags 68 `pub(crate)` items inside private modules. Only the `lib.rs` re-export list tells a reader what is public.
- **63 `#[inline(always)]`.** In a project that measures with callgrind this is probably deliberate, but nothing records which ones were measured. `#[expect(clippy::inline_always, reason = "measured: …")]` would preserve that knowledge.
- **Pedantic clippy totals 12,450 warnings across all targets.** About 8,150 are `unreadable_literal` in one generated table (`material/closure/bsdl_tables.rs`), and about 3,300 remain in hand-written production code. The largest groups are listed below.

  | warnings | lint | notes |
  |---|---|---|
  | 544 | `use_self` | |
  | 335 | `suboptimal_flops` | Must **not** be "fixed": `mul_add` changes rounding and would break the bit-identity invariants. Allow it at the workspace level, with a reason. |
  | 280 | `doc_markdown` | |
  | about 600 | the `cast_*` family | |

  Pedantic compliance is a 2-4 day mechanical job once a `[workspace.lints]` policy states which lints are rejected for domain reasons.

### Documentation

- **No `missing_docs` lint** on any crate. Measured with `-W missing_docs`, there are **545 undocumented public items**:

  | crate | undocumented items |
  |---|---|
  | crust-core | 264 |
  | crust-mtlx | 167 |
  | crust-assets | 57 |
  | crust-rt | 52 |
  | utils | 5 |

  358 of them are struct fields. Examples: `PtexMipSpace` (`config.rs:103`, see the misattached doc in section 3), `Renderer` (`tracer/mod.rs:104`), the `utils` crate root and its `Lerp` trait (`utils/src/common.rs:35`), and `ray_color` (`tracer/path.rs:57`).
- **Exactly one doc-test** in the workspace (`crust-rt/src/lib.rs:9-23`), against 573 `pub fn`. There are 0 `# Examples` sections, 0 `# Errors` sections and 0 `# Safety` sections; there are 10 `# Panics` sections. A new user of `crust-core` as a library has no runnable example of "load a USD file and render it" outside `main.rs`.
- **The prose documentation is excellent and roughly Diátaxis-shaped.**
  - **Tutorial:** `site/content/docs/getting-started/quick-start.md`.
  - **Reference:** `reference/command-line.md`, `environment-variables.md`, and `usd/*.md` for every `crust:*` attribute.
  - **Explanation:** `architecture/{overview,design-choices,limitations}.md`, `docs/*.md` (14 design records with measurements), and `openspec/specs/*/design.md` (8 capabilities, each ending in "Known gaps").
  - **How-to:** the one missing quadrant. The command cookbook in `openspec/specs/cli/design.md` is effectively it, but it is buried in a design record.

  The `README.md` (516 lines) covers features, build/run, the Moana benchmark and honest known limitations. It lacks a prerequisites/toolchain section and test instructions.
- **Where the documentation lives creates a risk.** The deepest knowledge (invariant pairs, traps already fallen into, A/B recipes) lives in `CLAUDE.md`, `docs/architecture.md` § Invariants and the openspec design records. None of it is enforced by rustdoc or the compiler. A contributor who never opens `openspec/` will break a "pair that must change together". The bitwise tests catch some of these pairs, but not the NEE ↔ bounce-weight pairing, which is statistical.

**Action:**
- Enable `#![warn(missing_docs)]` per library crate, ratcheting to `deny`.
- Fix the `PtexMipSpace`/`TriPackets` doc mix-up.
- Add a doc-tested "render a scene from a library" example to `crust-core/src/lib.rs`.
- Add `# Errors` to every public `Result` function.
- Mark the public error enums and settings structs `#[non_exhaustive]`.
- Move the command cookbook into a `site/` how-to section.

---

## 8) Dependencies: Supply Chain & Bloat Risk

### Dependency Audit

The tree is lean for what it does: about 180 unique packages, and no C/C++ toolchain is required. Direct dependencies are mainstream:

- glam, rayon, tracing, clap, image, exr, tiff, flate2, half, roxmltree, indicatif, criterion
- cranelift 0.136 (lockstep ×5)
- openusd 0.7 and openqmc-rs

Two are **git dependencies under the author's own GitHub account**:

| dependency | pin | location |
|---|---|---|
| `ptex = { package = "ptex-rust", git = "https://github.com/doubleailes/ptex-rs" }` | `rev = "884007d…"` | `Cargo.toml:40` |
| `opensubdiv-rs = { git = "https://github.com/doubleailes/OpenSubdiv-rs" }` | `tag = "0.5.0"` | `crates/crust-core/Cargo.toml:40` |

The `rev` pin is immutable. The **`tag` pin is not**: a re-pointed tag silently changes the build. Because `Cargo.lock` is not committed, nothing records which commit the tag resolved to. Both repositories are single-maintainer, so a bus-factor risk for two load-bearing components (Ptex texturing, Catmull-Clark subdivision) should be stated in `THIRD-PARTY.md`.

**Duplicate versions** (`cargo tree -d`):

| crate | versions | pulled by |
|---|---|---|
| `bitflags` | 1.3.2 / 2.x | `region` ← `cranelift-jit` |
| `hashbrown` | 0.16 / 0.17 | `gimli` ← `cranelift-codegen` |
| `miniz_oxide` | 0.8 / 0.9 | `exr`, `png` |
| `syn` | 2.x / 3.x | `derive_more` and `logos` (openusd) vs `bytemuck_derive` |
| `regex-automata`, `regex-syntax`, `memchr`, `fnv` | | build-graph splits |

All are upstream-driven and none is actionable today beyond a `cargo deny` `bans.multiple-versions = "warn"` to watch them.

### Security

**Advisory scan** (`cargo audit` against a freshly generated lock file): 3 findings, with no vulnerabilities but **two soundness advisories on a crate that ships**:

| crate | advisory | kind | path |
|---|---|---|---|
| `lru` 0.12.5 | RUSTSEC-2026-0002 | unsound (`IterMut` violates Stacked Borrows) | `ptex-rust` (git) → crust-assets |
| `lru` 0.12.5 | RUSTSEC-2026-0253 | unsound (possible use-after-free from missing panic safety in `LruCache::pop()`) | same |
| `paste` 1.0.15 | RUSTSEC-2024-0436 | unmaintained | `pulp` ← `exr` |

The `lru` findings matter more than their "warning" label suggests:
- The project's selling point is "safe Rust".
- `lru` sits under the streaming Ptex cache, on the render's hot path.
- `ptex-rust` is the author's own fork, so the fix (bump `lru` to a patched release) is fully in the project's control.

Without a committed lock file and an audit job, nothing would have surfaced this.

- **`Cargo.lock` is gitignored (`.gitignore:3`) in a workspace that ships a binary.** This is the most important supply-chain finding.
  - **Builds are not reproducible.** Release binaries for three targets are built from whatever the registry resolved on release day.
  - **`cargo audit` has nothing to audit in CI**, and Dependabot (`.github/dependabot.yml`) can only bump `Cargo.toml` requirements.
  - The repository already pays for this. The manifests contain paragraphs explaining why git dependencies must be pinned "because `Cargo.lock` is not checked in" (`Cargo.toml:22-27`, `crust-core/Cargo.toml:34-39`). `crust-assets/src/image_file.rs:185-188` ignores a canary test for the same reason.
  - Cargo's own guidance since 2023 is to commit the lock file for all projects, and unconditionally for binaries.
- **MSRV is not declared.** No `rust-version` field exists. The code uses let-chains and `is_multiple_of` (Rust ≥1.88 per the CI comment), but a user on an older toolchain gets a compiler error instead of Cargo's clear MSRV message.
- **Edition 2024 everywhere.** That is good: it brings `unsafe extern` and `unsafe` env mutation, which the project relies on.

### Compile Time

- **Eight proc-macro crates**, all transitive except clap's derive: `clap_derive`, `bytemuck_derive`, `derive_more-impl`, `logos-derive`, `strum_macros`, `thiserror-impl`, `zerocopy-derive`, `paste`. That is modest.
- **`lto = "fat"` plus `codegen-units = 1` in release.** It is justified with an instruction-count measurement (`Cargo.toml:58-72`), but it makes every release build a full serial LTO link, so the release CI and local `--release` loops pay for it. A `[profile.profiling]` that inherits `release` with `lto = "thin"` and `codegen-units = 16` would speed the iterate-and-measure loop. Keep fat LTO for shipped binaries.
- The `cranelift` stack is the heaviest compile-time dependency. It is optional at the `crust-core` level (`jit` feature) but **default-on in the binary** (`crust-render/Cargo.toml`, `default = ["jit"]`).

**Action:** commit `Cargo.lock`; replace the `opensubdiv-rs` `tag =` with `rev =`; add `rust-version = "1.88"` (or the true minimum) to `[workspace.package]`; add `cargo deny` with advisories, licenses, bans and sources (allow only crates.io and the two pinned git repositories).

---

## 9) Performance & Resource Management: Production Readiness

### Memory

The hot path is **clean, and the evidence is measured**:

- a hit returns `&dyn Material` with no `Arc` clone (`rt_world.rs:931`);
- per-worker `PathScratch` (`tracer/path.rs:222-246`);
- a thread-local MaterialX slot stack (`materialx.rs:97-103`);
- a capped per-thread closure pool (`closure/mod.rs:251-306`, `POOL_CAP = 16`), introduced after `memcpy` was measured at 17% of instructions;
- no `format!` or `Box<dyn>` per ray.

Risks:

- **Guiding training samples are unbounded.** They are pushed per path vertex (`path.rs:1356`) into per-work-unit `Vec`s that are held until the pass ends (`tracer/mod.rs:668`), with no cap and no `with_capacity`. Memory therefore scales with pixels × spp × depth on training passes. A high-resolution, high-depth guided render can grow without bound.
- **Microcache memory sits outside the budget and outlives the cache.** See section 4. That is about 108 MiB at 72 threads, never reclaimed in a long-lived host.
- **`ImportCaches`** (`usd_import/mod.rs:948-1003`) grow for the life of an import, by design: an epoch counter is bumped instead of clearing them. They are bounded by import lifetime, so this is acceptable for the CLI but worth stating for a long-lived host.

### I/O

- **Open file descriptors are unbounded.**
  - Each tiled file's reader pool "grows to the number of threads that ever miss concurrently" and is never trimmed (`crust-assets/src/tiled/cache.rs:383-387, 566-575`). The `files` list never shrinks.
  - The worst case is *textures × threads* descriptors. 1,000 `.tx` files on a 72-thread farm node is about 72k, against a common default `ulimit -n` of 1,024.
  - An `open()` failure becomes the fallback colour, an `errors` increment and a **`debug!`** (`:568-574`). **At the default log level this produces a silently wrong render.**
  - Ptex streams are capped by `max_streams`, so the tiled path is the outlier.
  - Fix: a global descriptor budget (an LRU of open readers), plus a `WARN` once when opens start failing.
- **Buffering is right.**
  - Tile cursors are `BufReader<File>` (`tiled/mod.rs:111`, `read.rs:22`).
  - The log file is deliberately unbuffered, because the subscriber is never dropped (`main.rs:352-356`).
  - TIFF sources are read whole into memory to be patched (`image_file.rs:26`), which is a documented trade-off.
- **There is no network I/O,** so timeouts, retries and pooling do not apply.
- **Non-atomic output writes.** See section 4.

### Observability

- **`tracing` is used, but only as a logger.**
  - There are about 236 macro call sites, **zero spans**, zero `#[instrument]` and zero structured `key = value` fields. Everything is format-string interpolation.
  - The workspace declares `tracing = { default-features = false }` (`Cargo.toml:12`), which also removes the `attributes` feature that `#[instrument]` needs.
  - Logs cannot be machine-parsed, and phase timings in the log cannot be correlated with phases.
- **`--stats` is text-only.** `RenderStats` implements `Display` (`stats.rs:703`) but has no `serde`/JSON form. A farm wanting to chart Mray/s, peak RSS or cache hit rates across renders must scrape a text table. Peak RSS is Linux-only (`/proc` `VmHWM`, `stats.rs:620-640`).
- **`--profile` is a strength.** It is per-thread, merged once per work unit, compiled out with a const-generic switch when off (`profile.rs:201-217`), and it measures its own overhead. It is, however, process-global (section 1).
- **No metrics export** (Prometheus or OpenTelemetry). That is acceptable for a CLI renderer. A `--stats-json <path>` would cover most of the need.

**Action:**
- Add an open-reader budget with a `WARN` on the first open failure.
- Cap or reserve the guiding training buffers.
- Add `--stats-json`.
- Enable `tracing`'s `attributes` feature and put spans on the import and render phases (`import`, `bvh_build`, `pass{n}`), with structured fields for counts.

---

## 10) Refactoring Estimation & Summary

### Top 3 Risks

1. **Silent degradation of authored data.** The importer turns read errors into "not this type", identity transforms, empty instancers or skipped meshes, often at `DEBUG` (section 2, items 1-8). The texture caches do the same for I/O errors, including descriptor exhaustion (section 9). Combined with a CI that has no golden-image gate (section 5), a regression or a bad asset produces *a plausible but wrong image with exit code 0*. In this domain that is the most expensive failure: it is discovered by a person, after a farm night.
2. **Non-reproducible and unaudited builds.** `Cargo.lock` is gitignored, one git dependency is pinned by a mutable tag, the toolchain pin lives only in CI (reproduced as a local clippy failure on 1.97.0), and there is no `cargo audit` or `cargo deny`. The release job uses a tag-pinned third-party action with write permissions. The project cannot answer "which exact code is in release 0.4.0?". The first audit run during this review found two soundness advisories (`lru` 0.12.5, via the author's own `ptex-rs`) in a project whose identity is "safe Rust".
3. **crust-core is not shaped for embedding or parallel evolution.**
   - Process-global profiler and config state, no cancellation, and microcache memory that outlives renders all block any long-lived host (DCC plugin, Python binding, render service).
   - A 560-item public surface with all-`pub` fields and no `#[non_exhaustive]` makes every internal refactor formally a breaking change.
   - A single 660-line `trace_path`, with no integrator seam, concentrates all integrator risk in one untestable function.

### Refactoring Scope

- **Task 1: build reproducibility and supply chain.**
  - Bump `lru` in `ptex-rs` to clear RUSTSEC-2026-0002 and RUSTSEC-2026-0253.
  - Commit `Cargo.lock` and `rust-toolchain.toml`.
  - Pin `opensubdiv-rs` by `rev`.
  - Add `rust-version`, `deny.toml`, and `cargo deny` plus `cargo audit` jobs.
  - Pin all actions by SHA and add the `github-actions` ecosystem to Dependabot.
- **Task 2: import diagnostics.**
  - Separate "absent" from "failed" in the importer.
  - Return an `ImportReport` with the `Scene`.
  - Raise skipped-content messages to `WARN` with a summary count.
  - Count and warn on Ptex stream I/O errors.
  - Add a `--strict` CLI flag that fails the render on any import error.
- **Task 3: CI regression gates.**
  - `check_images.sh check` at 16 spp against committed goldens.
  - A binary smoke test.
  - Scheduled runs of the ignored guiding tests.
  - `cargo llvm-cov` with a ratchet.
  - `cargo doc -D warnings`.
- **Task 4: lint and documentation policy.**
  - Add `[workspace.lints]` with pedantic enabled and domain exceptions stated (`suboptimal_flops`, `float_cmp` where bit-identity is intended, `inline_always` where measured).
  - Enable `missing_docs`, `missing_errors_doc` and `must_use_candidate`.
  - Fix the `PtexMipSpace` doc.
  - Add a library usage doc-test.
- **Task 5: crust-jit soundness hygiene.**
  - Use `unsafe extern "C" fn` for `Shade`, `host_apply` and `host_texture`, with a SAFETY-commented `unsafe` call site.
  - Run ASan or Valgrind on `tests/jit.rs` in the nightly workflow.
- **Task 6: error-type cleanup.**
  - `impl Error for MtlxError`.
  - Typed `FromStr` errors and a typed `JitError`.
  - `#[non_exhaustive]` on public error enums and settings structs.
  - `Result` instead of `Option` for `load_ies`/`parse_ies`.
- **Task 7: embedding readiness.**
  - A cancellation token, plus SIGINT handling that writes the partial image.
  - Atomic output writes.
  - `--threads` wired to every sizing heuristic.
  - Per-render profiler state.
  - Pass engine switches through options instead of `config()`.
  - Reclaim microcache memory when a cache is dropped.
  - A descriptor budget for tiled textures.
- **Task 8: fuzzing and property tests.** `cargo-fuzz` targets for IES, MaterialX, tiled-TIFF/EXR headers and the TIFF patcher; `proptest` for the kernel and JIT equivalences.
- **Task 9: structural.**
  - Narrow the crust-core public surface (`pub(crate)` plus a test-support module).
  - Split `trace_path` into per-vertex-kind functions with unit tests.
  - Extract `crust-usd` as its own crate.
  - Replace the hard-coded schema lists in `xform.rs` with a generic `Xformable` read (after verifying the `resetXformStack` defect).
  - Introduce `GeomId` and slot newtypes at the crate boundaries.
- **Task 10: process.** Add CHANGELOG (Keep a Changelog), CONTRIBUTING (derived from `CLAUDE.md`), SECURITY.md, and Conventional Commit prefixes, with release notes generated from them.

### Time Estimates

#### Minimal (Critical Gaps): 4–6 Days

- **Day 1:** Task 1 (lock file, toolchain, deny/audit, action pinning). It is mostly configuration, but the first `cargo deny` run will need license and ban triage.
- **Days 2–3:** Task 2 (import diagnostics, `WARN` levels, `--strict`). This touches about 10 sites in `usd_import/` and needs fixtures for each failure mode.
- **Day 4:** Task 3's golden-image and smoke gates, plus coverage reporting (no threshold yet).
- **Day 5:** Task 5 (the JIT `unsafe extern` change is about an hour; the sanitizer job is the rest) and Task 6.
- **Buffer:** the `PtexMipSpace` doc fix, the descriptor-exhaustion `WARN`, and the `xform.rs` fixture for the reset-stack defect.

Clippy-pedantic *compliance* is deliberately not in the minimal tier. About 3,300 hand-written warnings, a third of them domain-rejected, are better handled as a ratchet than as a blocker.

#### Comprehensive (Full Compliance): 5–7 Weeks

- Tasks 4 and 7: about 1.5 weeks. This includes about 3,300 pedantic warnings, 545 missing docs, and embedding work across crust-core and crust-assets.
- Task 8: about 1 week. Fuzz harnesses, corpus seeding from `samples/`, triage of what the fuzzers find, and proptest strategies for degenerate geometry.
- Task 9: about 2–3 weeks.
  - The `crust-usd` extraction and the public-surface narrowing ripple through 19 integration-test files.
  - Splitting `trace_path` must keep bit-identity, which `check_images.sh` can verify, and the NEE ↔ bounce pairing, which is statistical. This is the riskiest work and should land behind the golden-image gate from Task 3.
- Coverage: about 3 days. The workspace is already at about 90% of lines; the work is a CI ratchet plus targeted tests for the weak spots (CLI 68%, `xform.rs` 65%, `FileAssets` 57%, the error modules).
- Task 10 and slack: about 0.5 week.

The estimate assumes a senior Rust developer who also knows rendering. Someone new to the integrator's invariants should add about 50% to Task 9.

### Justification

The minimal tier converts the project's strongest informal disciplines (bit-identity checks, A/B measurement, documented fallbacks) into gates that run without a human remembering them. It closes the two failure modes that cost the most in production rendering: a plausible-but-wrong image that exits 0, and a release binary nobody can reproduce. The comprehensive tier is what makes crust-core safely embeddable and lets its integrator and importer evolve in parallel, at a defined API boundary, instead of through one 660-line function and a 560-item public surface. Without it, every feature added to the renderer increases the cost of the next one. The codebase's unusual quality means the starting point is good: most of this work is enforcement and boundary-drawing, not rewriting.
