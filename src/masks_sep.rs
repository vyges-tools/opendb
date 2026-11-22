// SPDX-License-Identifier: Apache-2.0
// © 2026 https://vyges.com. All Rights Reserved.

//! Prioritised long-range separators: the soft OPPOSITE pairs a sign-off decomposition is given
//! for colour balance, at several distances, each band at its own priority.
//!
//! A two-mask decomposition is asked (never forced) to put neighbouring shapes on opposite
//! masks, beyond the same-mask spacing, so that the masks alternate and the share of each mask
//! per window stays near one half. A deck states this as a list of separator layers, each an
//! edge-to-edge spacing band over one class of edges, each with a priority; a lower number is
//! the stronger preference. This module derives those pairs from the shapes; the colouring that
//! honours them is [`crate::masks_decomp::decompose_weighted`].
//!
//! The model (every reading below is pinned in a unit test):
//! - **Edges are the polygon's own boundary.** A side of a shape covered on its outer side by
//!   another shape of the same polygon (one net's touching metal) is not an edge there.
//! - **Edge classes**, per shape side, the shapes being the rectangles a routed wire is made of:
//!   - `major`: a side at least as long as the shape's other dimension (a wire's long sides);
//!   - `minor`: a side at most as long (a wire's ends); a square's sides are both;
//!   - `notend`: a side that is not a line end, a line end being a side no longer than `eolmax`
//!     whose neighbouring sides are at least `adjmin` long and which no metal of its own polygon
//!     continues (both corners convex);
//!   - `any`: every side.
//! - **A band** pairs two edges of its class that face each other (parallel, opposite outward
//!   normals, overlapping along their length), on different polygons, with the gap between them
//!   in `[lo, hi)`.
//! - **Shielded:** an edge measures only what it can see. Scanning outward from the edge, a
//!   shape is seen where no nearer shape (of any polygon) already covers that part of the
//!   edge's length; the scan stops when the edge's length is covered or the gap reaches the
//!   largest band. This is what makes a wide band mean "the next shape across", not "every shape
//!   within reach": two-colouring cannot alternate with a shape two tracks away and the one in
//!   between at once.
//!
//! Same input, same output: every walk is in index order and the result is sorted.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use vyges_layout::edges::{Axis, Edge};
use vyges_layout::geom::Rect;

use crate::masks::MaskShape;
use crate::masks_decomp::{line_ends, polygons, EolWindow};

/// Which edges a separator band measures (see the module notes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SepEdges {
    Any,
    Major,
    Minor,
    NotEnd { eol_max: i32, adj_min: i32 },
}

/// One separator band: edges of class `edges` facing each other with a gap in `[lo, hi)` DBU
/// prefer opposite masks, at `priority` (lower is stronger).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Separator {
    pub priority: u8,
    pub edges: SepEdges,
    pub lo: i32,
    pub hi: i32,
}

impl Separator {
    /// `LAYER:PRIORITY,EDGES,LO,HI[,eolmax=N][,adjmin=N]`, DBU; EDGES is `any`, `major`, `minor`
    /// or `notend` (which needs `eolmax` and `adjmin`).
    pub fn parse(spec: &str) -> Result<(String, Separator), String> {
        let (layer, rest) =
            spec.split_once(':').ok_or_else(|| format!("{spec}: wants LAYER:PRIORITY,EDGES,LO,HI[,eolmax=N,adjmin=N]"))?;
        let f: Vec<&str> = rest.split(',').map(str::trim).collect();
        if layer.is_empty() || f.len() < 4 {
            return Err(format!("{spec}: wants LAYER:PRIORITY,EDGES,LO,HI[,eolmax=N,adjmin=N]"));
        }
        let priority: u8 = f[0].parse().map_err(|_| format!("{spec}: PRIORITY not 0..255: {}", f[0]))?;
        let dbu = |what: &str, v: &str| v.parse::<i32>().map_err(|_| format!("{spec}: {what} not a DBU value: {v}"));
        let (lo, hi) = (dbu("LO", f[2])?, dbu("HI", f[3])?);
        let (mut eol_max, mut adj_min) = (None, None);
        for o in &f[4..] {
            if let Some(v) = o.strip_prefix("eolmax=") {
                eol_max = Some(dbu("eolmax", v)?);
            } else if let Some(v) = o.strip_prefix("adjmin=") {
                adj_min = Some(dbu("adjmin", v)?);
            } else {
                return Err(format!("{spec}: unknown option {o}"));
            }
        }
        let edges = match f[1] {
            "any" => SepEdges::Any,
            "major" => SepEdges::Major,
            "minor" => SepEdges::Minor,
            "notend" => match (eol_max, adj_min) {
                (Some(e), Some(a)) if e > 0 && a >= 0 => SepEdges::NotEnd { eol_max: e, adj_min: a },
                _ => return Err(format!("{spec}: notend needs eolmax=N > 0 and adjmin=N >= 0")),
            },
            other => return Err(format!("{spec}: EDGES is any, major, minor or notend, not {other}")),
        };
        if !matches!(edges, SepEdges::NotEnd { .. }) && (eol_max.is_some() || adj_min.is_some()) {
            return Err(format!("{spec}: eolmax/adjmin apply to notend only"));
        }
        if lo < 0 || hi <= lo {
            return Err(format!("{spec}: needs 0 <= LO < HI"));
        }
        Ok((layer.to_string(), Separator { priority, edges, lo, hi }))
    }
}

