// SPDX-License-Identifier: Apache-2.0
// The ODB 3D / chiplet schema read against POPULATED data with exact values — not discovery or
// graceful-empty. The fixture is synthesized by opendb-lib's test/make-3d-fixture.cpp
// (-DVYGES_ODB_MK3DFIXTURE=ON) because our safe API does not expose structural creation and we
// cannot read a .3dbv/.3dbx yet.
//
// The fixture, and what it is for:
//
//   stack : dbChip HIER, the database's top chip
//     |- u_top  : dbChipInst -> top_die  (DIE)       loc (1000, 2000, 3000)  orient MZ_R90
//     |- u_base : dbChipInst -> base_die (SUBSTRATE) loc (0, 0, 0)           orient R0
//     |- bond0  : dbChipConn  u_top/front <-> u_base/back, thickness 25
//     |- vdd_3d : dbChipNet   ·   path0 : dbChipPath
//
//   top_die  : block "top_die_blk" w/ inst "bump_pad0"; region "front" (FRONT) w/ one dbChipBump
//   base_die : region "back" (BACK)
//
// Region insts, bump insts and the entire UNFOLDED model are derived rather than stored — the
// first two by dbChipInst::create, the last by constructUnfoldedModel(), which runs on read.
use vyges_opendb::{registry, Db};

const FIXTURE: &str = "tests/fixtures/chiplet3d.odb";

fn chip(db: &Db, name: &str, field: &str) -> serde_json::Value {
    registry::get(db, "dbChip", field, &[name.into()]).unwrap()
}

fn inst(db: &Db, name: &str, field: &str) -> serde_json::Value {
    registry::get(db, "dbChipInst", field, &["stack".into(), name.into()]).unwrap()
}

fn get(db: &Db, class: &str, field: &str, keys: &[&str]) -> serde_json::Value {
    let keys: Vec<String> = keys.iter().map(|s| s.to_string()).collect();
    registry::get(db, class, field, &keys).unwrap()
}

#[test]
fn chips_persist_through_a_write_read_round_trip() {
    // The fixture was written with db->write() and is read back here with a fresh db. If the 3D
    // schema did not survive write_db/read_db, every read below would come back empty — so this
    // independently confirms the persistency claim for dbChip*, not just our marshalling.
    let db = Db::open(FIXTURE).unwrap();
    assert_eq!(chip(&db, "stack", "get_name"), serde_json::json!("stack"));
    assert_eq!(chip(&db, "top_die", "get_name"), serde_json::json!("top_die"));
    assert_eq!(chip(&db, "base_die", "get_name"), serde_json::json!("base_die"));
}

#[test]
fn chip_type_discriminates_die_from_substrate_from_hier() {
    // odb ships no getString() for dbChip::ChipType, so the generator emits the mapping. Three
    // DIFFERENT types are asserted deliberately: a fixture where every chip was a DIE could not
    // tell a working mapping from one that returns a constant.
    let db = Db::open(FIXTURE).unwrap();
    assert_eq!(chip(&db, "stack", "get_chip_type"), serde_json::json!("HIER"));
    assert_eq!(chip(&db, "top_die", "get_chip_type"), serde_json::json!("DIE"));
    assert_eq!(chip(&db, "base_die", "get_chip_type"), serde_json::json!("SUBSTRATE"));
}

#[test]
fn per_chip_stackup_is_readable() {
    // Per-chip geometry is what lets dies of different processes coexist in one database; the
    // thickness in particular is the Z extent the stack is built from.
    let db = Db::open(FIXTURE).unwrap();
    assert_eq!(chip(&db, "top_die", "get_width"), serde_json::json!(50000));
    assert_eq!(chip(&db, "top_die", "get_height"), serde_json::json!(40000));
    assert_eq!(chip(&db, "top_die", "get_thickness"), serde_json::json!(700));
    assert_eq!(chip(&db, "top_die", "is_tsv"), serde_json::json!(true));

    assert_eq!(chip(&db, "base_die", "get_thickness"), serde_json::json!(1500));
    assert_eq!(chip(&db, "base_die", "is_tsv"), serde_json::json!(false));
}

#[test]
fn chip_inst_placement_reads_x_y_and_z() {
    // Point3D expanded into three scalar sub-fields. The Z is the point of the whole exercise:
    // it is what a 2D dbInst location cannot express.
    let db = Db::open(FIXTURE).unwrap();
    assert_eq!(inst(&db, "u_top", "get_loc_x"), serde_json::json!(1000));
    assert_eq!(inst(&db, "u_top", "get_loc_y"), serde_json::json!(2000));
    assert_eq!(inst(&db, "u_top", "get_loc_z"), serde_json::json!(3000));

    assert_eq!(inst(&db, "u_base", "get_loc_z"), serde_json::json!(0));
}

