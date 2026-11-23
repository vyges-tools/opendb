// SPDX-License-Identifier: Apache-2.0
// © 2026 https://vyges.com. All Rights Reserved.

//! Same-mask forbidden spacing of a sandwiched wire, as hard OPPOSITE pairs of the decomposition.
//!
//! A deck may forbid a same-mask spacing band to a narrow wire that has metal of the other mask
//! close on both of its sides. This module derives the pairs of polygons that such a rule puts in
//! conflict when they share a mask; [`crate::masks_decomp`] colours them apart, as it does the
//! same-mask spacing and line-end pairs.
//!
//! The model (every reading below is pinned in a unit test):
//! - **Side to side, over a run.** Every measurement is between parallel edges facing each other
//!   (opposite outward normals), over a shared length of at least `run`. Edges are the polygon's
//!   own boundary (the union of its shapes).
//! - **The wire** (`W`): the parts of a polygon's edges that face OTHER metal outward no more than
//!   `s` away, merged per edge; of those, the parts facing such a part of the same polygon across
//!   it, `wlo..whi` apart (inclusive): both sides of the wire.
//! - **The other mask beside it:** any other polygon within `s`. Within `s` is closer than the
//!   same-mask spacing of a two-mask layer, so in any legal colouring it is the other mask; taking
//!   any polygon there keeps the pair set independent of the colouring it feeds.
//! - **The pair:** a part of a wire side facing another polygon outward `dlo..dhi` away
//!   (inclusive): the wire's polygon and that polygon take opposite masks.
//!
//! - **Never at the cost of a harder rule** ([`admit`]): a pair is taken only where the colouring
//!   can still hold it together with the same-mask spacing and line-end pairs and the anchored
//!   masks. One that would close an odd cycle or contradict an anchor is left out and counted:
//!   two masks cannot hold it there, it is the router's (or a stitch's) to clear.
//!
//! Same input, same output: every walk is in index order and the result is sorted.

use std::collections::{BTreeMap, BTreeSet};

use vyges_layout::edges::{ring_edges, Axis, Edge};
use vyges_layout::geom::Rect;

use crate::masks::MaskShape;
use crate::masks_decomp::polygons;

/// One forbidden-spacing rule (DBU): a wire `wlo..whi` wide with other metal no more than `s` away
/// on both sides must not share its mask with metal `dlo..dhi` away; each measurement over a run of
/// at least `run`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ForbiddenSpacing {
    pub wlo: i32,
    pub whi: i32,
    pub s: i32,
    pub run: i32,
    pub dlo: i32,
    pub dhi: i32,
}

impl ForbiddenSpacing {
    /// `LAYER:WLO,WHI,S,RUN,DLO,DHI`, DBU, ranges inclusive.
    pub fn parse(spec: &str) -> Result<(String, ForbiddenSpacing), String> {
        let (layer, rest) = spec.split_once(':').ok_or_else(|| format!("{spec}: wants LAYER:WLO,WHI,S,RUN,DLO,DHI"))?;
        let v: Vec<i32> = rest
            .split(',')
            .map(|x| x.trim().parse::<i32>().map_err(|_| format!("{spec}: not a DBU value: {x}")))
            .collect::<Result<_, _>>()?;
        let [wlo, whi, s, run, dlo, dhi] = v[..] else {
            return Err(format!("{spec}: wants six values WLO,WHI,S,RUN,DLO,DHI"));
        };
        if layer.is_empty() || wlo <= 0 || wlo > whi || s <= 0 || run < 0 || dlo <= 0 || dlo > dhi {
            return Err(format!("{spec}: needs a layer, 0 < WLO <= WHI, S > 0, RUN >= 0, 0 < DLO <= DHI"));
        }
        Ok((layer.to_string(), ForbiddenSpacing { wlo, whi, s, run, dlo, dhi }))
    }
}

fn outward(e: &Edge) -> (i32, i32) {
    ((e.b.1 - e.a.1).signum(), -(e.b.0 - e.a.0).signum())
}

/// The range an edge covers along its own axis.
fn span(e: &Edge) -> (i32, i32) {
    if e.axis() == Axis::Horizontal {
        (e.a.0.min(e.b.0), e.a.0.max(e.b.0))
    } else {
        (e.a.1.min(e.b.1), e.a.1.max(e.b.1))
    }
}

