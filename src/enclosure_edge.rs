// SPDX-License-Identifier: Apache-2.0
//! LEF58 enclosure-edge rules (`PROPERTY LEF58_ENCLOSUREEDGE`) on a cut layer: a pre-screen
//! checker. It reports; it never moves, adds or removes a shape.
//!
//! odb (pin 4155ac31) builds no rule from this property (an INFO "unsupported property") and keeps
//! only its raw text as a string property on the layer, so the reference detailed router never
//! checks it and there is no reference behaviour to match. The rules are read from that raw text
//! ([`Db::layer_string_properties`], the LEF statement as written) and checked as the LEF/DEF 5.8
//! language reference defines them (Layer (Cut), "Enclosure Edge Rule"). Checked:
//!
//! * `ENCLOSUREEDGE [CUTCLASS c] ABOVE o OPPOSITE ;` — when a cut edge's overhang by the metal
//!   above is at most `o`, the WHOLE opposite edge must also have an overhang of at most `o`
//!   ([`check_opposite`]).
//! * `… OPPOSITE NOCONCAVECORNER w ;` — as above, and: the cut edges orthogonal to an edge that
//!   meets the overhang (overhang at most `o`), extended by `o` past that edge, must have no
//!   concave corner of the metal above within `w` ([`check_no_concave_corner`]).
//!
//! Every other form is read and reported as not checked, naming its first clause
//! ([`NotChecked`]): `EXCEPTEOL`, `CUTTOBELOWSPACING`, `WRONGDIRECTION`, the parallel-neighbour
//! forms (`WIDTH` / `SPANLENGTH` / `INCLUDECORNER`), `CONVEXCORNERS`, `OPPOSITE` without `ABOVE`
//! (the reference requires it), a cut class the layer does not define, and anything that does not
//! parse. A report never reads as clean for a rule nobody looked at.
//!
//! Measurement (stated where the reference text leaves a choice; no oracle decides it):
//! * The metal above is the layer's merged shapes, every owner (wires, vias, special wires, block
//!   pins, placed instances' shapes): enclosure is a property of the metal, not of a net.
//! * The overhang at a point of a cut edge is how far the metal reaches outward from that point,
//!   perpendicular to the edge. An edge's overhang (the trigger, "overhang of a via cut is less
//!   than or equal to") is the SMALLEST along the edge, as an enclosure is measured; the "entire
//!   opposite edge" condition takes the LARGEST along the opposite edge. Both comparisons closed.
//! * A concave corner within `w`: Euclidean distance from the corner to the extended edge at most
//!   `w` (closed). A strict reading would make `NOCONCAVECORNER 0` say nothing; closed, it forbids
//!   a concave corner touching the extended edge, which is what a rule of 0 can mean.
//! * Cuts are the via cuts of routed wires and special wires; cuts inside library cells are the
//!   library's and are not collected. ⚠️ Of a special wire's vias only the DEF-defined (block)
//!   vias are decomposed by the binding ([`Db::swire_boxes`]); a LEF via placed in a special wire
//!   is not collected (power-grid vias are normally generated, so block vias). A cut the metal above does not cover is reported once
//!   ([`EeFinding::NotEnclosed`]) and not measured further.

#![deny(clippy::unwrap_used)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::masks_geom::{turns_left, union_area_in, LayerModel, Rect, Shape};
use crate::{Db, Result};

/// The layer property this module reads.
pub const PROPERTY: &str = "LEF58_ENCLOSUREEDGE";

/// Which metal a rule's overhang is required on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Above,
    Below,
    Both,
}

/// One checked statement, in microns as written.
#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    pub cut_class: Option<String>,
    pub overhang: f64,
    /// `NOCONCAVECORNER w`.
    pub no_concave_corner: Option<f64>,
}

/// A statement read but not checked, with the clause that puts it out of scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotChecked {
    pub clause: &'static str,
    /// The statement, whitespace normalised.
    pub statement: String,
}

pub const NC_EXCEPTEOL: &str = "OPPOSITE EXCEPTEOL";
pub const NC_CUTTOBELOW: &str = "OPPOSITE CUTTOBELOWSPACING";
pub const NC_WRONGDIRECTION: &str = "OPPOSITE WRONGDIRECTION";
pub const NC_NOT_ABOVE: &str = "OPPOSITE without ABOVE";
pub const NC_PARALLEL: &str = "WIDTH / SPANLENGTH / INCLUDECORNER (parallel neighbour form)";
pub const NC_CONVEX: &str = "CONVEXCORNERS";
pub const NC_CLASS: &str = "CUTCLASS not defined on the layer";
pub const NC_NO_ABOVE_LAYER: &str = "no routing layer above the cut layer";
pub const NC_MALFORMED: &str = "does not parse";

/// Read one property value (the string without its quotes; several `;`-terminated statements).
/// Statements are checked rules or [`NotChecked`], in the order written; empty ones are skipped.
#[must_use]
pub fn read_property(value: &str) -> (Vec<Rule>, Vec<NotChecked>) {
    let (mut rules, mut nc) = (Vec::new(), Vec::new());
    for stmt in value.split(';') {
        let toks: Vec<&str> = stmt.split_whitespace().collect();
        if toks.is_empty() {
            continue;
        }
        match read_statement(&toks) {
            Ok(r) => rules.push(r),
            Err(clause) => nc.push(NotChecked { clause, statement: toks.join(" ") }),
        }
    }
    (rules, nc)
}

fn num(t: Option<&&str>) -> Option<f64> {
    t.and_then(|s| s.parse::<f64>().ok()).filter(|v| v.is_finite() && *v >= 0.0)
}

