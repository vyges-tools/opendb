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
