// SPDX-License-Identifier: Apache-2.0
//! The merged-shape model a design-rule check of the reference detailed router works on, for the
//! shapes of ONE layer (here: one mask of one layer). Built as drt's checker builds it
//! (`FlexGC_init.cpp`, `initNet`):
//!
//! 1. `initNet_pins_polygon`: per net, the layer's shapes are merged; each merged polygon (with
//!    its holes) is a pin.
//! 2. `initNet_pins_polygonEdges`: each pin's outer boundary and holes become edges, in boundary
//!    order, each linked to the edge before and after it. The outer boundary runs
//!    counter-clockwise (the interior is on an edge's left), a hole clockwise. An edge is fixed
//!    when the same edge (same start and end) bounds the merge of the net's fixed shapes alone.
//! 3. `initNet_pins_maxRectangles`: each pin's maximal rectangles (boost polygon
//!    `get_max_rectangles`); a rectangle is fixed when it is also a maximal rectangle of the
//!    net's fixed shapes alone.
//!
//! Shapes that touch only at a corner are separate pins. Coordinates are DBU; a rectangle is
//! `[x0, y0, x1, y1]`.

use std::collections::{BTreeMap, BTreeSet};

pub type Rect = [i32; 4];
pub type Point = (i32, i32);

/// Edge direction, with drt's `frDirEnum` values (S + N = W + E = `OPPOSITEDIR` = 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Dir {
    S = 2,
    W = 3,
    E = 4,
    N = 5,
}

/// One polygon edge (drt `gcSegment`): from `low` to `high` in boundary order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    pub low: Point,
    pub high: Point,
    pub fixed: bool,
    pub pin: usize,
    /// Indices into [`LayerModel::edges`].
    pub prev: usize,
    pub next: usize,
}

impl Edge {
    /// drt `gcSegment::getDir`: N/S when vertical (by the order of the ends), else E/W.
    #[must_use]
    pub fn dir(&self) -> Dir {
        if self.low.0 == self.high.0 {
            if self.low.1 < self.high.1 {
                Dir::N
            } else {
                Dir::S
            }
        } else if self.low.0 < self.high.0 {
            Dir::E
        } else {
            Dir::W
        }
    }

    #[must_use]
    pub fn len(&self) -> i32 {
        (self.high.0 - self.low.0).abs() + (self.high.1 - self.low.1).abs()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The edge as a (degenerate) rectangle (`gtl::set_points(rect, low, high)`).
    #[must_use]
    pub fn rect(&self) -> Rect {
        set_points(self.low, self.high)
    }

    fn vector(&self) -> Point {
        ((self.high.0 - self.low.0).signum(), (self.high.1 - self.low.1).signum())
    }
}

/// A maximal rectangle of a pin (drt `gcRect`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaxRect {
    pub rect: Rect,
    pub fixed: bool,
    pub pin: usize,
}

impl MaxRect {
    /// drt `gcRect::width`: the smaller side.
    #[must_use]
    pub fn width(&self) -> i32 {
        (self.rect[2] - self.rect[0]).min(self.rect[3] - self.rect[1])
    }
    /// drt `gcRect::length`: the larger side.
    #[must_use]
    pub fn length(&self) -> i32 {
        (self.rect[2] - self.rect[0]).max(self.rect[3] - self.rect[1])
    }
}

/// A merged polygon of one net.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    pub net: usize,
}

/// One input shape: its net (an index into the caller's net list), rectangle and whether it is
/// fixed (drt: instance and block pins, special wires) rather than routed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shape {
    pub net: usize,
    pub rect: Rect,
    pub fixed: bool,
}

/// The model of one layer: pins, their edges and their maximal rectangles, in net order then
/// pin order.
#[derive(Debug, Clone, Default)]
pub struct LayerModel {
    pub pins: Vec<Pin>,
    pub edges: Vec<Edge>,
    pub rects: Vec<MaxRect>,
    /// Per net, its fixed shapes (for `hasRoute`).
    pub fixed_shapes: BTreeMap<usize, Vec<Rect>>,
    /// Per net, its route (not fixed) shapes (drt `gcNet::getPolygons(layer, false)`, for the
    /// end-of-line check's `hasRoute`).
    pub route_shapes: BTreeMap<usize, Vec<Rect>>,
}