/// `ENCLOSUREEDGE [CUTCLASS c] [ABOVE | BELOW] o { OPPOSITE … | … }`.
fn read_statement(t: &[&str]) -> std::result::Result<Rule, &'static str> {
    let up = |i: usize| t.get(i).map(|s| s.to_ascii_uppercase());
    if up(0).as_deref() != Some("ENCLOSUREEDGE") {
        return Err(NC_MALFORMED);
    }
    let mut i = 1;
    let mut cut_class = None;
    if up(i).as_deref() == Some("CUTCLASS") {
        cut_class = Some(t.get(i + 1).ok_or(NC_MALFORMED)?.to_string());
        i += 2;
    }
    let side = match up(i).as_deref() {
        Some("ABOVE") => {
            i += 1;
            Side::Above
        }
        Some("BELOW") => {
            i += 1;
            Side::Below
        }
        _ => Side::Both,
    };
    let overhang = num(t.get(i)).ok_or(NC_MALFORMED)?;
    i += 1;
    match up(i).as_deref() {
        Some("OPPOSITE") => i += 1,
        Some("INCLUDECORNER" | "WIDTH" | "SPANLENGTH") => return Err(NC_PARALLEL),
        Some("CONVEXCORNERS") => return Err(NC_CONVEX),
        _ => return Err(NC_MALFORMED),
    }
    let mut no_concave_corner = None;
    let mut unmodelled = None;
    while i < t.len() {
        match up(i).as_deref() {
            Some("NOCONCAVECORNER") => {
                no_concave_corner = Some(num(t.get(i + 1)).ok_or(NC_MALFORMED)?);
                i += 2;
            }
            Some("EXCEPTEOL") => {
                num(t.get(i + 1)).ok_or(NC_MALFORMED)?;
                unmodelled.get_or_insert(NC_EXCEPTEOL);
                i += 2;
            }
            Some("CUTTOBELOWSPACING") => {
                num(t.get(i + 1)).ok_or(NC_MALFORMED)?;
                i += 2;
                if up(i).as_deref() == Some("ABOVEMETAL") {
                    num(t.get(i + 1)).ok_or(NC_MALFORMED)?;
                    i += 2;
                }
                unmodelled.get_or_insert(NC_CUTTOBELOW);
            }
            Some("WRONGDIRECTION") => {
                i += 1;
                unmodelled.get_or_insert(NC_WRONGDIRECTION);
            }
            _ => return Err(NC_MALFORMED),
        }
    }
    if let Some(c) = unmodelled {
        return Err(c);
    }
    // The reference text: ABOVE must be specified when OPPOSITE is used.
    if side != Side::Above {
        return Err(NC_NOT_ABOVE);
    }
    Ok(Rule { cut_class, overhang, no_concave_corner })
}

// ---- geometry -----------------------------------------------------------------------------------

/// A cut edge, by the side of the cut it bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CutEdge {
    W,
    E,
    S,
    N,
}

impl CutEdge {
    pub const ALL: [CutEdge; 4] = [CutEdge::W, CutEdge::E, CutEdge::S, CutEdge::N];

    #[must_use]
    pub fn opposite(self) -> CutEdge {
        match self {
            CutEdge::W => CutEdge::E,
            CutEdge::E => CutEdge::W,
            CutEdge::S => CutEdge::N,
            CutEdge::N => CutEdge::S,
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            CutEdge::W => "W",
            CutEdge::E => "E",
            CutEdge::S => "S",
            CutEdge::N => "N",
        }
    }
}

/// The overhang along one cut edge, `(smallest, largest)`, measured on the maximal rectangles of
/// the metal: at each point of the edge, the farthest any maximal rectangle covering it reaches
/// outward. `None` when part of the edge has no metal over it, or the edge is degenerate.
///
/// The value only changes where a rectangle side crosses the edge, so the edge is cut at those
/// coordinates and each piece takes the best rectangle covering all of it.
#[must_use]
pub fn edge_overhang(cut: &Rect, edge: CutEdge, rects: &[Rect]) -> Option<(i32, i32)> {
    // along: the axis the edge runs on; perp: the axis the overhang is measured on
    let (alo, ahi, plo, phi) = match edge {
        CutEdge::S | CutEdge::N => (0, 2, 1, 3),
        CutEdge::W | CutEdge::E => (1, 3, 0, 2),
    };
    let outward_high = matches!(edge, CutEdge::N | CutEdge::E);
    let line = if outward_high { cut[phi] } else { cut[plo] };
    let (a0, a1) = (cut[alo], cut[ahi]);
    let mut cuts = vec![a0, a1];
    for r in rects {
        cuts.extend([r[alo], r[ahi]].into_iter().filter(|&v| v > a0 && v < a1));
    }
    cuts.sort_unstable();
    cuts.dedup();
    let (mut lo, mut hi) = (i32::MAX, i32::MIN);
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        // Spanning the piece and merely overlapping it agree here: the edge is cut at every
        // rectangle side inside it (a mutant testing overlap survives as equivalent).
        let reach = rects
            .iter()
            .filter(|r| r[alo] <= a && r[ahi] >= b && r[plo] <= line && r[phi] >= line)
            .map(|r| if outward_high { r[phi] - line } else { line - r[plo] })
            .max()?;
        lo = lo.min(reach);
        hi = hi.max(reach);
    }
    (lo <= hi).then_some((lo, hi))
}