/// The four sides of a rectangle, by outward normal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Left,
    Right,
    Bottom,
    Top,
}

const SIDES: [Side; 4] = [Side::Left, Side::Right, Side::Bottom, Side::Top];

impl Side {
    fn opposite(self) -> Side {
        match self {
            Side::Left => Side::Right,
            Side::Right => Side::Left,
            Side::Bottom => Side::Top,
            Side::Top => Side::Bottom,
        }
    }
}

/// Side `s` of `r`: (line coordinate, span along it, length of the other dimension).
fn side_of(r: &[i32; 4], s: Side) -> (i32, (i32, i32), i32) {
    match s {
        Side::Left => (r[0], (r[1], r[3]), r[2] - r[0]),
        Side::Right => (r[2], (r[1], r[3]), r[2] - r[0]),
        Side::Bottom => (r[1], (r[0], r[2]), r[3] - r[1]),
        Side::Top => (r[3], (r[0], r[2]), r[3] - r[1]),
    }
}

/// How `q` lies in front of side `s` (at `line`): its gap outward (negative when it reaches back
/// over the line) and whether it reaches outward past the line at all.
fn front(q: &[i32; 4], s: Side, line: i32) -> Option<i32> {
    let (near, far) = match s {
        Side::Left => (line - q[2], line - q[0]),
        Side::Right => (q[0] - line, q[2] - line),
        Side::Bottom => (line - q[3], line - q[1]),
        Side::Top => (q[1] - line, q[3] - line),
    };
    (far > 0).then_some(near)
}

fn span_along(q: &[i32; 4], s: Side) -> (i32, i32) {
    match s {
        Side::Left | Side::Right => (q[1], q[3]),
        Side::Bottom | Side::Top => (q[0], q[2]),
    }
}

/// Sorted, disjoint open intervals.
#[derive(Debug, Clone, Default)]
struct Cover(Vec<(i32, i32)>);

impl Cover {
    fn add(&mut self, (a, b): (i32, i32)) {
        if a >= b {
            return;
        }
        let mut out = Vec::with_capacity(self.0.len() + 1);
        let (mut a, mut b) = (a, b);
        for &(x, y) in &self.0 {
            if y < a || x > b {
                out.push((x, y));
            } else {
                a = a.min(x);
                b = b.max(y);
            }
        }
        out.push((a, b));
        out.sort_unstable();
        self.0 = out;
    }
    /// Does `(a, b)` reach outside the cover?
    fn sees(&self, (a, b): (i32, i32)) -> bool {
        if a >= b {
            return false;
        }
        let mut at = a;
        for &(x, y) in &self.0 {
            if y <= at {
                continue;
            }
            if x > at {
                return true;
            }
            at = y;
            if at >= b {
                return false;
            }
        }
        at < b
    }
    fn covers(&self, span: (i32, i32)) -> bool {
        !self.sees(span)
    }
}

/// Per shape side: its length, the shape's other dimension, and the parts of it that its own
/// polygon covers on the outer side (not an edge there).
struct SideInfo {
    line: i32,
    len: i32,
    other: i32,
    own_cover: Cover,
}

