// SPDX-License-Identifier: Apache-2.0
// The generated setter surface (L2/write) — only compiled/run under `--features gen-write`.
// Proves the three setter param paths (scalar, enum, and error-on-missing) round-trip against
// the generated read accessors.
#![cfg(feature = "gen-write")]

use vyges_opendb::Db;

const FIXTURE: &str = "tests/fixtures/counter.odb";

#[test]
fn generated_scalar_setter_round_trips() {
    let mut db = Db::open(FIXTURE).unwrap();
    let net = db.net_names().into_iter().next().unwrap();
    db.net_set_weight(&net, 42).unwrap();
    assert_eq!(db.net_get_weight(&net), 42);

    // the edit survives serialization
    let out = std::env::temp_dir().join("vyges_opendb_gen_write.odb");
    db.write(&out).unwrap();
    assert_eq!(Db::open(&out).unwrap().net_get_weight(&net), 42);
}

#[test]
fn generated_enum_setter_round_trips() {
    let mut db = Db::open(FIXTURE).unwrap();
    let inst = db.nth_inst_name(0);
    // dbOrientType parses "MX"; the generated enum-param setter constructs it from the string
    db.inst_set_orient(&inst, "MX").unwrap();
    assert_eq!(db.inst_get_orient(&inst), "MX");
}

#[test]
fn generated_multi_value_setter_round_trips_via_geometry() {
    let mut db = Db::open(FIXTURE).unwrap();
    let inst = db.nth_inst_name(0);
    // a 2-value setter (setOrigin(int x, int y)); read back through the expanded Point sub-fields
    db.inst_set_origin(&inst, 12_000, 34_000).unwrap();
    assert_eq!(db.inst_get_origin_x(&inst), 12_000);
    assert_eq!(db.inst_get_origin_y(&inst), 34_000);
}

#[test]
fn generated_setter_errs_on_missing_object() {
    let mut db = Db::open(FIXTURE).unwrap();
    // addressing a non-existent net must surface a typed error, not a panic or silent no-op
    assert!(db.net_set_weight("no_such_net", 1).is_err());
}

const CHIPLET: &str = "tests/fixtures/chiplet3d.odb";

#[test]
fn point3d_expands_into_scalar_setter_args() {
    // STRUCT_IN mirrors STRUCT_FIELDS on the write side: a geometry struct param becomes one
    // scalar arg per constructor component, reassembled at the call. `setLoc(const Point3D&)`
    // is the reason it exists — without it the location setter was unmarshallable, which is
    // what forced dbChipInst to expose no setters at all.
    let f = vyges_opendb::registry::WRITE_FIELDS
        .iter()
        .find(|f| f.class == "dbChipInst" && f.field == "set_loc")
        .expect("dbChipInst::set_loc missing from the write registry");
    assert_eq!(f.values, ["i32", "i32", "i32"], "Point3D should expand to x/y/z");

    // the 2D Point expands the same way
    assert!(vyges_opendb::registry::WRITE_FIELDS
        .iter()
        .any(|f| f.class == "dbChip" && f.field == "set_offset" && f.values.len() == 2));
}

#[test]
fn place_chip_inst_orders_the_coupled_setters_correctly() {
    // dbChipInst::setOrient and setLoc are COUPLED and order-dependent: setLoc does not store
    // the point given — it orients the master chip's cuboid and stores the DELTA that lands its
    // lower-left-lower corner there — and the location read back re-applies the CURRENT
    // orientation. Place-then-rotate therefore silently moves the chip.
    //
    // place_chip_inst exists to get that order right. After it returns, the location must read
    // back exactly as passed.
    let mut db = Db::open(CHIPLET).unwrap();
    db.place_chip_inst("stack", "u_top", "MZ_R90", 4000, 5000, 6000).unwrap();
    assert_eq!(db.chipinst_get_loc_x("stack", "u_top"), 4000);
    assert_eq!(db.chipinst_get_loc_y("stack", "u_top"), 5000);
    assert_eq!(db.chipinst_get_loc_z("stack", "u_top"), 6000);
    assert_eq!(db.chipinst_get_orient("stack", "u_top"), "MZ_R90");
}

#[test]
fn the_wrong_order_really_does_move_the_chip() {
    // Proves the hazard is real rather than theoretical — and therefore that place_chip_inst is
    // earning its keep. Setting the location BEFORE re-orienting leaves the chip somewhere other
    // than where it was put, silently.
    let mut db = Db::open(CHIPLET).unwrap();
    db.chipinst_set_orient("stack", "u_top", "R0").unwrap();
    db.chipinst_set_loc("stack", "u_top", 4000, 5000, 6000).unwrap();
    assert_eq!(db.chipinst_get_loc_x("stack", "u_top"), 4000); // correct so far

    db.chipinst_set_orient("stack", "u_top", "MZ_R90").unwrap(); // re-orient AFTER placing
    assert_ne!(
        db.chipinst_get_loc_x("stack", "u_top"),
        4000,
        "re-orienting a placed chip inst should move it — if this ever passes, odb changed \
         and place_chip_inst's ordering guarantee needs revisiting"
    );
}