/// The four edges' overhangs, in [`CutEdge::ALL`] order; `None` when any edge is not covered.
#[must_use]
pub fn cut_overhangs(cut: &Rect, rects: &[Rect]) -> Option<[(i32, i32); 4]> {
    let mut out = [(0, 0); 4];
    for (k, e) in CutEdge::ALL.into_iter().enumerate() {
        out[k] = edge_overhang(cut, e, rects)?;
    }
    Some(out)
}

/// An edge meets the overhang requirement when its overhang (the smallest along it) is at most
/// the rule's.
#[must_use]
pub fn meets(ov: (i32, i32), overhang: i32) -> bool {
    ov.0 <= overhang
}

/// `OPPOSITE`: each edge meeting the requirement whose opposite edge reaches farther than the
/// overhang anywhere along it, as `(edge, its overhang, opposite, opposite's largest)`.
#[must_use]
pub fn check_opposite(ov: &[(i32, i32); 4], overhang: i32) -> Vec<(CutEdge, i32, CutEdge, i32)> {
    let at = |e: CutEdge| ov[CutEdge::ALL.iter().position(|&x| x == e).unwrap_or(0)];
    CutEdge::ALL
        .into_iter()
        .filter(|&e| meets(at(e), overhang))
        .filter(|&e| at(e.opposite()).1 > overhang)
        .map(|e| (e, at(e).0, e.opposite(), at(e.opposite()).1))
        .collect()
}

/// The segments `NOCONCAVECORNER` measures from: the cut edges orthogonal to the edges meeting
/// the requirement, each extended by the overhang past every such edge it ends on. A segment is
/// a degenerate rectangle.
#[must_use]
pub fn extended_edges(cut: &Rect, ov: &[(i32, i32); 4], overhang: i32) -> Vec<Rect> {
    let m = |k: usize| meets(ov[k], overhang);
    let (w, e, s, n) = (m(0), m(1), m(2), m(3));
    let o = overhang;
    let mut out = Vec::new();
    if w || e {
        let (x0, x1) = (cut[0] - if w { o } else { 0 }, cut[2] + if e { o } else { 0 });
        out.push([x0, cut[1], x1, cut[1]]);
        out.push([x0, cut[3], x1, cut[3]]);
    }
    if s || n {
        let (y0, y1) = (cut[1] - if s { o } else { 0 }, cut[3] + if n { o } else { 0 });
        out.push([cut[0], y0, cut[0], y1]);
        out.push([cut[2], y0, cut[2], y1]);
    }
    out
}

/// The concave corners of the merged metal: the vertices where the boundary turns away from the
/// interior (the interior is on an edge's left, so a right turn).
#[must_use]
pub fn concave_corners(model: &LayerModel) -> Vec<(i32, i32)> {
    let mut out: Vec<(i32, i32)> =
        model.edges.iter().filter(|e| !turns_left(&model.edges[e.prev], e)).map(|e| e.low).collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Squared Euclidean distance from a point to an axis-parallel segment (a degenerate rectangle).
#[must_use]
pub fn point_to_segment2(p: (i32, i32), s: &Rect) -> i64 {
    let dx = (s[0] as i64 - p.0 as i64).max(p.0 as i64 - s[2] as i64).max(0);
    let dy = (s[1] as i64 - p.1 as i64).max(p.1 as i64 - s[3] as i64).max(0);
    dx * dx + dy * dy
}

/// `NOCONCAVECORNER w`: the concave corners within `w` of an extended edge (closed).
#[must_use]
pub fn check_no_concave_corner(segments: &[Rect], corners: &[(i32, i32)], within: i32) -> Vec<(i32, i32)> {
    let w2 = within as i64 * within as i64;
    corners.iter().copied().filter(|&c| segments.iter().any(|s| point_to_segment2(c, s) <= w2)).collect()
}

// ---- rules of a layer in DBU ----------------------------------------------------------------------

/// A checked rule in DBU, with its index among the layer's CHECKED rules, in the order written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleDbu {
    pub index: usize,
    /// The cut class's `(width, length)`, `None` for every cut.
    pub class: Option<(String, i32, i32)>,
    pub overhang: i32,
    pub within: Option<i32>,
}

impl RuleDbu {
    /// A cut belongs to the class when its two sides are the class's width and length, either way
    /// round.
    #[must_use]
    pub fn applies(&self, cut: &Rect) -> bool {
        match &self.class {
            None => true,
            Some((_, w, l)) => {
                let (a, b) = (cut[2] - cut[0], cut[3] - cut[1]);
                (a, b) == (*w, *l) || (a, b) == (*l, *w)
            }
        }
    }
}

fn dbu(um: f64, per_um: f64) -> i32 {
    (um * per_um).round() as i32
}

/// One cut layer's rules: the property read, cut classes resolved.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LayerRules {
    pub cut_layer: String,
    pub above_layer: Option<String>,
    pub rules: Vec<RuleDbu>,
    pub not_checked: Vec<NotChecked>,
    /// Statements stated on the layer, checked or not.
    pub statements: usize,
}

