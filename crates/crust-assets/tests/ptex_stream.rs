//! The streamed Ptex backend against the preloaded one.
//!
//! These are integration tests rather than unit tests because the thing under
//! test is *addressing a file*. The Ptex tests that existed before this
//! hand-built their arenas in memory — `ptex-rs` is a reader, and the
//! repository shipped no `.ptx` — which pins the mip reduction and the lookup
//! but cannot pin a tile grid, a stored mip level or a seek. `samples/textures/`
//! now carries four small files written by the reference C++ Ptex — see
//! `ptex_fixtures.md` there — and `samples/ptex_quads.usda` binds two of them
//! to geometry, so the same bytes back both this comparison and a rendered
//! one.
//!
//! The load-bearing claim is the first test's: **for any face and any
//! resolution both backends hold, a streamed lookup equals a preloaded one
//! bit for bit.** That is what makes streaming a residency change rather than
//! an appearance change, and it is the same invariant the `.tx` path states.
//! The second group measures where the two are *supposed* to differ — the mip
//! chain, reduced in linear light when preloaded and in the file's encoding
//! when streamed — rather than asserting the difference away.

use crust_assets::{PtexColor, PtexStream};
use crust_core::{PtexTexture, Vec3A};
use std::path::{Path, PathBuf};

/// Every fixture, with the authored `log2` resolution of its largest face.
///
/// The cap is passed to both backends explicitly, so a comparison is between
/// two textures asked for the same resolution rather than between whatever
/// the environment happened to say.
const FIXTURES: &[(&str, i8)] = &[
    // 1024x512, really tiled on disk, over stored mip levels. The one that
    // exercises the tile grid, and the reason for the whole exercise.
    ("quad_tiled", 10),
    // 16x16 down to 4x4, four channels with an alpha the decode ignores.
    ("quad_u8", 4),
    // Triangle mesh: symmetric reductions only, mirrored across the
    // anti-diagonal, and `uint16` samples that miss the decode table.
    ("tri_u16", 5),
    // `float32` samples, the other side of that table.
    ("quad_f32", 7),
];

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples/textures")
        .join(format!("{name}.ptx"))
}

/// A grid of sample points, deliberately including the borders and points a
/// half-texel outside every tile edge of the tiled fixture.
fn grid() -> Vec<(f32, f32)> {
    let mut out = Vec::new();
    // 37 is coprime with every power of two in these files, so the samples
    // land inside texels rather than repeatedly on their centres or edges —
    // a grid of 32 over a 32-texel face would only ever test one phase of the
    // bilinear weights.
    for i in 0..=37 {
        for j in 0..=37 {
            out.push((i as f32 / 37.0, j as f32 / 37.0));
        }
    }
    // Exactly on the borders and just inside them, where the clamp lives.
    for &u in &[0.0f32, 1.0, 0.5] {
        for &v in &[0.0f32, 1.0, 0.5] {
            out.push((u, v));
        }
    }
    out
}

fn bits(c: Vec3A) -> [u32; 3] {
    [c.x.to_bits(), c.y.to_bits(), c.z.to_bits()]
}

/// **The invariant.** At a resolution both backends hold, streaming changes
/// where the texels live and nothing else.
///
/// `width = 0.0` means point-sample the finest level held, so this compares
/// the two base levels: same `get_data_at_res`/`get_tile` bytes out of the
/// same file, same decode, same bilinear. Bit-for-bit, not within a
/// tolerance — the decode table is pinned against the scalar decode in the
/// unit tests precisely so this can be an equality.
#[test]
fn streamed_and_preloaded_agree_texel_for_texel() {
    for &(name, cap) in FIXTURES {
        let path = fixture(name);
        let pre = PtexColor::open_with(&path, true, cap).expect(name);
        let stream = PtexStream::open_with(&path, 8 << 20, Some(cap), true).expect(name);

        assert_eq!(
            PtexTexture::num_faces(&pre),
            PtexTexture::num_faces(&stream),
            "{name}: face count"
        );

        let mut compared = 0usize;
        for face in 0..PtexTexture::num_faces(&pre) as u32 {
            for &(u, v) in &grid() {
                let a = pre.eval(face, u, v, 0.0);
                let b = stream.eval(face, u, v, 0.0);
                assert_eq!(
                    bits(a),
                    bits(b),
                    "{name} face {face} at ({u}, {v}): preloaded {a:?} streamed {b:?}"
                );
                compared += 1;
            }
        }
        assert!(compared > 0, "{name}: nothing compared");
    }
}

