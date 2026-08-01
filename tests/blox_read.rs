// SPDX-License-Identifier: Apache-2.0
//! Reading 3Dblox interchange files — against **upstream's own example**, not one of ours.
//!
//! `tests/fixtures/3dblox/example.{3dbx,3dbv}` are copied verbatim from OpenROAD's
//! `src/odb/test/data/` (BSD-3-Clause, The OpenROAD Authors). That is the point: every 3D test
//! before this one ran on a design we built, so it could only ever show that we agree with
//! ourselves. This is the first time the stack reads a file written by someone else.
//!
//! Phase 1 is geometry only — no `APR_tech_file`, `LEF_file`, `DEF_file`, `liberty_file`,
//! `verilog_file` or `bmap`. What cannot be expressed is **reported**, never silently dropped;
//! that is what the returned list is for.
#![cfg(feature = "gen-write")]
use vyges_opendb::{blox, registry, Db};

const DBX: &str = "tests/fixtures/3dblox/example.3dbx";

fn markers(db: &Db, category: &str) -> i64 {
    registry::get(
        db,
        "dbMarkerCategory",
        "get_marker_count",
        &[category.to_string()],
    )
    .unwrap()
    .as_i64()
    .unwrap()
}

#[test]
fn upstreams_own_assembly_parses() {
    let a = blox::read_assembly(DBX).expect("upstream's example must parse");
    assert_eq!(a.dbx.design_name, "TopDesign");
    assert_eq!(a.dbx.insts.len(), 2);
    assert_eq!(a.dbx.connections.len(), 2);

    // the Stack entry is joined onto the ChipletInst — neither is complete alone
    let dup = a
        .dbx
        .insts
        .iter()
        .find(|i| i.name == "soc_inst_duplicate")
        .unwrap();
    assert_eq!(dup.reference, "SoC");
    assert_eq!(dup.placement.z, 300.0);
    assert_eq!(dup.placement.orient, "MZ");
    assert_eq!(dup.placement.loc, (100.0, 200.0));
}

#[test]
fn the_included_definition_file_is_found_and_read() {
    // `Header.include` is resolved relative to the *including* file, not the working directory.
    // Get that wrong and the assembly parses while referencing chiplets that do not exist.
    let a = blox::read_assembly(DBX).unwrap();
    let soc = a
        .defs
        .get("SoC")
        .expect("the .3dbv is pulled in by the .3dbx");
    assert_eq!(
        soc.chip_type, "die",
        "lowercase in the file; uppercase in the database API"
    );
    assert_eq!(soc.design_area, Some((955.0, 1082.0)));
    assert_eq!(soc.thickness, Some(300.0));
    assert_eq!(soc.regions.len(), 2);
    assert!(
        soc.regions.iter().all(|r| r.is_rectangular()),
        "this example's outlines are rects"
    );
}

#[test]
fn a_virtual_bond_is_read_as_absent_rather_than_missing() {
    // `bot: ~` is a deliberate modelling choice — a connection with no counterpart — and reading
    // it as "field omitted" would erase the distinction.
    let a = blox::read_assembly(DBX).unwrap();
    let v = a
        .dbx
        .connections
        .iter()
        .find(|c| c.name == "soc_to_virtual")
        .unwrap();
    assert!(v.bot.is_none(), "the virtual bond has no bottom region");
    assert_eq!(v.top.region, "back_reg");
    let real = a
        .dbx
        .connections
        .iter()
        .find(|c| c.name == "soc_to_soc")
        .unwrap();
    assert!(real.bot.is_some(), "and an ordinary bond does");
}

#[test]
fn upstreams_assembly_loads_into_a_database_and_lints_clean() {
    // Their own scenario begins by asserting this design is clean before it starts moving chips.
    // Reaching the same verdict from an independent reader is the strongest signal available
    // that the geometry was understood rather than merely accepted.
    let mut db = Db::new();
    let lossy = db.read_3dblox(DBX).expect("upstream's example must load");
    assert_eq!(
        db.check_3dblox().unwrap(),
        0,
        "a clean assembly must lint clean"
    );
    for c in [
        "3DBlox/Floating chips",
        "3DBlox/Overlapping chips",
        "3DBlox/Connection regions",
    ] {
        assert_eq!(markers(&db, c), 0, "{c}");
    }
    // and the one thing Phase 1 cannot express is named, not dropped
    assert_eq!(lossy.len(), 1);
    assert!(lossy[0].contains("soc_to_virtual"), "got: {lossy:?}");
}

