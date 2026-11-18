// SPDX-License-Identifier: Apache-2.0
//! Instance-terminal access points as the global router reads them.
//!
//! `dbITerm::getAccessPoints` returns every master pin's points for the instance's pin-access
//! index, in a map keyed by master pin and compared by object id: master pins ascending by id,
//! each pin's points in stored order. A non-core terminal's router reads exactly that list (the
//! detailed router assigns non-core cells no preferred point), so the ORDER is part of the value.

#![allow(non_snake_case)]

use vyges_opendb::Db;

const FIXTURE: &str = "tests/fixtures/counter.odb";

/// The first instance with a signal terminal, and that terminal.
fn an_iterm(db: &Db) -> (String, String, String) {
    for inst in db.inst_names() {
        let master = db.inst_master(&inst);
        for (term, io) in db.master_mterm_io(&master).expect("a master's terms") {
            if io == "INPUT" {
                return (inst, master, term);
            }
        }
    }
    panic!("the fixture has an instance with an input terminal");
}

fn a_routing_layer(db: &Db) -> String {
    db.layers_with_direction()
        .expect("the tech defines layers")
        .into_iter()
        .map(|(name, _)| name)
        .find(|l| db.layer_get_routing_level(l) > 0)
        .expect("a routing layer")
}

#[test]
fn every_access_point_comes_back_in_STORED_order_relative_to_the_instance() {
    let mut db = Db::open(FIXTURE).expect("fixture opens");
    let (inst, master, term) = an_iterm(&db);
    let layer = a_routing_layer(&db);
    let level = db.layer_get_routing_level(&layer);
    let before = db.iterm_access_points(&inst, &term).expect("readable");
    // Appended out of coordinate order: the read must keep insertion order, not sort.
    for p in [(300, 40), (100, 20), (200, 30)] {
        db.mpin_add_access_point(&master, &term, 0, 0, p, &layer, 0, (0, 0), &[], &[]).expect("added");
    }
    let after = db.iterm_access_points(&inst, &term).expect("readable");
    assert_eq!(&after[..before.len()], &before[..], "points already there keep their place");
    assert_eq!(&after[before.len()..], &[(300, 40, level), (100, 20, level), (200, 30, level)]);
    assert_eq!(db.iterm_access_point_count(&inst, &term).expect("countable"), after.len());
    // No preferred point was set: the preferred list stays as it was.
    assert!(db.iterm_pref_access_points(&inst, &term).expect("readable").len() <= before.len());
}

#[test]
fn an_unknown_terminal_has_NO_access_points() {
    let db = Db::open(FIXTURE).expect("fixture opens");
    let (inst, _, _) = an_iterm(&db);
    assert!(db.iterm_access_points(&inst, "no_such_term").expect("readable").is_empty());
}
