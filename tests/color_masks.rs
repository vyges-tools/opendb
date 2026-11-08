// SPDX-License-Identifier: Apache-2.0
//! The mask-colouring pass on the synthetic two-mask design (`tests/fixtures/masks/`): what it
//! colours, what it keeps, what it reports, and that the colours it writes reach the DEF.
//!
//! The fixture's tracks carry no DEF mask, so each pattern's first track is mask 1: M1 tracks
//! (horizontal) at y = 50, 150, 250 … are masks 1, 2, 1 …; M2 tracks at x = 50 … 950 make x = 950
//! the tenth track, mask 2.
#![cfg(unix)]

use vyges_loom::lef58_samemask::SameMaskRules;
use vyges_opendb::masks::{self, ColourOptions, Finding, SpacingSource};
use vyges_opendb::Db;

fn fixture(f: &str) -> String {
    format!("{}/tests/fixtures/masks/{f}", env!("CARGO_MANIFEST_DIR"))
}

fn load() -> Db {
    let mut db = Db::new();
    db.read_lef(fixture("masks.lef")).unwrap();
    db.read_def(fixture("masks.def"), "default").unwrap();
    db
}

fn kinds(f: &[Finding]) -> Vec<&'static str> {
    let mut k: Vec<&str> = f.iter().map(Finding::kind).collect();
    k.sort();
    k
}

/// An uncoloured on-track segment takes its track's mask; pre-coloured shapes keep theirs and a
/// disagreement with the track is reported (net a's M2 segment, mask 1 on a mask-2 track, and its
/// via's M1 digit); the uncoloured M1 pin box is reported; the via's empty M2 digit is filled.
#[test]
fn the_pass_colours_by_track_and_reports_what_it_keeps() {
    let mut db = load();
    let rep = masks::colour_masks(&mut db, &ColourOptions::default()).unwrap();
    assert_eq!(rep.layers.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(), ["M1", "M2"]);
    assert_eq!(rep.unmasked_tracks, ["M1", "M2"]);
    assert_eq!((rep.nets_checked, rep.nets_recoloured), (2, 2));
    assert_eq!(kinds(&rep.findings), ["precoloured_mismatch", "precoloured_mismatch", "uncoloured"]);
    // What reaches the DEF.
    let out = std::env::temp_dir().join(format!("vyges_opendb_colour_{}.def", std::process::id()));
    db.write_def(&out).unwrap();
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(text.contains("( 50 250 ) MASK 1 ( 950 * )"), "{text}");
    assert!(text.contains("MASK 221 V12"), "top digit filled from the M2 track (2), cut and bottom kept:\n{text}");
    let _ = std::fs::remove_file(out);
    // A second run has nothing left to colour.
    let again = masks::colour_masks(&mut db, &ColourOptions::default()).unwrap();
    assert_eq!(again.nets_recoloured, 0);
}

/// Two segments of one net on one track, 10 apart: coloured alike (same track), so the pair is a
/// same-mask spacing finding on that track. Neighbouring tracks alternate masks, so a stricter
/// same-mask spacing given for the layer finds nothing between correctly coloured neighbours —
/// only next to the via enclosure that kept its pre-coloured mask 1 on a mask-2 track.
#[test]
fn a_line_end_pair_on_one_track_is_a_same_mask_finding() {
    let mut db = load();
    let names = vec!["M1".to_string()];
    #[rustfmt::skip]
    let ops = [0, 0, 0, 1, 50, 250, 1, 500, 250, 0, 0, 0, 1, 560, 250, 1, 900, 250];
    db.net_write_wire("b", &ops, &names).unwrap();
    let rep = masks::colour_masks(&mut db, &ColourOptions::default()).unwrap();
    let same: Vec<&Finding> = rep.findings.iter().filter(|f| f.kind() == "same_mask_spacing").collect();
    assert_eq!(same.len(), 1, "{:?}", rep.findings);
    let Finding::SameMaskSpacing { layer, mask, gap, same_track, .. } = same[0] else { unreachable!() };
    assert_eq!((layer.as_str(), *mask, *gap, *same_track), ("M1", 1, 10, true));

    let opts = ColourOptions {
        same_mask_spacing: [("M1".to_string(), 120)].into(),
        check_only: true,
        ..ColourOptions::default()
    };
    let rep = masks::colour_masks(&mut db, &opts).unwrap();
    let mut pairs: Vec<([i32; 4], [i32; 4])> = rep
        .findings
        .iter()
        .filter_map(|f| match f {
            Finding::SameMaskSpacing { a, b, .. } => Some((a.1, b.1)),
            _ => None,
        })
        .collect();
    pairs.sort();
    let via = [900, 125, 1000, 175];
    assert_eq!(pairs.len(), 3, "{:?}", rep.findings);
    assert_eq!(pairs.iter().filter(|(a, b)| *a == via || *b == via).count(), 2, "{pairs:?}");
}

