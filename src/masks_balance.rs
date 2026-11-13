// SPDX-License-Identifier: Apache-2.0
//! Per-window mask balance on multi-mask (multi-patterned) metal layers: a pre-screen checker. It
//! reports; it never moves, adds, removes or recolours a shape.
//!
//! **No reference behaviour exists to match.** OpenROAD has no mask-balance rule: its density fill
//! colours fill round-robin and nothing in odb, drt or fill measures the area of one mask against
//! the others. The basis is the rule as a multi-patterning sign-off deck states it: per layer with
//! more than one mask, over windows of side W stepped by S, the mask-1 share
//!
//! ```text
//! share = area(mask 1) / (area(mask 1) + ... + area(mask n) + area(uncoloured))
//! ```
//!
//! must lie in an allowed range `[lo, hi]`. W, S and the range are parameters
//! ([`BalanceOptions`]); this module carries no default for any of them.
//!
//! What is counted (every interpretation below is pinned in a unit test):
//! * **Metal of the layer, every owner**: routed-wire boxes (via landings included), special-wire
//!   boxes (power grid, via enclosures included), block-pin boxes, the shapes of every placed
//!   instance (pins and obstructions), and fill (`dbFill`) of every mask. Instance shapes carry no
//!   mask: odb's LEF reader (pin 4155ac31) keeps no `MASK` on macro geometry, so cell metal is
//!   uncoloured here. An instance obstruction is counted as metal (the conservative reading of an
//!   abstract).
//! * **Categories**: one per mask number (design and fill of that mask together), plus
//!   `uncoloured` (design without a mask and fill with mask 0). Each category's area is the area of
//!   the UNION of its shapes inside the window, so overlapping shapes of one category count once.
//!   The total is the SUM of the category areas: metal covered by two categories (an uncoloured
//!   cell pin under a coloured wire) counts once in each.
//! * **Windows** tile the die area: origins at `die.lo + k * S` per axis, for `k = 0, 1, ...` up to
//!   and including the first window that reaches the far die edge; each window is CLIPPED to the
//!   die, so the last column and row may be narrower than W, and a die smaller than W is one
//!   window. Metal outside the die is not counted.
//! * **Range**: CLOSED. A window passes when `lo <= share <= hi`; a share exactly at a bound passes.
//! * A window without metal on the layer has no share: it is counted `empty`, never checked and
//!   never a finding.
//! * Only routing layers the tech marks with more than one mask are checked.

#![deny(clippy::unwrap_used)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

use std::collections::BTreeMap;

use crate::masks_geom::Rect;
use crate::{Db, Result};

/// Window, step and allowed mask-1 share range. Window and step in DBU.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BalanceOptions {
    pub window: i32,
    pub step: i32,
    pub lo: f64,
    pub hi: f64,
}

impl BalanceOptions {
    /// Validated options: a positive window, a positive step no larger than the window (a larger
    /// step leaves metal between windows unchecked), and `0 <= lo <= hi <= 1`.
    pub fn new(window: i32, step: i32, lo: f64, hi: f64) -> std::result::Result<Self, String> {
        if window <= 0 || step <= 0 {
            return Err(format!("window ({window}) and step ({step}) must be positive"));
        }
        if step > window {
            return Err(format!("step ({step}) larger than the window ({window}) leaves gaps"));
        }
        if !(0.0..=1.0).contains(&lo) || !(0.0..=1.0).contains(&hi) || lo > hi {
            return Err(format!("range [{lo}, {hi}] must satisfy 0 <= lo <= hi <= 1"));
        }
        Ok(BalanceOptions { window, step, lo, hi })
    }
}

/// One shape on a balance layer. `mask` 0: uncoloured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BalanceShape {
    pub rect: Rect,
    pub mask: u8,
    pub fill: bool,
}

/// The window origins along one axis of `[lo, hi)`: `lo + k * step` up to and including the first
/// window that reaches `hi`. Empty when the axis is empty.
#[must_use]
pub fn window_origins(lo: i32, hi: i32, window: i32, step: i32) -> Vec<i32> {
    let mut out = Vec::new();
    if hi <= lo || window <= 0 || step <= 0 {
        return out;
    }
    let mut o = i64::from(lo);
    loop {
        out.push(o as i32);
        if o + i64::from(window) >= i64::from(hi) {
            return out;
        }
        o += i64::from(step);
    }
}

