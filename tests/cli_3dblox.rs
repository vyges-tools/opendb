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

// ── The drawing ─────────────────────────────────────────────────────────────────────────────

/// A two-die stack, built here rather than read, so the *violating* variant below differs from
/// the clean one by exactly one move. Mirrors `chip_create.rs::clean_stack`.
fn built_stack() -> vyges_opendb::Db {
    use vyges_opendb::Db;
    let mut db = Db::open("tests/fixtures/counter.odb").unwrap();
    db.create_chip("stack", "", "HIER").unwrap();
    db.create_chip_block("stack", "stack_blk").unwrap();
    db.create_chip("base", "", "SUBSTRATE").unwrap();
    db.create_chip("upper", "", "DIE").unwrap();
    for (c, w, h, t) in [("base", 60000, 50000, 1500), ("upper", 50000, 40000, 700)] {
        db.chip_set_width(c, w).unwrap();
        db.chip_set_height(c, h).unwrap();
        db.chip_set_thickness(c, t).unwrap();
    }
    db.create_chip_region("base", "up", "FRONT", "").unwrap();
    db.set_chip_region_box("base", "up", 0, 0, 60000, 50000).unwrap();
    db.create_chip_region("upper", "down", "BACK", "").unwrap();
    db.set_chip_region_box("upper", "down", 0, 0, 50000, 40000).unwrap();
    db.create_chip_inst("stack", "base", "u_base").unwrap();
    db.create_chip_inst("stack", "upper", "u_upper").unwrap();
    db.place_chip_inst("stack", "u_base", "R0", 0, 0, 0).unwrap();
    db.place_chip_inst("stack", "u_upper", "R0", 0, 0, 1500).unwrap();
    db.create_chip_conn("bond", "stack", "u_upper", "down", "u_base", "up", 0).unwrap();
    db.set_top_chip("stack").unwrap();
    db.construct_unfolded_model().unwrap();
    db
}

#[test]
fn the_drawing_reads_the_placed_geometry_out_of_the_database() {
    use vyges_opendb::view3d::Assembly3d;
    let db = built_stack();
    let a = Assembly3d::read(&db, "stack").unwrap();

    assert_eq!(a.dies.len(), 2);
    assert_eq!(a.bonds.len(), 1);
    // Sorted by Z, so the substrate comes first and the drawing's legend is diffable run to run.
    assert_eq!(a.dies[0].inst, "u_base");
    assert_eq!(a.dies[1].inst, "u_upper");
    assert_eq!(a.dies[1].z, 1500.0);
    assert_eq!(a.dies[0].thickness, 1500.0);
    assert_eq!(a.dies[0].chip_type, "SUBSTRATE");
    assert_eq!(a.bonds[0].top, "u_upper");
    assert_eq!(a.bonds[0].bottom, "u_base");
}

#[test]
fn a_violation_reaches_the_drawing() {
    // The whole reason to draw an assembly is to see where a finding is. If the linter reports
    // an overlap and the picture does not mention it, the picture is actively misleading.
    use vyges_opendb::view3d::{to_svg, Assembly3d};
    let mut db = built_stack();
    db.place_chip_inst("stack", "u_upper", "R0", 12000, 12000, 1000).unwrap();
    db.construct_unfolded_model().unwrap();
    assert!(db.check_3dblox().unwrap() > 0, "the move must violate something");

    let findings: Vec<(String, String)> = ["Overlapping chips"]
        .iter()
        .filter_map(|c| {
            let p = format!("3DBlox/{c}");
            let n = vyges_opendb::registry::get(&db, "dbMarkerCategory", "get_marker_count",
                                                &[p.clone()]).ok()?.as_i64()?;
            (n > 0).then(|| (c.to_string(), format!("{n} marker(s)")))
        })
        .collect();
    assert!(!findings.is_empty());

    let svg = to_svg(&Assembly3d::read(&db, "stack").unwrap().with_findings(findings), 1000.0);
    assert!(svg.contains("Overlapping chips"), "the finding must appear on the drawing");
    assert!(!svg.contains("no violations"));
}

