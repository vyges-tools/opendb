// SPDX-License-Identifier: Apache-2.0
//! The whole-list accessors read their table in ONE pass (grt spent 77 minutes listing 239,544
//! instances one index at a time, each call walking the table from its start). The contract that
//! must survive that change is the ORDER: callers key on database order (pin access's first
//! instance of a class is its representative), so a one-pass list must equal the per-index one.
use vyges_opendb::Db;

const FIXTURE: &str = "tests/fixtures/counter.odb";

#[test]
fn a_one_pass_list_is_the_per_index_list() {
    let db = Db::open(FIXTURE).expect("fixture");
    let insts = db.inst_names();
    assert_eq!(insts.len(), db.num_insts());
    assert!(!insts.is_empty(), "the fixture must have instances, or this checks nothing");
    assert_eq!(insts, (0..db.num_insts()).map(|i| db.nth_inst_name(i)).collect::<Vec<_>>());
    let bterms = db.bterm_names();
    assert_eq!(bterms.len(), db.num_bterms());
    assert_eq!(bterms, (0..db.num_bterms()).map(|i| db.nth_bterm_name(i)).collect::<Vec<_>>());
    assert_eq!(db.net_names().len(), db.num_nets());
}

/// A net's instance pins, seen from the net, are exactly the pins that name it from the instance
/// side — each once.
#[test]
fn a_nets_pins_match_the_instances_view() {
    let db = Db::open(FIXTURE).expect("fixture");
    let mut from_nets: Vec<String> = Vec::new();
    for n in db.net_names() {
        let pins = db.net_iterms(&n);
        let mut uniq = pins.clone();
        uniq.sort();
        uniq.dedup();
        assert_eq!(uniq.len(), pins.len(), "net {n} lists a pin twice");
        from_nets.extend(pins);
    }
    let mut from_insts: Vec<String> = Vec::new();
    for i in db.inst_names() {
        for p in db.iterm_names(&i) {
            if !db.net_of(&i, &p).is_empty() {
                from_insts.push(format!("{i}/{p}"));
            }
        }
    }
    assert!(!from_insts.is_empty());
    from_nets.sort();
    from_insts.sort();
    assert_eq!(from_nets, from_insts);
}
