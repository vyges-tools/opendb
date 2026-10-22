// SPDX-License-Identifier: Apache-2.0
//! `Db::net_iterm_places` — one lookup by name for a whole net — answers as the per-pin calls it
//! replaces: aligned with `net_iterms`, each pin's id (`iterm_id`), its place (`iterm_avg_xy`,
//! else its instance's origin) and its instance's placed flag (`inst_is_placed`).

use vyges_opendb::Db;

#[test]
fn a_nets_pin_places_answer_as_the_per_pin_calls() {
    let mut db = Db::new();
    db.read_lef("tests/fixtures/insert_buffer/cells.lef").expect("lef");
    db.read_def("tests/fixtures/insert_buffer/top.def", "default").expect("def");
    for (m, n) in [("BUF_X1", "drvr"), ("BUF_X4", "a"), ("BUF_X1", "b")] {
        db.create_inst(m, n).unwrap();
    }
    // one placed away from the origin, two not
    db.set_inst_location("a", 1400, 2800).unwrap();
    db.set_inst_placement_status("a", "PLACED").unwrap();
    db.create_net("n1").unwrap();
    db.connect("drvr", "Z", "n1").unwrap();
    db.connect("a", "A", "n1").unwrap();
    db.connect("b", "A", "n1").unwrap();
    let (mut placed, mut unplaced) = (0, 0);
    for net in db.net_names() {
        let names = db.net_iterms(&net);
        let places = db.net_iterm_places(&net);
        assert_eq!(places.len(), names.len(), "{net}: one place per pin");
        for (name, p) in names.iter().zip(&places) {
            let (inst, term) = name.rsplit_once('/').unwrap();
            assert_eq!(p.id, db.iterm_id(inst, term).unwrap(), "{name}: id");
            let xy = db.iterm_avg_xy(inst, term).unwrap_or_else(|| (db.inst_get_origin_x(inst), db.inst_get_origin_y(inst)));
            assert_eq!((p.x, p.y), xy, "{name}: place");
            assert_eq!(p.placed, db.inst_is_placed(inst), "{name}: placed");
            if p.placed { placed += 1 } else { unplaced += 1 }
        }
    }
    assert!(placed > 0 && unplaced > 0, "both kinds of pin seen: {placed} placed, {unplaced} not");
    assert!(db.net_iterm_places("no_such_net").is_empty());
}
