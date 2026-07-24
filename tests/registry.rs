// SPDX-License-Identifier: Apache-2.0
// The runtime registry (generated get/set dispatch) — the surface `vyges mcp` drives by name.
use vyges_opendb::{registry, Db};

const FIXTURE: &str = "tests/fixtures/counter.odb";

#[test]
fn registry_is_populated() {
    // the read surface is large and every entry names a real class + addressing keys
    assert!(registry::FIELDS.len() > 250, "expected a broad read surface");
    for f in registry::FIELDS {
        assert!(f.class.starts_with("db"));
        assert!(!f.field.is_empty());
    }
}

#[test]
fn registry_covers_the_new_target_classes() {
    use std::collections::HashSet;
    let classes: HashSet<&str> = registry::FIELDS.iter().map(|f| f.class).collect();
    // hierarchy / grouping / region + index-addressed blockage / track-grid + DRC markers +
    // the deep module hierarchy (mod-inst/net/bterm/iterm)
    for c in ["dbModule", "dbGroup", "dbRegion", "dbBlockage", "dbTrackGrid",
              "dbMarkerCategory", "dbMarker",
              "dbModInst", "dbModNet", "dbModBTerm", "dbModITerm",
              "dbPowerDomain", "dbPowerSwitch", "dbIsolation", "dbLevelShifter",
              // tech / lib / parasitics / pins / tech-rules (core classes)
              "dbTech", "dbLib", "dbCapNode", "dbRSeg", "dbCCSeg", "dbSBox", "dbBPin", "dbMPin",
              "dbTechViaRule", "dbTechViaGenerateRule", "dbTechViaLayerRule",
              "dbTechLayerAntennaRule"] {
        // (dbTechAntennaPinModel is setter-only -> WRITE_FIELDS, not the read FIELDS below)
        assert!(classes.contains(c), "{c} should be exposed in the registry");
    }
}

#[test]
fn registry_tech_and_lib_read_populated() {
    // dbTech / dbLib are populated in the fixture — functional reads (not just discovery)
    let db = Db::open(FIXTURE).unwrap();
    // sky130 uses 1000 DBU/micron
    let dbu = registry::get(&db, "dbTech", "get_db_units_per_micron", &[]).unwrap();
    assert_eq!(dbu, serde_json::json!(1000));
    assert!(!db.tech_get_name().is_empty() || db.tech_get_db_units_per_micron() == 1000);
}

#[test]
fn registry_power_intent_reads_are_graceful_when_absent() {
    // the fixture has no UPF power intent; reads over a missing power domain must return typed
    // defaults, not panic
    let db = Db::open(FIXTURE).unwrap();
    let v = registry::get(&db, "dbPowerDomain", "get_voltage", &["no_pd".into()]).unwrap();
    assert_eq!(v, serde_json::json!(0.0)); // f32 voltage -> default 0.0
    let n = registry::get(&db, "dbLevelShifter", "get_name", &["no_ls".into()]).unwrap();
    assert_eq!(n, serde_json::json!(""));
}

#[test]
fn registry_marker_reads_are_graceful_when_absent() {
    // the clean fixture has no DRC markers; reads over a missing category must return typed
    // defaults (0 / ""), never panic — instrumentation must survive an empty design.
    let db = Db::open(FIXTURE).unwrap();
    let n = registry::get(&db, "dbMarkerCategory", "get_marker_count", &["nope".into()]).unwrap();
    assert_eq!(n, serde_json::json!(0));
    let name = registry::get(&db, "dbMarker", "get_name", &["nope".into(), "0".into()]).unwrap();
    assert_eq!(name, serde_json::json!(""));
}

