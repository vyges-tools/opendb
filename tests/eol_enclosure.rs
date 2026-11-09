// SPDX-License-Identifier: Apache-2.0
//! The LEF58 EOL-enclosure pre-screen on a synthetic design (`tests/fixtures/eol_enclosure/`, every
//! name and value invented). No reference checks these rules (odb builds none from them), so the
//! expectations follow the LEF/DEF 5.8 reference text and figures, laid out as wires. V1 rules:
//! r0 `EOLENCLOSURE 0.1 CUTCLASS VB SHORTEDGEONLY 0.02` (both metals), r1 `… VA ABOVE 0.03`
//! (M2), r2 `EOLENCLOSURE 0.07 CUTCLASS VA BELOW 0.02 0.0` (M1, exact 0).
//!
//! * `vbok`: a vertical VB cut in a through M2 wire, on an M1 wire with a 140-tall pad: clean.
//! * `vbend`: the M2 wire ends 10 past the cut's short N edge, its 60-wide end an EOL edge: r0.
//! * `vbpad`: a VB via alone, its M2 pad 5 past N and S and 10 past W and E: the short edges fail
//!   r0, the long ones (also onto EOL edges) are not measured under SHORTEDGEONLY.
//! * `vaend`: the M2 wire ends 20 past a VA cut: r1. On M1 the wire ends flush: exact 0, clean.
//! * `vaexact`: the M1 wire ends 10 past the cut: neither 0 nor 20, r2.
//! * `vawide`: the M1 end 10 past the cut is 80 wide, not under the EOL width 70: clean.
//! * `out`: a via whose M2 pad covers half its cut: not enclosed on M2, nothing measured there.
//! * `vdd`: a special-wire cut in a stripe as wide as the cut, its M1 pad the cut's size: every
//!   M1 edge is a 40-wide EOL edge at 0 (the exact overhang), the stripe's sides are long: clean.
#![cfg(unix)]

use std::process::Command;
use vyges_opendb::eol_enclosure::{self, EdgeKind, EolFinding, NC_CLASS, NC_MINEOLWIDTH};
use vyges_opendb::Db;

fn fixture(f: &str) -> String {
    format!("{}/tests/fixtures/eol_enclosure/{f}", env!("CARGO_MANIFEST_DIR"))
}

fn load(with_def: bool) -> Db {
    let mut db = Db::new();
    db.read_lef(fixture("eol.lef")).unwrap();
    if with_def {
        db.read_def(fixture("eol.def"), "default").unwrap();
    }
    db
}

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vyges-opendb"))
}

/// The rules come from the layer's raw property text, sides resolved to the neighbouring routing
/// layers and class sizes from odb's cut classes; MINEOLWIDTH and an undefined class are named.
#[test]
fn rules_are_read_from_the_layer_text() {
    let rules = eol_enclosure::read_rules(&load(false)).unwrap();
    let v1 = rules.iter().find(|l| l.cut_layer == "V1").expect("V1 states rules");
    assert_eq!((v1.below_layer.as_deref(), v1.above_layer.as_deref()), (Some("M1"), Some("M2")));
    assert_eq!(v1.statements, 4);
    type Got = (usize, Vec<String>, EdgeKind, i32, i32, Option<i32>);
    let got: Vec<Got> =
        v1.rules.iter().map(|r| (r.index, r.metals.clone(), r.edges, r.eol_width, r.overhang, r.exact)).collect();
    let m = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(got, vec![
        (0, m(&["M1", "M2"]), EdgeKind::Short, 100, 20, None),
        (1, m(&["M2"]), EdgeKind::All, 100, 30, None),
        (2, m(&["M1"]), EdgeKind::All, 70, 20, Some(0)),
    ]);
    assert_eq!(v1.rules[0].class, Some(("VB".into(), 40, 80)));
    assert_eq!(v1.not_checked.iter().map(|n| n.clause).collect::<Vec<_>>(), [NC_MINEOLWIDTH]);
    let v2 = rules.iter().find(|l| l.cut_layer == "V2").expect("V2 states a rule");
    assert!(v2.rules.is_empty());
    assert_eq!(v2.not_checked.iter().map(|n| n.clause).collect::<Vec<_>>(), [NC_CLASS]);
    assert_eq!(rules.len(), 2, "M1, M2, M3 are routing layers, never read");
}