/// Resolve read rules for one cut layer. `classes` are the layer's cut classes as
/// `(name, width, length)` in DBU.
#[must_use]
pub fn resolve(
    cut_layer: &str,
    above_layer: Option<&str>,
    read: (Vec<Rule>, Vec<NotChecked>),
    classes: &[(String, i32, i32)],
    per_um: f64,
) -> LayerRules {
    let (rules, mut not_checked) = read;
    let statements = rules.len() + not_checked.len();
    let mut out = Vec::new();
    for (index, r) in rules.into_iter().enumerate() {
        let statement = format!("ENCLOSUREEDGE … {}", r.overhang);
        if above_layer.is_none() {
            not_checked.push(NotChecked { clause: NC_NO_ABOVE_LAYER, statement });
            continue;
        }
        let class = match &r.cut_class {
            None => None,
            Some(name) => match classes.iter().find(|c| c.0.eq_ignore_ascii_case(name)) {
                Some(c) => Some(c.clone()),
                None => {
                    not_checked.push(NotChecked { clause: NC_CLASS, statement });
                    continue;
                }
            },
        };
        out.push(RuleDbu {
            index,
            class,
            overhang: dbu(r.overhang, per_um),
            within: r.no_concave_corner.map(|w| dbu(w, per_um)),
        });
    }
    LayerRules {
        cut_layer: cut_layer.to_string(),
        above_layer: above_layer.map(str::to_string),
        rules: out,
        not_checked,
        statements,
    }
}

// ---- findings and the pass ----------------------------------------------------------------------

/// One finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EeFinding {
    /// `OPPOSITE`: `edge` meets the overhang (its smallest `overhang`), the opposite edge reaches
    /// `opposite_max` somewhere, more than `limit`.
    Opposite {
        layer: String,
        rule: usize,
        net: String,
        cut: Rect,
        edge: CutEdge,
        overhang: i32,
        opposite: CutEdge,
        opposite_max: i32,
        limit: i32,
    },
    /// `NOCONCAVECORNER`: a concave corner of the metal above within `within` of an extended edge.
    ConcaveCorner { layer: String, rule: usize, net: String, cut: Rect, corner: (i32, i32), within: i32 },
    /// The metal above does not cover the cut; nothing measured.
    NotEnclosed { layer: String, net: String, cut: Rect },
}

impl EeFinding {
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            EeFinding::Opposite { .. } => "enclosure_edge_opposite",
            EeFinding::ConcaveCorner { .. } => "enclosure_edge_concave_corner",
            EeFinding::NotEnclosed { .. } => "cut_not_enclosed",
        }
    }

    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            EeFinding::Opposite { layer, rule, net, cut, edge, overhang, opposite, opposite_max, limit } => {
                serde_json::json!({ "kind": self.kind(), "layer": layer, "rule": rule, "net": net, "cut": cut,
                    "edge": edge.as_str(), "overhang": overhang, "opposite": opposite.as_str(),
                    "opposite_max": opposite_max, "limit": limit })
            }
            EeFinding::ConcaveCorner { layer, rule, net, cut, corner, within } => {
                serde_json::json!({ "kind": self.kind(), "layer": layer, "rule": rule, "net": net, "cut": cut,
                    "corner": [corner.0, corner.1], "within": within })
            }
            EeFinding::NotEnclosed { layer, net, cut } => {
                serde_json::json!({ "kind": self.kind(), "layer": layer, "net": net, "cut": cut })
            }
        }
    }
}

/// A cut to check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cut {
    pub net: String,
    pub rect: Rect,
}

/// What one cut layer's pass did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LayerReport {
    pub rules: LayerRules,
    pub cuts: usize,
    /// Cuts at least one checked rule applies to.
    pub cuts_checked: usize,
}

/// A uniform-grid index over rectangles, for the metal near a cut.
struct Grid {
    cell: i64,
    bins: HashMap<(i64, i64), Vec<usize>>,
}

impl Grid {
    fn new(rects: &[Rect], cell: i64) -> Grid {
        let mut bins: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
        for (k, r) in rects.iter().enumerate() {
            for cx in (r[0] as i64).div_euclid(cell)..=(r[2] as i64).div_euclid(cell) {
                for cy in (r[1] as i64).div_euclid(cell)..=(r[3] as i64).div_euclid(cell) {
                    bins.entry((cx, cy)).or_default().push(k);
                }
            }
        }
        Grid { cell, bins }
    }

    fn query(&self, q: &Rect) -> BTreeSet<usize> {
        let mut out = BTreeSet::new();
        for cx in (q[0] as i64).div_euclid(self.cell)..=(q[2] as i64).div_euclid(self.cell) {
            for cy in (q[1] as i64).div_euclid(self.cell)..=(q[3] as i64).div_euclid(self.cell) {
                if let Some(v) = self.bins.get(&(cx, cy)) {
                    out.extend(v.iter().copied());
                }
            }
        }
        out
    }
}

