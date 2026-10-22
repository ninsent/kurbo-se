// Copyright 2026 the kurbo-se Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Broad phase of the intersection searches: which pairs of segments are
//! close enough to be worth intersecting.
//!
//! Splitting a subpath at its crossings starts from every pair of segments
//! whose bounding boxes come within the search accuracy of each other. So
//! does cutting the raw offsets of the region construction against each
//! other. Finding where a subpath revisits an anchor starts from every pair
//! of anchors that nearly coincide. Testing all pairs takes time quadratic
//! in the segment count, and on a detailed outline it outweighed the rest of
//! the pipeline. A sweep over the boxes finds the same pairs, and only looks
//! at boxes that overlap in `x`.

use alloc::vec::Vec;

use kurbo::Rect;

/// Whether two boxes come within `slack` of touching.
#[inline]
pub(crate) fn overlaps(a: Rect, b: Rect, slack: f64) -> bool {
    !(a.x0 > b.x1 + slack || b.x0 > a.x1 + slack || a.y0 > b.y1 + slack || b.y0 > a.y1 + slack)
}

/// Up to this many boxes, scanning all pairs is cheaper than the sweep.
pub(crate) const SCAN_MAX: usize = 16;

/// Call `f(i, j)`, `i < j`, once for every pair of the `n` boxes
/// `box_of(0..n)` that passes [`overlaps`]. These are exactly the pairs an
/// all-pairs scan finds, in no particular order. The boxes come from a
/// function so that a caller with few of them need not collect them.
///
/// The sweep sorts the boxes by left edge. For `a` before `b`,
/// `a.x0 <= b.x0 <= b.x1` already, so the pair meets in `x` exactly when
/// `b.x0 <= a.x1 + slack`. Later boxes start no further left, so once one
/// fails that test, all after it fail too. Scanning forward from each box
/// until then reaches every pair that meets in `x`, once, from the earlier
/// box. `overlaps` decides the rest.
///
/// Pairs go to `f` as the sweep finds them, so memory stays linear in the
/// number of boxes however many pairs there are. Where an offset collapses,
/// most of its segments can pile into one cluster in which nearly every
/// pair overlaps. Those pairs have to be searched anyway, but storing them
/// first cost more than the search.
///
/// A box with a `NaN` left edge breaks the ordering, while `overlaps`
/// counts every `NaN` comparison as "not separated". Such input falls back
/// to the all-pairs scan, which defines the result. So does an input of at
/// most [`SCAN_MAX`] boxes, where the scan is cheaper than sorting. A `NaN`
/// anywhere else, or in `slack`, at most keeps the sweep from stopping
/// early, and `overlaps` decides as before.
pub(crate) fn for_each_overlapping_pair(
    n: usize,
    box_of: impl Fn(usize) -> Rect,
    slack: f64,
    mut f: impl FnMut(usize, usize),
) {
    if n > SCAN_MAX {
        let boxes: Vec<Rect> = (0..n).map(&box_of).collect();
        if !boxes.iter().any(|b| b.x0.is_nan()) {
            sweep(&boxes, slack, f);
            return;
        }
    }
    for i in 0..n {
        let a = box_of(i);
        for j in (i + 1)..n {
            if overlaps(a, box_of(j), slack) {
                f(i, j);
            }
        }
    }
}

