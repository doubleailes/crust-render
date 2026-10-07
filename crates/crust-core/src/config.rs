//! Every `CRUST_*` environment switch, parsed once into one typed [`Config`],
//! and the one standard variable crust honours, `OCIO`.
//!
//! Each switch exists to A/B an optimization against the behaviour it
//! replaced (`docs/architecture.md` § Environment switches, whose table is
//! this struct's field list). They used to be read where they were used, and
//! parsed six different ways: some were cached, others re-read per prim or per
//! texture open, a bad budget warned once per call site, and a bad
//! `CRUST_TEX_MAX` not at all. Now there is one boolean grammar
//! ([`env_flag`]), one number grammar ([`env_parse`]), and one warning per
//! bad value per process.
//!
//! `OCIO` is not a switch: it is OpenColorIO's own variable naming the config
//! every OCIO application uses, read here so the environment is still read in
//! one place. Only the CLI obeys it, as the fallback for `--ocio-config`; the
//! library keeps its builtin config unless a host installs another, so a
//! test's colours never depend on the shell it runs in.
//!
//! The process-wide value is [`config()`], read from the environment on first
//! use. Code that wants a different setting — a test comparing both sides of
//! a switch, a probe — builds a [`Config`] and passes it
//! (`FileAssets::with_config`) rather than mutating the process environment,
//! which since edition 2024 is `unsafe` and so out of reach of
//! `forbid(unsafe_code)` anyway.

use std::num::{NonZeroU64, NonZeroUsize};
use std::str::FromStr;
use std::sync::LazyLock;

use tracing::warn;

/// `CRUST_TRI_PACKETS`: which triangle packet layout a kernel scene commits
/// with — see `crust_rt::PacketLayout`. Both answer every query
/// bit-identically; `gathered` is the layout before indexed packets existed
/// (the honest A/B side), `indexed` halves the packet bytes and gathers
/// vertices at every test (a quarter fewer kernel bytes per triangle for
/// 13–30% slower traversal), `auto` is the measured default, gathered.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum TriPackets {
    Gathered,
    Indexed,
    #[default]
    Auto,
}

impl FromStr for TriPackets {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, ()> {
        match s {
            "gathered" => Ok(TriPackets::Gathered),
            "indexed" => Ok(TriPackets::Indexed),
            "auto" => Ok(TriPackets::Auto),
            _ => Err(()),
        }
    }
}

impl std::fmt::Display for TriPackets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            TriPackets::Gathered => "gathered",
            TriPackets::Indexed => "indexed",
            TriPackets::Auto => "auto",
        })
    }
}

impl From<TriPackets> for crust_rt::PacketLayout {
    fn from(t: TriPackets) -> Self {
        match t {
            TriPackets::Gathered => crust_rt::PacketLayout::Gathered,
            TriPackets::Indexed => crust_rt::PacketLayout::Indexed,
            TriPackets::Auto => crust_rt::PacketLayout::Auto,
        }
    }
}

/// Which mip chain a streamed Ptex texture is allowed to read
/// (`CRUST_PTEX_STREAM_MIPSPACE`).
///
/// **The chain is where the two backends part company, and the project's own
/// standard for that is refusal rather than a footnote.** A `.tx` records
/// the colour space its levels were reduced in (`crust:mipspace`) and a
/// mismatch is refused outright, for the reason the mismatch is dangerous:
/// level 0 stays perfectly correct and every coarser level is wrong, so it
/// shows up only under minification and looks exactly like a filtering bug.
///
/// A `.ptx` has no such marker and needs none — the answer is known. Crust
/// binds Ptex colour as display-encoded and decodes it by 2.2, while a
/// `.ptx`'s stored levels were reduced in the file's own encoding. That is the
/// mismatch, always, so the default is [`PtexMipSpace::Linear`]: a texture
/// that would read a curve-decoded chain is declined and preloaded, where the
/// preloading backend builds the pyramid in linear light from the decoded
/// base.
///
/// [`PtexMipSpace::File`] is the opt-in that takes the file's chain instead.
/// It is what every production Ptex cache does and what the measured
/// residency figures in `docs/ptex_streaming.md` were taken with, so it is a
/// real mode and not a debug switch — but it is a render that trades a known
/// bias (darker minified texture, up to 0.147 on the tiled fixture) for the
/// memory, and that trade is the operator's to make rather than the default.
///
/// The cost of the default is worth stating plainly: with the mip pyramid on
/// — which it is unless `CRUST_PTEX_MIP=0` — every mipmapped `.ptx` preloads,
/// so `CRUST_PTEX_STREAM=1` alone buys nothing on a normal render.
/// `CRUST_PTEX_STREAM_MIPSPACE=file` is how the island's 5.98 -> 0.61 GiB
/// comes back.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PtexMipSpace {
    /// Refuse a chain reduced in the file's encoding; preload such a texture.
    #[default]
    Linear,
    /// Accept the file's own chain, bias and all.
    File,
}

