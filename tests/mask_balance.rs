// SPDX-License-Identifier: Apache-2.0
//! The per-window mask-balance pre-screen on a synthetic design (`tests/fixtures/mask_balance/`,
//! every name and value invented). No reference checks this rule; the expectations follow the
//! module's stated interpretation (see `vyges_opendb::masks_balance`).
//!
//! Die 20 x 10 um, windows 10 um stepped 10 um: two windows per layer.
//! * M1 left: a mask-1 stripe (8 um2) and a block's uncoloured obstruction (4 um2): 8/12, inside
//!   [0.2, 0.8] only because the uncoloured metal is in the total.
//! * M1 right: a mask-1 stripe (8 um2) with a mask-1 fill inside it (merged: still 8), a mask-2
//!   fill (4 um2) and a mask-2 routed wire (6.05 x 0.05 um with its extensions): inside the range
//!   only because the fill is in the total.
//! * M2 left: a mask-2 stripe only: share 0, the one finding. M2 right: no metal, empty.
//! * M3 has one mask: never checked, though its only stripe is uncoloured.
#![cfg(unix)]

use std::process::Command;
use vyges_opendb::masks_balance::{self, BalanceOptions};
use vyges_opendb::Db;

fn fixture(f: &str) -> String {
    format!("{}/tests/fixtures/mask_balance/{f}", env!("CARGO_MANIFEST_DIR"))
}

fn load(def: &str) -> Db {
    let mut db = Db::new();
    db.read_lef(fixture("bal.lef")).unwrap();
    db.read_def(fixture(def), "default").unwrap();
    db
}

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vyges-opendb"))
}

fn opts() -> BalanceOptions {
    BalanceOptions::new(10_000, 10_000, 0.2, 0.8).unwrap()
}

/// The fill comes back from odb with its layer and mask.
#[test]
fn fill_is_read_with_layer_and_mask() {
    let db = load("bal.def");
    let m1 = i64::from(db.layer_get_number("M1"));
    let got: Vec<(i64, u8, [i32; 4])> = db.fill_rects().unwrap().iter().map(|f| (f.layer, f.mask, f.rect)).collect();
    assert_eq!(got, vec![(m1, 2, [11000, 6000, 15000, 7000]), (m1, 1, [11000, 1500, 13000, 2500])]);
}

/// Every window, area by area.
#[test]
fn each_window_is_measured_and_only_the_lopsided_one_reported() {
    let rep = masks_balance::check_mask_balance(&load("bal.def"), &opts()).unwrap();
    assert_eq!(rep.die, [0, 0, 20000, 10000]);
    let names: Vec<&str> = rep.layers.iter().map(|l| l.layer.as_str()).collect();
    assert_eq!(names, ["M1", "M2"], "M3 has one mask and is skipped");
    let m1 = &rep.layers[0];
    assert_eq!((m1.windows, m1.windows_checked, m1.windows_empty, m1.windows_outside), (2, 2, 0, 0));
    assert_eq!((m1.design_shapes, m1.fill_shapes), (4, 2), "two stripes, the wire, the block's obstruction");
    let right = 8_000_000.0 / (8_000_000.0 + 4_000_000.0 + 6050.0 * 50.0);
    assert_eq!(m1.min_share, Some(right));
    assert_eq!(m1.max_share, Some(8.0 / 12.0));
    let m2 = &rep.layers[1];
    assert_eq!((m2.windows, m2.windows_checked, m2.windows_empty, m2.windows_outside), (2, 1, 1, 1));
    assert_eq!(rep.windows_checked(), 3);
    assert_eq!(rep.findings.len(), 1, "{:?}", rep.findings);
    let f = &rep.findings[0];
    assert_eq!((f.layer.as_str(), f.window, f.share), ("M2", [0, 0, 10000, 10000], 0.0));
    assert_eq!((f.areas.mask1(), f.areas.total(), f.fill_area), (0, 10_000_000, 0));
}