// ---- rectangle helpers (boost polygon / gtl semantics) -------------------------------------------

/// `gtl::set_points`: the rectangle spanned by two points.
#[must_use]
pub fn set_points(a: Point, b: Point) -> Rect {
    [a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1)]
}

/// `gtl::intersects` with touching counted (its default), and the closed-box `bgi::intersects`
/// of the checker's region queries.
#[must_use]
pub fn intersects(a: &Rect, b: &Rect) -> bool {
    a[0] <= b[2] && b[0] <= a[2] && a[1] <= b[3] && b[1] <= a[3]
}

/// `gtl::generalized_intersect`: per axis the overlap, or the gap between the two when apart.
#[must_use]
pub fn generalized_intersect(a: &Rect, b: &Rect) -> Rect {
    let axis = |alo: i32, ahi: i32, blo: i32, bhi: i32| {
        let (lo, hi) = (alo.max(blo), ahi.min(bhi));
        if lo <= hi {
            (lo, hi)
        } else {
            (hi, lo)
        }
    };
    let (x0, x1) = axis(a[0], a[2], b[0], b[2]);
    let (y0, y1) = axis(a[1], a[3], b[1], b[3]);
    [x0, y0, x1, y1]
}

/// `gtl::euclidean_distance(a, b, orient)` per axis: the gap along X (`horizontal`) or Y, 0 when
/// the projections overlap or touch.
#[must_use]
pub fn axis_distance(a: &Rect, b: &Rect, horizontal: bool) -> i32 {
    let (i, j) = if horizontal { (0, 2) } else { (1, 3) };
    (b[i] - a[j]).max(a[i] - b[j]).max(0)
}

/// `gtl::square_euclidean_distance` of two rectangles.
#[must_use]
pub fn square_distance(a: &Rect, b: &Rect) -> i64 {
    let dx = axis_distance(a, b, true) as i64;
    let dy = axis_distance(a, b, false) as i64;
    dx * dx + dy * dy
}

/// The area of the union of `rects` inside `clip`.
#[must_use]
pub fn union_area_in(rects: &[Rect], clip: &Rect) -> i64 {
    let clipped: Vec<Rect> = rects
        .iter()
        .filter_map(|r| {
            let c = [r[0].max(clip[0]), r[1].max(clip[1]), r[2].min(clip[2]), r[3].min(clip[3])];
            (c[0] < c[2] && c[1] < c[3]).then_some(c)
        })
        .collect();
    let g = Grid::new(&clipped);
    let mut a = 0i64;
    for i in 0..g.nx() {
        for j in 0..g.ny() {
            if g.covered(i, j) {
                a += g.cell_area(i, j);
            }
        }
    }
    a
}

// ---- the compressed cell grid ---------------------------------------------------------------------

/// Rectangles on the grid of all their x and y coordinates; a cell is covered when a rectangle
/// covers it.
struct Grid {
    xs: Vec<i32>,
    ys: Vec<i32>,
    cov: Vec<bool>,
}

impl Grid {
    fn new(rects: &[Rect]) -> Grid {
        let xs: Vec<i32> = rects.iter().flat_map(|r| [r[0], r[2]]).collect::<BTreeSet<_>>().into_iter().collect();
        let ys: Vec<i32> = rects.iter().flat_map(|r| [r[1], r[3]]).collect::<BTreeSet<_>>().into_iter().collect();
        let (nx, ny) = (xs.len().saturating_sub(1), ys.len().saturating_sub(1));
        let mut cov = vec![false; nx * ny];
        for r in rects.iter().filter(|r| r[0] < r[2] && r[1] < r[3]) {
            let i0 = xs.partition_point(|&x| x < r[0]);
            let i1 = xs.partition_point(|&x| x < r[2]);
            let j0 = ys.partition_point(|&y| y < r[1]);
            let j1 = ys.partition_point(|&y| y < r[3]);
            for i in i0..i1 {
                for j in j0..j1 {
                    cov[i * ny + j] = true;
                }
            }
        }
        Grid { xs, ys, cov }
    }
    fn nx(&self) -> usize {
        self.xs.len().saturating_sub(1)
    }
    fn ny(&self) -> usize {
        self.ys.len().saturating_sub(1)
    }
    fn covered(&self, i: usize, j: usize) -> bool {
        self.cov[i * self.ny() + j]
    }
    fn cell_area(&self, i: usize, j: usize) -> i64 {
        (self.xs[i + 1] - self.xs[i]) as i64 * (self.ys[j + 1] - self.ys[j]) as i64
    }

