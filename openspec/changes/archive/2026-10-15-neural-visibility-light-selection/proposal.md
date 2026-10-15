# Proposal

## Why

`learned` light selection fixed ALab's worst failure: power gave 49% of NEE's picks to lights that no receiver could see. It did so with a **uniform grid** of per-cell tables, though, and a grid has two measured weaknesses:
- A cell that straddles a shadow boundary averages visibility across it. That is why the defensive share had to rise to 0.3, or `domelight` fireflied.
- A cell's resolution is fixed by the receiver count, not by where visibility changes.

Bokšanský & Meister (*Neural Visibility Cache for Real-Time Light Sampling*, JCGT 14(2), 2025) store the same knowledge, each light's visibility from a point, as a continuous function: a multi-resolution hash-grid encoding feeding a small MLP. This change brings a CPU, safe-Rust, deterministic version of it to crust as an **opt-in experiment**. The experiment answers one question in numbers: on a CPU, does a neural visibility cache beat the `learned` grid at **equal time**?

## What Changes

- A new light selection mode, `neural`, through `crust:lightSelection = "neural"` and `--light-selection neural`. It is opt-in, and `power` stays the default.
  - A deterministic training pre-pass labels each light **visible or not** from a set of receivers, using the shadow rays NEE itself would cast.
  - A small network learns each light's visibility probability `V̂ℓ(x)` from position.
  - NEE picks light ℓ at `x` with `(1 − D) · πℓ · V̂ℓ(x) / Σ + D / n_live`. Here `π` is the existing power table and `D` the defensive uniform share, which keeps every emitting light sampleable.
  - The bounce side weights emission with the same pmf, evaluated at the previous vertex, so MIS stays consistent.
- A new workspace crate, `crust-nn`. It holds a tiny, dependency-free, `forbid(unsafe_code)` neural-network kit: hash-grid encoding, dense layers, sigmoid binary cross-entropy, Adam, gradient checking. Its inference and training are bit-deterministic.
- When the network cannot help, it falls back to power selection, as `learned` does:
  - fewer than two lights, or no receivers → silent fallback;
  - more lights than the network's output layer supports → fallback with a `WARN`.
- A probe example, `nvc_bench`, times one inference against one shadow ray on the same scene. This is the go/no-go gate before any integration work.
- Documentation:
  - user pages (`reference/command-line.md`, `usd/render-settings.md`, architecture overview);
  - `docs/light_sampling.md` (a new §3.x with the measurements, whatever they show);
  - the `lighting` design record;
  - `docs/architecture.md` and CLAUDE.md, since the workspace grows from seven crates to eight.

Nothing changes for scenes that don't ask for `neural`: `power`, `uniform` and `learned` render bit-identically to before.

## Capabilities

### New Capabilities

None. Neural selection is another light selection mode, owned by `lighting`. `crust-nn` is an internal library, not a user-visible capability.

### Modified Capabilities

- `lighting`:
  - the "Light selection" requirement gains the `neural` choice;
  - new requirements cover its fallback, unbiasedness, MIS consistency and determinism.

## Impact

- **Code:**
  - new `crates/crust-nn/` (~600–900 lines with tests);
  - new `crates/crust-core/src/light_visibility.rs` (pre-pass labels, training loop, the per-vertex pmf);
  - `light/list.rs`: the `LightSelection::Neural` variant, and the `*_at` lookups routed to the network;
  - `tracer/mod.rs`: training hook beside `learned`'s;
  - `light_cache.rs`: the receiver walk is shared rather than duplicated;
  - `crust-render` gets the CLI choice automatically from `LightSelection::CHOICES`;
  - the new `examples/nvc_bench.rs`.
- **Dependencies:** none added. `crust-nn` uses only `std`, and `crust-core` depends on it by path.
- **Performance:**
  - No change unless `neural` is selected.
  - When it is: a pre-pass, plus one inference per NEE vertex. A thread-local memo serves the bounce side's lookup at the same vertex from that one inference.
  - The inference cost is **the** risk (see design.md), and the bench gate exists to measure it first.
- **Determinism:** tiles ↔ scanlines bit-identity, and independence from thread count, must hold for `neural` as they do for `learned`.