/// The same, with a cap low enough that the reader has to *reduce* to reach
/// it rather than read a stored level.
///
/// A cap of 2 over a 1024x512 face is an eight-level reduction, and over the
/// non-square faces it is an anisotropic one — the case `resolve` routes to
/// `FaceSource::Reduced`, where a "tile" is the whole face and the streaming
/// path is reading exactly what the preloading path reads. Worth its own test
/// because it is the arm where `is_tiled` is false, and a single-path bug
/// there would hide behind the tiled fixture passing.
#[test]
fn a_capped_reduction_agrees_too() {
    for &(name, _) in FIXTURES {
        let path = fixture(name);
        for cap in [0i8, 1, 2, 3] {
            let pre = PtexColor::open_with(&path, true, cap).expect(name);
            let stream = PtexStream::open_with(&path, 8 << 20, Some(cap), true).expect(name);
            for face in 0..PtexTexture::num_faces(&pre) as u32 {
                for &(u, v) in &grid() {
                    let a = pre.eval(face, u, v, 0.0);
                    let b = stream.eval(face, u, v, 0.0);
                    assert_eq!(
                        bits(a),
                        bits(b),
                        "{name} cap {cap} face {face} at ({u}, {v})"
                    );
                }
            }
        }
    }
}

/// A tap that straddles a tile boundary must still read both tiles.
///
/// This is the one genuinely new failure mode the tile API introduces: the
/// preloading path has a single contiguous face and cannot get this wrong,
/// while `texel` has to resolve each of the four bilinear taps to its own
/// tile. A version that resolved the tile once for the whole quad — the
/// obvious simplification — passes every other test here, because the fixture
/// faces that are not tiled report one tile covering everything.
///
/// So this walks the tiled fixture's seams specifically and insists the
/// answers match the preloaded ones there too.
#[test]
fn bilinear_taps_cross_tile_boundaries() {
    let path = fixture("quad_tiled");
    let cap = 10;
    let pre = PtexColor::open_with(&path, true, cap).expect("preload");
    let stream = PtexStream::open_with(&path, 8 << 20, Some(cap), true).expect("stream");

    // Face 0 is 1024x512. Its tile edges are at multiples of the tile
    // resolution; the exact tiling is the file's business, so rather than
    // hard-coding it this sweeps every power-of-two boundary from 8 texels
    // up, which is a superset of wherever the seams actually are.
    let (w, h) = (1024.0f32, 512.0f32);
    let mut seams = 0usize;
    for step in [8usize, 16, 32, 64, 128, 256] {
        for k in (step..1024).step_by(step) {
            // A hair either side of the boundary, so the four taps span it.
            for d in [-0.4f32, -0.1, 0.1, 0.4] {
                let u = (k as f32 + d) / w;
                let v = (k as f32 % h + d) / h;
                let a = pre.eval(0, u, v, 0.0);
                let b = stream.eval(0, u, v, 0.0);
                assert_eq!(bits(a), bits(b), "seam at texel {k}{d:+} -> ({u}, {v})");
                seams += 1;
            }
        }
    }
    assert!(seams > 500, "expected a real sweep, got {seams} points");

    // And the file really is tiled, or the sweep above proved nothing. Two
    // taps far apart in one face must have gone to different tiles, which
    // shows up as more reader lookups than a single-tile face would need.
    let stats = stream.stats();
    assert!(
        stats.reader_lookups > 1,
        "the tiled fixture served every tap from one tile: {stats:?}"
    );
}

