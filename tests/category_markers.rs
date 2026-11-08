// SPDX-License-Identifier: Apache-2.0
//! `Db::category_markers`: a marker category read back with everything under it, as a detailed
//! router reads its "DRC" markers from the database.
//!
//! 🔑 Fixture `tiny_drc_markers.odb` = the `tiny` design with four markers imported by the
//! reference's own `odb::dbMarkerCategory_fromTR` into `DRC` (subcategories `Short` and
//! `Cut Short`, two each). Expected values are the reference's answers on it (its `writeTR` of each
//! subcategory and the ids it reports): marker ids are per category and the walk is id-ordered, so
//! the subcategories interleave; and the third marker, imported with `net:n1 inst:u1 net:in`, holds
//! only `inst:u1 net:n1` (instance `u1` and net `in` share id 1 under one block).
use vyges_opendb::{CategoryMarker, Db};

const FIXTURE: &str = "tests/fixtures/tiny_drc_markers.odb";

fn m(layer: &str, rect: [i32; 4], nets: &[&str]) -> CategoryMarker {
    CategoryMarker { layer: Some(layer.into()), rect, source_nets: nets.iter().map(|s| s.to_string()).collect() }
}

#[test]
fn a_category_with_its_subcategories_in_the_database_order() {
    let db = Db::open(FIXTURE).expect("fixture");
    assert_eq!(
        db.category_markers("DRC").expect("markers"),
        vec![
            m("M1", [1000, 200, 1500, 400], &["in", "n1"]),
            m("V1", [1100, 300, 1170, 370], &["in", "n1"]),
            m("M2", [2000, 1000, 2300, 1140], &["n1"]),
            m("V1", [3000, 1500, 3070, 1570], &["n1"]),
        ]
    );
}

#[test]
fn an_absent_category_has_no_markers() {
    let db = Db::open(FIXTURE).expect("fixture");
    assert!(db.category_markers("NO_SUCH").expect("markers").is_empty());
    // A subcategory is not a top-level name.
    assert!(db.category_markers("Short").expect("markers").is_empty());
}
