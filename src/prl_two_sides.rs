// SPDX-License-Identifier: Apache-2.0
//! LEF58 PRL two-sides cut spacing (`PROPERTY LEF58_PRLTWOSIDESSPACING`) on a cut layer: a
//! pre-screen checker. It reports; it never moves, adds or removes a shape.
//!
//! odb (pin 4155ac31, and master as read 2026-11-09) builds no rule from this property and keeps
//! only its raw text as a string property on the layer; no router or checker of the reference
//! reads it. There is no reference behaviour to match, so the rules are read from that raw text
//! ([`Db::layer_string_properties`]) and checked as the LEF/DEF 5.8 language reference defines them
//! (Layer (Cut), "PRL Two Sides Spacing Rule", its text and Figure 2-155):
//!
//! > The PRL two sides spacing rule specifies that the center-to-center spacing between a cut of
//! > className1 and another cut of className2 that are greater than or equal to prl1 and less than
//! > or equal to prl2 on one side, and greater than or equal to prl3 and less than or equal to prl4
//! > on the other side to be prlSpacing. Otherwise, the center-to-center spacing of nonPrlSpacing
//! > should be applied on them. This spacing is checked in addition to the spacing defined in
//! > CUTCLASS SPACINGTABLE.
//!
//! Checked: `PRLTWOSIDESSPACING prlSpacing nonPrlSpacing CUTCLASS c1 TO c2 PRL p1 p2 p3 p4`
//! (Figure 2-155 spells the keyword `PRLTWOSIDES`; both are read). Every other form, a class the
//! layer does not define, and anything that does not parse is reported as not checked
//! ([`NotChecked`]). The property is also read under the name the reference's syntax summary
//! prints, `LEF58_PRLTWOSIDESPACING`.
//!
//! Interpretations (the text leaves these open; no oracle decides them; each pinned in a test):
//! * **PRL** of two cuts along an axis = the overlap of their extents on it, negative for a gap
//!   (Figure 2-155: two cuts apart by 0.09 in x and 0.06 in y have PRLs -0.09 and -0.06).
//! * **"On one side … on the other side"** = one axis's PRL in `[p1, p2]` and the other axis's in
//!   `[p3, p4]`, either way round (the text names no axis), closed at both ends.
//! * **Spacing** = Euclidean distance between the cut centres; a pair fails when it is strictly
//!   below the spacing that applies ("to be prlSpacing": at it is legal; Figure 2-155 a, 0.16 >=
//!   0.15, is OK).
//! * Every pair of distinct cuts of the two classes, any nets (the text has no same-net
//!   exemption); with `c1 TO c1` each unordered pair once. A cut collected twice at the same
//!   place is one cut.
//! * Cuts = routed-wire and special-wire (block) via cuts, as [`crate::enclosure_edge`] collects
//!   them; a cut belongs to a class when its sides are the class's width and length, either way
//!   round.

#![deny(clippy::unwrap_used)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

use std::collections::BTreeMap;

use crate::enclosure_edge::{collect_cuts, cut_classes, Cut, Grid, NotChecked};
use crate::masks_geom::Rect;
use crate::{Db, Result};

/// The layer property this module reads, and the spelling of the reference's syntax summary.
pub const PROPERTIES: [&str; 2] = ["LEF58_PRLTWOSIDESSPACING", "LEF58_PRLTWOSIDESPACING"];

pub const NC_CLASS: &str = "CUTCLASS not defined on the layer";
pub const NC_MALFORMED: &str = "does not parse";

/// One checked statement, in microns as written.
#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    pub prl_spacing: f64,
    pub non_prl_spacing: f64,
    pub class1: String,
    pub class2: String,
    /// `[p1, p2, p3, p4]`; PRLs may be negative.
    pub prl: [f64; 4],
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

fn spacing(t: Option<&&str>) -> Option<f64> {
    t.and_then(|s| s.parse::<f64>().ok()).filter(|v| v.is_finite() && *v >= 0.0)
}

