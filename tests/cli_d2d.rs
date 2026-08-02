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
    let (ok, j, _err) = run(&["--top", TOP, "--bottom", BAD]);
    assert!(!ok, "violations exit non-zero so CI fails");
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

// ── Assembly mode: the frame comes from the file ────────────────────────────────────────────

const STACK: &str = "tests/fixtures/3dblox/d2d/stack.3dbx";

/// Copy the fixture assembly to a scratch dir so a test can perturb it without touching the
/// checked-in one. Bump maps are resolved relative to the `.3dbv`, so the whole directory moves.
fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("d2d_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for f in std::fs::read_dir("tests/fixtures/3dblox/d2d").unwrap() {
        let f = f.unwrap().path();
        if f.is_file() {
            std::fs::copy(&f, dir.join(f.file_name().unwrap())).unwrap();
        }
    }
    dir
}

#[test]
fn an_assembly_is_checked_without_any_geometry_on_the_command_line() {
    // The whole point of reading the .3dbx: no --offset, no --flip-x, nothing to get wrong.
    let (ok, j, err) = run(&["--input", STACK]);
    assert!(ok, "{err}");
    assert_eq!(j["violations"], 0, "{j}");
    assert_eq!(j["interfaces_checked"], 1);

    let i = &j["interfaces"][0];
    assert_eq!(i["connection"], "d2d_bond");
    assert_eq!(i["top"], "u_mem.front");
    assert_eq!(i["bottom"], "u_logic.front");
    assert_eq!(i["matched"], 4);
    // The frame has to be reported, and it has to name the placements it came from.
    let frame = i["frame"].as_str().unwrap();
    assert!(frame.contains("MZ_MY") && frame.contains("R0"), "{frame}");
}

#[test]
fn a_defect_in_a_bump_map_reaches_the_assembly_level_report() {
    let dir = scratch("broken");
    std::fs::copy(dir.join("mem_front_broken.bmap"), dir.join("mem_front.bmap")).unwrap();
    let (ok, j, _err) = run(&["--input", dir.join("stack.3dbx").to_str().unwrap()]);
    assert!(!ok, "violations exit non-zero");
    assert_eq!(j["violations"], 5, "and the report is still printed");
    let k = &j["interfaces"][0]["by_kind"];
    assert_eq!(k["unmated"], 1);
    assert_eq!(k["misaligned"], 1);
    assert_eq!(k["net-mismatch"], 2);
    assert_eq!(k["cell-mismatch"], 1);
}

#[test]
fn the_wrong_orientation_is_loud_rather_than_silent() {
    // MZ flips the die's face and leaves X alone; MZ_MY also mirrors it. Using MZ where MZ_MY was
    // meant compares two dies in mirrored frames — which is precisely the mistake reading the
    // assembly is meant to make impossible, and it must not look clean when someone writes it.
    let dir = scratch("badorient");
    let p = dir.join("stack.3dbx");
    let text = std::fs::read_to_string(&p).unwrap().replace("orient: MZ_MY", "orient: MZ");
    std::fs::write(&p, text).unwrap();

    let (ok, j, _) = run(&["--input", p.to_str().unwrap()]);
    assert!(!ok, "a reversed interface must fail the job, not just report");
    assert_eq!(j["violations"], 4, "the whole interface should read as reversed");
    assert_eq!(j["interfaces"][0]["by_kind"]["net-mismatch"], 4);
}

#[test]
fn an_orientation_the_mapping_has_not_been_verified_for_is_refused() {
    // odb silently treats an unrecognised orientation as R0. Inheriting that would place a die
    // wrongly and then report the interface clean, so this has to fail loudly instead.
    let dir = scratch("unknownorient");
    let p = dir.join("stack.3dbx");
    let text = std::fs::read_to_string(&p).unwrap().replace("orient: MZ_MY", "orient: SIDEWAYS");
    std::fs::write(&p, text).unwrap();

    let (ok, _, err) = run(&["--input", p.to_str().unwrap()]);
    assert!(!ok, "an unknown orientation must not be silently treated as R0");
    assert!(err.contains("SIDEWAYS"), "{err}");
}

#[test]
fn a_bond_with_no_bump_map_is_listed_as_unchecked_not_as_clean() {
    // The difference between "we looked and found nothing" and "we did not look" is the whole
    // value of the report.
    let dir = scratch("nobmap");
    let p = dir.join("dies.3dbv");
    let text = std::fs::read_to_string(&p).unwrap().replace("        bmap: mem_front.bmap\n", "");
    std::fs::write(&p, text).unwrap();

    let (ok, j, err) = run(&["--input", dir.join("stack.3dbx").to_str().unwrap()]);
    assert!(ok, "{err}");
    assert_eq!(j["interfaces_checked"], 0);
    assert_eq!(j["violations"], 0);
    let skipped = j["interfaces_skipped"].as_array().unwrap();
    assert_eq!(skipped.len(), 1, "the unchecked bond must be named");
    assert!(skipped[0].as_str().unwrap().contains("d2d_bond"));
}

#[test]
fn mixing_the_two_input_forms_is_refused() {
    let (ok, _, err) = run(&["--input", STACK, "--top", TOP]);
    assert!(!ok);
    assert!(err.contains("--top"), "{err}");
}

#[test]
fn a_check_that_finds_something_exits_non_zero() {
    // A sign-off check that always exits 0 cannot gate anything: a CI job goes green over a dead
    // interface, which is the exact failure this command exists to prevent. Every other engine in
    // the suite exits non-zero on a violation; these were the exception until this test.
    let (clean_ok, j, _) = run(&["--top", TOP, "--bottom", GOOD]);
    assert!(clean_ok, "a clean interface must exit 0");
    assert_eq!(j["violations"], 0);

    let (bad_ok, j, _) = run(&["--top", TOP, "--bottom", BAD]);
    assert!(!bad_ok, "violations must exit non-zero so CI fails");
    assert!(j["violations"].as_u64().unwrap() > 0, "and still print the report");
}

#[test]
fn the_assembly_form_gates_too() {
    let (ok, j, _) = run(&["--input", STACK]);
    assert!(ok, "the fixture assembly is clean");
    assert_eq!(j["violations"], 0);

    let dir = scratch("exitcode");
    std::fs::copy(dir.join("mem_front_broken.bmap"), dir.join("mem_front.bmap")).unwrap();
    let (ok, j, _) = run(&["--input", dir.join("stack.3dbx").to_str().unwrap()]);
    assert!(!ok, "a broken interface read from the assembly must also fail the job");
    assert_eq!(j["violations"], 5, "and the report is still on stdout to be parsed");
}