/// The windows over `die`, row by row from the lower left, each clipped to the die.
#[must_use]
pub fn windows(die: &Rect, window: i32, step: i32) -> Vec<Rect> {
    let xs = window_origins(die[0], die[2], window, step);
    let ys = window_origins(die[1], die[3], window, step);
    let mut out = Vec::with_capacity(xs.len() * ys.len());
    for &y in &ys {
        for &x in &xs {
            let x1 = (i64::from(x) + i64::from(window)).min(i64::from(die[2])) as i32;
            let y1 = (i64::from(y) + i64::from(window)).min(i64::from(die[3])) as i32;
            out.push([x, y, x1, y1]);
        }
    }
    out
}

/// `r` clipped to `clip`; `None` when nothing with area is left.
fn clip(r: &Rect, clip: &Rect) -> Option<Rect> {
    let c = [r[0].max(clip[0]), r[1].max(clip[1]), r[2].min(clip[2]), r[3].min(clip[3])];
    (c[0] < c[2] && c[1] < c[3]).then_some(c)
}

/// The area of the union of `rects` (sweep over x, covered length over compressed y).
#[must_use]
pub fn union_area(rects: &[Rect]) -> i64 {
    let rects: Vec<&Rect> = rects.iter().filter(|r| r[0] < r[2] && r[1] < r[3]).collect();
    if rects.is_empty() {
        return 0;
    }
    let mut ys: Vec<i32> = rects.iter().flat_map(|r| [r[1], r[3]]).collect();
    ys.sort_unstable();
    ys.dedup();
    // (x, +1 open / -1 close, y index range)
    let mut events: Vec<(i32, i32, usize, usize)> = Vec::with_capacity(rects.len() * 2);
    for r in &rects {
        let a = ys.partition_point(|&y| y < r[1]);
        let b = ys.partition_point(|&y| y < r[3]);
        events.push((r[0], 1, a, b));
        events.push((r[2], -1, a, b));
    }
    events.sort_unstable();
    let mut tree = CoverTree::new(&ys);
    let mut area = 0i64;
    let mut last_x = events[0].0;
    for (x, d, a, b) in events {
        area += i64::from(x - last_x) * tree.covered();
        last_x = x;
        tree.add(a, b, d);
    }
    area
}

/// Segment tree over the elementary y intervals `[ys[i], ys[i+1])`: cover counts and the covered
/// length.
struct CoverTree<'y> {
    ys: &'y [i32],
    count: Vec<i32>,
    len: Vec<i64>,
}

impl<'y> CoverTree<'y> {
    fn new(ys: &'y [i32]) -> Self {
        let n = ys.len().saturating_sub(1).max(1);
        CoverTree { ys, count: vec![0; 4 * n], len: vec![0; 4 * n] }
    }
    fn covered(&self) -> i64 {
        self.len[1]
    }
    /// Add `d` to the cover of intervals `a..b`.
    fn add(&mut self, a: usize, b: usize, d: i32) {
        let n = self.ys.len().saturating_sub(1);
        if a < b && n > 0 {
            self.update(1, 0, n, a, b, d);
        }
    }
    fn update(&mut self, node: usize, lo: usize, hi: usize, a: usize, b: usize, d: i32) {
        if b <= lo || hi <= a {
            return;
        }
        if a <= lo && hi <= b {
            self.count[node] += d;
        } else {
            let mid = (lo + hi) / 2;
            self.update(2 * node, lo, mid, a, b, d);
            self.update(2 * node + 1, mid, hi, a, b, d);
        }
        self.len[node] = if self.count[node] > 0 {
            i64::from(self.ys[hi] - self.ys[lo])
        } else if hi - lo == 1 {
            0
        } else {
            self.len[2 * node] + self.len[2 * node + 1]
        };
    }
}

/// The metal of one window by category: mask number to area of the union (0: uncoloured).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WindowAreas {
    pub by_mask: BTreeMap<u8, i64>,
}

impl WindowAreas {
    /// Sum of the category areas.
    #[must_use]
    pub fn total(&self) -> i64 {
        self.by_mask.values().sum()
    }
    #[must_use]
    pub fn mask1(&self) -> i64 {
        self.by_mask.get(&1).copied().unwrap_or(0)
    }
    /// Mask-1 area over the total; `None` without metal.
    #[must_use]
    pub fn share(&self) -> Option<f64> {
        let t = self.total();
        (t > 0).then(|| self.mask1() as f64 / t as f64)
    }
}

