// SPDX-License-Identifier: Apache-2.0
//! Track masks (DEF `TRACKS ... MASK n [SAMEMASK]`) through odb's track grid, DEF reader and
//! writer: the colour grid a multi-patterned layer's wires are coloured by.
//!
//! 🔑 odb stores the mask per pattern (`dbTrackGrid::addGridPatternX/Y(origin, count, step,
//! first_mask, samemask)`, `getGridPatternX/Y` with the same two) and writes it back as
//! `MASK n [SAMEMASK]` only when `first_mask` is not 0 (`defout_impl.cpp`).
#![cfg(unix)]

use vyges_opendb::{Db, TrackMask};

fn fixture(f: &str) -> String {
    format!("{}/tests/fixtures/masks/{f}", env!("CARGO_MANIFEST_DIR"))
}

fn tmp(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("vyges_opendb_tracks_{}_{name}", std::process::id()))
}

fn load(def: &str) -> Db {
    let mut db = Db::new();
    db.read_lef(fixture("masks.lef")).unwrap();
    db.read_def(def, "default").unwrap();
    db
}

const NONE: TrackMask = TrackMask { first_mask: 0, same_mask: false };

/// Masks written on new patterns come back from the grid, survive `write_def` + `read_def`, and
/// a pattern written without one stays without.
#[test]
fn track_masks_round_trip_through_the_grid_and_def() {
    let mut db = load(&fixture("masks.def"));
    // The fixture's DEF tracks carry no mask.
    assert_eq!(db.track_pattern_masks("M1").unwrap(), (vec![], vec![NONE]));
    db.add_track_pattern_x_masked("M3", 100, 9, 200, 0, false).unwrap();
    db.add_track_pattern_y_masked("M1", 1050, 5, 100, 2, false).unwrap();
    db.add_track_pattern_x_masked("M2", 1050, 5, 100, 1, true).unwrap();
    let m1 = (vec![], vec![NONE, TrackMask { first_mask: 2, same_mask: false }]);
    let m2 = (vec![NONE, TrackMask { first_mask: 1, same_mask: true }], vec![]);
    assert_eq!(db.track_pattern_masks("M1").unwrap(), m1);
    assert_eq!(db.track_pattern_masks("M2").unwrap(), m2);
    let out = tmp("tracks.def");
    db.write_def(&out).unwrap();
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(text.contains("STEP 100 MASK 2 LAYER M1"), "{text}");
    assert!(text.contains("STEP 100 MASK 1 SAMEMASK LAYER M2"), "{text}");
    let back = load(out.to_str().unwrap());
    assert_eq!(back.track_pattern_masks("M1").unwrap(), m1);
    assert_eq!(back.track_pattern_masks("M2").unwrap(), m2);
    assert_eq!(back.track_pattern_masks("M3").unwrap().0, vec![NONE]);
    let _ = std::fs::remove_file(out);
}

/// A track mask past three is refused rather than handed to odb.
#[test]
fn a_track_mask_past_three_is_refused() {
    let mut db = load(&fixture("masks.def"));
    let err = db.add_track_pattern_y_masked("M1", 50, 5, 100, 4, false).unwrap_err().to_string();
    assert!(err.contains("track mask 4"), "{err}");
}