    /// Covered cells grouped into edge-connected components, each as a sorted cell list.
    fn components(&self) -> Vec<Vec<(usize, usize)>> {
        let (nx, ny) = (self.nx(), self.ny());
        let mut seen = vec![false; nx * ny];
        let mut out = Vec::new();
        for i in 0..nx {
            for j in 0..ny {
                if !self.covered(i, j) || seen[i * ny + j] {
                    continue;
                }
                let mut comp = Vec::new();
                let mut stack = vec![(i, j)];
                seen[i * ny + j] = true;
                while let Some((a, b)) = stack.pop() {
                    comp.push((a, b));
                    let mut nb = Vec::with_capacity(4);
                    if a > 0 {
                        nb.push((a - 1, b));
                    }
                    if a + 1 < nx {
                        nb.push((a + 1, b));
                    }
                    if b > 0 {
                        nb.push((a, b - 1));
                    }
                    if b + 1 < ny {
                        nb.push((a, b + 1));
                    }
                    for (c, d) in nb {
                        if self.covered(c, d) && !seen[c * ny + d] {
                            seen[c * ny + d] = true;
                            stack.push((c, d));
                        }
                    }
                }
                comp.sort_unstable();
                out.push(comp);
            }
        }
        out
    }
}

/// A component as its own cell set, for boundary and maximal-rectangle extraction.
struct Region<'g> {
    g: &'g Grid,
    cells: BTreeSet<(usize, usize)>,
}

impl Region<'_> {
    fn has(&self, i: isize, j: isize) -> bool {
        i >= 0 && j >= 0 && self.cells.contains(&(i as usize, j as usize))
    }

    /// Boundary loops, each a list of maximal straight edges `(from, to)`, interior on the left.
    fn loops(&self) -> Vec<Vec<(Point, Point)>> {
        let (xs, ys) = (&self.g.xs, &self.g.ys);
        // unit edges keyed by start point
        let mut out_of: BTreeMap<Point, Vec<Point>> = BTreeMap::new();
        for &(i, j) in &self.cells {
            let (ii, jj) = (i as isize, j as isize);
            let (x0, x1, y0, y1) = (xs[i], xs[i + 1], ys[j], ys[j + 1]);
            if !self.has(ii, jj - 1) {
                out_of.entry((x0, y0)).or_default().push((x1, y0));
            }
            if !self.has(ii + 1, jj) {
                out_of.entry((x1, y0)).or_default().push((x1, y1));
            }
            if !self.has(ii, jj + 1) {
                out_of.entry((x1, y1)).or_default().push((x0, y1));
            }
            if !self.has(ii - 1, jj) {
                out_of.entry((x0, y1)).or_default().push((x0, y0));
            }
        }
        let mut loops = Vec::new();
        while let Some((&start, _)) = out_of.iter().find(|(_, v)| !v.is_empty()) {
            let mut unit: Vec<(Point, Point)> = Vec::new();
            let mut at = start;
            let mut came: Option<Point> = None;
            while let Some(cands) = out_of.get_mut(&at) {
                if cands.is_empty() {
                    break;
                }
                // at a vertex left by two edges (two cells meeting at a corner), turn left
                let k = match came {
                    Some(d) if cands.len() > 1 => {
                        let turn = |p: &Point| {
                            let v = ((p.0 - at.0).signum(), (p.1 - at.1).signum());
                            d.0 * v.1 - d.1 * v.0
                        };
                        (0..cands.len()).max_by_key(|&k| turn(&cands[k])).unwrap_or(0)
                    }
                    _ => 0,
                };
                let to = cands.swap_remove(k);
                came = Some(((to.0 - at.0).signum(), (to.1 - at.1).signum()));
                unit.push((at, to));
                at = to;
                if at == start {
                    break;
                }
            }
            loops.push(merge_collinear(unit));
        }
        loops
    }

    /// Every maximal rectangle inside the region.
    fn max_rects(&self) -> Vec<Rect> {
        let (nx, ny) = (self.g.nx() as isize, self.g.ny() as isize);
        let mut out = BTreeSet::new();
        for i0 in 0..nx {
            // rows covered in every column i0..=i1
            let mut rows: Vec<bool> = (0..ny).map(|j| self.has(i0, j)).collect();
            for i1 in i0..nx {
                if i1 > i0 {
                    for j in 0..ny {
                        rows[j as usize] = rows[j as usize] && self.has(i1, j);
                    }
                }
                if !rows.iter().any(|&b| b) {
                    break;
                }
                let mut j = 0;
                while j < ny {
                    if !rows[j as usize] {
                        j += 1;
                        continue;
                    }
                    let j0 = j;
                    while j < ny && rows[j as usize] {
                        j += 1;
                    }
                    let j1 = j - 1;
                    let col_full = |c: isize| (j0..=j1).all(|r| self.has(c, r));
                    if !col_full(i0 - 1) && !col_full(i1 + 1) {
                        let g = self.g;
                        out.insert([
                            g.xs[i0 as usize],
                            g.ys[j0 as usize],
                            g.xs[i1 as usize + 1],
                            g.ys[j1 as usize + 1],
                        ]);
                    }
                }
            }
        }
        out.into_iter().collect()
    }
}