/// The microcache absorbs the repeated taps — at a tile's interior *and* on
/// a four-tile corner.
///
/// Not a performance assertion dressed as a test. A bilinear tap reads one
/// tile up to four times, so a microcache that never hit would mean the key
/// is wrong, and a wrong key is how one texture ends up answering for
/// another. What the rate measures is whether the key identifies a tile.
///
/// The corner half is why `MICRO_SLOTS` is 4 and not the two `tiled::cache`
/// keeps. A lookup sitting exactly where four tiles meet — both the u and the
/// v tap straddling a seam — needs four distinct tiles for its four taps, and
/// with two slots each tap evicts one the same lookup is about to ask for:
/// this case measured **0.000** of 1 600 fetches, a total thrash, against
/// 0.999 for a tap inside a tile. A `.ptx` grids *per face* rather than once
/// over the texture, and the faces that get tiled are the large ones a
/// streamed render lives in, so seams are routine rather than rare. Four
/// slots take the corner to 0.998 and cost the interior nothing.
///
/// Both halves are asserted, so shrinking the slot count back to two fails
/// here rather than quietly costing a render its cache.
#[test]
fn the_microcache_absorbs_most_taps() {
    let path = fixture("quad_tiled");

    // Inside a tile: a small magnified neighbourhood, so the four taps of
    // each lookup and the successive lookups all share one tile.
    let interior = PtexStream::open_with(&path, 8 << 20, Some(10), true).expect("stream");
    for i in 0..400 {
        let t = i as f32 / 400.0;
        interior.eval(0, 0.3 + 0.0005 * t, 0.7 + 0.0005 * t, 0.0);
    }
    let stats = interior.stats();
    assert!(
        stats.micro_rate() > 0.9,
        "microcache rate {:.3} inside one tile, over {} fetches: {stats:?}",
        stats.micro_rate(),
        stats.micro_hits + stats.reader_lookups
    );

    // On a four-tile corner. Face 0 is 1024x512, so (0.5, 0.5) puts the u
    // taps either side of texel 511/512 and the v taps either side of
    // 255/256 — four tiles for four taps, two slots.
    let corner = PtexStream::open_with(&path, 8 << 20, Some(10), true).expect("stream");
    for i in 0..400 {
        let t = i as f32 / 400.0;
        corner.eval(0, 0.5 + 0.000_001 * t, 0.5 + 0.000_001 * t, 0.0);
    }
    let corner_stats = corner.stats();
    assert!(
        corner_stats.micro_rate() > 0.9,
        "microcache rate {:.3} on a four-tile corner, over {} fetches — with \
         fewer slots than a corner has tiles this collapses to 0: \
         {corner_stats:?}",
        corner_stats.micro_rate(),
        corner_stats.micro_hits + corner_stats.reader_lookups
    );
    eprintln!(
        "microcache rate: {:.3} inside a tile, {:.3} on a four-tile corner",
        stats.micro_rate(),
        corner_stats.micro_rate()
    );
}

/// Two textures open at once must not answer for each other.
///
/// The microcache is a process-global thread-local keyed by
/// `(texture, face, resolution, tile)`, and the first three of those collide
/// constantly between two `.ptx` files in one scene — face 0 at 16x16 exists
/// in both fixtures below. The texture id is what keeps them apart, so this
/// interleaves lookups on two open textures and insists each still matches
/// its own preloaded oracle.
#[test]
fn two_open_textures_do_not_share_microcache_entries() {
    let a_path = fixture("quad_u8");
    let b_path = fixture("quad_tiled");
    let a_pre = PtexColor::open_with(&a_path, true, 4).expect("a");
    let b_pre = PtexColor::open_with(&b_path, true, 4).expect("b");
    let a = PtexStream::open_with(&a_path, 8 << 20, Some(4), true).expect("a");
    let b = PtexStream::open_with(&b_path, 8 << 20, Some(4), true).expect("b");

    for &(u, v) in &grid() {
        // Interleaved on purpose: alternating keeps both textures' entries
        // live in the two slots, which is exactly when a key that ignored the
        // texture would return the wrong one.
        assert_eq!(bits(a.eval(0, u, v, 0.0)), bits(a_pre.eval(0, u, v, 0.0)));
        assert_eq!(bits(b.eval(0, u, v, 0.0)), bits(b_pre.eval(0, u, v, 0.0)));
    }
}

/// `CRUST_PTEX_MIP=0`'s equivalent: with no pyramid, a footprint changes
/// nothing.
#[test]
fn without_mips_the_footprint_is_ignored() {
    let path = fixture("quad_tiled");
    let stream = PtexStream::open_with(&path, 8 << 20, Some(10), false).expect("stream");
    for &(u, v) in &grid() {
        let point = stream.eval(0, u, v, 0.0);
        for width in [0.01f32, 0.25, 1.0, 4.0] {
            assert_eq!(bits(stream.eval(0, u, v, width)), bits(point));
        }
    }
}

