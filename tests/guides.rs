// SPDX-License-Identifier: Apache-2.0
//! Route guides — global routing's output, and the first odb class we bind because an engine
//! needs to WRITE it rather than read it.
//!
//! Every assertion here states the upstream rule it pins, because the rule is not obvious from
//! the signature. `dbGuide::create(net, layer, via_layer, box, is_congested)` is declared in
//! `odb/include/odb/db.h`; `grt` calls it from `GlobalRouter::saveGuides`.

use vyges_opendb::Db;

const FIXTURE: &str = "tests/fixtures/counter.odb";

fn a_net(db: &Db) -> String {
    db.net_names().into_iter().next().expect("the fixture has at least one net")
}

/// Routing layers, in the tech's own order.
///
/// ⚠️ `layers_with_direction` is NOT a routing-layer filter — on this fixture it also returns
/// `pwell` and `nwell`, which carry a direction but have routing level 0. Filtering on the
/// direction alone silently hands back a well layer, and `dbGuide::create` accepts it, so the
/// mistake surfaces as a wrong layer name in a guide rather than as an error.
fn routing_layers(db: &Db) -> Vec<String> {
    db.layers_with_direction()
        .expect("the tech defines layers")
        .into_iter()
        .map(|(name, _dir)| name)
        .filter(|l| db.layer_get_routing_level(l) > 0)
        .collect()
}

/// Open the fixture with a clean guide slate.
///
/// ⚠️ `counter.odb` SHIPS WITH 6 guides already on it. Every test here used to assume index 0 was
/// the guide it had just written, which is only true of an empty net — so the suite is written
/// against a cleared block instead of against the fixture's incidental state.
fn open_cleared() -> Db {
    let mut db = Db::open(FIXTURE).expect("opens");
    db.clear_guides();
    db
}

fn a_layer(db: &Db) -> String {
    routing_layers(db).into_iter().next().expect("at least one routing layer")
}

#[test]
fn a_guide_reads_back_exactly_as_it_was_written() {
    let mut db = open_cleared();
    let net = a_net(&db);
    let layer = a_layer(&db);
    assert_eq!(db.num_guides(&net), 0, "open_cleared must leave a clean slate");

    // Deliberately asymmetric: four ints in a row is exactly the shape a transposition hides in.
    db.add_guide(&net, &layer, &layer, 100, 200, 3_000, 4_000, false)
        .expect("create");

    assert_eq!(db.num_guides(&net), 1);
    assert_eq!(
        (
            db.guide_get_box_x_min(&net, 0),
            db.guide_get_box_y_min(&net, 0),
            db.guide_get_box_x_max(&net, 0),
            db.guide_get_box_y_max(&net, 0),
        ),
        (100, 200, 3_000, 4_000)
    );
    assert_eq!(db.guide_get_box_dx(&net, 0), 2_900);
    assert_eq!(db.guide_get_box_dy(&net, 0), 3_800);
    assert_eq!(db.guide_get_net(&net, 0), net);
    assert_eq!(db.guide_get_layer(&net, 0), layer);
    assert!(!db.guide_is_congested(&net, 0));
}

#[test]
fn the_corner_order_does_not_matter_because_odb_rect_normalises() {
    // UPSTREAM RULE: `odb::Rect::init` does `std::tie(xlo_,xhi_) = std::minmax(x1,x2)`
    // (odb/geom.h). A caller that hands over the corners the other way round gets the same
    // rectangle, so our shim does not need to pre-order them — and must not be "fixed" to.
    let mut db = open_cleared();
    let net = a_net(&db);
    let layer = a_layer(&db);

    db.add_guide(&net, &layer, &layer, 3_000, 4_000, 100, 200, false)
        .expect("create with reversed corners");

    assert_eq!(db.guide_get_box_x_min(&net, 0), 100);
    assert_eq!(db.guide_get_box_y_min(&net, 0), 200);
    assert_eq!(db.guide_get_box_x_max(&net, 0), 3_000);
    assert_eq!(db.guide_get_box_y_max(&net, 0), 4_000);
}

#[test]
fn a_dbset_of_guides_iterates_NEWEST_FIRST() {
    // ⛔ UPSTREAM RULE, and the one that will bite `grt`: odb's `dbSet` PREPENDS, so reading a
    // net's guides back yields them in REVERSE creation order. Three guides, not two, because
    // with two "reversed" and "swapped" are the same observation and prove nothing.
    //
    // This is the same shape as the LEMON finding that decided `stt` — a third-party/underlying
    // container whose iteration order is not insertion order. An engine that writes guides in
    // route order and then reads index 0 expecting its first guide is wrong, silently.
    let mut db = open_cleared();
    let net = a_net(&db);
    let layer = a_layer(&db);

    // x_min encodes creation order: 10, 20, 30.
    for i in 1..=3 {
        db.add_guide(&net, &layer, &layer, i * 10, 0, i * 10 + 5, 5, false)
            .expect("create");
    }
    assert_eq!(db.num_guides(&net), 3);

    let seen: Vec<i32> = (0..3).map(|i| db.guide_get_box_x_min(&net, i)).collect();
    assert_eq!(
        seen,
        vec![30, 20, 10],
        "dbSet iterates newest-first; if this ever reads [10,20,30] odb changed and every \
         guide-order assumption in the routing engine needs re-checking"
    );
}

