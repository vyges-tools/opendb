// SPDX-License-Identifier: Apache-2.0
//! Polygon pin geometry, and the polygon offset the RDL router's target generation needs.
//!
//! The shims themselves are exercised in `opendb-lib`; what is checked here is that the wrappers
//! unflatten correctly and that `bloat` behaves as the reference's callers assume.

use vyges_opendb::Db;

const FIXTURE: &str = "tests/fixtures/counter.odb";

/// The octagonal bump pad from OpenROAD's `Nangate45_io/dummy_pads.lef`, in DEF units at 2000
/// DBU/micron, as a closed ring.
fn octagon() -> Vec<(i32, i32)> {
    vec![
        (45_000, 16_820),
        (45_000, -16_820),
        (16_820, -45_000),
        (-16_820, -45_000),
        (-45_000, -16_820),
        (-45_000, 16_820),
        (-16_820, 45_000),
        (16_820, 45_000),
        (45_000, 16_820),
    ]
}

#[test]
fn shrinking_an_octagon_keeps_it_an_octagon_and_pulls_every_flat_side_in() {
    let db = Db::open(FIXTURE).expect("opens");
    let small = db.polygon_bloat(&octagon(), -6_000).expect("bloat");

    // Closed ring, still eight distinct corners.
    assert_eq!(small.first(), small.last(), "the result is a closed ring");
    assert_eq!(small.len(), 9, "an octagon shrinks to an octagon, not to a rectangle");

    // The four axis-aligned sides are the ones target generation uses, and each must have moved
    // inward by exactly the margin: x = ±45000 becomes ±39000, y = ±45000 becomes ±39000.
    let xs: Vec<i32> = small.iter().map(|p| p.0).collect();
    let ys: Vec<i32> = small.iter().map(|p| p.1).collect();
    assert_eq!(*xs.iter().max().unwrap(), 39_000);
    assert_eq!(*xs.iter().min().unwrap(), -39_000);
    assert_eq!(*ys.iter().max().unwrap(), 39_000);
    assert_eq!(*ys.iter().min().unwrap(), -39_000);
}

#[test]
fn a_negative_margin_shrinks_and_a_positive_one_grows() {
    let db = Db::open(FIXTURE).expect("opens");
    let grown = db.polygon_bloat(&octagon(), 6_000).expect("bloat");
    let xs: Vec<i32> = grown.iter().map(|p| p.0).collect();
    assert_eq!(*xs.iter().max().unwrap(), 51_000, "a positive margin pushes the side out");
}

// ⚠️ A rectangle is a polygon too, and `bloat` reports it as five points — the same closed-ring
// convention the die outline uses. A caller that walks consecutive pairs therefore sees four
// edges, not three.
#[test]
fn a_rectangle_bloats_to_a_closed_five_point_ring() {
    let db = Db::open(FIXTURE).expect("opens");
    let rect = vec![(0, 0), (100, 0), (100, 50), (0, 50), (0, 0)];
    let out = db.polygon_bloat(&rect, 10).expect("bloat");
    assert_eq!(out.len(), 5);
    assert_eq!(out.first(), out.last());
    let xs: Vec<i32> = out.iter().map(|p| p.0).collect();
    let ys: Vec<i32> = out.iter().map(|p| p.1).collect();
    assert_eq!((*xs.iter().min().unwrap(), *xs.iter().max().unwrap()), (-10, 110));
    assert_eq!((*ys.iter().min().unwrap(), *ys.iter().max().unwrap()), (-10, 60));
}

// A ring too short to be a polygon is refused rather than half-read.
#[test]
fn a_degenerate_ring_yields_nothing() {
    let db = Db::open(FIXTURE).expect("opens");
    assert!(db.polygon_bloat(&[(0, 0), (1, 1)], 5).expect("no error").is_empty());
}

// An instance terminal with only RECT ports reports no polygons — the polygon path must not
// invent geometry for the ordinary case.
#[test]
fn a_rectangular_terminal_reports_no_polygon_geometry() {
    let db = Db::open(FIXTURE).expect("opens");
    let inst = db.inst_names().into_iter().next().expect("at least one instance");
    // Every terminal of the first instance: none of this library's masters uses POLYGON ports.
    let polygons: usize = ["A", "B", "Y", "Z", "CLK", "D", "Q", "VDD", "VSS"]
        .iter()
        .map(|t| db.iterm_pin_polygons(&format!("{inst}/{t}")).expect("no error").len())
        .sum();
    assert_eq!(polygons, 0, "rectangular ports must not be reported as polygons");
}

#[test]
fn an_absent_terminal_reports_nothing_rather_than_failing() {
    let db = Db::open(FIXTURE).expect("opens");
    assert!(db.iterm_pin_polygons("no_such_inst/no_such_pin").expect("no error").is_empty());
    assert!(db.iterm_pin_polygons("malformed-name").expect("no error").is_empty());
}
