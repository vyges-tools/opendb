// SPDX-License-Identifier: Apache-2.0
//! `Db::swap_pins` (the database half of the resizer's pin swap) and odb's own name uniquifiers
//! (`Db::make_new_inst_name` / `make_new_net_name`), each with the rule it pins.

use vyges_opendb::Db;

fn design() -> Db {
    let mut db = Db::new();
    db.read_lef("tests/fixtures/insert_buffer/cells.lef").expect("lef");
    db.read_def("tests/fixtures/insert_buffer/top.def", "default").expect("def");
    db
}

/// A two-input instance: `a` on its first input, `b` on its second, if the fixture's cells have
/// one — else the test is about a buffer's single input and is skipped.
fn two_input_master(db: &Db) -> Option<(String, String, String, String)> {
    for i in 0..db.num_masters().ok()? {
        let m = db.nth_master_name(i).ok()?;
        let terms = db.master_mterms(&m).ok()?;
        let signal: Vec<String> = terms.iter().filter(|(_, t)| t != "POWER" && t != "GROUND").map(|(n, _)| n.clone()).collect();
        if signal.len() >= 3 {
            return Some((m, signal[0].clone(), signal[1].clone(), signal[2].clone()));
        }
    }
    None
}

// Rule (Resizer::swapPins): the two pins trade nets; a pin without a net makes it a no-op.
#[test]
fn swapped_pins_trade_nets() {
    let mut db = design();
    let Some((master, p1, p2, _)) = two_input_master(&db) else { return };
    db.create_inst(&master, "g").unwrap();
    db.create_net("a").unwrap();
    db.create_net("b").unwrap();
    db.connect("g", &p1, "a").unwrap();
    assert!(!db.swap_pins("g", &p1, &p2).unwrap(), "one pin has no net: nothing done");
    db.connect("g", &p2, "b").unwrap();
    assert!(db.swap_pins("g", &p1, &p2).unwrap());
    assert_eq!((db.net_of("g", &p1), db.net_of("g", &p2)), ("b".to_string(), "a".to_string()));
}

// Rule (dbBlock::makeNewInstName / makeNewNetName, ALWAYS): the base and the block's next index;
// each call advances it, so two calls never give the same name.
#[test]
fn new_names_advance_the_block_counter() {
    let mut db = design();
    let a = db.make_new_inst_name("clone", "ALWAYS").unwrap();
    let b = db.make_new_inst_name("clone", "ALWAYS").unwrap();
    assert!(a.starts_with("clone") && b.starts_with("clone") && a != b, "{a} {b}");
    let n1 = db.make_new_net_name("net", "ALWAYS").unwrap();
    let n2 = db.make_new_net_name("net", "ALWAYS").unwrap();
    assert!(n1.starts_with("net") && n1 != n2, "{n1} {n2}");
}
