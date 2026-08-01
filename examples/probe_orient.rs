//! Where do a die's bumps land globally under each orientation?
use vyges_opendb::{registry, Db};
fn build(orient: &str) -> Db {
    let mut db = Db::open("tests/fixtures/counter.odb").unwrap();
    let m = db.find_master("");
    db.create_chip("stack", "", "HIER").unwrap();
    db.create_chip_block("stack", "stack_blk").unwrap();
    db.create_chip("upper", "", "DIE").unwrap();
    db.chip_set_width("upper", 50000).unwrap();
    db.chip_set_height("upper", 40000).unwrap();
    db.chip_set_thickness("upper", 700).unwrap();
    db.create_chip_region("upper", "down", "BACK", "").unwrap();
    db.set_chip_region_box("upper", "down", 0, 0, 50000, 40000).unwrap();
    db.create_chip_block("upper", "upper_blk").unwrap();
    db.set_top_chip("upper").unwrap();
    // one bump near the origin corner, so any mirror is unmistakable
    db.create_inst(&m, "b0").unwrap();
    db.set_inst_location("b0", 1000, 2000).unwrap();
    db.set_top_chip("stack").unwrap();
    db.create_chip_bump("upper", "down", "b0").unwrap();
    db.create_chip_inst("stack", "upper", "u").unwrap();
    db.place_chip_inst("stack", "u", orient, 0, 0, 0).unwrap();
    db.set_top_chip("stack").unwrap();
    db.construct_unfolded_model().unwrap();
    db
}
fn main() {
    println!("die 50000 x 40000 dbu, bump inst origin (1000, 2000)\n");
    for o in ["R0","R90","R180","R270","MX","MY","MXR90","MYR90",
              "MZ","MZ_R90","MZ_R180","MZ_R270","MZ_MX","MZ_MY","MZ_MXR90","MZ_MYR90","BOGUS"] {
        let db = build(o);
        let g = |f: &str| registry::get(&db, "dbUnfoldedChipBumpInst", f,
                                        &["u".into(), "0".into(), "0".into()])
            .ok().and_then(|v| v.as_i64());
        match (g("get_global_position_x"), g("get_global_position_y"), g("get_global_position_z")) {
            (Some(x), Some(y), Some(z)) => println!("  {o:<9} -> ({x}, {y}, {z})"),
            _ => println!("  {o:<9} -> unreadable"),
        }
    }
}
