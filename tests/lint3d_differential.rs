// SPDX-License-Identifier: Apache-2.0
//! check_3dblox, driven as a **write-then-check** flow rather than a read-only one.
//!
//! `lint3d.rs` opens a pre-built fixture and reads the violations in it. That exercises the
//! checker but not the loop an actual 3D flow runs: place a chiplet, rebuild the derived model,
//! lint, move it, lint again. This file drives that loop entirely through the safe Rust API.
//!
//! The situations are adapted from OpenROAD's own `src/odb/test/check_3dblox.tcl`
//! (BSD-3-Clause, The OpenROAD Authors) — the scenario, not the file: their script drives Tcl
//! against a `.3dbx` we cannot read, so the same geometric situations are reproduced here
//! against our fixture. Their scenario is the useful part, because it comes with the counts
//! their checker is expected to produce, and we run *their* checker (`checker.cpp` compiled
//! into our libodb). So a disagreement here is a defect in our bindings or in how we drive
//! them — which is exactly what a conformance test should be sensitive to.
//!
//! **Their floating-chip situations live in `chip_create.rs`, not here.** They need an assembly
//! that starts clean, and this fixture cannot: its connection is defective by construction, so
//! one chip is an isolated set in every placement. That was originally recorded here as a gap
//! needing C++ fixture work — it turned out to need the `dbChip*::create` bindings instead, and
//! once those landed the progression reproduced from a design built entirely in Rust.
//!
//! The cause is now known, and it was not what this fixture's generator claims. Bonding roles,
//! not orientation: the lower chip must present the **FRONT** face (which points TOP) and the
//! upper chip the **BACK** face (which points BOTTOM). This fixture bonds them the other way
//! round, which no orientation or offset can rescue — which is why `Connection regions` stayed
//! at 1 here under every geometry tried, including a perfectly mated pair.
//!
#![cfg(feature = "gen-write")]
use vyges_opendb::{registry, Db};

const FIXTURE: &str = "tests/fixtures/chiplet3d.odb";

fn markers(db: &Db, category: &str) -> i64 {
    registry::get(db, "dbMarkerCategory", "get_marker_count", &[category.to_string()])
        .unwrap()
        .as_i64()
        .expect("marker count is an integer")
}

/// Stack the two dice: base occupies z 0..1500, top sits on it at 1500..2200. Both mirrored in
/// Z so their bonding surfaces face each other, which is the physically sensible arrangement
/// even though this fixture's connection is deliberately defective in other respects.
fn stacked(db: &mut Db) {
    db.place_chip_inst("stack", "u_base", "MZ", 0, 0, 0).unwrap();
    db.place_chip_inst("stack", "u_top", "MZ", 0, 0, 1500).unwrap();
    db.construct_unfolded_model().unwrap();
}

#[test]
fn overlapping_dice_are_reported_and_the_report_clears_when_they_are_separated() {
    let mut db = Db::open(FIXTURE).unwrap();

    stacked(&mut db);
    db.check_3dblox().unwrap();
    assert_eq!(
        markers(&db, "3DBlox/Overlapping chips"),
        0,
        "stacked dice share a plane but no volume"
    );

    // Their situation 5: shift the upper die in X and Y and drop it so its z range (1000..1700)
    // cuts into the base's (0..1500). Two dice cannot occupy the same space.
    db.place_chip_inst("stack", "u_top", "MZ", 12000, 12000, 1000).unwrap();
    db.construct_unfolded_model().unwrap();
    db.check_3dblox().unwrap();
    assert_eq!(
        markers(&db, "3DBlox/Overlapping chips"),
        1,
        "a die driven into the one below it is one overlap"
    );

    // and it must clear again — a checker that only ever accumulates would pass the line above
    db.place_chip_inst("stack", "u_top", "MZ", 900_000, 900_000, 1000).unwrap();
    db.construct_unfolded_model().unwrap();
    db.check_3dblox().unwrap();
    assert_eq!(
        markers(&db, "3DBlox/Overlapping chips"),
        0,
        "moved clear in X and Y, the overlap must be withdrawn, not remembered"
    );
}

#[test]
fn the_unfolded_geometry_goes_stale_until_the_model_is_rebuilt() {
    // The unfolded tables are derived and never serialised, and nothing rebuilds them when a
    // chip moves — so a caller that reads them straight after a placement gets the PREVIOUS
    // geometry back. No error, no warning, which is the worst shape a wrong answer can take.
    // This is what `construct_unfolded_model` exists for.
    let mut db = Db::open(FIXTURE).unwrap();
    let (side0, z0) = (
        db.unfoldedregion_get_effective_side("u_top", 0),
        db.unfoldedregion_get_surface_z("u_top", 0),
    );

    db.place_chip_inst("stack", "u_top", "R0", 0, 0, 1500).unwrap();
    assert_eq!(
        (db.unfoldedregion_get_effective_side("u_top", 0), db.unfoldedregion_get_surface_z("u_top", 0)),
        (side0, z0),
        "the move must not be visible in the unfolded model until it is rebuilt"
    );

    db.construct_unfolded_model().unwrap();
    assert_eq!(
        (db.unfoldedregion_get_effective_side("u_top", 0), db.unfoldedregion_get_surface_z("u_top", 0)),
        ("TOP".to_string(), 2200),
        "and after the rebuild it reads the placement just written"
    );
}

#[test]
fn the_linter_refreshes_the_unfolded_model_itself() {
    // Measured, and the opposite of the natural assumption — OpenROAD's own script calls
    // constructUnfoldedModel before every check_3dblox, which reads as though the linter needed
    // it. It does not: the checker rebuilds the model on its own, which is why the overlap test
    // above is honest without a rebuild between move and check.
    //
    // Worth pinning precisely because it is upstream behaviour we rely on but do not control:
    // if a future ODB stopped refreshing, every lint-after-move would silently answer stale, and
    // this test is the only thing that would say so.
    let mut db = Db::open(FIXTURE).unwrap();
    db.place_chip_inst("stack", "u_top", "R0", 0, 0, 1500).unwrap();
    db.check_3dblox().unwrap();
    assert_eq!(
        (db.unfoldedregion_get_effective_side("u_top", 0), db.unfoldedregion_get_surface_z("u_top", 0)),
        ("TOP".to_string(), 2200),
        "check_3dblox is expected to rebuild the unfolded model before it reasons over it"
    );
}

#[test]
fn placing_a_chip_inst_propagates_into_the_unfolded_geometry() {
    // The bindings under the situations above: a placement written through the safe API has to
    // reach the derived tables the checker reads, or the tests above would be asserting on
    // nothing. top_die is 700 thick, so its bonding surface is at the top of its own extent
    // when upright and at the bottom when mirrored in Z.
    let mut db = Db::open(FIXTURE).unwrap();

    db.place_chip_inst("stack", "u_top", "R0", 0, 0, 1500).unwrap();
    db.construct_unfolded_model().unwrap();
    assert_eq!(db.unfoldedregion_get_effective_side("u_top", 0), "TOP");
    assert_eq!(
        db.unfoldedregion_get_surface_z("u_top", 0),
        2200,
        "upright: the FRONT surface is at z + thickness"
    );

    db.place_chip_inst("stack", "u_top", "MZ", 0, 0, 1500).unwrap();
    db.construct_unfolded_model().unwrap();
    assert_eq!(
        db.unfoldedregion_get_effective_side("u_top", 0),
        "BOTTOM",
        "the Z mirror turns the FRONT surface over"
    );
    assert_eq!(
        db.unfoldedregion_get_surface_z("u_top", 0),
        1500,
        "mirrored: the FRONT surface is at z itself"
    );
}
