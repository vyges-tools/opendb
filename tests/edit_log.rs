// SPDX-License-Identifier: Apache-2.0
//! `Db::edit_log_start` / `edit_log_take`: the block's edit callbacks, in the order odb raises them.

use vyges_opendb::Db;

fn design() -> Db {
    let mut db = Db::new();
    db.read_lef("tests/fixtures/insert_buffer/cells.lef").expect("lef");
    db.read_def("tests/fixtures/insert_buffer/top.def", "default").expect("def");
    db
}

// Rule (dbBlockCallBackObj): a connect is reported AFTER it (with the new net and its pins, this
// one included), a disconnect BEFORE it (with the old net, this pin still on it); creating a net
// or an instance is reported as it happens.
#[test]
fn connects_and_disconnects_are_logged_in_order() {
    let mut db = design();
    db.edit_log_start().unwrap();
    let master = db.nth_master_name(0).unwrap();
    let pin = db.master_mterms(&master).unwrap().into_iter().find(|(_, t)| t != "POWER" && t != "GROUND").map(|(n, _)| n).expect("a signal pin");
    db.create_inst(&master, "g").unwrap();
    db.create_net("a").unwrap();
    db.create_net("b").unwrap();
    db.connect("g", &pin, "a").unwrap();
    db.disconnect("g", &pin).unwrap();
    db.connect("g", &pin, "b").unwrap();
    let log = db.edit_log_take();
    let want = [
        format!("inst_create|g|{master}"),
        "net_create|a".into(),
        "net_create|b".into(),
        format!("iterm_connect|g/{pin}|a|g/{pin}"),
        format!("iterm_disconnect|g/{pin}|a|g/{pin}"),
        format!("iterm_connect|g/{pin}|b|g/{pin}"),
    ];
    assert_eq!(log, want);
    assert!(db.edit_log_take().is_empty(), "a take empties the log");
}

// Rule (dbBTerm::connect): the port moves onto the new net — off the one it was on — and the move
// is reported as a disconnect from the old net, then a connect to the new one.
#[test]
fn a_port_moves_to_the_net_it_is_connected_to() {
    let mut db = design();
    db.create_net("a").unwrap();
    db.create_net("b").unwrap();
    db.create_bterm("a", "p").unwrap();
    db.edit_log_start().unwrap();
    db.bterm_connect("p", "b").unwrap();
    assert_eq!(db.bterm_net("p"), "b");
    assert!(db.net_bterms("a").is_empty(), "the old net no longer has the port");
    assert_eq!(db.edit_log_take(), ["bterm_disconnect|p|a|p", "bterm_connect|p|b|p"]);
    assert!(db.bterm_connect("p", "no_such_net").is_err());
}
