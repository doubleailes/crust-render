# `.tx` reader probes

Throwaway probe programs backing the measurements in
[`docs/oiio_tx_texture_evaluation.md`](../../docs/oiio_tx_texture_evaluation.md).
They are **not** part of the workspace build — they are kept so the numbers in that
document can be reproduced or challenged.

## Producing the fixtures

Needs the OpenImageIO Python wheel (`pip install OpenImageIO numpy`, tested with
3.1.15). `gen_tx.py` writes `checker_u8.tx`, `checker_f32.tx`, `checker_u16_lzw.tx`
via `ImageBufAlgo.make_texture` — the same code path as `maketx` — and prints the
MIP/tile structure OIIO itself reports.

The larger fixtures used for the RAM and throughput tables are generated inline in
the doc's §3.3–§3.5 measurements: a 4096² RGB float source written as
`big_f32.tx` (zip), `big_none.tx` (`compression=none`) and `big_u8.tx` (uint8).
They total ~450 MB, so they are not checked in.

## Running the probes

Create a scratch binary crate, `cargo add tiff exr`, and drop the probe in as
`src/main.rs` (or `src/bin/<name>.rs`):

| Probe | Measures |
|---|---|
| `rss.rs` | per-phase I/O accounting, peak RSS for per-tile vs whole-image reads, `TileOffsets` inspection |
| `cost.rs` | where per-tile time goes: `seek_to_image` vs `read_chunk` vs buffer reuse vs level switching |
| `compat.rs` | magic-byte sniffing and a compatibility matrix over RGBA / `planarconfig separate` / 8-channel / EXR-flavoured `.tx` |
| `chan8.rs` | the >4-channel mis-striding bug (doc §4.1), against OIIO ground truth |

Numbers in the doc were taken in a 4-core container with a warm page cache,
`--release`, single runs — indicative, not a rigorous benchmark.
