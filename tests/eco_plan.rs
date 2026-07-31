// SPDX-License-Identifier: Apache-2.0
//! Replaying a timing-repair plan into the database.
//!
//! This closes the timing-driven ECO loop: the planner decides in the timer (where placement is
//! not an input, so no legalization question arises), emits a plan as a **file**, and this side
//! replays it. The two halves are joined by the file on purpose — one lives in the timer and
//! one in the database layer, and neither links the other.
//!
//! The plans used here are literal `vyges-eco-plan-v1` documents of the shape `vyges-sta-si`
//! emits, so this tests the actual interchange rather than an in-memory hand-off.
use vyges_opendb::eco::{apply_eco_plan, PLAN_SCHEMA};
use vyges_opendb::Db;

const FIXTURE: &str = "tests/fixtures/counter.odb";

/// Build a plan targeting a real driven pin in the fixture, the way the planner would.
fn plan_for(db: &Db, n: usize) -> (String, Vec<(String, String)>) {
    let targets: Vec<(String, String)> = db
        .inst_names()
        .into_iter()
        .filter_map(|i| {
            let p = db.input_pin(&i);
            (!p.is_empty() && !db.net_of(&i, &p).is_empty()).then_some((i, p))
        })
        .take(n)
        .collect();
    let buf = db.find_master("buf");
    let fixes: Vec<String> = targets
        .iter()
        .enumerate()
        .map(|(i, (inst, pin))| {
            format!(
                r#"{{"op":"insert_delay","target":"{inst}/{pin}","inst":"{inst}","pin":"{pin}","cell":"{buf}","name":"vy_hold{i}","whs_before_ns":-0.05,"whs_after_ns":0.01}}"#
            )
        })
        .collect();
    let json = format!(
        r#"{{"schema":"{PLAN_SCHEMA}","design":"{}","fix_count":{},"fixes":[{}],"rejected":[]}}"#,
        db.block_name(),
        fixes.len(),
        fixes.join(",")
    );
    (json, targets)
}

#[test]
fn a_plan_is_replayed_into_the_design() {
    let mut db = Db::open(FIXTURE).unwrap();
    let (plan, targets) = plan_for(&db, 2);
    let before = db.num_insts();

    let applied = apply_eco_plan(&mut db, &plan, true).unwrap();

    assert_eq!(applied.applied, targets.len());
    assert_eq!(db.num_insts(), before + targets.len(), "one cell per fix");
    // the planner's chosen names are honoured verbatim, so effects trace back to the plan
    for name in &applied.inserted {
        assert!(db.inst_names().contains(name), "{name} should exist in the design");
    }
    assert_eq!(applied.inserted, vec!["vy_hold0".to_string(), "vy_hold1".to_string()]);
}

#[test]
fn an_empty_plan_is_a_no_op_not_an_error() {
    let mut db = Db::open(FIXTURE).unwrap();
    let before = db.num_insts();
    let plan = format!(
        r#"{{"schema":"{PLAN_SCHEMA}","design":"{}","fix_count":0,"fixes":[],"rejected":[]}}"#,
        db.block_name()
    );
    let applied = apply_eco_plan(&mut db, &plan, true).unwrap();
    assert_eq!(applied.applied, 0);
    assert_eq!(db.num_insts(), before);
}

#[test]
fn a_plan_for_a_different_design_is_refused() {
    // Plans are files, and files get moved around. Applying one to the wrong block would be
    // silent corruption, so the mismatch has to be loud.
    let mut db = Db::open(FIXTURE).unwrap();
    let before = db.num_insts();
    let (good, _) = plan_for(&db, 1);
    let wrong = good.replace(&format!("\"design\":\"{}\"", db.block_name()), "\"design\":\"some_other_block\"");

    let err = apply_eco_plan(&mut db, &wrong, true).unwrap_err().to_string();
    assert!(err.contains("some_other_block"), "the error should name the mismatch: {err}");
    assert_eq!(db.num_insts(), before, "a refused plan must not have applied anything");

    // ...and the check is opt-out for callers who know what they are doing
    let applied = apply_eco_plan(&mut db, &wrong, false).unwrap();
    assert_eq!(applied.applied, 1);
}

#[test]
fn an_unknown_schema_is_refused_rather_than_guessed_at() {
    let mut db = Db::open(FIXTURE).unwrap();
    let (plan, _) = plan_for(&db, 1);
    let future = plan.replace(PLAN_SCHEMA, "vyges-eco-plan-v99");
    let err = apply_eco_plan(&mut db, &future, true).unwrap_err().to_string();
    assert!(err.contains("v99"), "the error should name the schema it got: {err}");
    assert!(err.contains(PLAN_SCHEMA), "and the one it wanted: {err}");
}

#[test]
fn a_plan_that_fails_partway_leaves_nothing_behind() {
    // The outcome worth ruling out. A half-applied plan matches neither the plan nor the timing
    // that justified it, and it looks like it worked.
    let mut db = Db::open(FIXTURE).unwrap();
    let before = db.num_insts();
    let (good, _) = plan_for(&db, 2);

    // append a third fix that cannot possibly apply
    let broken = good.replace(
        "],\"rejected\"",
        &format!(
            r#",{{"op":"insert_delay","target":"no_such_inst/A","inst":"no_such_inst","pin":"A","cell":"{}","name":"vy_bad"}}],"rejected""#,
            db.find_master("buf")
        ),
    );

    let err = apply_eco_plan(&mut db, &broken, true).unwrap_err();
    assert!(!err.to_string().is_empty());
    assert_eq!(db.num_insts(), before, "the two good fixes must have been rolled back too");
    for name in ["vy_hold0", "vy_hold1", "vy_bad"] {
        assert!(!db.inst_names().contains(&name.to_string()), "{name} should not survive");
    }
}

#[test]
fn an_unsupported_op_is_refused_loudly_not_skipped_silently() {
    // `resize` needs dbInst::swapMaster, which is not bound yet. Skipping it quietly would
    // leave the design inconsistent with the plan's predicted timing.
    let mut db = Db::open(FIXTURE).unwrap();
    let before = db.num_insts();
    let plan = format!(
        r#"{{"schema":"{PLAN_SCHEMA}","design":"{}","fixes":[{{"op":"resize","target":"x","inst":"x","cell":"BUFX4"}}]}}"#,
        db.block_name()
    );
    let err = apply_eco_plan(&mut db, &plan, true).unwrap_err().to_string();
    assert!(err.contains("resize"), "the error should name the op: {err}");
    assert_eq!(db.num_insts(), before);
}

#[test]
fn a_malformed_plan_is_rejected_without_touching_the_design() {
    let mut db = Db::open(FIXTURE).unwrap();
    let before = db.num_insts();
    for bad in ["", "{", "[]", r#"{"schema":"vyges-eco-plan-v1"}"#] {
        assert!(apply_eco_plan(&mut db, bad, true).is_err(), "should reject: {bad:?}");
        assert_eq!(db.num_insts(), before);
    }
}
