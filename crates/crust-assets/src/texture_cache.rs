//! What the two streaming texture caches — the `.tx` [`TileCache`] and the
//! Ptex [`PtexStream`] — share: their budgets' unit, and the per-thread
//! microcache set that keeps a texel fetch off every lock.
//!
//! [`TileCache`]: crate::tiled::TileCache
//! [`PtexStream`]: crate::PtexStream

/// Bytes in `mib` mebibytes — the unit every `CRUST_*_MB` budget is given in.
pub(crate) const fn mib_to_bytes(mib: u64) -> u64 {
    mib * 1024 * 1024
}

/// `bytes` in mebibytes, for a log line.
pub(crate) fn bytes_to_mib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

/// One set of a per-thread microcache: up to `N` `(key, value)` pairs, newest
/// first.
///
/// FIFO rather than LRU: promoting on a hit would make every hit a write, and
/// a hit is the common case by far (a bilinear tap reads one tile up to four
/// times in a row). A lookup scans `N` keys, and finds its hit at index 0 the
/// vast majority of the time.
pub(crate) struct Ways<K, V, const N: usize>([Option<(K, V)>; N]);

impl<K: PartialEq, V, const N: usize> Ways<K, V, N> {
    pub(crate) const EMPTY: Self = Ways([const { None }; N]);

    /// The value held for `key`, if any.
    ///
    /// Forced inline: at eight ways LLVM left it out of line, a call per texel
    /// fetch that was 40.8 M instructions of a 2.04 G alias-scene render.
    #[inline(always)]
    pub(crate) fn get(&self, key: &K) -> Option<&V> {
        let idx = self
            .0
            .iter()
            .position(|s| matches!(s, Some((k, _)) if k == key))?;
        self.0[idx].as_ref().map(|(_, v)| v)
    }

    /// Puts `(key, value)` in front, returning the oldest entry when it falls
    /// off the end — so a caller accounting for what the set holds can
    /// subtract it.
    #[inline]
    pub(crate) fn push(&mut self, key: K, value: V) -> Option<(K, V)> {
        let evicted = self.0[N - 1].take();
        self.0.rotate_right(1);
        self.0[0] = Some((key, value));
        evicted
    }

    /// The value the next [`Ways::push`] would hand back, if the set is full.
    #[inline]
    pub(crate) fn oldest_if_full(&self) -> Option<&V> {
        self.0[N - 1].as_ref().map(|(_, v)| v)
    }

    /// The values held, newest first.
    pub(crate) fn values(&self) -> impl Iterator<Item = &V> {
        self.0.iter().flatten().map(|(_, v)| v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ways_keep_the_newest_n_and_hand_back_the_evicted() {
        let mut w: Ways<u32, &str, 2> = Ways::EMPTY;
        assert!(w.get(&1).is_none());
        assert!(w.push(1, "a").is_none());
        assert!(w.push(2, "b").is_none());
        assert_eq!(w.get(&1), Some(&"a"));
        assert_eq!(w.push(3, "c"), Some((1, "a")));
        assert!(w.get(&1).is_none());
        assert_eq!(w.values().copied().collect::<Vec<_>>(), ["c", "b"]);
    }

    #[test]
    fn budgets_are_mebibytes() {
        assert_eq!(mib_to_bytes(1), 1 << 20);
        assert_eq!(bytes_to_mib(3 << 20), 3.0);
    }
}
