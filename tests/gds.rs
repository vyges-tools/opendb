// SPDX-License-Identifier: Apache-2.0
// The GDS surface over odb's reader and writer, and write-gds's refusals.
#![cfg(unix)]

use vyges_opendb::gds::{self, GdsElement, LayerMap};
use vyges_opendb::Db;

const INV1: &str = "tests/fixtures/sky130_fd_sc_hd__inv_1.gds";

fn tmp(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("vyges_opendb_{}_{name}", std::process::id()))
}

/// Every record of a GDS stream: (record type, data type, data).
fn records(b: &[u8]) -> Vec<(u8, u8, Vec<u8>)> {
    let (mut i, mut out) = (0, Vec::new());
    while i + 4 <= b.len() {
        let len = u16::from_be_bytes([b[i], b[i + 1]]) as usize;
        out.push((b[i + 2], b[i + 3], b[i + 4..i + len].to_vec()));
        i += len;
        if b[i - len + 2] == 0x04 {
            break;
        }
    }
    out
}

/// Written back, every ASCII record is of even length, as GDSII requires — odb's writer alone
/// leaves the odd ones unpadded (inv_1's one-letter pin labels) — and odb's reader reads the
/// padded file as the original.
#[test]
fn a_written_library_has_only_even_strings_and_reads_back_the_same() {
    let mut db = Db::new();
    let lib = db.gds_read(INV1).unwrap();
    let out = tmp("inv1.gds");
    db.gds_write(lib, &out).unwrap();
    let recs = records(&std::fs::read(&out).unwrap());
    let strings: Vec<_> = recs.iter().filter(|r| r.1 == 0x06).collect();
    assert!(strings.iter().any(|r| r.2.last() == Some(&0)), "inv_1 has odd-length strings to pad");
    assert!(strings.iter().all(|r| r.2.len() % 2 == 0));
    let back = db.gds_read(&out).unwrap();
    let s = "sky130_fd_sc_hd__inv_1";
    assert_eq!(db.gds_elements(back, s).unwrap(), db.gds_elements(lib, s).unwrap());
    assert_eq!(db.gds_units(back).unwrap(), (1e-3, 1e-9));
}

/// A cell the design instantiates and no cell GDS defines is refused, naming the cell — the
/// reference stream-out's ghost-cell error — rather than written as an empty reference.
#[test]
fn write_gds_refuses_a_cell_without_gds() {
    let mut db = Db::open("tests/fixtures/counter.odb").unwrap();
    let map = LayerMap::parse("met1 NET 68 20\n").unwrap();
    let err = gds::write_gds(&mut db, &map, &[INV1], tmp("counter.gds")).unwrap_err().to_string();
    assert!(err.contains("no GDS for cell(s)") && err.contains("sky130_fd_sc_hd__"), "{err}");
}

#[test]
fn a_new_library_round_trips_through_the_writer() {
    let mut db = Db::new();
    let lib = db.gds_create_lib("lib").unwrap();
    db.gds_set_units(lib, 1e-3, 1e-9).unwrap();
    db.gds_create_structure(lib, "leaf").unwrap();
    db.gds_create_structure(lib, "top").unwrap();
    db.gds_add(lib, "leaf", &[GdsElement::rect((68, 20), 0, 0, 100, 50)]).unwrap();
    let mx = gds::orient_strans("MX").unwrap();
    let top = vec![
        GdsElement::SRef { structure: "leaf".into(), origin: (1000, 2000), trans: mx, props: vec![] },
        GdsElement::Text { layer: 69, datatype: 5, origin: (5, 5), presentation: (0, 0), trans: gds::STrans::IDENTITY, text: "abc".into(), props: vec![] },
    ];
    db.gds_add(lib, "top", &top).unwrap();
    let out = tmp("new.gds");
    db.gds_write(lib, &out).unwrap();
    let back = db.gds_read(&out).unwrap();
    assert_eq!(db.gds_lib_name(back).unwrap(), "lib");
    assert_eq!(db.gds_structures(back).unwrap(), ["leaf", "top"]);
    assert_eq!(db.gds_elements(back, "top").unwrap(), top);
}
