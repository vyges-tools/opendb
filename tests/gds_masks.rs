// SPDX-License-Identifier: Apache-2.0
//! GDS stream-out of a multi-patterned design: each shape's mask picks its datatype through the
//! layer map's `<purpose>:MASK:<n>` entries, and a pin is labelled once per layer and mask.
//!
//! 🔑 The oracle is KLayout 0.30.4's DEF reader on the SAME fixture and map
//! (`tests/fixtures/masks/`), read with the map file and written as GDS; its shapes and labels are
//! listed below as KLayout reported them. KLayout also warned once — "No mapping for layer 'V1',
//! purpose 'VIA' Mask 2 - layer is ignored" — for the via whose DEF cut mask has no map entry.
#![cfg(unix)]

use std::collections::BTreeSet;
use vyges_opendb::gds::{self, GdsElement, LayerMap};
use vyges_opendb::Db;

fn fixture(f: &str) -> String {
    format!("{}/tests/fixtures/masks/{f}", env!("CARGO_MANIFEST_DIR"))
}

fn tmp(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("vyges_opendb_gdsmasks_{}_{name}", std::process::id()))
}

/// KLayout 0.30.4, `fx.def` + `fx.map` (= this fixture), shapes as `layer/datatype x0 y0 x1 y1`.
const KLAYOUT_SHAPES: &[(i16, i16, [i32; 4])] = &[
    (100, 0, [0, 0, 2000, 2000]),
    (10, 0, [25, 225, 975, 275]),
    (10, 0, [900, 1800, 1100, 1850]),
    (10, 1, [25, 25, 975, 75]),
    (10, 1, [900, 125, 1000, 175]),
    (10, 2, [0, 1925, 2000, 1975]),
    (10, 2, [25, 125, 975, 175]),
    (20, 0, [925, 100, 975, 200]),
    (20, 1, [1025, 1800, 1075, 1900]),
    (20, 1, [925, 125, 975, 1075]),
    (20, 2, [925, 1800, 975, 1900]),
];

/// KLayout's labels: one on M1 (its pin box has no mask) and one per mask on M2.
const KLAYOUT_LABELS: &[(i16, i16, &str, (i32, i32))] =
    &[(10, 10, "p", (1000, 1825)), (20, 10, "p", (950, 1850)), (20, 10, "p", (1050, 1850))];

#[test]
fn masked_shapes_stream_to_their_masks_datatypes_as_klayout_does() {
    let mut db = Db::new();
    db.read_lef(fixture("masks.lef")).unwrap();
    db.read_def(fixture("masks.def"), "default").unwrap();
    // The design has no cells; the stream-out still wants a cell library for its units.
    let cells = tmp("cells.gds");
    let lib = db.gds_create_lib("cells").unwrap();
    db.gds_set_units(lib, 1e-3, 1e-9).unwrap();
    db.gds_write(lib, &cells).unwrap();

    let map = LayerMap::parse(&std::fs::read_to_string(fixture("masks.map")).unwrap()).unwrap();
    let out = tmp("masks.gds");
    let so = gds::write_gds(&mut db, &map, &[&cells], &out).unwrap();
    assert_eq!(so.unmapped_masked, 1, "the V1 cut with DEF mask 2 has no V1 VIA:MASK:2 entry");

    let mut rd = Db::new();
    let back = rd.gds_read(&out).unwrap();
    let mut shapes = BTreeSet::new();
    let mut labels = BTreeSet::new();
    for e in rd.gds_elements(back, "masks").unwrap() {
        match e {
            GdsElement::Boundary { layer, datatype, xy, .. } => {
                let xs = xy.iter().map(|p| p.0);
                let ys = xy.iter().map(|p| p.1);
                let b = [xs.clone().min().unwrap(), ys.clone().min().unwrap(), xs.max().unwrap(), ys.max().unwrap()];
                shapes.insert((layer, datatype, b));
            }
            GdsElement::Text { layer, datatype, origin, text, .. } => {
                labels.insert((layer, datatype, text, origin));
            }
            other => panic!("unexpected element {other:?}"),
        }
    }
    let want: BTreeSet<_> = KLAYOUT_SHAPES.iter().copied().collect();
    assert_eq!(shapes, want);
    let want: BTreeSet<_> = KLAYOUT_LABELS.iter().map(|&(l, d, t, o)| (l, d, t.to_string(), o)).collect();
    assert_eq!(labels, want);
    let _ = std::fs::remove_file(out);
    let _ = std::fs::remove_file(cells);
}

/// The masks of special-wire and block-pin boxes, box for box with their geometry.
#[test]
fn special_wire_and_pin_boxes_carry_their_def_masks() {
    let mut db = Db::new();
    db.read_lef(fixture("masks.lef")).unwrap();
    db.read_def(fixture("masks.def"), "default").unwrap();
    assert_eq!(db.swire_boxes().unwrap().len(), 1);
    assert_eq!(db.swire_box_masks().unwrap(), vec![2]);
    assert_eq!(db.swire_box_nets().unwrap(), vec!["VDD".to_string()]);
    let boxes = db.bpin_layer_boxes("p", 0).unwrap();
    let masks = db.bpin_layer_box_masks("p", 0).unwrap();
    assert_eq!(boxes.len(), masks.len());
    let m2 = db.layer_get_number("M2") as i64;
    let mut got: Vec<(i64, i32, u8)> = boxes.iter().zip(&masks).map(|(b, &m)| (b.0, b.1, m)).collect();
    got.sort();
    let m1 = db.layer_get_number("M1") as i64;
    let mut want = vec![(m2, 925, 2), (m2, 1025, 1), (m1, 900, 0)];
    want.sort();
    assert_eq!(got, want);
}
