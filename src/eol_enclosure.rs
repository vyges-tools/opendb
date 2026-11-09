// SPDX-License-Identifier: Apache-2.0
//! LEF58 end-of-line enclosure rules (`PROPERTY LEF58_EOLENCLOSURE`) on a cut layer: a pre-screen
//! checker. It reports; it never moves, adds or removes a shape.
//!
//! odb (pin 4155ac31, and master as read 2026-11-09) builds no rule from this property and keeps
//! only its raw text as a string property on the layer; no detailed router or checker of the
//! reference reads it. There is no reference behaviour to match, so the rules are read from that
//! raw text ([`Db::layer_string_properties`]) and checked as the LEF/DEF 5.8 language reference
//! defines them (Layer (Cut), "EOL Enclosure Rule", its text and Figures 2-73 to 2-76):
//!
//! > `EOLENCLOSURE eolWidth` specifies the enclosure requirement on a EOL edge with width less
//! > than eolWidth. […] `overhang [exactOverhang]`: the enclosure EOL requirement is either exactly
//! > equal to exactOverhang or greater than or equal to overhang. `LONGEDGEONLY | SHORTEDGEONLY
//! > overhang`: the requirement is only applied to the side/long edges, or the end/short edges,
//! > of the rectangular cut. `ABOVE | BELOW`: the rule only applies on the above or below
//! > routing layer.
//!
//! Checked: `EOLENCLOSURE w [CUTCLASS c] [ABOVE | BELOW] {LONGEDGEONLY | SHORTEDGEONLY} o` and
//! `EOLENCLOSURE w [CUTCLASS c] [ABOVE | BELOW] o [exactOverhang]`. Every other clause
//! (`MINEOLWIDTH`, `HORIZONTAL` / `VERTICAL`, `EQUALRECTWIDTH`, `PARALLELEDGE`, `MINLENGTH`,
//! `ALLSIDES`), a class the layer does not define, an edge-kind rule on a square or unnamed class,
//! a side with no routing layer, and anything that does not parse is read and reported as not
//! checked, naming its clause ([`NotChecked`]).
//!
//! Interpretations (the text leaves these open; no oracle decides them; each pinned in a test):
//! * **EOL edge**: a boundary edge of the merged metal shorter than `eolWidth` (strict, "less
//!   than") with both corners convex. An edge on the check window's clip line is the clip's and
//!   never counts.
//! * **A cut edge is on an EOL edge** when, along some stretch of the cut edge, the metal reaches
//!   outward (perpendicular to the edge) exactly to an EOL edge facing the same way that overlaps
//!   that stretch with positive length. The overhang is that reach.
//! * **Violation**: overhang `< o` (closed at `o`: the text's "greater than or equal to"; Figure
//!   2-75 b's caption writes "<= 0.05", which we read as a slip), unless it equals `exactOverhang`.
//!   One finding per cut edge, at its smallest failing overhang.
//! * **Edge kinds**: with `SHORTEDGEONLY` only the cut's two shorter edges are measured, with
//!   `LONGEDGEONLY` only the longer two (Figure 2-73: a long edge under a 0.12-long metal end
//!   violates, a short edge under an EOL edge does not), otherwise all four.
//! * **Side**: without `ABOVE` / `BELOW` the rule holds on both neighbouring routing layers, each
//!   on its own.
//! * Metal = the layer's merged shapes, every owner; cuts = routed-wire and special-wire (block)
//!   via cuts, as [`crate::enclosure_edge`] collects them. A cut the metal does not cover is
//!   reported once per metal layer ([`EolFinding::NotEnclosed`]) and not measured.

#![deny(clippy::unwrap_used)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

use std::collections::BTreeMap;

use crate::enclosure_edge::{collect_cuts, collect_metal, cut_classes, Cut, CutEdge, Grid, NotChecked};
use crate::masks_geom::{turns_left, union_area_in, LayerModel, Rect, Shape};
use crate::{Db, Result};

/// The layer property this module reads.
pub const PROPERTY: &str = "LEF58_EOLENCLOSURE";

pub const NC_MINEOLWIDTH: &str = "MINEOLWIDTH";
pub const NC_DIRECTION: &str = "HORIZONTAL / VERTICAL";
pub const NC_EQUALRECTWIDTH: &str = "EQUALRECTWIDTH";
pub const NC_PARALLELEDGE: &str = "PARALLELEDGE";
pub const NC_MINLENGTH: &str = "MINLENGTH";
pub const NC_ALLSIDES: &str = "ALLSIDES";
pub const NC_CLASS: &str = "CUTCLASS not defined on the layer";
pub const NC_EDGE_KIND_SQUARE: &str = "LONGEDGEONLY / SHORTEDGEONLY without a rectangular cut class";
pub const NC_NO_LAYER: &str = "no routing layer on the rule's side";
pub const NC_MALFORMED: &str = "does not parse";

/// Which metal the rule holds on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Above,
    Below,
    Both,
}

/// Which cut edges the overhang is required on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeKind {
    All,
    Short,
    Long,
}