/// `f` parallel to `e` and facing it: the offset from `e` to `f` along `e`'s outward normal.
fn facing(e: &Edge, f: &Edge) -> Option<i32> {
    let (oe, of) = (outward(e), outward(f));
    if e.axis() != f.axis() || of != (-oe.0, -oe.1) {
        return None;
    }
    Some(if e.axis() == Axis::Horizontal { (f.a.1 - e.a.1) * oe.1 } else { (f.a.0 - e.a.0) * oe.0 })
}

fn merge(mut iv: Vec<(i32, i32)>) -> Vec<(i32, i32)> {
    iv.sort_unstable();
    let mut m: Vec<(i32, i32)> = Vec::new();
    for (lo, hi) in iv {
        match m.last_mut() {
            Some(l) if lo <= l.1 => l.1 = l.1.max(hi),
            _ => m.push((lo, hi)),
        }
    }
    m
}

/// The parts of `parts` that `(flo, fhi)` overlaps over at least `run` (and a positive length).
fn shared(parts: &[(i32, i32)], (flo, fhi): (i32, i32), run: i32) -> Vec<(i32, i32)> {
    parts
        .iter()
        .filter_map(|&(lo, hi)| {
            let (a, b) = (lo.max(flo), hi.min(fhi));
            (b > a && b - a >= run).then_some((a, b))
        })
        .collect()
}

/// The boundary edges of the union of `rects`.
fn boundary(rects: &[Rect]) -> Vec<Edge> {
    let tiles = vyges_layout::boolean::boolean(rects, &[], vyges_layout::boolean::Op::Or);
    vyges_layout::contour::trace_contours(&tiles).iter().flat_map(|r| ring_edges(r)).collect()
}

fn rect_of(s: &MaskShape) -> Rect {
    Rect { x0: s.rect[0], y0: s.rect[1], x1: s.rect[2], y1: s.rect[3] }
}

/// The shape of polygon `members` holding the middle of `part` of edge `e`.
fn holder(shapes: &[MaskShape], members: &[usize], e: &Edge, part: (i32, i32)) -> usize {
    let c = (part.0 + part.1) / 2;
    let (x, y) = if e.axis() == Axis::Horizontal { (c, e.a.1) } else { (e.a.0, c) };
    members
        .iter()
        .copied()
        .find(|&k| {
            let r = shapes[k].rect;
            r[0] <= x && x <= r[2] && r[1] <= y && y <= r[3]
        })
        .unwrap_or(members[0])
}

/// Hard OPPOSITE pairs from forbidden-spacing rules: `(i, j)`, `i < j`, shape indices, one pair
/// per two polygons a rule of `rules` puts in conflict on one mask (see the module doc).
pub fn forbidden_pairs(shapes: &[MaskShape], rules: &[ForbiddenSpacing]) -> Vec<(usize, usize)> {
    if rules.is_empty() || shapes.len() < 2 {
        return Vec::new();
    }
    let poly = polygons(shapes, &vec![true; shapes.len()]);
    let mut members: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (k, &p) in poly.iter().enumerate() {
        members.entry(p).or_default().push(k);
    }
    let reach = rules.iter().map(|r| r.s.max(r.dhi)).max().unwrap_or(0) + 1;
    let mut near: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
    for (i, j, _) in crate::masks::conflict_edges(shapes, reach) {
        let (p, q) = (poly[i], poly[j]);
        if p != q {
            near.entry(p).or_default().insert(q);
            near.entry(q).or_default().insert(p);
        }
    }
    let edges_of = |ps: &mut dyn Iterator<Item = usize>| -> Vec<Edge> {
        let rects: Vec<Rect> = ps.flat_map(|p| members[&p].iter().map(|&k| rect_of(&shapes[k]))).collect();
        boundary(&rects)
    };
    let mut out: BTreeSet<(usize, usize)> = BTreeSet::new();
    for (&p, qs) in &near {
        let own = edges_of(&mut std::iter::once(p));
        let others = edges_of(&mut qs.iter().copied());
        let per_q: Vec<(usize, Vec<Edge>)> = qs.iter().map(|&q| (q, edges_of(&mut std::iter::once(q)))).collect();
        for r in rules {
            // 1. own edges facing other metal outward, no more than s away
            let beside: Vec<Vec<(i32, i32)>> = own
                .iter()
                .map(|e| {
                    let iv = others
                        .iter()
                        .filter(|f| facing(e, f).is_some_and(|g| g > 0 && g <= r.s))
                        .flat_map(|f| shared(&[span(e)], span(f), r.run))
                        .collect();
                    merge(iv)
                })
                .collect();
            // 2. of those, the parts facing such a part across the polygon, wlo..whi
            let sides: Vec<Vec<(i32, i32)>> = own
                .iter()
                .enumerate()
                .map(|(i, e)| {
                    let mut iv = Vec::new();
                    for (j, f) in own.iter().enumerate() {
                        if i == j {
                            continue;
                        }
                        if facing(e, f).is_some_and(|g| -g >= r.wlo && -g <= r.whi) {
                            for &q in &beside[j] {
                                iv.extend(shared(&beside[i], q, r.run));
                            }
                        }
                    }
                    merge(iv)
                })
                .collect();
            // 3. the wire's sides facing another polygon outward, dlo..dhi
            for (e, parts) in own.iter().zip(&sides) {
                if parts.is_empty() {
                    continue;
                }
                for (q, eq) in &per_q {
                    for f in eq {
                        if !facing(e, f).is_some_and(|g| g >= r.dlo && g <= r.dhi) {
                            continue;
                        }
                        if let Some(&part) = shared(parts, span(f), r.run).first() {
                            let i = holder(shapes, &members[&p], e, part);
                            let j = holder(shapes, &members[q], f, part);
                            out.insert((i.min(j), i.max(j)));
                        }
                    }
                }
            }
        }
    }
    out.into_iter().collect()
}

