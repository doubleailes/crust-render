# Design

## Context

Output paths are resolved in one place in the CLI, `select_products` (`crates/crust-render/src/main.rs`):

```
productName (read at -f by the import)
        │
        ▼
select_products(aovs, -o)      -o replaces the first product's name
        │                      empty name / no writable vars → coded warning, dropped
        │                      refuse_shared_paths → product.shared_path
        ▼
anchor join (MCP session)      relative → session directory
        ▼
write_product / write_beauty   preview_png(path) = path with .png
```

`crust render` (`render_and_write`, also run by MCP's `render_final`) and `crust check` both call
`select_products`. `check` wraps the call in a `WarningScope`, so any coded warning raised there
appears in its report. The no-products case does not go through `select_products`: its path is
`beauty_output(-o)` (default `output.exr`). The frame is `RenderArgs.frame` / `SceneArgs.frame`
(`Option<f64>`, finite by `parse_frame`). Nothing in `crust-core` sees output paths, and that does
not change.

## Goals / Non-Goals

**Goals:**
- One expansion function that every output path goes through, in both commands, before collision
  detection.
- Exactly husk's names for the frame variables, in both of husk's forms, so a command line or stage
  written for husk names the same file under crust.

**Non-Goals:**
- No other variables: environment variables, `$HIP`/`$OS`-style Houdini variables, camera,
  product or AOV names.
- No frame ranges. One invocation still renders one time code.
- No expansion inside `crust-core`. Expanding `productName` at import would require the import to
  know CLI conventions, and `-o` would still need a second pass.

## Decisions

### D1. One `expand_frame_vars(path, frame) -> (String, Vec<Issue>)` in `crust-render`

It is a pure function: it takes a string and a frame (`f64`) and returns the expanded string plus
what it saw (unknown names, subframe use). It raises no warnings itself; the caller turns each
issue into a `warning!` that names the product prim (`at = …`), or nothing for `-o` with no
products. Keeping it pure lets the unit tests cover the whole grammar without a stage.
*Alternative:* expand inside `refuse_shared_paths`. Rejected because the no-products path never
reaches it, and the function would then do two jobs.

### D2. Expansion order: `-o` substitution → expansion → refusals → anchor join

`select_products` gains a `frame: f64` argument. It expands every product name (including the one
`-o` just replaced) **before** the empty-name check and `refuse_shared_paths`, so collisions are
judged on the files actually written (the image-output scenario "Products that collide only once
expanded"). The MCP anchor join runs after expansion, as it does now. `beauty_output` gains the
frame too and expands `-o` for the no-products case. `check` passes its `-f` so its report shows
the same files a render would write.

A name that expands to the empty string cannot happen: every variable expands to at least one
character, and an unknown one is kept as written.

### D3. Grammar

```
\$                      → literal "$"
$ { NAME }  |  $ NAME   → variable, NAME = [A-Za-z_][A-Za-z0-9_]*  (longest match, as Houdini)
$ anything else         → literal "$", no issue
NAME = F | F2..F9 | FF  → expanded;  anything else → kept verbatim + unknown-variable issue
< NAME >                → NAME = F | F2..F9 | FF expanded, exactly as $NAME
< anything else >       → literal, no issue
```

The `<F4>` form is husk's UDIM-style token. It matters more than `$F4` for stages: Houdini
expands `$F4` when it writes the stage, so a Solaris-authored `productName` that varies per frame
carries `<F4>`, never `$F4` (task 1.1). Any other `<…>` (`<UDIM>`, `<AOV>`) is left alone and
raises nothing. Angle brackets are not a variable syntax anywhere else crust reads, so a warning
would only be noise.

Matching takes the longest name, as Houdini does, so `$F4x` is the unknown name `F4x`, not `$F4`
followed by `x`. That is the safe failure: the file keeps a visible `$F4x` instead of silently
becoming `0012x`. A `\` that is not followed by
`$` is literal, so Windows paths are unaffected. An unclosed `${` is literal.

### D4. Number formatting

- `$F` / `$Fn`: `r = frame.round()` (halves away from zero, Rust's `f64::round`), formatted as
  `{:0n$}` on the `i64`. Rust counts the sign in the width, so `-3` with `$F4` gives `-003`. That
  matches Houdini's `padzero`, which pads the whole string.
- `$FF`: the shortest round-trip form of the `f64` (Rust's `{}`), with a trailing `.0` dropped, so
  `12` gives `12` and `12.5` gives `12.5`. `-0.0` prints as `0`.
- A subframe is `frame.fract() != 0.0`. Using `$F`/`$Fn` on one is an issue. Rounding was chosen
  over truncation so that `-f 12.999` (a common motion-blur-centred frame) names frame 13, not 12.
  The warning exists because either rule can make two subframes write one file.

### D5. Frame without `-f`: 1

The stage is rendered at its default values, which have no time code. Husk's default frame is 1,
so frame variables expand as 1. The `INFO` line that lists products shows the expanded name, so the
substitution is visible. Two alternatives were rejected. Refusing (an error) breaks the quickest
use of a stage authored with `$F4`. Using the stage's `startTimeCode` would name a frame whose
values the render did not read.

### D6. Two warning codes in `crust-core/src/warnings.rs`

- `product.unknown_variable` (kind `refused`, `Each`): an unknown name in a path, left as written.
- `product.subframe_frame` (kind `approximated`, `Each`): `$F`/`$Fn` rounded a fractional frame.

Both are listed in `site/content/docs/reference/warnings.md`. The existing test that keeps the list
and the reference in step enforces this. Adding codes bumps no report version.

### D7. Sources for the husk rules (task 1.1)

SideFX's documentation host is not reachable from the environment the change was implemented in,
so the rules rest on these sources:

- **`$F`, `$FF`, `$F4`, and `<F>`, `<FF>`, `<F4>` in husk's output path.** A SideFX forum reply
  lists both forms for husk's `-o` (sidefx.com/forum/topic/79320). A SideFX staff reply in another
  thread says a USD output path needs `<F4>` "in place of the `$F4`, which only works when
  rendering frame-by-frame since the value of `$F4` is evaluated in Houdini before it makes into
  the usd" (sidefx.com/forum/topic/74873).
- **Padding counts the sign, and halves round away from zero.** These are Houdini's `padzero` and
  `round` expression functions as documented (`padzero(4, -3)` gives `-003`). They are not
  re-checked here.
- **Default frame 1.** This is **unconfirmed**. The husk reference found only says
  `hou.frame()` returns the first frame to be rendered, and a forum question about husk's
  frameless default has no official answer. D5's reasons stand without it: crust renders the
  stage's default values, and 1 names a frame rather than refusing. A difference found later is a
  one-line change in `render_and_write` and `check`.

## Risks / Trade-offs

- [A path that already contains a literal `$F…`, now expanded] → Unlikely in a filename. `\$`
  escapes it, and the change is listed as **BREAKING (narrow)** in the proposal.
- [The shell eats `$F4` in an unquoted `-o`, giving `out..exr` silently] → The CLI cannot see
  this. The `-o` help text and `command-line.md` say to single-quote it. `$F4` in an authored
  `productName` is unaffected.
- [Husk's default frame or rounding rule differs from what is assumed here] → Task 1.1 checks
  SideFX's husk and expression documentation before any code is written. The spec states crust's rule explicitly, so a mismatch is a
  documented difference, not ambiguity.
- [`refuse_shared_paths` now sees more collisions (time-invariant `productName`s that differ only in
  a variable)] → That is the point: they would have overwritten each other.

## Migration Plan

None needed. Stages and command lines without `$` behave identically (pinned by the existing
product tests, which run unchanged). Rollback is reverting the commit.
