// SPDX-License-Identifier: Apache-2.0
//! `Db::edit_log_start` / `edit_log_take`: the block's edit callbacks, in the order odb raises them.

use vyges_opendb::Db;

fn design() -> Db {
    let mut db = Db::new();
    db.read_lef("tests/fixtures/insert_buffer/cells.lef").expect("lef");
    db.read_def("tests/fixtures/insert_buffer/top.def", "default").expect("def");
    db
}

// Rule (dbBlockCallBackObj): a connect is reported AFTER it (with the new net), a disconnect BEFORE
// it (with the old net, this pin still on it); creating a net or an instance is reported as it
// happens. A load pin's line lists its net's drivers (none here).
#[test]
fn connects_and_disconnects_are_logged_in_order() {
    let mut db = design();
    db.edit_log_start().unwrap();
    let master = "BUF_X1";
    let pin = "A";
    db.create_inst(master, "g").unwrap();
    db.create_net("a").unwrap();
    db.create_net("b").unwrap();
    db.connect("g", pin, "a").unwrap();
    db.disconnect("g", pin).unwrap();
    db.connect("g", pin, "b").unwrap();
    let log = db.edit_log_take();
    let want = [
        format!("inst_create|g|{master}"),
        "net_create|a".into(),
        "net_create|b".into(),
        format!("iterm_connect|g/{pin}|a|"),
        format!("iterm_disconnect|g/{pin}|a|"),
        format!("iterm_connect|g/{pin}|b|"),
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

// Rule (OpenSTA `Sta::replaceCellPinInvalidate`: an input pin whose capacitance changes
// invalidates the delays of its net's DRIVERS; `Network::drivers(net)` is kept per net): a swap
// lists each terminal's net with the terminals that can drive it only — every instance pin the
// LEF does not call INPUT, every port it does not call OUTPUT — and the list kept for a net
// follows a connect to it.
#[test]
fn a_swap_lists_each_terms_net_with_its_drivers_only() {
    let mut db = design();
    for (m, n) in [("BUF_X1", "d"), ("BUF_X1", "l1"), ("BUF_X1", "l2")] {
        db.create_inst(m, n).unwrap();
    }
    db.create_net("x").unwrap();
    db.create_net("y").unwrap();
    db.create_bterm("x", "pin_in").unwrap();
    db.bterm_set_io_type("pin_in", "INPUT").unwrap();
    db.create_bterm("y", "pin_out").unwrap();
    db.bterm_set_io_type("pin_out", "OUTPUT").unwrap();
    db.connect("d", "Z", "x").unwrap();
    db.connect("l1", "A", "x").unwrap();
    db.connect("l2", "A", "x").unwrap();
    db.connect("l1", "Z", "y").unwrap();
    db.edit_log_start().unwrap();
    assert!(db.swap_master("l1", "BUF_X4").unwrap());
    // x: d/Z and the input port drive it; l1/A, l2/A do not. y: l1/Z drives; the output port not.
    assert_eq!(db.edit_log_take(), ["swap_before|l1|BUF_X1|BUF_X4|A=x=d/Z,pin_in;Z=y=l1/Z", "swap_after|l1|A=x=d/Z,pin_in;Z=y=l1/Z"]);
    // a driver connected to x after the swap above: the next swap lists it
    db.create_inst("BUF_X1", "d2").unwrap();
    db.connect("d2", "Z", "x").unwrap();
    db.edit_log_take();
    assert!(db.swap_master("l2", "BUF_X4").unwrap());
    let log = db.edit_log_take();
    let terms = log[0].rsplit('|').next().unwrap_or("");
    let x = terms.split(';').next().and_then(|t| t.strip_prefix("A=x=")).unwrap_or("");
    let mut drivers: Vec<&str> = x.split(',').collect();
    drivers.sort_unstable();
    assert_eq!(drivers, ["d/Z", "d2/Z", "pin_in"], "{log:?}");
}

// A supply terminal (LEF `USE POWER`/`GROUND`) names its net and lists no drivers: no timing
// port is a supply pin, and a supply net's "drivers" are every cell's supply pin — listing them
// made each resize copy the whole design.
#[test]
fn a_swap_lists_no_drivers_for_a_supply_terminal() {
    let mut db = Db::new();
    db.read_lef("tests/fixtures/edit_log_supply/cells.lef").expect("lef");
    db.read_def("tests/fixtures/edit_log_supply/top.def", "default").expect("def");
    db.create_net("vdd").unwrap();
    db.connect("u1", "VDD", "vdd").unwrap();
    db.connect("u2", "VDD", "vdd").unwrap();
    db.edit_log_start().unwrap();
    assert!(db.swap_master("u1", "BUF_X2").unwrap());
    let log = db.edit_log_take();
    // in drives `in` (an input port), u1/Y drives n1, vdd lists none, VSS is unconnected
    assert_eq!(log, ["swap_before|u1|BUF|BUF_X2|A=in=in;Y=n1=u1/Y;VDD=vdd=;VSS==", "swap_after|u1|A=in=in;Y=n1=u1/Y;VDD=vdd=;VSS=="], "{log:?}");
}

// Rule (OpenSTA `Sta::connectPinAfter` / `disconnectPinBefore`): a driver's wires go to every load
// of its net, a load's come from the net's drivers only. A connect or disconnect line lists the
// whole net for a terminal that can drive it and the net's drivers for a load — listing the whole
// net per load made a fanout repair O(fanout^2).
#[test]
fn a_load_lists_its_nets_drivers_and_a_driver_the_whole_net() {
    let mut db = design();
    for n in ["d", "l1", "l2"] {
        db.create_inst("BUF_X1", n).unwrap();
    }
    db.create_net("x").unwrap();
    db.connect("l1", "A", "x").unwrap();
    db.edit_log_start().unwrap();
    db.connect("d", "Z", "x").unwrap();
    db.connect("l2", "A", "x").unwrap();
    db.disconnect("l1", "A").unwrap();
    db.disconnect("d", "Z").unwrap();
    // (the net's terminals newest first, as odb keeps them)
    assert_eq!(db.edit_log_take(), ["iterm_connect|d/Z|x|d/Z,l1/A", "iterm_connect|l2/A|x|d/Z", "iterm_disconnect|l1/A|x|d/Z", "iterm_disconnect|d/Z|x|l2/A,d/Z"]);
    // the driver gone, a load lists none
    db.connect("l1", "A", "x").unwrap();
    assert_eq!(db.edit_log_take(), ["iterm_connect|l1/A|x|"]);
}
