// SPDX-License-Identifier: Apache-2.0
//! Forbidden spacing and EOL-extension spacing on the merged-shape model, correlated with the
//! reference detailed router's own checker.
//!
//! 🔑 Oracle: drt `check_drc` (OpenROAD at the pin, one thread) on `tests/fixtures/masks/
//! shape_rules.{lef,def}` — a synthetic two-layer design with the rules WITHOUT `SAMEMASK`
//! (which drt checks; the SAMEMASK forms it never sees). Our checker runs the same rules on the
//! same shapes taken as one mask, and must give the same markers: the box, the rule and the
//! source nets, which is what drt's `addMarker` keys a marker by. The expected lists below are
//! drt's report, converted to DBU.
//!
//! The design: on M1 (horizontal, `FORBIDDENSPACING 0.2 0.3 WIDTH 0.1 PRL 0.15 TWOEDGES 0.25`)
//! wires a, b at a forbidden 250 apart with c in the gap, c, e likewise with b in the gap, and a
//! T-shaped net f over h and k; d, g, h, e have nothing within TWOEDGES on one side. On M2
//! (vertical, `EOLEXTENSIONSPACING`) an end-to-end pair p, q with a side neighbour r, an L-shaped
//! net t beside u and below v, and a lone s.
#![cfg(unix)]

use std::collections::BTreeSet;
use vyges_loom::lef58_samemask::SameMaskRules;
use vyges_opendb::masks_check::{check_shape_rules, SameMaskRulesDbu, RULE_EOL_EXTENSION, RULE_FORBIDDEN};
use vyges_opendb::masks_geom::{LayerModel, Shape};
use vyges_opendb::Db;

fn fixture(f: &str) -> String {
    format!("{}/tests/fixtures/masks/{f}", env!("CARGO_MANIFEST_DIR"))
}

/// Every marker of the design as `(rule, bbox, nets)`, M1 then M2.
fn markers(forbidden: &str, eol: &str) -> BTreeSet<(&'static str, [i32; 4], Vec<String>)> {
    let mut db = Db::new();
    db.read_lef(fixture("shape_rules.lef")).unwrap();
    db.read_def(fixture("shape_rules.def"), "default").unwrap();
    let mut nets: Vec<String> = db.block_get_nets();
    nets.sort();
    // as in the LEF: forbidden spacing on M1, EOL extension on M2
    let mut m1 = SameMaskRules::default();
    m1.read_property("LEF58_FORBIDDENSPACING", forbidden);
    let mut m2 = SameMaskRules::default();
    m2.read_property("LEF58_EOLEXTENSIONSPACING", eol);
    let mut out = BTreeSet::new();
    for (layer, horizontal, rules) in [("M1", true, m1), ("M2", false, m2)] {
        let rules = SameMaskRulesDbu::from_um(&rules, 1000.0);
        let number = db.layer_get_number(layer) as i64;
        let mut shapes = Vec::new();
        for (n, net) in nets.iter().enumerate() {
            for b in db.net_wire_boxes(net).iter().filter(|b| b.layer == number) {
                shapes.push(Shape { net: n, rect: [b.x0, b.y0, b.x1, b.y1], fixed: false });
            }
        }
        let model = LayerModel::build(&shapes);
        for m in check_shape_rules(&model, horizontal, &rules, &nets) {
            out.insert((m.rule, m.bbox, m.nets.iter().map(|&k| nets[k].clone()).collect()));
        }
    }
    out
}

