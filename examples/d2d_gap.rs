// SPDX-License-Identifier: Apache-2.0
//! The measurement behind `check-d2d`: what `check_3dblox` reports for a broken die-to-die
//! interface, next to what `check-d2d` reports for the same one.
//!
//! Run with `cargo run --features gen-write --example d2d_gap`. This is the evidence for the
//! claim in `src/d2d.rs`, kept runnable so it can be re-checked against a newer OpenROAD rather
//! than believed.
use vyges_opendb::d2d::{check, BumpMap, Transform};
use vyges_opendb::{registry, Db};

/// Build a two-die stack whose interface has the defect described by `offset` and `mate`.
fn assembly(offset: i32, mate: bool) -> Db {
    let mut db = Db::open("tests/fixtures/counter.odb").unwrap();
    let m = db.find_master("");
    db.create_chip("stack", "", "HIER").unwrap();
    db.create_chip_block("stack", "stack_blk").unwrap();
    db.create_chip("base", "", "SUBSTRATE").unwrap();
    db.create_chip("upper", "", "DIE").unwrap();
    for (c, w, h, t) in [("base", 60000, 50000, 1500), ("upper", 50000, 40000, 700)] {
        db.chip_set_width(c, w).unwrap();
        db.chip_set_height(c, h).unwrap();
        db.chip_set_thickness(c, t).unwrap();
    }
    db.create_chip_region("base", "up", "FRONT", "").unwrap();
    db.set_chip_region_box("base", "up", 0, 0, 60000, 50000).unwrap();
    db.create_chip_region("upper", "down", "BACK", "").unwrap();
    db.set_chip_region_box("upper", "down", 0, 0, 50000, 40000).unwrap();

    db.create_chip_block("upper", "upper_blk").unwrap();
    db.set_top_chip("upper").unwrap();
    for (n, x) in [("tx0", 1000), ("tx1", 2000)] {
        db.create_inst(&m, n).unwrap();
        db.set_inst_location(n, x, 1000).unwrap();
    }
    db.create_chip_block("base", "base_blk").unwrap();
    db.set_top_chip("base").unwrap();
    db.create_inst(&m, "rx0").unwrap();
    db.set_inst_location("rx0", 1000 + offset, 1000).unwrap();
    if mate {
        db.create_inst(&m, "rx1").unwrap();
        db.set_inst_location("rx1", 2000, 1000).unwrap();
    }
    db.set_top_chip("stack").unwrap();

    db.create_chip_bump("upper", "down", "tx0").unwrap();
    db.create_chip_bump("upper", "down", "tx1").unwrap();
    db.create_chip_bump("base", "up", "rx0").unwrap();
    if mate {
        db.create_chip_bump("base", "up", "rx1").unwrap();
    }
    db.create_chip_inst("stack", "base", "u_base").unwrap();
    db.create_chip_inst("stack", "upper", "u_upper").unwrap();
    db.place_chip_inst("stack", "u_base", "R0", 0, 0, 0).unwrap();
    db.place_chip_inst("stack", "u_upper", "R0", 0, 0, 1500).unwrap();
    db.create_chip_conn("bond", "stack", "u_upper", "down", "u_base", "up", 0).unwrap();
    db.create_chip_net("stack", "d2d0").unwrap();
    db.add_chip_net_bump("stack", "d2d0", "u_upper", "down", 0).unwrap();
    db.add_chip_net_bump("stack", "d2d0", "u_base", "up", 0).unwrap();
    db.set_top_chip("stack").unwrap();
    db.construct_unfolded_model().unwrap();
    db
}

/// The same interface as a pair of bump maps, in microns (the fixture is 1000 DBU/um).
fn bump_maps(offset: i32, mate: bool) -> (BumpMap, BumpMap) {
    let top = "tx0 MICROBUMP 1.0 1.0 tx[0] d2d0\ntx1 MICROBUMP 2.0 1.0 tx[1] d2d1\n";
    let mut bot = format!(
        "rx0 MICROBUMP {:.3} 1.0 rx[0] d2d0\n",
        1.0 + f64::from(offset) / 1000.0
    );
    if mate {
        bot.push_str("rx1 MICROBUMP 2.0 1.0 rx[1] d2d1\n");
    }
    (BumpMap::parse(top), BumpMap::parse(&bot))
}

fn main() {
    println!(
        "{:<46} {:>14} {:>14}",
        "interface", "check_3dblox", "check-d2d"
    );
    println!("{}", "-".repeat(78));

    for (label, offset, mate) in [
        ("a top bump with no mating bump at all", 0, false),
        ("everything mated and exactly aligned", 0, true),
        ("a mating pair off by 1 DBU (1 nm)", 1, true),
        ("a mating pair off by 5000 DBU (5 um)", 5000, true),
    ] {
        let db = assembly(offset, mate);
        // check_3dblox takes &self — it annotates in memory and never edits the design.
        let upstream = db.check_3dblox().unwrap();
        // Show the bumps really reached the model, so a clean upstream result cannot be
        // explained away as "there was nothing to check".
        let mut seen: Vec<String> = Vec::new();
        for p in ["u_upper", "u_base"] {
            for b in 0..2 {
                let g = |f: &str| {
                    registry::get(
                        &db,
                        "dbUnfoldedChipBumpInst",
                        f,
                        &[p.into(), "0".into(), b.to_string()],
                    )
                    .ok()
                    .and_then(|v| v.as_i64())
                };
                if let (Some(x), Some(y)) = (g("get_global_position_x"), g("get_global_position_y")) {
                    if (x, y) != (0, 0) {
                        seen.push(format!("{p}({x},{y})"));
                    }
                }
            }
        }

        let (t, b) = bump_maps(offset, mate);
        let ours = check(&t, &b, Transform::default(), None);
        println!(
            "{label:<46} {upstream:>14} {:>14}",
            ours.violations()
        );
        println!("      bumps in the unfolded model: {}", seen.join(" "));
        for f in &ours.findings {
            println!("      check-d2d: {}", f.message());
        }
    }
    println!(
        "\nEvery row above is a real defect. Rows 1, 3 and 4 are dead or mis-wired silicon."
    );
}
