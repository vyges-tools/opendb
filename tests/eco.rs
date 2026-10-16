// SPDX-License-Identifier: Apache-2.0
use vyges_opendb::Db;

const FIXTURE: &str = "tests/fixtures/counter.odb";

#[test]
fn read_basics() {
    let db = Db::open(FIXTURE).expect("open");
    assert_eq!(db.block_name(), "counter");
    assert_eq!(db.num_insts(), 229);
    assert_eq!(db.num_nets(), 52);
    assert_eq!(db.inst_names().len(), 229);
}

// Find an instance with an input pin that is driven by a net.
fn find_driven_input(db: &Db) -> (String, String, String) {
    for i in 0..db.num_insts() {
        let inst = db.nth_inst_name(i);
        let pin = db.input_pin(&inst);
        if pin.is_empty() {
            continue;
        }
        let net = db.net_of(&inst, &pin);
        if !net.is_empty() {
            return (inst, pin, net);
        }
    }
    panic!("no driven input pin found in fixture");
}

#[test]
fn insert_buffer_end_to_end() {
    let mut db = Db::open(FIXTURE).unwrap();
    let (n0, m0) = (db.num_insts(), db.num_nets());

    let buf_master = db.find_master("buf");
    assert!(!buf_master.is_empty(), "no buffer master in fixture libs");
    let (inst, pin, driver) = find_driven_input(&db);

    db.insert_buffer(&inst, &pin, &buf_master, "vyges_buf0", 10_000, 10_000)
        .expect("insert_buffer");

    // one new inst + one new net
    assert_eq!(db.num_insts(), n0 + 1);
    assert_eq!(db.num_nets(), m0 + 1);
    // target pin moved to the new net; buffer input picked up the original driver
    assert_eq!(db.net_of(&inst, &pin), "vyges_buf0_net");
    let a = db.input_pin("vyges_buf0");
    assert_eq!(db.net_of("vyges_buf0", &a), driver);

    // the edit survives serialization
    let out = std::env::temp_dir().join("vyges_opendb_eco.odb");
    db.write(&out).unwrap();
    let db2 = Db::open(&out).unwrap();
    assert_eq!(db2.num_insts(), n0 + 1);
    assert_eq!(db2.net_of(&inst, &pin), "vyges_buf0_net");
}

#[test]
fn insert_eco_buffers_step() {
    use vyges_opendb::eco::{insert_eco_buffers, EcoBuffer};
    let mut db = Db::open(FIXTURE).unwrap();
    let (n0, m0) = (db.num_insts(), db.num_nets());
    let buf = db.find_master("buf");
    let (inst, pin, _driver) = find_driven_input(&db);

    let specs = vec![EcoBuffer { target: format!("{inst}/{pin}"), buffer: buf }];
    let n = insert_eco_buffers(&mut db, &specs).expect("insert_eco_buffers");

    assert_eq!(n, 1);
    assert_eq!(db.num_insts(), n0 + 1);
    assert_eq!(db.num_nets(), m0 + 1);
    assert_eq!(db.net_of(&inst, &pin), "eco_buffer_0_net");
}

#[test]
fn insert_eco_diodes_step() {
    use vyges_opendb::eco::{insert_eco_diodes, EcoDiode};
    let mut db = Db::open(FIXTURE).unwrap();
    let (n0, m0) = (db.num_insts(), db.num_nets());
    // a diode cell if the fixture libs have one; otherwise any cell with an input pin — this
    // exercises the tie-onto-net mechanism, which is what distinguishes a diode from a buffer.
    let mut cell = db.find_master("diode");
    if cell.is_empty() {
        cell = db.find_master("buf");
    }
    assert!(!cell.is_empty(), "no diode/buf master in fixture libs");
    let (inst, pin, driver) = find_driven_input(&db);

    let specs = vec![EcoDiode { target: format!("{inst}/{pin}"), diode: cell }];
    let n = insert_eco_diodes(&mut db, &specs).expect("insert_eco_diodes");

    assert_eq!(n, 1);
    // one new inst, and NO new net — a diode is a leaf tied onto the existing net
    assert_eq!(db.num_insts(), n0 + 1);
    assert_eq!(db.num_nets(), m0);
    // the target pin's net is UNCHANGED (no rewiring), and the diode's pin joined that same net
    assert_eq!(db.net_of(&inst, &pin), driver);
    let dp = db.input_pin("eco_diode_0");
    assert_eq!(db.net_of("eco_diode_0", &dp), driver);
}

