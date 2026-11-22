// SPDX-License-Identifier: Apache-2.0
//! The two tapeout gates, end to end through the binary on the synthetic two-mask fixture
//! (`tests/fixtures/masks/`, M1 and M2 `MASK 2`, invented values):
//!
//! * `color-masks --require-coloured` REFUSES (exit 3) a design that still has an uncoloured shape
//!   on a multi-mask layer after the pass, and passes one that has none;
//! * `check-gds-layers` REFUSES (exit 3) a layout using a pair outside the allowed list, a drawn
//!   shape on a pair the map sends uncoloured multi-mask shapes to, and (with `--check-map`) a map
//!   targeting a pair outside the list; it says exit 2 when asked to check nothing.
//!
//! Each gate is shown to FIRE on a bad input before it is trusted on a good one.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

fn fixture(f: &str) -> String {
    format!("{}/tests/fixtures/masks/{f}", env!("CARGO_MANIFEST_DIR"))
}

fn tmp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("vyges_opendb_tapeout_{}_{name}", std::process::id()))
}

/// Exit code, stdout, stderr.
fn run(args: &[&str]) -> (i32, String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_vyges-opendb")).env("VYGES_LOG_FORMAT", "text").args(args).output().unwrap();
    (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout).into(), String::from_utf8_lossy(&o.stderr).into())
}

fn s(p: &std::path::Path) -> &str {
    p.to_str().unwrap()
}

/// The fixture design, with its one uncoloured block-pin box (M1) given mask 1 when `pin_mask`.
fn design(name: &str, pin_mask: bool) -> PathBuf {
    let mut def = std::fs::read_to_string(fixture("masks.def")).unwrap();
    if pin_mask {
        let (from, to) = ("+ LAYER M1 ( -50 0 )", "+ LAYER M1 MASK 1 ( -50 0 )");
        assert!(def.contains(from), "fixture changed");
        def = def.replace(from, to);
    }
    let d = tmp(&format!("{name}.def"));
    std::fs::write(&d, def).unwrap();
    let odb = tmp(&format!("{name}.odb"));
    let (rc, _, err) = run(&["import", "--lef", &fixture("masks.lef"), "--def", s(&d), "--output", s(&odb)]);
    assert_eq!(rc, 0, "{err}");
    odb
}

/// Colour the design (exit code, stderr) into `<name>.col.odb`.
fn colour(odb: &std::path::Path, name: &str, require: bool) -> (i32, String, PathBuf) {
    let out = tmp(&format!("{name}.col.odb"));
    let mut a = vec!["color-masks", "--input", s(odb), "--output", s(&out)];
    if require {
        a.push("--require-coloured");
    }
    let (rc, _, err) = run(&a);
    (rc, err, out)
}

fn stream(odb: &std::path::Path, name: &str) -> PathBuf {
    let gds = tmp(&format!("{name}.gds"));
    let cells = format!("{}/tests/fixtures/sky130_fd_sc_hd__inv_1.gds", env!("CARGO_MANIFEST_DIR"));
    let (rc, _, err) = run(&["write-gds", "--input", s(odb), "--map", &fixture("masks.map"), "--gds", &cells, "--output", s(&gds)]);
    assert_eq!(rc, 0, "{err}");
    gds
}

// Rule (tapeout): every shape on a multi-patterned layer carries a mask. The fixture's M1 block
// pin box has none and no track decides one (the pass reports, never forces): refused, exit 3,
// the shape named. With the pin box coloured, the same run passes. Without the flag the run
// reports the shape as a finding and exits 0, as before.
#[test]
fn require_coloured_refuses_an_uncoloured_shape_and_passes_a_coloured_design() {
    let bad = design("bad", false);
    let (rc, err, _) = colour(&bad, "bad", true);
    assert_eq!(rc, 3, "{err}");
    assert!(err.contains("REFUSED: 1 shape(s) on multi-mask layer(s) carry no mask"), "{err}");
    assert!(err.contains("\"layer\":\"M1\""), "the shape is named: {err}");
    let (rc, err, _) = colour(&bad, "bad2", false);
    assert_eq!(rc, 0, "without the gate the finding is reported, not refused: {err}");

    let good = design("good", true);
    let (rc, err, out) = colour(&good, "good", true);
    assert_eq!(rc, 0, "{err}");
    // The gate on the coloured result alone (the flow's re-check) passes too.
    let (rc, _, err) = run(&["color-masks", "--input", s(&out), "--check-only", "--require-coloured"]);
    assert_eq!(rc, 0, "{err}");
}