fn signed(t: Option<&&str>) -> Option<f64> {
    t.and_then(|s| s.parse::<f64>().ok()).filter(|v| v.is_finite())
}

fn read_statement(t: &[&str]) -> std::result::Result<Rule, &'static str> {
    let up = |i: usize| t.get(i).map(|s| s.to_ascii_uppercase());
    if up(0).as_deref() != Some("PRLTWOSIDESSPACING") || t.len() != 12 {
        return Err(NC_MALFORMED);
    }
    let prl_spacing = spacing(t.get(1)).ok_or(NC_MALFORMED)?;
    let non_prl_spacing = spacing(t.get(2)).ok_or(NC_MALFORMED)?;
    if up(3).as_deref() != Some("CUTCLASS") || up(5).as_deref() != Some("TO") || !matches!(up(7).as_deref(), Some("PRL" | "PRLTWOSIDES")) {
        return Err(NC_MALFORMED);
    }
    let mut prl = [0.0; 4];
    for (k, p) in prl.iter_mut().enumerate() {
        *p = signed(t.get(8 + k)).ok_or(NC_MALFORMED)?;
    }
    Ok(Rule { prl_spacing, non_prl_spacing, class1: t[4].to_string(), class2: t[6].to_string(), prl })
}

// ---- rules of a layer in DBU ----------------------------------------------------------------------

/// A checked rule in DBU, with its index among the layer's CHECKED rules, in the order written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleDbu {
    pub index: usize,
    /// The two classes as `(name, width, length)`.
    pub class1: (String, i32, i32),
    pub class2: (String, i32, i32),
    pub prl_spacing: i32,
    pub non_prl_spacing: i32,
    pub prl: [i32; 4],
}

fn of_class(c: &(String, i32, i32), cut: &Rect) -> bool {
    let (a, b) = (cut[2] - cut[0], cut[3] - cut[1]);
    (a, b) == (c.1, c.2) || (a, b) == (c.2, c.1)
}

impl RuleDbu {
    /// The PRL condition: one axis's PRL in `[p1, p2]` and the other's in `[p3, p4]`, either way
    /// round, all closed.
    #[must_use]
    pub fn parallel(&self, prl_x: i32, prl_y: i32) -> bool {
        let [p1, p2, p3, p4] = self.prl;
        let a = |v: i32| (p1..=p2).contains(&v);
        let b = |v: i32| (p3..=p4).contains(&v);
        (a(prl_x) && b(prl_y)) || (a(prl_y) && b(prl_x))
    }

    /// The spacing that applies to a pair with these PRLs.
    #[must_use]
    pub fn required(&self, prl_x: i32, prl_y: i32) -> i32 {
        if self.parallel(prl_x, prl_y) {
            self.prl_spacing
        } else {
            self.non_prl_spacing
        }
    }
}

fn dbu(um: f64, per_um: f64) -> i32 {
    (um * per_um).round() as i32
}

/// One cut layer's rules: the property read, classes resolved.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LayerRules {
    pub cut_layer: String,
    pub rules: Vec<RuleDbu>,
    pub not_checked: Vec<NotChecked>,
    /// Statements stated on the layer, checked or not.
    pub statements: usize,
}

/// Resolve read rules for one cut layer. `classes` are the layer's cut classes as
/// `(name, width, length)` in DBU.
#[must_use]
pub fn resolve(cut_layer: &str, read: (Vec<Rule>, Vec<NotChecked>), classes: &[(String, i32, i32)], per_um: f64) -> LayerRules {
    let (rules, mut not_checked) = read;
    let statements = rules.len() + not_checked.len();
    let find = |name: &str| classes.iter().find(|c| c.0.eq_ignore_ascii_case(name)).cloned();
    let mut out = Vec::new();
    for (index, r) in rules.into_iter().enumerate() {
        let (Some(class1), Some(class2)) = (find(&r.class1), find(&r.class2)) else {
            not_checked.push(NotChecked { clause: NC_CLASS, statement: format!("PRLTWOSIDESSPACING {} {} …", r.prl_spacing, r.non_prl_spacing) });
            continue;
        };
        out.push(RuleDbu {
            index,
            class1,
            class2,
            prl_spacing: dbu(r.prl_spacing, per_um),
            non_prl_spacing: dbu(r.non_prl_spacing, per_um),
            prl: r.prl.map(|p| dbu(p, per_um)),
        });
    }
    LayerRules { cut_layer: cut_layer.to_string(), rules: out, not_checked, statements }
}

