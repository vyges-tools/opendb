// SPDX-License-Identifier: Apache-2.0
//! The global-routing congestion grid (`dbGCellGrid`) — one per block, and `grt`'s second output
//! after the route guides.
//!
//! Shape mirrored from `FastRouteCore::updateDbCongestion`: get-or-create the grid, reset it,
//! re-add the X/Y patterns, then write capacity and usage per `(layer, x, y)`.

use vyges_opendb::Db;

const FIXTURE: &str = "tests/fixtures/counter.odb";

fn a_layer(db: &Db) -> String {
    db.layers_with_direction()
        .expect("the tech defines layers")
        .into_iter()
        .map(|(n, _)| n)
        .find(|l| db.layer_get_routing_level(l) > 0)
        .expect("a routing layer")
}

/// A block with a KNOWN grid: one 4-line X pattern and one 3-line Y pattern.
///
/// ⚠️ `counter.odb` already carries a gcell grid **with patterns of its own** — the second time
/// this fixture has turned out not to be blank (it also ships with 6 route guides). So this
/// resets the grid before adding ours; a test that merely *added* patterns would assert against
/// the fixture's incidental geometry and would break the day the fixture is regenerated.
fn with_grid() -> Db {
    let mut db = Db::open(FIXTURE).expect("opens");
    db.ensure_gcell_grid().expect("create");
    db.gcell_reset_grid().expect("known state");
    db.gcell_add_grid_pattern_x(0, 4, 1_000).expect("x pattern");
    db.gcell_add_grid_pattern_y(0, 3, 2_000).expect("y pattern");
    db
}

#[test]
fn ensure_is_get_or_create_and_never_replaces_an_existing_grid() {
    // ⚠️ The fixture ALREADY has a grid, so this cannot test the create path — it tests the half
    // that matters for `grt`, which calls the equivalent of `ensure` on every run and must not
    // wipe a grid a previous stage built.
    let mut db = Db::open(FIXTURE).expect("opens");
    assert!(db.has_gcell_grid(), "counter.odb ships with a gcell grid");

    db.ensure_gcell_grid().expect("first");
    db.gcell_reset_grid().expect("known state");
    db.gcell_add_grid_pattern_x(0, 4, 1_000).expect("x pattern");

    db.ensure_gcell_grid().expect("second call");
    assert_eq!(
        db.gcell_get_num_grid_patterns_x(),
        1,
        "ensure must be get-or-create; a second call must not reset the grid"
    );
}

// ⬜ UNWITNESSED: the null-grid path. Every `gcell_*` call errors when the block has no grid
// (`require_gcell` in shim.cc), but `counter.odb` always has one and odb offers no way to remove
// it — `resetGrid` drops the patterns, not the grid. So that branch has no case exercising it
// here, and it is recorded as absent rather than left to look covered.

#[test]
fn grid_patterns_round_trip_and_expand_to_lines() {
    let db = with_grid();
    assert_eq!(db.gcell_get_num_grid_patterns_x(), 1);
    assert_eq!(db.gcell_get_num_grid_patterns_y(), 1);
    // Deliberately different counts and steps per axis: a transposed x/y shows up here.
    assert_eq!(db.gcell_grid_pattern_x(0).expect("x"), (0, 4, 1_000));
    assert_eq!(db.gcell_grid_pattern_y(0).expect("y"), (0, 3, 2_000));

    // A pattern is a generator, not a list: `count` lines `step` apart from `origin`.
    assert_eq!(db.gcell_grid_x().expect("x lines"), vec![0, 1_000, 2_000, 3_000]);
    assert_eq!(db.gcell_grid_y().expect("y lines"), vec![0, 2_000, 4_000]);
}

#[test]
fn an_out_of_range_pattern_index_is_an_error() {
    let db = with_grid();
    assert!(db.gcell_grid_pattern_x(1).is_err());
    assert!(db.gcell_grid_pattern_y(1).is_err());
}

#[test]
fn a_dbu_coordinate_maps_to_the_gcell_that_contains_it() {
    let db = with_grid();
    // x lines at 0/1000/2000/3000 -> 1500 falls in column 1, 2500 in column 2.
    assert_eq!(db.gcell_x_idx(1_500).expect("x idx"), 1);
    assert_eq!(db.gcell_x_idx(2_500).expect("x idx"), 2);
    // y lines at 0/2000/4000 -> 3000 falls in row 1.
    assert_eq!(db.gcell_y_idx(3_000).expect("y idx"), 1);
}

