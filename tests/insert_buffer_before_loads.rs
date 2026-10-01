// SPDX-License-Identifier: Apache-2.0
//! `Db::insert_buffer_before_loads` — odb's own `dbNet::insertBufferBeforeLoads` — on the flat
//! netlists OpenROAD's `TestInsertBuffer` BeforeLoads cases build in code (1, 2, 4, 8, 9, 10, 11,
//! 12, 14), each checked against the netlist that case's expected output states: every net with
//! a pin, by its members, and the new instances' names and masters.
//!
//! Not here: the hierarchical cases (module nets) and the flat cases that read a Verilog netlist
//! first — this database layer has no Verilog reader.

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

fn it(inst: &str, pin: &str) -> (String, String) {
    (inst.to_string(), pin.to_string())
}

fn port(db: &mut Db, net: &str, name: &str, io: &str) {
    db.create_bterm(net, name).unwrap();
    db.bterm_set_io_type(name, io).unwrap();
}

// BeforeLoads_Case1: drvr -> n1 -> buf0 -> n2 -> load; a buffer before buf0/A, default names.
#[test]
fn case1_one_load() {
    let mut db = design();
    for (m, n) in [("BUF_X1", "drvr"), ("BUF_X1", "buf0"), ("BUF_X1", "load")] {
        db.create_inst(m, n).unwrap();
    }
    db.create_net("n1").unwrap();
    db.create_net("n2").unwrap();
    db.connect("drvr", "Z", "n1").unwrap();
    db.connect("buf0", "A", "n1").unwrap();
    db.connect("buf0", "Z", "n2").unwrap();
    db.connect("load", "A", "n2").unwrap();
    let b = db.insert_buffer_before_loads(Some("n1"), &[it("buf0", "A")], &[], "BUF_X4", None, "buf", None, "ALWAYS", false).unwrap();
    assert_eq!(b, "buf1");
    assert_eq!(db.inst_master("buf1"), "BUF_X4");
    assert_eq!(nets(&db), expect(&[("n1", &["buf1/A", "drvr/Z"]), ("n2", &["buf0/Z", "load/A"]), ("net1", &["buf0/A", "buf1/Z"])]));
}

// BeforeLoads_Case2: in -> buf0 -> out; a buffer before buf0/A, then one before the OUTPUT PORT:
// the new net takes the port's name and the original net, which had it, is renamed.
#[test]
fn case2_before_a_pin_then_before_an_output_port() {
    let mut db = design();
    db.create_inst("BUF_X1", "buf0").unwrap();
    db.create_net("in").unwrap();
    db.create_net("out").unwrap();
    port(&mut db, "in", "in", "INPUT");
    port(&mut db, "out", "out", "OUTPUT");
    db.connect("buf0", "A", "in").unwrap();
    db.connect("buf0", "Z", "out").unwrap();
    let b1 = db.insert_buffer_before_loads(Some("in"), &[it("buf0", "A")], &[], "BUF_X4", None, "buf", None, "ALWAYS", false).unwrap();
    let b2 = db.insert_buffer_before_loads(Some("out"), &[], &["out".into()], "BUF_X4", None, "buf", None, "ALWAYS", false).unwrap();
    assert_eq!((b1.as_str(), b2.as_str()), ("buf1", "buf2"));
    assert_eq!(
        nets(&db),
        expect(&[("in", &["PORT:in", "buf1/A"]), ("net1", &["buf0/A", "buf1/Z"]), ("net2", &["buf0/Z", "buf2/A"]), ("out", &["PORT:out", "buf2/Z"])])
    );
}

// BeforeLoads_Case4: two insertions on one net with base names `new0` and `new1` — the block-wide
// counter suffixes them (`new01`, `new12`); the second takes the first's input as a load.
#[test]
fn case4_counter_runs_across_calls() {
    let mut db = design();
    for n in ["drvr0", "load0", "load1", "load2"] {
        db.create_inst("BUF_X1", n).unwrap();
    }
    db.create_net("n1").unwrap();
    db.connect("drvr0", "Z", "n1").unwrap();
    for n in ["load0", "load1", "load2"] {
        db.connect(n, "A", "n1").unwrap();
    }
    let a = db.insert_buffer_before_loads(Some("n1"), &[it("load0", "A"), it("load1", "A")], &[], "BUF_X4", None, "new0", None, "ALWAYS", false).unwrap();
    let b = db.insert_buffer_before_loads(Some("n1"), &[it(&a, "A"), it("load2", "A")], &[], "BUF_X4", None, "new1", None, "ALWAYS", false).unwrap();
    assert_eq!((a.as_str(), b.as_str()), ("new01", "new12"));
    assert_eq!(
        nets(&db),
        expect(&[("n1", &["drvr0/Z", "new12/A"]), ("net1", &["load0/A", "load1/A", "new01/Z"]), ("net2", &["load2/A", "new01/A", "new12/Z"])])
    );
}

// BeforeLoads_Case8: input port -> load1; a buffer before load1/A.
#[test]
fn case8_input_port_net() {
    let mut db = design();
    db.create_inst("BUF_X1", "load1").unwrap();
    db.create_net("n1").unwrap();
    port(&mut db, "n1", "in", "INPUT");
    db.connect("load1", "A", "n1").unwrap();
    let b = db.insert_buffer_before_loads(Some("n1"), &[it("load1", "A")], &[], "BUF_X4", None, "new_buf", None, "ALWAYS", false).unwrap();
    assert_eq!(b, "new_buf1");
    assert_eq!(nets(&db), expect(&[("n1", &["PORT:in", "new_buf1/A"]), ("net1", &["load1/A", "new_buf1/Z"])]));
}

