// SPDX-License-Identifier: Apache-2.0
//! The LEF58 enclosure-edge pre-screen on a synthetic design (`tests/fixtures/enclosure_edge/`,
//! every name and value invented). No reference checks these rules (odb builds none from them), so
//! the expectations are the LEF/DEF 5.8 reference text's own examples, laid out as wires:
//!
//! * `ok`: a cut in a straight M2 wire, 0 on W and E, the wire running on both ways: clean.
//! * `opp`: the wire stops flush with the cut's N edge (0) and runs on to the S: N meets the
//!   overhang and the whole S edge reaches past it — OPPOSITE (the reference's example b).
//! * `cc`: as `ok`, plus a leg to the E whose top is 40 below the cut: the S edge, extended by the
//!   overhang (20) past W and E, ends 40 from the concave corner, under NOCONCAVECORNER 50
//!   (the reference's example a). `far`: the same leg 60 below: clean.
//! * `vb`: a class-VB cut (rule overhang 10) on a pad reaching 10 W and 20 E, flush N: W and N
//!   each meet the overhang with the opposite edge reaching past it — two findings.
//! * `out`: a via whose M2 pad covers half its cut: reported as not enclosed, nothing measured.
//! * `vdd`: a special-wire cut (a DEF via) in a straight stripe: collected and clean.
#![cfg(unix)]

use std::process::Command;
use vyges_opendb::enclosure_edge::{self, CutEdge, EeFinding, NC_CLASS, NC_WRONGDIRECTION};
use vyges_opendb::Db;

fn fixture(f: &str) -> String {
    format!("{}/tests/fixtures/enclosure_edge/{f}", env!("CARGO_MANIFEST_DIR"))
}

fn load(with_def: bool) -> Db {
    let mut db = Db::new();
    db.read_lef(fixture("ee.lef")).unwrap();
    if with_def {
        db.read_def(fixture("ee.def"), "default").unwrap();
    }
    db
}

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vyges-opendb"))
}

/// The rules come from the layer's raw property text: V1 has two checked and one not (WRONGDIRECTION),
/// V2 names a class it does not define; the class sizes come from odb's cut classes.
#[test]
fn rules_are_read_from_the_layer_text() {
    let rules = enclosure_edge::read_rules(&load(false)).unwrap();
    let v1 = rules.iter().find(|l| l.cut_layer == "V1").expect("V1 states rules");
    assert_eq!(v1.above_layer.as_deref(), Some("M2"));
    assert_eq!(v1.statements, 3);
    type Got = (Option<(String, i32, i32)>, i32, Option<i32>);
    let got: Vec<Got> = v1.rules.iter().map(|r| (r.class.clone(), r.overhang, r.within)).collect();
    assert_eq!(got, vec![(Some(("VA".into(), 40, 40)), 20, Some(50)), (Some(("VB".into(), 40, 80)), 10, None)]);
    assert_eq!(v1.not_checked.iter().map(|n| n.clause).collect::<Vec<_>>(), [NC_WRONGDIRECTION]);
    let v2 = rules.iter().find(|l| l.cut_layer == "V2").expect("V2 states a rule");
    assert!(v2.rules.is_empty());
    assert_eq!(v2.not_checked.iter().map(|n| n.clause).collect::<Vec<_>>(), [NC_CLASS]);
    assert_eq!(rules.len(), 2, "M1, M2, M3 are routing layers, never read");
}