/// Stage: check one cut layer's cuts against its rules, over the metal above.
///
/// Per cut, the metal is clipped to the cut grown by the farthest any rule looks (overhang plus
/// `within`, plus one): clipping adds only convex corners, and an overhang cut short at the
/// window's edge is still more than every rule's overhang, so no verdict changes.
#[must_use]
pub fn check_layer(rules: &LayerRules, cuts: &[Cut], metal: &[Rect]) -> (usize, Vec<EeFinding>) {
    let mut findings = Vec::new();
    if rules.rules.is_empty() {
        return (0, findings);
    }
    let reach = rules.rules.iter().map(|r| r.overhang + r.within.unwrap_or(0)).max().unwrap_or(0) + 1;
    let grid = Grid::new(metal, i64::from(reach.max(1)) * 8);
    let mut checked = 0;
    let layer = &rules.cut_layer;
    for c in cuts {
        let applying: Vec<&RuleDbu> = rules.rules.iter().filter(|r| r.applies(&c.rect)).collect();
        if applying.is_empty() {
            continue;
        }
        checked += 1;
        let win = [c.rect[0] - reach, c.rect[1] - reach, c.rect[2] + reach, c.rect[3] + reach];
        let near: Vec<Rect> = grid
            .query(&win)
            .into_iter()
            .map(|k| metal[k])
            .map(|r| [r[0].max(win[0]), r[1].max(win[1]), r[2].min(win[2]), r[3].min(win[3])])
            .filter(|r| r[0] < r[2] && r[1] < r[3])
            .collect();
        let area = i64::from(c.rect[2] - c.rect[0]) * i64::from(c.rect[3] - c.rect[1]);
        let model = LayerModel::build(&near.iter().map(|&rect| Shape { net: 0, rect, fixed: false }).collect::<Vec<_>>());
        let maxr: Vec<Rect> = model.rects.iter().map(|m| m.rect).collect();
        let ov = (union_area_in(&near, &c.rect) == area).then(|| cut_overhangs(&c.rect, &maxr)).flatten();
        let Some(ov) = ov else {
            findings.push(EeFinding::NotEnclosed { layer: layer.clone(), net: c.net.clone(), cut: c.rect });
            continue;
        };
        let corners = if applying.iter().any(|r| r.within.is_some()) { concave_corners(&model) } else { Vec::new() };
        for r in applying {
            for (edge, overhang, opposite, opposite_max) in check_opposite(&ov, r.overhang) {
                findings.push(EeFinding::Opposite {
                    layer: layer.clone(),
                    rule: r.index,
                    net: c.net.clone(),
                    cut: c.rect,
                    edge,
                    overhang,
                    opposite,
                    opposite_max,
                    limit: r.overhang,
                });
            }
            if let Some(within) = r.within {
                let segs = extended_edges(&c.rect, &ov, r.overhang);
                for corner in check_no_concave_corner(&segs, &corners, within) {
                    findings.push(EeFinding::ConcaveCorner {
                        layer: layer.clone(),
                        rule: r.index,
                        net: c.net.clone(),
                        cut: c.rect,
                        corner,
                        within,
                    });
                }
            }
        }
    }
    (checked, findings)
}

/// The whole pass's result.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Report {
    pub layers: Vec<LayerReport>,
    pub findings: Vec<EeFinding>,
    /// The database holds a block (otherwise only the rules were read).
    pub has_block: bool,
}

impl Report {
    #[must_use]
    pub fn rules_checked(&self) -> usize {
        self.layers.iter().map(|l| l.rules.rules.len()).sum()
    }
    #[must_use]
    pub fn not_checked_by_clause(&self) -> BTreeMap<&'static str, usize> {
        let mut m = BTreeMap::new();
        for nc in self.layers.iter().flat_map(|l| l.rules.not_checked.iter()) {
            *m.entry(nc.clause).or_default() += 1;
        }
        m
    }
}

/// Stage: every cut layer's enclosure-edge rules, read from the layer's raw property text.
pub fn read_rules(db: &Db) -> Result<Vec<LayerRules>> {
    let per_um = f64::from(db.dbu_per_micron());
    let mut out = Vec::new();
    for (name, _) in db.layers_with_direction()? {
        if db.layer_get_type(&name)? != "CUT" {
            continue;
        }
        let props = db.layer_string_properties(&name);
        let texts: Vec<&str> = props.iter().filter(|(k, _)| k.eq_ignore_ascii_case(PROPERTY)).map(|(_, v)| v.as_str()).collect();
        if texts.is_empty() {
            continue;
        }
        let read = read_property(&texts.join(" ; "));
        let above = db.layer_name_by_number(i64::from(db.layer_get_number(&name)) + 1);
        let above = (!above.is_empty() && db.layer_get_type(&above).ok().as_deref() == Some("ROUTING")).then_some(above);
        let classes: Vec<(String, i32, i32)> = db
            .layer_get_tech_layer_cut_class_rules(&name)
            .into_iter()
            .enumerate()
            .map(|(k, n)| {
                let w = db.cutclassrule_get_width(&name, k);
                let l = if db.cutclassrule_is_length_valid(&name, k) { db.cutclassrule_get_length(&name, k) } else { w };
                (n, w, l)
            })
            .collect();
        out.push(resolve(&name, above.as_deref(), read, &classes, per_um));
    }
    Ok(out)
}

/// Stage: the cuts on a layer, from routed wires and special wires, with their nets.
pub fn collect_cuts(db: &Db, layer_number: i64) -> Result<Vec<Cut>> {
    let mut out = Vec::new();
    for net in db.block_get_nets() {
        for b in db.net_wire_boxes(&net).iter().filter(|b| b.layer == layer_number && b.from_via) {
            out.push(Cut { net: net.clone(), rect: [b.x0, b.y0, b.x1, b.y1] });
        }
    }
    for (b, n) in db.swire_boxes()?.iter().zip(db.swire_box_nets()?) {
        if b.0 == layer_number {
            out.push(Cut { net: n, rect: [b.1, b.2, b.3, b.4] });
        }
    }
    Ok(out)
}

/// Stage: every shape on a routing layer, any owner.
pub fn collect_metal(db: &Db, layer_number: i64) -> Result<Vec<Rect>> {
    let mut out = Vec::new();
    for net in db.block_get_nets() {
        out.extend(db.net_wire_boxes(&net).iter().filter(|b| b.layer == layer_number).map(|b| [b.x0, b.y0, b.x1, b.y1]));
    }
    out.extend(db.swire_boxes()?.iter().filter(|b| b.0 == layer_number).map(|b| [b.1, b.2, b.3, b.4]));
    for t in db.block_get_b_terms() {
        for p in 0..db.num_bterm_get_b_pins(&t) {
            out.extend(db.bpin_layer_boxes(&t, p)?.iter().filter(|b| b.0 == layer_number).map(|b| [b.1, b.2, b.3, b.4]));
        }
    }
    out.extend(db.inst_shapes()?.iter().filter(|b| b.0 == layer_number).map(|b| [b.1, b.2, b.3, b.4]));
    out.retain(|r| r[0] < r[2] && r[1] < r[3]);
    Ok(out)
}

