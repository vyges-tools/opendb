// SPDX-License-Identifier: Apache-2.0
//! Mask colour on routed wires (multi-patterned layers), through odb's own wire encoder and
//! decoder and its DEF reader and writer — nothing here re-implements the colour encoding.
//!
//! 🔑 The rules pinned, as odb (dbWireCodec.cpp, definNet.cpp, defout_impl.cpp) applies them:
//!   * A colour is STATE in the wire's op stream: it holds for every shape after it until the
//!     stream sets another (`setColor` / `clearColor`, 0 = none).
//!   * The encoder clears the metal colour after every via it adds; the via colour is not cleared.
//!   * A DEF `MASK n` before a point colours the segment that point ENDS; `MASK tcb` before a via
//!     gives its top, cut and bottom colours (in that order, digits 0..3).
//!
//! Fixture: `tests/fixtures/masks/` — a synthetic two-mask stack (M1, M2 at `MASK 2`).
#![cfg(unix)]

use std::path::PathBuf;
use vyges_opendb::{Db, ViaMask};

fn fixture(f: &str) -> String {
    format!("{}/tests/fixtures/masks/{f}", env!("CARGO_MANIFEST_DIR"))
}

fn tmp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("vyges_opendb_masks_{}_{name}", std::process::id()))
}

fn load(def: &str) -> Db {
    let mut db = Db::new();
    db.read_lef(fixture("masks.lef")).unwrap();
    db.read_def(def, "default").unwrap();
    db
}

type Seg = (String, (i32, i32), (i32, i32), Option<u8>);

/// A net's wire segments `(layer, from, to, colour)` and vias `(name, colour)`, walked from the
/// decoder's records and the colour state at each — a segment's colour is the state at its end.
fn walk(db: &Db, net: &str) -> (Vec<Seg>, Vec<(String, Option<ViaMask>)>) {
    let recs = db.net_wire_decode(net).unwrap();
    let masks = db.net_wire_decode_masks(net).unwrap();
    assert_eq!(recs.len(), masks.len(), "one mask state per decoder record");
    let (mut segs, mut vias) = (Vec::new(), Vec::new());
    let (mut layer, mut prev) = (String::new(), None);
    for (r, m) in recs.iter().zip(&masks).skip(1) {
        let f: Vec<&str> = r.split('|').collect();
        match f[0] {
            "0" | "1" | "2" | "3" => {
                layer = f[1].to_string();
                prev = None;
            }
            "4" | "5" => {
                let p = (f[1].parse().unwrap(), f[2].parse().unwrap());
                if let Some(q) = prev {
                    segs.push((layer.clone(), q, p, m.color));
                }
                prev = Some(p);
            }
            "6" | "7" => {
                vias.push((f[1].to_string(), m.via));
                layer = f[3].to_string();
            }
            _ => {}
        }
    }
    (segs, vias)
}

fn seg(layer: &str, a: (i32, i32), b: (i32, i32), c: Option<u8>) -> Seg {
    (layer.to_string(), a, b, c)
}

/// The DEF's colours read back as written: `MASK 1`/`MASK 2` on the M1 segments, `MASK 021` on
/// the via (top 0, cut 2, bottom 1), `MASK 1` on the M2 segment after it, and an uncoloured net.
#[test]
fn def_mask_statements_read_as_segment_and_via_colours() {
    let db = load(&fixture("masks.def"));
    let (segs, vias) = walk(&db, "a");
    assert_eq!(
        segs,
        vec![
            seg("M1", (50, 50), (950, 50), Some(1)),
            seg("M1", (50, 150), (950, 150), Some(2)),
            seg("M2", (950, 150), (950, 1050), Some(1)),
        ]
    );
    assert_eq!(vias, vec![("V12".to_string(), Some(ViaMask { bottom: 1, cut: 2, top: 0 }))]);
    let (segs, vias) = walk(&db, "b");
    assert_eq!(segs, vec![seg("M1", (50, 250), (950, 250), None)]);
    assert!(vias.is_empty());
}

