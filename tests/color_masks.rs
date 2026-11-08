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
