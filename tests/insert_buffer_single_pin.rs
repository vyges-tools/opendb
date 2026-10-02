// SPDX-License-Identifier: Apache-2.0
//! `Db::insert_buffer_after_driver` / `Db::insert_buffer_before_load` — odb's own
//! `dbNet::insertBufferAfterDriver` / `dbNet::insertBufferBeforeLoad` — on flat versions of the
//! upstream `TestInsertBuffer` AfterDriver_Case1/2 and BeforeLoad_Case1 netlists (their module
//! instances dropped: this layer builds no hierarchy). Each asserts the connections that case
//! asserts — which side of the cut keeps the ORIGINAL net — and the nets that follow from them.

use std::collections::BTreeMap;

use vyges_opendb::Db;

fn design() -> Db {
    let mut db = Db::new();
    db.read_lef("tests/fixtures/insert_buffer/cells.lef").expect("lef");
    db.read_def("tests/fixtures/insert_buffer/top.def", "default").expect("def");
    db
}

/// Every net with a pin: its instance pins (`inst/pin`) and ports (`PORT:name`), sorted.
fn nets(db: &Db) -> BTreeMap<String, Vec<String>> {
    let mut out = BTreeMap::new();
    for net in db.net_names() {
        let mut members: Vec<String> = db.net_iterms(&net);
        members.extend(db.net_bterms(&net).into_iter().map(|b| format!("PORT:{b}")));
        if !members.is_empty() {
            members.sort();
            out.insert(net, members);
        }
    }
    out
}

fn expect(pairs: &[(&str, &[&str])]) -> BTreeMap<String, Vec<String>> {
    pairs
        .iter()
        .map(|(n, m)| {
            let mut v: Vec<String> = m.iter().map(|s| s.to_string()).collect();
            v.sort();
            (n.to_string(), v)
        })
        .collect()
}

fn port(db: &mut Db, net: &str, name: &str, io: &str) {
    db.create_bterm(net, name).unwrap();
    db.bterm_set_io_type(name, io).unwrap();
}

// AfterDriver_Case1: in -> drvr_inst -> net -> {load0_inst, load2_inst}. The rule: an INSTANCE-PIN
// driver moves to the new net (`net1`) and the buffer drives the ORIGINAL net, loads and all.
#[test]
fn after_an_instance_driver_the_driver_moves_to_the_new_net() {
    let mut db = design();
    for n in ["drvr_inst", "load0_inst", "load2_inst"] {
        db.create_inst("BUF_X1", n).unwrap();
    }
    db.create_net("in").unwrap();
    db.create_net("net").unwrap();
    port(&mut db, "in", "in", "INPUT");
    db.connect("drvr_inst", "A", "in").unwrap();
    db.connect("drvr_inst", "Z", "net").unwrap();
    db.connect("load0_inst", "A", "net").unwrap();
    db.connect("load2_inst", "A", "net").unwrap();
    let b = db.insert_buffer_after_driver((Some("drvr_inst"), "Z"), "BUF_X4", None, "buf", None, "ALWAYS").unwrap();
    assert_eq!(db.inst_master(&b), "BUF_X4");
    assert_eq!(
        nets(&db),
        expect(&[
            ("in", &["PORT:in", "drvr_inst/A"]),
            ("net1", &[&format!("{b}/A"), "drvr_inst/Z"]),
            ("net", &[&format!("{b}/Z"), "load0_inst/A", "load2_inst/A"]),
        ])
    );
}

// AfterDriver_Case2: port X drives net X -> {load0_inst, load2_inst}. The rule: a PORT driver keeps
// its net (`X`, the buffer's input) and the loads move to the new net the buffer drives (`net1`).
// buffer_ports -inputs is this call.
#[test]
fn after_a_port_driver_the_port_keeps_its_net() {
    let mut db = design();
    for n in ["load0_inst", "load2_inst"] {
        db.create_inst("BUF_X1", n).unwrap();
    }
    db.create_net("X").unwrap();
    port(&mut db, "X", "X", "INPUT");
    db.connect("load0_inst", "A", "X").unwrap();
    db.connect("load2_inst", "A", "X").unwrap();
    let b = db.insert_buffer_after_driver((None, "X"), "BUF_X4", None, "buf", None, "ALWAYS").unwrap();
    assert_eq!(
        nets(&db),
        expect(&[("X", &["PORT:X", &format!("{b}/A")]), ("net1", &[&format!("{b}/Z"), "load0_inst/A", "load2_inst/A"])])
    );
}

// BeforeLoad_Case1 (flat): drvr -> net -> {load0_inst, load2_inst, port load_output}. Before a
// load, the buffer's input stays on the original net and its output takes the load to a new net;
// before an OUTPUT PORT, the port keeps its net and the buffer's input takes a new one.
// buffer_ports -outputs is the port call.
#[test]
fn before_a_load_the_buffer_input_stays_on_the_net() {
    let mut db = design();
    for n in ["drvr_inst", "load0_inst", "load2_inst"] {
        db.create_inst("BUF_X1", n).unwrap();
    }
    db.create_net("net").unwrap();
    db.connect("drvr_inst", "Z", "net").unwrap();
    db.connect("load0_inst", "A", "net").unwrap();
    db.connect("load2_inst", "A", "net").unwrap();
    port(&mut db, "net", "load_output", "OUTPUT");
    let b1 = db.insert_buffer_before_load((Some("load0_inst"), "A"), "BUF_X4", None, "buf", None, "ALWAYS").unwrap();
    let after_one = nets(&db);
    assert!(after_one["net"].contains(&format!("{b1}/A")), "{after_one:?}");
    let load0_net = after_one.iter().find(|(_, m)| m.contains(&"load0_inst/A".to_string())).unwrap();
    assert_eq!(load0_net.1, &vec![format!("{b1}/Z"), "load0_inst/A".to_string()]);
    let b2 = db.insert_buffer_before_load((None, "load_output"), "BUF_X4", None, "buf", None, "ALWAYS").unwrap();
    let after_two = nets(&db);
    let port_net = after_two.iter().find(|(_, m)| m.contains(&"PORT:load_output".to_string())).unwrap();
    assert_eq!(port_net.1, &vec!["PORT:load_output".to_string(), format!("{b2}/Z")]);
    assert!(after_two.values().any(|m| m.contains(&format!("{b2}/A")) && m.contains(&"drvr_inst/Z".to_string())), "{after_two:?}");
}
