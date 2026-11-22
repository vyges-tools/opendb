// SPDX-License-Identifier: Apache-2.0
// © 2026 https://vyges.com. All Rights Reserved.

//! Two-mask decomposition of one layer: a constraint colouring over polygons.
//!
//! The colour step first gives every routed shape its track's mask (track parity). Track parity
//! is a choice, not a rule: what a multi-patterned layer must satisfy is that no two shapes of one
//! mask break a same-mask rule. Where the tracks' own parity puts two such shapes on one mask (two
//! tracks apart but closer than the same-mask spacing, as around a narrower track pitch), the
//! decomposition here recolours whole polygons so that every hard constraint holds, wherever a
//! legal colouring exists.
//!
//! The model, as a decomposition engine takes it:
//! - **Nodes are polygons:** one net's shapes on the layer that overlap or share an edge are one
//!   piece of metal and take one mask (no stitch is ever made here).
//! - **Hard OPPOSITE edges:** two polygons that would break a same-mask rule if they shared a mask
//!   (the caller's predicate, the same one the check uses).
//! - **Anchors are never flipped:** a shape that came in coloured (a pre-coloured wire, a coloured
//!   special wire or pin) forces its polygon's mask. A polygon holding anchors of both masks is
//!   designed as a stitch and is frozen as it is; a neighbour must then differ from the frozen
//!   shape it is close to, which is a forced mask on the neighbour.
//! - **Preference:** the colour each shape has now (track parity). A component free of anchors
//!   takes the phase that keeps more of its shapes' current masks.
//!
//! Per connected component of the hard edges, lowest polygon first:
//! 1. Two-colour it by breadth-first search from its lowest polygon (neighbours in index order).
//!    An odd cycle: the component cannot be coloured; it keeps its current masks (the check
//!    reports the cycle).
//! 2. Forced masks fix the phase. Two forced masks that disagree under the colouring: an anchor
//!    conflict; the component keeps its current masks and the conflict is reported.
//! 3. Otherwise the phase keeping more shapes' current masks; **tie-break:** the phase in which
//!    the component's lowest polygon keeps the mask of its lowest coloured shape (mask 1 if it
//!    has none).
//!
//! Same input, same output: polygons and edges are numbered by shape order, every walk is in
//! index order, and nothing depends on hashing.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::masks::MaskShape;

/// What [`decompose`] decided for one layer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Decomposition {
    /// Per input shape, the mask it should carry; `None` where it is left as it is (frozen,
    /// excluded, odd cycle, anchor conflict).
    pub mask: Vec<Option<u8>>,
    pub polygons: usize,
    pub components: usize,
    /// Polygons whose mask the decomposition changed.
    pub flipped: usize,
    /// Components left as they were: not two-colourable.
    pub odd_components: usize,
    /// Anchor conflicts: the shapes of the forced polygons of each conflicting component.
    pub anchor_conflicts: Vec<Vec<usize>>,
}

/// One net's touching shapes (overlap, or a shared edge of positive length), any mask, among
/// `take`: the polygon of each taken shape, named by its lowest shape index; `usize::MAX` for a
/// shape not taken.
#[must_use]
pub fn polygons(shapes: &[MaskShape], take: &[bool]) -> Vec<usize> {
    fn find(p: &mut [usize], mut x: usize) -> usize {
        while p[x] != x {
            p[x] = p[p[x]];
            x = p[x];
        }
        x
    }
    let mut p: Vec<usize> = (0..shapes.len()).collect();
    let mut by_net: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (k, s) in shapes.iter().enumerate().filter(|(k, _)| take[*k]) {
        by_net.entry(s.net.as_str()).or_default().push(k);
    }
    for v in by_net.values_mut() {
        v.sort_by_key(|&k| (shapes[k].rect[0], k));
        for x in 0..v.len() {
            let a = shapes[v[x]].rect;
            for &k in &v[x + 1..] {
                let b = shapes[k].rect;
                if b[0] > a[2] {
                    break;
                }
                let ox = a[2].min(b[2]) as i64 - a[0].max(b[0]) as i64;
                let oy = a[3].min(b[3]) as i64 - a[1].max(b[1]) as i64;
                if ox >= 0 && oy >= 0 && (ox > 0 || oy > 0) {
                    let (ra, rb) = (find(&mut p, v[x]), find(&mut p, k));
                    let (lo, hi) = (ra.min(rb), ra.max(rb));
                    p[hi] = lo;
                }
            }
        }
    }
    (0..shapes.len()).map(|k| if take[k] { find(&mut p, k) } else { usize::MAX }).collect()
}

