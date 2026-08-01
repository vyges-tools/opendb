// SPDX-License-Identifier: Apache-2.0
//! Building a 3D assembly from Rust — `dbChip*::create` and what it unblocks.
//!
//! Until these bindings the 3D surface was read-only in practice: an assembly could be inspected
//! and its chips moved, but the only way to bring one into existence was a separate C++ program
//! (`test/make-3d-fixture.cpp`). That was not an abstract limitation — it is why the
//! `check_3dblox` differential could reproduce upstream's *overlap* scenario and not its
//! *floating-chip progression*, which needs a clean multi-die assembly to start from.
//!
//! So the tests here are the proof the gap is closed, in order of what they establish:
//! a design can be built, it lints **clean**, and upstream's own scenario then runs against it
//! end to end.
//!
//! Scenario adapted from OpenROAD's `src/odb/test/check_3dblox.tcl` (BSD-3-Clause, The OpenROAD
//! Authors) — the situations and their expected counts, not the file.
#![cfg(feature = "gen-write")]
use vyges_opendb::{registry, Db};

fn markers(db: &Db, category: &str) -> i64 {
    registry::get(
        db,
        "dbMarkerCategory",
        "get_marker_count",
        &[category.to_string()],
    )
    .unwrap()
    .as_i64()
    .expect("marker count is an integer")
}

/// A physically sensible two-die stack: a 1500-thick substrate occupying z 0..1500, and a
/// 700-thick die resting exactly on it.
///
/// The bonding roles are the point. The lower chip presents a **FRONT** region, which under
/// `R0` faces **TOP** with its surface at `z + thickness`; the upper chip presents a **BACK**
/// region, which faces **BOTTOM** with its surface at `z`. Both land at 1500, pointing at each
/// other. Get those roles the wrong way round and no orientation or offset can rescue it —
/// which is exactly what the older hand-built fixture did, and why it could never lint clean.
fn clean_stack() -> Db {
    let mut db = Db::open("tests/fixtures/counter.odb").unwrap();
    db.create_chip("stack", "", "HIER").unwrap();
    db.create_chip_block("stack", "stack_blk").unwrap();
    db.create_chip("base", "", "SUBSTRATE").unwrap();
    db.create_chip("upper", "", "DIE").unwrap();

    for (c, w, h, t) in [("base", 60000, 50000, 1500), ("upper", 50000, 40000, 700)] {
        db.chip_set_width(c, w).unwrap();
        db.chip_set_height(c, h).unwrap();
        db.chip_set_thickness(c, t).unwrap();
    }
    db.create_chip_region("base", "up", "FRONT", "").unwrap();
    db.set_chip_region_box("base", "up", 0, 0, 60000, 50000)
        .unwrap();
    db.create_chip_region("upper", "down", "BACK", "").unwrap();
    db.set_chip_region_box("upper", "down", 0, 0, 50000, 40000)
        .unwrap();

    // regions before insts — `create` derives the region instances from the master as it
    // stands, so anything added afterwards is silently not instantiated
    db.create_chip_inst("stack", "base", "u_base").unwrap();
    db.create_chip_inst("stack", "upper", "u_upper").unwrap();
    db.place_chip_inst("stack", "u_base", "R0", 0, 0, 0)
        .unwrap();
    db.place_chip_inst("stack", "u_upper", "R0", 0, 0, 1500)
        .unwrap();

    db.create_chip_conn("bond", "stack", "u_upper", "down", "u_base", "up", 0)
        .unwrap();
    db.set_top_chip("stack").unwrap();
    db.construct_unfolded_model().unwrap();
    db
}

#[test]
fn an_assembly_built_from_rust_lints_clean() {
    // The first clean 3D design this crate has ever had. It matters beyond coverage: every
    // scenario below starts from zero, so a count of 1 afterwards is attributable to the move
    // that produced it rather than to something the fixture was born with.
    let db = clean_stack();
    assert_eq!(
        db.check_3dblox().unwrap(),
        0,
        "a well-formed stack must lint clean"
    );
    for c in [
        "3DBlox/Connection regions",
        "3DBlox/Floating chips",
        "3DBlox/Overlapping chips",
        "3DBlox/Logical Connectivity",
        "3DBlox/Unused internal_ext",
        "3DBlox/Bump Alignment",
        "3DBlox/Alignment Markers",
    ] {
        assert_eq!(
            markers(&db, c),
            0,
            "{c} should be clean on a well-formed stack"
        );
    }
}

