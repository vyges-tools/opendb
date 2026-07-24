// SPDX-License-Identifier: Apache-2.0
// Functional tests against a HIERARCHICAL fixture with a real module hierarchy + a DRC marker,
// so the mod-inst / mod-net / mod-bterm / marker accessors are validated on *populated* data
// (the flat counter.odb only allowed discovery + graceful-empty checks).
//
// hier.odb is synthesized by vyges-tools-opendb-lib/test/make-hier-fixture.cpp (see that file for
// the exact objects + names). Regenerate: build odb_mkfixture (-DVYGES_ODB_MKFIXTURE=ON) and run
//   odb_mkfixture counter.odb hier.odb
use vyges_opendb::{registry, Db};

const HIER: &str = "tests/fixtures/hier.odb";

#[test]
fn hierarchical_mod_inst_and_master() {
    let db = Db::open(HIER).unwrap();
    let top = db.block_name();
    // the top module now has a child mod-inst "u_leaf" (dbModule::getChildren -> dbModInst names)
    let children = db.module_get_children(&top);
    assert!(children.iter().any(|c| c == "u_leaf"), "top children should include u_leaf: {children:?}");

    // the mod-inst resolves by hierarchical name; its master is the "leaf" module we created
    assert_eq!(db.modinst_get_name("u_leaf"), "u_leaf");
    assert_eq!(db.modinst_get_master("u_leaf"), "leaf");
    // it has two mod-iterms (A, Y) mirroring the leaf's ports
    assert_eq!(db.modinst_get_mod_i_terms("u_leaf").len(), 2);
}

#[test]
fn hierarchical_mod_net_and_bterms() {
    let db = Db::open(HIER).unwrap();
    // the module net resolves by hierarchical name
    assert_eq!(db.modnet_get_name("hier_net"), "hier_net");

    // the leaf module's boundary terminals (addressed by module + index): A=INPUT, Y=OUTPUT
    let names: Vec<String> = (0..2).map(|i| db.modbterm_get_name("leaf", i)).collect();
    assert!(names.iter().any(|n| n == "A") && names.iter().any(|n| n == "Y"), "leaf ports: {names:?}");
    for i in 0..2 {
        let io = db.modbterm_get_io_type("leaf", i);
        assert!(io == "INPUT" || io == "OUTPUT", "modbterm io type: {io}");
    }
}

#[test]
fn hierarchical_drc_marker() {
    let db = Db::open(HIER).unwrap();
    // one marker in category "drc_test", with the exact bbox / flags we wrote
    assert_eq!(db.marker_cat_get_name("drc_test"), "drc_test");
    assert_eq!(db.marker_cat_get_marker_count("drc_test"), 1);

    assert_eq!(db.marker_get_b_box_x_min("drc_test", 0), 1000);
    assert_eq!(db.marker_get_b_box_y_min("drc_test", 0), 2000);
    assert_eq!(db.marker_get_b_box_x_max("drc_test", 0), 5000);
    assert_eq!(db.marker_get_b_box_y_max("drc_test", 0), 8000);
    assert!(db.marker_is_waived("drc_test", 0));
    assert_eq!(db.marker_get_comment("drc_test", 0), "test drc");
    assert_eq!(db.marker_get_line_number("drc_test", 0), 42);
    assert_eq!(db.marker_get_tech_layer("drc_test", 0), "met1");
}

#[test]
fn hierarchical_registry_dispatch() {
    // the same populated data is reachable through the generic registry (the vyges mcp path)
    let db = Db::open(HIER).unwrap();
    let master = registry::get(&db, "dbModInst", "get_master", &["u_leaf".into()]).unwrap();
    assert_eq!(master, serde_json::json!("leaf"));
    let waived = registry::get(&db, "dbMarker", "is_waived", &["drc_test".into(), "0".into()]).unwrap();
    assert_eq!(waived, serde_json::json!(true));
}