// ---- findings and the pass ----------------------------------------------------------------------

/// The PRLs of two cuts, `(x, y)`: the overlap of their extents, negative for a gap.
#[must_use]
pub fn prls(a: &Rect, b: &Rect) -> (i32, i32) {
    (a[2].min(b[2]) - a[0].max(b[0]), a[3].min(b[3]) - a[1].max(b[1]))
}

/// Whether two cuts' centres are strictly closer than `spacing`: exact, in doubled coordinates
/// (a centre may sit on a half DBU).
#[must_use]
pub fn closer_than(a: &Rect, b: &Rect, spacing: i32) -> bool {
    let dx = i64::from(a[0]) + i64::from(a[2]) - i64::from(b[0]) - i64::from(b[2]);
    let dy = i64::from(a[1]) + i64::from(a[3]) - i64::from(b[1]) - i64::from(b[3]);
    let s = 2 * i64::from(spacing);
    dx * dx + dy * dy < s * s
}

/// One finding: two cuts whose centres are closer than the spacing that applies to them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrlFinding {
    pub layer: String,
    pub rule: usize,
    pub nets: (String, String),
    pub cuts: (Rect, Rect),
    pub prl: (i32, i32),
    /// The PRL condition held, so `prlSpacing` applied.
    pub parallel: bool,
    pub required: i32,
}

impl PrlFinding {
    #[must_use]
    pub fn kind(&self) -> &'static str {
        "prl_two_sides_spacing"
    }

    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({ "kind": self.kind(), "layer": self.layer, "rule": self.rule,
            "nets": [self.nets.0, self.nets.1], "cuts": [self.cuts.0, self.cuts.1],
            "prl": [self.prl.0, self.prl.1], "parallel": self.parallel, "required": self.required })
    }
}

/// Stage: every pair of a rule's classes on one cut layer. Returns the cuts at least one rule
/// applies to, and the findings, rule by rule in the order written, pairs in cut order.
#[must_use]
pub fn check_layer(rules: &LayerRules, cuts: &[Cut]) -> (usize, Vec<PrlFinding>) {
    let mut findings = Vec::new();
    // one cut per place: a cut collected twice is the same cut
    let mut uniq: Vec<&Cut> = Vec::new();
    for c in cuts {
        if !uniq.iter().any(|u| u.rect == c.rect) {
            uniq.push(c);
        }
    }
    let applied = uniq.iter().filter(|c| rules.rules.iter().any(|r| of_class(&r.class1, &c.rect) || of_class(&r.class2, &c.rect))).count();
    let rects: Vec<Rect> = uniq.iter().map(|c| c.rect).collect();
    for r in &rules.rules {
        let reach = r.prl_spacing.max(r.non_prl_spacing);
        let grid = Grid::new(&rects, i64::from(reach.max(1)) * 4);
        for (i, a) in uniq.iter().enumerate() {
            if !of_class(&r.class1, &a.rect) {
                continue;
            }
            let q = [a.rect[0] - reach, a.rect[1] - reach, a.rect[2] + reach, a.rect[3] + reach];
            for j in grid.query(&q) {
                let b = uniq[j];
                // c1 TO c1: each unordered pair once
                let same = r.class1.0.eq_ignore_ascii_case(&r.class2.0);
                if j == i || !of_class(&r.class2, &b.rect) || (same && j < i) {
                    continue;
                }
                let (px, py) = prls(&a.rect, &b.rect);
                let required = r.required(px, py);
                if closer_than(&a.rect, &b.rect, required) {
                    findings.push(PrlFinding {
                        layer: rules.cut_layer.clone(),
                        rule: r.index,
                        nets: (a.net.clone(), b.net.clone()),
                        cuts: (a.rect, b.rect),
                        prl: (px, py),
                        parallel: r.parallel(px, py),
                        required,
                    });
                }
            }
        }
    }
    (applied, findings)
}