impl FromStr for PtexMipSpace {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, ()> {
        match s {
            "linear" => Ok(PtexMipSpace::Linear),
            "file" => Ok(PtexMipSpace::File),
            _ => Err(()),
        }
    }
}

impl std::fmt::Display for PtexMipSpace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            PtexMipSpace::Linear => "linear",
            PtexMipSpace::File => "file",
        })
    }
}

/// The `CRUST_*` switches (and `OCIO`) in effect. [`Config::default`] is
/// every switch unset; [`config()`] is the process's environment.
///
/// Booleans name the optimization, so `true` is the new behaviour and `false`
/// the one it replaced — except `ptex_stream`, which is off by default.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// `CRUST_STREAM_IMPORT`: import one masked stage per subtree (`false`:
    /// one stage for the whole scene).
    pub stream_import: bool,
    /// `CRUST_MESH_BAKE`: bake single-placement meshes into world space
    /// (`false`: instance every mesh; bit-identical).
    pub mesh_bake: bool,
    /// `CRUST_SUBDIV`: refine subdivision cages (`false`: render every cage
    /// unrefined).
    pub subdiv: bool,
    /// `CRUST_DISPLACE`: displace meshes whose material defines a scalar
    /// displacement (`false`: import every mesh undisplaced, and a `none`
    /// mesh as its faceted cage, exactly as before displacement was read).
    pub displace: bool,
    /// `CRUST_ADAPTIVE_PER_FACE`: in adaptive subdivision, tessellate each
    /// mesh per face at its edges' own rates (`false`: refine each mesh to one
    /// level, the per-mesh adaptive behaviour this replaced).
    pub adaptive_per_face: bool,
    /// `CRUST_ADAPTIVE_FRUSTUM`: in adaptive subdivision, leave geometry wholly
    /// outside the render camera's view unrefined (`false`: refine by distance
    /// alone, in view or not).
    pub adaptive_frustum: bool,
    /// `CRUST_TRI_PACKETS`: the kernel's triangle packet layout
    /// (`gathered` | `indexed` | `auto`; bit-identical either way).
    pub tri_packets: TriPackets,
    /// `CRUST_MTLX_OPT`: fold, hoist and prune MaterialX programs (`false`:
    /// run them as compiled; bit-identical).
    pub mtlx_opt: bool,
    /// `CRUST_SHADER_JIT`: JIT-compile MaterialX programs (`false`: interpret
    /// them; bit-identical).
    pub shader_jit: bool,
    /// `CRUST_RAY_CONES`: texture footprints from ray cones (`false`: every
    /// footprint zero, the finest mip always).
    pub ray_cones: bool,
    /// `CRUST_TEX`: load UV textures (`false`: decline every one, so surfaces
    /// render on their constants).
    pub tex: bool,
    /// `CRUST_TEX_MAX`: the preloaded tile edge cap, in pixels, at least 1.
    pub tex_max: NonZeroUsize,
    /// `CRUST_TEX_MIP`: build mip pyramids on preloaded UV textures.
    pub tex_mip: bool,
    /// `CRUST_TEX_STREAM`: stream from a `.tx` when one exists (`false`:
    /// preload everything).
    pub tex_stream: bool,
    /// `CRUST_TEX_CACHE_MB`: the `.tx` tile cache budget, in MiB.
    pub tex_cache_mb: NonZeroU64,
    /// `CRUST_TEX_MAX_OPEN_FILES`: idle `.tx` readers the tile cache keeps
    /// open across all files. `0`: keep every reader, as before the cap.
    pub tex_max_open_files: usize,
    /// `CRUST_PTEX`: load Ptex textures (`false`: decline every one).
    pub ptex: bool,
    /// `CRUST_PTEX_MAX_LOG2`: the per-face resolution cap as a log2 edge in
    /// `0..=14`, `None` when unset — the preloading backend then caps at its
    /// default, the streaming one not at all.
    pub ptex_max_log2: Option<i8>,
    /// `CRUST_PTEX_MIP`: build per-face mip pyramids.
    pub ptex_mip: bool,
    /// `CRUST_PTEX_STREAM`: page Ptex tiles through the reader's cache. Off
    /// by default.
    pub ptex_stream: bool,
    /// `CRUST_PTEX_CACHE_MB`: the Ptex streaming budget, in MiB, shared by all
    /// streamed files.
    pub ptex_cache_mb: NonZeroUsize,
    /// `CRUST_PTEX_STREAM_MIN_MB`: files that would preload in less than this
    /// preload even when streaming. `0` admits everything.
    pub ptex_stream_min_mb: usize,
    /// `CRUST_PTEX_STREAM_MIPSPACE`: which mip chain a streamed `.ptx` may
    /// read.
    pub ptex_mip_space: PtexMipSpace,
    /// `OCIO`: the OpenColorIO config to use when the host names none — a
    /// path or an `ocio://` URI, `None` when unset or empty. Not validated
    /// here: loading it is the host's, which reports a bad one as an error.
    pub ocio: Option<String>,
}

