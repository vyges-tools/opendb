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
//! 4. **Long-range separators** ([`decompose_soft`]): soft OPPOSITE pairs, shapes closer than a
//!    range beyond the same-mask spacing (the colour-balance remedy a sign-off decomposition
//!    offers: ask, never force). A hard component's colouring is fixed up to its phase, so a soft
//!    pair only decides the phase of a free component (no anchor, no forced mask). Free
//!    components are revisited in index order; one flips only when that strictly raises the
//!    number of its soft pairs on opposite masks, counted against everything else as it stands.
//!    Each flip raises the total, so the passes end (capped at 8). A hard constraint is never
//!    traded: a component flips whole. With prioritised separators
//!    ([`decompose_weighted`], pairs from [`crate::masks_sep`]) the count is lexicographic: a
//!    component flips when the strongest priority the flip changes gains, whatever it costs a
//!    weaker one.
//!
//! Same input, same output: polygons and edges are numbered by shape order, every walk is in
//! index order, and nothing depends on hashing.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use vyges_layout::edges::{ring_edges, Axis, Edge};

use vyges_layout::geom::Rect;

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
    /// Soft pairs given (between two coloured shapes of different polygons), and how many end on
    /// opposite masks.
    pub soft_pairs: usize,
    pub soft_met: usize,
    /// Free components the long-range separators flipped.
    pub soft_flips: usize,
    /// Per separator priority: (pairs given, pairs on opposite masks).
    pub soft_by_priority: BTreeMap<u8, (usize, usize)>,
}

/// Passes of the separator search over the free components (each flip raises the met pairs,
/// strongest priority first, so the search ends; this caps it).
const SOFT_PASSES: usize = 8;

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
    decompose_soft(shapes, pairs, &[], anchor)
}

/// [`decompose`], then the long-range separators: `soft` pairs prefer opposite masks (step 4),
/// all at one priority.
#[must_use]
pub fn decompose_soft(shapes: &[MaskShape], pairs: &[(usize, usize)], soft: &[(usize, usize)], anchor: &[bool]) -> Decomposition {
    let soft: Vec<(usize, usize, u8)> = soft.iter().map(|&(a, b)| (a, b, 0)).collect();
    decompose_weighted(shapes, pairs, &soft, anchor)
}

/// [`decompose`], then prioritised separators: `soft` pairs `(a, b, priority)` prefer opposite
/// masks, a lower priority number being the stronger preference (step 4, lexicographic).
#[must_use]
pub fn decompose_weighted(
    shapes: &[MaskShape],
    pairs: &[(usize, usize)],
    soft: &[(usize, usize, u8)],
    anchor: &[bool],
) -> Decomposition {
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
    // free components (phase from preference only): (polygons, phase)
    let mut free: Vec<(Vec<usize>, u8)> = Vec::new();
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
        let is_free = phases.is_empty();
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
            for &k in &members[p] {
                out.mask[k] = Some(m);
            }
        }
        if is_free {
            free.push((comp, phase));
        }
    }
    // 4. separators: soft pairs decide the phases of free components, priority by priority
    let eff = |out: &Decomposition, k: usize| out.mask[k].unwrap_or(shapes[k].mask);
    let soft: Vec<(usize, usize, u8)> =
        soft.iter().copied().filter(|&(a, b, _)| take[a] && take[b] && poly[a] != poly[b]).collect();
    if !soft.is_empty() && !free.is_empty() {
        let mut comp_of: BTreeMap<usize, usize> = BTreeMap::new();
        for (c, (polys, _)) in free.iter().enumerate() {
            for p in polys {
                for &k in &members[p] {
                    comp_of.insert(k, c);
                }
            }
        }
        // per free component, its soft pairs as (own shape, other shape, priority), other outside it
        let mut touch: Vec<Vec<(usize, usize, u8)>> = vec![Vec::new(); free.len()];
        for &(a, b, pr) in &soft {
            let (ca, cb) = (comp_of.get(&a), comp_of.get(&b));
            if ca.is_some() && ca == cb {
                continue;
            }
            if let Some(&c) = ca {
                touch[c].push((a, b, pr));
            }
            if let Some(&c) = cb {
                touch[c].push((b, a, pr));
            }
        }
        for _ in 0..SOFT_PASSES {
            let mut changed = false;
            for c in 0..free.len() {
                // per priority: pairs a flip would meet minus pairs it would break
                let mut gain: BTreeMap<u8, i64> = BTreeMap::new();
                for &(a, b, pr) in &touch[c] {
                    let (ma, mb) = (eff(&out, a), eff(&out, b));
                    if ma == 0 || mb == 0 {
                        continue;
                    }
                    *gain.entry(pr).or_default() += if ma != mb { -1 } else { 1 };
                }
                // flip when the strongest priority it changes gains
                if gain.values().find(|&&g| g != 0).is_some_and(|&g| g > 0) {
                    changed = true;
                    out.soft_flips += 1;
                    free[c].1 = 3 - free[c].1;
                    for p in &free[c].0 {
                        for &k in &members[p] {
                            out.mask[k] = out.mask[k].map(|m| 3 - m);
                        }
                    }
                }
            }
            if !changed {
                break;
            }
        }
    }
    for &(a, b, pr) in &soft {
        let (ma, mb) = (eff(&out, a), eff(&out, b));
        if ma != 0 && mb != 0 {
            out.soft_pairs += 1;
            out.soft_met += usize::from(ma != mb);
            let e = out.soft_by_priority.entry(pr).or_default();
            e.0 += 1;
            e.1 += usize::from(ma != mb);
        }
    }
    for p in members.keys() {
        if members[p].iter().any(|&k| out.mask[k].is_some_and(|m| m != shapes[k].mask)) {
            out.flipped += 1;
        }
    }
    out
}