#[test]
fn manual_placement_steps() {
    use vyges_opendb::eco::{
        manual_global_placement, manual_macro_placement, GlobalPlacement, MacroPlacement,
    };
    let mut db = Db::open(FIXTURE).unwrap();
    let inst = db.nth_inst_name(0);

    let n = manual_global_placement(
        &mut db,
        &[GlobalPlacement { instance: inst.clone(), x: 12_345, y: 67_890 }],
    )
    .unwrap();
    assert_eq!(n, 1);
    assert_eq!(db.inst_location(&inst), (12_345, 67_890));

    // macro placement + orient; location moves and survives serialization. The orientation is
    // set BEFORE the location, so the placed box's lower left lands exactly on the given point
    // whatever the orientation (set after, it would move the box about its origin).
    manual_macro_placement(
        &mut db,
        &[MacroPlacement { instance: inst.clone(), x: 1_000, y: 2_000, orient: Some("R90".into()) }],
        true,
    )
    .unwrap();
    assert_eq!(db.inst_location(&inst), (1_000, 2_000));
    assert_eq!(db.inst_get_orient(&inst), "R90");
    assert_eq!(db.inst_get_placement_status(&inst), "FIRM");
    // Already fixed: refused. Not in the design: refused.
    assert!(manual_macro_placement(&mut db, &[MacroPlacement { instance: inst.clone(), x: 0, y: 0, orient: None }], true).is_err());
    assert!(manual_macro_placement(&mut db, &[MacroPlacement { instance: "no_such_macro".into(), x: 0, y: 0, orient: None }], true).is_err());

    let out = std::env::temp_dir().join("vyges_opendb_place.odb");
    db.write(&out).unwrap();
    assert_eq!(Db::open(&out).unwrap().inst_location(&inst), (1_000, 2_000));
}

#[test]
fn diodes_on_ports_step() {
    use vyges_opendb::eco::{diodes_on_ports, DiodesOnPorts};
    let mut db = Db::open(FIXTURE).unwrap();
    let (n0, m0) = (db.num_insts(), db.num_nets());
    let mut diode = db.find_master("diode");
    if diode.is_empty() {
        diode = db.find_master("buf");
    }
    assert!(!diode.is_empty());

    let n = diodes_on_ports(&mut db, &DiodesOnPorts { diode, ports: vec![] }).unwrap();
    assert!(n > 0, "expected a diode on each connected port");
    // one new inst per diode, and NO new nets (each diode is a leaf tied onto the port's net)
    assert_eq!(db.num_insts(), n0 + n);
    assert_eq!(db.num_nets(), m0);
}

#[test]
fn obstruction_steps() {
    let mut db = Db::open(FIXTURE).unwrap();
    let n0 = db.num_obstructions();
    db.add_obstruction("met1", 0, 0, 1_000, 1_000).unwrap();
    db.add_obstruction("met2", 0, 0, 1_000, 1_000).unwrap();
    assert_eq!(db.num_obstructions(), n0 + 2);
    let removed = db.clear_obstructions();
    assert_eq!(removed, n0 + 2);
    assert_eq!(db.num_obstructions(), 0);
}

#[test]
fn custom_io_placement_step() {
    use vyges_opendb::eco::{custom_io_placement, IoPlacement};
    let mut db = Db::open(FIXTURE).unwrap();
    let port = db.bterm_names().into_iter().next().unwrap();
    let n = custom_io_placement(
        &mut db,
        &[IoPlacement { port, layer: "met2".into(), llx: 1_000, lly: 2_000, urx: 1_140, ury: 2_140 }],
    )
    .unwrap();
    assert_eq!(n, 1);
    // the placed pin survives serialization
    let out = std::env::temp_dir().join("vyges_opendb_io.odb");
    db.write(&out).unwrap();
    Db::open(&out).unwrap();
}

#[test]
fn write_def_exports_the_design() {
    let db = Db::open(FIXTURE).unwrap();
    let out = std::env::temp_dir().join("vyges_opendb_out.def");
    db.write_def(&out).unwrap();
    let def = std::fs::read_to_string(&out).unwrap();
    assert!(def.contains("DESIGN counter"), "DEF should name the design; got:\n{}", &def[..def.len().min(200)]);
    assert!(def.contains("END DESIGN"));
}

#[test]
fn write_def_then_apply_floorplan_template() {
    let db = Db::open(FIXTURE).unwrap();
    let (n0, m0) = (db.num_insts(), db.num_nets());
    // write-def produces a full DEF (round-trips the design to text)
    let def = std::env::temp_dir().join("vyges_opendb_rt.def");
    db.write_def(&def).unwrap();
    assert!(std::fs::read_to_string(&def).unwrap().contains("DESIGN counter"));

    // apply-def-template applies a floorplan *skeleton* (die area here) via FLOORPLAN mode —
    // it updates the floorplan and leaves components/nets intact.
    let tmpl = std::env::temp_dir().join("vyges_opendb_tmpl.def");
    std::fs::write(
        &tmpl,
        "VERSION 5.8 ;\nDESIGN counter ;\nUNITS DISTANCE MICRONS 1000 ;\nDIEAREA ( 0 0 ) ( 200000 200000 ) ;\nEND DESIGN\n",
    )
    .unwrap();
    let mut db2 = Db::open(FIXTURE).unwrap();
    db2.read_def(&tmpl, "floorplan").unwrap();
    assert_eq!(db2.num_insts(), n0);
    assert_eq!(db2.num_nets(), m0);
}

#[test]
fn errors_are_typed() {
    let mut db = Db::open(FIXTURE).unwrap();
    assert!(db.create_inst("no_such_master", "x").is_err());
    assert!(db.insert_buffer("no_inst", "A", "no_master", "b", 0, 0).is_err());
    assert!(db.insert_diode("no_inst", "A", "no_master", "d", 0, 0).is_err());
    assert!(db.set_inst_orient("no_inst", "R0").is_err());
    assert!(db.add_obstruction("no_such_layer", 0, 0, 1, 1).is_err());
}
