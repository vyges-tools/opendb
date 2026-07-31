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
    // A quietly skipped fix would leave the design inconsistent with the plan's predicted
    // timing, which is worse than failing.
    let mut db = Db::open(FIXTURE).unwrap();
    let before = db.num_insts();
    let plan = format!(
        r#"{{"schema":"{PLAN_SCHEMA}","design":"{}","fixes":[{{"op":"teleport","target":"x","inst":"x","cell":"BUFX4"}}]}}"#,
        db.block_name()
    );
    let err = apply_eco_plan(&mut db, &plan, true).unwrap_err().to_string();
    assert!(err.contains("teleport"), "the error should name the op: {err}");
    assert_eq!(db.num_insts(), before);
}

#[test]
fn a_resize_plan_replaces_the_cell() {
    // The setup-repair move, now that swapMaster is bound. A resize replaces in place — no new
    // instance — so it is invisible to an instance count and has to be checked by master.
    let mut db = Db::open(DEMO).unwrap();
    assert_eq!(db.inst_master("g1"), "INV");
    let before = db.num_insts();
    let plan = format!(
        r#"{{"schema":"{PLAN_SCHEMA}","design":"eco_demo","fixes":[{{"op":"resize","target":"g1","inst":"g1","cell":"BUF"}}]}}"#
    );

    let applied = apply_eco_plan(&mut db, &plan, true).unwrap();

    assert_eq!(applied.applied, 1);
    assert_eq!(applied.resized, vec!["g1".to_string()]);
    assert!(applied.inserted.is_empty(), "a resize inserts nothing");
    assert_eq!(db.inst_master("g1"), "BUF");
    assert_eq!(db.num_insts(), before);
}