#[test]
fn the_viewer_turns_an_interchange_file_into_a_drawing_in_one_command() {
    // The user-facing claim: a .3dbx someone sends you becomes a picture without first
    // converting it to a database by hand.
    let svg = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("cli_3dblox_view.svg");
    let _ = std::fs::remove_file(&svg);
    let out = Command::new(bin())
        .args(["view-3dblox", "--input", DBX, "--output"])
        .arg(&svg)
        .output()
        .expect("run view-3dblox");
    assert!(
        out.status.success(),
        "view-3dblox failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc = std::fs::read_to_string(&svg).expect("svg written");
    assert!(doc.starts_with("<svg") && doc.trim_end().ends_with("</svg>"));
    assert!(doc.contains("TopDesign"), "the design name belongs on the drawing");
    // Both dies, and the flip, must survive the whole path from file to picture.
    assert!(doc.contains("soc_inst") && doc.contains("soc_inst_duplicate"));
    assert!(doc.contains("flipped"), "the MZ die is flipped and the drawing must say so");
}

#[test]
fn a_database_input_without_top_is_refused_rather_than_drawn_empty() {
    // An empty page that says "no violations" is the worst possible output here, so a missing
    // --top has to be an error and not a default.
    let odb = out("viewtop");
    assert!(Command::new(bin())
        .args(["read-3dblox", "--input", DBX, "--output"])
        .arg(&odb)
        .status()
        .unwrap()
        .success());

    let out_ = Command::new(bin())
        .args(["view-3dblox", "--input"])
        .arg(&odb)
        .args(["--output", "/dev/null"])
        .output()
        .unwrap();
    assert!(!out_.status.success());
    assert!(String::from_utf8_lossy(&out_.stderr).contains("--top"));
}

#[test]
fn the_output_extension_picks_the_format() {
    // A --format flag that can disagree with the filename is how PNG bytes end up in a file
    // called .svg, which nothing downstream will open. The name is the single source of truth.
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let run = |name: &str| {
        let p = dir.join(name);
        let _ = std::fs::remove_file(&p);
        let out = Command::new(bin())
            .args(["view-3dblox", "--input", DBX, "--output"])
            .arg(&p)
            .output()
            .expect("run view-3dblox");
        (out, p)
    };

    let (out, png) = run("cli_3dblox_fmt.png");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let bytes = std::fs::read(&png).unwrap();
    assert_eq!(&bytes[..8], &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);

    let (out, svg) = run("cli_3dblox_fmt.svg");
    assert!(out.status.success());
    assert!(std::fs::read_to_string(&svg).unwrap().starts_with("<svg"));

    // An extension neither back-end can produce is an error, not a silent default — writing SVG
    // into a .jpg would be a file nobody can open and no message saying why.
    let (out, _) = run("cli_3dblox_fmt.jpg");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains(".jpg"));
}

#[test]
fn the_png_scale_reaches_the_image() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let dims = |scale: &str, name: &str| {
        let p = dir.join(name);
        assert!(Command::new(bin())
            .args(["view-3dblox", "--input", DBX, "--scale", scale, "--output"])
            .arg(&p)
            .status()
            .unwrap()
            .success());
        let b = std::fs::read(&p).unwrap();
        u32::from_be_bytes(b[16..20].try_into().unwrap())
    };
    assert_eq!(dims("2", "cli_3dblox_s2.png"), 2 * dims("1", "cli_3dblox_s1.png"));
}

// ── Bumps ───────────────────────────────────────────────────────────────────────────────────

