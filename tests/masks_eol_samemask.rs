// SPDX-License-Identifier: Apache-2.0
//! `SPACING s ENDOFLINE w WITHIN wi SAMEMASK ENDTOEND e EXTENSION x MINLENGTH l TWOSIDES`, per
//! mask, correlated with the reference detailed router's own end-of-line check.
//!
//! 🔑 Oracle: odb keeps this form (it is the reference's `frLef58SpacingEndOfLineConstraint` with
//! the same-mask flag) and drt checks it MASK-BLIND, so on a design whose shapes are all on one
//! mask the reference's check and ours must agree exactly. drt `check_drc` (OpenROAD at the pin,
//! one thread), patched with `fprintf` trace lines (`VYT EL_*`, `VYT ML_EDGE`), was run on
//! `tests/fixtures/masks/eol_samemask.{lef,def}`; `traces/ref-eol1.trace` is its sorted trace.
//! Ours runs with the reference's two readings ([`EolWithin::Reference`],
//! [`EolTwoSides::Reference`]) and must print the same lines.
//!
//! The design (M2 vertical, wires 0.05 wide, every value invented): a/b end to end 35 apart;
//! c/d end to end with no common run until the 0.01 EXTENSION grows both; e's end beside the
//! side of f, 10 past its corner (closes only when WITHIN is read as written); g's end with a
//! 15 long side (one-sided MINLENGTH qualifies it, TWOSIDES as written does not) under the side
//! of h; i/j 80 apart (exactly ENDTOEND: clean), k/l 79 apart; m and n alone.
#![cfg(unix)]

use std::collections::BTreeSet;
use vyges_opendb::masks_check::{
    capture_trace, check_metal_end_of_line_eol, eol_spacing_rules, EolTwoSides, EolWithin, RULE_EOL_SPACING,
};
use vyges_opendb::masks_geom::{LayerModel, Shape};
use vyges_opendb::Db;

fn fixture(f: &str) -> String {
    format!("{}/tests/fixtures/masks/{f}", env!("CARGO_MANIFEST_DIR"))
}

type Found = BTreeSet<([i32; 4], Vec<String>)>;

/// Our markers and trace on the fixture, every shape on one mask, under the given readings.
fn run(within: EolWithin, two_sides: EolTwoSides) -> (Found, Vec<String>) {
    let mut db = Db::new();
    db.read_lef(fixture("eol_samemask.lef")).unwrap();
    db.read_def(fixture("eol_samemask.def"), "default").unwrap();
    let mut nets: Vec<String> = db.block_get_nets();
    nets.sort();
    let (rules, nc) = eol_spacing_rules(&db, "M2", within, two_sides);
    assert_eq!(rules.len(), 1, "the one SAMEMASK end-of-line rule");
    assert!(nc.is_empty(), "{nc:?}");
    let number = db.layer_get_number("M2") as i64;
    let mut shapes = Vec::new();
    for (n, net) in nets.iter().enumerate() {
        for b in db.net_wire_boxes(net).iter().filter(|b| b.layer == number) {
            shapes.push(Shape { net: n, rect: [b.x0, b.y0, b.x1, b.y1], fixed: false });
        }
    }
    let model = LayerModel::build(&shapes);
    let (out, lines) = capture_trace(|| {
        let mut out = BTreeSet::new();
        for e in 0..model.edges.len() {
            check_metal_end_of_line_eol(&model, e, &rules[0], 0, &nets, &mut out);
        }
        out
    });
    let found = out
        .into_iter()
        .map(|m| {
            assert_eq!(m.rule, RULE_EOL_SPACING);
            (m.bbox, m.nets.iter().map(|&n| nets[n].clone()).collect())
        })
        .collect();
    let mut lines: Vec<String> = lines.into_iter().filter(|l| l.starts_with("VYT EL_") || l.starts_with("VYT ML_EDGE")).collect();
    lines.sort();
    (found, lines)
}

fn reference_trace(name: &str) -> Vec<String> {
    let t = std::fs::read_to_string(fixture(&format!("traces/{name}"))).unwrap();
    t.lines().map(str::to_string).collect()
}

