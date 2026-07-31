// SPDX-License-Identifier: Apache-2.0
//! `swap_master` — replacing an instance's library cell in place.
//!
//! G2 of the timing-driven ECO loop: the setup-repair move (upsize a critical driver), and
//! equally the Vt-swap and downsize moves. `dbInst::swapMaster` takes a `dbMaster*`, so the
//! binding generator could never reach it — this is a hand-written shim like `insert_buffer`.
//!
//! The `eco_demo` fixture is used because it has two masters with identical pins (`INV` and
//! `BUF`, both `A`/`Y`) — which is what a real resize looks like: same pins, different drive.
//! OpenDB enforces the pin part and not the logic part, and the tests distinguish the two.
use vyges_opendb::Db;

const DEMO: &str = "tests/fixtures/eco_demo.odb";

#[test]
fn an_instance_cell_is_replaced_in_place() {
    let mut db = Db::open(DEMO).unwrap();
    assert_eq!(db.inst_master("g1"), "INV");
    let (before_loc, before_insts) = (db.inst_location("g1"), db.num_insts());

    assert!(db.swap_master("g1", "BUF").unwrap());

    assert_eq!(db.inst_master("g1"), "BUF", "the cell must actually have changed");
    assert_eq!(db.num_insts(), before_insts, "a resize replaces, it does not add");
    assert_eq!(db.inst_location("g1"), before_loc, "the instance stays where it was");
}

#[test]
fn connectivity_survives_a_pin_compatible_swap() {
    // The whole point of a resize is that the design still works afterwards. INV and BUF share
    // pin names, so every connection must come through unchanged.
    let mut db = Db::open(DEMO).unwrap();
    let before: Vec<(String, String)> = db
        .iterm_names("g1")
        .into_iter()
        .map(|p| (p.clone(), db.net_of("g1", &p)))
        .collect();
    assert!(!before.is_empty());

    db.swap_master("g1", "BUF").unwrap();

    let after: Vec<(String, String)> = db
        .iterm_names("g1")
        .into_iter()
        .map(|p| (p.clone(), db.net_of("g1", &p)))
        .collect();
    assert_eq!(after, before, "pin-to-net connectivity must be preserved across the swap");
}

#[test]
fn a_swap_rolls_back_with_the_eco_journal() {
    // swapMaster is journaled (kSwapObject), which is what lets a speculative resize be undone.
    // Without this the whole plan-and-apply model would not extend to setup repair.
    let mut db = Db::open(DEMO).unwrap();
    db.eco_begin().unwrap();
    db.swap_master("g1", "BUF").unwrap();
    assert_eq!(db.inst_master("g1"), "BUF");

    db.eco_undo().unwrap();
    assert_eq!(db.inst_master("g1"), "INV", "undo must restore the original cell");
}

#[test]
fn an_unknown_instance_or_master_errors_rather_than_silently_doing_nothing() {
    let mut db = Db::open(DEMO).unwrap();
    let e = db.swap_master("no_such_inst", "BUF").unwrap_err().to_string();
    assert!(e.contains("no_such_inst"), "should name the missing instance: {e}");

    let e = db.swap_master("g1", "NO_SUCH_CELL").unwrap_err().to_string();
    assert!(e.contains("NO_SUCH_CELL"), "should name the missing master: {e}");
    assert_eq!(db.inst_master("g1"), "INV", "a failed swap must change nothing");
}

#[test]
fn swapping_to_the_same_cell_is_a_harmless_no_op() {
    // A planner may well propose the identity swap while exploring; it must not be an error.
    let mut db = Db::open(DEMO).unwrap();
    assert!(db.swap_master("g1", "INV").unwrap());
    assert_eq!(db.inst_master("g1"), "INV");
}

#[test]
fn a_pin_incompatible_swap_is_refused_by_the_database() {
    // Better than expected, and worth pinning: OpenDB compares the two masters' MTerms — same
    // count, same names — and refuses otherwise. DFF has CK/D/Q where INV has A/Y, so this is
    // rejected rather than stranding g1's connections.
    //
    // Note what this does NOT protect against: same pins is not same function. Two cells could
    // both be A/Y and compute opposite things, and the database would happily swap them. That
    // part is the caller's problem, which is why a planner needs library equivalence classes
    // before it can drive this move.
    let mut db = Db::open(DEMO).unwrap();
    assert!(!db.swap_master("g1", "DFF").unwrap(), "incompatible pins must be refused");
    assert_eq!(db.inst_master("g1"), "INV", "and the instance must be untouched");
    assert_eq!(db.iterm_names("g1"), vec!["A".to_string(), "Y".to_string()]);
}