/// Outward normal of a directed contour edge (solid on the left).
fn outward(e: &Edge) -> (i32, i32) {
    ((e.b.1 - e.a.1).signum(), -(e.b.0 - e.a.0).signum())
}

fn normal(s: Side) -> (i32, i32) {
    match s {
        Side::Left => (-1, 0),
        Side::Right => (1, 0),
        Side::Bottom => (0, -1),
        Side::Top => (0, 1),
    }
}

struct Ctx<'a> {
    shapes: &'a [MaskShape],
    poly: Vec<usize>,
    members: BTreeMap<usize, Vec<usize>>,
    sides: HashMap<(usize, u8), SideInfo>,
    ends: HashMap<(usize, i32, i32), Vec<Edge>>,
}

impl Ctx<'_> {
    fn side(&mut self, k: usize, s: Side) -> &SideInfo {
        let (shapes, poly, members) = (self.shapes, &self.poly, &self.members);
        self.sides.entry((k, s as u8)).or_insert_with(|| {
            let r = shapes[k].rect;
            let (line, span, other) = side_of(&r, s);
            let mut own_cover = Cover::default();
            for &m in &members[&poly[k]] {
                if m == k {
                    continue;
                }
                let q = shapes[m].rect;
                if front(&q, s, line).is_some_and(|g| g <= 0) {
                    let (a, b) = span_along(&q, s);
                    own_cover.add((a.max(span.0), b.min(span.1)));
                }
            }
            SideInfo { line, len: span.1 - span.0, other, own_cover }
        })
    }

    /// Is side `s` of shape `k` (where it is an edge) part of a line end of its polygon?
    fn is_end(&mut self, k: usize, s: Side, eol_max: i32, adj_min: i32) -> bool {
        let p = self.poly[k];
        let (shapes, members) = (self.shapes, &self.members);
        let ends = self.ends.entry((p, eol_max, adj_min)).or_insert_with(|| {
            let rects: Vec<Rect> = members[&p]
                .iter()
                .map(|&m| {
                    let r = shapes[m].rect;
                    Rect { x0: r[0], y0: r[1], x1: r[2], y1: r[3] }
                })
                .collect();
            let w = EolWindow { space: 1, eol_max, adj_min, ext: 0, to_eol: false, len_lt: None, len_ge: None };
            line_ends(&rects, &w)
        });
        let r = shapes[k].rect;
        let (line, span, _) = side_of(&r, s);
        ends.iter().any(|e| {
            let horizontal = e.axis() == Axis::Horizontal;
            let (at, a, b) = if horizontal {
                (e.a.1, e.a.0.min(e.b.0), e.a.0.max(e.b.0))
            } else {
                (e.a.0, e.a.1.min(e.b.1), e.a.1.max(e.b.1))
            };
            outward(e) == normal(s) && at == line && a <= span.0 && span.1 <= b
        })
    }

    fn has(&mut self, edges: SepEdges, k: usize, s: Side) -> bool {
        let (len, other) = {
            let si = self.side(k, s);
            (si.len, si.other)
        };
        match edges {
            SepEdges::Any => true,
            SepEdges::Major => len >= other,
            SepEdges::Minor => len <= other,
            SepEdges::NotEnd { eol_max, adj_min } => !self.is_end(k, s, eol_max, adj_min),
        }
    }
}