#[test]
fn the_geometry_survives_the_micron_to_dbu_conversion() {
    // Coordinates in the file are microns; the database stores DBU. At the example's precision
    // of 2000, a 300 um stack height is 600000 DBU — an off-by-one-thousand here is exactly the
    // class of error that makes a design look fine and lint wrong.
    let mut db = Db::new();
    db.read_3dblox(DBX).unwrap();
    assert_eq!(
        db.dbu_per_micron(),
        2000,
        "the header's precision is adopted"
    );
    assert_eq!(
        db.chipinst_get_loc_z("TopDesign", "soc_inst_duplicate"),
        300 * 2000
    );
    assert_eq!(db.chip_get_thickness("SoC"), 300 * 2000);
    assert_eq!(db.chip_get_width("SoC"), 955 * 2000);
}

#[test]
fn an_unvalidated_format_version_is_refused_rather_than_parsed() {
    // The failure this guards is silent: a format that moved under us would otherwise be read
    // with the old meaning and no complaint.
    let raw = std::fs::read_to_string(DBX)
        .unwrap()
        .replace("version: \"1.0\"", "version: \"9.9\"");
    let e = blox::parse_dbx(DBX, &raw).expect_err("an unknown version must be refused");
    let msg = e.to_string();
    assert!(
        msg.contains("9.9") && msg.contains("has not been validated"),
        "got: {msg}"
    );
    assert!(
        msg.contains("Header.version"),
        "the error must locate itself: {msg}"
    );
}

#[test]
fn an_incompatible_precision_is_refused_rather_than_silently_rescaling() {
    // Loading into a database that already has a coarser precision would rescale every
    // coordinate. odb applies the same rule; so do we, and we say why.
    let mut db = Db::open("tests/fixtures/counter.odb").unwrap();
    assert_eq!(db.dbu_per_micron(), 1000);
    let e = db.read_3dblox(DBX).expect_err("2000 does not divide 1000");
    assert!(e.to_string().contains("precision"), "got: {e}");
}

#[test]
fn a_reference_to_an_undefined_chiplet_is_an_error_with_the_name_in_it() {
    // Distinguishes a typo from an include we failed to read — only the reader can tell them
    // apart, and the caller cannot act on "parse failed".
    let raw = std::fs::read_to_string(DBX)
        .unwrap()
        .replace("reference: SoC", "reference: NoSuchDie");
    let tmp = std::env::temp_dir().join("blox-bad-ref.3dbx");
    std::fs::copy(
        "tests/fixtures/3dblox/example.3dbv",
        std::env::temp_dir().join("example.3dbv"),
    )
    .unwrap();
    std::fs::write(&tmp, raw).unwrap();
    let e = blox::read_assembly(tmp.to_str().unwrap())
        .expect_err("must not accept a dangling reference");
    assert!(e.to_string().contains("NoSuchDie"), "got: {e}");
    let _ = std::fs::remove_file(&tmp);
}

// ---- Phase 2: the technology a chiplet brings with it -------------------------------------------

const WITHTECH: &str = "tests/fixtures/3dblox/withtech.3dbx";

#[test]
fn a_chiplets_apr_tech_file_is_globbed_and_read() {
    // `APR_tech_file: [TECH_DIR/*_tech.lef]` exercises three things at once that are separately
    // easy to get wrong: the `#!define` is expanded, the path resolves against the .3dbv that
    // named it, and the `*` is a real pattern rather than a literal.
    let a = blox::read_assembly(WITHTECH).unwrap();
    let die = a.defs.get("Die").unwrap();
    assert_eq!(die.apr_tech_files.len(), 1);
    let p = &die.apr_tech_files[0];
    assert!(
        p.ends_with("*_tech.lef"),
        "the pattern survives parsing: {p}"
    );
    assert!(!p.contains("TECH_DIR"), "the macro must be expanded: {p}");
}