#[test]
fn the_bonding_surfaces_meet_where_the_geometry_says_they_should() {
    // The assertion behind the fixture's design: both surfaces resolve to z=1500 and face each
    // other. Without this a "clean" verdict could just mean the checker found nothing to look at.
    let db = clean_stack();
    assert_eq!(db.unfoldedregion_get_surface_z("u_base", 0), 1500);
    assert_eq!(db.unfoldedregion_get_effective_side("u_base", 0), "TOP");
    assert_eq!(db.unfoldedregion_get_surface_z("u_upper", 0), 1500);
    assert_eq!(db.unfoldedregion_get_effective_side("u_upper", 0), "BOTTOM");
}

#[test]
fn upstreams_floating_chip_progression_reproduces() {
    // OpenROAD's own scenario, and the half that was previously out of reach. Their counts:
    // clean 0, vertical gap 1, a second disconnected set 2.
    let mut db = clean_stack();
    db.check_3dblox().unwrap();
    assert_eq!(
        markers(&db, "3DBlox/Floating chips"),
        0,
        "stacked and bonded: nothing floats"
    );

    db.place_chip_inst("stack", "u_upper", "R0", 0, 0, 11500)
        .unwrap();
    db.construct_unfolded_model().unwrap();
    db.check_3dblox().unwrap();
    assert_eq!(
        markers(&db, "3DBlox/Floating chips"),
        1,
        "a vertical gap strands the upper die"
    );
    assert_eq!(
        markers(&db, "3DBlox/Connection regions"),
        1,
        "and the bond's mating gap no longer matches its thickness"
    );

    db.create_chip_inst("stack", "upper", "u_third").unwrap();
    db.place_chip_inst("stack", "u_third", "R0", 900_000, 900_000, 0)
        .unwrap();
    db.construct_unfolded_model().unwrap();
    db.check_3dblox().unwrap();
    assert_eq!(
        markers(&db, "3DBlox/Floating chips"),
        2,
        "a third, unbonded chip is a second set"
    );
}

#[test]
fn upstreams_overlap_situation_reproduces_from_a_clean_start() {
    let mut db = clean_stack();
    db.check_3dblox().unwrap();
    assert_eq!(markers(&db, "3DBlox/Overlapping chips"), 0);

    // drive the upper die down into the substrate's z range and across it in x/y
    db.place_chip_inst("stack", "u_upper", "R0", 12000, 12000, 1000)
        .unwrap();
    db.construct_unfolded_model().unwrap();
    db.check_3dblox().unwrap();
    assert_eq!(
        markers(&db, "3DBlox/Overlapping chips"),
        1,
        "two dice cannot share a volume"
    );
}

#[test]
fn an_internal_ext_region_nobody_bonds_to_is_reported() {
    // A fourth check exercised. `INTERNAL_EXT` declares a surface meant to be connected; one
    // that no connection references is a modelling error, and until there was a design we could
    // build, this check had never been observed to fire at all.
    let mut db = clean_stack();
    db.create_chip_region("upper", "spare", "INTERNAL_EXT", "")
        .unwrap();
    db.set_chip_region_box("upper", "spare", 0, 0, 1000, 1000)
        .unwrap();
    // re-instantiate so the new region reaches an inst (see the ordering note above)
    db.create_chip_inst("stack", "upper", "u_spare").unwrap();
    db.place_chip_inst("stack", "u_spare", "R0", 0, 0, 1500)
        .unwrap();
    db.construct_unfolded_model().unwrap();
    db.check_3dblox().unwrap();
    assert!(
        markers(&db, "3DBlox/Unused internal_ext") > 0,
        "an INTERNAL_EXT region referenced by no connection must be reported"
    );
}

#[test]
fn a_rust_built_assembly_survives_a_write_and_reopen() {
    // Persistency was previously only ever verified for a C++-built fixture. If `write_db` did
    // not round-trip what these bindings create, every read below would come back empty.
    let dir = std::env::temp_dir().join("vyges-chip-create-roundtrip");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("stack.odb");
    clean_stack().write(&path).unwrap();

    let db = Db::open(&path).unwrap();
    assert_eq!(db.chip_get_name("stack"), "stack");
    assert_eq!(db.chip_get_chip_type("base"), "SUBSTRATE");
    assert_eq!(db.chipinst_get_loc_z("stack", "u_upper"), 1500);
    // the unfolded model is derived, never serialised — the reader rebuilds it
    assert_eq!(db.unfoldedregion_get_surface_z("u_upper", 0), 1500);
    assert_eq!(
        db.check_3dblox().unwrap(),
        0,
        "and it still lints clean after a round trip"
    );
    let _ = std::fs::remove_file(&path);
}