/// Colours written through the encoder survive `write_def` and a fresh read: the round trip
/// item the colour step depends on.
#[test]
fn written_colours_survive_a_def_round_trip() {
    let mut db = load(&fixture("masks.def"));
    let names = vec!["M1".to_string(), "V12".to_string(), "M2".to_string()];
    #[rustfmt::skip]
    let ops = [
        0, 0, 0,          // path on M1
        1, 50, 250,       // start
        6, 2,             // metal colour 2
        1, 950, 250,      // M1 segment, colour 2
        8, 2, 1, 0,       // via colours: bottom 2, cut 1, top none
        3, 1,             // tech via V12 (the encoder clears the metal colour here)
        1, 950, 850,      // M2 segment, uncoloured
        0, 2, 0,          // a new path for the next coloured segment (see the test below)
        1, 950, 850,
        6, 1,
        1, 950, 1450,     // M2 segment, colour 1
    ];
    db.net_write_wire("b", &ops, &names).unwrap();
    let want_segs = vec![
        seg("M1", (50, 250), (950, 250), Some(2)),
        seg("M2", (950, 250), (950, 850), None),
        seg("M2", (950, 850), (950, 1450), Some(1)),
    ];
    let want_vias = vec![("V12".to_string(), Some(ViaMask { bottom: 2, cut: 1, top: 0 }))];
    assert_eq!(walk(&db, "b"), (want_segs.clone(), want_vias.clone()), "as encoded");

    let out = tmp("roundtrip.def");
    db.write_def(&out).unwrap();
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(text.contains("MASK 012 V12"), "DEF via mask is top, cut, bottom:\n{text}");
    let back = load(out.to_str().unwrap());
    // Compare what the colours ARE, segment by segment, not the op stream (the writer and the
    // reader need not produce the same ops for the same colouring).
    assert_eq!(walk(&back, "b"), (want_segs, want_vias), "after write_def + read_def");
    // And the untouched net keeps its colours too.
    assert_eq!(walk(&back, "a"), walk(&db, "a"));
    let _ = std::fs::remove_file(out);
}

/// ⛔ odb's DEF writer (`defout_impl.cpp`, `point_cnt % 2 == 0`) writes a wire `MASK` only before
/// the 2nd, 4th … point of a path, and the reader's colour is sticky: the colour of a segment
/// ending on the 3rd point is not merely dropped but read back as the previous segment's. Pinned
/// so a change upstream is noticed; the colour pass writes one coloured segment per path for it.
#[test]
fn the_def_writer_drops_the_mask_of_an_odd_point() {
    let mut db = load(&fixture("masks.def"));
    let names = vec!["M1".to_string()];
    #[rustfmt::skip]
    let ops = [0, 0, 0, 1, 50, 350, 6, 1, 1, 500, 350, 6, 2, 1, 950, 350];
    db.net_write_wire("b", &ops, &names).unwrap();
    let (segs, _) = walk(&db, "b");
    assert_eq!(segs[1].3, Some(2), "in the database the second segment is mask 2");
    let out = tmp("oddpoint.def");
    db.write_def(&out).unwrap();
    let back = load(out.to_str().unwrap());
    let (segs, _) = walk(&back, "b");
    assert_eq!(segs[0].3, Some(1));
    assert_eq!(segs[1].3, Some(1), "after DEF: the first segment's mask, not its own");
    let _ = std::fs::remove_file(out);
}

/// Each decomposed box carries its own mask: a segment its metal colour; a via's bottom metal,
/// cut and top metal their digits of the via colour.
#[test]
fn box_masks_line_up_with_the_decomposed_boxes() {
    let db = load(&fixture("masks.def"));
    let boxes = db.net_wire_boxes("a");
    let masks = db.net_wire_box_masks("a").unwrap();
    assert_eq!(boxes.len(), masks.len(), "one mask per box");
    let num = |l: &str| db.layer_get_number(l) as i64;
    let mut got: Vec<(i64, bool, u8)> = boxes.iter().zip(&masks).map(|(b, &m)| (b.layer, b.from_via, m)).collect();
    got.sort();
    let mut want = vec![
        (num("M1"), false, 1),
        (num("M1"), false, 2),
        (num("M1"), true, 1),
        (num("V1"), true, 2),
        (num("M2"), true, 0),
        (num("M2"), false, 1),
    ];
    want.sort();
    assert_eq!(got, want);
    assert_eq!(db.net_wire_box_masks("b").unwrap(), vec![0]);
}

/// A colour outside 1..3 is an error naming it — LEF/DEF allows three masks at most and 0 is "none".
#[test]
fn an_out_of_range_colour_is_refused() {
    let mut db = load(&fixture("masks.def"));
    let names = vec!["M1".to_string()];
    let err = db.net_write_wire("b", &[0, 0, 0, 1, 50, 250, 6, 4, 1, 950, 250], &names).unwrap_err().to_string();
    assert!(err.contains("mask colour 4"), "{err}");
    let err = db.net_write_wire("b", &[0, 0, 0, 1, 50, 250, 8, 0, 5, 0], &names).unwrap_err().to_string();
    assert!(err.contains("via mask colour 5"), "{err}");
}
