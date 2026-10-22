// Copyright 2026 the kurbo-se Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Horizontal bands: items listed by the heights they span, so that a query
//! at one height, or over a range of heights, reaches only the items that
//! can matter there.
//!
//! [`Bands`] divides a range of heights into equal bands, and [`Buckets`]
//! lists item ids per band. The flattened source index, the corners of its
//! joins, the index of raw offset loops and the fill probe's segments are
//! all listed this way. An item is listed in every band its span meets. The
//! band of a height never decreases as the height grows, so an item whose
//! span holds height `y` is always listed in the band of `y`.

use alloc::vec::Vec;
use core::ops::RangeInclusive;

/// The division of a range of heights into equal bands.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Bands {
    y0: f64,
    inv_h: f64,
    n: usize,
}

impl Default for Bands {
    /// One band. A default [`Buckets`] reads it as empty until the owner is
    /// rebuilt over real spans.
    fn default() -> Bands {
        Bands {
            y0: 0.0,
            inv_h: 0.0,
            n: 1,
        }
    }
}

impl Bands {
    /// `n` bands, at least one, over the heights the spans cover.
    fn covering(spans: &[(f64, f64)], n: usize) -> Bands {
        let y0 = spans.iter().map(|s| s.0).fold(f64::INFINITY, f64::min);
        let y1 = spans.iter().map(|s| s.1).fold(f64::NEG_INFINITY, f64::max);
        let n = n.max(1);
        if spans.is_empty() {
            return Bands {
                y0: 0.0,
                inv_h: 0.0,
                n,
            };
        }
        Bands {
            y0,
            inv_h: n as f64 / (y1 - y0).max(1e-12),
            n,
        }
    }

    /// About four items to a band, and at most 256 bands, over the heights
    /// the spans cover. That is fine enough for a query at one height to
    /// meet few items.
    pub(crate) fn fine(spans: &[(f64, f64)]) -> Bands {
        Bands::covering(spans, (spans.len() / 4).clamp(1, 256))
    }

    pub(crate) fn len(&self) -> usize {
        self.n
    }

    /// The band holding height `y`, clamped to the bands there are.
    pub(crate) fn of(&self, y: f64) -> usize {
        crate::math::floor((y - self.y0) * self.inv_h).clamp(0.0, (self.n - 1) as f64) as usize
    }

    /// The bands a span of heights meets.
    pub(crate) fn spanned(&self, (lo, hi): (f64, f64)) -> RangeInclusive<usize> {
        self.of(lo)..=self.of(hi)
    }

    /// The bands a distance query over the heights `lo..=hi` searches. They
    /// are the bands [`Bands::spanned`] gives, plus the next one, unless
    /// `hi` falls exactly on a band boundary or no band follows.
    ///
    /// A distance test rounds at the scale of the edge it tests, so it can
    /// accept an edge whose span lies a rounding error outside `lo..=hi`.
    /// Whether a query finds such an edge depends on the bands it searches.
    /// These are the bands the scan searched before this index replaced it,
    /// so every answer stays the same.
    pub(crate) fn searched(&self, (lo, hi): (f64, f64)) -> RangeInclusive<usize> {
        let top = crate::math::ceil((hi - self.y0) * self.inv_h).clamp(0.0, (self.n - 1) as f64);
        self.of(lo)..=top as usize
    }
}

/// Item ids listed per band, with all bands in one allocation. Band `k` is
/// `entries[starts[k]..starts[k + 1]]`, in ascending id order until a
/// caller reorders it. A default `Buckets` lists nothing, so every band
/// reads as empty until it is built.
#[derive(Default, Debug)]
pub(crate) struct Buckets {
    starts: Vec<usize>,
    entries: Vec<u32>,
}

impl Buckets {
    /// List each item `i` in every one of `bands` that its span `spans[i]`
    /// meets.
    pub(crate) fn new(bands: &Bands, spans: &[(f64, f64)]) -> Buckets {
        let mut buckets = Buckets::default();
        buckets.rebuild(bands, spans);
        buckets
    }

    /// [`Buckets::new`] in place, keeping the storage.
    pub(crate) fn rebuild(&mut self, bands: &Bands, spans: &[(f64, f64)]) {
        self.rebuild_by_bands(bands.len(), spans.len(), |i| bands.spanned(spans[i]));
    }

    /// List each item `i` in `0..items` in the bands `bands_of(i)`, out of
    /// `n` bands, keeping the storage.
    pub(crate) fn rebuild_by_bands(
        &mut self,
        n: usize,
        items: usize,
        bands_of: impl Fn(usize) -> RangeInclusive<usize>,
    ) {
        let starts = &mut self.starts;
        starts.clear();
        starts.resize(n + 1, 0);
        for i in 0..items {
            for k in bands_of(i) {
                starts[k + 1] += 1;
            }
        }
        for k in 0..n {
            starts[k + 1] += starts[k];
        }
        self.entries.clear();
        self.entries.resize(starts[n], 0);
        // Each band's start serves as its fill cursor and ends up at the
        // band's end, which is where the next band starts. Shifting the
        // cursors up by one place restores the starts.
        for i in 0..items {
            for k in bands_of(i) {
                self.entries[starts[k]] = i as u32;
                starts[k] += 1;
            }
        }
        starts.copy_within(0..n, 1);
        starts[0] = 0;
    }

    /// The number of bands.
    pub(crate) fn len(&self) -> usize {
        self.starts.len().saturating_sub(1)
    }

    /// The ids listed in band `k`.
    pub(crate) fn get(&self, k: usize) -> &[u32] {
        match (self.starts.get(k), self.starts.get(k + 1)) {
            (Some(&from), Some(&to)) => &self.entries[from..to],
            _ => &[],
        }
    }

    /// The ids listed in band `k`, to reorder.
    pub(crate) fn get_mut(&mut self, k: usize) -> &mut [u32] {
        &mut self.entries[self.starts[k]..self.starts[k + 1]]
    }

    /// The position in [`Buckets::entries`] where band `k` begins, for
    /// data kept parallel to the entries.
    pub(crate) fn offset(&self, k: usize) -> usize {
        self.starts.get(k).copied().unwrap_or(0)
    }

    /// The number of entries over all bands.
    pub(crate) fn total(&self) -> usize {
        self.entries.len()
    }
}