/// No multi-mask layer: nothing to do, and the report says so rather than "applied".
#[test]
fn a_design_without_multi_mask_layers_is_left_alone() {
    let mut db = Db::open(format!("{}/tests/fixtures/counter.odb", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let rep = masks::colour_masks(&mut db, &ColourOptions::default()).unwrap();
    assert!(rep.layers.is_empty());
    assert_eq!((rep.nets_checked, rep.nets_recoloured, rep.findings.len()), (0, 0, 0));
}

/// The same-mask spacing comes from the layer's LEF58 SAMEMASK rules (which odb drops) when the
/// tech LEF is given: the larger of a plain `SPACING s SAMEMASK` and a TWOWIDTHS table's first
/// entry, in DBU; `--same-mask-spacing` still overrides it, and a layer with no rule keeps its
/// base spacing. The LEF58 value finds the same pairs as the equal option value.
#[test]
fn lef58_same_mask_rules_set_the_spacing() {
    let mut m1 = SameMaskRules::default();
    m1.read_property("LEF58_SPACINGTABLE", "SPACINGTABLE TWOWIDTHS SAMEMASK WIDTH 0 0.12 0.14 WIDTH 0.1 0.14 0.16 ;");
    m1.read_property("LEF58_SPACING", "SPACING 0.11 SAMEMASK ;");
    let opts = ColourOptions {
        same_mask_rules: [("M1".to_string(), m1)].into(),
        check_only: true,
        ..ColourOptions::default()
    };
    let db = load();
    let layers = masks::mask_layers(&db, &opts).unwrap();
    let got: Vec<(&str, i32, SpacingSource)> =
        layers.iter().map(|l| (l.name.as_str(), l.same_mask_spacing, l.spacing_source)).collect();
    assert_eq!(got, [("M1", 120, SpacingSource::Lef58SameMask), ("M2", 50, SpacingSource::BaseSpacing)]);

    let both = ColourOptions { same_mask_spacing: [("M1".to_string(), 70)].into(), ..opts.clone() };
    let m1 = &masks::mask_layers(&db, &both).unwrap()[0];
    assert_eq!((m1.same_mask_spacing, m1.spacing_source), (70, SpacingSource::Option));

    // the same pairs as the option at the same value
    let mut db = load();
    let names = vec!["M1".to_string()];
    #[rustfmt::skip]
    let ops = [0, 0, 0, 1, 50, 250, 1, 500, 250, 0, 0, 0, 1, 560, 250, 1, 900, 250];
    db.net_write_wire("b", &ops, &names).unwrap();
    let _ = masks::colour_masks(&mut db, &ColourOptions::default()).unwrap();
    let by_lef = masks::colour_masks(&mut db, &opts).unwrap();
    let by_option = ColourOptions { same_mask_spacing: [("M1".to_string(), 120)].into(), check_only: true, ..ColourOptions::default() };
    let by_option = masks::colour_masks(&mut db, &by_option).unwrap();
    type Pair = ([i32; 4], [i32; 4], i64, i32, &'static str);
    let pairs = |r: &masks::ColourReport| -> Vec<Pair> {
        r.findings
            .iter()
            .filter_map(|f| match f {
                Finding::SameMaskSpacing { a, b, gap, required, rule, .. } => Some((a.1, b.1, *gap, *required, *rule)),
                _ => None,
            })
            .collect()
    };
    let (lef, opt) = (pairs(&by_lef), pairs(&by_option));
    assert_eq!(lef.len(), 3, "{lef:?}");
    // the same pairs and requirement; the rule named is the table's first entry (0.12 > 0.11)
    let strip = |v: &[Pair]| -> Vec<Pair> { v.iter().map(|p| (p.0, p.1, p.2, p.3, "")).collect() };
    assert_eq!(strip(&lef), strip(&opt));
    assert!(lef.iter().all(|p| p.4 == "SPACINGTABLE TWOWIDTHS SAMEMASK"), "{lef:?}");
    assert!(opt.iter().all(|p| p.4 == "layer same-mask spacing"), "{opt:?}");
}

/// The TWOWIDTHS SAMEMASK table is checked pair by pair, with odb's TWOWIDTHS lookup: two
/// same-mask M1 wires (y = 50 and y = 250, both mask 1, 150 apart) running in parallel for 950
/// take the long-run row (PRL 0.5) and must keep 200, which the screening spacing (the table's
/// first entry, 60) does not see. A short run keeps the first row.
#[test]
fn a_two_widths_same_mask_table_is_checked_by_width_and_run() {
    let mut m1 = SameMaskRules::default();
    m1.read_property("LEF58_SPACINGTABLE", "SPACINGTABLE TWOWIDTHS SAMEMASK WIDTH 0 0.06 0.06 WIDTH 0.04 PRL 0.5 0.06 0.2 ;");
    m1.read_property("LEF58_FORBIDDENSPACING", "FORBIDDENSPACING 0.2 0.25 SAMEMASK ;");
    let opts = ColourOptions { same_mask_rules: [("M1".to_string(), m1)].into(), ..ColourOptions::default() };
    let mut db = load();
    let rep = masks::colour_masks(&mut db, &opts).unwrap();
    let m1 = &rep.layers[0];
    assert_eq!((m1.same_mask_spacing, m1.spacing_source), (60, SpacingSource::Lef58SameMask));
    let rules = m1.rules.as_ref().unwrap();
    let not_checked = vec![(vyges_opendb::masks_check::NOT_CHECKED_FORBIDDEN_FORM, 1)];
    assert_eq!(rules.not_checked, not_checked, "a form the reference refuses (no TWOEDGES, PRL, WIDTH): counted");
    let long_run = rep.findings.iter().find_map(|f| match f {
        Finding::SameMaskSpacing { a, b, gap, required, rule, mask: 1, .. }
            if [a.1, b.1].contains(&[25, 25, 975, 75]) && [a.1, b.1].contains(&[25, 225, 975, 275]) =>
        {
            Some((*gap, *required, *rule))
        }
        _ => None,
    });
    assert_eq!(long_run, Some((150, 200, "SPACINGTABLE TWOWIDTHS SAMEMASK")), "{:?}", rep.findings);
    // the same design under the screening value alone: the pair is clean
    let flat = ColourOptions { same_mask_spacing: [("M1".to_string(), 60)].into(), check_only: true, ..ColourOptions::default() };
    let rep = masks::colour_masks(&mut db, &flat).unwrap();
    assert!(!rep.findings.iter().any(|f| matches!(f, Finding::SameMaskSpacing { gap: 150, .. })), "{:?}", rep.findings);
}

fn load_special() -> Db {
    let mut db = Db::new();
    db.read_lef(fixture("masks.lef")).unwrap();
    db.read_def(fixture("special.def"), "default").unwrap();
    db
}

/// Power-grid shapes: a narrow on-track strap takes its track's mask (M1 y = 350 is track 3,
/// mask 2; M2 x = 1550 is track 15, mask 2); a followpin between tracks, a strap wide enough to
/// reach three track centrelines and a strap across the preferred direction are reported and
/// left uncoloured; a pre-coloured strap keeps its mask, reported when its track says otherwise
/// (y = 850 is track 8, mask 1) and silent when it agrees (y = 1950, mask 2). M3 is single-mask.
#[test]
fn power_grid_shapes_take_their_tracks_mask_or_are_reported() {
    let mut db = load_special();
    let before: Vec<_> = db.special_boxes().unwrap();
    assert_eq!(before.len(), 8, "{before:?}");
    let rep = masks::colour_masks(&mut db, &ColourOptions::default()).unwrap();
    assert_eq!(rep.special_coloured, 2, "{:?}", rep.findings);
    let colour_kinds: Vec<&str> = kinds(&rep.findings)
        .into_iter()
        .filter(|k| !matches!(*k, "uncoloured" | "same_mask_spacing"))
        .collect();
    assert_eq!(colour_kinds, ["multi_track", "off_track", "precoloured_mismatch", "wrong_way"], "{:?}", rep.findings);
    let multi = rep.findings.iter().find_map(|f| match f {
        Finding::MultiTrack { tracks, rect, .. } => Some((*tracks, rect[1], rect[3])),
        _ => None,
    });
    assert_eq!(multi, Some((3, 425, 675)));
    let mm = rep.findings.iter().find_map(|f| match f {
        Finding::PrecolouredMismatch { has, track, at, .. } => Some((*has, *track, at.1)),
        _ => None,
    });
    assert_eq!(mm, Some((2, 1, 850)));
    // The three shapes left alone, and the via landings no coloured shape of their net holds (the
    // followpin via's M1, the y = 380 via's M1, both of the y = 850 via's), are the uncoloured ones
    // the block check reports.
    let unc = rep.findings.iter().filter(|f| f.kind() == "uncoloured").count();
    assert_eq!(unc, 7, "{:?}", rep.findings);
    let after = db.special_boxes().unwrap();
    let mask_at = |layer_y: i32, horizontal: bool| {
        after
            .iter()
            .find(|b| if horizontal { (b.rect[1] + b.rect[3]) / 2 == layer_y } else { (b.rect[0] + b.rect[2]) / 2 == layer_y })
            .map(|b| b.mask)
    };
    assert_eq!(mask_at(350, true), Some(2));
    assert_eq!(mask_at(1550, false), Some(2));
    assert_eq!(mask_at(100, true), Some(0));
    assert_eq!(mask_at(850, true), Some(2), "pre-coloured kept");
    assert_eq!(mask_at(1500, true), Some(0), "single-mask layer untouched");
    // What reaches the DEF, and a second run has nothing left to colour.
    let out = std::env::temp_dir().join(format!("vyges_opendb_colour_special_{}.def", std::process::id()));
    db.write_def(&out).unwrap();
    let text = std::fs::read_to_string(&out).unwrap();
    let _ = std::fs::remove_file(&out);
    assert!(text.contains("( 0 350 ) MASK 2 ( 2000 350 )"), "{text}");
    assert!(text.contains("( 1550 0 ) MASK 2 ( 1550 2000 )"), "{text}");
    let again = masks::colour_masks(&mut db, &ColourOptions::default()).unwrap();
    assert_eq!(again.special_coloured, 0);
}

/// Check-only colours no power-grid shape.
#[test]
fn check_only_leaves_power_grid_shapes_alone() {
    let mut db = load_special();
    let opts = ColourOptions { check_only: true, ..ColourOptions::default() };
    let rep = masks::colour_masks(&mut db, &opts).unwrap();
    assert_eq!(rep.special_coloured, 0);
    assert!(db.special_boxes().unwrap().iter().filter(|b| b.mask != 0).count() == 2);
}

/// The special-box mask setter refuses what odb cannot hold or what is not there: a mask past 3,
/// an index past the net's wires or boxes, a net that does not exist.
#[test]
fn the_special_box_setter_refuses_bad_input() {
    let mut db = load_special();
    assert!(db.set_special_box_mask("VDD", 0, 1, 4).is_err());
    assert!(db.set_special_box_mask("VDD", 0, 99, 1).is_err());
    assert!(db.set_special_box_mask("VDD", 9, 0, 1).is_err());
    assert!(db.set_special_box_mask("nope", 0, 0, 1).is_err());
    db.set_special_box_mask("VDD", 0, 1, 1).unwrap();
    let b = db.special_boxes().unwrap().into_iter().find(|b| b.net == "VDD" && b.sbox == 1).unwrap();
    assert_eq!((b.swire, b.mask, (b.rect[1] + b.rect[3]) / 2), (0, 1, 350));
}

/// A special via's landing metal takes the mask of the same net's coloured shape that wholly holds
/// it: at the M1 y = 350 / M2 x = 1550 crossing both landings take mask 2 (the cut is left as it
/// was); on the followpin (off-track, uncoloured) only the M2 landing is coloured; at y = 380 the
/// M1 landing only overlaps the y = 350 strap, so only M2 is coloured; at (500, 850) the M1 landing
/// sits in ANOTHER net's coloured strap (VSS), so nothing is.
#[test]
fn power_grid_via_landings_follow_the_shape_holding_them() {
    let mut db = load_special();
    let before = db.special_vias().unwrap();
    assert_eq!(before.len(), 4, "{before:?}");
    assert!(before.iter().all(|v| v.bottom.1.is_some() && v.top.1.is_some() && v.masks == [0, 0, 0]), "{before:?}");
    let rep = masks::colour_masks(&mut db, &ColourOptions::default()).unwrap();
    assert_eq!(rep.special_vias_coloured, 3);
    let mut got: Vec<(i32, [u8; 3])> = db
        .special_vias()
        .unwrap()
        .iter()
        .map(|v| { let r = v.bottom.1.unwrap(); ((r[1] + r[3]) / 2, v.masks) })
        .collect();
    got.sort();
    assert_eq!(got, [(100, [0, 0, 2]), (350, [2, 0, 2]), (380, [0, 0, 2]), (850, [0, 0, 0])]);
    let again = masks::colour_masks(&mut db, &ColourOptions::default()).unwrap();
    assert_eq!(again.special_vias_coloured, 0);
}

/// A pre-coloured via landing keeps its mask; one that differs from the shape holding it is
/// reported (both landings of the crossing via, set to mask 1 inside mask-2 straps).
#[test]
fn a_precoloured_via_landing_is_kept_and_reported() {
    let mut db = load_special();
    let v = db.special_vias().unwrap().into_iter().find(|v| v.bottom.1.unwrap()[1] == 325).unwrap();
    db.set_special_via_masks(&v.net, v.swire, v.sbox, [1, 0, 1]).unwrap();
    let rep = masks::colour_masks(&mut db, &ColourOptions::default()).unwrap();
    let mm: Vec<(&str, u8, u8)> = rep
        .findings
        .iter()
        .filter_map(|f| match f {
            Finding::PrecolouredMismatch { layer, has, track, at, .. } if *at == (1550, 350) => Some((layer.as_str(), *has, *track)),
            _ => None,
        })
        .collect();
    assert_eq!(mm, [("M1", 1, 2), ("M2", 1, 2)], "{:?}", rep.findings);
    let after = db.special_vias().unwrap().into_iter().find(|x| x.sbox == v.sbox && x.swire == v.swire && x.net == v.net).unwrap();
    assert_eq!(after.masks, [1, 0, 1]);
}

/// A pre-coloured power-grid shape that no single track names is kept and not reported: the
/// designer's mask stands (the followpin between tracks, given mask 1).
#[test]
fn a_precoloured_off_track_shape_is_kept_silently() {
    let mut db = load_special();
    let fp = db.special_boxes().unwrap().into_iter().find(|b| b.shape == "FOLLOWPIN").unwrap();
    db.set_special_box_mask(&fp.net, fp.swire, fp.sbox, 1).unwrap();
    let rep = masks::colour_masks(&mut db, &ColourOptions::default()).unwrap();
    assert!(!rep.findings.iter().any(|f| f.kind() == "off_track"), "{:?}", rep.findings);
    let after = db.special_boxes().unwrap().into_iter().find(|b| b.shape == "FOLLOWPIN").unwrap();
    assert_eq!(after.mask, 1);
}