#[test]
fn capacity_and_usage_round_trip_per_cell_and_are_independent() {
    let mut db = with_grid();
    let layer = a_layer(&db);

    db.gcell_set_capacity(&layer, 1, 2, 12.0).expect("cap");
    db.gcell_set_usage(&layer, 1, 2, 7.0).expect("use");

    assert_eq!(db.gcell_capacity(&layer, 1, 2).expect("cap"), 12.0);
    assert_eq!(db.gcell_usage(&layer, 1, 2).expect("use"), 7.0);
    // (1,2) and (2,1) are different cells — the index order is not symmetric.
    assert_eq!(db.gcell_capacity(&layer, 2, 1).expect("cap"), 0.0);
}

#[test]
fn reset_grid_and_reset_congestion_map_are_NOT_the_same_call() {
    // ⛔ The trap. `resetGrid` drops the grid PATTERNS as well as the congestion data;
    // `resetCongestionMap` keeps the patterns and zeroes only usage/capacity. `grt` calls
    // resetGrid and then re-adds its patterns immediately — code that called the other one would
    // keep a stale grid geometry and only look wrong much later, in the guides.
    let mut db = with_grid();
    let layer = a_layer(&db);
    db.gcell_set_capacity(&layer, 1, 1, 9.0).expect("cap");

    db.gcell_reset_congestion_map().expect("reset map");
    assert_eq!(db.gcell_get_num_grid_patterns_x(), 1, "the patterns must survive");
    assert_eq!(db.gcell_capacity(&layer, 1, 1).expect("cap"), 0.0, "the data must not");

    db.gcell_reset_grid().expect("reset grid");
    assert_eq!(db.gcell_get_num_grid_patterns_x(), 0, "resetGrid drops the patterns too");
    assert_eq!(db.gcell_get_num_grid_patterns_y(), 0);
}

#[test]
fn the_congestion_map_flattens_to_one_row_per_gcell_agreeing_with_per_cell_reads() {
    // 🔑 The bulk read exists so a correlation gate does not make nx*ny FFI calls per layer.
    // It is only worth having if it agrees with the per-cell path exactly.
    let mut db = with_grid();
    let layer = a_layer(&db);
    db.gcell_set_capacity(&layer, 1, 2, 12.0).expect("cap");
    db.gcell_set_usage(&layer, 1, 2, 7.0).expect("use");
    db.gcell_set_capacity(&layer, 3, 0, 4.0).expect("cap");

    let rows = db.gcell_layer_congestion(&layer).expect("map");
    assert!(!rows.is_empty(), "a grid with patterns must yield cells");

    for r in &rows {
        assert_eq!(
            db.gcell_capacity(&layer, r.x_idx, r.y_idx).expect("cap"),
            r.capacity,
            "row ({}, {}) capacity disagrees with the per-cell read",
            r.x_idx, r.y_idx
        );
        assert_eq!(
            db.gcell_usage(&layer, r.x_idx, r.y_idx).expect("use"),
            r.usage,
            "row ({}, {}) usage disagrees with the per-cell read",
            r.x_idx, r.y_idx
        );
    }

    // the two cells written above must be present with their values
    let hit = rows.iter().find(|r| r.x_idx == 1 && r.y_idx == 2).expect("cell (1,2)");
    assert_eq!((hit.capacity, hit.usage), (12.0, 7.0));
    let hit2 = rows.iter().find(|r| r.x_idx == 3 && r.y_idx == 0).expect("cell (3,0)");
    assert_eq!(hit2.capacity, 4.0);
}

#[test]
fn an_unknown_layer_is_an_error_on_every_path() {
    let mut db = with_grid();
    assert!(db.gcell_capacity("no_such_layer", 0, 0).is_err());
    assert!(db.gcell_usage("no_such_layer", 0, 0).is_err());
    assert!(db.gcell_layer_congestion("no_such_layer").is_err());
    assert!(db.gcell_set_capacity("no_such_layer", 0, 0, 1.0).is_err());
}

#[test]
fn a_block_bool_property_distinguishes_absent_from_false() {
    // ⛔ The reason this is Option<bool> and not bool. `grt` stamps `use_cugr` on the block in
    // saveGuides so a reader can tell which of the two routers produced the guides. A design
    // routed before that property existed has it ABSENT — and that must not read as "FastRoute",
    // which is what a plain `bool` with a false default would have said.
    let mut db = Db::open(FIXTURE).expect("opens");
    assert_eq!(db.block_bool_property("vyges_test_absent").expect("read"), None);

    db.block_set_bool_property("vyges_test_flag", false).expect("create false");
    assert_eq!(db.block_bool_property("vyges_test_flag").expect("read"), Some(false));

    // set-or-create: a second write updates in place rather than duplicating
    db.block_set_bool_property("vyges_test_flag", true).expect("update");
    assert_eq!(db.block_bool_property("vyges_test_flag").expect("read"), Some(true));
}