/// Default `.tx` tile cache and Ptex streaming budgets, in MiB: OIIO's own
/// default, and what a path tracer — whose working set is the frame, not a
/// locality window — wants.
pub const DEFAULT_CACHE_MB: usize = 1024;

/// Default cap on the `.tx` tile cache's idle open files. It leaves the
/// default 1024-descriptor soft limit room for the threads' checked-out
/// readers, Ptex streaming, the USD stage and the outputs.
pub const DEFAULT_TEX_MAX_OPEN_FILES: usize = 256;

/// Default preloaded UV tile edge cap, in pixels.
pub const DEFAULT_TEX_MAX: usize = 1024;

/// Default Ptex streaming admission floor, in MiB (see
/// `crust_assets::PTEX_DEFAULT_STREAM_MIN_MB` for the measurement).
pub const DEFAULT_PTEX_STREAM_MIN_MB: usize = 8;

impl Default for Config {
    fn default() -> Self {
        Config {
            stream_import: true,
            mesh_bake: true,
            subdiv: true,
            displace: true,
            adaptive_per_face: true,
            adaptive_frustum: true,
            tri_packets: TriPackets::Auto,
            mtlx_opt: true,
            shader_jit: true,
            ray_cones: true,
            tex: true,
            tex_max: NonZeroUsize::new(DEFAULT_TEX_MAX).unwrap(),
            tex_mip: true,
            tex_stream: true,
            tex_cache_mb: NonZeroU64::new(DEFAULT_CACHE_MB as u64).unwrap(),
            tex_max_open_files: DEFAULT_TEX_MAX_OPEN_FILES,
            ptex: true,
            ptex_max_log2: None,
            ptex_mip: true,
            ptex_stream: false,
            ptex_cache_mb: NonZeroUsize::new(DEFAULT_CACHE_MB).unwrap(),
            ptex_stream_min_mb: DEFAULT_PTEX_STREAM_MIN_MB,
            ptex_mip_space: PtexMipSpace::Linear,
            ocio: None,
        }
    }
}

