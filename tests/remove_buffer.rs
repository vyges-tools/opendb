// SPDX-License-Identifier: Apache-2.0
//! `Db::remove_buffer` — the database half of the resizer's buffer removal, over odb's own
//! `dbNet::mergeNet` — and `Db::net_can_merge` (`dbNet::canMergeNet`). Each test states the rule
//! it pins: which net survives, what it takes over, and that an ECO undo puts everything back.

use std::collections::BTreeMap;

use vyges_opendb::Db;

fn design() -> Db {
    let mut db = Db::new();
    db.read_lef("tests/fixtures/insert_buffer/cells.lef").expect("lef");
    db.read_def("tests/fixtures/insert_buffer/top.def", "default").expect("def");
    db
}

/// Every net with a pin: its instance pins (`inst/pin`) and ports (`PORT:name`), sorted.
fn nets(db: &Db) -> BTreeMap<String, Vec<String>> {
    let mut out = BTreeMap::new();
    for net in db.net_names() {
        let mut members: Vec<String> = db.net_iterms(&net);
        members.extend(db.net_bterms(&net).into_iter().map(|b| format!("PORT:{b}")));
        if !members.is_empty() {
            members.sort();
            out.insert(net, members);
        }
    }
    out
}

fn expect(pairs: &[(&str, &[&str])]) -> BTreeMap<String, Vec<String>> {
    pairs
        .iter()
        .map(|(n, m)| {
            let mut v: Vec<String> = m.iter().map(|s| s.to_string()).collect();
            v.sort();
            (n.to_string(), v)
        })
        .collect()
}

/// drvr -> a -> buf -> b -> {load0, load1}; `out_port` puts a port on `b`.
fn chain(out_port: bool) -> Db {
    let mut db = design();
    for n in ["drvr", "buf", "load0", "load1"] {
        db.create_inst("BUF_X1", n).unwrap();
    }
    for n in ["a", "b"] {
        db.create_net(n).unwrap();
    }
    db.connect("drvr", "Z", "a").unwrap();
    db.connect("buf", "A", "a").unwrap();
    db.connect("buf", "Z", "b").unwrap();
    db.connect("load0", "A", "b").unwrap();
    db.connect("load1", "A", "b").unwrap();
    if out_port {
        db.create_bterm("b", "y").unwrap();
        db.bterm_set_io_type("y", "OUTPUT").unwrap();
    }
    db
}

// Rule (removeBuffer): with no port on either side the INPUT net survives and takes the output
// net's loads; the buffer and the output net are gone.
#[test]
fn the_input_net_survives_an_internal_buffer() {
    let mut db = chain(false);
    assert_eq!(db.remove_buffer("buf", "A", "Z").unwrap(), "a");
    assert_eq!(nets(&db), expect(&[("a", &["drvr/Z", "load0/A", "load1/A"])]));
    assert!(!db.inst_names().contains(&"buf".to_string()));
    assert!(!db.net_names().contains(&"b".to_string()));
}

// Rule (removeBuffer): when only the OUTPUT net has a port, it survives (the port keeps its net)
// and takes the driver.
#[test]
fn a_port_on_the_output_keeps_the_output_net() {
    let mut db = chain(true);
    assert_eq!(db.remove_buffer("buf", "A", "Z").unwrap(), "b");
    assert_eq!(nets(&db), expect(&[("b", &["PORT:y", "drvr/Z", "load0/A", "load1/A"])]));
    assert!(!db.net_names().contains(&"a".to_string()));
}

// Rule (MoveCommitter::restoreJournal → undoEco): the removal is journaled — undone, the buffer
// and both nets are back as they were.
#[test]
fn an_eco_undo_puts_the_buffer_back() {
    let mut db = chain(false);
    let before = nets(&db);
    db.eco_begin().unwrap();
    db.remove_buffer("buf", "A", "Z").unwrap();
    assert!(!db.eco_is_empty().unwrap());
    db.eco_undo().unwrap();
    assert_eq!(nets(&db), before);
    assert_eq!(db.inst_master("buf"), "BUF_X1");
}

// Rule (dbNet::canMergeNet): a dont_touch net, or a dont_touch instance on the net to go,
// forbids the merge. (Setting dont_touch is the generated write surface.)
#[cfg(feature = "gen-write")]
#[test]
fn dont_touch_forbids_a_merge() {
    let mut db = chain(false);
    assert!(db.net_can_merge("a", "b").unwrap());
    db.inst_set_do_not_touch("load0", true).unwrap();
    assert!(!db.net_can_merge("a", "b").unwrap(), "a dont_touch instance on the net to go");
    assert!(db.net_can_merge("b", "a").unwrap(), "only the net to go's instances count");
}