#[test]
fn a_refused_resize_fails_the_whole_plan_rather_than_being_skipped() {
    // OpenDB refuses a pin-incompatible swap by returning false. Carrying on would leave the
    // design inconsistent with a plan whose predicted timing assumed the swap happened — so the
    // refusal has to fail the plan, and the mixed insertion before it must roll back too.
    let mut db = Db::open(DEMO).unwrap();
    let before = (db.num_insts(), db.inst_master("g1"));
    let plan = format!(
        r#"{{"schema":"{PLAN_SCHEMA}","design":"eco_demo","fixes":[{{"op":"insert_delay","target":"r1/D","inst":"r1","pin":"D","cell":"BUF","name":"vy_mix0"}},{{"op":"resize","target":"g1","inst":"g1","cell":"DFF"}}]}}"#
    );

    let err = apply_eco_plan(&mut db, &plan, true).unwrap_err().to_string();
    assert!(err.contains("g1"), "the error should name the instance it could not resize: {err}");
    assert_eq!((db.num_insts(), db.inst_master("g1")), before, "everything must roll back");
    assert!(!db.inst_names().contains(&"vy_mix0".to_string()));
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

// ---- the shared fixture: both halves of the loop on ONE design ------------------------------

/// `eco_demo.odb` and sta-si's `eco_demo.v` are emitted by the SAME generator
/// (opendb-lib `test/make-eco-fixture.cpp`), so instance, pin and cell names agree by
/// construction rather than by maintenance. `eco_demo.plan.json` is the real output of
/// `vyges-sta-si`'s hold planner run against that netlist — not a hand-written approximation.
const DEMO: &str = "tests/fixtures/eco_demo.odb";
const DEMO_PLAN: &str = "tests/fixtures/eco_demo.plan.json";

/// How many fixes of each op the checked-in plan carries. Derived from the file rather than
/// hard-coded, so improving the planner cannot silently invalidate the applier's tests — it was
/// hard-coded once, and a better plan promptly broke it.
fn plan_op_counts() -> (usize, usize) {
    let text = std::fs::read_to_string(DEMO_PLAN).unwrap();
    (
        text.matches(r#""op":"insert_delay""#).count(),
        text.matches(r#""op":"resize""#).count(),
    )
}

#[test]
fn the_planners_own_plan_applies_to_the_matching_design() {
    // This is the end-to-end the loop existed for: sta-si times the .v, decides, and emits a
    // plan; this applies that plan, unmodified, to the .odb of the same design.
    let mut db = Db::open(DEMO).unwrap();
    let plan = std::fs::read_to_string(DEMO_PLAN).unwrap();
    let (inserts, resizes) = plan_op_counts();
    assert!(inserts + resizes > 0, "the checked-in plan should not be empty");
    let before = (db.num_insts(), db.num_nets());

    let applied = apply_eco_plan(&mut db, &plan, true).unwrap();

    assert_eq!(applied.applied, inserts + resizes, "every fix should apply");
    assert_eq!(applied.inserted.len(), inserts);
    assert_eq!(applied.resized.len(), resizes);
    // insertions add an instance and split a net; resizes replace in place
    assert_eq!(db.num_insts(), before.0 + inserts, "one new cell per insertion");
    assert_eq!(db.num_nets(), before.1 + inserts, "each insertion splits a net");
}

#[test]
fn every_cell_the_plan_names_exists_in_the_design_library() {
    // The invariant the shared fixture is supposed to guarantee, asserted directly. The planner
    // may propose any cell its Liberty offers; if the database's library does not hold that
    // master the plan cannot apply — and the failure surfaces here, in the applier, a long way
    // from the cause. This caught exactly that after the timing library grew drive ladders the
    // fixture generator had not been taught about.
    let db = Db::open(DEMO).unwrap();
    let text = std::fs::read_to_string(DEMO_PLAN).unwrap();
    for chunk in text.split(r#""cell":""#).skip(1) {
        let cell = chunk.split('"').next().unwrap();
        assert!(
            !db.find_master(cell).is_empty(),
            "plan names cell '{cell}', which the design library does not hold"
        );
    }
}

#[test]
fn an_inserted_buffer_is_actually_spliced_into_the_path() {
    // Presence is not correctness. The repair only works if the buffer sits BETWEEN the old
    // driver and the sink: sink now reads a new net, and that net is driven by the buffer whose
    // input is the original net. A buffer merely dangling on the side would insert no delay at
    // all while looking, by instance count, exactly like success.
    let mut db = Db::open(DEMO).unwrap();
    // r1/D is the first fix's target; capture the net it sits on beforehand
    let original_net = db.net_of("r1", "D");
    assert!(!original_net.is_empty());

    let plan = std::fs::read_to_string(DEMO_PLAN).unwrap();
    apply_eco_plan(&mut db, &plan, true).unwrap();

    let new_net = db.net_of("r1", "D");
    assert_ne!(new_net, original_net, "the sink must have been re-pointed at a new net");

    // the buffer drives the sink's new net and is fed by the original one
    let buf_out = db.output_pin("vy_hold0");
    let buf_in = db.input_pin("vy_hold0");
    assert_eq!(db.net_of("vy_hold0", &buf_out), new_net, "buffer must drive the sink's net");
    assert_eq!(db.net_of("vy_hold0", &buf_in), original_net, "buffer must be fed by the old net");
    assert_eq!(db.inst_master("vy_hold0"), "BUF", "must use the cell the plan named");
}

#[test]
fn the_inserted_cell_lands_where_its_target_is() {
    // Inserted cells inherit the target instance's location — they overlap it, deliberately, and
    // legalization is a separate step. Asserting it here keeps that contract visible: a cell at
    // (0,0) by accident would be a placement bug hiding behind correct connectivity.
    let mut db = Db::open(DEMO).unwrap();
    let target = db.inst_location("r1");
    let plan = std::fs::read_to_string(DEMO_PLAN).unwrap();
    apply_eco_plan(&mut db, &plan, true).unwrap();
    assert_eq!(db.inst_location("vy_hold0"), target);
}