#[test]
fn the_technology_comes_from_the_lef_rather_than_a_placeholder() {
    // The difference Phase 2 exists to make. Before this, every chip shared a placeholder tech
    // with no layers; now the precision and the layers come from the chiplet's own LEF.
    let mut db = Db::new();
    db.read_3dblox(WITHTECH).expect("must load");
    assert_eq!(
        db.dbu_per_micron(),
        2000,
        "precision from the LEF's UNITS block"
    );
    assert_eq!(
        db.check_3dblox().unwrap(),
        0,
        "and the stack is still well-formed"
    );
}

#[test]
fn geometry_from_a_lef_backed_read_converts_correctly() {
    let mut db = Db::new();
    db.read_3dblox(WITHTECH).unwrap();
    assert_eq!(db.chip_get_thickness("Die"), 50 * 2000);
    assert_eq!(db.chip_get_width("Die"), 100 * 2000);
    // d1 is mirrored and sits directly on d0
    assert_eq!(db.chipinst_get_loc_z("WithTech", "d1"), 50 * 2000);
}

#[test]
fn a_tech_file_that_matches_nothing_is_reported_not_ignored() {
    // A glob that resolves to no file is a broken reference, and silence about it is how a
    // design gets timed against a technology nobody noticed was absent.
    let raw = std::fs::read_to_string("tests/fixtures/3dblox/withtech.3dbv")
        .unwrap()
        .replace("*_tech.lef", "*_nonexistent.lef");
    let dir = std::env::temp_dir().join("blox-notech");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("withtech.3dbv"), raw).unwrap();
    std::fs::copy(WITHTECH, dir.join("withtech.3dbx")).unwrap();

    let mut db = Db::new();
    // still loads — a missing tech file must not lose the geometry — but falls back visibly
    db.read_3dblox(dir.join("withtech.3dbx").to_str().unwrap())
        .unwrap();
    assert_eq!(db.check_3dblox().unwrap(), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

// ── bmap: the leg of the format the reader used to skip ─────────────────────────────────────

#[test]
fn a_regions_bump_map_is_read_and_resolved_against_the_definition_file() {
    // `bmap:` was previously ignored along with the rest of the external collateral. It is the
    // one piece the die-to-die check needs, and a path that resolves against the wrong directory
    // fails at open time rather than at parse time — much further from the cause.
    let a = blox::read_assembly(DBX).unwrap();
    let soc = a.defs.get("SoC").unwrap();
    let back = soc.regions.iter().find(|r| r.name == "back_reg").unwrap();
    let bmap = back.bmap.as_deref().expect("back_reg declares a bmap");
    assert!(bmap.ends_with("example.bmap"), "{bmap}");
    assert!(
        bmap.contains("fixtures/3dblox"),
        "resolved against the .3dbv that named it, not the cwd: {bmap}"
    );

    // A region that declares none must stay None rather than becoming an empty path that would
    // then fail to open.
    let front = soc.regions.iter().find(|r| r.name == "front_reg").unwrap();
    assert_eq!(front.bmap, None);
}

#[test]
fn an_assemblys_bonded_pairs_resolve_to_maps_and_placements() {
    // What `check-d2d --input` walks: which surfaces mate, where their bump maps are, and how
    // each die is placed — all from the file, so none of it has to be asserted by a caller.
    let pairs = blox::bonded_pairs("tests/fixtures/3dblox/d2d/stack.3dbx").unwrap();
    assert_eq!(pairs.len(), 1);
    let p = &pairs[0];
    assert_eq!(p.connection, "d2d_bond");
    assert_eq!(p.top.inst, "u_mem");
    assert_eq!(p.top.orient, "MZ_MY");
    assert_eq!(p.bottom.orient, "R0");
    assert_eq!(p.top.design_area, Some((200.0, 200.0)));
    assert!(p.top.bmap.as_deref().unwrap().ends_with("mem_front.bmap"));
    assert!(p.bottom.bmap.as_deref().unwrap().ends_with("logic_front.bmap"));
}

#[test]
fn a_virtual_bond_is_not_offered_as_a_pair_to_check() {
    // `bot: ~` has no second surface. Reporting it as a defective interface would be wrong —
    // it is deliberately virtual.
    let pairs = blox::bonded_pairs(DBX).unwrap();
    assert!(
        pairs.iter().all(|p| p.connection != "soc_to_virtual"),
        "the virtual bond must be skipped, not checked"
    );
    assert_eq!(pairs.len(), 1, "only the real bond remains");
}