impl Config {
    /// The switches as the process environment sets them. Warns (once per
    /// call) for every value it cannot read, and uses the default for it.
    pub fn from_env() -> Config {
        Config::from_lookup(|name| present(std::env::var_os(name)))
    }

    /// The switches as `lookup` answers for each variable name — the
    /// environment in [`Config::from_env`], a table in a test.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Config {
        let d = Config::default();
        let flag = |name, default| env_flag(&lookup, name, default);
        Config {
            stream_import: flag("CRUST_STREAM_IMPORT", d.stream_import),
            mesh_bake: flag("CRUST_MESH_BAKE", d.mesh_bake),
            subdiv: flag("CRUST_SUBDIV", d.subdiv),
            displace: flag("CRUST_DISPLACE", d.displace),
            adaptive_per_face: flag("CRUST_ADAPTIVE_PER_FACE", d.adaptive_per_face),
            adaptive_frustum: flag("CRUST_ADAPTIVE_FRUSTUM", d.adaptive_frustum),
            tri_packets: env_parse(
                &lookup,
                "CRUST_TRI_PACKETS",
                d.tri_packets,
                "`gathered`, `indexed` or `auto`",
            ),
            mtlx_opt: flag("CRUST_MTLX_OPT", d.mtlx_opt),
            shader_jit: flag("CRUST_SHADER_JIT", d.shader_jit),
            ray_cones: flag("CRUST_RAY_CONES", d.ray_cones),
            tex: flag("CRUST_TEX", d.tex),
            tex_max: env_parse(&lookup, "CRUST_TEX_MAX", d.tex_max, "a positive integer"),
            tex_mip: flag("CRUST_TEX_MIP", d.tex_mip),
            tex_stream: flag("CRUST_TEX_STREAM", d.tex_stream),
            tex_cache_mb: env_parse(
                &lookup,
                "CRUST_TEX_CACHE_MB",
                d.tex_cache_mb,
                "a positive integer",
            ),
            tex_max_open_files: env_parse(
                &lookup,
                "CRUST_TEX_MAX_OPEN_FILES",
                d.tex_max_open_files,
                "an integer",
            ),
            ptex: flag("CRUST_PTEX", d.ptex),
            // Ptex resolutions are log2-encoded in an i8; 14 is 16384, well
            // past any authored face.
            ptex_max_log2: env_parse(
                &lookup,
                "CRUST_PTEX_MAX_LOG2",
                MaxLog2(d.ptex_max_log2),
                "an integer in 0..=14",
            )
            .0,
            ptex_mip: flag("CRUST_PTEX_MIP", d.ptex_mip),
            ptex_stream: flag("CRUST_PTEX_STREAM", d.ptex_stream),
            ptex_cache_mb: env_parse(
                &lookup,
                "CRUST_PTEX_CACHE_MB",
                d.ptex_cache_mb,
                "a positive integer",
            ),
            ptex_stream_min_mb: env_parse(
                &lookup,
                "CRUST_PTEX_STREAM_MIN_MB",
                d.ptex_stream_min_mb,
                "an integer",
            ),
            ptex_mip_space: env_parse(
                &lookup,
                "CRUST_PTEX_STREAM_MIPSPACE",
                d.ptex_mip_space,
                "`linear` or `file`",
            ),
            // OCIO's own convention: an empty `OCIO` is no config.
            ocio: lookup("OCIO").filter(|v| !v.is_empty()),
        }
    }
}

/// `CRUST_PTEX_MAX_LOG2`'s value: parsed only from an integer in `0..=14`.
#[derive(Clone, Copy)]
struct MaxLog2(Option<i8>);

impl FromStr for MaxLog2 {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, ()> {
        match s.parse::<i8>() {
            Ok(n) if (0..=14).contains(&n) => Ok(MaxLog2(Some(n))),
            _ => Err(()),
        }
    }
}