/// A tight range makes both M1 windows findings, with their category areas.
#[test]
fn findings_carry_the_category_areas() {
    let o = BalanceOptions::new(10_000, 10_000, 0.7, 0.8).unwrap();
    let rep = masks_balance::check_mask_balance(&load("bal.def"), &o).unwrap();
    let m1: Vec<_> = rep.findings.iter().filter(|f| f.layer == "M1").collect();
    assert_eq!(m1.len(), 2);
    let areas: Vec<Vec<(u8, i64)>> = m1.iter().map(|f| f.areas.by_mask.iter().map(|(&m, &a)| (m, a)).collect()).collect();
    assert_eq!(areas[0], vec![(0, 4_000_000), (1, 8_000_000)]);
    assert_eq!(areas[1], vec![(1, 8_000_000), (2, 4_000_000 + 302_500)]);
    assert_eq!((m1[0].fill_area, m1[1].fill_area), (0, 6_000_000));
}

fn run(args: &[&str]) -> (i32, serde_json::Value) {
    let out = bin().arg("check-mask-balance").args(args).output().unwrap();
    let code = out.status.code().unwrap_or(-1);
    let json = serde_json::from_slice(&out.stdout).unwrap_or(serde_json::Value::Null);
    (code, json)
}

/// The command line: status words, counts, exit codes, and no defaults.
#[test]
fn cli_reports_status_counts_and_exit_codes() {
    let (lef, def) = (fixture("bal.lef"), fixture("bal.def"));
    let params = ["--balance-window", "10", "--balance-step", "10", "--balance-range", "0.2,0.8"];
    let mut args = vec!["--lef", lef.as_str(), "--def", def.as_str()];
    args.extend(params);
    let (code, j) = run(&args);
    assert_eq!(code, 0);
    assert_eq!(j["status"], "checked");
    assert_eq!(j["windows_checked"], 3);
    assert_eq!(j["windows_outside"], 1);
    assert_eq!(j["window_dbu"], 10000);
    assert_eq!(j["findings"][0]["layer"], "M2");
    assert_eq!(j["findings"][0]["areas"]["mask_2"], 10_000_000);
    let mut failing = args.clone();
    failing.push("--fail-on-findings");
    assert_eq!(run(&failing).0, 1);

    // Each parameter is required: no value is assumed.
    for skip in [0, 2, 4] {
        let mut a = vec!["--lef", lef.as_str(), "--def", def.as_str()];
        for (k, p) in params.chunks(2).enumerate() {
            if k * 2 != skip {
                a.extend(p);
            }
        }
        assert_ne!(run(&a).0, 0, "without {}", params[skip]);
    }
    // Bad values are refused.
    for (flag, v) in [("--balance-range", "0.8,0.2"), ("--balance-range", "0.2"), ("--balance-step", "20"), ("--balance-window", "x")] {
        let mut a: Vec<&str> = vec!["--lef", lef.as_str(), "--def", def.as_str()];
        for p in params.chunks(2) {
            a.extend([p[0], if p[0] == flag { v } else { p[1] }]);
        }
        assert_ne!(run(&a).0, 0, "{flag} {v}");
    }

    // A design with no metal: windows exist, none checked; never a pass word.
    let empty = fixture("empty.def");
    let mut a = vec!["--lef", lef.as_str(), "--def", empty.as_str()];
    a.extend(params);
    let (code, j) = run(&a);
    assert_eq!((code, j["status"].as_str(), j["windows_checked"].as_u64()), (0, Some("nothing_checkable"), Some(0)));

    // A stack without a multi-mask layer.
    let (plef, pdef) = (
        format!("{}/tests/fixtures/prl_two_sides/prl.lef", env!("CARGO_MANIFEST_DIR")),
        format!("{}/tests/fixtures/prl_two_sides/prl.def", env!("CARGO_MANIFEST_DIR")),
    );
    let mut a = vec!["--lef", plef.as_str(), "--def", pdef.as_str()];
    a.extend(params);
    assert_eq!(run(&a).1["status"], "no_multi_mask_layers");
}
