//! The byte counts behind the `*_MB` cache settings.
//!
//! Every budget is stated in MiB in [`crust_core::Config`], whose defaults
//! (`crust_core::config::DEFAULT_CACHE_MB` and
//! `DEFAULT_PTEX_STREAM_MIN_MB`) are the one place a default is written, and
//! every one is converted to bytes here, through [`mib`]. The `.tx` and Ptex
//! sides used to carry a helper and a default constant each, in different
//! integer types, each re-doing the multiplication.

use crust_core::Config;

/// Bytes in `mb` mebibytes.
const fn mib(mb: u64) -> u64 {
    mb * 1024 * 1024
}

/// The `.tx` tile cache budget `config` asks for (`CRUST_TEX_CACHE_MB`).
pub(crate) fn tex_cache_bytes(config: &Config) -> u64 {
    mib(config.tex_cache_mb.get())
}

/// The Ptex cache budget `config` asks for (`CRUST_PTEX_CACHE_MB`), shared
/// by every streamed `.ptx`.
///
/// Its default is the `.tx` cache's 1024, and so OIIO's own. Upstream's
/// [`ptex::DEFAULT_CACHE_BUDGET`] is 64 MiB, which is a library's answer for a
/// caller that has not thought about it; a renderer has, and a path tracer
/// asks for texels from every worker in an order nothing can predict, so the
/// working set is the frame rather than a locality window.
pub(crate) fn ptex_cache_bytes(config: &Config) -> usize {
    mib(config.ptex_cache_mb.get() as u64) as usize
}

/// The preload size below which a `.ptx` is not streamed
/// (`CRUST_PTEX_STREAM_MIN_MB`). See `ptex_stream::DEFAULT_STREAM_MIN_MB`.
pub(crate) fn ptex_stream_min_bytes(config: &Config) -> usize {
    mib(config.ptex_stream_min_mb as u64) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The defaults are a GiB of cache on each side and an 8 MiB streaming
    /// floor, stated once in crust-core and in MiB.
    #[test]
    fn default_budgets_in_bytes() {
        let c = Config::default();
        assert_eq!(tex_cache_bytes(&c), 1 << 30);
        assert_eq!(ptex_cache_bytes(&c), 1 << 30);
        assert_eq!(ptex_stream_min_bytes(&c), 8 << 20);
    }
}
