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

#[test]
fn chip_inst_exposes_no_setters_while_set_loc_is_unmarshallable() {
    // Deliberate withholding, not an oversight. dbChipInst::setOrient and setLoc are COUPLED:
    // setLoc does not store the point it is given — it orients the master chip's cuboid and
    // stores the delta that lands its lower-left-lower corner on that point, and getLoc() is
    // getCuboid().lll(), which re-applies the CURRENT orientation. Re-orienting an already-placed
    // chip inst therefore silently MOVES it. setLoc takes a Point3D, which is not a marshallable
    // setter param, so a caller who tripped that could not put the chip back. Exposing only the
    // destructive half of the pair is worse than exposing neither, so the generator withholds it
    // (see `skip_setters` in TARGETS).
    //
    // This test pins the decision: a regeneration must not silently hand the footgun back.
    assert!(
        !vyges_opendb::registry::WRITE_FIELDS.iter().any(|f| f.class == "dbChipInst"),
        "dbChipInst must expose no setters while setLoc is unmarshallable"
    );
    // dbChip's setters are unaffected — independent scalars, not a coupled pair.
    assert!(vyges_opendb::registry::WRITE_FIELDS
        .iter()
        .any(|f| f.class == "dbChip" && f.field == "set_thickness"));
}