impl std::fmt::Display for MaxLog2 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            Some(n) => write!(f, "{n}"),
            None => f.write_str("the default"),
        }
    }
}

/// The one boolean grammar: `0`, `false`, `off`, `no` are off; `1`, `true`,
/// `on`, `yes` are on; unset is `default`, and anything else warns and is
/// `default` too.
pub fn env_flag(lookup: impl Fn(&str) -> Option<String>, name: &str, default: bool) -> bool {
    let Some(v) = lookup(name) else {
        return default;
    };
    match v.as_str() {
        "0" | "false" | "off" | "no" => false,
        "1" | "true" | "on" | "yes" => true,
        _ => {
            warn!(
                "{name}={v} is not a boolean (0/1, false/true, off/on, no/yes) — using {}",
                if default { 1 } else { 0 }
            );
            default
        }
    }
}

/// The one number (or keyword) grammar: `T::from_str`, where the type's own
/// parser is the validation — a `NonZero*` refuses 0. Unset is `default`; an
/// unreadable value warns, naming what was `expected`, and is `default` too.
pub fn env_parse<T: FromStr + std::fmt::Display>(
    lookup: impl Fn(&str) -> Option<String>,
    name: &str,
    default: T,
    expected: &str,
) -> T {
    let Some(v) = lookup(name) else {
        return default;
    };
    match v.parse::<T>() {
        Ok(t) => t,
        Err(_) => {
            warn!("{name}={v} is not {expected} — using {default}");
            default
        }
    }
}

/// A variable as the parsers see it: `None` only when it is unset. A value
/// that is not Unicode is still *set* — `std::env::var(..).ok()` would
/// collapse it into unset and skip the warning — so it is converted lossily:
/// the U+FFFD that replaces its invalid bytes is in no accepted value, which
/// sends it down the parser's warn-and-default path like any other bad value.
fn present(value: Option<std::ffi::OsString>) -> Option<String> {
    value.map(|v| v.to_string_lossy().into_owned())
}

static CONFIG: LazyLock<Config> = LazyLock::new(Config::from_env);