/// One checked statement, in microns as written.
#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    pub eol_width: f64,
    pub cut_class: Option<String>,
    pub side: Side,
    pub edges: EdgeKind,
    pub overhang: f64,
    pub exact: Option<f64>,
}

/// Read one property value (several `;`-terminated statements), in the order written.
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

/// The whole grammar is parsed in its written order; a clause we do not model is remembered and
/// returned once the statement is known to parse.
fn read_statement(t: &[&str]) -> std::result::Result<Rule, &'static str> {
    let up = |i: usize| t.get(i).map(|s| s.to_ascii_uppercase());
    if up(0).as_deref() != Some("EOLENCLOSURE") {
        return Err(NC_MALFORMED);
    }
    let eol_width = num(t.get(1)).ok_or(NC_MALFORMED)?;
    let mut i = 2;
    let mut unmodelled: Option<&'static str> = None;
    if up(i).as_deref() == Some("MINEOLWIDTH") {
        num(t.get(i + 1)).ok_or(NC_MALFORMED)?;
        i += 2;
        unmodelled.get_or_insert(NC_MINEOLWIDTH);
    }
    if matches!(up(i).as_deref(), Some("HORIZONTAL" | "VERTICAL")) {
        i += 1;
        unmodelled.get_or_insert(NC_DIRECTION);
    }
    if up(i).as_deref() == Some("EQUALRECTWIDTH") {
        i += 1;
        unmodelled.get_or_insert(NC_EQUALRECTWIDTH);
    }
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
    let edges = match up(i).as_deref() {
        Some("LONGEDGEONLY") => {
            i += 1;
            EdgeKind::Long
        }
        Some("SHORTEDGEONLY") => {
            i += 1;
            EdgeKind::Short
        }
        _ => EdgeKind::All,
    };
    let overhang = num(t.get(i)).ok_or(NC_MALFORMED)?;
    i += 1;
    let mut exact = None;
    if edges == EdgeKind::All {
        match up(i).as_deref() {
            None => {}
            Some("PARALLELEDGE") => {
                num(t.get(i + 1)).ok_or(NC_MALFORMED)?;
                if up(i + 2).as_deref() != Some("EXTENSION") {
                    return Err(NC_MALFORMED);
                }
                num(t.get(i + 3)).ok_or(NC_MALFORMED)?;
                num(t.get(i + 4)).ok_or(NC_MALFORMED)?;
                i += 5;
                if up(i).as_deref() == Some("MINLENGTH") {
                    num(t.get(i + 1)).ok_or(NC_MALFORMED)?;
                    i += 2;
                }
                unmodelled.get_or_insert(NC_PARALLELEDGE);
            }
            Some("MINLENGTH") => {
                num(t.get(i + 1)).ok_or(NC_MALFORMED)?;
                i += 2;
                unmodelled.get_or_insert(NC_MINLENGTH);
            }
            Some("ALLSIDES") => {
                i += 1;
                unmodelled.get_or_insert(NC_ALLSIDES);
            }
            Some(_) => {
                exact = Some(num(t.get(i)).ok_or(NC_MALFORMED)?);
                i += 1;
            }
        }
    }
    if i != t.len() {
        return Err(NC_MALFORMED);
    }
    if let Some(c) = unmodelled {
        return Err(c);
    }
    Ok(Rule { eol_width, cut_class, side, edges, overhang, exact })
}

// ---- rules of a layer in DBU ----------------------------------------------------------------------

/// A checked rule in DBU, with its index among the layer's CHECKED rules, in the order written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleDbu {
    pub index: usize,
    /// The cut class's `(name, width, length)`; `None` for every cut.
    pub class: Option<(String, i32, i32)>,
    /// The routing layers the rule holds on (one or both neighbours).
    pub metals: Vec<String>,
    pub edges: EdgeKind,
    pub eol_width: i32,
    pub overhang: i32,
    pub exact: Option<i32>,
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

    /// The cut edges measured: all four, or the shorter or longer pair of a rectangular cut.
    #[must_use]
    pub fn measured_edges(&self, cut: &Rect) -> Vec<CutEdge> {
        let (w, h) = (cut[2] - cut[0], cut[3] - cut[1]);
        // W/E edges have length h; S/N edges have length w
        let we_shorter = h < w;
        match self.edges {
            EdgeKind::All => CutEdge::ALL.to_vec(),
            _ if w == h => Vec::new(),
            EdgeKind::Short if we_shorter => vec![CutEdge::W, CutEdge::E],
            EdgeKind::Short => vec![CutEdge::S, CutEdge::N],
            EdgeKind::Long if we_shorter => vec![CutEdge::S, CutEdge::N],
            EdgeKind::Long => vec![CutEdge::W, CutEdge::E],
        }
    }

    /// The overhang fails the rule: below `overhang` and not exactly `exactOverhang`.
    #[must_use]
    pub fn fails(&self, reach: i32) -> bool {
        reach < self.overhang && self.exact != Some(reach)
    }
}

fn dbu(um: f64, per_um: f64) -> i32 {
    (um * per_um).round() as i32
}

