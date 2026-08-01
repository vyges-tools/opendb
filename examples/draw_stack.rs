// SPDX-License-Identifier: Apache-2.0
//! Build a small 2.5D/3D assembly and draw it, to both back-ends.
//!
//! Doubles as the generator for the images in `docs/`: regenerate with
//! `cargo run --features gen-write --example draw_stack`. A committed picture that nothing can
//! reproduce goes stale silently, so the generator lives next to the output.
//!
//! The stack is deliberately not uniform — a wide substrate, an offset logic die, and a narrower
//! flipped memory die on top — because a drawing of two identical coincident dies demonstrates
//! none of what the views are for.
use vyges_opendb::view3d::{to_png, to_svg, Assembly3d};
use vyges_opendb::Db;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut db = Db::open("tests/fixtures/counter.odb")?;
    db.create_chip("stack", "", "HIER")?;
    db.create_chip_block("stack", "stack_blk")?;

    // (name, type, width, height, thickness) in DBU; the fixture is 1000 DBU/µm.
    let dies = [
        ("interposer", "SUBSTRATE", 5_200_000, 4_000_000, 100_000),
        ("logic", "DIE", 3_000_000, 2_600_000, 250_000),
        ("hbm", "DIE", 1_600_000, 2_200_000, 180_000),
    ];
    for (n, t, w, h, th) in dies {
        db.create_chip(n, "", t)?;
        db.chip_set_width(n, w)?;
        db.chip_set_height(n, h)?;
        db.chip_set_thickness(n, th)?;
        db.create_chip_region(n, "up", "FRONT", "")?;
        db.set_chip_region_box(n, "up", 0, 0, w, h)?;
        db.create_chip_region(n, "down", "BACK", "")?;
        db.set_chip_region_box(n, "down", 0, 0, w, h)?;
    }

    // Regions before insts: `create` derives region instances from the master as it stands.
    for (inst, master) in [
        ("u_interposer", "interposer"),
        ("u_logic", "logic"),
        ("u_hbm", "hbm"),
    ] {
        db.create_chip_inst("stack", master, inst)?;
    }
    db.place_chip_inst("stack", "u_interposer", "R0", 0, 0, 0)?;
    db.place_chip_inst("stack", "u_logic", "R0", 300_000, 400_000, 100_000)?;
    // MZ: flipped, so the memory die's FRONT faces down onto the logic die below it.
    db.place_chip_inst("stack", "u_hbm", "MZ", 3_400_000, 700_000, 350_000)?;

    db.create_chip_conn("logic_to_interposer", "stack", "u_logic", "down", "u_interposer", "up", 0)?;
    db.create_chip_conn("hbm_to_interposer", "stack", "u_hbm", "down", "u_interposer", "up", 0)?;
    db.set_top_chip("stack")?;
    db.construct_unfolded_model()?;

    let violations = db.check_3dblox()?;
    let findings = if violations > 0 {
        vec![("Floating chips".into(), "u_hbm".into())]
    } else {
        Vec::new()
    };
    let asm = Assembly3d::read(&db, "stack")?.with_findings(findings);

    std::fs::write("docs/example-stack.svg", to_svg(&asm, 1000.0))?;
    std::fs::write("docs/example-stack.png", to_png(&asm, 1000.0, 2.0))?;
    eprintln!(
        "wrote docs/example-stack.{{svg,png}} — {} dies, {} bonds, {violations} violation(s)",
        asm.dies.len(),
        asm.bonds.len()
    );
    Ok(())
}
