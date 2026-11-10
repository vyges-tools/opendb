// SPDX-License-Identifier: Apache-2.0
//! LEF58_MINIMUMCUT rules written per cut class (`MINIMUMCUT CUTCLASS c n …`): the database keeps
//! each class's count in a map (by class name), read through `mincutrule_cut_class_cuts`. Every
//! layer name and value here is invented.

use vyges_opendb::Db;

fn db(name: &str, rules: &str) -> Db {
    let lef = format!(
        "VERSION 5.8 ;\nUNITS\n  DATABASE MICRONS 1000 ;\nEND UNITS\nMANUFACTURINGGRID 0.005 ;\n\
         LAYER V0\n  TYPE CUT ;\n  WIDTH 0.04 ;\n  PROPERTY LEF58_CUTCLASS \"CUTCLASS VA WIDTH 0.04 ; CUTCLASS VB WIDTH 0.05 ;\" ;\nEND V0\n\
         LAYER M1\n  TYPE ROUTING ;\n  DIRECTION HORIZONTAL ;\n  PITCH 0.12 ;\n  WIDTH 0.04 ;\n  SPACING 0.04 ;\n{rules}END M1\n\
         END LIBRARY\n"
    );
    let path = std::env::temp_dir().join(format!("vyges-opendb-mincut-{}-{name}.lef", std::process::id()));
    std::fs::write(&path, lef).expect("write lef");
    let mut db = Db::new();
    db.read_lef(path.to_str().expect("utf-8 path")).expect("read lef");
    let _ = std::fs::remove_file(&path);
    db
}

/// The per-class counts in the map's order (by class name, whatever the LEF's order); a rule
/// without CUTCLASS has none; an index past the rules gives none.
#[test]
fn a_per_cut_class_minimum_cut_rule_reads_its_counts_by_class_name() {
    let d = db(
        "pcc",
        "  PROPERTY LEF58_MINIMUMCUT \"MINIMUMCUT CUTCLASS VB 3 CUTCLASS VA 2 WIDTH 0.2 ;\" ;\n  PROPERTY LEF58_MINIMUMCUT \"MINIMUMCUT 2 WIDTH 0.3 ;\" ;\n",
    );
    assert_eq!(d.num_layer_get_tech_layer_min_cut_rules("M1"), 2);
    let all: Vec<Vec<(String, i32)>> = (0..2).map(|k| d.mincutrule_cut_class_cuts("M1", k)).collect();
    let per_class = all.iter().position(|v| !v.is_empty()).expect("one rule is per cut class");
    assert_eq!(all[per_class], [("VA".to_string(), 2), ("VB".to_string(), 3)]);
    assert!(all[1 - per_class].is_empty());
    assert!(d.mincutrule_cut_class_cuts("M1", 5).is_empty());
    assert!(d.mincutrule_cut_class_cuts("NOPE", 0).is_empty());
}