/// The pass: read the rules ([`read_rules`]), then per cut layer with a checked rule collect its
/// cuts ([`collect_cuts`]) and the metal above ([`collect_metal`]) and check them ([`check_layer`]).
/// Without a block only the rules are read.
pub fn check_enclosure_edges(db: &Db) -> Result<Report> {
    let has_block = !db.block_name().is_empty();
    let mut rep = Report { has_block, ..Report::default() };
    for rules in read_rules(db)? {
        let mut lr = LayerReport { rules, ..LayerReport::default() };
        if has_block && !lr.rules.rules.is_empty() {
            if let Some(above) = lr.rules.above_layer.clone() {
                let cuts = collect_cuts(db, i64::from(db.layer_get_number(&lr.rules.cut_layer)))?;
                let metal = collect_metal(db, i64::from(db.layer_get_number(&above)))?;
                let (checked, f) = check_layer(&lr.rules, &cuts, &metal);
                lr.cuts = cuts.len();
                lr.cuts_checked = checked;
                rep.findings.extend(f);
            }
        }
        rep.layers.push(lr);
    }
    Ok(rep)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(o: i32, w: Option<i32>) -> LayerRules {
        LayerRules {
            cut_layer: "V1".into(),
            above_layer: Some("M2".into()),
            rules: vec![RuleDbu { index: 0, class: None, overhang: o, within: w }],
            not_checked: vec![],
            statements: 1,
        }
    }

    fn kinds(f: &[EeFinding]) -> Vec<&'static str> {
        f.iter().map(EeFinding::kind).collect()
    }

    /// The two OPPOSITE forms read as checked rules; every other form is named by its first
    /// clause, a form that does not parse as such.
    #[test]
    fn statements_are_checked_or_named() {
        let (r, nc) = read_property("ENCLOSUREEDGE CUTCLASS VA ABOVE 0.02 OPPOSITE ;\n ENCLOSUREEDGE CUTCLASS VA ABOVE 0.01 OPPOSITE NOCONCAVECORNER 0.05 ;");
        assert!(nc.is_empty(), "{nc:?}");
        assert_eq!(r, vec![
            Rule { cut_class: Some("VA".into()), overhang: 0.02, no_concave_corner: None },
            Rule { cut_class: Some("VA".into()), overhang: 0.01, no_concave_corner: Some(0.05) },
        ]);
        let clause = |s: &str| {
            let (r, nc) = read_property(s);
            assert!(r.is_empty(), "{s}");
            nc[0].clause
        };
        assert_eq!(clause("ENCLOSUREEDGE ABOVE 0.02 OPPOSITE EXCEPTEOL 0.15 ;"), NC_EXCEPTEOL);
        assert_eq!(clause("ENCLOSUREEDGE ABOVE 0.02 OPPOSITE NOCONCAVECORNER 0.05 CUTTOBELOWSPACING 0.07 ABOVEMETAL 0.03 ;"), NC_CUTTOBELOW);
        assert_eq!(clause("ENCLOSUREEDGE ABOVE 0.02 OPPOSITE WRONGDIRECTION ;"), NC_WRONGDIRECTION);
        assert_eq!(clause("ENCLOSUREEDGE BELOW 0.02 OPPOSITE ;"), NC_NOT_ABOVE);
        assert_eq!(clause("ENCLOSUREEDGE 0.02 OPPOSITE ;"), NC_NOT_ABOVE);
        assert_eq!(clause("ENCLOSUREEDGE 0.02 WIDTH 0.2 PARALLEL 0.25 WITHIN 0.11 ;"), NC_PARALLEL);
        assert_eq!(clause("ENCLOSUREEDGE 0.05 INCLUDECORNER WIDTH BOTHWIRE 0.2 PARALLEL 0.5 WITHIN 0.1 ;"), NC_PARALLEL);
        assert_eq!(clause("ENCLOSUREEDGE 0.02 CONVEXCORNERS 0.1 0.1 PARALLEL 0.1 LENGTH 0.2 ;"), NC_CONVEX);
        assert_eq!(clause("ENCLOSUREEDGE ABOVE OPPOSITE ;"), NC_MALFORMED);
        assert_eq!(clause("ENCLOSUREEDGE ABOVE 0.02 OPPOSITE NOCONCAVECORNER ;"), NC_MALFORMED);
        assert_eq!(clause("ENCLOSUREEDGE ABOVE 0.02 OPPOSITE FOO ;"), NC_MALFORMED);
        assert_eq!(clause("ENCLOSURE ABOVE 0.02 0.03 ;"), NC_MALFORMED);
        assert_eq!(read_property(" ; ;").0.len() + read_property(" ; ;").1.len(), 0);
    }

    /// A cut class the layer does not define, and a cut layer with no routing layer above, leave a
    /// rule unchecked and say so; a defined class resolves case-insensitively to DBU.
    #[test]
    fn resolution_names_what_it_cannot_check() {
        let read = || read_property("ENCLOSUREEDGE CUTCLASS va ABOVE 0.02 OPPOSITE ; ENCLOSUREEDGE CUTCLASS VB ABOVE 0.02 OPPOSITE ;");
        let lr = resolve("V1", Some("M2"), read(), &[("VA".into(), 40, 40)], 1000.0);
        assert_eq!(lr.statements, 2);
        assert_eq!(lr.rules, vec![RuleDbu { index: 0, class: Some(("VA".into(), 40, 40)), overhang: 20, within: None }]);
        assert_eq!(lr.not_checked.iter().map(|n| n.clause).collect::<Vec<_>>(), [NC_CLASS]);
        let none = resolve("V1", None, read(), &[("VA".into(), 40, 40)], 1000.0);
        assert!(none.rules.is_empty());
        assert_eq!(none.not_checked.len(), 2);
        assert!(none.not_checked.iter().all(|n| n.clause == NC_NO_ABOVE_LAYER));
    }

    /// A class applies to a cut of its width and length, either way round, and to nothing else.
    #[test]
    fn a_class_matches_by_size_either_way_round() {
        let r = RuleDbu { index: 0, class: Some(("VR".into(), 40, 80)), overhang: 20, within: None };
        assert!(r.applies(&[0, 0, 40, 80]) && r.applies(&[0, 0, 80, 40]));
        assert!(!r.applies(&[0, 0, 40, 40]) && !r.applies(&[0, 0, 80, 80]));
        assert!(RuleDbu { class: None, ..r }.applies(&[0, 0, 1, 1]));
    }

    /// Overhang along an edge: the smallest and largest reach outward. A cut half on a narrow wire
    /// and half on a wider block (reference text, OPPOSITE example b): the S edge reaches 0 on the
    /// wire and 30 under the block.
    #[test]
    fn overhang_is_measured_along_the_whole_edge() {
        let wire = [0, 0, 100, 40];
        let block = [60, -30, 200, 40];
        let cut = [40, 0, 80, 40];
        let m = LayerModel::build(&[Shape { net: 0, rect: wire, fixed: false }, Shape { net: 0, rect: block, fixed: false }]);
        let maxr: Vec<Rect> = m.rects.iter().map(|r| r.rect).collect();
        assert_eq!(edge_overhang(&cut, CutEdge::S, &maxr), Some((0, 30)));
        assert_eq!(edge_overhang(&cut, CutEdge::N, &maxr), Some((0, 0)));
        assert_eq!(edge_overhang(&cut, CutEdge::W, &maxr), Some((40, 40)));
        assert_eq!(edge_overhang(&cut, CutEdge::E, &maxr), Some((120, 120)));
        assert_eq!(edge_overhang(&[300, 0, 340, 40], CutEdge::S, &maxr), None, "no metal");
        assert_eq!(edge_overhang(&[40, 0, 40, 40], CutEdge::S, &maxr), None, "degenerate edge");
        // mirrored: the block on the W, so the largest piece comes FIRST along the edge
        let m = LayerModel::build(&[Shape { net: 0, rect: [0, 0, 100, 40], fixed: false }, Shape { net: 0, rect: [-100, -30, 60, 40], fixed: false }]);
        let maxr: Vec<Rect> = m.rects.iter().map(|r| r.rect).collect();
        assert_eq!(edge_overhang(&cut, CutEdge::S, &maxr), Some((0, 30)));
    }

    /// OPPOSITE (reference text): an edge at most the overhang requires the WHOLE opposite edge at
    /// most the overhang; both closed. Example a: 0 on N and S, wide on W and E: clean. Example b:
    /// N is 0, S reaches 30: a finding on N. Example c: every edge 10: clean.
    #[test]
    fn opposite_needs_the_whole_opposite_edge() {
        // [W, E, S, N]
        assert!(check_opposite(&[(50, 50), (50, 50), (0, 0), (0, 0)], 20).is_empty());
        assert_eq!(check_opposite(&[(40, 40), (120, 120), (0, 30), (0, 0)], 20), vec![(CutEdge::N, 0, CutEdge::S, 30)],
            "S also meets at its smallest (0), and N's largest is 0: only N");
        assert!(check_opposite(&[(10, 10); 4], 10).is_empty(), "at the overhang meets, and is allowed");
        assert_eq!(check_opposite(&[(20, 20), (21, 21), (50, 50), (50, 50)], 20), vec![(CutEdge::W, 20, CutEdge::E, 21)]);
        assert!(check_opposite(&[(21, 21), (21, 21), (50, 50), (50, 50)], 20).is_empty(), "none meets");
        // The trigger is the edge's SMALLEST overhang: W reaches 0 to 30 along it, so it meets,
        // and E (25 all along) is past the overhang.
        assert_eq!(check_opposite(&[(0, 30), (25, 25), (50, 50), (50, 50)], 20), vec![(CutEdge::W, 0, CutEdge::E, 25)]);
    }

    /// The extended edges: W and E meet, so the S and N edges grow by the overhang both ways (the
    /// red line of the reference's NOCONCAVECORNER example); only W meets: grown to the W only.
    #[test]
    fn edges_extend_past_the_edges_that_meet() {
        let cut = [0, 0, 40, 40];
        assert_eq!(extended_edges(&cut, &[(20, 20), (20, 20), (50, 50), (50, 50)], 20), vec![[-20, 0, 60, 0], [-20, 40, 60, 40]]);
        assert_eq!(extended_edges(&cut, &[(20, 20), (21, 21), (50, 50), (50, 50)], 20), vec![[-20, 0, 40, 0], [-20, 40, 40, 40]]);
        assert_eq!(extended_edges(&cut, &[(50, 50), (50, 50), (0, 0), (50, 50)], 20), vec![[0, -20, 0, 40], [40, -20, 40, 40]]);
        assert!(extended_edges(&cut, &[(50, 50); 4], 20).is_empty());
    }

    /// Distance to a segment is Euclidean and a corner counts at `within` too (closed), so
    /// `NOCONCAVECORNER 0` forbids a corner touching the extended edge and nothing farther.
    #[test]
    fn concave_corner_distance_is_closed_and_euclidean() {
        let seg = [[-20, 0, 60, 0]];
        assert_eq!(point_to_segment2((60, -40), &seg[0]), 1600);
        assert_eq!(point_to_segment2((90, -40), &seg[0]), 900 + 1600);
        assert_eq!(check_no_concave_corner(&seg, &[(60, -40)], 50), vec![(60, -40)]);
        assert_eq!(check_no_concave_corner(&seg, &[(60, -50)], 50), vec![(60, -50)], "at exactly within");
        assert!(check_no_concave_corner(&seg, &[(60, -51)], 50).is_empty());
        assert!(check_no_concave_corner(&seg, &[(91, -40)], 50).is_empty(), "past 50 diagonally");
        assert_eq!(check_no_concave_corner(&seg, &[(60, 0)], 0), vec![(60, 0)], "0: touching the extended edge");
        assert!(check_no_concave_corner(&seg, &[(61, 0)], 0).is_empty());
    }

    /// Only right turns of the merged boundary are concave: an L has one, a rectangle none.
    #[test]
    fn an_l_has_one_concave_corner() {
        let l = LayerModel::build(&[Shape { net: 0, rect: [0, 0, 40, 200], fixed: false }, Shape { net: 0, rect: [0, 0, 200, 40], fixed: false }]);
        assert_eq!(concave_corners(&l), vec![(40, 40)]);
        let r = LayerModel::build(&[Shape { net: 0, rect: [0, 0, 40, 200], fixed: false }]);
        assert!(concave_corners(&r).is_empty());
    }

    /// The reference's NOCONCAVECORNER example a, end to end: a cut in a vertical wire with 20 on
    /// W and E, the wire turning E 40 below the cut. The S edge extended by 20 ends 40 from the
    /// corner: a finding with within 50 or 40 (closed), none with within 39 or the leg 51 below.
    #[test]
    fn the_reference_concave_corner_example() {
        let cut = Cut { net: "a".into(), rect: [20, 200, 60, 240] };
        let metal = |leg_top: i32| vec![[0, 0, 80, 400], [80, 0, 300, leg_top]];
        let (n, f) = check_layer(&rule(20, Some(50)), std::slice::from_ref(&cut), &metal(160));
        assert_eq!(n, 1);
        assert_eq!(f, vec![EeFinding::ConcaveCorner { layer: "V1".into(), rule: 0, net: "a".into(), cut: cut.rect, corner: (80, 160), within: 50 }]);
        assert_eq!(check_layer(&rule(20, Some(40)), std::slice::from_ref(&cut), &metal(160)).1.len(), 1);
        assert!(check_layer(&rule(20, Some(39)), std::slice::from_ref(&cut), &metal(160)).1.is_empty());
        assert!(check_layer(&rule(20, Some(50)), std::slice::from_ref(&cut), &metal(149)).1.is_empty());
        assert!(check_layer(&rule(20, None), &[cut], &metal(160)).1.is_empty(), "no NOCONCAVECORNER: OPPOSITE only, clean");
    }

    /// The pass: a cut sticking out of its metal is reported once and measured no further; a cut
    /// no rule applies to is not counted; an OPPOSITE violation is found through the window.
    #[test]
    fn the_layer_pass_reports_each_kind() {
        let out = Cut { net: "b".into(), rect: [90, 0, 130, 40] };
        let (n, f) = check_layer(&rule(20, Some(50)), &[out], &[[0, 0, 100, 40]]);
        assert_eq!((n, kinds(&f)), (1, vec!["cut_not_enclosed"]));
        // every edge covered, the middle over a hole: still not enclosed (the area, not the edges)
        let ring = [[-20, -20, 140, 20], [-20, 100, 140, 140], [-20, -20, 20, 140], [100, -20, 140, 140]];
        let (_, f) = check_layer(&rule(20, Some(50)), &[Cut { net: "r".into(), rect: [0, 0, 120, 120] }], &ring);
        assert_eq!(kinds(&f), vec!["cut_not_enclosed"]);
        let mut classed = rule(20, None);
        classed.rules[0].class = Some(("VB".into(), 40, 80));
        assert_eq!(check_layer(&classed, &[Cut { net: "c".into(), rect: [0, 0, 40, 40] }], &[[0, 0, 40, 40]]).0, 0);
        // example b of the reference: N flush, S reaching 30 under the block
        let cut = Cut { net: "d".into(), rect: [40, 0, 80, 40] };
        let (_, f) = check_layer(&rule(20, None), &[cut], &[[0, 0, 100, 40], [60, -30, 200, 40]]);
        assert_eq!(kinds(&f), vec!["enclosure_edge_opposite"]);
        // far metal beyond the window cannot change a verdict
        let cut = Cut { net: "e".into(), rect: [1000, 0, 1040, 40] };
        let (_, f) = check_layer(&rule(20, None), &[cut], &[[980, 0, 1060, 40], [1000, 40, 1040, 5000]]);
        assert_eq!(kinds(&f), vec!["enclosure_edge_opposite"], "S is 0, N reaches past the window");
        assert!(check_layer(&LayerRules::default(), &[], &[]).1.is_empty());
    }
}