// ---- line-end windows as hard constraints ------------------------------------------------------

/// A same-mask line-end rule, in DBU, as a sign-off deck states it: a **line end** is a boundary
/// edge of a polygon no longer than `eol_max` whose two corners are convex and whose two
/// neighbouring edges are at least `adj_min` long; its window reaches `space` outward from the
/// edge, widened by `ext` on each side. With `to_eol` the rule is broken by another line end
/// facing it (itself widened by `ext`, closer than `space`); otherwise by any shape of the layer
/// in the open window. `len_lt` / `len_ge` keep only line ends shorter than / at least that long.
///
/// 🔑 Two polygons that break such a rule on one mask must take opposite masks, as two closer
/// than the same-mask spacing must. The window reaches where plain spacing does not: two line
/// ends on adjacent tracks, offset along the track, are further apart than the same-mask spacing
/// corner to corner and still inside each other's widened windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EolWindow {
    pub space: i32,
    pub eol_max: i32,
    pub adj_min: i32,
    pub ext: i32,
    pub to_eol: bool,
    pub len_lt: Option<i32>,
    pub len_ge: Option<i32>,
}

impl EolWindow {
    /// `LAYER:SPACE,EOLMAX,ADJMIN,EXT[,toeol][,lenlt=N][,lenge=N]`, DBU.
    pub fn parse(spec: &str) -> Result<(String, EolWindow), String> {
        let (layer, rest) = spec.split_once(':').ok_or_else(|| format!("{spec}: wants LAYER:SPACE,EOLMAX,ADJMIN,EXT[,...]"))?;
        let mut it = rest.split(',');
        let mut num = |what: &str| -> Result<i32, String> {
            let v = it.next().ok_or_else(|| format!("{spec}: no {what}"))?;
            v.trim().parse().map_err(|_| format!("{spec}: {what} not a DBU value: {v}"))
        };
        let (space, eol_max, adj_min, ext) = (num("SPACE")?, num("EOLMAX")?, num("ADJMIN")?, num("EXT")?);
        let mut w = EolWindow { space, eol_max, adj_min, ext, to_eol: false, len_lt: None, len_ge: None };
        for o in it {
            let o = o.trim();
            let val = |v: &str| v.parse::<i32>().map_err(|_| format!("{spec}: not a DBU value: {v}"));
            if o == "toeol" {
                w.to_eol = true;
            } else if let Some(v) = o.strip_prefix("lenlt=") {
                w.len_lt = Some(val(v)?);
            } else if let Some(v) = o.strip_prefix("lenge=") {
                w.len_ge = Some(val(v)?);
            } else {
                return Err(format!("{spec}: unknown option {o}"));
            }
        }
        if layer.is_empty() || space <= 0 || eol_max <= 0 || adj_min < 0 || ext < 0 {
            return Err(format!("{spec}: needs a layer, SPACE and EOLMAX > 0, ADJMIN and EXT >= 0"));
        }
        Ok((layer.to_string(), w))
    }
}