/// Streaming must be bounded by its budget, not by the asset.
///
/// The point of the exercise, stated as a number: the tiled fixture's face 0
/// is 1024x512 at one `u8` channel, so preloading it to `f32` RGB costs 6 MiB
/// of texels for that face alone, while streaming it under a 256 KiB budget
/// stays under the budget — with the tile-directory and block entries the
/// reader also keeps in the same cache, so this checks the real accounted
/// total rather than a pixel-only one.
#[test]
fn residency_is_bounded_by_the_budget() {
    let path = fixture("quad_tiled");
    let budget = 256 * 1024;
    let stream = PtexStream::open_with(&path, budget, None, true).expect("stream");

    // Walk the whole face at full resolution, which touches every tile.
    for i in 0..=256 {
        for j in 0..=256 {
            stream.eval(0, i as f32 / 256.0, j as f32 / 256.0, 0.0);
        }
    }
    let stats = stream.stats();
    assert!(
        stats.cache.bytes_resident <= budget,
        "resident {} over budget {budget}: {stats:?}",
        stats.cache.bytes_resident
    );
    assert!(
        stats.cache.evictions > 0,
        "nothing was evicted, so the budget was never reached: {stats:?}"
    );

    // And the uncapped stream really did read the authored resolution, which
    // is what a preloaded texture cannot afford to do. 1024x512 as `f32` RGB
    // is 6 MiB for one face; the cache held at most a quarter of a MiB.
    let pre = PtexColor::open_with(&path, true, 10).expect("preload");
    assert!(
        pre.bytes() > 6 * 1024 * 1024,
        "the preloaded oracle should be the expensive one: {} bytes",
        pre.bytes()
    );
}

/// Where the two backends are *supposed* to disagree, measured.
///
/// A preloaded pyramid is reduced in linear light; a streamed one comes off
/// disk reduced in the file's own display encoding and is decoded afterwards.
/// `x^2.2` is convex, so the streamed chain is the darker of the two at every
/// level above the base, and this test states that as a direction and a
/// magnitude rather than leaving it to be found in a render.
///
/// It is deliberately not an equality: pinning the numbers would make it a
/// change detector for the fixture. What is pinned is the *shape* — zero at
/// the base, one-directional above it, and bounded — which is what a reader
/// deciding whether to turn streaming on needs to know.
#[test]
fn the_mip_chains_differ_only_in_the_documented_direction() {
    let path = fixture("quad_tiled");
    let cap = 10;
    let pre = PtexColor::open_with(&path, true, cap).expect("preload");
    let stream = PtexStream::open_with(&path, 8 << 20, Some(cap), true).expect("stream");

    // Base level: identical, as the first test already says. Restated here so
    // the comparison below has a zero to be measured against.
    let mut base_diff = 0.0f32;
    for &(u, v) in &grid() {
        let d = (pre.eval(0, u, v, 0.0) - stream.eval(0, u, v, 0.0))
            .abs()
            .max_element();
        base_diff = base_diff.max(d);
    }
    assert_eq!(base_diff, 0.0, "the base levels must be identical");

    // Coarser levels: the streamed chain never exceeds the preloaded one by
    // more than float noise, and is visibly below it somewhere.
    let mut worst_over = 0.0f32;
    let mut worst_under = 0.0f32;
    for &width in &[0.02f32, 0.05, 0.1, 0.25, 0.5, 1.0] {
        for &(u, v) in &grid() {
            let p = pre.eval(0, u, v, width);
            let s = stream.eval(0, u, v, width);
            for ch in 0..3 {
                worst_over = worst_over.max(s[ch] - p[ch]);
                worst_under = worst_under.max(p[ch] - s[ch]);
            }
        }
    }
    // Convexity gives the direction. The small positive allowance is for the
    // levels where the file's own reduction and crust's happen to coincide
    // and only rounding separates them.
    assert!(
        worst_over < 1e-3,
        "streamed mips came out brighter by {worst_over}, which convexity forbids"
    );
    assert!(
        worst_under > 1e-3,
        "the two chains agreed everywhere ({worst_under}); either the fixture \
         has no encoded range left to differ over or the chains were confused"
    );
    eprintln!(
        "mip-chain divergence on quad_tiled: streamed darker by up to {worst_under:.4}, \
         brighter by at most {worst_over:.2e}"
    );
}

