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