#[test]
fn via_layer_is_what_distinguishes_a_via_guide_from_a_wire_guide() {
    // UPSTREAM RULE: `via_layer` is NOT optional. `GlobalRouter::saveGuides` passes the SAME layer
    // twice for a wire segment and two DIFFERENT layers for a via, and `getViaLayer()` is the only
    // way a reader tells them apart. A caller that always passes `layer` twice writes wire guides
    // where vias belong — and upstream's `.guideok` goldens distinguish the two, so the mistake
    // would surface as a diff on every via in the design rather than as an error here.
    let mut db = open_cleared();
    let net = a_net(&db);
    let layers = routing_layers(&db);
    assert!(layers.len() >= 2, "need two routing layers to tell the shapes apart");
    let (lo, hi) = (&layers[0], &layers[1]);

    db.add_guide(&net, lo, lo, 0, 0, 100, 100, false).expect("wire guide");
    db.add_guide(&net, lo, hi, 0, 0, 100, 100, false).expect("via guide");

    // ⚠️ newest-first: index 0 is the VIA guide, index 1 the wire guide.
    assert_eq!(db.guide_get_layer(&net, 0), *lo);
    assert_eq!(db.guide_get_via_layer(&net, 0), *hi, "a via guide names the layer it rises to");

    assert_eq!(db.guide_get_layer(&net, 1), *lo);
    assert_eq!(db.guide_get_via_layer(&net, 1), *lo, "a wire guide names its own layer twice");
}

#[test]
fn is_congested_round_trips_and_is_not_a_default() {
    // `grt` computes this once per run as `is_congested_ && !allow_congestion_ && !use_cugr_`
    // and stamps every guide with it, so a binding that dropped the flag would be invisible
    // until a congested design was scored.
    let mut db = open_cleared();
    let net = a_net(&db);
    let layer = a_layer(&db);

    db.add_guide(&net, &layer, &layer, 0, 0, 10, 10, true).expect("congested");
    db.add_guide(&net, &layer, &layer, 0, 0, 10, 10, false).expect("clean");

    // ⚠️ newest-first (see `a_dbset_of_guides_iterates_NEWEST_FIRST`): index 0 is the CLEAN one.
    assert!(!db.guide_is_congested(&net, 0));
    assert!(db.guide_is_congested(&net, 1));
}

#[test]
fn clearing_removes_them_and_reports_how_many() {
    let mut db = open_cleared();
    let net = a_net(&db);
    let layer = a_layer(&db);
    for _ in 0..3 {
        db.add_guide(&net, &layer, &layer, 0, 0, 10, 10, false).expect("create");
    }
    assert_eq!(db.num_guides(&net), 3);

    assert_eq!(db.clear_guides(), 3);
    assert_eq!(db.num_guides(&net), 0);
}

#[test]
fn an_unknown_net_or_layer_is_an_error_not_a_silent_no_op() {
    let mut db = open_cleared();
    let net = a_net(&db);
    let layer = a_layer(&db);

    assert!(db.add_guide("no_such_net", &layer, &layer, 0, 0, 10, 10, false).is_err());
    assert!(db.add_guide(&net, "no_such_layer", &layer, 0, 0, 10, 10, false).is_err());
    assert!(db.add_guide(&net, &layer, "no_such_layer", 0, 0, 10, 10, false).is_err());
    assert_eq!(db.num_guides(&net), 0, "a failed create must leave nothing behind");
}

#[test]
fn guides_are_transactional_unlike_obstructions() {
    // UPSTREAM RULE, and the one worth having a test for: `odb/src/db/dbJournal.cpp` handles
    // `dbGuideObj` in BOTH directions — undo-of-create destroys the guide (:1776) and
    // undo-of-delete re-creates it (:1941). Guides are therefore NOT in the same category as
    // obstructions and blockages, whose geometry the journal does not carry, and `add_guide` is
    // safe inside `eco_try`. The default assumption for a geometry-ish class would have been the
    // opposite, which is why this is pinned rather than commented.
    let mut db = open_cleared();
    let net = a_net(&db);
    let layer = a_layer(&db);

    let kept = db
        .eco_try(|db| {
            db.add_guide(&net, &layer, &layer, 0, 0, 10, 10, false)?;
            Ok(false) // rejected
        })
        .expect("eco_try");
    assert!(!kept);
    assert_eq!(db.num_guides(&net), 0, "a rejected attempt must leave no guide behind");

    let kept = db
        .eco_try(|db| {
            db.add_guide(&net, &layer, &layer, 0, 0, 10, 10, false)?;
            Ok(true) // accepted
        })
        .expect("eco_try");
    assert!(kept);
    assert_eq!(db.num_guides(&net), 1);
}
