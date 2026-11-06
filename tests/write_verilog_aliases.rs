// SPDX-License-Identifier: Apache-2.0
//! `write_verilog` port aliases against the reference (`tests/fixtures/write_verilog_alias`,
//! `top.golden.v` written by the pinned reference from `top.def` with `ref.tcl`; never edited).

use vyges_opendb::Db;

const D: &str = "tests/fixtures/write_verilog_alias/";

fn design(def: &str) -> Db {
    let mut db = Db::new();
    db.read_lef("tests/fixtures/insert_buffer/cells.lef").expect("lef");
    db.read_def(format!("{D}{def}"), "default").expect("def");
    db
}

// Rule (`VerilogWriter::writeAssigns`, OpenSTA 22a4bb4d / 0b73f5da / d39cf5eb at cc9027d6): a port
// whose net is named otherwise gets an assign — an input port on a signal net `assign net = port;`,
// an output or inout port `assign port = net;`; a port on a net of its own name none.
#[test]
fn a_port_on_a_net_named_otherwise_is_aliased_as_the_reference_writes_it() {
    let golden = std::fs::read_to_string(format!("{D}top.golden.v")).expect("golden");
    assert_eq!(vyges_opendb::verilog::write_verilog(&design("top.def"), false, &[]).unwrap(), golden);
}

// Rule (`VerilogWriter::checkInputAliases`, OpenSTA 4ef36d3b / 378c4e97): two input ports on one
// signal net cannot be written (STA-2749), checked before anything is written. Witness: the
// reference on `shared.def`, `cannot write Verilog: input ports in_a and in_b share signal net n_in`.
#[test]
fn two_input_ports_on_one_signal_net_are_refused() {
    let e = vyges_opendb::verilog::write_verilog(&design("shared.def"), false, &[]).unwrap_err();
    assert_eq!(e, "cannot write Verilog: input ports in_a and in_b share signal net n_in");
}
