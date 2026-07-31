// SPDX-License-Identifier: Apache-2.0
// The ODB 3D / chiplet schema (dbChip, dbChipInst) read against POPULATED data with exact
// values — not discovery or graceful-empty. The fixture is synthesized by opendb-lib's
// test/make-3d-fixture.cpp (-DVYGES_ODB_MK3DFIXTURE=ON) because our safe API does not expose
// structural creation and we cannot read a .3dbv/.3dbx yet.
//
// The fixture, and what it is for:
//
//   stack : dbChip HIER, no tech
//     |- u_top  : dbChipInst -> top_die  (DIE)       loc (1000, 2000, 3000)  orient MZ_R90
//     |- u_base : dbChipInst -> base_die (SUBSTRATE) loc (0, 0, 0)           orient R0
use vyges_opendb::{registry, Db};

const FIXTURE: &str = "tests/fixtures/chiplet3d.odb";

fn chip(db: &Db, name: &str, field: &str) -> serde_json::Value {
    registry::get(db, "dbChip", field, &[name.into()]).unwrap()
}

fn inst(db: &Db, name: &str, field: &str) -> serde_json::Value {
    registry::get(db, "dbChipInst", field, &["stack".into(), name.into()]).unwrap()
}

#[test]
fn chips_persist_through_a_write_read_round_trip() {
    // The fixture was written with db->write() and is read back here with a fresh db. If the 3D
    // schema did not survive write_db/read_db, every read below would come back empty — so this
    // independently confirms the persistency claim for dbChip*, not just our marshalling.
    let db = Db::open(FIXTURE).unwrap();
    assert_eq!(chip(&db, "stack", "get_name"), serde_json::json!("stack"));
    assert_eq!(chip(&db, "top_die", "get_name"), serde_json::json!("top_die"));
    assert_eq!(chip(&db, "base_die", "get_name"), serde_json::json!("base_die"));
}

#[test]
fn chip_type_discriminates_die_from_substrate_from_hier() {
    // odb ships no getString() for dbChip::ChipType, so the generator emits the mapping. Three
    // DIFFERENT types are asserted deliberately: a fixture where every chip was a DIE could not
    // tell a working mapping from one that returns a constant.
    let db = Db::open(FIXTURE).unwrap();
    assert_eq!(chip(&db, "stack", "get_chip_type"), serde_json::json!("HIER"));
    assert_eq!(chip(&db, "top_die", "get_chip_type"), serde_json::json!("DIE"));
    assert_eq!(chip(&db, "base_die", "get_chip_type"), serde_json::json!("SUBSTRATE"));
}

#[test]
fn per_chip_stackup_is_readable() {
    // Per-chip geometry is what lets dies of different processes coexist in one database; the
    // thickness in particular is the Z extent the stack is built from.
    let db = Db::open(FIXTURE).unwrap();
    assert_eq!(chip(&db, "top_die", "get_width"), serde_json::json!(50000));
    assert_eq!(chip(&db, "top_die", "get_height"), serde_json::json!(40000));
    assert_eq!(chip(&db, "top_die", "get_thickness"), serde_json::json!(700));
    assert_eq!(chip(&db, "top_die", "is_tsv"), serde_json::json!(true));

    assert_eq!(chip(&db, "base_die", "get_thickness"), serde_json::json!(1500));
    assert_eq!(chip(&db, "base_die", "is_tsv"), serde_json::json!(false));
}

#[test]
fn chip_inst_placement_reads_x_y_and_z() {
    // Point3D expanded into three scalar sub-fields. The Z is the point of the whole exercise:
    // it is what a 2D dbInst location cannot express.
    let db = Db::open(FIXTURE).unwrap();
    assert_eq!(inst(&db, "u_top", "get_loc_x"), serde_json::json!(1000));
    assert_eq!(inst(&db, "u_top", "get_loc_y"), serde_json::json!(2000));
    assert_eq!(inst(&db, "u_top", "get_loc_z"), serde_json::json!(3000));

    assert_eq!(inst(&db, "u_base", "get_loc_z"), serde_json::json!(0));
}

#[test]
fn chip_inst_orientation_includes_the_z_mirror() {
    // dbOrientType3D is a 2D orientation plus a mirror-in-Z flag, and getString() encodes both.
    // u_top is rotated AND flipped — the "MZ_" prefix is exactly the part a 2D dbOrientType
    // cannot represent, so a binding that quietly dropped to the 2D type would fail here.
    let db = Db::open(FIXTURE).unwrap();
    assert_eq!(inst(&db, "u_top", "get_orient"), serde_json::json!("MZ_R90"));
    assert_eq!(inst(&db, "u_base", "get_orient"), serde_json::json!("R0"));
}

#[test]
fn chip_inst_resolves_its_master_and_parent() {
    // The relation accessors marshal a dbChip* to its name, which is what makes the stack
    // walkable: parent chip -> inst -> master chip, and the master is addressable in turn.
    let db = Db::open(FIXTURE).unwrap();
    assert_eq!(inst(&db, "u_top", "get_master_chip"), serde_json::json!("top_die"));
    assert_eq!(inst(&db, "u_top", "get_parent_chip"), serde_json::json!("stack"));
    assert_eq!(inst(&db, "u_base", "get_master_chip"), serde_json::json!("base_die"));

    // and the name reached through the relation is itself a valid key
    let master = inst(&db, "u_base", "get_master_chip");
    let master = master.as_str().unwrap();
    assert_eq!(chip(&db, master, "get_chip_type"), serde_json::json!("SUBSTRATE"));
}

#[test]
fn a_chip_inst_is_keyed_by_its_parent_not_globally() {
    // dbDatabase::getChipInsts() is flat and inst names are unique only within their parent
    // chip, which is why the key is (parent chip, inst). Looking u_top up under the wrong
    // parent must miss rather than silently resolve.
    let db = Db::open(FIXTURE).unwrap();
    let wrong = registry::get(&db, "dbChipInst", "get_loc_z", &["top_die".into(), "u_top".into()])
        .unwrap();
    assert_eq!(wrong, serde_json::json!(0));
    let right = inst(&db, "u_top", "get_loc_z");
    assert_eq!(right, serde_json::json!(3000));
}
