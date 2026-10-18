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

// The observer log records the same callbacks, independently: restarting the edit log (as a tool
// editing the block does) clears the edit log only, and a take of one leaves the other.
#[test]
fn the_observer_log_survives_an_edit_log_restart() {
    let mut db = design();
    let master = db.nth_master_name(0).unwrap();
    db.observer_log_start().unwrap();
    db.edit_log_start().unwrap();
    db.create_inst(&master, "g").unwrap();
    db.edit_log_start().unwrap();
    db.create_net("a").unwrap();
    assert_eq!(db.edit_log_take(), ["net_create|a"]);
    db.destroy_inst("g").unwrap();
    let destroyed = ["iterm_destroy|g/Z|", "iterm_destroy|g/A|", "inst_destroy|g"];
    // the observer's net events carry the net's id: the block's ids, in its net order
    let names = db.block_get_nets();
    let id = db.net_ids()[names.iter().position(|n| n == "a").expect("net a")];
    let mut want = vec![format!("inst_create|g|{master}"), format!("net_create|a|{id}")];
    want.extend(destroyed.iter().map(|s| s.to_string()));
    assert_eq!(db.observer_log_take(), want);
    assert!(db.observer_log_take().is_empty(), "a take empties the observer");
    assert_eq!(db.edit_log_take(), destroyed);
}

// Each net event of the observer names the net's id (a rename raises no callback; the id is the
// identity through one): a destroyed net's, then a new net's — odb may give it the freed slot.
#[test]
fn the_observer_names_each_net_by_its_id() {
    let mut db = design();
    db.create_net("a").unwrap();
    let ids = |db: &Db| db.block_get_nets().into_iter().zip(db.net_ids()).collect::<std::collections::HashMap<_, _>>();
    let a = ids(&db)["a"];
    db.observer_log_start().unwrap();
    db.destroy_net("a").unwrap();
    db.create_net("b").unwrap();
    let b = ids(&db)["b"];
    assert_eq!(db.observer_log_take(), [format!("net_destroy|a||{a}"), format!("net_create|b|{b}")]);
    assert_eq!(db.net_ids().len(), db.block_get_nets().len());
}