/// Join consecutive unit edges of one direction, starting the loop at a corner.
fn merge_collinear(unit: Vec<(Point, Point)>) -> Vec<(Point, Point)> {
    let n = unit.len();
    if n == 0 {
        return Vec::new();
    }
    let dir = |e: &(Point, Point)| ((e.1 .0 - e.0 .0).signum(), (e.1 .1 - e.0 .1).signum());
    let start = (0..n).find(|&k| dir(&unit[k]) != dir(&unit[(k + n - 1) % n])).unwrap_or(0);
    let mut out: Vec<(Point, Point)> = Vec::new();
    for k in 0..n {
        let e = unit[(start + k) % n];
        match out.last_mut() {
            Some(last) if dir(last) == dir(&e) => last.1 = e.1,
            _ => out.push(e),
        }
    }
    out
}

/// `gtl::orientation(prev, edge) == 1`: the turn from `prev` into `edge` is to the left.
#[must_use]
pub fn turns_left(prev: &Edge, edge: &Edge) -> bool {
    let (a, b) = (prev.vector(), edge.vector());
    a.0 * b.1 - a.1 * b.0 > 0
}

/// Merged polygons' edges and maximal rectangles of a set of rectangles.
fn merge(rects: &[Rect]) -> (Vec<Vec<Vec<(Point, Point)>>>, Vec<Vec<Rect>>) {
    let g = Grid::new(rects);
    let mut loops = Vec::new();
    let mut maxr = Vec::new();
    for comp in g.components() {
        let r = Region { g: &g, cells: comp.into_iter().collect() };
        loops.push(r.loops());
        maxr.push(r.max_rects());
    }
    (loops, maxr)
}

impl LayerModel {
    /// The model of one layer's shapes, nets in index order.
    #[must_use]
    pub fn build(shapes: &[Shape]) -> LayerModel {
        let mut by_net: BTreeMap<usize, Vec<&Shape>> = BTreeMap::new();
        for s in shapes {
            by_net.entry(s.net).or_default().push(s);
        }
        let mut m = LayerModel::default();
        for (net, v) in by_net {
            let all: Vec<Rect> = v.iter().map(|s| s.rect).collect();
            let fixed: Vec<Rect> = v.iter().filter(|s| s.fixed).map(|s| s.rect).collect();
            let (floops, fmax) = merge(&fixed);
            let fixed_edges: BTreeSet<(Point, Point)> = floops.into_iter().flatten().flatten().collect();
            let fixed_max: BTreeSet<Rect> = fmax.into_iter().flatten().collect();
            let (loops, maxr) = merge(&all);
            for (pl, pr) in loops.into_iter().zip(maxr) {
                let pin = m.pins.len();
                m.pins.push(Pin { net });
                for lp in pl {
                    let base = m.edges.len();
                    let n = lp.len();
                    for (k, (a, b)) in lp.into_iter().enumerate() {
                        m.edges.push(Edge {
                            low: a,
                            high: b,
                            fixed: fixed_edges.contains(&(a, b)),
                            pin,
                            prev: base + (k + n - 1) % n,
                            next: base + (k + 1) % n,
                        });
                    }
                }
                for r in pr {
                    m.rects.push(MaxRect { rect: r, fixed: fixed_max.contains(&r), pin });
                }
            }
            let route: Vec<Rect> = v.iter().filter(|s| !s.fixed).map(|s| s.rect).collect();
            if !route.is_empty() {
                m.route_shapes.insert(net, route);
            }
            if !fixed.is_empty() {
                m.fixed_shapes.insert(net, fixed);
            }
        }
        m
    }

