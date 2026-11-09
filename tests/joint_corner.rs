// SPDX-License-Identifier: Apache-2.0
//! `JOINTCORNERSPACING … SAMEMASK` through the colour step: the rule is read from the layer's raw
//! property text in the database (odb builds no rule from it), and two facing L corners on one
//! mask are reported, on two masks not. No reference checks this rule; the expected results are
//! the LEF/DEF reference figure's (see `vyges_opendb::joint_corner`).
#![cfg(unix)]

use vyges_opendb::masks::{self, ColourOptions, Finding};
use vyges_opendb::Db;

fn fixture(f: &str) -> String {
    format!("{}/tests/fixtures/masks/{f}", env!("CARGO_MANIFEST_DIR"))
}

fn load() -> Db {
    let mut db = Db::new();
    db.read_lef(fixture("joint_corner.lef")).unwrap();
    db.read_def(fixture("masks.def"), "default").unwrap();
    db
}

/// The SAMEMASK statement on M1 is read and checked; the one without SAMEMASK on M2 is a
/// mask-blind rule, counted as not checked.
#[test]
fn rules_come_from_the_layer_text() {
    let db = load();
    let layers = masks::mask_layers(&db, &ColourOptions::default()).unwrap();
    let m1 = layers[0].rules.as_ref().expect("M1 has a joint corner rule");
    assert_eq!(layers[0].name, "M1");
    assert_eq!(m1.joint_corner.len(), 1);
    assert_eq!((m1.joint_corner[0].spacing, m1.joint_corner[0].span_length, m1.joint_corner[0].edge_length), (50, 60, Some(100)));
    assert!(m1.has_shape_rules());
    let m2 = layers[1].rules.as_ref().expect("M2 reports its rule");
    assert!(m2.joint_corner.is_empty() && !m2.has_shape_rules());
    assert_eq!(m2.not_checked, vec![(vyges_opendb::joint_corner::NOT_CHECKED_NO_SAMEMASK, 1)]);
    // the option overrides every rule, this one too
    let opt = ColourOptions { same_mask_spacing: [("M1".to_string(), 70)].into(), ..ColourOptions::default() };
    assert!(masks::mask_layers(&db, &opt).unwrap()[0].rules.is_none());
}

/// Net b redrawn as the figure's two L's (rectangles, wire width 0.05 under JOINTWIDTH 0.07),
/// their outer corners (300, 1025) and (324, 1057) 0.04 apart, under 0.05.
fn draw(db: &mut Db, lower_mask: i32, upper_mask: i32) {
    let names = vec!["M1".to_string()];
    let mut ops = Vec::new();
    let mut rect = |mask: i32, r: [i32; 4]| {
        let (cx, cy) = (r[0], r[1]);
        ops.extend([0, 0, 0, 6, mask, 1, cx, cy, 5, 0, 0, r[2] - cx, r[3] - cy]);
    };
    rect(lower_mask, [100, 975, 300, 1025]);
    rect(lower_mask, [250, 800, 300, 1025]);
    rect(upper_mask, [324, 1057, 524, 1107]);
    rect(upper_mask, [324, 1057, 374, 1282]);
    db.net_write_wire("b", &ops, &names).unwrap();
}

fn joint_corner_findings(rep: &masks::ColourReport) -> Vec<(u8, [i32; 4])> {
    rep.findings
        .iter()
        .filter_map(|f| match f {
            Finding::SameMaskRule { mask, marker, .. } if f.kind() == "joint_corner_spacing" => Some((*mask, *marker)),
            _ => None,
        })
        .collect()
}

#[test]
fn facing_corners_on_one_mask_are_reported_on_two_masks_not() {
    let opts = ColourOptions { check_only: true, ..ColourOptions::default() };
    let mut db = load();
    draw(&mut db, 1, 1);
    let rep = masks::colour_masks(&mut db, &opts).unwrap();
    assert_eq!(joint_corner_findings(&rep), vec![(1, [300, 1025, 324, 1057])], "{:?}", rep.findings);
    let f = rep.findings.iter().find(|f| f.kind() == "joint_corner_spacing").unwrap();
    assert_eq!(f.to_json()["rule"], "JOINTCORNERSPACING SAMEMASK");

    let mut db = load();
    draw(&mut db, 1, 2);
    let rep = masks::colour_masks(&mut db, &opts).unwrap();
    assert!(joint_corner_findings(&rep).is_empty(), "{:?}", rep.findings);
}