/// The sweep of [`for_each_overlapping_pair`].
fn sweep(boxes: &[Rect], slack: f64, mut f: impl FnMut(usize, usize)) {
    let mut order: Vec<usize> = (0..boxes.len()).collect();
    order.sort_unstable_by(|&a, &b| boxes[a].x0.total_cmp(&boxes[b].x0));
    let sorted: Vec<Rect> = order.iter().map(|&i| boxes[i]).collect();
    for (k, a) in sorted.iter().enumerate() {
        // `overlaps` compares against this same sum, so the exit agrees
        // with it to the last bit.
        let reach = a.x1 + slack;
        for (l, b) in sorted.iter().enumerate().skip(k + 1) {
            if b.x0 > reach {
                break;
            }
            if overlaps(*a, *b, slack) {
                let (i, j) = (order[k], order[l]);
                f(i.min(j), i.max(j));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny deterministic generator, enough to scatter boxes.
    fn xorshift(state: &mut u64) -> f64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        (*state >> 11) as f64 / (1u64 << 53) as f64
    }

    fn all_pairs(boxes: &[Rect], slack: f64) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        for i in 0..boxes.len() {
            for j in (i + 1)..boxes.len() {
                if overlaps(boxes[i], boxes[j], slack) {
                    out.push((i, j));
                }
            }
        }
        out
    }

    fn swept_pairs(boxes: &[Rect], slack: f64) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        for_each_overlapping_pair(boxes.len(), |i| boxes[i], slack, |i, j| out.push((i, j)));
        out.sort_unstable();
        out
    }

    /// The sweep finds exactly the pairs of the all-pairs scan, each once.
    /// Besides scattered boxes, it covers the cases where a sweep can slip:
    /// touching boxes, shared edges, equal left edges and boxes of zero
    /// size, at every slack from none to large.
    #[test]
    fn sweep_matches_all_pairs() {
        let mut state = 0x9E37_79B9_7F4A_7C15;
        for round in 0..200 {
            let n = 1 + round % 80;
            let boxes: Vec<Rect> = (0..n)
                .map(|k| {
                    // Snap to a coarse grid every other round so edges
                    // coincide exactly and ties in `x0` are common.
                    let snap = |v: f64| {
                        if round % 2 == 0 {
                            (v * 8.0).floor()
                        } else {
                            v * 8.0
                        }
                    };
                    let (x, y) = (snap(xorshift(&mut state)), snap(xorshift(&mut state)));
                    let (w, h) = if k % 7 == 0 {
                        (0.0, 0.0)
                    } else {
                        (
                            snap(xorshift(&mut state)) * 0.3,
                            snap(xorshift(&mut state)) * 0.3,
                        )
                    };
                    Rect::new(x, y, x + w, y + h)
                })
                .collect();
            for slack in [0.0, 0.25, 1.0, 5.0] {
                assert_eq!(
                    swept_pairs(&boxes, slack),
                    all_pairs(&boxes, slack),
                    "round {round}, slack {slack}"
                );
            }
        }
    }

    /// `NaN` coordinates and a `NaN` slack give the pairs `overlaps` gives.
    /// A `NaN` left edge sends the boxes to the all-pairs scan, and a `NaN`
    /// anywhere else only stops the sweep's early exit.
    #[test]
    fn nan_boxes_match_all_pairs() {
        let row: Vec<Rect> = (0..20)
            .map(|k| Rect::new(k as f64, 0.0, k as f64 + 1.5, 1.0))
            .collect();
        for nan in [
            Rect::new(f64::NAN, 0.0, 1.0, 1.0),
            Rect::new(-f64::NAN, 0.0, 1.0, 1.0),
            Rect::new(3.0, 0.0, f64::NAN, 1.0),
            Rect::new(3.0, f64::NAN, 4.0, 1.0),
        ] {
            let mut boxes = row.clone();
            boxes[3] = nan;
            assert_eq!(swept_pairs(&boxes, 0.0), all_pairs(&boxes, 0.0), "{nan:?}");
        }
        assert_eq!(swept_pairs(&row, f64::NAN), all_pairs(&row, f64::NAN));
    }

    /// The sweep's exit and `overlaps` round alike. Each box's left edge
    /// lands on the sum `x1 + slack` of the box before, or an ulp either
    /// side of it, where an exit that rounded differently could drop pairs.
    #[test]
    fn sweep_exit_agrees_with_overlaps_to_the_ulp() {
        let ulps = |x: f64, k: i64| f64::from_bits((x.to_bits() as i64 + k) as u64);
        for slack in [0.1, 1.0 / 3.0, 0.7, 2.5] {
            let mut boxes = Vec::new();
            let mut x1 = 0.3_f64;
            for k in 0..40i64 {
                let x0 = ulps(x1 + slack, k % 3 - 1);
                boxes.push(Rect::new(x0, 0.0, x0 + 0.05 * (k % 5) as f64, 1.0));
                x1 = x0 + 0.05 * (k % 5) as f64;
            }
            assert_eq!(
                swept_pairs(&boxes, slack),
                all_pairs(&boxes, slack),
                "slack {slack}"
            );
        }
    }
}
