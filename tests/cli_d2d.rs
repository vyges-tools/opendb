// SPDX-License-Identifier: Apache-2.0
//! `check-d2d` through the shipped binary.
//!
//! The point of this command is the gap measured in `src/d2d.rs`: upstream's `check_3dblox`
//! matches bumps on exact integer-DBU equality and `continue`s past anything without a
//! counterpart, so an unmated bump, a 1 nm misalignment and a 5 µm misalignment all report
//! **zero violations**. These assert that the same interfaces come back non-zero here, named.
#![cfg(feature = "gen-write")]

use std::process::Command;

const TOP: &str = "tests/fixtures/d2d/top.bmap";
const GOOD: &str = "tests/fixtures/d2d/good.bmap";
const BAD: &str = "tests/fixtures/d2d/defective.bmap";

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_vyges-opendb")
}

fn run(args: &[&str]) -> (bool, serde_json::Value, String) {
    let out = Command::new(bin())
        .arg("check-d2d")
        .args(args)
        .output()
        .expect("run check-d2d");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let json = serde_json::from_str(stdout.trim()).unwrap_or(serde_json::Value::Null);
    (out.status.success(), json, String::from_utf8_lossy(&out.stderr).to_string())
}

#[test]
fn a_matching_interface_reports_clean() {
    let (ok, j, err) = run(&["--top", TOP, "--bottom", GOOD]);
    assert!(ok, "{err}");
    assert_eq!(j["violations"], 0, "{j}");
    assert_eq!(j["matched"], 4);
}

#[test]
fn the_defects_upstream_reports_zero_for_are_all_named() {
    let (ok, j, err) = run(&["--top", TOP, "--bottom", BAD]);
    assert!(ok, "{err}");
    assert!(j["violations"].as_u64().unwrap() > 0);

    let kinds = &j["by_kind"];
    assert_eq!(kinds["unmated"], 1, "a bump with no mate is dead silicon");
    assert_eq!(kinds["misaligned"], 1, "1 nm off is a misalignment, not two orphans");
    assert_eq!(kinds["net-mismatch"], 2, "swapped signals are two wrong bumps");
    assert_eq!(kinds["cell-mismatch"], 1, "a C4 against a microbump");

    // Every finding has to name the bump, or it is not actionable.
    for f in j["findings"].as_array().unwrap() {
        let m = f["message"].as_str().unwrap();
        assert!(m.contains("bt") || m.contains("bb"), "unlocatable finding: {m}");
    }
}

#[test]
fn the_transform_is_reported_so_clean_means_something() {
    // "No violations" is meaningless without knowing what frame it was computed in.
    let (_, j, _) = run(&["--top", TOP, "--bottom", GOOD, "--offset-x", "3.5", "--flip-x"]);
    assert_eq!(j["transform"]["dx_um"], 3.5);
    assert_eq!(j["transform"]["flip_x"], true);
}

#[test]
fn an_offset_that_is_needed_and_missing_shows_up_as_a_dead_interface() {
    // The realistic mistake: forgetting the placement offset. It must not look clean.
    let shifted = std::env::temp_dir().join("cli_d2d_shifted.bmap");
    let text = std::fs::read_to_string(GOOD)
        .unwrap()
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| {
            let t: Vec<&str> = l.split_whitespace().collect();
            let x: f64 = t[2].parse::<f64>().unwrap() + 500.0;
            format!("{} {} {} {} {} {}\n", t[0], t[1], x, t[3], t[4], t[5])
        })
        .collect::<String>();
    std::fs::write(&shifted, text).unwrap();
    let p = shifted.to_str().unwrap();

    let (_, without, _) = run(&["--top", TOP, "--bottom", p]);
    assert_eq!(without["violations"], 8, "4 top + 4 bottom bumps, none mating");

    let (_, with, _) = run(&["--top", TOP, "--bottom", p, "--offset-x", "-500"]);
    assert_eq!(with["violations"], 0, "with the offset it is the same interface");
}

#[test]
fn the_tolerance_says_where_it_came_from() {
    let (_, derived, _) = run(&["--top", TOP, "--bottom", GOOD]);
    assert_eq!(derived["tolerance_source"], "derived from bump pitch");

    let (_, given, _) = run(&["--top", TOP, "--bottom", GOOD, "--tolerance", "0.5"]);
    assert_eq!(given["tolerance_source"], "specified");
    assert_eq!(given["tolerance_um"], 0.5);
}

#[test]
fn a_missing_file_is_an_error_not_a_clean_report() {
    // The worst possible outcome: a typo'd path reported as an interface with no violations.
    let (ok, _, err) = run(&["--top", TOP, "--bottom", "/nonexistent.bmap"]);
    assert!(!ok);
    assert!(err.contains("nonexistent.bmap"), "{err}");
}

#[test]
fn a_missing_argument_is_refused() {
    let (ok, _, err) = run(&["--top", TOP]);
    assert!(!ok);
    assert!(err.contains("--bottom"));
}

#[test]
fn the_command_describes_itself() {
    let out = Command::new(bin())
        .args(["check-d2d", "--describe"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let d: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(d["name"], "check-d2d");
    assert_eq!(d["invocation"]["emits_json"], true);
    // That the transform is not inferred is the single most important caveat: a checker that
    // guessed an alignment and then called everything matched would be worse than none.
    assert!(d["provenance_limitations"]
        .as_array()
        .unwrap()
        .iter()
        .any(|l| l.as_str().unwrap().contains("NOT inferred")));
}
