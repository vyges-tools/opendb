// SPDX-License-Identifier: Apache-2.0
//! `Db::dft_write_scan_chains` — `Dft::writeToOdb` as recorded in the database, read back through
//! the DEF writer's SCANCHAINS section.

use vyges_opendb::{Db, DftScanChain, DftScanInst, DftTerm};

fn design() -> Db {
    let mut db = Db::new();
    db.read_lef("tests/fixtures/insert_buffer/cells.lef").expect("lef");
    db.read_def("tests/fixtures/insert_buffer/top.def", "default").expect("def");
    for n in ["sc_first", "sc_mid", "sc_last"] {
        db.create_inst("BUF_X1", n).unwrap();
    }
    for (net, port, io) in [("se", "se", "INPUT"), ("si", "si", "INPUT"), ("so", "so", "OUTPUT")] {
        db.create_net(net).unwrap();
        db.create_bterm(net, port).unwrap();
        db.bterm_set_io_type(port, io).unwrap();
    }
    db
}

fn chain(name: &str) -> DftScanChain {
    let pin = |i: &str, t: &str| DftTerm::Pin(i.into(), t.into());
    DftScanChain {
        name: name.into(),
        cells: ["sc_first", "sc_mid", "sc_last"]
            .map(|i| DftScanInst { inst: i.into(), bits: 1, enable: pin(i, "A"), scan_in: pin(i, "A"), scan_out: pin(i, "Z"), clock: "clk".into(), falling: false })
            .into(),
        enable: DftTerm::Port("se".into()),
        scan_in: DftTerm::Port("si".into()),
        scan_out: DftTerm::Port("so".into()),
    }
}

fn scanchains(db: &Db) -> String {
    let dir = std::env::temp_dir().join(format!("vyges-opendb-dft-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("out.def");
    db.write_def(&path).unwrap();
    let def = std::fs::read_to_string(&path).unwrap();
    let start = def.find("SCANCHAINS").expect("a SCANCHAINS section");
    let end = def[start..].find("END SCANCHAINS").unwrap();
    def[start..start + end].to_string()
}

// Upstream rule (`Dft::writeToOdb`, OpenROAD #12097): `dbScanList::add` inserts at the front, so
// the list is reversed once its cells are in and runs scan-in to scan-out, as the DEF reader
// stores it; and the block's scan chains are reset first, so a second write replaces the first.
#[test]
fn scan_chains_run_in_to_out_and_replace_the_last_ones() {
    let mut db = design();
    db.dft_write_scan_chains(&[chain("chain_a")]).unwrap();
    let s = scanchains(&db);
    let at = |n: &str| s.find(n).unwrap_or_else(|| panic!("{n} in {s}"));
    assert!(at("sc_first") < at("sc_mid") && at("sc_mid") < at("sc_last"), "{s}");
    db.dft_write_scan_chains(&[chain("chain_b")]).unwrap();
    let s = scanchains(&db);
    assert!(s.contains("chain_b") && !s.contains("chain_a"), "{s}");
}