#[test]
fn chip_inst_orientation_includes_the_z_mirror() {
    // dbOrientType3D is a 2D orientation plus a mirror-in-Z flag, and getString() encodes both.
    // u_top is rotated AND flipped — the "MZ_" prefix is exactly the part a 2D dbOrientType
    // cannot represent, so a binding that quietly dropped to the 2D type would fail here.
    let db = Db::open(FIXTURE).unwrap();
    assert_eq!(inst(&db, "u_top", "get_orient"), serde_json::json!("MZ_R90"));
    assert_eq!(inst(&db, "u_base", "get_orient"), serde_json::json!("R0"));
}

#[test]
fn chip_inst_resolves_its_master_and_parent() {
    // The relation accessors marshal a dbChip* to its name, which is what makes the stack
    // walkable: parent chip -> inst -> master chip, and the master is addressable in turn.
    let db = Db::open(FIXTURE).unwrap();
    assert_eq!(inst(&db, "u_top", "get_master_chip"), serde_json::json!("top_die"));
    assert_eq!(inst(&db, "u_top", "get_parent_chip"), serde_json::json!("stack"));
    assert_eq!(inst(&db, "u_base", "get_master_chip"), serde_json::json!("base_die"));

    // and the name reached through the relation is itself a valid key
    let master = inst(&db, "u_base", "get_master_chip");
    let master = master.as_str().unwrap();
    assert_eq!(chip(&db, master, "get_chip_type"), serde_json::json!("SUBSTRATE"));
}

#[test]
fn a_chip_inst_is_keyed_by_its_parent_not_globally() {
    // dbDatabase::getChipInsts() is flat and inst names are unique only within their parent
    // chip, which is why the key is (parent chip, inst). Looking u_top up under the wrong
    // parent must miss rather than silently resolve.
    let db = Db::open(FIXTURE).unwrap();
    let wrong = registry::get(&db, "dbChipInst", "get_loc_z", &["top_die".into(), "u_top".into()])
        .unwrap();
    assert_eq!(wrong, serde_json::json!(0));
    let right = inst(&db, "u_top", "get_loc_z");
    assert_eq!(right, serde_json::json!(3000));
}

// ---- bonding surfaces, bumps, connections -----------------------------------------------------

#[test]
fn chip_regions_carry_a_side_and_a_box() {
    // dbChipRegion::Side is the second enum with no getString(), so the generator maps it too.
    // The two regions carry DIFFERENT sides on purpose — see the ChipType note above.
    let db = Db::open(FIXTURE).unwrap();
    assert_eq!(get(&db, "dbChipRegion", "get_side", &["top_die", "front"]), serde_json::json!("FRONT"));
    assert_eq!(get(&db, "dbChipRegion", "get_side", &["base_die", "back"]), serde_json::json!("BACK"));

    assert_eq!(get(&db, "dbChipRegion", "get_box_x_max", &["top_die", "front"]), serde_json::json!(50000));
    assert_eq!(get(&db, "dbChipRegion", "get_chip", &["top_die", "front"]), serde_json::json!("top_die"));
    // a region names the layer it bonds on, which is how it ties back to the die's own tech
    assert_eq!(get(&db, "dbChipRegion", "get_layer", &["top_die", "front"]), serde_json::json!("nwell"));
}

#[test]
fn region_insts_are_derived_per_chip_inst() {
    // dbChipInst::create builds a dbChipRegionInst for every region on the master chip — we never
    // create these directly. Keyed by (chip, inst, region): the region name comes from the MASTER,
    // the inst from the parent.
    let db = Db::open(FIXTURE).unwrap();
    assert_eq!(
        get(&db, "dbChipRegionInst", "get_chip_inst", &["stack", "u_top", "front"]),
        serde_json::json!("u_top")
    );
    assert_eq!(
        get(&db, "dbChipRegionInst", "get_chip_region", &["stack", "u_top", "front"]),
        serde_json::json!("front")
    );
}

#[test]
fn a_bump_ties_a_region_back_to_the_dies_netlist() {
    // This is what dbChipBump is for: the bump wraps a placed dbInst in the die's own block, so a
    // bonding surface can be resolved to the design underneath it. Addressed by position, since
    // a bump has neither a name nor a find*.
    let db = Db::open(FIXTURE).unwrap();
    assert_eq!(get(&db, "dbChipBump", "get_inst", &["top_die", "front", "0"]), serde_json::json!("bump_pad0"));
    assert_eq!(get(&db, "dbChipBump", "get_chip", &["top_die", "front", "0"]), serde_json::json!("top_die"));
    assert_eq!(get(&db, "dbChipBump", "get_chip_region", &["top_die", "front", "0"]), serde_json::json!("front"));
    // unassigned net/bterm come back blank rather than erroring
    assert_eq!(get(&db, "dbChipBump", "get_net", &["top_die", "front", "0"]), serde_json::json!(""));
}