/// Category areas of the shapes inside `window` (each category's union, clipped).
#[must_use]
pub fn window_areas<'s>(shapes: impl IntoIterator<Item = &'s BalanceShape>, window: &Rect) -> WindowAreas {
    let mut cat: BTreeMap<u8, Vec<Rect>> = BTreeMap::new();
    for s in shapes {
        if let Some(c) = clip(&s.rect, window) {
            cat.entry(s.mask).or_default().push(c);
        }
    }
    WindowAreas { by_mask: cat.into_iter().map(|(m, v)| (m, union_area(&v))).collect() }
}

/// Inside the closed range `[lo, hi]`.
#[must_use]
pub fn in_range(share: f64, lo: f64, hi: f64) -> bool {
    lo <= share && share <= hi
}

/// A window whose mask-1 share is outside the range.
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub layer: String,
    pub window: Rect,
    pub share: f64,
    pub areas: WindowAreas,
    /// The union of all fill in the window, any mask (context: how much of the window is fill).
    pub fill_area: i64,
}

impl Finding {
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        let by: serde_json::Map<String, serde_json::Value> = self
            .areas
            .by_mask
            .iter()
            .map(|(m, a)| (if *m == 0 { "uncoloured".to_string() } else { format!("mask_{m}") }, (*a).into()))
            .collect();
        serde_json::json!({
            "kind": "mask_balance",
            "layer": self.layer,
            "window": self.window,
            "share": self.share,
            "mask1_area": self.areas.mask1(),
            "total_area": self.areas.total(),
            "areas": by,
            "fill_area": self.fill_area,
        })
    }
}

/// One checked layer.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LayerReport {
    pub layer: String,
    pub num_masks: u8,
    pub design_shapes: usize,
    pub fill_shapes: usize,
    pub windows: usize,
    /// Windows with metal on the layer, whose share was compared with the range.
    pub windows_checked: usize,
    pub windows_empty: usize,
    pub windows_outside: usize,
    pub min_share: Option<f64>,
    pub max_share: Option<f64>,
}

/// What [`check_mask_balance`] found.
#[derive(Debug, Clone, Default)]
pub struct BalanceReport {
    pub die: Rect,
    pub layers: Vec<LayerReport>,
    pub findings: Vec<Finding>,
}

impl BalanceReport {
    #[must_use]
    pub fn windows_checked(&self) -> usize {
        self.layers.iter().map(|l| l.windows_checked).sum()
    }
}

/// Shape indices bucketed by a grid of `cell`-sized squares from `origin`.
struct Buckets {
    origin: (i64, i64),
    cell: i64,
    map: BTreeMap<(i64, i64), Vec<usize>>,
}

