// SPDX-License-Identifier: Apache-2.0
//! `RDLSegment::preprocess`'s geometry, bound from boost::polygon rather than reimplemented.
//!
//! ⛔ **These tests exist to kill the obvious simplification.** The reference filters by
//! `polygon_90_set_data::interact`, which is TOUCH connectivity built on `touch_90_operation` —
//! the same scanline machinery behind `connectivity_extraction_90`. It keeps whole polygons whose
//! label touches the other set. A boolean AND (`a & b`) looks like the same thing and is not: two
//! shapes sharing an edge with zero gap **interact**, while their **intersection is empty**.
//!
//! 🔑 So `zero_gap_...` below is the load-bearing case. It passes only while the binding is really
//! calling `interact`; swap in an intersection-and-test-area and it fails.

use vyges_opendb::rdl_preprocess;

/// The pad lies entirely inside the bump — `rdl_route_assignments_overlapping_iterms`'s
/// `p_co2_8_o`, whose real geometry this is. Upstream locks the segment and writes NO wire.
#[test]
fn a_destination_inside_the_source_is_locked_with_no_wire() {
    let bump = [(130000, 5550000, 220000, 5640000)];
    let pad = [(170000, 5610000, 180000, 5630000)];
    let (verdict, stubs) = rdl_preprocess(&bump, &pad, 1600).unwrap();
    assert_eq!(verdict, 1, "overlapping shapes lock the segment");
    assert!(stubs.is_empty(), "branch 1 writes no wire of any kind, not even a stub");
}

/// ⛔ **The test that dies on `intersection`.** These two share the edge x = 1000 exactly: zero
/// gap, zero overlap area. `interact` keeps the polygon; `a & b` is empty.
#[test]
fn zero_gap_shapes_touch_and_are_locked_even_though_they_do_not_overlap() {
    let a = [(0, 0, 1000, 1000)];
    let b = [(1000, 0, 2000, 1000)];
    let (verdict, stubs) = rdl_preprocess(&a, &b, 1600).unwrap();
    assert_eq!(
        verdict, 1,
        "abutting shapes INTERACT — an intersection test would call this no contact and route it"
    );
    assert!(stubs.is_empty());
}

/// A gap smaller than the layer's spacing: locked, and the gap is bridged with stubs rather than
/// routed. `p_co2_6_o`'s real geometry — a 1000-unit gap against `metal10 getSpacing` of 1600.
#[test]
fn a_gap_below_the_layer_spacing_is_bridged_with_stubs() {
    let bump = [(130000, 5199000, 220000, 5289000)];
    let pad = [(170000, 5290000, 180000, 5310000)];
    let (verdict, stubs) = rdl_preprocess(&bump, &pad, 1600).unwrap();
    assert_eq!(verdict, 2, "within the layer spacing, so locked with stubs");
    assert!(!stubs.is_empty(), "branch 2 must produce the bridging rectangles");
    // The stub has to span the gap in y and must not reach back into the source.
    let spans = stubs.iter().any(|&(_, y0, _, y1)| y0 <= 5289000 && y1 >= 5290000);
    assert!(spans, "a stub must bridge 5289000..5290000, got {stubs:?}");
    assert!(
        stubs.iter().all(|&(_, _, _, y1)| y1 > 5289000),
        "`check -= source` removes the source's own area, so no stub lies inside the bump"
    );
}

/// Far apart in both senses: no contact, and none after bloating. The segment routes normally.
#[test]
fn shapes_beyond_the_spacing_need_a_route() {
    let a = [(0, 0, 1000, 1000)];
    let b = [(1000 + 1601, 0, 3000, 1000)];
    let (verdict, stubs) = rdl_preprocess(&a, &b, 1600).unwrap();
    assert_eq!(verdict, 0, "a gap wider than the spacing is a real routing job");
    assert!(stubs.is_empty());
}

/// ⚠️ The boundary itself: a gap of exactly the spacing. Bloating by `min_dist` closes it to a
/// touch, and a touch interacts — so this locks. One unit more and it routes.
#[test]
fn a_gap_of_exactly_the_spacing_still_interacts_once_bloated() {
    let a = [(0, 0, 1000, 1000)];
    let (v_at, _) = rdl_preprocess(&a, &[(2600, 0, 3600, 1000)], 1600).unwrap();
    let (v_past, _) = rdl_preprocess(&a, &[(2601, 0, 3601, 1000)], 1600).unwrap();
    assert_eq!(v_at, 2, "a gap of exactly min_dist closes to a touch when bloated");
    assert_eq!(v_past, 0, "one unit further apart and it is a route");
}
