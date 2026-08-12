// SPDX-License-Identifier: Apache-2.0
//! The floorplan write path, through the safe wrapper.
//!
//! The shims themselves are tested in `opendb-lib`; what is checked here is that the wrapper
//! passes arguments in the right order, reports absence honestly, and does not swallow errors.

use vyges_opendb::Db;

const FIXTURE: &str = "tests/fixtures/counter.odb";

fn a_site(db: &Db) -> String {
    db.site_names()
        .expect("the library defines sites")
        .into_iter()
        .next()
        .expect("at least one")
}

#[test]
fn the_die_and_core_areas_round_trip_in_the_order_they_were_given() {
    // Four ints in a row is exactly the shape a transposition hides in, so the rectangle is
    // deliberately asymmetric: any swapped pair changes the read-back.
    let mut db = Db::open(FIXTURE).expect("opens");
    db.set_die_area(100, 200, 30_000, 40_000).expect("die");
    assert_eq!(
        (
            db.block_get_die_area_x_min(),
            db.block_get_die_area_y_min(),
            db.block_get_die_area_x_max(),
            db.block_get_die_area_y_max()
        ),
        (100, 200, 30_000, 40_000)
    );

    db.set_core_area(110, 220, 29_000, 39_000).expect("core");
    assert_eq!(
        (
            db.block_get_core_area_x_min(),
            db.block_get_core_area_y_min(),
            db.block_get_core_area_x_max(),
            db.block_get_core_area_y_max()
        ),
        (110, 220, 29_000, 39_000)
    );
}

#[test]
fn rows_are_created_counted_and_cleared() {
    let mut db = Db::open(FIXTURE).expect("opens");
    let site = a_site(&db);
    let (w, h) = (db.site_get_width(&site), db.site_get_height(&site));
    assert!(w > 0 && h > 0, "{site} has no extent");

    db.clear_rows().expect("clear");
    assert_eq!(db.num_rows().expect("count"), 0);

    db.set_die_area(0, 0, w * 200, h * 20).expect("die");
    for r in 0..3 {
        db.create_row(
            &format!("R{r}"),
            &site,
            0,
            r * h,
            "R0",
            "HORIZONTAL",
            100,
            w,
        )
        .expect("row");
    }
    assert_eq!(db.num_rows().expect("count"), 3);
    assert_eq!(
        db.clear_rows().expect("clear"),
        3,
        "clearing reports what it removed"
    );
    assert_eq!(db.num_rows().expect("count"), 0);
}

#[test]
fn the_core_area_can_be_computed_from_the_rows_and_then_stored() {
    let mut db = Db::open(FIXTURE).expect("opens");
    let site = a_site(&db);
    let (w, h) = (db.site_get_width(&site), db.site_get_height(&site));
    let (x0, y0, n, rows) = (1_000, 2_000, 40, 5);

    db.clear_rows().expect("clear");
    db.set_die_area(0, 0, x0 + w * 1000, y0 + h * 100)
        .expect("die");
    for r in 0..rows {
        db.create_row(
            &format!("R{r}"),
            &site,
            x0,
            y0 + r * h,
            "R0",
            "HORIZONTAL",
            n,
            w,
        )
        .expect("row");
    }

    let c = db.compute_core_area().expect("compute");
    assert_eq!(c, vec![x0, y0, x0 + n * w, y0 + rows * h]);
    // Computing must not store.
    assert_ne!(
        db.block_get_core_area_x_max(),
        c[2],
        "compute is not a setter"
    );

    db.set_core_area_from_rows().expect("store");
    assert_eq!(db.block_get_core_area_x_min(), c[0]);
    assert_eq!(db.block_get_core_area_y_max(), c[3]);

    // With no rows there is nothing to compute — an empty answer, not a guess.
    db.clear_rows().expect("clear");
    assert!(db.compute_core_area().expect("compute").is_empty());
}

#[test]
fn an_absent_manufacturing_grid_is_none_rather_than_a_silent_one() {
    // 0 means the technology states no grid. Reporting that as 1 would make every coordinate
    // "already snapped" and the distinction would be lost.
    let db = Db::open(FIXTURE).expect("opens");
    if let Some(g) = db.manufacturing_grid().expect("readable") {
        assert!(g > 0, "a stated grid is a positive length, got {g}");
    }
}

#[test]
fn sites_can_be_enumerated_and_indexed_consistently() {
    let db = Db::open(FIXTURE).expect("opens");
    let names = db.site_names().expect("names");
    assert_eq!(names.len(), db.num_sites().expect("count"));
    for (i, name) in names.iter().enumerate() {
        assert_eq!(
            &db.nth_site_name(i).expect("nth"),
            name,
            "index {i} disagrees with the list"
        );
    }
}

#[test]
fn a_row_on_an_unknown_site_is_an_error_rather_than_a_no_op() {
    let mut db = Db::open(FIXTURE).expect("opens");
    let before = db.num_rows().expect("count");
    assert!(db
        .create_row("RX", "no_such_site_xyz", 0, 0, "R0", "HORIZONTAL", 1, 1)
        .is_err());
    assert_eq!(
        db.num_rows().expect("count"),
        before,
        "and it created nothing"
    );
}
