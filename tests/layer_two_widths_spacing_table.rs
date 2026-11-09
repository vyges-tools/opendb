// SPDX-License-Identifier: Apache-2.0
//! A layer's classic TWOWIDTHS spacing table (`Db::layer_two_widths_spacing_table`) and odb's own
//! lookup on it (`Db::layer_find_tw_spacing`). Synthetic technology; every name and value is
//! invented.

use vyges_opendb::{Db, TwoWidthsSpacingTable};

const LEF: &str = "VERSION 5.8 ;\nUNITS\n  DATABASE MICRONS 1000 ;\nEND UNITS\nMANUFACTURINGGRID 0.005 ;\n\
LAYER M1\n  TYPE ROUTING ;\n  DIRECTION HORIZONTAL ;\n  PITCH 0.12 ;\n  WIDTH 0.04 ;\n\
  SPACINGTABLE\n    TWOWIDTHS\n      WIDTH 0.00 0.04 0.06 0.10\n      WIDTH 0.10 PRL 0.20 0.06 0.08 0.12\n\
      WIDTH 0.30 PRL 0.40 0.10 0.12 0.16 ;\nEND M1\n\
LAYER M2\n  TYPE ROUTING ;\n  DIRECTION VERTICAL ;\n  PITCH 0.12 ;\n  WIDTH 0.04 ;\n  SPACING 0.04 ;\nEND M2\n\
END LIBRARY\n";

/// `tag` keeps each test's LEF path its own: the tests run in parallel and each removes its file.
fn db(tag: &str) -> Db {
    let path = std::env::temp_dir().join(format!("opendb-two-widths-{}-{tag}.lef", std::process::id()));
    std::fs::write(&path, LEF).expect("write lef");
    let mut db = Db::new();
    db.read_lef(path.to_str().expect("utf-8 path")).expect("read lef");
    let _ = std::fs::remove_file(&path);
    db
}

/// Rule (lefin, two-width spacing rule): rows in LEF order, each width's PRL or -1 when the row
/// gives none, the matrix row by row; a layer without the table (or an unknown one) reads `None`.
#[test]
fn the_table_reads_as_lefin_stores_it() {
    let d = db("read");
    let t = d.layer_two_widths_spacing_table("M1").expect("read").expect("M1 has the table");
    assert_eq!(
        t,
        TwoWidthsSpacingTable {
            widths: vec![0, 100, 300],
            prls: vec![-1, 200, 400],
            spacing: vec![vec![40, 60, 100], vec![60, 80, 120], vec![100, 120, 160]],
        }
    );
    assert_eq!(d.layer_two_widths_spacing_table("M2").expect("read"), None);
    assert_eq!(d.layer_two_widths_spacing_table("NOPE").expect("read"), None);
}

/// Rule (`dbTechLayer::findTwSpacing` / `getTwIdx`): each width picks the row BEFORE the first row
/// width not below it (a width equal to a row's is the row before), then rows are walked back
/// while their PRL exceeds the run (clamped at 0); a row without PRL takes any run.
#[test]
fn odb_lookup_on_the_table() {
    let d = db("find");
    let f = |w1, w2, prl| d.layer_find_tw_spacing("M1", w1, w2, prl).expect("find");
    assert_eq!(f(40, 40, 0), 40);
    assert_eq!(f(100, 100, 1000), 40, "100 is the row before the row of 100");
    assert_eq!(f(150, 40, 1000), 60);
    assert_eq!(f(150, 150, 199), 40, "row 1 needs a run of 200");
    assert_eq!(f(150, 150, 200), 80);
    assert_eq!(f(500, 500, 400), 160);
    assert_eq!(f(500, 500, 300), 80, "row 2 needs 400, row 1 takes 300");
    assert_eq!(f(500, 40, -5), 40, "a negative run is 0");
}

/// The table survives a database round trip.
#[test]
fn the_table_survives_a_write_and_read_back() {
    let d = db("roundtrip");
    let out = std::env::temp_dir().join(format!("opendb-two-widths-{}.odb", std::process::id()));
    d.write(out.to_str().expect("utf-8 path")).expect("write odb");
    let back = Db::open(out.to_str().expect("utf-8 path")).expect("open odb");
    let _ = std::fs::remove_file(&out);
    assert_eq!(back.layer_two_widths_spacing_table("M1").expect("read"), d.layer_two_widths_spacing_table("M1").expect("read"));
}