    /// The net of a pin.
    #[must_use]
    pub fn net_of(&self, pin: usize) -> usize {
        self.pins[pin].net
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(net: usize, rect: Rect) -> Shape {
        Shape { net, rect, fixed: false }
    }

    /// A T: its maximal rectangles are the bar and the full-height stem; its boundary is one
    /// counter-clockwise loop of 8 edges, every turn left but the two inner corners.
    #[test]
    fn a_t_shape_merges_to_one_pin() {
        let m = LayerModel::build(&[s(0, [0, 0, 400, 50]), s(0, [175, 0, 225, 300])]);
        assert_eq!(m.pins.len(), 1);
        assert_eq!(m.rects.iter().map(|r| r.rect).collect::<Vec<_>>(), vec![[0, 0, 400, 50], [175, 0, 225, 300]]);
        assert_eq!(m.edges.len(), 8);
        let lefts = m.edges.iter().filter(|e| turns_left(&m.edges[e.prev], e)).count();
        assert_eq!(lefts, 6);
        // counter-clockwise: the bottom edge runs east
        assert!(m.edges.iter().any(|e| e.low == (0, 0) && e.high == (400, 0) && e.dir() == Dir::E));
    }

    /// Two nets and a ring with a hole: a hole runs clockwise (interior on its left).
    #[test]
    fn a_ring_has_a_clockwise_hole() {
        let ring = [s(0, [0, 0, 300, 50]), s(0, [0, 250, 300, 300]), s(0, [0, 0, 50, 300]), s(0, [250, 0, 300, 300])];
        let m = LayerModel::build(&ring);
        assert_eq!(m.pins.len(), 1);
        assert_eq!(m.edges.len(), 8);
        // the hole's bottom edge (y = 50) runs west
        assert!(m.edges.iter().any(|e| e.low.1 == 50 && e.high.1 == 50 && e.dir() == Dir::W));
        assert_eq!(m.rects.len(), 4);
    }

    #[test]
    fn corner_touching_shapes_are_separate_pins_and_fixed_flags_follow_the_fixed_shapes() {
        let m = LayerModel::build(&[s(0, [0, 0, 100, 50]), s(0, [100, 50, 200, 100])]);
        assert_eq!(m.pins.len(), 2);
        let f = LayerModel::build(&[Shape { net: 0, rect: [0, 0, 100, 50], fixed: true }, s(0, [100, 0, 300, 50])]);
        assert_eq!(f.pins.len(), 1);
        assert_eq!(f.rects.len(), 1);
        assert!(!f.rects[0].fixed, "the merged rectangle is not a rectangle of the fixed shape alone");
        assert!(f.edges.iter().any(|e| e.fixed), "the fixed shape's left edge is a fixed edge");
        assert_eq!(union_area_in(&[[0, 0, 100, 50]], &[50, 0, 150, 100]), 2500);
    }

    #[test]
    fn rectangle_helpers_follow_gtl() {
        assert_eq!(generalized_intersect(&[0, 0, 100, 50], &[0, 150, 100, 200]), [0, 50, 100, 150]);
        assert_eq!(axis_distance(&[0, 0, 100, 50], &[0, 150, 100, 200], false), 100);
        assert_eq!(axis_distance(&[0, 0, 100, 50], &[0, 150, 100, 200], true), 0);
        assert_eq!(square_distance(&[0, 0, 10, 10], &[13, 14, 20, 20]), 9 + 16);
        assert!(intersects(&[0, 0, 10, 10], &[10, 0, 20, 10]), "touching intersects");
    }
}
