// SPDX-License-Identifier: Apache-2.0
//! The ECO journal: speculative edits with a real rollback.
//!
//! G4 of the timing-driven ECO loop. The loop's whole shape depends on being able to *try* a
//! fix, re-time, and put the design back if it did not help — so "undo works" is not a detail,
//! it is the load-bearing assumption. These tests establish it against real edits rather than
//! trusting OpenDB's documentation, which describes the journal as a mechanism for replicating
//! deltas onto a remote database, not as a local undo.
use vyges_opendb::{Db, Error};

const FIXTURE: &str = "tests/fixtures/counter.odb";

/// A cheap structural signature — enough to notice a design that did not come back.
fn shape(db: &Db) -> (usize, usize, usize) {
    (db.num_insts(), db.num_nets(), db.num_bterms())
}

/// Insert a buffer on some driven input pin. Returns the instance name it created.
fn insert_a_buffer(db: &mut Db, name: &str) -> String {
    let inst = db
        .inst_names()
        .into_iter()
        .find(|i| {
            let p = db.input_pin(i);
            !p.is_empty() && !db.net_of(i, &p).is_empty()
        })
        .expect("fixture should have a driven input pin");
    let pin = db.input_pin(&inst);
    let buf = db.find_master("buf");
    db.insert_buffer(&inst, &pin, &buf, name, 10_000, 10_000)
        .expect("insert_buffer should succeed");
    name.to_string()
}

#[test]
fn undo_actually_reverts_a_real_edit() {
    let mut db = Db::open(FIXTURE).unwrap();
    let before = shape(&db);

    db.eco_begin().unwrap();
    let buf = insert_a_buffer(&mut db, "eco_probe0");
    assert!(db.inst_names().contains(&buf), "the buffer should exist while the ECO is open");
    assert_ne!(shape(&db), before, "the edit should have changed the design");

    db.eco_undo().unwrap();
    assert_eq!(shape(&db), before, "undo must restore instance/net/port counts");
    assert!(!db.inst_names().contains(&buf), "the inserted buffer must be gone");
}

#[test]
fn commit_keeps_the_edit() {
    let mut db = Db::open(FIXTURE).unwrap();
    let before = shape(&db);

    db.eco_begin().unwrap();
    let buf = insert_a_buffer(&mut db, "eco_keep0");
    db.eco_commit().unwrap();

    assert!(db.inst_names().contains(&buf), "a committed buffer must survive");
    assert_ne!(shape(&db), before, "a committed edit must still be there");
}

#[test]
fn eco_try_keeps_on_true_and_reverts_on_false() {
    let mut db = Db::open(FIXTURE).unwrap();
    let before = shape(&db);

    // rejected attempt — the design must come back untouched
    let kept = db
        .eco_try(|db| {
            insert_a_buffer(db, "eco_reject0");
            Ok(false) // "re-timed it; it did not help"
        })
        .unwrap();
    assert!(!kept);
    assert_eq!(shape(&db), before, "a rejected attempt must leave no trace");
    assert!(!db.inst_names().contains(&"eco_reject0".to_string()));

    // accepted attempt
    let kept = db
        .eco_try(|db| {
            insert_a_buffer(db, "eco_accept0");
            Ok(true)
        })
        .unwrap();
    assert!(kept);
    assert!(db.inst_names().contains(&"eco_accept0".to_string()));
}

#[test]
fn a_failing_attempt_is_rolled_back_not_left_half_applied() {
    // The case that matters most. A fix that edits, then errors, would otherwise leave the
    // design in a state neither the caller nor the timer can reason about — worse than having
    // no undo at all, because it looks like it worked.
    let mut db = Db::open(FIXTURE).unwrap();
    let before = shape(&db);

    let err = db
        .eco_try(|db| {
            insert_a_buffer(db, "eco_boom0");
            Err(Error::Odb("simulated failure after a partial edit".into()))
        })
        .unwrap_err();

    assert!(err.to_string().contains("simulated failure"), "the original error must survive");
    assert_eq!(shape(&db), before, "a failed attempt must be rolled back");
    assert!(!db.inst_names().contains(&"eco_boom0".to_string()));
}

#[test]
fn repeated_speculation_returns_to_the_same_state_every_time() {
    // The loop tries many candidates in sequence. Rollback has to be exact each time, not
    // approximately right — drift across attempts would silently corrupt the design.
    let mut db = Db::open(FIXTURE).unwrap();
    let before = shape(&db);

    for i in 0..5 {
        let kept = db
            .eco_try(|db| {
                insert_a_buffer(db, &format!("eco_spec{i}"));
                Ok(false)
            })
            .unwrap();
        assert!(!kept);
        assert_eq!(shape(&db), before, "state drifted after attempt {i}");
    }
    for i in 0..5 {
        assert!(!db.inst_names().contains(&format!("eco_spec{i}")));
    }
}

#[test]
fn an_untouched_eco_reports_empty() {
    // Lets a caller distinguish "the fix was a no-op" from "the fix did something" without
    // diffing the design.
    let mut db = Db::open(FIXTURE).unwrap();
    db.eco_begin().unwrap();
    assert!(db.eco_is_empty().unwrap(), "a fresh ECO with no edits should be empty");

    insert_a_buffer(&mut db, "eco_notempty0");
    assert!(!db.eco_is_empty().unwrap(), "an ECO with an edit should not be empty");
    db.eco_undo().unwrap();
}