/// The separator pairs of one layer: `(i, j, priority)`, `i < j` shape indices, one per two
/// shapes and band priority that see each other across a gap in a band of their class. Shapes
/// with `fixed && mask == 0` take no part but still shield.
#[must_use]
pub fn separator_pairs(shapes: &[MaskShape], seps: &[Separator]) -> Vec<(usize, usize, u8)> {
    let reach = seps.iter().map(|s| s.hi).max().unwrap_or(0);
    if reach <= 0 || shapes.len() < 2 {
        return Vec::new();
    }
    let take: Vec<bool> = shapes.iter().map(|s| !(s.fixed && s.mask == 0)).collect();
    let poly = polygons(shapes, &vec![true; shapes.len()]);
    let mut members: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (k, &p) in poly.iter().enumerate() {
        members.entry(p).or_default().push(k);
    }
    let mut cx = Ctx { shapes, poly, members, sides: HashMap::new(), ends: HashMap::new() };
    // grid index; a slab of the outward scan is one cell deep
    let cell = (reach / 16).max(1);
    let mut grid: HashMap<(i32, i32), Vec<usize>> = HashMap::new();
    for (k, sh) in shapes.iter().enumerate() {
        let r = sh.rect;
        for gx in r[0].div_euclid(cell)..=r[2].div_euclid(cell) {
            for gy in r[1].div_euclid(cell)..=r[3].div_euclid(cell) {
                grid.entry((gx, gy)).or_default().push(k);
            }
        }
    }
    let query = |x0: i32, y0: i32, x1: i32, y1: i32| -> Vec<usize> {
        let mut v: BTreeSet<usize> = BTreeSet::new();
        for gx in x0.div_euclid(cell)..=x1.div_euclid(cell) {
            for gy in y0.div_euclid(cell)..=y1.div_euclid(cell) {
                if let Some(c) = grid.get(&(gx, gy)) {
                    v.extend(c.iter().copied());
                }
            }
        }
        v.into_iter().collect()
    };
    let mut out: BTreeSet<(usize, usize, u8)> = BTreeSet::new();
    for k in (0..shapes.len()).filter(|&k| take[k]) {
        let r = shapes[k].rect;
        for s in SIDES {
            let (line, span, _) = side_of(&r, s);
            if span.1 <= span.0 {
                continue;
            }
            let mut my_bands: Vec<Separator> = Vec::new();
            for b in seps {
                if cx.has(b.edges, k, s) {
                    my_bands.push(*b);
                }
            }
            let my_reach = my_bands.iter().map(|b| b.hi).max().unwrap_or(0);
            if my_reach <= 0 {
                continue;
            }
            // only the boundary parts of the side measure
            let mut cover = cx.side(k, s).own_cover.clone();
            debug_assert_eq!(cx.side(k, s).line, line);
            if cover.covers(span) {
                continue;
            }
            let mut depth = 0i32;
            while depth < my_reach && !cover.covers(span) {
                let (d0, d1) = (depth, depth.saturating_add(cell));
                let (x0, y0, x1, y1) = match s {
                    Side::Left => (line - d1, span.0, line - d0, span.1),
                    Side::Right => (line + d0, span.0, line + d1, span.1),
                    Side::Bottom => (span.0, line - d1, span.1, line - d0),
                    Side::Top => (span.0, line + d0, span.1, line + d1),
                };
                // shapes whose gap falls in this slab (or that reach back over the line, in the first)
                let mut cand: Vec<(i32, usize)> = query(x0, y0, x1, y1)
                    .into_iter()
                    .filter(|&j| j != k)
                    .filter_map(|j| {
                        let q = shapes[j].rect;
                        let (a, b) = span_along(&q, s);
                        if a.max(span.0) >= b.min(span.1) {
                            return None;
                        }
                        let g = front(&q, s, line)?;
                        let in_slab = if depth == 0 { g < d1 } else { g >= d0 && g < d1 };
                        in_slab.then_some((g, j))
                    })
                    .collect();
                cand.sort_unstable();
                // one gap at a time: shapes at the same gap do not shield each other
                let mut i = 0;
                while i < cand.len() {
                    let g = cand[i].0;
                    let mut e = i;
                    while e < cand.len() && cand[e].0 == g {
                        e += 1;
                    }
                    let mut adds = Vec::new();
                    for &(_, j) in &cand[i..e] {
                        let q = shapes[j].rect;
                        let (a, b) = span_along(&q, s);
                        let vis = (a.max(span.0), b.min(span.1));
                        if g > 0 && take[j] && cx.poly[j] != cx.poly[k] && cover.sees(vis) {
                            for band in &my_bands {
                                if g >= band.lo && g < band.hi && cx.has(band.edges, j, s.opposite()) {
                                    out.insert((k.min(j), k.max(j), band.priority));
                                }
                            }
                        }
                        adds.push(vis);
                    }
                    for v in adds {
                        cover.add(v);
                    }
                    i = e;
                }
                depth = d1;
            }
        }
    }
    out.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(net: &str, r: [i32; 4]) -> MaskShape {
        MaskShape { net: net.into(), rect: r, mask: 1, fixed: false }
    }

    fn band(p: u8, edges: SepEdges, lo: i32, hi: i32) -> Separator {
        Separator { priority: p, edges, lo, hi }
    }

    /// Rule: a band pairs two facing edges of its class with the gap in [lo, hi): the bound lo
    /// is in, hi is out.
    #[test]
    fn a_band_is_closed_below_and_open_above() {
        let s = vec![sh("a", [0, 0, 1000, 20]), sh("b", [0, 70, 1000, 90])]; // gap 50
        assert_eq!(separator_pairs(&s, &[band(3, SepEdges::Major, 50, 80)]), vec![(0, 1, 3)]);
        assert!(separator_pairs(&s, &[band(3, SepEdges::Major, 0, 50)]).is_empty());
        assert!(separator_pairs(&s, &[band(3, SepEdges::Major, 51, 80)]).is_empty());
    }

    /// Rule: shielded. Three parallel wires: the outer two do not see each other through the
    /// middle one, so a wide band pairs neighbours only.
    #[test]
    fn a_shape_in_between_shields() {
        let s = vec![sh("a", [0, 0, 1000, 20]), sh("b", [0, 60, 1000, 80]), sh("c", [0, 120, 1000, 140])];
        assert_eq!(separator_pairs(&s, &[band(6, SepEdges::Any, 0, 1000)]), vec![(0, 1, 6), (1, 2, 6)]);
        // a shorter middle wire leaves part of the outer edges in view
        let s = vec![sh("a", [0, 0, 1000, 20]), sh("b", [0, 60, 400, 80]), sh("c", [0, 120, 1000, 140])];
        assert_eq!(separator_pairs(&s, &[band(6, SepEdges::Any, 0, 1000)]), vec![(0, 1, 6), (0, 2, 6), (1, 2, 6)]);
        // the shielding holds within one step of the outward scan too (gaps 10 and 30)
        let s = vec![sh("a", [0, 0, 1000, 20]), sh("b", [0, 30, 1000, 40]), sh("c", [0, 50, 1000, 60])];
        assert_eq!(separator_pairs(&s, &[band(6, SepEdges::Any, 0, 1000)]), vec![(0, 1, 6), (1, 2, 6)]);
    }

    /// Rule: major edges are long sides, minor edges are ends. Two wires end to end on one track
    /// face by their ends (minor), two side by side by their long sides (major).
    #[test]
    fn major_and_minor_classes() {
        let ends = vec![sh("a", [0, 0, 400, 20]), sh("b", [500, 0, 900, 20])];
        assert_eq!(separator_pairs(&ends, &[band(5, SepEdges::Minor, 0, 200)]), vec![(0, 1, 5)]);
        assert!(separator_pairs(&ends, &[band(3, SepEdges::Major, 0, 200)]).is_empty());
        let sides = vec![sh("a", [0, 0, 400, 20]), sh("b", [0, 100, 400, 120])];
        assert_eq!(separator_pairs(&sides, &[band(3, SepEdges::Major, 0, 200)]), vec![(0, 1, 3)]);
        assert!(separator_pairs(&sides, &[band(5, SepEdges::Minor, 0, 200)]).is_empty());
        // both edges must be of the class: a wire's end facing another wire's side is neither
        let t = vec![sh("a", [0, 0, 20, 400]), sh("b", [-200, 450, 200, 470])];
        assert!(separator_pairs(&t, &[band(3, SepEdges::Major, 0, 200)]).is_empty());
        assert!(separator_pairs(&t, &[band(5, SepEdges::Minor, 0, 200)]).is_empty());
        assert_eq!(separator_pairs(&t, &[band(6, SepEdges::Any, 0, 200)]), vec![(0, 1, 6)]);
    }

    /// Rule: a square's sides are both major and minor (its side is as long as its other
    /// dimension).
    #[test]
    fn a_square_is_major_and_minor() {
        let s = vec![sh("a", [0, 0, 20, 20]), sh("b", [60, 0, 80, 20])];
        assert_eq!(separator_pairs(&s, &[band(3, SepEdges::Major, 0, 80)]), vec![(0, 1, 3)]);
        assert_eq!(separator_pairs(&s, &[band(5, SepEdges::Minor, 0, 80)]), vec![(0, 1, 5)]);
    }

    /// Rule: shapes at one gap do not shield each other: a wire and its own via pad, flush on
    /// the facing side, are both seen.
    #[test]
    fn shapes_at_one_gap_are_all_seen() {
        let s = vec![sh("a", [0, 0, 1000, 20]), sh("b", [0, 60, 500, 80]), sh("b", [400, 60, 440, 80])];
        assert_eq!(separator_pairs(&s, &[band(6, SepEdges::Any, 0, 100)]), vec![(0, 1, 6), (0, 2, 6)]);
    }

    /// Rule: notend excludes a line end (short side, both corners convex, neighbours long
    /// enough); a wire's end that its own net's metal continues is not a line end.
    #[test]
    fn notend_drops_line_ends() {
        let ne = SepEdges::NotEnd { eol_max: 30, adj_min: 10 };
        let ends = vec![sh("a", [0, 0, 400, 20]), sh("b", [440, 0, 840, 20])];
        assert!(separator_pairs(&ends, &[band(2, ne, 0, 62)]).is_empty());
        // the same gap between long sides is a pair
        let sides = vec![sh("a", [0, 0, 400, 20]), sh("b", [0, 60, 400, 80])];
        assert_eq!(separator_pairs(&sides, &[band(2, ne, 0, 62)]), vec![(0, 1, 2)]);
        // an end that its own net's metal continues (a jog up along the same line) is no
        // longer a line end: it is part of a long edge
        let v = vec![sh("a", [0, 0, 400, 20]), sh("b", [440, 0, 460, 200])];
        assert!(separator_pairs(&v, &[band(2, ne, 0, 62)]).is_empty());
        let jog = vec![sh("a", [0, 0, 400, 20]), sh("a", [380, 0, 400, 200]), sh("b", [440, 0, 460, 200])];
        assert_eq!(separator_pairs(&jog, &[band(2, ne, 0, 62)]), vec![(0, 2, 2), (1, 2, 2)]);
    }

    /// Rule: a side covered by its own polygon is not an edge; touching shapes of one net never
    /// pair, and shapes of one polygon shield.
    #[test]
    fn own_polygon_is_not_a_neighbour() {
        let s = vec![sh("a", [0, 0, 400, 20]), sh("a", [0, 20, 400, 40]), sh("b", [0, 100, 400, 120])];
        assert_eq!(separator_pairs(&s, &[band(6, SepEdges::Any, 0, 200)]), vec![(1, 2, 6)]);
    }

    /// Rule: one pair per band priority: a pair in two bands of different priority is listed at
    /// both.
    #[test]
    fn a_pair_carries_each_band_it_meets() {
        let s = vec![sh("a", [0, 0, 1000, 20]), sh("b", [0, 70, 1000, 90])];
        let p = separator_pairs(&s, &[band(3, SepEdges::Major, 0, 80), band(6, SepEdges::Any, 0, 1000)]);
        assert_eq!(p, vec![(0, 1, 3), (0, 1, 6)]);
    }

    /// Rule: an unmasked fixed shape takes no part but still shields.
    #[test]
    fn an_unmasked_fixed_shape_shields() {
        let mut s = vec![sh("a", [0, 0, 1000, 20]), sh("p", [0, 60, 1000, 80]), sh("c", [0, 120, 1000, 140])];
        s[1].fixed = true;
        s[1].mask = 0;
        assert!(separator_pairs(&s, &[band(6, SepEdges::Any, 0, 1000)]).is_empty());
    }

    #[test]
    fn parse_forms() {
        let (l, s) = Separator::parse("M2:2,notend,0,62,eolmax=80,adjmin=20").unwrap();
        assert_eq!((l.as_str(), s), ("M2", band(2, SepEdges::NotEnd { eol_max: 80, adj_min: 20 }, 0, 62)));
        assert_eq!(Separator::parse("M2:4,major,500,2500").unwrap().1, band(4, SepEdges::Major, 500, 2500));
        for bad in ["M2:2,notend,0,62", "M2:2,major,62,62", "M2:2,wide,0,62", "M2:x,any,0,1", ":1,any,0,1", "M2:1,any,0,1,eolmax=3"] {
            assert!(Separator::parse(bad).is_err(), "{bad}");
        }
    }
}