fn set(v: &[([i32; 4], &[&str])]) -> Found {
    v.iter().map(|(b, n)| (*b, n.iter().map(|s| s.to_string()).collect())).collect()
}

/// Line for line, the reference's trace; and its markers.
#[test]
fn the_reference_reading_matches_the_reference_trace() {
    let (found, lines) = run(EolWithin::Reference, EolTwoSides::Reference);
    let want = reference_trace("ref-eol1.trace");
    assert_eq!(lines.len(), want.len(), "line count");
    for (k, (a, b)) in lines.iter().zip(&want).enumerate() {
        assert_eq!(a, b, "line {k}");
    }
    assert_eq!(found, set(REF_MARKERS));
}

/// The two readings that differ from the reference: WITHIN as written closes e against f;
/// TWOSIDES as written stops g's end from qualifying.
#[test]
fn the_written_readings_move_exactly_their_cases() {
    let (reference, _) = run(EolWithin::Reference, EolTwoSides::Reference);
    let (within, _) = run(EolWithin::AsWritten, EolTwoSides::Reference);
    let (two, _) = run(EolWithin::Reference, EolTwoSides::AsWritten);
    let added: Found = within.difference(&reference).cloned().collect();
    let dropped: Found = reference.difference(&two).cloned().collect();
    assert!(reference.is_subset(&within), "{within:?}");
    assert!(two.is_subset(&reference), "{two:?}");
    assert_eq!(added, set(WITHIN_ADDS));
    assert_eq!(dropped, set(TWO_SIDES_DROPS));
}

// The reference's report on the fixture, in DBU (drt check_drc at the pin: the four
// Lef58SpacingEndOfLine markers; e/f is clean because the reference reads WITHIN as 0 here).
const REF_MARKERS: &[([i32; 4], &[&str])] = &[
    ([1000, 2025, 1050, 2060], &["a", "b"]),
    ([2050, 2025, 2050, 2075], &["c", "d"]),
    ([4000, 2025, 4050, 2050], &["g", "h"]),
    ([6000, 2025, 6050, 2104], &["k", "l"]),
];
const WITHIN_ADDS: &[([i32; 4], &[&str])] = &[([3050, 2025, 3060, 2035], &["e", "f"])];
const TWO_SIDES_DROPS: &[([i32; 4], &[&str])] = &[([4000, 2025, 4050, 2050], &["g", "h"])];

/// The colour step reads the rule from the database for every mask of M2 (it is a same-mask rule,
/// so it is checked on each mask's shapes alone).
#[test]
fn the_colour_step_reads_the_rule() {
    use vyges_opendb::masks::{self, ColourOptions};
    let mut db = Db::new();
    db.read_lef(fixture("eol_samemask.lef")).unwrap();
    db.read_def(fixture("eol_samemask.def"), "default").unwrap();
    let layers = masks::mask_layers(&db, &ColourOptions::default()).unwrap();
    let m2 = layers.iter().find(|l| l.name == "M2").expect("M2 is a mask layer");
    let r = m2.rules.as_ref().expect("M2 has a same-mask rule");
    assert_eq!(r.eol_spacing.len(), 1);
    assert_eq!((r.eol_spacing[0].eol_space, r.eol_spacing[0].eol_width, r.eol_spacing[0].within), (70, 60, 20));
    assert!(r.has_shape_rules() && r.not_checked.is_empty(), "{:?}", r.not_checked);
}

/// Only SAMEMASK statements are read (a rule without SAMEMASK is the reference's own, checked
/// mask-blind there); a SAMEMASK statement with a clause not modelled here is counted by that
/// clause and not checked.
#[test]
fn only_modelled_same_mask_rules_are_checked() {
    let mut db = Db::new();
    db.read_lef(fixture("eol_samemask_nc.lef")).unwrap();
    let (rules, nc) = eol_spacing_rules(&db, "M2", EolWithin::Reference, EolTwoSides::Reference);
    assert!(rules.is_empty(), "{rules:?}");
    assert_eq!(nc, vec![("SPACING ENDOFLINE SAMEMASK PARALLELEDGE", 1)]);
}
