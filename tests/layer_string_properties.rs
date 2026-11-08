// SPDX-License-Identifier: Apache-2.0
//! A layer's string properties (`Db::layer_string_properties`): odb's LEF reader keeps every layer
//! `PROPERTY` as a string property on the layer, whether it parsed it into a rule or not (an
//! unsupported one only gets an INFO). Synthetic technology; every name and value is invented.

use vyges_opendb::Db;

const LEF: &str = "VERSION 5.8 ;\nUNITS\n  DATABASE MICRONS 1000 ;\nEND UNITS\nMANUFACTURINGGRID 0.005 ;\n\
LAYER M1\n  TYPE ROUTING ;\n  DIRECTION HORIZONTAL ;\n  PITCH 0.12 ;\n  WIDTH 0.04 ;\n  SPACING 0.04 ;\n\
  PROPERTY LEF58_SPACING \"SPACING 0.09 ENDOFLINE 0.07 WITHIN 0.025 ;\" ;\n\
  PROPERTY LEF58_EOLENCLOSURE \"EOLENCLOSURE 0.05 ABOVE 0.01 ;\" ;\nEND M1\n\
LAYER V1\n  TYPE CUT ;\n  SPACING 0.05 ;\n  WIDTH 0.04 ;\nEND V1\nEND LIBRARY\n";

/// `tag` keeps each test's LEF path its own: the tests run in parallel and each removes its file.
fn db(tag: &str) -> Db {
    let path = std::env::temp_dir().join(format!("opendb-layer-props-{}-{tag}.lef", std::process::id()));
    std::fs::write(&path, LEF).expect("write lef");
    let mut db = Db::new();
    db.read_lef(path.to_str().expect("utf-8 path")).expect("read lef");
    let _ = std::fs::remove_file(&path);
    db
}

/// Rule (lefin `layer()`): every PROPERTY is stored, the parsed one (LEF58_SPACING) and the one odb
/// does not support (LEF58_EOLENCLOSURE) alike, with its raw text; a layer without any has none,
/// and an unknown layer reads as empty.
#[test]
fn every_layer_property_is_kept_parsed_or_not() {
    let d = db("kept");
    let got = d.layer_string_properties("M1");
    let names: Vec<&str> = got.iter().map(|(k, _)| k.as_str()).collect();
    assert!(names.contains(&"LEF58_SPACING") && names.contains(&"LEF58_EOLENCLOSURE"), "{got:?}");
    let enc = got.iter().find(|(k, _)| k == "LEF58_EOLENCLOSURE").expect("kept");
    assert_eq!(enc.1.trim(), "EOLENCLOSURE 0.05 ABOVE 0.01 ;");
    assert!(d.layer_string_properties("V1").is_empty());
    assert!(d.layer_string_properties("NOPE").is_empty());
}

/// The raw text survives a database round trip.
#[test]
fn the_properties_survive_a_write_and_read_back() {
    let d = db("roundtrip");
    let out = std::env::temp_dir().join(format!("opendb-layer-props-{}.odb", std::process::id()));
    d.write(out.to_str().expect("utf-8 path")).expect("write odb");
    let back = Db::open(out.to_str().expect("utf-8 path")).expect("open odb");
    let _ = std::fs::remove_file(&out);
    assert_eq!(back.layer_string_properties("M1"), d.layer_string_properties("M1"));
}
