// SPDX-License-Identifier: Apache-2.0
//! `check-3d-nets` through the shipped binary.
//!
//! The claim under test is narrow and load-bearing: a stack whose every bond is perfectly mated
//! and correctly netted can still fail to carry a signal, and nothing else reports it. So the
//! severed case is asserted against its own control — the same fixture with `tsv: true` — and,
//! where the build can construct a database, against what upstream's own linter says about the
//! very same assembly.

use std::process::Command;

const SEVERED: &str = "tests/fixtures/3dblox/nets/stack_notsv.3dbx";
const THROUGH: &str = "tests/fixtures/3dblox/nets/stack_tsv.3dbx";

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_vyges-opendb")
}

fn run(args: &[&str]) -> (bool, serde_json::Value, String) {
    let out = Command::new(bin())
        .arg("check-3d-nets")
        .args(args)
        .output()
        .expect("run check-3d-nets");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let json = serde_json::from_str(stdout.trim()).unwrap_or(serde_json::Value::Null);
    (out.status.success(), json, String::from_utf8_lossy(&out.stderr).to_string())
}

#[test]
fn a_net_that_cannot_cross_a_die_without_tsvs_fails_the_run() {
    let (ok, j, err) = run(&["--input", SEVERED]);
    assert!(!ok, "a severed net must exit non-zero so CI fails: {err}");
    assert_eq!(j["violations"], 1, "{j}");
    assert_eq!(j["by_kind"]["severed"], 1);

    let f = &j["findings"][0];
    assert_eq!(f["kind"], "severed");
    assert_eq!(f["net"], "n_thru");
    assert_eq!(f["chip_inst"], "u_mid", "the finding has to name the die that cannot pass it");
    assert_eq!(f["tsv"], false);
    // Both bonds were genuinely checked and both mated — the point being that a clean interface
    // report is not a connected stack.
    assert_eq!(j["interfaces_checked"], 2);
    for b in j["bonds"].as_array().unwrap() {
        assert!(b["matched"].as_u64().unwrap() > 0, "{b}");
    }
}

#[test]
fn the_same_stack_with_tsvs_is_clean() {
    // The control. Same bump maps, same placements, one flag different.
    let (ok, j, err) = run(&["--input", THROUGH]);
    assert!(ok, "{err} {j}");
    assert_eq!(j["violations"], 0, "{j}");
    assert_eq!(j["nets"], 2);
    assert_eq!(j["bumps"], 6);
}

#[test]
fn a_net_that_terminates_on_a_die_is_not_reported() {
    // `n_local` runs base -> mid and stops, in both fixtures. If this were flagged the checker
    // would be useless on any real stack, because most interface nets do exactly this.
    for input in [SEVERED, THROUGH] {
        let (_, j, _) = run(&["--input", input]);
        let hits: Vec<String> = j["findings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|f| f["net"] == "n_local")
            .map(|f| f["message"].as_str().unwrap_or_default().to_string())
            .collect();
        assert!(hits.is_empty(), "{input}: {hits:?}");
    }
}

#[test]
fn upstreams_own_example_reports_nothing_because_nothing_bonded_carries_bumps() {
    // The regression that shaped the whole design. example.3dbx instantiates one chiplet twice, and
    // its only bump map sits on a region no connection bonds. Grouping bumps by net name globally
    // made every VDD, VSS and soc_io[n] look like a net in two pieces — 38 violations on an
    // assembly where not one interface was checked. A netName belongs to its own die's netlist.
    let (ok, j, err) = run(&["--input", "tests/fixtures/3dblox/example.3dbx"]);
    assert!(ok, "{err}");
    assert_eq!(j["violations"], 0, "{j}");
    assert_eq!(j["bumps"], 128, "the bumps were still read");
    assert_eq!(j["interfaces_checked"], 0);
    assert_eq!(
        j["interfaces_skipped"].as_array().unwrap().len(),
        2,
        "and both bonds are named as unchecked rather than passing for clean"
    );
}

#[test]
fn the_report_says_where_its_net_names_came_from() {
    // A continuity verdict is uninterpretable without it: nets read from bump maps and nets read
    // from a netlist can disagree, and a reader has to know which produced the answer.
    let (_, j, _) = run(&["--input", THROUGH]);
    assert_eq!(j["net_source"], "bump maps");
    assert_eq!(j["tsv_inference"], true);
}

#[test]
fn tsv_inference_can_be_turned_off_and_the_report_says_so() {
    // Joining a die's two faces by matching net name is a convention, not a standard. A user whose
    // maps do not follow it must be able to get the conservative answer.
    let (ok, j, _) = run(&["--input", THROUGH, "--no-tsv-inference"]);
    assert!(!ok, "with the inference off the through-path is not assumed");
    assert_eq!(j["tsv_inference"], false);
    assert_eq!(j["by_kind"]["severed"], 1);
    // And the die that declared TSVs is now reported as unused — which is informational, so it
    // must not be what made the run fail.
    assert_eq!(j["by_kind"]["tsv-unused"], 1);
    assert_eq!(j["violations"], 1, "tsv-unused is not a violation: {j}");
}

#[test]
fn a_database_is_refused_with_the_reason_rather_than_checked_emptily() {
    // The failure this guards against is the worst kind: a database we built carries no chip nets,
    // so a database-driven check would report every stack clean.
    let (ok, _, err) = run(&["--input", "some/stack.odb"]);
    assert!(!ok);
    assert!(err.contains(".3dbx"), "{err}");
    assert!(err.contains("setNet"), "the reason has to name what is missing: {err}");
}

#[test]
fn the_step_describes_itself_and_admits_what_it_infers() {
    let out = Command::new(bin())
        .args(["check-3d-nets", "--describe"])
        .output()
        .expect("describe");
    let j: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("--describe emits JSON");
    assert_eq!(j["name"], "check-3d-nets");
    let limits = j["provenance_limitations"].as_array().unwrap();
    assert!(
        limits.iter().any(|l| l.as_str().unwrap().contains("net names matching")),
        "the TSV inference is a convention and the descriptor must say so: {limits:?}"
    );
}

/// What upstream's own linter says about the identical assembly.
///
/// This is the whole argument for the command existing, so it is measured rather than asserted
/// from reading source. Needs `gen-write` because it has to build the database first.
#[test]
#[cfg(feature = "gen-write")]
fn upstream_reports_the_severed_stack_as_clean() {
    let odb = std::env::temp_dir().join("nets3d_severed.odb");
    let odb = odb.to_str().unwrap();
    let read = Command::new(bin())
        .args(["read-3dblox", "--input", SEVERED, "--output", odb])
        .output()
        .expect("read-3dblox");
    assert!(
        read.status.success(),
        "{}",
        String::from_utf8_lossy(&read.stderr)
    );

    let lint = Command::new(bin())
        .args(["check-3dblox", "--input", odb])
        .output()
        .expect("check-3dblox");
    let j: serde_json::Value =
        serde_json::from_slice(&lint.stdout).unwrap_or(serde_json::Value::Null);
    assert_eq!(
        j["violations"], 0,
        "upstream's structural lint has nothing to say about a net that cannot cross a die: {j}"
    );

    // Ours, on the same assembly.
    let (ok, mine, _) = run(&["--input", SEVERED]);
    assert!(!ok);
    assert_eq!(mine["by_kind"]["severed"], 1);
}
