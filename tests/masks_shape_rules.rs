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
use vyges_opendb::masks_check::{
    capture_trace, check_metal_end_of_line_ext, check_shape_rules, SameMaskRulesDbu, RULE_EOL_EXTENSION, RULE_FORBIDDEN,
};
use vyges_opendb::masks_geom::{LayerModel, Shape};
use vyges_opendb::Db;

fn fixture(f: &str) -> String {
    format!("{}/tests/fixtures/masks/{f}", env!("CARGO_MANIFEST_DIR"))
}

/// Every marker of the design as `(rule, bbox, nets)`, M1 then M2.
fn markers(forbidden: &str, eol: &str) -> BTreeSet<(&'static str, [i32; 4], Vec<String>)> {
    markers_of("shape_rules.def", forbidden, eol)
}

type Found = BTreeSet<(&'static str, [i32; 4], Vec<String>)>;

fn markers_of(def: &str, forbidden: &str, eol: &str) -> Found {
    let mut db = Db::new();
    db.read_lef(fixture("shape_rules.lef")).unwrap();
    db.read_def(fixture(def), "default").unwrap();
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

/// `MINLENGTH l [TWOSIDES]` on an EOL-extension end, as the reference qualifies an end of its
/// EOL spacing rule (`checkMetalEndOfLine_eol_hasMinMaxLength`): the end counts only when its
/// previous OR next side edge is at least `l` long, BOTH with TWOSIDES (closed comparisons). An end
/// that fails raises no marker of its own.
///
/// `eol_minlength.def`: on M2, nets w, x, y each end in a 50-long end whose side edges are
/// (100, 50), (80, 30) and (70, 20) long, with a neighbour 75 above and below (so each end raises
/// markers when it qualifies); every other end has long sides. Rule: spacing 0.08, ENDOFLINE 0.06,
/// EXTENSION 0.03. Each end is checked on its own, so a neighbour's end raising the same marker
/// cannot hide the outcome.
#[test]
fn min_length_qualifies_an_end_by_its_side_edges() {
    let mut db = Db::new();
    db.read_lef(fixture("shape_rules.lef")).unwrap();
    db.read_def(fixture("eol_minlength.def"), "default").unwrap();
    let mut nets: Vec<String> = db.block_get_nets();
    nets.sort();
    let number = db.layer_get_number("M2") as i64;
    let mut shapes = Vec::new();
    for (n, net) in nets.iter().enumerate() {
        for b in db.net_wire_boxes(net).iter().filter(|b| b.layer == number) {
            shapes.push(Shape { net: n, rect: [b.x0, b.y0, b.x1, b.y1], fixed: false });
        }
    }
    let model = LayerModel::build(&shapes);
    // the ends of w, x, y: the vertical edge at the end of each short horizontal arm
    let end = |x: i32| {
        (0..model.edges.len())
            .find(|&k| {
                let e = &model.edges[k];
                e.low.0 == x && e.high.0 == x && e.low.1.min(e.high.1) == 1075 && e.low.1.max(e.high.1) == 1125
            })
            .expect("the fixture has this end")
    };
    let ends = [("w", end(1125)), ("x", end(1605)), ("y", end(2095))];
    let base = "EOLEXTENSIONSPACING 0.08 SAMEMASK ENDOFLINE 0.06 EXTENSION 0.03";
    for (clause, qualifying) in [
        ("", vec!["w", "x", "y"]),
        ("MINLENGTH 0.05 TWOSIDES", vec!["w"]), // w's 50 side meets 0.05: closed
        ("MINLENGTH 0.1 TWOSIDES", vec![]),
        ("MINLENGTH 0.08", vec!["w", "x"]), // one side is enough: w 100, x 80
        ("MINLENGTH 0.1", vec!["w"]),
        ("MINLENGTH 0.02 TWOSIDES", vec!["w", "x", "y"]), // y's 20 side meets 0.02
    ] {
        let mut r = SameMaskRules::default();
        r.read_property("LEF58_EOLEXTENSIONSPACING", &format!("{base} {clause} ;"));
        let rules = SameMaskRulesDbu::from_um(&r, 1000.0);
        assert_eq!(rules.approximated(), 0, "{clause}: modelled in full");
        for &(net, e) in &ends {
            let mut out = BTreeSet::new();
            check_metal_end_of_line_ext(&model, e, &rules.eol_extension[0], 0, &nets, &mut out);
            assert_eq!(!out.is_empty(), qualifying.contains(&net), "{clause}: {net}'s end, markers {out:?}");
        }
    }
}

/// A TWOSIDES with no MINLENGTH qualifies nothing; it is checked without and reported.
#[test]
fn a_lone_two_sides_is_reported_approximated() {
    let mut r = SameMaskRules::default();
    r.read_property("LEF58_EOLEXTENSIONSPACING", "EOLEXTENSIONSPACING 0.08 SAMEMASK ENDOFLINE 0.06 EXTENSION 0.03 TWOSIDES ;");
    let d = SameMaskRulesDbu::from_um(&r, 1000.0);
    assert_eq!(d.approximated(), 1);
    assert_eq!(d.eol_extension[0].min_length, vec![None]);
}

/// Our trace for one run, sorted: the `VYT` lines the reference prints at the same points.
fn trace_of(def: &str, forbidden: &str, eol: &str) -> Vec<String> {
    let mut t = capture_trace(|| markers_of(def, forbidden, eol)).1;
    t.sort();
    t
}

/// A reference trace: the instrumented reference checker's `VYT` lines for the same design and
/// rules (without SAMEMASK), sorted. `traces/README` says how each was made.
fn ref_trace(name: &str) -> Vec<String> {
    std::fs::read_to_string(fixture(&format!("traces/{name}.trace"))).unwrap().lines().map(str::to_string).collect()
}

fn only(prefix: &str, t: &[String]) -> Vec<String> {
    t.iter().filter(|l| l.starts_with(prefix)).cloned().collect()
}

/// Every step of both checks, call for call: each max rectangle and its skips, each candidate,
/// each pair with its run lengths and distances, each third shape and why it was skipped; each
/// edge and whether it is an end, each extended end with its query region, each edge pair with
/// its clipped rectangle and squared distance. A changed branch shows here even when the marker
/// set happens not to move.
#[test]
fn the_trace_is_the_references_line_for_line() {
    let one_row = "EOLEXTENSIONSPACING 0.08 SAMEMASK ENDOFLINE 0.06 EXTENSION 0.03 ;";
    assert_eq!(trace_of("shape_rules.def", FORBIDDEN, one_row), ref_trace("ref-v1"));
    let two_rows = "EOLEXTENSIONSPACING 0.08 SAMEMASK PARALLELONLY ENDOFLINE 0.04 EXTENSION 0.02 ENDOFLINE 0.06 EXTENSION 0.03 ;";
    assert_eq!(trace_of("shape_rules.def", FORBIDDEN, two_rows), ref_trace("ref-v2"));
    // shape_rules2.def: one shape per branch the first design does not reach — a short edge
    // between a concave and a convex corner (not an end), an end facing its own net (same-pin
    // skip), two facing horizontal ends (PARALLELONLY keeps the side facing an end running
    // south), and an extended end lying exactly on the query region's face (strict intersection).
    // (The design is one reference worker tile: a shape crossing a tile edge would be visited
    // once per tile there, with each tile's query seeing only its own shapes.)
    assert_eq!(trace_of("shape_rules2.def", FORBIDDEN, one_row), ref_trace("ref-v1-cases"));
    assert_eq!(trace_of("shape_rules2.def", FORBIDDEN, two_rows), ref_trace("ref-v2-cases"));
}

/// The MINLENGTH test, call for call against the reference's on its EOL spacing rule (the same
/// edges reach it: same end width, same convexity test). One-sided runs match exactly. The
/// reference's EOL-spacing reader hands the test a different flag for TWOSIDES, so its runs written
/// `MINLENGTH 0.05 TWOSIDES` and `MINLENGTH 0.1 TWOSIDES` are evaluated one-sided: they equal our
/// one-sided runs. Its TWOSIDES branch
/// is reached only through a PARALLELEDGE … TWOEDGES rule, whose parallel-edge filter lets three
/// ends through; those three lines appear in our TWOSIDES runs unchanged.
#[test]
fn the_min_length_trace_is_the_references() {
    let base = "EOLEXTENSIONSPACING 0.08 SAMEMASK ENDOFLINE 0.06 EXTENSION 0.03";
    let ml = |clause: &str| only("VYT ML_EDGE", &trace_of("eol_minlength.def", FORBIDDEN, &format!("{base} {clause} ;")));
    assert_eq!(ml("MINLENGTH 0.05"), ref_trace("ref-ml1"), "the reference evaluates its TWOSIDES run one-sided");
    assert_eq!(ml("MINLENGTH 0.1"), ref_trace("ref-ml2"), "the reference evaluates its TWOSIDES run one-sided");
    assert_eq!(ml("MINLENGTH 0.1"), ref_trace("ref-ml3"));
    for (clause, r) in [("MINLENGTH 0.05 TWOSIDES", "ref-ml4"), ("MINLENGTH 0.1 TWOSIDES", "ref-ml5"), ("MINLENGTH 0.08 TWOSIDES", "ref-ml6")] {
        let ours = ml(clause);
        let want = ref_trace(r);
        assert_eq!(want.len(), 3);
        for line in &want {
            assert!(ours.contains(line), "{clause}: {line}");
        }
    }
}

/// Trace dump for correlating the MINLENGTH test with the reference's (run with
/// `VYGES_MASKS_TRACE=1 -- --ignored --nocapture`): one block per configuration.
#[test]
#[ignore]
fn min_length_trace_dump() {
    let base = "EOLEXTENSIONSPACING 0.08 SAMEMASK ENDOFLINE 0.06 EXTENSION 0.03";
    for (k, clause) in ["MINLENGTH 0.05", "MINLENGTH 0.1", "MINLENGTH 0.08", "MINLENGTH 0.05 TWOSIDES", "MINLENGTH 0.1 TWOSIDES", "MINLENGTH 0.08 TWOSIDES"]
        .iter()
        .enumerate()
    {
        eprintln!("VYCFG {k} {clause}");
        let _ = markers_of("eol_minlength.def", FORBIDDEN, &format!("{base} {clause} ;"));
    }
}