/// Every case of the design, finding by finding.
#[test]
fn each_case_fires_or_stays_clean() {
    let rep = enclosure_edge::check_enclosure_edges(&load(true)).unwrap();
    assert!(rep.has_block);
    let v1 = rep.layers.iter().find(|l| l.rules.cut_layer == "V1").unwrap();
    assert_eq!((v1.cuts, v1.cuts_checked), (7, 7), "six nets and the special net, each class covered");
    let mut got: Vec<String> = rep
        .findings
        .iter()
        .map(|f| match f {
            EeFinding::Opposite { net, rule, edge, overhang, opposite, opposite_max, limit, .. } => {
                format!("{net} opposite r{rule} {}={overhang} {}>{limit}:{}", edge.as_str(), opposite.as_str(), *opposite_max > *limit)
            }
            EeFinding::ConcaveCorner { net, rule, corner, within, .. } => format!("{net} corner r{rule} {corner:?} <{within}"),
            EeFinding::NotEnclosed { net, .. } => format!("{net} not_enclosed"),
        })
        .collect();
    got.sort();
    assert_eq!(got, vec![
        "cc corner r0 (520, 440) <50",
        "opp opposite r0 N=0 S>20:true",
        "out not_enclosed",
        "vb opposite r1 N=0 S>10:true",
        "vb opposite r1 W=10 E>10:true",
    ]);
    // the measured values of the vb pad: 10 W, 20 E
    let w = rep.findings.iter().find_map(|f| match f {
        EeFinding::Opposite { net, edge: CutEdge::W, opposite_max, .. } if net == "vb" => Some(*opposite_max),
        _ => None,
    });
    assert_eq!(w, Some(20));
}

/// Without a block, only the rules are read: nothing collected, nothing found.
#[test]
fn tech_only_reads_rules_and_checks_nothing() {
    let rep = enclosure_edge::check_enclosure_edges(&load(false)).unwrap();
    assert!(!rep.has_block);
    assert_eq!(rep.rules_checked(), 2);
    assert!(rep.findings.is_empty());
    assert!(rep.layers.iter().all(|l| l.cuts == 0 && l.cuts_checked == 0));
}

/// The command, judged by exit code: findings fail it under --fail-on-findings and not without;
/// the JSON names the status and the clause of each rule not checked; a tech without the property
/// is its own status, never "checked".
#[test]
fn the_command_reports_by_exit_code() {
    let run = |extra: &[&str]| {
        let out = bin()
            .args(["check-enclosure-edge", "--lef", &fixture("ee.lef")])
            .args(extra)
            .output()
            .expect("run vyges-opendb");
        (out.status.code(), String::from_utf8_lossy(&out.stdout).to_string(), String::from_utf8_lossy(&out.stderr).to_string())
    };
    let def = fixture("ee.def");
    let (code, json, err) = run(&["--def", &def, "--fail-on-findings"]);
    assert_eq!(code, Some(1), "{err}");
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["status"], "checked");
    assert_eq!(v["rules_checked"], 2);
    assert_eq!(v["rules_not_checked"][NC_WRONGDIRECTION], 1);
    assert_eq!(v["rules_not_checked"][NC_CLASS], 1);
    assert_eq!(v["findings"].as_array().unwrap().len(), 5);
    // odb's own LEF reader echoes an unsupported property's text (ODB-0388); our lines do not
    let ours: Vec<&str> = err.lines().filter(|l| l.contains("check-enclosure-edge:") && !l.contains("finding(s)")).collect();
    assert_eq!(ours.len(), 2, "{err}");
    assert!(ours.iter().all(|l| !l.contains("ENCLOSUREEDGE CUTCLASS")), "{ours:?}");
    let (code, _, _) = run(&["--def", &def]);
    assert_eq!(code, Some(0));
    let (code, json, _) = run(&["--fail-on-findings"]);
    assert_eq!(code, Some(0));
    assert_eq!(serde_json::from_str::<serde_json::Value>(&json).unwrap()["status"], "rules_read");
    // a technology with no ENCLOSUREEDGE property at all
    let plain = format!("{}/tests/fixtures/masks/masks.lef", env!("CARGO_MANIFEST_DIR"));
    let out = bin().args(["check-enclosure-edge", "--lef", &plain]).output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()["status"], "no_rules");
    // bad arguments are an error, not a clean report
    let out = bin().args(["check-enclosure-edge"]).output().unwrap();
    assert_ne!(out.status.code(), Some(0));
}