/// Decompose one two-mask layer. `pairs`: shape pairs that would break a same-mask rule on one
/// mask (any order, duplicates allowed). `anchor[k]`: shape k's mask is fixed (it came in
/// coloured). A fixed shape with no mask (`shapes[k].fixed && mask == 0`) takes no part: it can be
/// neither coloured nor recoloured here.
#[must_use]
pub fn decompose(shapes: &[MaskShape], pairs: &[(usize, usize)], anchor: &[bool]) -> Decomposition {
    let n = shapes.len();
    let take: Vec<bool> = shapes.iter().map(|s| !(s.fixed && s.mask == 0)).collect();
    let poly = polygons(shapes, &take);
    // anchors per polygon: the masks forced on it by its own anchored shapes
    let mut own: BTreeMap<usize, BTreeSet<u8>> = BTreeMap::new();
    for k in (0..n).filter(|&k| take[k] && anchor[k] && shapes[k].mask != 0) {
        own.entry(poly[k]).or_default().insert(shapes[k].mask);
    }
    let frozen: BTreeSet<usize> = own.iter().filter(|(_, m)| m.len() > 1).map(|(&p, _)| p).collect();
    let mut forced: BTreeMap<usize, BTreeSet<u8>> =
        own.iter().filter(|(p, m)| m.len() == 1 && !frozen.contains(p)).map(|(&p, m)| (p, m.clone())).collect();
    // edges between free polygons; a frozen shape forces the other mask on a free neighbour
    let mut adj: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
    for &(a, b) in pairs {
        if !take[a] || !take[b] || poly[a] == poly[b] {
            continue;
        }
        let (pa, pb) = (poly[a], poly[b]);
        match (frozen.contains(&pa), frozen.contains(&pb)) {
            (false, false) => {
                adj.entry(pa).or_default().insert(pb);
                adj.entry(pb).or_default().insert(pa);
            }
            (true, false) if shapes[a].mask != 0 => {
                forced.entry(pb).or_default().insert(3 - shapes[a].mask);
            }
            (false, true) if shapes[b].mask != 0 => {
                forced.entry(pa).or_default().insert(3 - shapes[b].mask);
            }
            _ => {}
        }
    }
    let nodes: BTreeSet<usize> = (0..n).filter(|&k| take[k] && !frozen.contains(&poly[k])).map(|k| poly[k]).collect();
    // shapes of each polygon, in order
    let mut members: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for k in (0..n).filter(|&k| take[k]) {
        members.entry(poly[k]).or_default().push(k);
    }
    let mut out = Decomposition { mask: vec![None; n], polygons: members.len(), ..Decomposition::default() };
    let mut side: BTreeMap<usize, u8> = BTreeMap::new();
    for &start in &nodes {
        if side.contains_key(&start) {
            continue;
        }
        out.components += 1;
        // 1. two-colour the component (side 0 / 1), breadth first, neighbours in index order
        let mut comp = vec![start];
        let mut odd = false;
        side.insert(start, 0);
        let mut q = VecDeque::from([start]);
        while let Some(u) = q.pop_front() {
            for &v in adj.get(&u).into_iter().flatten() {
                match side.get(&v) {
                    None => {
                        side.insert(v, 1 - side[&u]);
                        comp.push(v);
                        q.push_back(v);
                    }
                    Some(&s) if s == side[&u] => odd = true,
                    Some(_) => {}
                }
            }
        }
        comp.sort_unstable();
        if odd {
            out.odd_components += 1;
            continue;
        }
        // 2. forced masks fix the phase: mask(side 0) = phase, mask(side 1) = 3 - phase
        let mut phases = BTreeSet::new();
        for p in comp.iter().filter(|p| forced.contains_key(p)) {
            for &m in &forced[p] {
                phases.insert(if side[p] == 0 { m } else { 3 - m });
            }
        }
        let phase = match phases.len() {
            0 => {
                // 3. keep more current masks; tie: the lowest polygon keeps its first mask
                let keep = |ph: u8| -> usize {
                    comp.iter()
                        .flat_map(|p| members[p].iter().map(move |&k| (p, k)))
                        .filter(|&(p, k)| shapes[k].mask == if side[p] == 0 { ph } else { 3 - ph })
                        .count()
                };
                let (k1, k2) = (keep(1), keep(2));
                if k1 != k2 {
                    if k1 > k2 { 1 } else { 2 }
                } else {
                    let low = comp[0];
                    let first = members[&low].iter().map(|&k| shapes[k].mask).find(|&m| m != 0).unwrap_or(1);
                    if side[&low] == 0 { first } else { 3 - first }
                }
            }
            1 => *phases.iter().next().expect("one phase"),
            _ => {
                let mut c: Vec<usize> = comp.iter().filter(|p| forced.contains_key(p)).flat_map(|p| members[p].iter().copied()).collect();
                c.sort_unstable();
                out.anchor_conflicts.push(c);
                continue;
            }
        };
        for p in &comp {
            let m = if side[p] == 0 { phase } else { 3 - phase };
            if members[p].iter().any(|&k| shapes[k].mask != m) {
                out.flipped += 1;
            }
            for &k in &members[p] {
                out.mask[k] = Some(m);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(net: &str, r: [i32; 4], mask: u8) -> MaskShape {
        MaskShape { net: net.into(), rect: r, mask, fixed: false }
    }

    fn masks(d: &Decomposition) -> Vec<u8> {
        d.mask.iter().map(|m| m.unwrap_or(0)).collect()
    }

    /// Rule: track parity is only a preference. Two wires two tracks apart (same parity, same
    /// mask) that would break a same-mask rule are put on different masks; the component keeps
    /// the phase that changes fewer shapes (here: one of the two is flipped, the lower keeps its
    /// mask by the tie-break).
    #[test]
    fn a_hard_pair_on_one_parity_is_split() {
        let s = vec![sh("a", [0, 0, 1000, 32], 1), sh("c", [0, 96, 1000, 128], 1)];
        let d = decompose(&s, &[(0, 1)], &[false, false]);
        assert_eq!(masks(&d), vec![1, 2]);
        assert_eq!((d.components, d.flipped, d.odd_components), (1, 1, 0));
        // no constraint: nothing moves
        let d = decompose(&s, &[], &[false, false]);
        assert_eq!(masks(&d), vec![1, 1]);
        assert_eq!(d.flipped, 0);
    }

    /// Rule: the phase keeps more shapes' current masks. A chain a - b - c (b breaks the rule
    /// with both) on masks 1, 1, 2 has two colourings: (1, 2, 1) keeps one shape's mask (a),
    /// (2, 1, 2) keeps two (b and c), so the second is taken.
    #[test]
    fn the_phase_keeps_more_current_masks() {
        let s = vec![sh("a", [0, 0, 10, 10], 1), sh("b", [0, 20, 10, 30], 1), sh("c", [0, 40, 10, 50], 2)];
        let d = decompose(&s, &[(0, 1), (1, 2)], &[false; 3]);
        assert_eq!(masks(&d), vec![2, 1, 2]);
        assert_eq!(d.flipped, 1);
    }

    /// Tie-break, stated: equal counts, the component's lowest polygon keeps the mask of its
    /// lowest coloured shape (1 if none is coloured).
    #[test]
    fn a_tie_keeps_the_lowest_polygons_mask() {
        let s = vec![sh("a", [0, 0, 10, 10], 2), sh("b", [0, 20, 10, 30], 2)];
        assert_eq!(masks(&decompose(&s, &[(0, 1)], &[false; 2])), vec![2, 1]);
        let s = vec![sh("a", [0, 0, 10, 10], 0), sh("b", [0, 20, 10, 30], 0)];
        assert_eq!(masks(&decompose(&s, &[(1, 0)], &[false; 2])), vec![1, 2]);
    }

    /// Rule (anchors never flipped): an anchored shape keeps its mask and fixes the phase of its
    /// component, against the preference. Two anchors the colouring cannot both keep: an anchor
    /// conflict, reported, and the component is left as it was.
    #[test]
    fn anchors_are_never_flipped() {
        let s = vec![sh("a", [0, 0, 10, 10], 1), sh("b", [0, 20, 10, 30], 1), sh("c", [0, 40, 10, 50], 2)];
        // without the anchor the phase would flip b alone (previous test); anchored on b, the
        // others follow it
        let d = decompose(&s, &[(0, 1), (1, 2)], &[false, true, false]);
        assert_eq!(masks(&d), vec![2, 1, 2]);
        let d = decompose(&s, &[(0, 1), (1, 2)], &[true, false, false]);
        assert_eq!(masks(&d), vec![1, 2, 1]);
        // a and c anchored on different masks, two steps apart: no colouring keeps both
        let d = decompose(&s, &[(0, 1), (1, 2)], &[true, false, true]);
        assert_eq!(masks(&d), vec![0, 0, 0]);
        assert_eq!(d.anchor_conflicts, vec![vec![0, 2]]);
    }

    /// Rule: one net's touching shapes are one piece of metal and take one mask, whatever their
    /// tracks gave them (a jog across two tracks); shapes of two nets, or meeting at a corner
    /// only, are separate.
    #[test]
    fn a_nets_touching_shapes_take_one_mask() {
        let s = vec![
            sh("n", [0, 0, 100, 32], 1),
            sh("n", [100, 0, 132, 96], 2), // abuts the first on an edge
            sh("m", [200, 0, 300, 32], 1),
            sh("n", [132, 96, 160, 128], 2), // meets the second at a corner only
        ];
        let p = polygons(&s, &[true; 4]);
        assert_eq!(p, vec![0, 0, 2, 3]);
        let d = decompose(&s, &[], &[false; 4]);
        assert_eq!(masks(&d)[..2], [1, 1], "tie: the lowest shape's mask");
        assert_eq!(d.flipped, 1);
    }

    /// An odd cycle cannot be coloured: the component keeps its masks (the check reports it).
    #[test]
    fn an_odd_cycle_is_left_as_it_is() {
        let s = vec![sh("a", [0, 0, 1, 1], 1), sh("b", [5, 0, 6, 1], 1), sh("c", [10, 0, 11, 1], 2)];
        let d = decompose(&s, &[(0, 1), (1, 2), (0, 2)], &[false; 3]);
        assert_eq!(masks(&d), vec![0, 0, 0]);
        assert_eq!(d.odd_components, 1);
        // an even cycle colours
        let s4 = vec![sh("a", [0, 0, 1, 1], 1), sh("b", [5, 0, 6, 1], 1), sh("c", [10, 0, 11, 1], 1), sh("d", [15, 0, 16, 1], 1)];
        let d = decompose(&s4, &[(0, 1), (1, 2), (2, 3), (3, 0)], &[false; 4]);
        assert_eq!(masks(&d), vec![1, 2, 1, 2]);
    }

    /// A polygon anchored on both masks is a designed stitch: frozen as it is, and a free
    /// neighbour close to one of its shapes must take the other mask. A fixed shape with no mask
    /// takes no part.
    #[test]
    fn a_stitched_polygon_is_frozen_and_forces_its_neighbours() {
        let mut s = vec![sh("p", [0, 0, 100, 32], 1), sh("p", [100, 0, 200, 32], 2), sh("q", [150, 64, 300, 96], 2)];
        s[0].fixed = true;
        s[1].fixed = true;
        let d = decompose(&s, &[(1, 2)], &[true, true, false]);
        assert_eq!(d.mask, vec![None, None, Some(1)]);
        let mut u = vec![sh("pin", [0, 0, 10, 10], 0), sh("w", [0, 20, 10, 30], 1)];
        u[0].fixed = true;
        let d = decompose(&u, &[(0, 1)], &[false, false]);
        assert_eq!(d.mask, vec![None, Some(1)]);
        assert_eq!(d.polygons, 1);
    }

    /// Same input, same output, whatever order the pairs come in.
    #[test]
    fn the_result_does_not_depend_on_pair_order() {
        let s: Vec<MaskShape> = (0..8).map(|k| sh(&format!("n{k}"), [0, k * 40, 10, k * 40 + 10], 1)).collect();
        let pairs: Vec<(usize, usize)> = (0..7).map(|k| (k, k + 1)).collect();
        let mut rev = pairs.iter().map(|&(a, b)| (b, a)).collect::<Vec<_>>();
        rev.reverse();
        let a = decompose(&s, &pairs, &[false; 8]);
        assert_eq!(a, decompose(&s, &rev, &[false; 8]));
        assert_eq!(masks(&a), vec![1, 2, 1, 2, 1, 2, 1, 2]);
    }
}
