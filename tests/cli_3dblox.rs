// SPDX-License-Identifier: Apache-2.0
//! The 3D path **through the shipped binary**, not through the library.
//!
//! Every other 3D test in this repo calls `Db` directly, and all of them passed while the reader
//! was unreachable from the command line: `read_3dblox` existed, was tested, and no one holding a
//! `.3dbx` could do anything with it. A library test cannot catch that by construction, because
//! the thing that was missing is the wiring between the library and the binary.
//!
//! So this asserts the user-visible pipeline: a 3Dblox assembly file goes in, a database comes
//! out, and `check-3dblox` reads that database back. Two commands, no Rust.
#![cfg(feature = "gen-write")]

use std::path::PathBuf;
use std::process::Command;

const DBX: &str = "tests/fixtures/3dblox/example.3dbx";

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_vyges-opendb")
}

/// Per-test path under the target dir, named after the caller so two tests never collide.
fn out(tag: &str) -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    p.push(format!("cli_3dblox_{tag}.odb"));
    let _ = std::fs::remove_file(&p);
    p
}

#[test]
fn an_assembly_file_becomes_a_database_that_the_linter_can_read() {
    let odb = out("pipeline");

    let read = Command::new(bin())
        .args(["read-3dblox", "--input", DBX, "--output"])
        .arg(&odb)
        .output()
        .expect("run read-3dblox");
    assert!(
        read.status.success(),
        "read-3dblox failed: {}",
        String::from_utf8_lossy(&read.stderr)
    );
    assert!(odb.exists(), "read-3dblox reported success and wrote nothing");

    // The second half is the part that matters: the database has to be readable by a *separate
    // invocation*. A round trip inside one process would not prove the file is any good.
    let check = Command::new(bin())
        .args(["check-3dblox", "--input"])
        .arg(&odb)
        .output()
        .expect("run check-3dblox");
    assert!(
        check.status.success(),
        "check-3dblox failed: {}",
        String::from_utf8_lossy(&check.stderr)
    );

    let report = String::from_utf8_lossy(&check.stdout);
    let v: serde_json::Value = serde_json::from_str(report.trim()).expect("report is JSON");
    // Upstream's own example is a clean assembly. If this ever reports violations, either the
    // reader placed something wrongly or the checker regressed — both worth failing on.
    assert_eq!(v["violations"], 0, "upstream's example should lint clean: {report}");
}

#[test]
fn what_the_database_cannot_hold_is_named_on_the_way_through() {
    // `example.3dbx` contains a virtual bond (`bot: ~`) — a connection with no bottom die. odb has
    // nowhere to put it. The failure mode being guarded against is the quiet one: a read that
    // succeeds, drops it, and leaves the user believing the whole assembly loaded.
    let out = Command::new(bin())
        .args(["read-3dblox", "--input", DBX, "--output"])
        .arg(out("lossy"))
        .output()
        .expect("run read-3dblox");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("soc_to_virtual"),
        "the dropped connection must be named, by name: {err}"
    );
}

#[test]
fn the_command_describes_itself() {
    // `--describe` is how the agent layer discovers this command exists; it has to answer before
    // anyone supplies an input file.
    let out = Command::new(bin())
        .args(["read-3dblox", "--describe"])
        .output()
        .expect("run read-3dblox --describe");
    assert!(out.status.success());
    let d: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("--describe emits JSON");
    assert_eq!(d["name"], "read-3dblox");
    // The limits are part of the description. A caller that reads this and not the source should
    // still learn that a heterogeneous stack will not survive the trip.
    assert!(
        d["provenance_limitations"]
            .as_array()
            .expect("provenance_limitations is a list")
            .iter()
            .any(|l| l.as_str().unwrap_or("").contains("technology")),
        "one-technology-per-database is a real limit and must be stated in --describe"
    );
}

#[test]
fn a_missing_output_path_is_refused_rather_than_half_done() {
    let out = Command::new(bin())
        .args(["read-3dblox", "--input", DBX])
        .output()
        .expect("run read-3dblox");
    assert!(!out.status.success(), "no --output should be an error");
    assert!(String::from_utf8_lossy(&out.stderr).contains("--output"));
}
