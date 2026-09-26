// SPDX-License-Identifier: Apache-2.0
//! A database error a user can act on: a DEF read that fails because no LEF defined its cells must
//! say so — the generic error, its cause with the offending cell and instance, and what to do —
//! rather than the bare `ODB-0421` libodb's exception carries.
#![cfg(unix)]

use vyges_opendb::Db;

fn fixture(f: &str) -> String {
    format!("{}/tests/fixtures/tiny/{f}", env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn a_def_whose_cells_no_lef_defines_explains_itself() {
    // The technology half of the fixture LEF only: everything before its first MACRO.
    let lef = std::fs::read_to_string(fixture("tiny.lef")).unwrap();
    let tech = &lef[..lef.find("\nMACRO").expect("the fixture defines cells")];
    let tech_path = std::env::temp_dir().join(format!("vyges-opendb-tech-{}.lef", std::process::id()));
    std::fs::write(&tech_path, format!("{tech}\nEND LIBRARY\n")).unwrap();

    let mut db = Db::new();
    db.read_lef(&tech_path).expect("the technology reads");
    let e = db.read_def(fixture("tiny.def"), "default").expect_err("no LEF defines the DEF's cells").to_string();
    assert!(e.starts_with("ODB-0421: DEF parser returns an error!"), "{e}");
    assert!(e.contains("caused by ODB-0092: unknown library cell referenced ("), "{e}");
    assert!(e.contains("a LEF defining that cell was not read"), "{e}");
}