#[test]
fn a_connection_carries_thickness_and_its_region_paths() {
    // dbChipConn is the physical bond between two regions. Thickness is what the linter checks
    // the mating-surface gap against, and the region paths are how a conn addresses regions that
    // may sit several chip-insts deep.
    let db = Db::open(FIXTURE).unwrap();
    assert_eq!(get(&db, "dbChipConn", "get_name", &["stack", "bond0"]), serde_json::json!("bond0"));
    assert_eq!(get(&db, "dbChipConn", "get_thickness", &["stack", "bond0"]), serde_json::json!(25));
    assert_eq!(get(&db, "dbChipConn", "get_parent_chip", &["stack", "bond0"]), serde_json::json!("stack"));
    // std::vector<dbChipInst*> marshals as a list of names, like a dbSet
    assert_eq!(
        get(&db, "dbChipConn", "get_top_region_path", &["stack", "bond0"]),
        serde_json::json!(["u_top"])
    );
}

#[test]
fn chip_nets_and_paths_are_addressable() {
    let db = Db::open(FIXTURE).unwrap();
    assert_eq!(get(&db, "dbChipNet", "get_name", &["stack", "vdd_3d"]), serde_json::json!("vdd_3d"));
    assert_eq!(get(&db, "dbChipNet", "get_chip", &["stack", "vdd_3d"]), serde_json::json!("stack"));
    assert_eq!(get(&db, "dbChipPath", "get_name", &["stack", "path0"]), serde_json::json!("path0"));
}

// ---- the unfolded model -----------------------------------------------------------------------

#[test]
fn the_unfolded_model_is_rebuilt_on_read() {
    // The unfolded tables are DERIVED, never serialised — _dbDatabase::operator>> calls
    // constructUnfoldedModel() on read whenever the database holds more than one chip. So these
    // answer from a plain Db::open with nothing else called. A top-level inst unfolds to a path
    // that is just its own name.
    let db = Db::open(FIXTURE).unwrap();
    assert_eq!(get(&db, "dbUnfoldedChipInst", "get_name", &["u_top"]), serde_json::json!("u_top"));
    assert_eq!(
        get(&db, "dbUnfoldedChipInst", "get_chip_inst_path", &["u_top"]),
        serde_json::json!(["u_top"])
    );
    // and the flat db-level sets map back to the folded, named objects
    assert_eq!(get(&db, "dbUnfoldedChipConn", "get_chip_conn", &["0"]), serde_json::json!("bond0"));
    assert_eq!(get(&db, "dbUnfoldedChipNet", "get_chip_net", &["0"]), serde_json::json!("vdd_3d"));
}

#[test]
fn effective_side_is_computed_not_copied() {
    // The strongest evidence the unfolded model actually resolves geometry rather than copying
    // it: top_die's region is declared FRONT on the master, but u_top is mirrored in Z (MZ_R90),
    // so its EFFECTIVE side in the assembled stack is BOTTOM. A binding that merely echoed the
    // folded value would report FRONT here.
    let db = Db::open(FIXTURE).unwrap();
    assert_eq!(get(&db, "dbChipRegion", "get_side", &["top_die", "front"]), serde_json::json!("FRONT"));
    assert_eq!(
        get(&db, "dbUnfoldedChipRegionInst", "get_effective_side", &["u_top", "0"]),
        serde_json::json!("BOTTOM")
    );
    assert_eq!(get(&db, "dbUnfoldedChipRegionInst", "is_bottom", &["u_top", "0"]), serde_json::json!(true));
    assert_eq!(get(&db, "dbUnfoldedChipRegionInst", "is_top", &["u_top", "0"]), serde_json::json!(false));

    // the bonding surface's absolute Z in the assembled stack
    assert_eq!(get(&db, "dbUnfoldedChipRegionInst", "get_surface_z", &["u_top", "0"]), serde_json::json!(3000));
    assert_eq!(
        get(&db, "dbUnfoldedChipRegionInst", "get_parent_chip", &["u_top", "0"]),
        serde_json::json!("u_top")
    );
}

#[test]
fn bumps_resolve_to_absolute_positions() {
    // getGlobalPosition() is the payoff of the whole unfolded model — a bump's coordinates in the
    // assembled stack, with the parent chip inst's rotation and Z offset already applied. The X
    // here is NOT the bump's position in top_die's own frame; it has been rotated through R90.
    let db = Db::open(FIXTURE).unwrap();
    assert_eq!(
        get(&db, "dbUnfoldedChipBumpInst", "get_global_position_x", &["u_top", "0", "0"]),
        serde_json::json!(39640)
    );
    assert_eq!(
        get(&db, "dbUnfoldedChipBumpInst", "get_global_position_y", &["u_top", "0", "0"]),
        serde_json::json!(3840)
    );
    // Z lands on the parent's bonding surface
    assert_eq!(
        get(&db, "dbUnfoldedChipBumpInst", "get_global_position_z", &["u_top", "0", "0"]),
        serde_json::json!(3000)
    );
}
