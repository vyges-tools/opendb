// SPDX-License-Identifier: Apache-2.0
//! The LEF58 PRL two-sides cut spacing pre-screen on a synthetic design
//! (`tests/fixtures/prl_two_sides/`, every name and value invented). No reference checks this rule
//! (odb builds none from it), so the expectations follow the LEF/DEF 5.8 reference text and its
//! Figure 2-155, whose rule V1 states: `PRLTWOSIDESSPACING 0.15 0.2 CUTCLASS VA TO VA PRL -0.07
//! -0.05 -0.1 -0.08`.
//!
//! * `p1`, `p2`: two VA cuts 0.09 apart in x and 0.06 in y (the figure's case a), centres 0.164
//!   apart >= 0.15: clean.
//! * `q1`, `q2`: 0.05 apart both ways, out of the PRL ranges, centres 0.127 apart < 0.2: a finding.
//! * `r`: one net, two VA cuts in a row 0.1 apart (PRL 0.04 in y): < 0.2, a finding (no same-net
//!   exemption in the text).
//! * `b1`, `b2`: a VB cut beside a VA cut: VB is not named by the rule, no pair.
//! * `vdd`: a special-wire VA cut alone: collected, no pair.
#![cfg(unix)]

use std::process::Command;
use vyges_opendb::prl_two_sides::{self, NC_CLASS};
use vyges_opendb::Db;

fn fixture(f: &str) -> String {
    format!("{}/tests/fixtures/prl_two_sides/{f}", env!("CARGO_MANIFEST_DIR"))
}

fn load(with_def: bool) -> Db {
    let mut db = Db::new();
    db.read_lef(fixture("prl.lef")).unwrap();
    if with_def {
        db.read_def(fixture("prl.def"), "default").unwrap();
    }
    db
}

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vyges-opendb"))
}

/// The rules come from the layer's raw property text, under either spelling of the property; the
/// classes from odb's cut classes; an undefined class is named.
#[test]
fn rules_are_read_from_the_layer_text() {
    let rules = prl_two_sides::read_rules(&load(false)).unwrap();
    let v1 = rules.iter().find(|l| l.cut_layer == "V1").expect("V1 states rules");
    assert_eq!(v1.statements, 2);
    let got: Vec<(usize, i32, i32, [i32; 4])> = v1.rules.iter().map(|r| (r.index, r.prl_spacing, r.non_prl_spacing, r.prl)).collect();
    assert_eq!(got, vec![(0, 150, 200, [-70, -50, -100, -80])]);
    assert_eq!(v1.rules[0].class1, ("VA".to_string(), 40, 40));
    assert_eq!(v1.not_checked.iter().map(|n| n.clause).collect::<Vec<_>>(), [NC_CLASS]);
    let v2 = rules.iter().find(|l| l.cut_layer == "V2").expect("V2 states a rule (one-S spelling)");
    assert_eq!((v2.statements, v2.rules.len()), (1, 1));
    assert_eq!(rules.len(), 2, "M1, M2, M3 are routing layers, never read");
}

/// Every case of the design, finding by finding.
#[test]
fn each_case_fires_or_stays_clean() {
    let rep = prl_two_sides::check_prl_two_sides(&load(true)).unwrap();
    assert!(rep.has_block);
    let v1 = rep.layers.iter().find(|l| l.rules.cut_layer == "V1").unwrap();
    assert_eq!((v1.cuts, v1.cuts_checked), (9, 8), "eight routed cuts and the special one; b1 is VB");
    let mut got: Vec<String> = rep
        .findings
        .iter()
        .map(|f| format!("{}-{} prl={:?} parallel={} <{}", f.nets.0, f.nets.1, f.prl, f.parallel, f.required))
        .collect();
    got.sort();
    assert_eq!(got, vec!["q1-q2 prl=(-50, -50) parallel=false <200", "r-r prl=(-60, 40) parallel=false <200"]);
}

/// Without a block, only the rules are read: nothing collected, nothing found.
#[test]
fn tech_only_reads_rules_and_checks_nothing() {
    let rep = prl_two_sides::check_prl_two_sides(&load(false)).unwrap();
    assert!(!rep.has_block);
    assert_eq!(rep.rules_checked(), 2);
    assert!(rep.findings.is_empty());
    assert!(rep.layers.iter().all(|l| l.cuts == 0 && l.cuts_checked == 0));
}

/// The command, judged by exit code: findings fail it under --fail-on-findings and not without;
/// the JSON names the status and the clause of each rule not checked; a tech without the property
/// is its own status, never "checked"; our diag lines never carry the rule text.
#[test]
fn the_command_reports_by_exit_code() {
    let run = |extra: &[&str]| {
        let out = bin()
            .args(["check-prl-two-sides", "--lef", &fixture("prl.lef")])
            .args(extra)
            .output()
            .expect("run vyges-opendb");
        (out.status.code(), String::from_utf8_lossy(&out.stdout).to_string(), String::from_utf8_lossy(&out.stderr).to_string())
    };
    let def = fixture("prl.def");
    let (code, json, err) = run(&["--def", &def, "--fail-on-findings"]);
    assert_eq!(code, Some(1), "{err}");
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["status"], "checked");
    assert_eq!(v["rules_checked"], 2);
    assert_eq!(v["rules_not_checked"][NC_CLASS], 1);
    assert_eq!(v["findings"].as_array().unwrap().len(), 2);
    let ours: Vec<&str> = err.lines().filter(|l| l.contains("check-prl-two-sides:") && !l.contains("finding(s)")).collect();
    assert_eq!(ours.len(), 1, "{err}");
    assert!(ours.iter().all(|l| !l.contains("PRLTWOSIDESSPACING 0.") && !l.contains("TO V")), "{ours:?}");
    let (code, _, _) = run(&["--def", &def]);
    assert_eq!(code, Some(0));
    let (code, json, _) = run(&["--fail-on-findings"]);
    assert_eq!(code, Some(0));
    assert_eq!(serde_json::from_str::<serde_json::Value>(&json).unwrap()["status"], "rules_read");
    let plain = format!("{}/tests/fixtures/masks/masks.lef", env!("CARGO_MANIFEST_DIR"));
    let out = bin().args(["check-prl-two-sides", "--lef", &plain]).output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()["status"], "no_rules");
    let out = bin().args(["check-prl-two-sides"]).output().unwrap();
    assert_ne!(out.status.code(), Some(0));
}