/// What one cut layer's pass did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LayerReport {
    pub rules: LayerRules,
    pub cuts: usize,
    /// Cuts of a class some rule names.
    pub cuts_checked: usize,
}

/// The whole pass's result.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Report {
    pub layers: Vec<LayerReport>,
    pub findings: Vec<PrlFinding>,
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

/// Stage: every cut layer's PRL two-sides rules, read from the layer's raw property text.
pub fn read_rules(db: &Db) -> Result<Vec<LayerRules>> {
    let per_um = f64::from(db.dbu_per_micron());
    let mut out = Vec::new();
    for (name, _) in db.layers_with_direction()? {
        if db.layer_get_type(&name)? != "CUT" {
            continue;
        }
        let props = db.layer_string_properties(&name);
        let texts: Vec<&str> = props
            .iter()
            .filter(|(k, _)| PROPERTIES.iter().any(|p| k.eq_ignore_ascii_case(p)))
            .map(|(_, v)| v.as_str())
            .collect();
        if texts.is_empty() {
            continue;
        }
        let read = read_property(&texts.join(" ; "));
        out.push(resolve(&name, read, &cut_classes(db, &name), per_um));
    }
    Ok(out)
}

/// The pass: read the rules ([`read_rules`]); then per cut layer with a checked rule collect its
/// cuts and check every pair ([`check_layer`]). Without a block only the rules are read.
pub fn check_prl_two_sides(db: &Db) -> Result<Report> {
    let has_block = !db.block_name().is_empty();
    let mut rep = Report { has_block, ..Report::default() };
    for rules in read_rules(db)? {
        let mut lr = LayerReport { rules, ..LayerReport::default() };
        if has_block && !lr.rules.rules.is_empty() {
            let cuts = collect_cuts(db, i64::from(db.layer_get_number(&lr.rules.cut_layer)))?;
            let (checked, f) = check_layer(&lr.rules, &cuts);
            lr.cuts = cuts.len();
            lr.cuts_checked = checked;
            rep.findings.extend(f);
        }
        rep.layers.push(lr);
    }
    Ok(rep)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn va() -> (String, i32, i32) {
        ("VA".to_string(), 40, 40)
    }

    /// Figure 2-155's rule in DBU: 0.15 / 0.2, VA TO VA, PRL -0.07 -0.05 -0.1 -0.08.
    fn fig() -> RuleDbu {
        RuleDbu { index: 0, class1: va(), class2: va(), prl_spacing: 150, non_prl_spacing: 200, prl: [-70, -50, -100, -80] }
    }

    fn layer(r: RuleDbu) -> LayerRules {
        LayerRules { cut_layer: "V1".into(), rules: vec![r], not_checked: vec![], statements: 1 }
    }

    fn cut(net: &str, x: i32, y: i32) -> Cut {
        Cut { net: net.into(), rect: [x, y, x + 40, y + 40] }
    }

    /// The grammar: the kit's form and the figure's keyword read; anything else does not parse.
    #[test]
    fn statements_are_checked_or_named() {
        let (r, nc) = read_property(
            "PRLTWOSIDESSPACING 0.15 0.2 CUTCLASS VA TO VA PRL -0.07 -0.05 -0.1 -0.08 ;\n\
             PRLTWOSIDESSPACING 0.15 0.2 CUTCLASS VA TO VB PRLTWOSIDES -0.07 -0.05 -0.1 -0.08 ;",
        );
        assert!(nc.is_empty(), "{nc:?}");
        assert_eq!(r[0], Rule { prl_spacing: 0.15, non_prl_spacing: 0.2, class1: "VA".into(), class2: "VA".into(), prl: [-0.07, -0.05, -0.1, -0.08] });
        assert_eq!((r[1].class1.as_str(), r[1].class2.as_str()), ("VA", "VB"));
        let clause = |s: &str| {
            let (r, nc) = read_property(s);
            assert!(r.is_empty(), "{s}");
            nc[0].clause
        };
        assert_eq!(clause("PRLTWOSIDESSPACING 0.15 0.2 CUTCLASS VA TO VA PRL -0.07 -0.05 -0.1 ;"), NC_MALFORMED);
        assert_eq!(clause("PRLTWOSIDESSPACING 0.15 0.2 CUTCLASS VA VA PRL -0.07 -0.05 -0.1 -0.08 ;"), NC_MALFORMED);
        assert_eq!(clause("PRLTWOSIDESSPACING 0.15 0.2 CUTCLASS VA AND VA PRL -0.07 -0.05 -0.1 -0.08 ;"), NC_MALFORMED);
        assert_eq!(clause("PRLTWOSIDESSPACING 0.15 0.2 CUTCLASS VA TO VA PRL -0.07 -0.05 -0.1 -0.08 0.1 ;"), NC_MALFORMED);
        assert_eq!(clause("PRLTWOSIDESSPACING -0.15 0.2 CUTCLASS VA TO VA PRL -0.07 -0.05 -0.1 -0.08 ;"), NC_MALFORMED);
        assert_eq!(clause("PRLTWOSIDESSPACING 0.15 0.2 CUTCLASS VA TO VA WITHIN -0.07 -0.05 -0.1 -0.08 ;"), NC_MALFORMED);
        assert_eq!(clause("PRLTWOSIDESSPACING 0.15 0.2 LAYER VA TO VA PRL -0.07 -0.05 -0.1 -0.08 ;"), NC_MALFORMED);
        assert_eq!(read_property(" ; ;").0.len() + read_property(" ; ;").1.len(), 0);
    }

    /// Classes resolve case-insensitively to DBU; a rule naming a class the layer lacks is not
    /// checked and named.
    #[test]
    fn resolution_names_what_it_cannot_check() {
        let read = read_property(
            "PRLTWOSIDESSPACING 0.15 0.2 CUTCLASS va TO VA PRL -0.07 -0.05 -0.1 -0.08 ; \
             PRLTWOSIDESSPACING 0.15 0.2 CUTCLASS VA TO VC PRL -0.07 -0.05 -0.1 -0.08 ;",
        );
        let lr = resolve("V1", read, &[va()], 1000.0);
        assert_eq!(lr.statements, 2);
        assert_eq!(lr.rules, vec![fig()]);
        assert_eq!(lr.not_checked.iter().map(|n| n.clause).collect::<Vec<_>>(), [NC_CLASS]);
    }

    /// PRL is the extent overlap, negative for a gap; the condition takes one axis in each range,
    /// either way round, closed at both ends.
    #[test]
    fn the_prl_condition_is_two_sided_and_closed() {
        assert_eq!(prls(&[0, 0, 40, 40], &[130, 100, 170, 140]), (-90, -60));
        assert_eq!(prls(&[0, 0, 40, 40], &[20, 30, 60, 70]), (20, 10));
        let r = fig();
        assert!(r.parallel(-90, -60) && r.parallel(-60, -90), "either way round");
        assert!(r.parallel(-100, -70) && r.parallel(-80, -50), "closed at the ends");
        assert!(!r.parallel(-101, -60) && !r.parallel(-90, -49) && !r.parallel(-60, -60) && !r.parallel(-90, -90));
        assert_eq!((r.required(-90, -60), r.required(-60, -60)), (150, 200));
    }

    /// Centre-to-centre, Euclidean, strict; exact on half-DBU centres.
    #[test]
    fn spacing_is_euclidean_between_centres_and_strict() {
        let a = [0, 0, 40, 40];
        assert!(closer_than(&a, &[90, 120, 130, 160], 151), "centres 90 x 120 apart: 150");
        assert!(!closer_than(&a, &[90, 120, 130, 160], 150), "at the spacing is legal");
        assert!(closer_than(&a, &[0, 0, 41, 41], 1), "half-DBU centre: 0.5 apart");
        assert!(!closer_than(&a, &[1, 0, 41, 40], 1));
    }

    /// Figure 2-155 a: two VA cuts 0.09 apart in x and 0.06 in y fulfil the PRL condition and their
    /// centres are 0.164 apart (>= 0.15): OK. 0.08 x 0.06 apart: still in the ranges, centres 0.156
    /// apart: OK. 0.05 x 0.05 apart: x is not in [-0.1, -0.08], so nonPrlSpacing 0.2 applies to
    /// centres 0.127 apart: a finding.
    #[test]
    fn figure_2_155() {
        let run = |b: Cut| check_layer(&layer(fig()), &[cut("a", 0, 0), b]).1;
        // 90 gap in x, 60 in y: centres 130 x 100 apart = 164
        assert!(run(cut("b", 130, 100)).is_empty());
        let f = run(cut("b", 90, 90));
        assert_eq!(f.len(), 1);
        assert_eq!((f[0].prl, f[0].parallel, f[0].required), ((-50, -50), false, 200));
        assert_eq!(f[0].nets, ("a".to_string(), "b".to_string()));
        assert!(run(cut("b", 120, 100)).is_empty());
    }

    /// Pairs: each unordered pair once for `c1 TO c1`; any nets, the same net too; a cut of
    /// another class, and a cut collected twice, are not pairs; `c1 TO c2` pairs only c1 with c2,
    /// in both placements.
    #[test]
    fn the_pairs_looked_at() {
        let close = [cut("a", 0, 0), cut("a", 100, 0)];
        let (n, f) = check_layer(&layer(fig()), &close);
        assert_eq!((n, f.len()), (2, 1), "same net, one pair, aligned (PRL 40 in y): 100 < 200");
        let twice = [cut("a", 0, 0), cut("b", 0, 0)];
        assert_eq!(check_layer(&layer(fig()), &twice), (1, vec![]));
        let vb = Cut { net: "b".into(), rect: [100, 0, 140, 80] };
        assert_eq!(check_layer(&layer(fig()), &[cut("a", 0, 0), vb.clone()]), (1, vec![]), "a VB cut is not a VA");
        let ab = RuleDbu { class2: ("VB".to_string(), 40, 80), ..fig() };
        let (n, f) = check_layer(&layer(ab.clone()), &[cut("a", 0, 0), vb.clone()]);
        assert_eq!((n, f.len()), (2, 1));
        assert_eq!(f[0].nets, ("a".to_string(), "b".to_string()), "c1 first");
        let (_, f) = check_layer(&layer(ab), &[vb, cut("a", 0, 0)]);
        assert_eq!(f.len(), 1, "order of collection does not matter");
        // a VB cut turned (80 wide, 40 tall) is still a VB
        let turned = Cut { net: "b".into(), rect: [100, 0, 180, 40] };
        let ab = RuleDbu { class2: ("VB".to_string(), 40, 80), ..fig() };
        assert_eq!(check_layer(&layer(ab), &[cut("a", 0, 0), turned]).1.len(), 1);
        // far apart: nothing
        assert!(check_layer(&layer(fig()), &[cut("a", 0, 0), cut("b", 1000, 0)]).1.is_empty());
    }

    /// The search reaches the LARGER of the two spacings: two aligned cuts 195 apart (centres),
    /// their edges 155 apart, beyond prlSpacing 150 but under nonPrlSpacing 200, placed across a
    /// bin boundary of a grid sized for the smaller spacing.
    #[test]
    fn the_search_reaches_the_larger_spacing() {
        let a = Cut { net: "a".into(), rect: [1008, 0, 1048, 40] };
        let b = Cut { net: "b".into(), rect: [1203, 0, 1243, 40] };
        let f = check_layer(&layer(fig()), &[a, b]).1;
        assert_eq!(f.len(), 1);
        assert_eq!((f[0].prl, f[0].required), ((-155, 40), 200));
    }
}
