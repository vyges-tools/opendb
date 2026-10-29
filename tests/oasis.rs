// SPDX-License-Identifier: Apache-2.0
// OASIS through the Db: a GDS library written as OASIS and read back into odb.
#![cfg(unix)]

use vyges_opendb::gds::{GdsElement, STrans};
use vyges_opendb::oasis::{self, WriteOptions};
use vyges_opendb::Db;

const INV1: &str = "tests/fixtures/sky130_fd_sc_hd__inv_1.gds";

fn tmp(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("vyges_opendb_oasis_{}_{name}", std::process::id()))
}

/// An element as OASIS keeps it: a text without presentation or transform, a BOX as the
/// rectangle BOUNDARY it draws.
fn as_oasis_keeps(e: &GdsElement) -> String {
    let e = match e.clone() {
        GdsElement::Text { layer, datatype, origin, text, props, .. } => {
            GdsElement::Text { layer, datatype, origin, presentation: (0, 0), trans: STrans::IDENTITY, text, props }
        }
        GdsElement::Box { layer, datatype, bounds: (x0, y0, x1, y1), props } => {
            let mut r = GdsElement::rect((layer, datatype), x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1));
            if let GdsElement::Boundary { props: p, .. } = &mut r {
                *p = props;
            }
            r
        }
        e => e,
    };
    format!("{e:?}")
}

/// inv_1 written as OASIS and read back: the same units, the same structures, and every
/// element as OASIS keeps it.
#[test]
fn a_gds_library_written_as_oasis_reads_back_as_oasis_keeps_it() {
    let mut db = Db::new();
    let lib = db.gds_read(INV1).unwrap();
    let out = tmp("inv1.oas");
    db.oasis_write(lib, &out, &WriteOptions::default()).unwrap();
    let back = db.oasis_read(&out).unwrap();
    let (u0, m0) = db.gds_units(lib).unwrap();
    let (u1, m1) = db.gds_units(back).unwrap();
    assert!((u0 - u1).abs() < 1e-15 && (m0 - m1).abs() < 1e-21, "{u0} {m0} vs {u1} {m1}");
    assert_eq!(db.gds_structures(back).unwrap(), db.gds_structures(lib).unwrap());
    for s in db.gds_structures(lib).unwrap() {
        let mut want: Vec<String> = db.gds_elements(lib, &s).unwrap().iter().map(as_oasis_keeps).collect();
        let mut got: Vec<String> = db.gds_elements(back, &s).unwrap().iter().map(as_oasis_keeps).collect();
        want.sort();
        got.sort();
        assert!(!want.is_empty());
        assert_eq!(got, want, "structure {s}");
    }
    let _ = std::fs::remove_file(out);
}

/// The extension decides the format, in any case; `layout_read` and `layout_write` follow it.
#[test]
fn the_extension_names_the_format() {
    assert!(oasis::is_oasis_path("a.oas") && oasis::is_oasis_path("a.OASIS") && oasis::is_oasis_path("d/a.Oas"));
    assert!(!oasis::is_oasis_path("a.gds") && !oasis::is_oasis_path("oas") && !oasis::is_oasis_path("a.oas.gz"));
    let mut db = Db::new();
    let lib = db.layout_read(INV1).unwrap();
    let out = tmp("ext.oas");
    db.layout_write(lib, &out).unwrap();
    assert!(std::fs::read(&out).unwrap().starts_with(b"%SEMI-OASIS\r\n"));
    let back = db.layout_read(&out).unwrap();
    assert_eq!(db.gds_structures(back).unwrap(), db.gds_structures(lib).unwrap());
    let _ = std::fs::remove_file(out);
}

/// Not an OASIS file, or one cut short: an error naming the file, not a panic.
#[test]
fn a_file_that_is_not_oasis_is_an_error() {
    let mut db = Db::new();
    let e = db.oasis_read(INV1).unwrap_err().to_string();
    assert!(e.contains("not an OASIS file") && e.contains("inv_1"), "{e}");
    let lib = db.gds_read(INV1).unwrap();
    let out = tmp("cut.oas");
    db.oasis_write(lib, &out, &WriteOptions::default()).unwrap();
    let b = std::fs::read(&out).unwrap();
    std::fs::write(&out, &b[..b.len() / 2]).unwrap();
    assert!(db.oasis_read(&out).is_err());
    let _ = std::fs::remove_file(out);
}