/// One cut layer's rules: the property read, classes and sides resolved.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LayerRules {
    pub cut_layer: String,
    pub below_layer: Option<String>,
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
    below_layer: Option<&str>,
    above_layer: Option<&str>,
    read: (Vec<Rule>, Vec<NotChecked>),
    classes: &[(String, i32, i32)],
    per_um: f64,
) -> LayerRules {
    let (rules, mut not_checked) = read;
    let statements = rules.len() + not_checked.len();
    let mut out = Vec::new();
    for (index, r) in rules.into_iter().enumerate() {
        let statement = format!("EOLENCLOSURE {} … {}", r.eol_width, r.overhang);
        let metals: Vec<&str> = match r.side {
            Side::Above => above_layer.into_iter().collect(),
            Side::Below => below_layer.into_iter().collect(),
            Side::Both => below_layer.into_iter().chain(above_layer).collect(),
        };
        let metals: Vec<String> = metals.into_iter().map(str::to_string).collect();
        if metals.is_empty() {
            not_checked.push(NotChecked { clause: NC_NO_LAYER, statement });
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
        let rectangular = class.as_ref().is_some_and(|c| c.1 != c.2);
        if r.edges != EdgeKind::All && !rectangular {
            not_checked.push(NotChecked { clause: NC_EDGE_KIND_SQUARE, statement });
            continue;
        }
        out.push(RuleDbu {
            index,
            class,
            metals,
            edges: r.edges,
            eol_width: dbu(r.eol_width, per_um),
            overhang: dbu(r.overhang, per_um),
            exact: r.exact.map(|e| dbu(e, per_um)),
        });
    }
    LayerRules {
        cut_layer: cut_layer.to_string(),
        below_layer: below_layer.map(str::to_string),
        above_layer: above_layer.map(str::to_string),
        rules: out,
        not_checked,
        statements,
    }
}

// ---- geometry -----------------------------------------------------------------------------------

/// One stretch `[a, b]` of a cut edge (along the edge's axis) and how far the metal reaches
/// outward over all of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Piece {
    pub a: i32,
    pub b: i32,
    pub reach: i32,
}

/// The cut edge split where any maximal rectangle's side crosses it, each piece with the farthest
/// reach of a maximal rectangle spanning it. `None` when some piece has no metal over it, or the
/// edge is degenerate. (Same measure as [`crate::enclosure_edge::edge_overhang`], kept per piece.)
#[must_use]
pub fn edge_pieces(cut: &Rect, edge: CutEdge, rects: &[Rect]) -> Option<Vec<Piece>> {
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
    let mut out = Vec::new();
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let reach = rects
            .iter()
            .filter(|r| r[alo] <= a && r[ahi] >= b && r[plo] <= line && r[phi] >= line)
            .map(|r| if outward_high { r[phi] - line } else { line - r[plo] })
            .max()?;
        out.push(Piece { a, b, reach });
    }
    (!out.is_empty()).then_some(out)
}

/// Whether the metal boundary at `piece.reach` beyond a cut edge, over the piece's stretch of it,
/// is an EOL edge: a boundary edge facing the same way, on that line, overlapping the stretch with
/// positive length, shorter than `eol_width`, both corners convex, and not on the clip `window`.
#[must_use]
pub fn eol_edge_at(model: &LayerModel, window: &Rect, cut: &Rect, edge: CutEdge, piece: &Piece, eol_width: i32) -> bool {
    let Piece { a, b, reach: dist } = *piece;
    let (out, line) = match edge {
        CutEdge::W => ((-1, 0), cut[0] - dist),
        CutEdge::E => ((1, 0), cut[2] + dist),
        CutEdge::S => ((0, -1), cut[1] - dist),
        CutEdge::N => ((0, 1), cut[3] + dist),
    };
    model.edges.iter().any(|e| {
        let (dx, dy) = ((e.high.0 - e.low.0).signum(), (e.high.1 - e.low.1).signum());
        // the interior is on an edge's left, so its outward normal is (dy, -dx)
        if (dy, -dx) != out {
            return false;
        }
        let (at, e0, e1) = if out.0 == 0 {
            (e.low.1, e.low.0.min(e.high.0), e.low.0.max(e.high.0))
        } else {
            (e.low.0, e.low.1.min(e.high.1), e.low.1.max(e.high.1))
        };
        let clip = if out.0 == 0 { at == window[1] || at == window[3] } else { at == window[0] || at == window[2] };
        at == line
            && !clip
            && e0.max(a) < e1.min(b)
            && e.len() < eol_width
            && turns_left(&model.edges[e.prev], e)
            && turns_left(e, &model.edges[e.next])
    })
}

/// One rule on one cut edge: the smallest overhang onto an EOL edge that fails it, if any.
#[must_use]
pub fn check_edge(model: &LayerModel, window: &Rect, cut: &Rect, edge: CutEdge, pieces: &[Piece], rule: &RuleDbu) -> Option<i32> {
    pieces
        .iter()
        .filter(|p| rule.fails(p.reach))
        .filter(|p| eol_edge_at(model, window, cut, edge, p, rule.eol_width))
        .map(|p| p.reach)
        .min()
}

// ---- findings and the pass ----------------------------------------------------------------------