#[test]
fn registry_miss_is_assertive_and_catalogued() {
    let db = Db::open(FIXTURE).unwrap();
    // the "unimplemented but real odb API" catalog is the food-chain "what to fix" map
    assert!(registry::UNIMPLEMENTED.len() > 100, "expected a populated unimplemented catalog");

    // calling a real-but-unbound odb method -> assertive "not implemented" (also emitted as an
    // ODB-0900 vyges-events warning naming the exact class::method)
    let u = &registry::UNIMPLEMENTED[0];
    let e = registry::get(&db, u.class, u.field, &[]).unwrap_err();
    assert!(e.to_string().contains("not implemented"), "got: {e}");

    // a non-odb class is distinguished from a real class (ODB-0902 vs ODB-0901)
    let e2 = registry::get(&db, "dbBogusClass", "x", &[]).unwrap_err();
    assert!(e2.to_string().contains("not an odb class"), "got: {e2}");
    // a real class + bogus field
    let e3 = registry::get(&db, "dbNet", "totally_bogus_field", &[]).unwrap_err();
    assert!(e3.to_string().contains("unknown field"), "got: {e3}");
}

#[test]
fn registry_get_dispatches_all_value_kinds() {
    let db = Db::open(FIXTURE).unwrap();

    // string (no keys) — agrees with the hand-written accessor
    let v = registry::get(&db, "dbBlock", "get_name", &[]).unwrap();
    assert_eq!(v, serde_json::json!(db.block_name()));

    // enum-string with a str key
    let net = db.net_names().into_iter().next().unwrap();
    let v = registry::get(&db, "dbNet", "get_sig_type", &[net.clone()]).unwrap();
    assert_eq!(v, serde_json::json!(db.net_get_sig_type(&net)));

    // list — length matches the instance count
    let insts = registry::get(&db, "dbBlock", "get_insts", &[]).unwrap();
    assert_eq!(insts.as_array().unwrap().len(), db.num_insts());

    // errors are typed, not panics
    assert!(registry::get(&db, "dbInst", "no_such_field", &[]).is_err());
    assert!(registry::get(&db, "dbNope", "get_name", &[]).is_err());
    // a str key where an idx is required (dbBox is index-addressed) → typed error
    assert!(registry::get(&db, "dbBox", "x_min", &["not_a_number".into()]).is_err());
}

#[test]
fn registry_get_index_addressed() {
    let mut db = Db::open(FIXTURE).unwrap();
    db.add_obstruction("met1", 1000, 2000, 5000, 8000).unwrap();
    // an index-addressed dbBox read finds the obstruction bbox we just added
    let n = db.num_obstructions();
    let found = (0..n).any(|i| {
        registry::get(&db, "dbBox", "x_min", &[i.to_string()]).unwrap() == serde_json::json!(1000)
    });
    assert!(found, "index-addressed dbBox.x_min should surface the added obstruction");
}

#[cfg(feature = "gen-write")]
#[test]
fn registry_set_dispatches() {
    let mut db = Db::open(FIXTURE).unwrap();
    assert!(registry::WRITE_FIELDS.len() > 100, "expected a broad write surface");

    let net = db.net_names().into_iter().next().unwrap();
    // scalar set via registry (value string-encoded), read back via registry
    registry::set(&mut db, "dbNet", "set_weight", &[net.clone()], &["7".into()]).unwrap();
    assert_eq!(registry::get(&db, "dbNet", "get_weight", &[net.clone()]).unwrap(), serde_json::json!(7));

    // enum set via registry (constructed from the string)
    let inst = db.nth_inst_name(0);
    registry::set(&mut db, "dbInst", "set_orient", &[inst.clone()], &["MY".into()]).unwrap();
    assert_eq!(registry::get(&db, "dbInst", "get_orient", &[inst]).unwrap(), serde_json::json!("MY"));

    // bad value + missing object are typed errors
    assert!(registry::set(&mut db, "dbNet", "set_weight", &[net], &["NaN".into()]).is_err());
    assert!(registry::set(&mut db, "dbNet", "set_weight", &["no_net".into()], &["1".into()]).is_err());
}