fn outward(e: &Edge) -> (i32, i32) {
    ((e.b.1 - e.a.1).signum(), -(e.b.0 - e.a.0).signum())
}

fn edge_box(e: &Edge) -> Rect {
    Rect { x0: e.a.0.min(e.b.0), y0: e.a.1.min(e.b.1), x1: e.a.0.max(e.b.0), y1: e.a.1.max(e.b.1) }
}

/// The line ends of the union of `rects` (see [`EolWindow`]).
pub fn line_ends(rects: &[Rect], w: &EolWindow) -> Vec<Edge> {
    let mut out = Vec::new();
    // the contour tracer wants tiles that do not overlap: union first
    let tiles = vyges_layout::boolean::boolean(rects, &[], vyges_layout::boolean::Op::Or);
    for ring in vyges_layout::contour::trace_contours(&tiles) {
        let ring = ring_edges(&ring);
        let n = ring.len();
        if n < 4 {
            continue;
        }
        let turn = |p: &Edge, q: &Edge| -> i64 {
            let (ux, uy) = (i64::from(p.b.0 - p.a.0), i64::from(p.b.1 - p.a.1));
            let (vx, vy) = (i64::from(q.b.0 - q.a.0), i64::from(q.b.1 - q.a.1));
            ux * vy - uy * vx
        };
        for i in 0..n {
            let (p, e, q) = (&ring[(i + n - 1) % n], &ring[i], &ring[(i + 1) % n]);
            let len = e.len();
            if len <= i64::from(w.eol_max)
                && p.len() >= i64::from(w.adj_min)
                && q.len() >= i64::from(w.adj_min)
                && turn(p, e) > 0
                && turn(e, q) > 0
                && w.len_lt.is_none_or(|v| len < i64::from(v))
                && w.len_ge.is_none_or(|v| len >= i64::from(v))
            {
                out.push(*e);
            }
        }
    }
    out
}

/// The window of line end `e`: `space` outward, widened by `ext` along it.
pub fn eol_window(e: &Edge, space: i32, ext: i32) -> Rect {
    let (ox, oy) = outward(e);
    let b = edge_box(e);
    if e.axis() == Axis::Horizontal {
        let (y0, y1) = if oy > 0 { (b.y0, b.y0 + space) } else { (b.y0 - space, b.y0) };
        Rect { x0: b.x0 - ext, y0, x1: b.x1 + ext, y1 }
    } else {
        let (x0, x1) = if ox > 0 { (b.x0, b.x0 + space) } else { (b.x0 - space, b.x0) };
        Rect { x0, y0: b.y0 - ext, x1, y1: b.y1 + ext }
    }
}

/// How far `r` lies outward of line end `e` (0 when it reaches back over the edge).
fn eol_gap(e: &Edge, r: &Rect) -> i32 {
    let at = if e.axis() == Axis::Horizontal { e.a.1 } else { e.a.0 };
    let v = match outward(e) {
        (1, _) => r.x0 - at,
        (-1, _) => at - r.x1,
        (_, 1) => r.y0 - at,
        _ => at - r.y1,
    };
    v.max(0)
}

/// Does line end `f` (facing `e`) break `w` against `e`: closer than `space` across the gap, and
/// the two, each widened by `ext`, overlapping along the edge.
fn ends_face(e: &Edge, f: &Edge, w: &EolWindow) -> bool {
    let (oe, of) = (outward(e), outward(f));
    if e.axis() != f.axis() || of != (-oe.0, -oe.1) {
        return false;
    }
    let g = eol_gap(e, &edge_box(f));
    let (we, wf) = (eol_window(e, 0, w.ext), eol_window(f, 0, w.ext));
    let along = if e.axis() == Axis::Horizontal {
        we.x0.max(wf.x0) < we.x1.min(wf.x1)
    } else {
        we.y0.max(wf.y0) < we.y1.min(wf.y1)
    };
    g > 0 && g < w.space && along
}