/// One finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EolFinding {
    /// A cut edge on an EOL edge of `metal` with too little overhang.
    Overhang {
        layer: String,
        metal: String,
        rule: usize,
        net: String,
        cut: Rect,
        edge: CutEdge,
        overhang: i32,
        required: i32,
        exact: Option<i32>,
    },
    /// The metal does not cover the cut; nothing measured on that layer.
    NotEnclosed { layer: String, metal: String, net: String, cut: Rect },
}

impl EolFinding {
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            EolFinding::Overhang { .. } => "eol_enclosure",
            EolFinding::NotEnclosed { .. } => "cut_not_enclosed",
        }
    }

    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            EolFinding::Overhang { layer, metal, rule, net, cut, edge, overhang, required, exact } => {
                serde_json::json!({ "kind": self.kind(), "layer": layer, "metal": metal, "rule": rule, "net": net,
                    "cut": cut, "edge": edge.as_str(), "overhang": overhang, "required": required, "exact": exact })
            }
            EolFinding::NotEnclosed { layer, metal, net, cut } => {
                serde_json::json!({ "kind": self.kind(), "layer": layer, "metal": metal, "net": net, "cut": cut })
            }
        }
    }
}

/// Stage: check one cut layer's cuts against the rules that hold on `metal_layer`, over that
/// layer's metal. Returns the cuts at least one rule applied to, and the findings.
///
/// Per cut the metal is clipped to the cut grown by the farthest any rule looks (its overhang,
/// exact overhang and EOL width) plus one: an edge clipped short at the window keeps at least that
/// much length, so it is never taken for an EOL edge, and a reach cut short there passes.
#[must_use]
pub fn check_layer(rules: &LayerRules, metal_layer: &str, cuts: &[Cut], metal: &[Rect]) -> (usize, Vec<EolFinding>) {
    let mut findings = Vec::new();
    let here: Vec<&RuleDbu> = rules.rules.iter().filter(|r| r.metals.iter().any(|m| m == metal_layer)).collect();
    if here.is_empty() {
        return (0, findings);
    }
    let reach = here.iter().map(|r| r.overhang.max(r.exact.unwrap_or(0)).max(r.eol_width)).max().unwrap_or(0) + 1;
    let grid = Grid::new(metal, i64::from(reach.max(1)) * 8);
    let mut checked = 0;
    for c in cuts {
        let applying: Vec<&&RuleDbu> = here.iter().filter(|r| r.applies(&c.rect)).collect();
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
        let not_enclosed = || EolFinding::NotEnclosed {
            layer: rules.cut_layer.clone(),
            metal: metal_layer.to_string(),
            net: c.net.clone(),
            cut: c.rect,
        };
        if union_area_in(&near, &c.rect) != area {
            findings.push(not_enclosed());
            continue;
        }
        let model = LayerModel::build(&near.iter().map(|&rect| Shape { net: 0, rect, fixed: false }).collect::<Vec<_>>());
        let maxr: Vec<Rect> = model.rects.iter().map(|m| m.rect).collect();
        let mut pieces = BTreeMap::new();
        for e in CutEdge::ALL {
            match edge_pieces(&c.rect, e, &maxr) {
                Some(p) => pieces.insert(e, p),
                None => break,
            };
        }
        if pieces.len() != 4 {
            findings.push(not_enclosed());
            continue;
        }
        for r in applying {
            for e in r.measured_edges(&c.rect) {
                if let Some(overhang) = check_edge(&model, &win, &c.rect, e, &pieces[&e], r) {
                    findings.push(EolFinding::Overhang {
                        layer: rules.cut_layer.clone(),
                        metal: metal_layer.to_string(),
                        rule: r.index,
                        net: c.net.clone(),
                        cut: c.rect,
                        edge: e,
                        overhang,
                        required: r.overhang,
                        exact: r.exact,
                    });
                }
            }
        }
    }
    (checked, findings)
}

/// What one cut layer's pass did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LayerReport {
    pub rules: LayerRules,
    pub cuts: usize,
    /// Per metal layer checked, the cuts at least one rule applied to.
    pub cuts_checked: BTreeMap<String, usize>,
}

/// The whole pass's result.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Report {
    pub layers: Vec<LayerReport>,
    pub findings: Vec<EolFinding>,
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

fn routing_neighbour(db: &Db, cut_layer: &str, step: i64) -> Option<String> {
    let n = db.layer_name_by_number(i64::from(db.layer_get_number(cut_layer)) + step);
    (!n.is_empty() && db.layer_get_type(&n).ok().as_deref() == Some("ROUTING")).then_some(n)
}

/// Stage: every cut layer's EOL enclosure rules, read from the layer's raw property text.
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
        let below = routing_neighbour(db, &name, -1);
        let above = routing_neighbour(db, &name, 1);
        out.push(resolve(&name, below.as_deref(), above.as_deref(), read, &cut_classes(db, &name), per_um));
    }
    Ok(out)
}