/// Of the forbidden-spacing pairs `cand` (in order), the ones a two-mask colouring can hold with
/// every pair of `hard` and the anchored masks (`anchor[k]`: shape `k` keeps `shapes[k].mask`),
/// and how many were left out. One net's touching shapes are one polygon and share a mask; each
/// pair in `hard` puts its two shapes on opposite masks. A pair is left out when its two shapes
/// are already tied to the same mask by those, or by the pairs taken before it.
#[must_use]
pub fn admit(shapes: &[MaskShape], hard: &[(usize, usize)], anchor: &[bool], cand: &[(usize, usize)]) -> (Vec<(usize, usize)>, usize) {
    // union-find with the parity to the root; node `n` is "mask 1", tying the anchored shapes
    let n = shapes.len();
    let mut up: Vec<usize> = (0..=n).collect();
    let mut par: Vec<u8> = vec![0; n + 1];
    fn find(up: &mut [usize], par: &mut [u8], x: usize) -> (usize, u8) {
        let mut p = 0u8;
        let mut r = x;
        while up[r] != r {
            p ^= par[r];
            r = up[r];
        }
        // path compression, keeping each node's parity to the new root
        let (mut y, mut q) = (x, p);
        while up[y] != r {
            let next = up[y];
            let pn = q ^ par[y];
            up[y] = r;
            par[y] = q;
            y = next;
            q = pn;
        }
        (r, p)
    }
    // tie a and b at parity d; false when they are tied the other way already
    let mut tie = |a: usize, b: usize, d: u8| -> bool {
        let ((ra, pa), (rb, pb)) = (find(&mut up, &mut par, a), find(&mut up, &mut par, b));
        if ra == rb {
            return pa ^ pb == d;
        }
        let (lo, hi) = (ra.min(rb), ra.max(rb));
        up[hi] = lo;
        par[hi] = pa ^ pb ^ d;
        true
    };
    let poly = polygons(shapes, &vec![true; n]);
    for (k, &p) in poly.iter().enumerate() {
        tie(k, p, 0);
    }
    for &(i, j) in hard {
        tie(i, j, 1);
    }
    for (k, s) in shapes.iter().enumerate() {
        if anchor[k] && s.mask != 0 {
            tie(k, n, u8::from(s.mask == 2));
        }
    }
    let mut kept = Vec::new();
    let mut dropped = 0;
    for &(i, j) in cand {
        if tie(i, j, 1) {
            kept.push((i, j));
        } else {
            dropped += 1;
        }
    }
    (kept, dropped)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(net: &str, r: [i32; 4]) -> MaskShape {
        MaskShape { net: net.into(), rect: r, mask: 1, fixed: false }
    }

    const R: ForbiddenSpacing = ForbiddenSpacing { wlo: 19, whi: 23, s: 26, run: 76, dlo: 84, dhi: 97 };

    /// W (0..310 x 0..wt) with metal `bl` under and `bu` over it (18 thick), and G whose bottom is
    /// `g` over W's top; the shape order is W, upper B, G, lower B.
    fn case(wt: i32, bl: Option<i32>, bu: i32, g: i32) -> Vec<MaskShape> {
        let mut v = vec![sh("w", [0, 0, 310, wt]), sh("b1", [0, wt + bu, 310, wt + bu + 18]), sh("g", [0, wt + g, 310, wt + g + 21])];
        if let Some(bl) = bl {
            v.push(sh("b2", [0, -bl - 18, 310, -bl]));
        }
        v
    }

    /// Rule: the wire's polygon and the facing polygon pair up, inclusive at every bound; one
    /// side's metal alone, a gap beyond `s`, a width or a distance outside its range: no pair.
    #[test]
    fn a_sandwiched_wire_pairs_with_the_metal_it_faces() {
        let n = |v: Vec<MaskShape>| forbidden_pairs(&v, &[R]);
        assert_eq!(n(case(21, Some(25), 25, 90)), vec![(0, 2)], "W 21, metal 25 off both sides, G 90 over");
        for wt in [19, 23] {
            assert_eq!(n(case(wt, Some(25), 25, 90)).len(), 1, "width {wt} is within 19..23");
        }
        for wt in [18, 24] {
            assert!(n(case(wt, Some(25), 25, 90)).is_empty(), "width {wt} is not");
        }
        assert!(n(case(21, None, 25, 90)).is_empty(), "metal on one side only");
        assert_eq!(n(case(21, Some(26), 25, 90)).len(), 1, "26 is no more than 26");
        assert!(n(case(21, Some(27), 25, 90)).is_empty(), "27 is more than 26");
        for g in [84, 97] {
            assert_eq!(n(case(21, Some(25), 25, g)).len(), 1, "G at {g} is within 84..97");
        }
        for g in [83, 98] {
            assert!(n(case(21, Some(25), 25, g)).is_empty(), "G at {g} is not");
        }
    }

    /// Rule: each measurement is over a run of at least `run`, per pair before the merge.
    #[test]
    fn each_step_needs_its_run() {
        let n = |v: Vec<MaskShape>| forbidden_pairs(&v, &[R]).len();
        let mut short = case(21, None, 25, 90);
        short.push(sh("b2", [0, -43, 75, -25]));
        assert_eq!(n(short), 0, "the metal under W runs 75");
        let mut just = case(21, None, 25, 90);
        just.push(sh("b2", [0, -43, 76, -25]));
        assert_eq!(n(just), 1, "a run of 76");
        let mut g75 = case(21, Some(25), 25, 90);
        g75[2] = sh("g", [0, 112, 75, 133]);
        assert_eq!(n(g75), 0, "G shares 75");
        let mut g76 = case(21, Some(25), 25, 90);
        g76[2] = sh("g", [0, 112, 76, 133]);
        assert_eq!(n(g76), 1, "G shares 76");
        // the metal under W in two pieces, one polygon each, 60 and 65 long: two runs, not one
        let mut pieces = case(21, None, 25, 90);
        pieces.extend([sh("b2", [0, -59, 60, -25]), sh("b3", [60, -59, 125, -19])]);
        assert_eq!(n(pieces), 0, "runs of 60 and 65 are not one of 125");
    }

    /// Rule: the parts facing other metal merge per edge before the width step: two pieces under
    /// W, 83 each at gaps 25 and 21 (two edges of the metal), give one part of 166 on W's edge,
    /// and over it the metal 84 long, half of it over each piece, still finds a run of 84.
    #[test]
    fn parts_beside_the_wire_merge_per_edge() {
        let v = vec![
            sh("w", [0, 0, 310, 21]),
            sh("b1", [41, 46, 125, 64]),
            sh("g", [0, 112, 310, 133]),
            sh("b2", [0, -43, 83, -25]),
            sh("b3", [83, -39, 166, -21]),
        ];
        assert_eq!(forbidden_pairs(&v, &[R]), vec![(0, 2)]);
    }

    /// Rule: the metal beside the wire is any other polygon (it is the other mask in any legal
    /// colouring), and the pair is with the polygon faced, under the wire as well as over it.
    #[test]
    fn the_pair_is_with_the_polygon_faced_on_either_side() {
        let under = vec![sh("w", [0, 0, 310, 21]), sh("b1", [0, 46, 310, 64]), sh("b2", [0, -43, 310, -25]), sh("g", [0, -113, 310, -92])];
        assert_eq!(forbidden_pairs(&under, &[R]), vec![(0, 3)], "G 92 under W");
        // the wire made of two shapes of one net: one polygon, the pair names the shape faced
        let mut two = case(21, Some(25), 25, 90);
        two[0] = sh("w", [0, 0, 150, 21]);
        two.push(sh("w", [150, 0, 310, 21]));
        assert_eq!(forbidden_pairs(&two, &[R]).len(), 1, "one pair per two polygons");
    }

    /// Rule: the pairs are hard OPPOSITE pairs of the colouring. On one track pattern that puts W
    /// and G on one mask (W and the metal beside it apart by spacing), the decomposition flips G.
    #[test]
    fn the_colouring_puts_the_faced_metal_on_the_other_mask() {
        let mut v = case(21, Some(25), 25, 90);
        for (k, m) in [(0, 1), (1, 2), (2, 1), (3, 2)] {
            v[k].mask = m;
        }
        let spacing = vec![(0, 1), (0, 3)];
        let free = vec![false; v.len()];
        let d = crate::masks_decomp::decompose(&v, &spacing, &free);
        assert_eq!(d.mask[0], d.mask[2], "spacing alone leaves W and G on one mask");
        let mut pairs = spacing.clone();
        pairs.extend(forbidden_pairs(&v, &[R]));
        pairs.sort_unstable();
        let d = crate::masks_decomp::decompose(&v, &pairs, &free);
        assert_ne!(d.mask[0], d.mask[2], "with the forbidden-spacing pair they differ");
        assert_ne!(d.mask[0], d.mask[1]);
        assert_ne!(d.mask[0], d.mask[3]);
    }

    /// Rule: a pair is never taken at the cost of a harder rule. W and G tied to one mask through
    /// spacing pairs (W-B1 and B1-G) cannot also be opposite: the
    /// pair is left out and counted; on a free layout it is taken. An anchor that fixes both ends
    /// to one mask leaves it out the same way.
    #[test]
    fn a_pair_that_would_close_an_odd_cycle_is_left_out() {
        let v = case(21, Some(25), 25, 90);
        let cand = forbidden_pairs(&v, &[R]);
        assert_eq!(cand, vec![(0, 2)]);
        let free = vec![false; v.len()];
        assert_eq!(admit(&v, &[(0, 1), (0, 3)], &free, &cand), (cand.clone(), 0), "W-B and W-B: W and G still free");
        assert_eq!(admit(&v, &[(0, 1), (1, 2)], &free, &cand), (vec![], 1), "W-B1-G ties W and G to one mask");
        let mut a = v.clone();
        a[0].mask = 2;
        a[2].mask = 2;
        let anchored = vec![true, false, true, false];
        assert_eq!(admit(&a, &[], &anchored, &cand), (vec![], 1), "both anchored on mask 2");
        a[2].mask = 1;
        assert_eq!(admit(&a, &[], &anchored, &cand), (cand.clone(), 0), "anchored on opposite masks");
        // one net's touching shapes are one polygon: W and G tied through shapes of their own nets
        let mut net = v.clone();
        net.push(sh("w", [310, 0, 397, 21]));
        net.push(sh("g", [310, 112, 397, 133]));
        assert_eq!(admit(&net, &[(1, 4), (1, 5)], &vec![false; net.len()], &cand), (vec![], 1), "via the shapes W and G touch");
    }

    /// Rule: masks along a chain of opposite pairs alternate, however the chain was joined. The
    /// chain 0-1-2-3-4 joined out of order (3-4, 1-2, 2-3, 0-1): 0 and 4 on one mask, 0 and 3 opposite,
    /// 1 and 4 opposite; each answer read after the ones before it.
    #[test]
    fn a_chain_of_pairs_alternates_in_any_join_order() {
        let v: Vec<MaskShape> = (0..5).map(|k| sh(&format!("n{k}"), [k * 1000, 0, k * 1000 + 21, 310])).collect();
        let hard = [(3, 4), (1, 2), (2, 3), (0, 1)];
        let free = vec![false; v.len()];
        assert_eq!(admit(&v, &hard, &free, &[(0, 4), (1, 4), (0, 3), (1, 3)]), (vec![(1, 4), (0, 3)], 2));
    }

    #[test]
    fn a_rule_parses_from_its_spec() {
        assert_eq!(ForbiddenSpacing::parse("M2:19,23,26,76,84,97"), Ok(("M2".to_string(), R)));
        for bad in ["M2", "M2:19,23,26,76,84", "M2:23,19,26,76,84,97", "M2:19,23,26,76,97,84", ":19,23,26,76,84,97", "M2:a,23,26,76,84,97"] {
            assert!(ForbiddenSpacing::parse(bad).is_err(), "{bad}");
        }
    }
}