// Rule (tapeout): a layout uses only the allowed pairs. Listing every pair the layout holds
// passes; leaving one out is refused, exit 3, the pair named.
#[test]
fn a_pair_outside_the_allowed_list_is_refused() {
    let (_, _, col) = colour(&design("pairs", true), "pairs", true);
    let gds = stream(&col, "pairs");
    let (rc, out, err) = run(&["check-gds-layers", "--gds", s(&gds), "--allowed-pairs", "/dev/null"]);
    assert_eq!(rc, 3, "an empty list allows nothing: {err}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let used: Vec<String> = v["pairs"].as_array().unwrap().iter().map(|p| format!("{} {}", p["layer"], p["datatype"])).collect();
    assert!(used.contains(&"10 1".to_string()) && used.contains(&"20 2".to_string()), "{used:?}");
    let all = tmp("pairs.txt");
    std::fs::write(&all, used.join("\n")).unwrap();
    let (rc, _, err) = run(&["check-gds-layers", "--gds", s(&gds), "--allowed-pairs", s(&all)]);
    assert_eq!(rc, 0, "{err}");
    let fewer = tmp("fewer.txt");
    std::fs::write(&fewer, used.iter().filter(|p| *p != "20 2").cloned().collect::<Vec<_>>().join("\n")).unwrap();
    let (rc, _, err) = run(&["check-gds-layers", "--gds", s(&gds), "--allowed-pairs", s(&fewer)]);
    assert_eq!(rc, 3, "{err}");
    assert!(err.contains("pair 20/2 is not on the allowed list") && err.contains("REFUSED"), "{err}");
}

// Rule (tapeout): no drawn shape on a pair the map sends UNCOLOURED shapes of a multi-mask
// layer to (here 10/0 and 20/0: M1 and M2 are MASK 2). The layout of the uncoloured design
// (colouring not required) holds the M1 pin box on 10/0: refused. The coloured design's layout
// holds nothing there: clean. The M3 shapes (30/0) are single-mask and never count.
#[test]
fn an_uncoloured_multi_mask_shape_in_the_layout_is_refused() {
    let (rc, _, col_bad) = colour(&design("ubad", false), "ubad", false);
    assert_eq!(rc, 0);
    let bad = stream(&col_bad, "ubad");
    let args = |g: &PathBuf| vec!["check-gds-layers".to_string(), "--gds".into(), s(g).into(), "--map".into(), fixture("masks.map"), "--lef".into(), fixture("masks.lef")];
    let a = args(&bad);
    let (rc, out, err) = run(&a.iter().map(String::as_str).collect::<Vec<_>>());
    assert_eq!(rc, 3, "{err}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["uncoloured"]["multi_mask_layers"], serde_json::json!(["M1", "M2"]));
    let targets: Vec<(i64, i64)> = v["uncoloured"]["targets"].as_array().unwrap().iter().map(|t| (t["layer"].as_i64().unwrap(), t["datatype"].as_i64().unwrap())).collect();
    assert_eq!(targets, [(10, 0), (20, 0)]);
    assert!(err.contains("pair 10/0 holds 1 uncoloured shape(s) of multi-mask layer(s) M1"), "{err}");

    let (rc, _, col_good) = colour(&design("ugood", true), "ugood", true);
    assert_eq!(rc, 0);
    let good = stream(&col_good, "ugood");
    let a = args(&good);
    let (rc, out, err) = run(&a.iter().map(String::as_str).collect::<Vec<_>>());
    assert_eq!(rc, 0, "{err}\n{out}");
}

// With --check-map, a map entry targeting a pair outside the allowed list is refused even when no
// shape went there (the map would emit it on another design).
#[test]
fn check_map_refuses_a_map_target_outside_the_allowed_list() {
    let (_, _, col) = colour(&design("map", true), "map", true);
    let gds = stream(&col, "map");
    let (_, out, _) = run(&["check-gds-layers", "--gds", s(&gds), "--allowed-pairs", "/dev/null"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let used: Vec<String> = v["pairs"].as_array().unwrap().iter().map(|p| format!("{} {}", p["layer"], p["datatype"])).collect();
    let pairs = tmp("map-pairs.txt");
    std::fs::write(&pairs, used.join("\n")).unwrap();
    let base = ["check-gds-layers", "--gds", s(&gds), "--allowed-pairs", s(&pairs), "--map", &fixture("masks.map"), "--lef", &fixture("masks.lef")];
    let (rc, _, err) = run(&base);
    assert_eq!(rc, 0, "without --check-map the map's unused targets are not judged: {err}");
    let mut with = base.to_vec();
    with.push("--check-map");
    let (rc, _, err) = run(&with);
    assert_eq!(rc, 3, "{err}");
    assert!(err.contains("layer map entry targets a pair not on the allowed list"), "{err}");
}

// A pass word never comes from a run that checked nothing: no check asked for, or a layout with
// no shapes, is exit 2.
#[test]
fn a_run_that_checks_nothing_is_exit_2() {
    let (_, _, col) = colour(&design("none", true), "none", true);
    let gds = stream(&col, "none");
    assert_eq!(run(&["check-gds-layers", "--gds", s(&gds)]).0, 2);
    assert_eq!(run(&["check-gds-layers", "--gds", s(&gds), "--map", &fixture("masks.map")]).0, 2, "a map without the tech");
    let empty = tmp("empty.gds");
    let rec = |t: u8, dt: u8, d: &[u8]| -> Vec<u8> {
        let mut v = ((d.len() + 4) as u16).to_be_bytes().to_vec();
        v.extend([t, dt]);
        v.extend(d);
        v
    };
    let mut b = rec(0x00, 0x02, &600i16.to_be_bytes());
    b.extend(rec(0x01, 0x02, &[0; 24]));
    b.extend(rec(0x04, 0x00, &[]));
    std::fs::write(&empty, b).unwrap();
    assert_eq!(run(&["check-gds-layers", "--gds", s(&empty), "--allowed-pairs", "/dev/null"]).0, 2);
}