/// The process's switches, read from the environment on first use and fixed
/// from then on.
pub fn config() -> &'static Config {
    &CONFIG
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A set but non-Unicode value is a bad value, not an unset one: it
    /// reaches the parsers (which warn) and falls back to the default.
    #[cfg(unix)]
    #[test]
    fn a_non_unicode_value_is_set_and_refused() {
        use std::os::unix::ffi::OsStringExt;
        let raw = std::ffi::OsString::from_vec(vec![b'1', 0xff]);
        let seen = present(Some(raw)).expect("a set variable is present");
        assert!(seen.contains('\u{FFFD}'));
        assert_eq!(present(None), None);
        let c = Config::from_lookup(|name| (name == "CRUST_MESH_BAKE").then(|| seen.clone()));
        assert_eq!(c.mesh_bake, Config::default().mesh_bake);
        let c = Config::from_lookup(|name| (name == "CRUST_TEX_MAX").then(|| seen.clone()));
        assert_eq!(c.tex_max, Config::default().tex_max);
    }

    fn with(vars: &[(&str, &str)]) -> Config {
        let vars: Vec<(String, String)> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Config::from_lookup(|name| vars.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()))
    }

    #[test]
    fn nothing_set_is_the_default() {
        assert_eq!(with(&[]), Config::default());
    }

    #[test]
    fn booleans_share_one_grammar() {
        for off in ["0", "false", "off", "no"] {
            assert!(!with(&[("CRUST_SUBDIV", off)]).subdiv, "{off}");
        }
        for on in ["1", "true", "on", "yes"] {
            assert!(with(&[("CRUST_PTEX_STREAM", on)]).ptex_stream, "{on}");
        }
        // Unreadable is the default, whichever way the default points.
        assert!(with(&[("CRUST_SUBDIV", "maybe")]).subdiv);
        assert!(
            with(&[]).adaptive_per_face,
            "per-face is the adaptive default"
        );
        assert!(!with(&[("CRUST_ADAPTIVE_PER_FACE", "0")]).adaptive_per_face);
        assert!(with(&[]).displace, "displacement is on by default");
        assert!(!with(&[("CRUST_DISPLACE", "0")]).displace);
        assert!(with(&[]).adaptive_frustum);
        assert!(!with(&[("CRUST_ADAPTIVE_FRUSTUM", "0")]).adaptive_frustum);
        assert!(!with(&[("CRUST_PTEX_STREAM", "")]).ptex_stream);
    }

    #[test]
    fn numbers_are_validated_by_their_type() {
        let c = with(&[
            ("CRUST_TEX_MAX", "256"),
            ("CRUST_TEX_CACHE_MB", "0"),
            ("CRUST_PTEX_CACHE_MB", "64"),
            ("CRUST_PTEX_STREAM_MIN_MB", "0"),
            ("CRUST_PTEX_MAX_LOG2", "15"),
        ]);
        assert_eq!(c.tex_max.get(), 256);
        // A zero budget is refused; a zero admission floor is meaningful.
        assert_eq!(c.tex_cache_mb.get(), DEFAULT_CACHE_MB as u64);
        assert_eq!(c.ptex_cache_mb.get(), 64);
        assert_eq!(c.ptex_stream_min_mb, 0);
        assert_eq!(c.ptex_max_log2, None);
        assert_eq!(with(&[("CRUST_PTEX_MAX_LOG2", "3")]).ptex_max_log2, Some(3));
        assert_eq!(
            with(&[("CRUST_TEX_MAX", "big")]).tex_max.get(),
            DEFAULT_TEX_MAX
        );
    }

    #[test]
    fn the_open_file_cap_admits_zero_as_unbounded() {
        assert_eq!(with(&[]).tex_max_open_files, DEFAULT_TEX_MAX_OPEN_FILES);
        let c = with(&[("CRUST_TEX_MAX_OPEN_FILES", "0")]);
        assert_eq!(c.tex_max_open_files, 0);
        let c = with(&[("CRUST_TEX_MAX_OPEN_FILES", "64")]);
        assert_eq!(c.tex_max_open_files, 64);
        for bad in ["lots", "-1"] {
            let c = with(&[("CRUST_TEX_MAX_OPEN_FILES", bad)]);
            assert_eq!(c.tex_max_open_files, DEFAULT_TEX_MAX_OPEN_FILES, "{bad}");
        }
    }

    #[test]
    fn mip_space_parses_its_two_names() {
        let file = with(&[("CRUST_PTEX_STREAM_MIPSPACE", "file")]);
        assert_eq!(file.ptex_mip_space, PtexMipSpace::File);
        for (spelling, want) in [
            ("gathered", TriPackets::Gathered),
            ("indexed", TriPackets::Indexed),
            ("auto", TriPackets::Auto),
        ] {
            let c =
                Config::from_lookup(|n| (n == "CRUST_TRI_PACKETS").then(|| spelling.to_string()));
            assert_eq!(c.tri_packets, want);
        }
        let bad = Config::from_lookup(|n| (n == "CRUST_TRI_PACKETS").then(|| "fast".to_string()));
        assert_eq!(bad.tri_packets, TriPackets::Auto);
        let bad = with(&[("CRUST_PTEX_STREAM_MIPSPACE", "srgb")]);
        assert_eq!(bad.ptex_mip_space, PtexMipSpace::Linear);
        for m in [PtexMipSpace::Linear, PtexMipSpace::File] {
            assert_eq!(m.to_string().parse::<PtexMipSpace>(), Ok(m));
        }
    }

    #[test]
    fn ocio_names_a_config_unless_empty() {
        assert_eq!(with(&[]).ocio, None);
        assert_eq!(with(&[("OCIO", "")]).ocio, None);
        let c = with(&[("OCIO", "/studio/config.ocio")]);
        assert_eq!(c.ocio.as_deref(), Some("/studio/config.ocio"));
    }
}