// BeforeLoads_Case9: drvr -> output port; a buffer before the port — its new net is named after it.
#[test]
fn case9_before_an_output_port() {
    let mut db = design();
    db.create_inst("BUF_X1", "drvr").unwrap();
    db.create_net("n1").unwrap();
    port(&mut db, "n1", "out", "OUTPUT");
    db.connect("drvr", "Z", "n1").unwrap();
    let b = db.insert_buffer_before_loads(Some("n1"), &[], &["out".into()], "BUF_X4", None, "new_buf", None, "ALWAYS", false).unwrap();
    assert_eq!(b, "new_buf1");
    assert_eq!(nets(&db), expect(&[("n1", &["drvr/Z", "new_buf1/A"]), ("out", &["PORT:out", "new_buf1/Z"])]));
}

// BeforeLoads_Case10: a feedthrough (in and out on one net); a buffer before the output port.
#[test]
fn case10_feedthrough() {
    let mut db = design();
    db.create_net("n1").unwrap();
    port(&mut db, "n1", "in", "INPUT");
    port(&mut db, "n1", "out", "OUTPUT");
    let b = db.insert_buffer_before_loads(Some("n1"), &[], &["out".into()], "BUF_X4", None, "new_buf", None, "ALWAYS", false).unwrap();
    assert_eq!(b, "new_buf1");
    assert_eq!(nets(&db), expect(&[("n1", &["PORT:in", "new_buf1/A"]), ("out", &["PORT:out", "new_buf1/Z"])]));
}

// BeforeLoads_Case11: input port -> load1, load2; a buffer before load1/A only.
#[test]
fn case11_partial_loads() {
    let mut db = design();
    db.create_inst("BUF_X1", "load1").unwrap();
    db.create_inst("BUF_X1", "load2").unwrap();
    db.create_net("n1").unwrap();
    port(&mut db, "n1", "in", "INPUT");
    db.connect("load1", "A", "n1").unwrap();
    db.connect("load2", "A", "n1").unwrap();
    let b = db.insert_buffer_before_loads(Some("n1"), &[it("load1", "A")], &[], "BUF_X4", None, "new_buf", None, "ALWAYS", false).unwrap();
    assert_eq!(b, "new_buf1");
    assert_eq!(nets(&db), expect(&[("n1", &["PORT:in", "load2/A", "new_buf1/A"]), ("net1", &["load1/A", "new_buf1/Z"])]));
}

// BeforeLoads_Case12: drvr -> load1 and an output port; a buffer before BOTH — the new net is
// named after the port.
#[test]
fn case12_pin_and_port_loads() {
    let mut db = design();
    db.create_inst("BUF_X1", "drvr").unwrap();
    db.create_inst("BUF_X1", "load1").unwrap();
    db.create_net("n1").unwrap();
    port(&mut db, "n1", "out", "OUTPUT");
    db.connect("drvr", "Z", "n1").unwrap();
    db.connect("load1", "A", "n1").unwrap();
    let b = db.insert_buffer_before_loads(Some("n1"), &[it("load1", "A")], &["out".into()], "BUF_X4", None, "new_buf", None, "ALWAYS", false).unwrap();
    assert_eq!(b, "new_buf1");
    assert_eq!(nets(&db), expect(&[("n1", &["drvr/Z", "new_buf1/A"]), ("out", &["PORT:out", "load1/A", "new_buf1/Z"])]));
}

// BeforeLoads_Case14: loads on two nets (`loads_on_diff_nets`), IF_NEEDED naming with base `buf`
// and no net base — the names need no suffix.
#[test]
fn case14_loads_on_different_nets_if_needed_names() {
    let mut db = design();
    db.create_inst("LOGIC0_X1", "drvr_inst").unwrap();
    db.create_inst("BUF_X1", "load1_inst").unwrap();
    db.create_inst("BUF_X1", "load2_inst").unwrap();
    db.create_net("drvr_net").unwrap();
    db.create_net("other_net").unwrap();
    db.connect("drvr_inst", "Z", "drvr_net").unwrap();
    db.connect("load1_inst", "A", "drvr_net").unwrap();
    db.connect("load2_inst", "A", "other_net").unwrap();
    let b = db.insert_buffer_before_loads(Some("drvr_net"), &[it("load1_inst", "A"), it("load2_inst", "A")], &[], "BUF_X1", None, "buf", None, "IF_NEEDED", true).unwrap();
    assert_eq!(b, "buf");
    assert_eq!(nets(&db), expect(&[("drvr_net", &["buf/A", "drvr_inst/Z"]), ("net", &["buf/Z", "load1_inst/A", "load2_inst/A"])]));
}

// With no net named, the first load's net in odb's set order is used (the resizer's call), and a
// location places the buffer there.
#[test]
fn no_net_infers_the_first_loads_net_and_a_location_places_it() {
    let mut db = design();
    db.create_inst("BUF_X1", "drvr").unwrap();
    db.create_inst("BUF_X1", "load").unwrap();
    db.create_net("n1").unwrap();
    db.connect("drvr", "Z", "n1").unwrap();
    db.connect("load", "A", "n1").unwrap();
    let b = db.insert_buffer_before_loads(None, &[it("load", "A")], &[], "BUF_X4", Some((4000, 2400)), "wire", None, "ALWAYS", false).unwrap();
    assert_eq!(b, "wire1");
    assert_eq!(db.inst_location("wire1"), (4000, 2400));
    assert_eq!(nets(&db), expect(&[("n1", &["drvr/Z", "wire1/A"]), ("net1", &["load/A", "wire1/Z"])]));
}
