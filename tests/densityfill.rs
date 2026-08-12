// SPDX-License-Identifier: Apache-2.0
//! The density-fill substrate through the safe wrapper.
use vyges_opendb::Db;

const FIXTURE: &str = "tests/fixtures/counter.odb";

#[test]
fn instance_metal_and_obstructions_come_back_as_typed_boxes() {
    let db = Db::open(FIXTURE).expect("opens");
    let shapes = db.inst_shapes().expect("readable");
    assert!(!shapes.is_empty(), "a placed design has instance metal");
    for (layer, x0, y0, x1, y1) in &shapes {
        assert!(x1 >= x0 && y1 >= y0, "a shape box is not inverted");
        assert!(
            !db.layer_name_by_number(*layer).is_empty(),
            "the layer resolves"
        );
    }
    let _ = db.obstruction_boxes().expect("readable");
}

#[test]
fn fill_can_be_placed_counted_and_regenerated() {
    let mut db = Db::open(FIXTURE).expect("opens");
    assert_eq!(db.num_fills().expect("count"), 0);
    let layer = db.layer_name_by_number(db.inst_shapes().expect("shapes")[0].0);

    db.create_fill(false, 0, &layer, 0, 0, 100, 100)
        .expect("created");
    assert_eq!(db.num_fills().expect("count"), 1);
    // Fill is regenerated wholesale, never patched, so clearing has to be cheap and total.
    assert_eq!(db.clear_fills().expect("cleared"), 1);
    assert_eq!(db.num_fills().expect("count"), 0);
    assert!(db
        .create_fill(false, 0, "no_such_layer_xyz", 0, 0, 1, 1)
        .is_err());
}