/// Every case of the design, finding by finding.
#[test]
fn each_case_fires_or_stays_clean() {
    let rep = eol_enclosure::check_eol_enclosures(&load(true)).unwrap();
    assert!(rep.has_block);
    let v1 = rep.layers.iter().find(|l| l.rules.cut_layer == "V1").unwrap();
    assert_eq!(v1.cuts, 8, "seven nets and the special net");
    assert_eq!(v1.cuts_checked.get("M1"), Some(&8));
    assert_eq!(v1.cuts_checked.get("M2"), Some(&8));
    let mut got: Vec<String> = rep
        .findings
        .iter()
        .map(|f| match f {
            EolFinding::Overhang { net, metal, rule, edge, overhang, required, .. } => {
                format!("{net} {metal} r{rule} {}={overhang}<{required}", edge.as_str())
            }
            EolFinding::NotEnclosed { net, metal, .. } => format!("{net} {metal} not_enclosed"),
        })
        .collect();
    got.sort();
    assert_eq!(got, vec![
        "out M2 not_enclosed",
        "vaend M2 r1 N=20<30",
        "vaexact M1 r2 E=10<20",
        "vbend M2 r0 N=10<20",
        "vbpad M2 r0 N=5<20",
        "vbpad M2 r0 S=5<20",
    ]);
}

/// Without a block, only the rules are read: nothing collected, nothing found.
#[test]
fn tech_only_reads_rules_and_checks_nothing() {
    let rep = eol_enclosure::check_eol_enclosures(&load(false)).unwrap();
    assert!(!rep.has_block);
    assert_eq!(rep.rules_checked(), 3);
    assert!(rep.findings.is_empty());
    assert!(rep.layers.iter().all(|l| l.cuts == 0 && l.cuts_checked.is_empty()));
}

/// The command, judged by exit code: findings fail it under --fail-on-findings and not without;
/// the JSON names the status and the clause of each rule not checked; a tech without the property
/// is its own status, never "checked"; our diag lines never carry the rule text.
#[test]
fn the_command_reports_by_exit_code() {
    let run = |extra: &[&str]| {
        let out = bin()
            .args(["check-eol-enclosure", "--lef", &fixture("eol.lef")])
            .args(extra)
            .output()
            .expect("run vyges-opendb");
        (out.status.code(), String::from_utf8_lossy(&out.stdout).to_string(), String::from_utf8_lossy(&out.stderr).to_string())
    };
    let def = fixture("eol.def");
    let (code, json, err) = run(&["--def", &def, "--fail-on-findings"]);
    assert_eq!(code, Some(1), "{err}");
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["status"], "checked");
    assert_eq!(v["rules_checked"], 3);
    assert_eq!(v["rules_not_checked"][NC_MINEOLWIDTH], 1);
    assert_eq!(v["rules_not_checked"][NC_CLASS], 1);
    assert_eq!(v["findings"].as_array().unwrap().len(), 6);
    assert_eq!(v["findings_by_kind"]["eol_enclosure"], 5);
    let ours: Vec<&str> = err.lines().filter(|l| l.contains("check-eol-enclosure:") && !l.contains("finding(s)")).collect();
    assert_eq!(ours.len(), 2, "{err}");
    assert!(ours.iter().all(|l| !l.contains("EOLENCLOSURE 0.") && !l.contains("CUTCLASS V")), "{ours:?}");
    let (code, _, _) = run(&["--def", &def]);
    assert_eq!(code, Some(0));
    let (code, json, _) = run(&["--fail-on-findings"]);
    assert_eq!(code, Some(0));
    assert_eq!(serde_json::from_str::<serde_json::Value>(&json).unwrap()["status"], "rules_read");
    let plain = format!("{}/tests/fixtures/masks/masks.lef", env!("CARGO_MANIFEST_DIR"));
    let out = bin().args(["check-eol-enclosure", "--lef", &plain]).output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()["status"], "no_rules");
    let out = bin().args(["check-eol-enclosure"]).output().unwrap();
    assert_ne!(out.status.code(), Some(0));
}