/// The d2d fixture assembly copied to scratch, so a test can perturb its bump maps.
fn bump_fixture(tag: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("bumps_{tag}"));
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
fn bumps_from_a_bump_map_survive_into_the_written_database() {
    // The reader used to load geometry only, so a .3dbx produced a database with no bumps and
    // every bump-related check had nothing to look at — reporting clean for want of input.
    let dir = bump_fixture("roundtrip");
    let odb = dir.join("out.odb");
    let out = Command::new(bin())
        .args(["read-3dblox", "--input"])
        .arg(dir.join("stack.3dbx"))
        .arg("--output")
        .arg(&odb)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    // Read back from the file, not from the process that wrote it — the bumps have to persist.
    let pos = |path: &str, idx: u32| -> i64 {
        let o = Command::new(bin())
            .args(["get", "-i"])
            .arg(&odb)
            .args([
                "--class", "dbUnfoldedChipBumpInst",
                "--field", "get_global_position_x",
                "--key", path, "--key", "0", "--key", &idx.to_string(),
            ])
            .output()
            .unwrap();
        String::from_utf8_lossy(&o.stdout).trim().parse().unwrap_or(-1)
    };
    // logic_front.bmap puts bumps at 40/80/120/160 um; the header precision is 2000 dbu/um.
    assert_eq!(pos("u_logic", 0), 80_000);
    assert_eq!(pos("u_logic", 3), 320_000);
    // u_mem is MZ_MY — mirrored about its 200 um die — so its bumps land on the same points.
    assert_eq!(pos("u_mem", 0), 80_000);
    assert_eq!(pos("u_mem", 3), 320_000);
}

#[test]
fn a_bump_outside_its_die_is_now_caught_from_an_assembly() {
    // The capability loading bumps actually buys: check-3dblox's Bump Alignment check had
    // nothing to run on before, because a .3dbx produced a database with no bumps at all.
    let dir = bump_fixture("outside");
    let mut map = std::fs::read_to_string(dir.join("logic_front.bmap")).unwrap();
    map.push_str("lg_stray MICROBUMP 900.0 900.0 tx[9] d2d_stray\n"); // far outside the 200 um die
    std::fs::write(dir.join("logic_front.bmap"), map).unwrap();

    let odb = dir.join("out.odb");
    assert!(Command::new(bin())
        .args(["read-3dblox", "--input"])
        .arg(dir.join("stack.3dbx"))
        .arg("--output")
        .arg(&odb)
        .status()
        .unwrap()
        .success());

    let out = Command::new(bin()).args(["check-3dblox", "--input"]).arg(&odb).output().unwrap();
    assert!(
        out.status.success(),
        "check-3dblox must not abort on a bump finding: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let j: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(j["violations"], 1);
    let cat = &j["categories"][0];
    assert_eq!(cat["category"], "Bump Alignment");
    assert!(cat["markers"][0]["comment"]
        .as_str()
        .unwrap()
        .contains("outside its parent region"));
}

#[test]
fn a_bump_finding_does_not_abort_the_process() {
    // Not a hypothetical. dbMarker::getName() switches on its sources' object types and calls
    // logger->error() on dbChipBumpInst, which it does not handle; utl::Logger::error throws,
    // our generated getter is bound infallible, and the process dies. A single bump outside its
    // region killed check-3dblox outright before this was guarded.
    let dir = bump_fixture("noabort");
    let mut map = std::fs::read_to_string(dir.join("logic_front.bmap")).unwrap();
    map.push_str("lg_stray MICROBUMP 900.0 900.0 tx[9] d2d_stray\n");
    std::fs::write(dir.join("logic_front.bmap"), map).unwrap();
    let odb = dir.join("out.odb");
    Command::new(bin())
        .args(["read-3dblox", "--input"])
        .arg(dir.join("stack.3dbx"))
        .arg("--output")
        .arg(&odb)
        .status()
        .unwrap();

    let out = Command::new(bin()).args(["check-3dblox", "--input"]).arg(&odb).output().unwrap();
    // An abort shows up as a signal, not an exit code, so check both.
    assert!(out.status.success(), "exited {:?}", out.status);
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("libc++abi"),
        "the process aborted: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_missing_bump_map_is_reported_rather_than_read_as_no_bumps() {
    let dir = bump_fixture("missingmap");
    std::fs::remove_file(dir.join("mem_front.bmap")).unwrap();
    let out = Command::new(bin())
        .args(["read-3dblox", "--input"])
        .arg(dir.join("stack.3dbx"))
        .arg("--output")
        .arg(dir.join("out.odb"))
        .output()
        .unwrap();
    assert!(out.status.success(), "the geometry is still worth having");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("mem_front.bmap"), "the missing map must be named: {err}");
}