/// The pass: read the rules ([`read_rules`]); then per cut layer with a checked rule collect its
/// cuts, and per metal layer a rule holds on, that layer's metal, and check them
/// ([`check_layer`]). Without a block only the rules are read.
pub fn check_eol_enclosures(db: &Db) -> Result<Report> {
    let has_block = !db.block_name().is_empty();
    let mut rep = Report { has_block, ..Report::default() };
    for rules in read_rules(db)? {
        let mut lr = LayerReport { rules, ..LayerReport::default() };
        if has_block && !lr.rules.rules.is_empty() {
            let cuts = collect_cuts(db, i64::from(db.layer_get_number(&lr.rules.cut_layer)))?;
            lr.cuts = cuts.len();
            let metals: Vec<String> = [&lr.rules.below_layer, &lr.rules.above_layer].into_iter().flatten().cloned().collect();
            for m in metals {
                if !lr.rules.rules.iter().any(|r| r.metals.contains(&m)) {
                    continue;
                }
                let metal = collect_metal(db, i64::from(db.layer_get_number(&m)))?;
                let (checked, f) = check_layer(&lr.rules, &m, &cuts, &metal);
                lr.cuts_checked.insert(m, checked);
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

    fn rule(edges: EdgeKind, eol: i32, o: i32, exact: Option<i32>, class: Option<(i32, i32)>) -> RuleDbu {
        RuleDbu {
            index: 0,
            class: class.map(|(w, l)| ("VX".to_string(), w, l)),
            metals: vec!["M2".into()],
            edges,
            eol_width: eol,
            overhang: o,
            exact,
        }
    }

    fn layer(r: RuleDbu) -> LayerRules {
        LayerRules {
            cut_layer: "V1".into(),
            below_layer: Some("M1".into()),
            above_layer: Some("M2".into()),
            rules: vec![r],
            not_checked: vec![],
            statements: 1,
        }
    }

    fn run(r: RuleDbu, cut: Rect, metal: &[Rect]) -> Vec<(CutEdge, i32)> {
        let (_, f) = check_layer(&layer(r), "M2", &[Cut { net: "n".into(), rect: cut }], metal);
        f.into_iter()
            .map(|f| match f {
                EolFinding::Overhang { edge, overhang, .. } => (edge, overhang),
                EolFinding::NotEnclosed { .. } => (CutEdge::W, -1),
            })
            .collect()
    }

    /// The grammar (LEF/DEF 5.8, Layer (Cut), EOL Enclosure Rule), in its written order: the kit's
    /// four forms read as checked rules; every unmodelled clause is named, also when it follows a
    /// part that parses; anything out of order or short does not parse.
    #[test]
    fn statements_are_checked_or_named() {
        let (r, nc) = read_property(
            "EOLENCLOSURE 0.1 CUTCLASS VB SHORTEDGEONLY 0.02 ; EOLENCLOSURE 0.1 CUTCLASS VA BELOW 0.03 ;\n\
             EOLENCLOSURE 0.1 CUTCLASS VB ABOVE LONGEDGEONLY 0.04 ; EOLENCLOSURE 0.15 CUTCLASS VA 0.05 0.0 ;",
        );
        assert!(nc.is_empty(), "{nc:?}");
        let got: Vec<(Side, EdgeKind, f64, Option<f64>)> = r.iter().map(|x| (x.side, x.edges, x.overhang, x.exact)).collect();
        assert_eq!(got, vec![
            (Side::Both, EdgeKind::Short, 0.02, None),
            (Side::Below, EdgeKind::All, 0.03, None),
            (Side::Above, EdgeKind::Long, 0.04, None),
            (Side::Both, EdgeKind::All, 0.05, Some(0.0)),
        ]);
        assert_eq!(r[0].cut_class.as_deref(), Some("VB"));
        assert_eq!(r[0].eol_width, 0.1);
        let clause = |s: &str| {
            let (r, nc) = read_property(s);
            assert!(r.is_empty(), "{s}");
            nc[0].clause
        };
        assert_eq!(clause("EOLENCLOSURE 0.1 MINEOLWIDTH 0.05 CUTCLASS VB SHORTEDGEONLY 0.02 ;"), NC_MINEOLWIDTH);
        assert_eq!(clause("EOLENCLOSURE 0.15 EQUALRECTWIDTH CUTCLASS VA 0.05 0.0 ;"), NC_EQUALRECTWIDTH);
        assert_eq!(clause("EOLENCLOSURE 0.15 VERTICAL CUTCLASS VA 0.05 ;"), NC_DIRECTION);
        assert_eq!(clause("EOLENCLOSURE 0.15 CUTCLASS VA 0.05 PARALLELEDGE 0.18 EXTENSION 0.05 0.07 ;"), NC_PARALLELEDGE);
        assert_eq!(clause("EOLENCLOSURE 0.15 CUTCLASS VA 0.05 PARALLELEDGE 0.18 EXTENSION 0.05 0.07 MINLENGTH 0.1 ;"), NC_PARALLELEDGE);
        assert_eq!(clause("EOLENCLOSURE 0.15 CUTCLASS VA 0.05 MINLENGTH 0.1 ;"), NC_MINLENGTH);
        assert_eq!(clause("EOLENCLOSURE 0.15 0.05 ALLSIDES ;"), NC_ALLSIDES);
        assert_eq!(clause("EOLENCLOSURE 0.15 CUTCLASS VA 0.05 PARALLELEDGE 0.18 0.05 0.07 ;"), NC_MALFORMED);
        assert_eq!(clause("EOLENCLOSURE 0.15 CUTCLASS VA ;"), NC_MALFORMED);
        assert_eq!(clause("EOLENCLOSURE CUTCLASS VA 0.05 ;"), NC_MALFORMED);
        assert_eq!(clause("EOLENCLOSURE 0.15 CUTCLASS VA SHORTEDGEONLY 0.02 0.0 ;"), NC_MALFORMED, "no exact with an edge kind");
        assert_eq!(clause("EOLENCLOSURE 0.15 ABOVE CUTCLASS VA 0.05 ;"), NC_MALFORMED, "out of order");
        assert_eq!(clause("EOLENCLOSURE 0.15 CUTCLASS VA 0.05 0.0 0.01 ;"), NC_MALFORMED);
        assert_eq!(clause("EOLENCLOSURE 0.15 MINEOLWIDTH 0.05 CUTCLASS VA 0.05 FOO ;"), NC_MALFORMED, "malformed wins");
        assert_eq!(clause("ENCLOSURE 0.15 0.05 ;"), NC_MALFORMED);
        assert_eq!(read_property(" ; ;").0.len() + read_property(" ; ;").1.len(), 0);
    }

    /// Resolution: a side with no routing layer, an undefined class, and an edge kind on a square
    /// (or no) class are not checked and named; without ABOVE / BELOW the rule holds on both
    /// neighbours, BELOW on the one below.
    #[test]
    fn resolution_names_what_it_cannot_check() {
        let classes = [("VA".to_string(), 40, 40), ("VB".to_string(), 40, 80)];
        let read = || {
            read_property(
                "EOLENCLOSURE 0.1 CUTCLASS vb SHORTEDGEONLY 0.02 ; EOLENCLOSURE 0.1 CUTCLASS VA BELOW 0.03 ; \
                 EOLENCLOSURE 0.1 CUTCLASS VC 0.03 ; EOLENCLOSURE 0.1 CUTCLASS VA SHORTEDGEONLY 0.02 ; \
                 EOLENCLOSURE 0.1 LONGEDGEONLY 0.02 ; EOLENCLOSURE 0.1 CUTCLASS VA ABOVE 0.03 ;",
            )
        };
        let lr = resolve("V1", Some("M1"), Some("M2"), read(), &classes, 1000.0);
        assert_eq!(lr.statements, 6);
        let got: Vec<(usize, Vec<String>, i32)> = lr.rules.iter().map(|r| (r.index, r.metals.clone(), r.overhang)).collect();
        assert_eq!(got, vec![
            (0, vec!["M1".to_string(), "M2".to_string()], 20),
            (1, vec!["M1".to_string()], 30),
            (5, vec!["M2".to_string()], 30),
        ]);
        assert_eq!(lr.rules[0].class, Some(("VB".into(), 40, 80)));
        assert_eq!(lr.rules[0].eol_width, 100);
        let nc: Vec<&str> = lr.not_checked.iter().map(|n| n.clause).collect();
        assert_eq!(nc, [NC_CLASS, NC_EDGE_KIND_SQUARE, NC_EDGE_KIND_SQUARE]);
        let top = resolve("V9", Some("M9"), None, read(), &classes, 1000.0);
        let nc: Vec<&str> = top.not_checked.iter().map(|n| n.clause).collect();
        assert_eq!(nc.iter().filter(|c| **c == NC_NO_LAYER).count(), 1, "only the ABOVE rule loses its layer: {nc:?}");
        assert_eq!(top.rules.iter().map(|r| r.metals.clone()).collect::<Vec<_>>(), vec![vec!["M9".to_string()]; 2]);
    }

    /// Edge kinds: a 40 x 80 (W x H) cut's short edges are S and N, long are W and E; turned, the
    /// other way; all four without a kind; none on a square cut.
    #[test]
    fn edge_kinds_pick_the_short_or_long_pair() {
        let r = |k| rule(k, 100, 20, None, Some((40, 80)));
        assert_eq!(r(EdgeKind::Short).measured_edges(&[0, 0, 40, 80]), [CutEdge::S, CutEdge::N]);
        assert_eq!(r(EdgeKind::Long).measured_edges(&[0, 0, 40, 80]), [CutEdge::W, CutEdge::E]);
        assert_eq!(r(EdgeKind::Short).measured_edges(&[0, 0, 80, 40]), [CutEdge::W, CutEdge::E]);
        assert_eq!(r(EdgeKind::Long).measured_edges(&[0, 0, 80, 40]), [CutEdge::S, CutEdge::N]);
        assert_eq!(r(EdgeKind::All).measured_edges(&[0, 0, 40, 40]), CutEdge::ALL);
        assert!(r(EdgeKind::Short).measured_edges(&[0, 0, 40, 40]).is_empty());
    }

    /// The comparison (text: "either exactly equal to exactOverhang or greater than or equal to
    /// overhang"): below the overhang fails, at it passes, and the exact value passes.
    #[test]
    fn at_the_overhang_or_exact_passes() {
        let r = rule(EdgeKind::All, 150, 50, Some(0), None);
        assert!(r.fails(49) && r.fails(20) && r.fails(1));
        assert!(!r.fails(50) && !r.fails(200) && !r.fails(0));
        assert!(rule(EdgeKind::All, 150, 50, None, None).fails(0));
    }

    /// Figure 2-74 (EQUALRECTWIDTH left out, so its case c is a violation): rule `EOLENCLOSURE 0.15
    /// CUTCLASS VA 0.05 0.0`, a 0.1-wide horizontal wire end (a) with the cut flush: zero is allowed;
    /// (b) 0.02 short of the end: a violation; (c) a 0.1 x 0.14 pad, the cut on its S edge 0.02 from
    /// W and E: S is 0 (allowed), W and E are 0.14-long EOL edges at 0.02, violations.
    #[test]
    fn figure_2_74_exact_overhang() {
        let r = || rule(EdgeKind::All, 150, 50, Some(0), None);
        let wire = [0, 0, 400, 100];
        assert!(run(r(), [340, 30, 400, 70], &[wire]).is_empty(), "(a) flush with the end");
        assert_eq!(run(r(), [320, 30, 380, 70], &[wire]), vec![(CutEdge::E, 20)], "(b)");
        let pad = [0, 0, 100, 140];
        assert_eq!(run(r(), [20, 0, 80, 60], &[pad]), vec![(CutEdge::W, 20), (CutEdge::E, 20)], "(c)");
    }

    /// Figure 2-73 (LONGEDGEONLY 0.04, EOL width 0.15): (a) a horizontal VB cut in a 0.12-long bar
    /// with a leg hanging from its E end: the bar's top is a 0.12 EOL edge (both corners convex),
    /// the cut's long N edge sits 0.01 under it: a violation. The bar's E side runs on down the
    /// leg, so it is no EOL edge. (b) a vertical VB cut whose short N edge is under the end of a
    /// 0.11-wide wire: not a long edge, OK.
    #[test]
    fn figure_2_73_long_edge_only() {
        let r = || rule(EdgeKind::Long, 150, 40, None, Some((40, 80)));
        let bar = [0, 0, 120, 60];
        let leg = [80, -200, 120, 0];
        assert_eq!(run(r(), [30, 10, 110, 50], &[bar, leg]), vec![(CutEdge::N, 10)]);
        let wire = [0, 0, 110, 200];
        assert!(run(r(), [35, 100, 75, 180], &[wire]).is_empty(), "(b)");
        // the same (b) under SHORTEDGEONLY: the short N edge is on the EOL edge, 20 short of 40
        let s = rule(EdgeKind::Short, 150, 40, None, Some((40, 80)));
        assert_eq!(run(s, [35, 100, 75, 180], &[wire]), vec![(CutEdge::N, 20)]);
    }

    /// The EOL edge: strictly shorter than the EOL width; both corners convex (the end of a wire
    /// that widens behind it still counts, a notch's bottom does not); facing the same way; and
    /// overlapping the cut edge with positive length (a metal end beside the cut is not its end).
    #[test]
    fn what_makes_an_eol_edge() {
        let r = |eol| rule(EdgeKind::All, eol, 50, None, None);
        let wire = [0, 0, 400, 100];
        let cut = [320, 30, 380, 70];
        assert_eq!(run(r(101), cut, &[wire]), vec![(CutEdge::E, 20)]);
        assert!(run(r(100), cut, &[wire]).is_empty(), "the end is exactly the EOL width: not less");
        // the end of a narrow stub on a wide wire: corners convex, an EOL edge
        let wide = [0, -200, 300, 300];
        assert_eq!(run(r(150), cut, &[wide, wire]), vec![(CutEdge::E, 20)]);
        // a notch opening to the E: the boundary beyond E is the notch's bottom, its corners concave
        let notch = [[0, -100, 600, 0], [0, 100, 600, 200], [0, 0, 400, 100]];
        assert!(run(r(150), cut, &notch).is_empty(), "E faces the notch: no convex end");
        // overlap with positive length: the wire's E end spans y 0..100; a stretch meeting it at
        // y = 100 only is beside it, a stretch overlapping it by 10 is on it
        let m = LayerModel::build(&[Shape { net: 0, rect: wire, fixed: false }]);
        let win = [-1000, -1000, 1000, 1000];
        assert!(!eol_edge_at(&m, &win, &[320, 100, 380, 140], CutEdge::E, &Piece { a: 100, b: 140, reach: 20 }, 150));
        assert!(eol_edge_at(&m, &win, &[320, 90, 380, 140], CutEdge::E, &Piece { a: 90, b: 140, reach: 20 }, 150));
        assert!(!eol_edge_at(&m, &win, &[320, 90, 380, 140], CutEdge::W, &Piece { a: 90, b: 140, reach: 20 }, 150), "faces the other way");
        assert!(!eol_edge_at(&m, &win, &[320, 90, 380, 140], CutEdge::E, &Piece { a: 90, b: 140, reach: 21 }, 150), "not on that line");
    }

    /// The window: a long wire edge cut short by the clip is never an EOL edge, however near the
    /// cut sits; and the reach clipped at the window passes.
    #[test]
    fn the_clip_never_makes_an_end() {
        let r = rule(EdgeKind::All, 150, 50, None, None);
        // a 0.04-tall wire running far both ways: N and S at 30 from the cut, wire edges long
        let wire = [-100_000, 0, 100_000, 100];
        assert!(run(r.clone(), [0, 30, 60, 70], &[wire]).is_empty());
        // the window reaches the EOL width past the cut, not just the overhang: a long wire whose
        // top is 10 over the cut stays long after the clip, so no end is made of it
        let shallow = rule(EdgeKind::All, 150, 20, None, None);
        assert!(run(shallow, [0, 30, 60, 70], &[[-100_000, 0, 100_000, 80]]).is_empty());
        let edges = LayerModel::build(&[Shape { net: 0, rect: [-151, 0, 211, 100], fixed: false }]);
        let win = [-151, -121, 211, 221];
        assert!(!eol_edge_at(&edges, &win, &[0, 30, 60, 70], CutEdge::E, &Piece { a: 30, b: 70, reach: 151 }, 150), "clip edge");
    }

    /// Per piece: a cut whose E edge is half under a short step (at 10) and half under a long run
    /// (at 300). The step's 50-long end meets the run at a concave corner, so under an EOL width of
    /// 60 it is no EOL edge: clean (the shape's other sides are 110 and longer).
    #[test]
    fn the_end_is_judged_where_it_is() {
        let r = rule(EdgeKind::All, 60, 50, None, None);
        let stub = [0, 0, 110, 50];
        let run_on = [0, 50, 400, 120];
        assert!(run(r, [40, 20, 100, 80], &[stub, run_on]).is_empty());
        let m = LayerModel::build(&[Shape { net: 0, rect: stub, fixed: false }, Shape { net: 0, rect: run_on, fixed: false }]);
        let maxr: Vec<Rect> = m.rects.iter().map(|x| x.rect).collect();
        assert_eq!(edge_pieces(&[40, 20, 100, 80], CutEdge::E, &maxr), Some(vec![Piece { a: 20, b: 50, reach: 10 }, Piece { a: 50, b: 80, reach: 300 }]));
        assert_eq!(edge_pieces(&[500, 20, 560, 80], CutEdge::E, &maxr), None);
        assert_eq!(edge_pieces(&[40, 20, 40, 80], CutEdge::S, &maxr), None, "degenerate");
    }

    /// Two prongs over one cut edge, 5 and 15 past it, each ending in its own short end (the notch
    /// between them reaches down to the edge, its corners concave): both fail, and the edge is
    /// reported once, at its SMALLEST overhang.
    #[test]
    fn one_finding_per_edge_at_its_smallest() {
        let r = rule(EdgeKind::All, 150, 50, None, None);
        let base = [-50, -50, 150, 40];
        let (a, b) = ([0, 40, 40, 45], [60, 40, 100, 55]);
        assert_eq!(run(r, [0, 0, 100, 40], &[base, a, b]), vec![(CutEdge::N, 5)]);
    }

    /// A cut the metal does not cover is reported once and not measured; a cut of another class is
    /// not looked at; a rule holding on M1 only is not run on M2.
    #[test]
    fn uncovered_and_out_of_scope_cuts() {
        let r = rule(EdgeKind::All, 150, 50, None, Some((60, 40)));
        let wire = [0, 0, 400, 100];
        let (n, f) = check_layer(&layer(r.clone()), "M2", &[Cut { net: "a".into(), rect: [380, 30, 440, 70] }], &[wire]);
        assert_eq!((n, f.iter().map(EolFinding::kind).collect::<Vec<_>>()), (1, vec!["cut_not_enclosed"]));
        let (n, f) = check_layer(&layer(r.clone()), "M2", &[Cut { net: "a".into(), rect: [320, 30, 360, 70] }], &[wire]);
        assert_eq!((n, f.len()), (0, 0), "a 40 x 40 cut is not of the 60 x 40 class");
        // a ring: every cut edge lies on metal, its middle over the hole: not enclosed either
        let ring = [[0, 0, 200, 40], [0, 160, 200, 200], [0, 0, 40, 200], [160, 0, 200, 200]];
        let any = rule(EdgeKind::All, 150, 50, None, None);
        let (_, f) = check_layer(&layer(any), "M2", &[Cut { net: "a".into(), rect: [20, 20, 180, 180] }], &ring);
        assert_eq!(f.iter().map(EolFinding::kind).collect::<Vec<_>>(), vec!["cut_not_enclosed"]);
        let below = RuleDbu { metals: vec!["M1".into()], ..r };
        let (n, f) = check_layer(&layer(below), "M2", &[Cut { net: "a".into(), rect: [320, 30, 380, 70] }], &[wire]);
        assert_eq!((n, f.len()), (0, 0));
    }
}