/// An out-of-range face id and a non-finite coordinate return the fallback
/// rather than panicking.
///
/// The trait's contract, and it is load-bearing: a texture is consulted from
/// inside the integrator, where a panic takes down a render thread. The
/// preloading path has its own version of this; the streaming path reaches
/// the same states through a reader that returns `Err`.
#[test]
fn a_bad_lookup_falls_back_instead_of_panicking() {
    let stream = PtexStream::open_with(&fixture("quad_u8"), 8 << 20, Some(4), true).expect("open");
    let n = PtexTexture::num_faces(&stream) as u32;
    for face in [n, n + 1, u32::MAX] {
        let c = stream.eval(face, 0.5, 0.5, 0.0);
        assert!(c.is_finite(), "face {face} gave {c:?}");
    }
    for (u, v) in [(f32::NAN, 0.5), (0.5, f32::NAN), (f32::INFINITY, -1.0)] {
        let c = stream.eval(0, u, v, 0.0);
        assert!(c.is_finite(), "({u}, {v}) gave {c:?}");
    }
    for width in [f32::NAN, f32::INFINITY, -1.0, 1e30] {
        let c = stream.eval(0, 0.5, 0.5, width);
        assert!(c.is_finite(), "width {width} gave {c:?}");
    }
}

/// The streaming backend is threaded, since that is the only way a renderer
/// will ever call it.
///
/// `SharedReader` is `&self` over a mutex; this is the test that the whole
/// sampler above it — the thread-local microcache included — is actually safe
/// and consistent under that. Each thread checks against the preloaded oracle,
/// so a race that returned a neighbouring tile's texels would fail rather than
/// merely being tolerated.
#[test]
fn concurrent_lookups_agree_with_the_oracle() {
    let path = fixture("quad_tiled");
    let cap = 10;
    let pre = std::sync::Arc::new(PtexColor::open_with(&path, true, cap).expect("preload"));
    let stream =
        std::sync::Arc::new(PtexStream::open_with(&path, 512 * 1024, Some(cap), true).expect("s"));

    std::thread::scope(|scope| {
        for t in 0..8 {
            let (pre, stream) = (pre.clone(), stream.clone());
            scope.spawn(move || {
                for i in 0..200 {
                    // Each thread walks a different diagonal, so the threads
                    // contend for tiles without all asking for the same one.
                    let u = ((i * 7 + t * 13) % 256) as f32 / 256.0;
                    let v = ((i * 11 + t * 5) % 256) as f32 / 256.0;
                    assert_eq!(
                        bits(stream.eval(0, u, v, 0.0)),
                        bits(pre.eval(0, u, v, 0.0)),
                        "thread {t} at ({u}, {v})"
                    );
                }
            });
        }
    });
}

/// **The budget belongs to the render, not to a file.**
///
/// `ptex::SharedReader` owns its cache, which is the right shape for a
/// library — a `.ptx` is a self-contained pyramid — but it means N textures
/// opened at `CRUST_PTEX_CACHE_MB` each would hold N times it. On a stage that
/// binds Ptex per element (the Moana island does, across its 20 elements) the
/// default 1 GiB would become tens of GiB, and the feature whose whole purpose
/// is to bound residency would be unbounded in the texture count.
///
/// `FileAssets` divides one budget over the streamed textures as they arrive.
/// This pins the property directly on the mechanism: open several, re-budget
/// the way `rebudget_ptex` does, and the *total* is what was asked for.
#[test]
fn one_budget_is_shared_across_textures_not_repeated_per_file() {
    let names = ["quad_tiled", "quad_u8", "tri_u16", "quad_f32"];
    let total = 64 * 1024 * 1024;

    let streams: Vec<_> = names
        .iter()
        .map(|n| PtexStream::open_with(&fixture(n), total, None, true).expect(n))
        .collect();

    // Opened at the full budget each, which is the bug: four textures, four
    // times the budget.
    let naive: usize = streams.iter().map(|s| s.stats().cache.bytes_budget).sum();
    assert_eq!(
        naive,
        total * names.len(),
        "a per-file budget should multiply — if this stopped being true the \
         sharing below is testing nothing"
    );

    // Shared, as `FileAssets` does it.
    let share = total / streams.len();
    for s in &streams {
        s.set_budget(share);
    }
    let shared: usize = streams.iter().map(|s| s.stats().cache.bytes_budget).sum();
    assert_eq!(
        shared, total,
        "the total must be the budget that was asked for"
    );

    // And the textures still work at their reduced budget.
    for (n, s) in names.iter().zip(&streams) {
        let pre = PtexColor::open_with(&fixture(n), true, 4).expect(n);
        let capped = PtexStream::open_with(&fixture(n), share, Some(4), true).expect(n);
        for &(u, v) in grid().iter().take(64) {
            assert_eq!(
                bits(capped.eval(0, u, v, 0.0)),
                bits(pre.eval(0, u, v, 0.0)),
                "{n} after re-budgeting"
            );
        }
        assert!(s.faces() > 0);
    }
}