/// The shape of a polygon `members` that holds line end `e` (its midpoint), so a frozen
/// polygon's constraint lands on the shape that is close.
fn holder(shapes: &[MaskShape], members: &[usize], e: &Edge) -> usize {
    let (mx, my) = ((e.a.0 + e.b.0) / 2, (e.a.1 + e.b.1) / 2);
    *members
        .iter()
        .find(|&&k| {
            let r = shapes[k].rect;
            r[0] <= mx && mx <= r[2] && r[1] <= my && my <= r[3]
        })
        .unwrap_or(&members[0])
}

/// Hard OPPOSITE pairs from line-end windows: `(i, j)`, `i < j`, shape indices, one pair per
/// two polygons a rule of `windows` puts in conflict on one mask. Polygons as [`polygons`] forms
/// them; a polygon's own ends never constrain it.
pub fn eol_pairs(shapes: &[MaskShape], windows: &[EolWindow]) -> Vec<(usize, usize)> {
    if windows.is_empty() || shapes.len() < 2 {
        return Vec::new();
    }
    let poly = polygons(shapes, &vec![true; shapes.len()]);
    let mut members: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (k, &p) in poly.iter().enumerate() {
        members.entry(p).or_default().push(k);
    }
    // every point of a window lies within hypot(space, ext) of the end it widens, and a facing
    // end is widened by ext again, so two polygons in conflict have shapes at least that close
    let reach = windows
        .iter()
        .map(|w| {
            let across = if w.to_eol { 2 * w.ext } else { w.ext };
            f64::from(w.space).hypot(f64::from(across)).ceil() as i32 + 1
        })
        .max()
        .unwrap_or(0);
    let mut near: BTreeSet<(usize, usize)> = BTreeSet::new();
    for (i, j, _) in crate::masks::conflict_edges(shapes, reach) {
        let (p, q) = (poly[i], poly[j]);
        if p != q {
            near.insert((p, q));
            near.insert((q, p));
        }
    }
    let mut ends: BTreeMap<(usize, usize), Vec<Edge>> = BTreeMap::new();
    let mut ends_of = |p: usize, wi: usize| -> Vec<Edge> {
        ends.entry((p, wi))
            .or_insert_with(|| {
                let rects: Vec<Rect> = members[&p]
                    .iter()
                    .map(|&k| {
                        let r = shapes[k].rect;
                        Rect { x0: r[0], y0: r[1], x1: r[2], y1: r[3] }
                    })
                    .collect();
                line_ends(&rects, &windows[wi])
            })
            .clone()
    };
    let mut out: BTreeSet<(usize, usize)> = BTreeSet::new();
    for (wi, w) in windows.iter().enumerate() {
        for &(p, q) in &near {
            for e in ends_of(p, wi) {
                let hit = if w.to_eol {
                    ends_of(q, wi).iter().find(|f| ends_face(&e, f, w)).map(|f| holder(shapes, &members[&q], f))
                } else {
                    let win = eol_window(&e, w.space, w.ext);
                    members[&q].iter().copied().find(|&k| {
                        let r = shapes[k].rect;
                        win.intersects(&Rect { x0: r[0], y0: r[1], x1: r[2], y1: r[3] })
                    })
                };
                if let Some(j) = hit {
                    let i = holder(shapes, &members[&p], &e);
                    out.insert((i.min(j), i.max(j)));
                }
            }
        }
    }
    out.into_iter().collect()
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

    /// Rule (long-range separators, step 4): a soft pair asks two free components for opposite
    /// masks. Two lone wires on one mask, a soft pair apart: the first visited (the lower) flips,
    /// which meets the pair. Without the soft pair nothing moves; the soft pair is never a hard
    /// pair, so `decompose` is `decompose_soft` with none.
    #[test]
    fn a_soft_pair_splits_two_free_components() {
        let s = vec![sh("a", [0, 0, 1000, 32], 1), sh("b", [0, 200, 1000, 232], 1)];
        let d = decompose_soft(&s, &[], &[(0, 1)], &[false; 2]);
        assert_eq!(masks(&d), vec![2, 1]);
        assert_eq!((d.soft_pairs, d.soft_met, d.soft_flips, d.flipped), (1, 1, 1, 1));
        assert_eq!(decompose_soft(&s, &[], &[], &[false; 2]), decompose(&s, &[], &[false; 2]));
        assert_eq!(masks(&decompose(&s, &[], &[false; 2])), vec![1, 1]);
    }

    /// Rule (step 4 asks, never forces): an anchored shape never flips for a soft pair; the free
    /// neighbour moves instead. A hard component flips whole, so its hard pair stays split; a
    /// component flips only when it meets strictly more soft pairs (a tie keeps the preference).
    #[test]
    fn soft_pairs_never_flip_an_anchor_or_break_a_hard_pair() {
        // a anchored on 1, b free on 1, soft a-b: b takes 2
        let s = vec![sh("a", [0, 0, 10, 10], 1), sh("b", [0, 100, 10, 110], 1)];
        let d = decompose_soft(&s, &[], &[(0, 1)], &[true, false]);
        assert_eq!(masks(&d), vec![1, 2]);
        // both anchored on 1: the soft pair stays unmet, nothing flips
        let d = decompose_soft(&s, &[], &[(0, 1)], &[true, true]);
        assert_eq!(masks(&d), vec![1, 1]);
        assert_eq!((d.soft_pairs, d.soft_met, d.soft_flips), (1, 0, 0));
        // hard pair b-c (b 1, c 2 by parity), anchor a on 1 soft-near b: the component flips whole
        let s = vec![sh("a", [0, 0, 10, 10], 1), sh("b", [0, 100, 10, 110], 1), sh("c", [0, 120, 10, 130], 2)];
        let d = decompose_soft(&s, &[(1, 2)], &[(0, 1)], &[true, false, false]);
        assert_eq!(masks(&d), vec![1, 2, 1]);
        // the same with a second soft pair a-c: one met either way, a tie keeps parity
        let d = decompose_soft(&s, &[(1, 2)], &[(0, 1), (0, 2)], &[true, false, false]);
        assert_eq!(masks(&d), vec![1, 1, 2]);
        assert_eq!((d.soft_pairs, d.soft_met, d.soft_flips), (2, 1, 0));
        // a soft pair inside one component (here a hard pair too) decides nothing
        let d = decompose_soft(&s, &[(1, 2)], &[(1, 2)], &[false; 3]);
        assert_eq!(d.soft_flips, 0);
        // two polygons the hard pairs put on one mask (a-x-b), soft a-b: flipping the component
        // cannot meet it, so it must not flip (nor oscillate)
        let s = vec![sh("a", [0, 0, 10, 10], 1), sh("x", [0, 20, 10, 30], 2), sh("b", [0, 40, 10, 50], 1)];
        let d = decompose_soft(&s, &[(0, 1), (1, 2)], &[(0, 2)], &[false; 3]);
        assert_eq!((masks(&d), d.soft_flips, d.soft_met), (vec![1, 2, 1], 0, 0));
    }

    /// Rule (prioritised separators): the count is lexicographic, strongest priority (lowest
    /// number) first. x (free, mask 1) has one strong pair to a (anchored 1) and two weak pairs
    /// to b and c (anchored 2): a plain count keeps x on 1 (two met against one), the priorities
    /// put it on 2. With the priorities swapped it stays on 1.
    #[test]
    fn a_stronger_separator_outweighs_any_number_of_weaker_ones() {
        let s = vec![
            sh("x", [0, 0, 10, 10], 1),
            sh("a", [0, 100, 10, 110], 1),
            sh("b", [0, -110, 10, -100], 2),
            sh("c", [100, 0, 110, 10], 2),
        ];
        let anch = [false, true, true, true];
        let d = decompose_soft(&s, &[], &[(0, 1), (0, 2), (0, 3)], &anch);
        assert_eq!(masks(&d)[0], 1);
        let d = decompose_weighted(&s, &[], &[(0, 1, 2), (0, 2, 6), (0, 3, 6)], &anch);
        assert_eq!(masks(&d)[0], 2);
        assert_eq!(d.soft_by_priority, BTreeMap::from([(2, (1, 1)), (6, (2, 0))]));
        let d = decompose_weighted(&s, &[], &[(0, 1, 6), (0, 2, 2), (0, 3, 2)], &anch);
        assert_eq!(masks(&d)[0], 1);
        assert_eq!(d.soft_by_priority, BTreeMap::from([(2, (2, 2)), (6, (1, 0))]));
    }

    /// Rule (step 4 is a local search to a fixpoint): three free wires in a row, all on one
    /// mask, soft pairs between neighbours: the passes end with every pair met (1 2 1 or 2 1 2).
    #[test]
    fn soft_pairs_reach_a_fixpoint() {
        let s = vec![sh("a", [0, 0, 10, 10], 1), sh("b", [0, 100, 10, 110], 1), sh("c", [0, 200, 10, 210], 1)];
        let d = decompose_soft(&s, &[], &[(0, 1), (1, 2)], &[false; 3]);
        assert_eq!(d.soft_met, 2, "{:?}", masks(&d));
        let m = masks(&d);
        assert!(m[0] != m[1] && m[1] != m[2]);
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

    // ---- line-end windows -------------------------------------------------------------------

    /// The deck's M2 same-mask line-end-to-line-end window and its plain line-end window
    /// (generic values here, DBU = 1 nm): end edge no longer than 80, neighbours at least 20.
    const S3: EolWindow = EolWindow { space: 80, eol_max: 80, adj_min: 20, ext: 32, to_eol: true, len_lt: None, len_ge: None };
    const S4: EolWindow = EolWindow { space: 70, eol_max: 80, adj_min: 20, ext: 70, to_eol: false, len_lt: None, len_ge: None };

    /// The counterexample a decomposed layout left on M2: two 32-wide wires on adjacent tracks
    /// (pitch 64), a's right end at x 5075 (y 7424..7456) and b's left end at x 5143
    /// (y 7360..7392), both on one mask. 68 apart along the track and 32 across: corner to
    /// corner ~75, clear of a 66 same-mask spacing, so no spacing pair splits them.
    fn counterexample(b_dx: i32, b_dy: i32) -> Vec<MaskShape> {
        vec![sh("a", [4000, 7424, 5075, 7456], 1), sh("b", [5143 + b_dx, 7360 + b_dy, 6200 + b_dx, 7392 + b_dy], 1)]
    }

    /// Rule (deck line-end-to-line-end window): a line end's window reaches `space` outward,
    /// widened by `ext` along the edge; another line end facing it, itself widened by `ext`,
    /// inside that window breaks the rule on one mask. Here a's end widened is y 7392..7488 and
    /// b's is 7328..7424: they overlap by 32, 68 < 80 apart, so a and b must take opposite masks.
    #[test]
    fn the_m2_line_end_counterexample_is_a_hard_pair() {
        let s = counterexample(0, 0);
        assert!(crate::masks::conflict_edges(&s, 66).is_empty(), "plain spacing does not see it");
        assert_eq!(eol_pairs(&s, &[S3]), vec![(0, 1)]);
        assert_eq!(eol_pairs(&s, &[S4]), vec![(0, 1)], "b's end lies in a's widened window too");
        let d = decompose(&s, &eol_pairs(&s, &[S3, S4]), &[false, false]);
        assert_eq!(masks(&d), vec![1, 2]);
        // the windows are open: at exactly `space` apart nothing is broken
        assert!(eol_pairs(&counterexample(12, 0), &[S3]).is_empty(), "80 apart");
        assert_eq!(eol_pairs(&counterexample(11, 0), &[S3]), vec![(0, 1)], "79 apart");
        // one more track down, the widened ends no longer overlap (7264..7360 vs 7392..7488)
        assert!(eol_pairs(&counterexample(0, -64), &[S3]).is_empty());
        // the widened ends just touching (b's widened top at 7392): no overlap along the edge
        assert!(eol_pairs(&counterexample(0, -32), &[S3]).is_empty());
        assert_eq!(eol_pairs(&counterexample(0, -31), &[S3]), vec![(0, 1)]);
        // the pair names the shape that holds the end (a frozen polygon's constraint lands on
        // the shape that is close), not the polygon's first shape
        let split = vec![sh("a", [4000, 7424, 4500, 7456], 1), sh("a", [4500, 7424, 5075, 7456], 1), sh("b", [5143, 7360, 6200, 7392], 1)];
        assert_eq!(eol_pairs(&split, &[S3]), vec![(1, 2)]);
        // ends that point the same way do not face
        let same_way = vec![sh("a", [4000, 7424, 5075, 7456], 1), sh("b", [4100, 7360, 5143, 7392], 1)];
        assert!(eol_pairs(&same_way, &[S3]).is_empty());
    }

    /// Rule (what a line end is): an edge no longer than `eol_max` with two convex corners and
    /// neighbours at least `adj_min` long, on the merged polygon. A wire wider than `eol_max`
    /// has no line end; a stub of the same net flush with a's end makes that edge too long; a
    /// neighbour shorter than `adj_min` disqualifies it.
    #[test]
    fn only_line_ends_of_the_merged_polygon_open_a_window() {
        let wide = vec![sh("a", [4000, 7400, 5075, 7481], 1), sh("b", [5143, 7360, 6200, 7392], 1)];
        assert!(eol_pairs(&wide, &[S3]).is_empty(), "a's end is 81 long");
        let mut stub = counterexample(0, 0);
        stub.push(sh("a", [5043, 7424, 5075, 7600], 1));
        assert!(eol_pairs(&stub, &[S3]).is_empty(), "a's right edge is now 176 long");
        // the stub is part of a's polygon: the pair (if any) never joins a to itself
        assert!(eol_pairs(&stub, &[S4]).iter().all(|&(i, j)| (i, j) != (0, 2)));
        let short = vec![sh("a", [5056, 7424, 5075, 7456], 1), sh("b", [5143, 7360, 6200, 7392], 1)];
        assert!(eol_pairs(&short, &[S3]).is_empty(), "a's neighbours are 19 long");
        let lt = EolWindow { len_lt: Some(32), ..S3 };
        assert!(eol_pairs(&counterexample(0, 0), &[lt]).is_empty(), "both ends are 32, not < 32");
        let ge = EolWindow { len_ge: Some(32), ..S3 };
        assert_eq!(eol_pairs(&counterexample(0, 0), &[ge]), vec![(0, 1)]);
    }

    /// Rule (plain line-end window): any shape of the layer in the open window, not only a line
    /// end. A wide wire (no line end of its own) 50 beyond a's end and 40 across is in S4's window
    /// (reach 70, widened 70), not in S3's (it needs a facing end).
    #[test]
    fn a_plain_window_takes_any_shape() {
        let s = vec![sh("a", [4000, 7424, 5075, 7456], 1), sh("w", [5125, 7200, 5300, 7384], 1)];
        assert!(eol_pairs(&s, &[S3]).is_empty());
        assert_eq!(eol_pairs(&s, &[S4]), vec![(0, 1)]);
        // past the reach
        let far = vec![sh("a", [4000, 7424, 5075, 7456], 1), sh("w", [5145, 7200, 5300, 7384], 1)];
        assert!(eol_pairs(&far, &[S4]).is_empty());
    }

    /// The limits are inclusive: an end exactly `eol_max` long, with neighbours exactly
    /// `adj_min` long, is a line end.
    #[test]
    fn a_line_end_at_the_limits_is_one() {
        let at_max = vec![sh("a", [4000, 7400, 5075, 7480], 1), sh("b", [5143, 7360, 6200, 7392], 1)];
        assert_eq!(eol_pairs(&at_max, &[S3]), vec![(0, 1)], "a's end is 80 long");
        let at_adj = vec![sh("a", [5055, 7424, 5075, 7456], 1), sh("b", [5143, 7360, 6200, 7392], 1)];
        assert_eq!(eol_pairs(&at_adj, &[S3]), vec![(0, 1)], "a's neighbours are 20 long");
    }

    /// The same counterexample on a vertical layer (x and y exchanged): the window and the
    /// overlap along the edge are taken on the other axis.
    #[test]
    fn the_counterexample_turned_a_quarter_is_a_hard_pair_too() {
        let t = |s: Vec<MaskShape>| -> Vec<MaskShape> {
            s.into_iter().map(|m| MaskShape { rect: [m.rect[1], m.rect[0], m.rect[3], m.rect[2]], ..m }).collect()
        };
        assert_eq!(eol_pairs(&t(counterexample(0, 0)), &[S3]), vec![(0, 1)]);
        assert_eq!(eol_pairs(&t(counterexample(0, 0)), &[S4]), vec![(0, 1)]);
        assert!(eol_pairs(&t(counterexample(0, -32)), &[S3]).is_empty());
        assert_eq!(eol_pairs(&t(counterexample(0, -31)), &[S3]), vec![(0, 1)]);
        // S4's window widened along the edge: w sits beside a's end, past it by 50 and 60 to the
        // side of the edge (inside ext 70, outside a window not widened)
        let side = vec![sh("a", [4000, 7424, 5075, 7456], 1), sh("w", [5125, 7516, 5300, 7600], 1)];
        assert_eq!(eol_pairs(&t(side.clone()), &[S4]), vec![(0, 1)]);
        assert_eq!(eol_pairs(&side, &[S4]), vec![(0, 1)]);
    }

    /// Line ends are taken on the union: overlapping shapes of one net (a wire and the via
    /// landing at its end) are one outline, and the end is the outline's.
    #[test]
    fn line_ends_are_of_the_union_of_overlapping_shapes() {
        let r = |x0, y0, x1, y1| Rect { x0, y0, x1, y1 };
        let ends = line_ends(&[r(4000, 7424, 5075, 7456), r(5000, 7424, 5075, 7456)], &S3);
        assert_eq!(ends.len(), 2, "{ends:?}");
        assert!(ends.iter().any(|e| e.a.0 == 5075 && e.b.0 == 5075));
        // a landing wider than the wire: the wire's end is gone, the landing's is 40 + its sides
        let ends = line_ends(&[r(4000, 7424, 5075, 7456), r(5035, 7416, 5075, 7464)], &S3);
        assert!(ends.iter().all(|e| e.a.0 != 5075 || e.len() == 48), "{ends:?}");
    }

    /// A polygon's own line ends never constrain it: a U of one net whose shorter prong's end
    /// window holds the longer prong is one piece of metal, not a pair.
    #[test]
    fn a_polygons_own_ends_are_no_pair() {
        let u = vec![sh("c", [0, 0, 1050, 32], 1), sh("c", [0, 96, 1000, 128], 1), sh("c", [0, 0, 32, 128], 1)];
        assert!(eol_pairs(&u, &[S4]).is_empty());
        // the same prongs as two nets are a pair
        let two = vec![sh("c", [0, 0, 1050, 32], 1), sh("d", [100, 96, 1000, 128], 1)];
        assert_eq!(eol_pairs(&two, &[S4]), vec![(0, 1)]);
    }

    #[test]
    fn an_eol_window_parses_from_its_spec() {
        assert_eq!(EolWindow::parse("M2:80,80,20,32,toeol"), Ok(("M2".to_string(), S3)));
        assert_eq!(EolWindow::parse("M1:70,80,20,16,lenlt=42").unwrap().1, EolWindow { space: 70, ext: 16, to_eol: false, len_lt: Some(42), ..S3 });
        assert_eq!(EolWindow::parse("M1:67,80,20,16,lenge=42").unwrap().1.len_ge, Some(42));
        for bad in ["M2", "M2:80,80,20", "M2:80,80,20,x", ":80,80,20,32", "M2:0,80,20,32", "M2:80,80,20,32,both"] {
            assert!(EolWindow::parse(bad).is_err(), "{bad}");
        }
    }
}