fn want(list: &[(&'static str, [i32; 4], &[&str])]) -> BTreeSet<(&'static str, [i32; 4], Vec<String>)> {
    list.iter().map(|(r, b, n)| (*r, *b, n.iter().map(|s| s.to_string()).collect())).collect()
}

const FORBIDDEN: &str = "FORBIDDENSPACING 0.2 0.3 SAMEMASK WIDTH 0.1 PRL 0.15 TWOEDGES 0.25 ;";

/// drt's forbidden-spacing rule (`checkForbiddenSpc_main`): a max rectangle narrower than WIDTH
/// and longer than PRL, with another max rectangle within TWOEDGES on both sides, against a
/// neighbour at a gap in [min, max] running in parallel longer than PRL; the marker is the gap
/// between them, raised once per THIRD max rectangle inside that gap that touches neither. The
/// T's stem is its own max rectangle and is vertical, so it pairs only with vertical shapes.
const REF_FORBIDDEN: &[(&str, [i32; 4], &[&str])] = &[
    (RULE_FORBIDDEN, [75, 1075, 1925, 1325], &["a", "b", "c"]),
    (RULE_FORBIDDEN, [75, 1275, 1925, 1525], &["b", "c", "e"]),
    (RULE_FORBIDDEN, [2475, 1775, 2925, 2025], &["f", "h", "k"]),
];

/// drt's EOL-extension rule (`checkMetalEndOfLine_ext`), one row (`ENDOFLINE 0.06 EXTENSION
/// 0.03`), spacing 0.08: an end shorter than 0.06 with both corners convex is pushed out by the
/// extension and must keep 0.08 from every other polygon's edge (itself extended when it is an
/// end too); the marker spans the far edge's original extent and the end. The L's inner arm end
/// and both ends of p, q are ends; long sides are not.
const REF_EOL_ONE_ROW: &[(&str, [i32; 4], &[&str])] = &[
    (RULE_EOL_EXTENSION, [3025, 1025, 3025, 1075], &["p", "q"]),
    (RULE_EOL_EXTENSION, [3025, 1025, 3075, 1075], &["p", "q"]),
    (RULE_EOL_EXTENSION, [3075, 1025, 3075, 1075], &["p", "q"]),
    (RULE_EOL_EXTENSION, [3075, 875, 3125, 875], &["p", "r"]),
    (RULE_EOL_EXTENSION, [3075, 1025, 3125, 1025], &["p", "r"]),
    (RULE_EOL_EXTENSION, [3075, 1075, 3125, 1075], &["q", "r"]),
    (RULE_EOL_EXTENSION, [3075, 1225, 3125, 1225], &["q", "r"]),
    (RULE_EOL_EXTENSION, [4375, 575, 4425, 625], &["t", "u"]),
    (RULE_EOL_EXTENSION, [4375, 575, 4475, 625], &["t", "u"]),
    (RULE_EOL_EXTENSION, [4225, 575, 4275, 675], &["t", "v"]),
    (RULE_EOL_EXTENSION, [4225, 625, 4275, 675], &["t", "v"]),
];

#[test]
fn forbidden_and_eol_extension_markers_match_the_reference() {
    let got = markers(FORBIDDEN, "EOLEXTENSIONSPACING 0.08 SAMEMASK ENDOFLINE 0.06 EXTENSION 0.03 ;");
    let mut all = want(REF_FORBIDDEN);
    all.extend(want(REF_EOL_ONE_ROW));
    assert_eq!(got, all);
}

/// PARALLELONLY and a two-row table (`ENDOFLINE 0.04 EXTENSION 0.02 ENDOFLINE 0.06 EXTENSION
/// 0.03`). The table is drt's `fr1DLookupTbl` with `lowerBound = false`: a 0.05 end lies in
/// [0.04, 0.06) and takes the SECOND row's 0.03. PARALLELONLY keeps a non-end edge only when it
/// faces the end (opposite direction), and an extended end only by its side facing the end.
#[test]
fn parallel_only_and_a_two_row_table_match_the_reference() {
    let got = markers(
        FORBIDDEN,
        "EOLEXTENSIONSPACING 0.08 SAMEMASK PARALLELONLY ENDOFLINE 0.04 EXTENSION 0.02 ENDOFLINE 0.06 EXTENSION 0.03 ;",
    );
    let mut all = want(REF_FORBIDDEN);
    all.extend(want(&[
        (RULE_EOL_EXTENSION, [4375, 575, 4425, 625], &["t", "u"]),
        (RULE_EOL_EXTENSION, [4225, 625, 4275, 675], &["t", "v"]),
    ]));
    assert_eq!(got, all);
}

/// A rule in a form drt refuses (no TWOEDGES) is not checked, and says so.
#[test]
fn a_forbidden_rule_the_reference_refuses_is_counted_not_checked() {
    let mut r = SameMaskRules::default();
    r.read_property("LEF58_FORBIDDENSPACING", "FORBIDDENSPACING 0.2 0.3 SAMEMASK WIDTH 0.1 PRL 0.15 ;");
    let d = SameMaskRulesDbu::from_um(&r, 1000.0);
    assert!(d.forbidden.is_empty());
    assert_eq!(d.not_checked.len(), 1);
    assert_eq!(d.not_checked[0].1, 1);
}
