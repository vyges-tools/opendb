// SPDX-License-Identifier: Apache-2.0
// check_3dblox — OpenDB's 3D structural linter, run over a real chiplet assembly.
//
// This is the sign-off shape we care about: a CHECKER that reports through ordinary dbMarker
// objects and modifies nothing. The linter compiles into our libodb with no new dependencies
// (only checker.cpp from src/3dblox; the rest of that directory needs yaml-cpp and OpenSTA),
// and its findings are read back through the marker accessors we already had.
//
// The 3D fixture is DELIBERATELY defective — see make-3d-fixture.cpp. Asserting "0 violations"
// would be a weak test, since 0 is also what a broken checker returns.
use vyges_opendb::{registry, Db};

const FIXTURE: &str = "tests/fixtures/chiplet3d.odb";

fn marker_count(db: &Db, category: &str) -> serde_json::Value {
    registry::get(db, "dbMarkerCategory", "get_marker_count", &[category.to_string()]).unwrap()
}

fn marker(db: &Db, category: &str, i: usize, field: &str) -> String {
    registry::get(db, "dbMarker", field, &[category.to_string(), i.to_string()])
        .unwrap()
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[test]
fn a_clean_design_reports_no_violations() {
    // A flat 2D design has no chiplet assembly to violate anything. Establishes that a zero is
    // meaningful before the tests below read anything into a non-zero.
    let db = Db::open("tests/fixtures/counter.odb").unwrap();
    assert_eq!(db.check_3dblox().unwrap(), 0);
    let db = Db::open("tests/fixtures/hier.odb").unwrap();
    assert_eq!(db.check_3dblox().unwrap(), 0);
}

#[test]
fn the_linter_finds_the_fixtures_two_planted_defects() {
    let db = Db::open(FIXTURE).unwrap();
    assert_eq!(db.check_3dblox().unwrap(), 2);

    // one per check, filed under the "3DBlox" top category on the chip
    assert_eq!(marker_count(&db, "3DBlox/Connection regions"), serde_json::json!(1));
    assert_eq!(marker_count(&db, "3DBlox/Floating chips"), serde_json::json!(1));

    // and the checks that pass really do report nothing
    for clean in ["3DBlox/Overlapping chips", "3DBlox/Bump Alignment", "3DBlox/Alignment Markers"] {
        assert_eq!(marker_count(&db, clean), serde_json::json!(0), "{clean} should be clean");
    }
}

#[test]
fn a_violation_explains_itself_in_geometric_terms() {
    // The substance of the finding, not just its count. u_top is mirrored in Z, so top_die's
    // FRONT region ends up facing BOTTOM — and it is bonded to a BACK region that also faces
    // BOTTOM. Two surfaces pointing the same way cannot mate.
    //
    // Note the checker is reasoning over the UNFOLDED model here: nothing in the folded
    // database says "front faces bottom"; that falls out of applying u_top's orientation.
    let db = Db::open(FIXTURE).unwrap();
    db.check_3dblox().unwrap();

    let comment = marker(&db, "3DBlox/Connection regions", 0, "get_comment");
    assert!(comment.contains("bond0"), "should name the connection: {comment}");
    assert!(comment.contains("u_top/front"), "should name the top region: {comment}");
    assert!(comment.contains("u_base/back"), "should name the bottom region: {comment}");
    assert!(comment.contains("BOTTOM"), "should explain the face directions: {comment}");

    // the second finding is a CONSEQUENCE of the first — with its only bond invalid, u_base is
    // not reachable, so it is reported as an isolated set
    let floating = marker(&db, "3DBlox/Floating chips", 0, "get_comment");
    assert!(floating.contains("u_base"), "should name the isolated chip: {floating}");
}

#[test]
fn lint_markers_are_reachable_through_the_ordinary_marker_surface() {
    // The point of the linter landing on dbMarker: no new read path was needed. These are the
    // same accessors the 2D DRC markers use, addressed with a slash path because the 3D
    // categories nest under a top category on the CHIP rather than sitting on the block.
    let db = Db::open(FIXTURE).unwrap();
    db.check_3dblox().unwrap();

    // the top category aggregates every sub-category's markers
    assert_eq!(marker_count(&db, "3DBlox"), serde_json::json!(2));
    // a marker carries a name as well as a comment
    assert!(marker(&db, "3DBlox/Floating chips", 0, "get_name").contains("u_base"));
    // an unknown path misses gracefully rather than erroring
    assert_eq!(marker_count(&db, "3DBlox/No Such Check"), serde_json::json!(0));
}

#[test]
fn checking_is_idempotent() {
    // The checker uses createOrReplace, so re-running must not accumulate duplicate markers.
    // Anything driving this in a loop (an agent, a CI step) depends on that.
    let db = Db::open(FIXTURE).unwrap();
    let first = db.check_3dblox().unwrap();
    let second = db.check_3dblox().unwrap();
    assert_eq!(first, second, "re-running the linter must not accumulate markers");
    assert_eq!(marker_count(&db, "3DBlox"), serde_json::json!(2));
}

#[test]
fn diagnostics_can_be_captured_off_stdout() {
    // OpenDB logs "[WARNING ODB-nnnn] …" to STDOUT by default, which would corrupt any caller
    // emitting JSON there — the check-3dblox subcommand being the immediate one. Capture must
    // return that text rather than letting it escape.
    let mut db = Db::open(FIXTURE).unwrap();
    let (violations, logs) = db.with_captured_logs(|db| db.check_3dblox().unwrap());
    assert_eq!(violations, 2);
    assert!(logs.contains("ODB-"), "expected OpenDB diagnostics to be captured, got: {logs:?}");
    assert!(logs.contains("bond0"), "captured text should name the failing connection: {logs:?}");

    // sinks are restored afterwards, so a second capture still works rather than asserting
    let (_, again) = db.with_captured_logs(|db| db.check_3dblox().unwrap());
    assert!(again.contains("ODB-"), "capture must be repeatable: {again:?}");
}

#[test]
fn the_linter_leaves_the_design_alone() {
    // "Checker, not repairer" — it annotates with markers and touches nothing else.
    let db = Db::open(FIXTURE).unwrap();
    let before = (db.num_insts(), db.num_nets(), db.num_bterms(), db.total_wire_length());
    db.check_3dblox().unwrap();
    let after = (db.num_insts(), db.num_nets(), db.num_bterms(), db.total_wire_length());
    assert_eq!(before, after);
}
