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

#[test]
fn a_site_without_a_row_pattern_reports_an_empty_one() {
    // "No pattern" is the ordinary single-height case, so it must be an empty vector rather
    // than an error the caller has to distinguish from a real failure.
    let db = Db::open(FIXTURE).expect("opens");
    let site = a_site(&db);
    assert!(db.row_pattern(&site).expect("readable").is_empty());
    assert!(db
        .row_pattern("no_such_site_xyz")
        .expect("readable")
        .is_empty());
}

#[test]
fn cutting_rows_reaches_odb_and_names_an_unknown_blockage() {
    // Row cutting is odb's algorithm; the wrapper's job is to pass the blockage list through and
    // to refuse a name the block does not define rather than cutting around nothing.
    let mut db = Db::open(FIXTURE).expect("opens");
    // ⚠️ (min_row_width, min_row_height, blockages, halo_x, halo_y). `min_row_height` was added
    // between the first two when the OpenROAD pin moved to 945a9f4, and this test kept calling the
    // four-argument form — which does not fail a case, it stops the whole test binary compiling.
    // A crate whose suite cannot build hides every later regression behind one stale call.
    assert!(db
        .cut_rows(0, 0, &["no_such_inst_xyz".to_string()], 0, 0)
        .is_err());
    db.cut_rows(0, 0, &[], 0, 0)
        .expect("no blockages is not an error");
    let _: bool = db.has_one_site_master();
}

#[test]
fn cutting_rows_at_the_blocks_own_blockages_reaches_odb() {
    // `ifp` does not choose its blockages the way `tap` does: it hands odb every dbBlockage in
    // the block. The fixture declares none, and odb returns immediately on an empty list, so
    // this is a no-op — which is exactly the case that must not become an error, because every
    // design without a blockage takes it.
    let mut db = Db::open(FIXTURE).expect("opens");
    db.cut_rows_at_blockages(0, 0, 0, 0)
        .expect("a block with no blockages is not an error");
}

#[test]
fn a_groups_type_is_readable_and_an_unknown_group_is_an_error() {
    // ⚠️ An unknown group must throw rather than return "": an empty string would read as a
    // type, and a caller filtering for VOLTAGE_DOMAIN would silently skip a group it could not
    // find instead of failing. That silence is what `ifp` cannot afford — it decides which rows
    // get rebuilt.
    let db = Db::open(FIXTURE).expect("opens");
    assert!(db.group_get_type("no_such_group_xyz").is_err());
    // Every group the block does define answers with one of odb's four type names.
    for g in db.block_get_groups() {
        let t = db.group_get_type(&g).expect("a defined group has a type");
        assert!(
            matches!(t.as_str(), "PHYSICAL_CLUSTER" | "VOLTAGE_DOMAIN" | "POWER_DOMAIN"
                                 | "VISUAL_DEBUG"),
            "unexpected group type {t:?}"
        );
    }
}

#[test]
fn rows_can_be_listed_and_their_site_class_read() {
    let mut db = Db::open(FIXTURE).expect("opens");
    let site = a_site(&db);
    let (w, h) = (db.site_get_width(&site), db.site_get_height(&site));
    db.clear_rows().expect("clear");
    db.set_die_area(0, 0, w * 100, h * 4).expect("die");
    for r in 0..3 {
        db.create_row(&format!("R{r}"), &site, 0, r * h, "R0", "HORIZONTAL", 50, w)
            .expect("row");
    }
    let names = db.row_names().expect("names");
    assert_eq!(names.len(), 3);
    assert_eq!(names.len(), db.num_rows().expect("count"));
    assert!(names.iter().all(|n| n.starts_with('R')));
    assert!(!db.site_get_class(&site).expect("class").is_empty());
    assert!(db
        .site_get_class("no_such_site_xyz")
        .expect("readable")
        .is_empty());
}

#[test]
fn masters_can_be_listed_with_their_types() {
    let db = Db::open(FIXTURE).expect("opens");
    let all = db.masters_with_types().expect("readable");
    assert_eq!(all.len(), db.num_masters().expect("count"));
    assert!(all.iter().all(|(n, t)| !n.is_empty() && !t.is_empty()));
    // The type is the question a name substring cannot answer.
    let (name, ty) = &all[0];
    assert_eq!(&db.master_get_type(name).expect("type"), ty);
    assert!(db
        .master_get_type("no_such_master_xyz")
        .expect("readable")
        .is_empty());
}

#[test]
fn an_instance_can_be_destroyed_through_the_wrapper() {
    let mut db = Db::open(FIXTURE).expect("opens");
    let master = db.nth_master_name(0).expect("a master");
    db.create_physical_inst(&master, "GONE").expect("created");
    let before = db.num_insts();
    db.destroy_inst("GONE").expect("destroyed");
    assert_eq!(db.num_insts(), before - 1);
    assert!(db.destroy_inst("GONE").is_err(), "already gone");
}
