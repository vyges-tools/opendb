// SPDX-License-Identifier: Apache-2.0
//! libodb's diagnostics once an events sink is installed ([`vyges_opendb::init_events_logging`]):
//! a database created afterwards logs EVENTS-ONLY, and [`Db::with_captured_logs`] must keep working
//! in that mode — the shim leaves its permanent redirect for the capture and re-enters it after.
//!
//! ⚠️ Its own test binary on purpose: installing the sink is process-wide and one-way, so these
//! tests must not share a process with ones that expect libodb's default stdout sink.
#![cfg(unix)]

use vyges_opendb::Db;

fn lef() -> String {
    format!("{}/tests/fixtures/3dblox/minimal_tech.lef", env!("CARGO_MANIFEST_DIR"))
}

/// A capture returns libodb's text in events-only mode, and a SECOND capture on the same database
/// works too — utl refuses a redirect while another is active, so a shim that failed to leave or
/// re-enter its permanent redirect would fail here.
#[test]
fn captures_still_return_the_text_once_events_only() {
    vyges_opendb::init_events_logging();
    let mut db = Db::new();
    let (r, first) = db.with_captured_logs(|db| db.read_lef(&lef()));
    r.expect("the technology LEF reads");
    assert!(first.contains("ODB-0227"), "the capture lost libodb's LEF message: {first:?}");
    let (_, second) = db.with_captured_logs(|db| db.read_lef(&lef()));
    assert!(!second.is_empty(), "a second capture on the same database returned nothing");
    // And a database opened AFTER the sink goes the same way.
    let mut fresh = Db::new();
    let (r, third) = fresh.with_captured_logs(|db| db.read_lef(&lef()));
    r.expect("the technology LEF reads");
    assert!(third.contains("ODB-0227"), "{third:?}");
}
