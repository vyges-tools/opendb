// SPDX-License-Identifier: Apache-2.0
//! `eliminate-dead-logic` — the reference's `eliminate_dead_logic` (`Resizer::eliminateDeadLogic`),
//! run by the open flow right after synthesis, before the floorplan sizes the die.
//!
//! Reference rule (pin 4155ac31), a mark and sweep:
//! * roots: the driver of each top port's net; every leaf whose master is not CORE (macros, pads)
//!   or that is dont-touch or sits on a dont-touch net;
//! * mark: from each kept instance, the driver of every non-supply pin's net (a net's driver is
//!   odb's `dbNet::getFirstDriverTerm`);
//! * sweep: unmarked leaves are destroyed, then every net with no terminal left (not special, not
//!   dont-touch).
//!
//! Oracle: the reference on `tests/fixtures/dead_logic` keeps `feeds_ram live1 live2 ram` and nets
//! `a clk n1 n5 y`, and reports "Removed 5 unused instances and 5 unused nets."

use std::path::PathBuf;
use std::process::Command;

use vyges_opendb::Db;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_vyges-opendb")
}

fn tmp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("vyges-edl-{}-{name}", std::process::id()))
}

fn run(args: &[&str]) -> String {
    let out = Command::new(bin()).args(args).output().expect("run vyges-opendb");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "vyges-opendb {args:?} failed: {stderr}");
    stderr
}

/// Each test imports to its own file: the tests of this binary run in parallel in one process, so
/// a shared `in.odb` would be rewritten by one test while another reads it.
fn imported(test: &str) -> PathBuf {
    let odb = tmp(&format!("{test}-in.odb"));
    run(&[
        "import",
        "--lef",
        "tests/fixtures/dead_logic/cells.lef",
        "--verilog",
        "tests/fixtures/dead_logic/top.v",
        "--output",
        odb.to_str().unwrap(),
    ]);
    odb
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

#[test]
fn removes_what_reaches_no_port_macro_or_dont_touch() {
    let input = imported("removes");
    let output = tmp("out.odb");
    let log = run(&["eliminate-dead-logic", "--input", input.to_str().unwrap(), "--output", output.to_str().unwrap()]);
    // The reference's own report line.
    assert!(log.contains("Removed 5 unused instances and 5 unused nets."), "{log}");

    let db = Db::open(output.to_str().unwrap()).unwrap();
    // Kept: the path to `y`; the macro (not CORE) and, through it, its input's driver.
    // Gone: a dead chain, the flop it feeds (a flop is CORE: nobody reads Q), a loop with no root.
    assert_eq!(sorted(db.inst_names()), ["feeds_ram", "live1", "live2", "ram"]);
    // Nets left with no terminal are removed: n2 n3 n4 (dead chain and flop), l1 l2 (loop).
    assert_eq!(sorted(db.net_names()), ["a", "clk", "n1", "n5", "y"]);
}

#[test]
fn a_second_run_removes_nothing() {
    let input = imported("second-run");
    let once = tmp("once.odb");
    let twice = tmp("twice.odb");
    run(&["eliminate-dead-logic", "--input", input.to_str().unwrap(), "--output", once.to_str().unwrap()]);
    let log = run(&["eliminate-dead-logic", "--input", once.to_str().unwrap(), "--output", twice.to_str().unwrap()]);
    assert!(log.contains("Removed 0 unused instances and 0 unused nets."), "{log}");
}

#[test]
fn a_check_that_can_fail_does_fail() {
    // Without the pass, the dead cells are still there: the assertion above is not vacuous.
    let db = Db::open(imported("control").to_str().unwrap()).unwrap();
    assert!(db.inst_names().iter().any(|i| i == "ff_dead"));
    assert!(db.net_names().iter().any(|n| n == "l1"));
}