impl Buckets {
    fn new(shapes: &[BalanceShape], die: &Rect, cell: i32) -> Self {
        let mut b = Buckets { origin: (i64::from(die[0]), i64::from(die[1])), cell: i64::from(cell.max(1)), map: BTreeMap::new() };
        for (k, s) in shapes.iter().enumerate() {
            let Some(r) = clip(&s.rect, die) else { continue };
            let (lo, hi) = b.range(&r);
            for cx in lo.0..=hi.0 {
                for cy in lo.1..=hi.1 {
                    b.map.entry((cx, cy)).or_default().push(k);
                }
            }
        }
        b
    }
    /// Cells touched by the half-open `r`.
    fn range(&self, r: &Rect) -> ((i64, i64), (i64, i64)) {
        let f = |v: i32, o: i64| (i64::from(v) - o).div_euclid(self.cell);
        (
            (f(r[0], self.origin.0), f(r[1], self.origin.1)),
            (f(r[2] - 1, self.origin.0), f(r[3] - 1, self.origin.1)),
        )
    }
    /// Sorted, distinct indices of shapes in cells touched by `w`.
    fn query(&self, w: &Rect) -> Vec<usize> {
        let (lo, hi) = self.range(w);
        let mut out = Vec::new();
        for cx in lo.0..=hi.0 {
            for cy in lo.1..=hi.1 {
                if let Some(v) = self.map.get(&(cx, cy)) {
                    out.extend_from_slice(v);
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }
}

/// Stage: every window of one layer, its share against the range.
pub fn check_layer(
    layer: &str,
    num_masks: u8,
    shapes: &[BalanceShape],
    die: &Rect,
    opts: &BalanceOptions,
    findings: &mut Vec<Finding>,
) -> LayerReport {
    let mut rep = LayerReport {
        layer: layer.to_string(),
        num_masks,
        design_shapes: shapes.iter().filter(|s| !s.fill).count(),
        fill_shapes: shapes.iter().filter(|s| s.fill).count(),
        ..LayerReport::default()
    };
    let buckets = Buckets::new(shapes, die, opts.step);
    for w in windows(die, opts.window, opts.step) {
        rep.windows += 1;
        let near = buckets.query(&w);
        let areas = window_areas(near.iter().map(|&k| &shapes[k]), &w);
        let Some(share) = areas.share() else {
            rep.windows_empty += 1;
            continue;
        };
        rep.windows_checked += 1;
        rep.min_share = Some(rep.min_share.map_or(share, |m| m.min(share)));
        rep.max_share = Some(rep.max_share.map_or(share, |m| m.max(share)));
        if in_range(share, opts.lo, opts.hi) {
            continue;
        }
        rep.windows_outside += 1;
        let fill: Vec<Rect> = near.iter().filter(|&&k| shapes[k].fill).filter_map(|&k| clip(&shapes[k].rect, &w)).collect();
        findings.push(Finding { layer: layer.to_string(), window: w, share, areas, fill_area: union_area(&fill) });
    }
    rep
}

/// Stage: the routing layers the tech marks with more than one mask, as (name, number, masks).
pub fn balance_layers(db: &Db) -> Result<Vec<(String, i64, u8)>> {
    let mut out = Vec::new();
    for (name, _dir) in db.layers_with_direction()? {
        let masks = db.layer_get_num_masks(&name);
        if db.layer_get_type(&name)? != "ROUTING" || masks < 2 {
            continue;
        }
        out.push((name.clone(), i64::from(db.layer_get_number(&name)), masks.min(u32::from(u8::MAX)) as u8));
    }
    Ok(out)
}

/// Stage: every shape on the given layers (by number): routed wires, special wires, block pins,
/// placed instances' shapes (uncoloured), and fill.
pub fn collect_shapes(db: &Db, layers: &[i64]) -> Result<BTreeMap<i64, Vec<BalanceShape>>> {
    let mut out: BTreeMap<i64, Vec<BalanceShape>> = layers.iter().map(|&l| (l, Vec::new())).collect();
    let mut push = |layer: i64, rect: Rect, mask: u8, fill: bool| {
        if let Some(v) = out.get_mut(&layer) {
            v.push(BalanceShape { rect, mask, fill });
        }
    };
    for net in db.block_get_nets() {
        if db.net_is_special(&net) {
            continue;
        }
        let boxes = db.net_wire_boxes(&net);
        let masks = db.net_wire_box_masks(&net)?;
        for (b, &m) in boxes.iter().zip(&masks) {
            push(b.layer, [b.x0, b.y0, b.x1, b.y1], m, false);
        }
    }
    for (b, &m) in db.swire_boxes()?.iter().zip(&db.swire_box_masks()?) {
        push(b.0, [b.1, b.2, b.3, b.4], m, false);
    }
    for t in db.block_get_b_terms() {
        for p in 0..db.num_bterm_get_b_pins(&t) {
            let boxes = db.bpin_layer_boxes(&t, p)?;
            let masks = db.bpin_layer_box_masks(&t, p)?;
            for (b, &m) in boxes.iter().zip(&masks) {
                push(b.0, [b.1, b.2, b.3, b.4], m, false);
            }
        }
    }
    for s in db.inst_shapes()? {
        push(s.0, [s.1, s.2, s.3, s.4], 0, false);
    }
    for f in db.fill_rects()? {
        push(f.layer, f.rect, f.mask, true);
    }
    Ok(out)
}

/// Check every multi-mask routing layer of the block. One stage per step, in order.
pub fn check_mask_balance(db: &Db, opts: &BalanceOptions) -> Result<BalanceReport> {
    let die = [
        db.block_get_die_area_x_min(),
        db.block_get_die_area_y_min(),
        db.block_get_die_area_x_max(),
        db.block_get_die_area_y_max(),
    ];
    let mut rep = BalanceReport { die, ..BalanceReport::default() };
    let layers = balance_layers(db)?;
    if layers.is_empty() {
        return Ok(rep);
    }
    let numbers: Vec<i64> = layers.iter().map(|l| l.1).collect();
    let shapes = collect_shapes(db, &numbers)?;
    for (name, number, masks) in &layers {
        let v = shapes.get(number).map_or(&[][..], Vec::as_slice);
        let l = check_layer(name, *masks, v, &die, opts, &mut rep.findings);
        rep.layers.push(l);
    }
    Ok(rep)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(rect: Rect, mask: u8) -> BalanceShape {
        BalanceShape { rect, mask, fill: false }
    }
    fn fill(rect: Rect, mask: u8) -> BalanceShape {
        BalanceShape { rect, mask, fill: true }
    }
    fn opts(w: i32, s: i32, lo: f64, hi: f64) -> BalanceOptions {
        BalanceOptions::new(w, s, lo, hi).unwrap()
    }

    /// The share's denominator holds every category: mask 1, mask 2, uncoloured design and fill
    /// of every mask (fill with mask 0 is uncoloured).
    #[test]
    fn share_counts_uncoloured_and_fill_in_the_total() {
        let w = [0, 0, 100, 100];
        let shapes = [
            sh([0, 0, 10, 10], 1),     // 100 mask 1
            sh([20, 0, 30, 10], 2),    // 100 mask 2
            sh([40, 0, 50, 10], 0),    // 100 uncoloured design
            fill([60, 0, 70, 10], 2),  // 100 mask-2 fill
            fill([80, 0, 90, 10], 0),  // 100 fill without a mask
            fill([0, 20, 10, 30], 1),  // 100 mask-1 fill
        ];
        let a = window_areas(&shapes, &w);
        assert_eq!(a.by_mask, BTreeMap::from([(0, 200), (1, 200), (2, 200)]));
        assert_eq!(a.total(), 600);
        assert_eq!(a.share(), Some(200.0 / 600.0));
        // Without the fill the share is different: the fill is in.
        let design: Vec<BalanceShape> = shapes.iter().copied().filter(|s| !s.fill).collect();
        assert_eq!(window_areas(&design, &w).share(), Some(100.0 / 300.0));
        let no_unc: Vec<BalanceShape> = shapes.iter().copied().filter(|s| s.mask != 0).collect();
        assert_eq!(window_areas(&no_unc, &w).share(), Some(0.5));
    }

    /// Each category is a merged union: overlapping shapes of one mask count once. Overlap across
    /// categories counts in each (the total is the sum of the category unions).
    #[test]
    fn category_area_is_the_merged_union() {
        assert_eq!(union_area(&[[0, 0, 10, 10], [5, 5, 15, 15]]), 175);
        assert_eq!(union_area(&[[0, 0, 10, 10], [0, 0, 10, 10]]), 100);
        assert_eq!(union_area(&[[0, 0, 10, 10], [2, 2, 4, 4]]), 100);
        assert_eq!(union_area(&[[0, 0, 10, 10], [10, 0, 20, 10]]), 200, "abutting");
        assert_eq!(union_area(&[[0, 0, 30, 10], [10, -10, 20, 20]]), 300 + 200);
        assert_eq!(union_area(&[]), 0);
        assert_eq!(union_area(&[[0, 0, 0, 10]]), 0, "degenerate");
        let w = [0, 0, 100, 100];
        let a = window_areas(&[sh([0, 0, 10, 10], 1), fill([5, 0, 15, 10], 1), sh([0, 0, 10, 10], 0)], &w);
        assert_eq!(a.by_mask, BTreeMap::from([(0, 100), (1, 150)]));
        assert_eq!(a.total(), 250, "the uncoloured box under mask 1 counts in both");
    }

    /// A shape is clipped to the window before its area counts.
    #[test]
    fn shapes_are_clipped_to_the_window() {
        let a = window_areas(&[sh([-10, -10, 10, 10], 1), sh([90, 0, 200, 10], 2), sh([100, 0, 110, 10], 2)], &[0, 0, 100, 100]);
        assert_eq!(a.by_mask, BTreeMap::from([(1, 100), (2, 100)]));
    }

    /// Origins at lo + k*step up to and including the first window reaching hi; windows clipped
    /// to the die.
    #[test]
    fn window_stepping_and_clipping_at_the_die_edge() {
        assert_eq!(window_origins(0, 100, 50, 25), vec![0, 25, 50]);
        assert_eq!(window_origins(0, 105, 50, 25), vec![0, 25, 50, 75]);
        assert_eq!(window_origins(0, 100, 50, 50), vec![0, 50]);
        assert_eq!(window_origins(10, 40, 50, 25), vec![10], "die smaller than a window: one");
        assert_eq!(window_origins(0, 0, 50, 25), Vec::<i32>::new());
        assert_eq!(window_origins(-100, 0, 50, 30), vec![-100, -70, -40]);
        let w = windows(&[0, 0, 105, 60], 50, 25);
        assert_eq!(w.len(), 4 * 2);
        assert_eq!(w[0], [0, 0, 50, 50]);
        assert_eq!(w[3], [75, 0, 105, 50], "last column clipped to the die");
        assert_eq!(w[7], [75, 25, 105, 60], "last row clipped too");
    }

    /// The range is closed: a share exactly at either bound passes.
    #[test]
    fn range_bounds_are_inclusive() {
        assert!(in_range(0.2, 0.2, 0.8));
        assert!(in_range(0.8, 0.2, 0.8));
        assert!(!in_range(0.19999, 0.2, 0.8));
        assert!(!in_range(0.80001, 0.2, 0.8));
        // Exactly at the bound through the area arithmetic: 20/100 and 80/100.
        let die = [0, 0, 100, 100];
        let o = opts(100, 100, 0.2, 0.8);
        for (m1, m2, outside) in [(20, 80, 0), (80, 20, 0), (19, 81, 1), (81, 19, 1)] {
            let shapes = [sh([0, 0, m1, 1], 1), sh([0, 1, m2, 2], 2)];
            let mut f = Vec::new();
            let r = check_layer("M", 2, &shapes, &die, &o, &mut f);
            assert_eq!((r.windows_checked, r.windows_outside, f.len()), (1, outside, outside), "{m1}/{m2}");
        }
    }

    /// Per-window findings, empty windows counted and never checked; the lopsided window alone.
    #[test]
    fn check_layer_reports_each_window_outside() {
        let die = [0, 0, 200, 100];
        // Left half balanced, right half all mask 1, nothing in the top-right quarter.
        let shapes = [
            sh([0, 0, 50, 10], 1),
            sh([0, 20, 50, 30], 2),
            sh([150, 0, 200, 10], 1),
            fill([110, 0, 120, 10], 1),
        ];
        let mut f = Vec::new();
        let r = check_layer("M2", 2, &shapes, &die, &opts(100, 100, 0.2, 0.8), &mut f);
        assert_eq!((r.windows, r.windows_checked, r.windows_empty, r.windows_outside), (2, 2, 0, 1));
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].window, [100, 0, 200, 100]);
        assert_eq!(f[0].share, 1.0);
        assert_eq!(f[0].fill_area, 100);
        assert_eq!((r.design_shapes, r.fill_shapes), (3, 1));
        assert_eq!((r.min_share, r.max_share), (Some(0.5), Some(1.0)));
        // Smaller windows: the empty ones (the top row, and x 50..100 at the bottom) are counted, not
        // checked.
        let mut f = Vec::new();
        let r = check_layer("M2", 2, &shapes, &die, &opts(50, 50, 0.2, 0.8), &mut f);
        assert_eq!((r.windows, r.windows_checked, r.windows_empty), (8, 3, 5));
        assert_eq!(f.iter().map(|x| x.window).collect::<Vec<_>>(), vec![[100, 0, 150, 50], [150, 0, 200, 50]]);
    }

    /// Overlapping windows (step < window) find a shape through every window holding it, whichever
    /// bucket it was filed in.
    #[test]
    fn overlapping_windows_see_every_shape() {
        let die = [0, 0, 100, 100];
        let shapes = [sh([60, 60, 70, 70], 1)];
        let mut f = Vec::new();
        let r = check_layer("M", 2, &shapes, &die, &opts(50, 25, 0.2, 0.8), &mut f);
        // Windows at 25 and 50 on each axis hold it: 2 x 2.
        assert_eq!((r.windows, r.windows_checked), (9, 4));
        assert_eq!(f.len(), 4);
    }

    #[test]
    fn options_are_validated() {
        assert!(BalanceOptions::new(0, 1, 0.2, 0.8).is_err());
        assert!(BalanceOptions::new(10, 0, 0.2, 0.8).is_err());
        assert!(BalanceOptions::new(10, 11, 0.2, 0.8).is_err(), "step beyond the window leaves gaps");
        assert!(BalanceOptions::new(10, 10, 0.8, 0.2).is_err());
        assert!(BalanceOptions::new(10, 10, -0.1, 0.2).is_err());
        assert!(BalanceOptions::new(10, 10, 0.2, 1.1).is_err());
        assert!(BalanceOptions::new(10, 5, 0.0, 1.0).is_ok());
    }
}
