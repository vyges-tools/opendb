// SPDX-License-Identifier: Apache-2.0
//! `vyges-opendb` — OpenROAD's OpenDB (libodb) design-database CLI, shipped by `vyges install opendb`.
//!
//! A thin multi-tool over the safe [`vyges_opendb`] API (OpenROAD's OpenDB / libodb). Unix-only:
//! libodb is native C++ and is not built on non-unix targets.
//!
//! Subcommands:
//!   info                 read a `.odb` and print a one-line block summary (read path).
//!   insert-eco-buffers   splice ECO buffers into a `.odb` (Loom step; LibreLane-compatible
//!                        `Odb.InsertECOBuffers` database surgery). Legalization is separate.
//!
//! Arg parsing is deliberately hand-rolled (no clap) to match the rest of the suite and keep
//! the dependency surface minimal.
use serde::Deserialize;
use vyges_opendb::{eco, report, Db};

type Fail = Box<dyn std::error::Error>;

const USAGE: &str = "\
vyges-opendb — OpenROAD's OpenDB (libodb) design database

usage:
  vyges-opendb <command> [options]

commands:
  info                --input <f.odb>
                      Print a one-line summary: block name + inst/net/bterm counts.

  insert-eco-buffers  --input <in.odb> --output <out.odb> [--config <eco.json>]
                      Insert ECO buffers (INSERT_ECO_BUFFERS in the config) into the design.

  insert-eco-diodes   --input <in.odb> --output <out.odb> [--config <eco.json>]
                      Tie antenna diodes (INSERT_ECO_DIODES in the config) onto target nets.

  manual-global-placement  --input <in.odb> --output <out.odb> [--config <cfg.json>]
                      Set instance origins (MANUAL_GLOBAL_PLACEMENT in the config).

  manual-macro-placement   --input <in.odb> --output <out.odb> [--config <cfg.json>]
                      Place + orient macros (MANUAL_MACRO_PLACEMENT in the config).

  diodes-on-ports     --input <in.odb> --output <out.odb> [--config <cfg.json>]
                      Tie antenna diodes onto I/O port nets (DIODES_ON_PORTS in the config).

  cell-frequency-tables     --input <f.odb>
                      Print a JSON table of instance count per master cell (report).

  report-disconnected-pins  --input <f.odb>
                      Print a JSON list of pins/ports with no net (report).

  set-power-connections     --input <in.odb> --output <out.odb> [--config <cfg.json>]
                      Wire instance pins to (power) nets (SET_POWER_CONNECTIONS in the config).

  add-obstructions          --input <in.odb> --output <out.odb> [--config <cfg.json>]
                      Add routing/PDN obstruction rects (OBSTRUCTIONS in the config).

  remove-obstructions       --input <in.odb> --output <out.odb>
                      Remove all obstructions.

  write-verilog-header      --input <f.odb> [--output <f.v>]
                      Emit a Verilog module header (ports + directions).

  report-wire-length        --input <f.odb>
                      Print the total routed wire length as JSON (report).

  report-connectivity       --input <f.odb>
                      Dump the netlist connectivity graph as JSON (report).

  read-3dblox               --input <f.3dbx> --output <out.odb> [--into <in.odb>]
                      Read a 3Dblox assembly (the 2.5D/3D interchange format) into a
                      database, so it can be linted or queried. Reports anything the
                      format expresses and the database cannot.
  view-3dblox               --input <f.3dbx|f.odb> --output <out.svg|out.png>
                            [--top <chip>] [--scale <n>]
                      Draw the assembly: cross-section + plan, with any check-3dblox
                      findings listed on it. Format follows the output extension.
  check-d2d                 --input <stack.3dbx> | --top <a.bmap> --bottom <b.bmap>
                            [--offset-x <um>] [--offset-y <um>] [--flip-x]
                            [--tolerance <um>]
                      Check a die-to-die interface: unmated bumps, misalignment, net
                      and bump-cell mismatch across the bond. Emits JSON.
  check-3dblox              --input <f.odb>
                      3D/chiplet structural sign-off lint; reports violations as JSON (check).

  apply-eco-plan            --input <in.odb> --plan <plan.json> --output <out.odb>
                      Replay a timing-repair plan (all-or-nothing) into the design.

  custom-io-placement       --input <in.odb> --output <out.odb> [--config <cfg.json>]
                      Place I/O port pins (CUSTOM_IO_PLACEMENT in the config).

  write-def                 --input <f.odb> --output <f.def>
                      Export the design to a DEF 5.8 file (libodb v1 LEF/DEF I/O).

  read-def                  --input <in.odb> --def <f.def> --output <out.odb>
                      Import a DEF into the design (libodb v1 LEF/DEF I/O).

  apply-def-template        --input <in.odb> --template <f.def> --output <out.odb>
                      Apply a template DEF's floorplan (Odb.ApplyDEFTemplate).

  fields              [--class <dbClass>] [--writable]
                      List the generated instrumentation fields (discovery; JSON).

  get                 --input <f.odb> --class <dbClass> --field <name> [--key <k>]...
                      Read any generated field by (class, field) + addressing keys (JSON).

  set                 --input <in.odb> --output <out.odb> --class <dbClass> --field <name>
                      [--key <k>]... [--value <v>]...
                      Apply a generated setter (requires a build with --features gen-write).

  --version, -V       Print the version.
  --help,    -h       Print this help.
";

fn main() {
    // Centralize libodb's native (C++ utl::Logger) diagnostics through vyges-events, alongside the
    // Rust-surface events — so everything odb emits lands in the MCP causal trail.
    vyges_opendb::init_events_logging();
    if let Err(e) = run() {
        eprintln!("vyges-opendb: error: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Fail> {
    // `vyges mcp` appends `--json` to every engine call (the Loom envelope convention). Our read
    // subcommands already emit JSON and the rest write files, so accept it globally as a no-op —
    // otherwise every opendb call through MCP dies on "unknown argument: --json".
    let mut args = std::env::args().skip(1).filter(|a| a != "--json");
    let cmd = args.next().unwrap_or_default();
    match cmd.as_str() {
        "info" => info(args),
        "insert-eco-buffers" => insert_eco_buffers(args),
        "insert-eco-diodes" => insert_eco_diodes(args),
        "manual-global-placement" => manual_global_placement(args),
        "manual-macro-placement" => manual_macro_placement(args),
        "diodes-on-ports" => diodes_on_ports(args),
        "cell-frequency-tables" => cell_frequency_tables(args),
        "report-disconnected-pins" => report_disconnected_pins(args),
        "set-power-connections" => set_power_connections(args),
        "add-obstructions" => add_obstructions(args),
        "remove-obstructions" => remove_obstructions(args),
        "write-verilog-header" => write_verilog_header(args),
        "report-wire-length" => report_wire_length(args),
        "read-3dblox" => read_3dblox(args),
        "view-3dblox" => view_3dblox(args),
        "check-d2d" => check_d2d(args),
        "check-3dblox" => check_3dblox(args),
        "apply-eco-plan" => apply_eco_plan(args),
        "report-connectivity" => report_connectivity(args),
        "custom-io-placement" => custom_io_placement(args),
        "write-def" => write_def(args),
        "read-def" => read_def(args),
        "apply-def-template" => apply_def_template(args),
        "fields" => fields(args),
        "get" => get(args),
        "set" => set(args),
        "-V" | "--version" => {
            println!("vyges-opendb {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        "" | "-h" | "--help" => {
            print!("{USAGE}");
            Ok(())
        }
        other => Err(format!("unknown command '{other}'. Try 'vyges-opendb --help'.").into()),
    }
}

/// `info --input <f.odb>` — read a design and print a one-line summary.
fn info(mut args: impl Iterator<Item = String>) -> Result<(), Fail> {
    let mut input = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--input" | "-i" => input = args.next(),
            "-h" | "--help" => {
                eprintln!("usage: vyges-opendb info --input <f.odb>");
                return Ok(());
            }
            other => return Err(format!("info: unknown argument: {other}").into()),
        }
    }
    let input = input.ok_or("info: --input <f.odb> required")?;
    let db = Db::open(&input)?;
    println!(
        "{input}: block={} insts={} nets={} bterms={}",
        db.block_name(),
        db.num_insts(),
        db.num_nets(),
        db.num_bterms(),
    );
    Ok(())
}

#[derive(Deserialize, Default)]
struct EcoConfig {
    #[serde(rename = "INSERT_ECO_BUFFERS", default)]
    insert_eco_buffers: Vec<eco::EcoBuffer>,
}

/// Machine-readable step contract (the Vyges/Loom step convention): identity, the CLI args, and
/// the config schema — so an orchestrator (Sley / Loom auto-mode) can introspect a step without
/// running it. `insert-eco-buffers --describe` emits this; every step ships the same shape.
const INSERT_ECO_BUFFERS_DESCRIBE: &str = r#"{
  "step": "insert-eco-buffers",
  "summary": "Splice ECO buffers into a placed .odb (database surgery; legalization is a separate step).",
  "librelane_equivalent": "Odb.InsertECOBuffers",
  "unix_only": true,
  "args": [
    { "name": "--input",  "kind": "input",  "type": "path", "required": true,  "description": "input .odb design" },
    { "name": "--output", "kind": "output", "type": "path", "required": true,  "description": "output .odb after ECO" },
    { "name": "--config", "kind": "config", "type": "path", "required": false, "description": "JSON with INSERT_ECO_BUFFERS (default: no-op)" }
  ],
  "config_schema": {
    "INSERT_ECO_BUFFERS": {
      "type": "array",
      "description": "buffers to insert; each rewires the target pin's driver through a new buffer",
      "item": {
        "target": { "type": "string", "description": "instance/pin to buffer, e.g. inst42/A" },
        "buffer": { "type": "string", "description": "library cell master, e.g. sky130_fd_sc_hd__buf_2" }
      }
    }
  }
}"#;

/// `insert-eco-buffers --input <in.odb> --output <out.odb> [--config <eco.json>] | --describe`.
fn insert_eco_buffers(mut args: impl Iterator<Item = String>) -> Result<(), Fail> {
    let (mut input, mut output, mut config) = (None, None, None);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--input" | "-i" => input = args.next(),
            "--output" | "-o" => output = args.next(),
            "--config" | "-c" => config = args.next(),
            "--describe" => {
                println!("{INSERT_ECO_BUFFERS_DESCRIBE}");
                return Ok(());
            }
            "-h" | "--help" => {
                eprintln!("usage: vyges-opendb insert-eco-buffers --input <in.odb> --output <out.odb> --config <eco.json>");
                eprintln!("       vyges-opendb insert-eco-buffers --describe   # JSON step contract");
                return Ok(());
            }
            other => return Err(format!("insert-eco-buffers: unknown argument: {other}").into()),
        }
    }
    let input = input.ok_or("insert-eco-buffers: --input <in.odb> required")?;
    let output = output.ok_or("insert-eco-buffers: --output <out.odb> required")?;
    let cfg: EcoConfig = match config {
        Some(p) => serde_json::from_str(&std::fs::read_to_string(&p)?)?,
        None => EcoConfig::default(),
    };

    let mut db = Db::open(&input)?;
    let n = eco::insert_eco_buffers(&mut db, &cfg.insert_eco_buffers)?;
    db.write(&output)?;
    eprintln!("insert-eco-buffers: inserted {n} buffer(s), {input} -> {output}");
    Ok(())
}

#[derive(Deserialize, Default)]
struct DiodeConfig {
    #[serde(rename = "INSERT_ECO_DIODES", default)]
    insert_eco_diodes: Vec<eco::EcoDiode>,
}

const INSERT_ECO_DIODES_DESCRIBE: &str = r#"{
  "step": "insert-eco-diodes",
  "summary": "Tie antenna diodes onto target nets in a placed .odb (database surgery; a diode is a leaf, no rewiring).",
  "librelane_equivalent": "Odb.InsertECODiodes",
  "unix_only": true,
  "args": [
    { "name": "--input",  "kind": "input",  "type": "path", "required": true,  "description": "input .odb design" },
    { "name": "--output", "kind": "output", "type": "path", "required": true,  "description": "output .odb after ECO" },
    { "name": "--config", "kind": "config", "type": "path", "required": false, "description": "JSON with INSERT_ECO_DIODES (default: no-op)" }
  ],
  "config_schema": {
    "INSERT_ECO_DIODES": {
      "type": "array",
      "description": "diodes to insert; each ties an antenna diode onto the target pin's net (no rewiring)",
      "item": {
        "target": { "type": "string", "description": "instance/pin whose net gets a diode, e.g. inst42/A" },
        "diode":  { "type": "string", "description": "antenna-diode master, e.g. sky130_fd_sc_hd__diode_2" }
      }
    }
  }
}"#;

/// `insert-eco-diodes --input <in.odb> --output <out.odb> [--config <eco.json>] | --describe`.
fn insert_eco_diodes(mut args: impl Iterator<Item = String>) -> Result<(), Fail> {
    let (mut input, mut output, mut config) = (None, None, None);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--input" | "-i" => input = args.next(),
            "--output" | "-o" => output = args.next(),
            "--config" | "-c" => config = args.next(),
            "--describe" => {
                println!("{INSERT_ECO_DIODES_DESCRIBE}");
                return Ok(());
            }
            "-h" | "--help" => {
                eprintln!("usage: vyges-opendb insert-eco-diodes --input <in.odb> --output <out.odb> --config <eco.json>");
                eprintln!("       vyges-opendb insert-eco-diodes --describe   # JSON step contract");
                return Ok(());
            }
            other => return Err(format!("insert-eco-diodes: unknown argument: {other}").into()),
        }
    }
    let input = input.ok_or("insert-eco-diodes: --input <in.odb> required")?;
    let output = output.ok_or("insert-eco-diodes: --output <out.odb> required")?;
    let cfg: DiodeConfig = match config {
        Some(p) => serde_json::from_str(&std::fs::read_to_string(&p)?)?,
        None => DiodeConfig::default(),
    };

    let mut db = Db::open(&input)?;
    let n = eco::insert_eco_diodes(&mut db, &cfg.insert_eco_diodes)?;
    db.write(&output)?;
    eprintln!("insert-eco-diodes: inserted {n} diode(s), {input} -> {output}");
    Ok(())
}

#[derive(Deserialize, Default)]
struct GlobalPlacementConfig {
    #[serde(rename = "MANUAL_GLOBAL_PLACEMENT", default)]
    manual_global_placement: Vec<eco::GlobalPlacement>,
}

const MANUAL_GLOBAL_PLACEMENT_DESCRIBE: &str = r#"{
  "step": "manual-global-placement",
  "summary": "Set instance origins in a .odb before global placement (database surgery).",
  "librelane_equivalent": "Odb.ManualGlobalPlacement",
  "unix_only": true,
  "args": [
    { "name": "--input",  "kind": "input",  "type": "path", "required": true,  "description": "input .odb design" },
    { "name": "--output", "kind": "output", "type": "path", "required": true,  "description": "output .odb after placement" },
    { "name": "--config", "kind": "config", "type": "path", "required": false, "description": "JSON with MANUAL_GLOBAL_PLACEMENT (default: no-op)" }
  ],
  "config_schema": {
    "MANUAL_GLOBAL_PLACEMENT": {
      "type": "array",
      "description": "instances to fix at an origin",
      "item": {
        "instance": { "type": "string",  "description": "instance name" },
        "x":        { "type": "integer", "description": "origin x in DBU" },
        "y":        { "type": "integer", "description": "origin y in DBU" }
      }
    }
  }
}"#;

/// `manual-global-placement --input <in.odb> --output <out.odb> [--config <cfg.json>] | --describe`.
fn manual_global_placement(mut args: impl Iterator<Item = String>) -> Result<(), Fail> {
    let (mut input, mut output, mut config) = (None, None, None);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--input" | "-i" => input = args.next(),
            "--output" | "-o" => output = args.next(),
            "--config" | "-c" => config = args.next(),
            "--describe" => {
                println!("{MANUAL_GLOBAL_PLACEMENT_DESCRIBE}");
                return Ok(());
            }
            "-h" | "--help" => {
                eprintln!("usage: vyges-opendb manual-global-placement --input <in.odb> --output <out.odb> --config <cfg.json>");
                return Ok(());
            }
            other => return Err(format!("manual-global-placement: unknown argument: {other}").into()),
        }
    }
    let input = input.ok_or("manual-global-placement: --input <in.odb> required")?;
    let output = output.ok_or("manual-global-placement: --output <out.odb> required")?;
    let cfg: GlobalPlacementConfig = match config {
        Some(p) => serde_json::from_str(&std::fs::read_to_string(&p)?)?,
        None => GlobalPlacementConfig::default(),
    };

    let mut db = Db::open(&input)?;
    let n = eco::manual_global_placement(&mut db, &cfg.manual_global_placement)?;
    db.write(&output)?;
    eprintln!("manual-global-placement: placed {n} instance(s), {input} -> {output}");
    Ok(())
}

#[derive(Deserialize, Default)]
struct MacroPlacementConfig {
    #[serde(rename = "MANUAL_MACRO_PLACEMENT", default)]
    manual_macro_placement: Vec<eco::MacroPlacement>,
}

const MANUAL_MACRO_PLACEMENT_DESCRIBE: &str = r#"{
  "step": "manual-macro-placement",
  "summary": "Place + orient macros in a .odb (database surgery).",
  "librelane_equivalent": "Odb.ManualMacroPlacement",
  "unix_only": true,
  "args": [
    { "name": "--input",  "kind": "input",  "type": "path", "required": true,  "description": "input .odb design" },
    { "name": "--output", "kind": "output", "type": "path", "required": true,  "description": "output .odb after placement" },
    { "name": "--config", "kind": "config", "type": "path", "required": false, "description": "JSON with MANUAL_MACRO_PLACEMENT (default: no-op)" }
  ],
  "config_schema": {
    "MANUAL_MACRO_PLACEMENT": {
      "type": "array",
      "description": "macros to place + orient",
      "item": {
        "instance": { "type": "string",  "description": "macro instance name" },
        "x":        { "type": "integer", "description": "origin x in DBU" },
        "y":        { "type": "integer", "description": "origin y in DBU" },
        "orient":   { "type": "string",  "description": "R0/R90/R180/R270/MX/MY/MXR90/MYR90 (optional)" }
      }
    }
  }
}"#;

/// `manual-macro-placement --input <in.odb> --output <out.odb> [--config <cfg.json>] | --describe`.
fn manual_macro_placement(mut args: impl Iterator<Item = String>) -> Result<(), Fail> {
    let (mut input, mut output, mut config) = (None, None, None);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--input" | "-i" => input = args.next(),
            "--output" | "-o" => output = args.next(),
            "--config" | "-c" => config = args.next(),
            "--describe" => {
                println!("{MANUAL_MACRO_PLACEMENT_DESCRIBE}");
                return Ok(());
            }
            "-h" | "--help" => {
                eprintln!("usage: vyges-opendb manual-macro-placement --input <in.odb> --output <out.odb> --config <cfg.json>");
                return Ok(());
            }
            other => return Err(format!("manual-macro-placement: unknown argument: {other}").into()),
        }
    }
    let input = input.ok_or("manual-macro-placement: --input <in.odb> required")?;
    let output = output.ok_or("manual-macro-placement: --output <out.odb> required")?;
    let cfg: MacroPlacementConfig = match config {
        Some(p) => serde_json::from_str(&std::fs::read_to_string(&p)?)?,
        None => MacroPlacementConfig::default(),
    };

    let mut db = Db::open(&input)?;
    let n = eco::manual_macro_placement(&mut db, &cfg.manual_macro_placement)?;
    db.write(&output)?;
    eprintln!("manual-macro-placement: placed {n} macro(s), {input} -> {output}");
    Ok(())
}

#[derive(Deserialize, Default)]
struct DiodesOnPortsConfig {
    #[serde(rename = "DIODES_ON_PORTS")]
    diodes_on_ports: Option<eco::DiodesOnPorts>,
}

const DIODES_ON_PORTS_DESCRIBE: &str = r#"{
  "step": "diodes-on-ports",
  "summary": "Tie antenna diodes onto I/O port nets in a placed .odb (database surgery).",
  "librelane_equivalent": "Odb.DiodesOnPorts",
  "unix_only": true,
  "args": [
    { "name": "--input",  "kind": "input",  "type": "path", "required": true,  "description": "input .odb design" },
    { "name": "--output", "kind": "output", "type": "path", "required": true,  "description": "output .odb after ECO" },
    { "name": "--config", "kind": "config", "type": "path", "required": false, "description": "JSON with DIODES_ON_PORTS (default: no-op)" }
  ],
  "config_schema": {
    "DIODES_ON_PORTS": {
      "type": "object",
      "description": "tie an antenna diode onto each selected port's net",
      "item": {
        "diode": { "type": "string", "description": "antenna-diode master, e.g. sky130_fd_sc_hd__diode_2" },
        "ports": { "type": "array",  "description": "specific port names; omitted/empty = all ports" }
      }
    }
  }
}"#;

/// `diodes-on-ports --input <in.odb> --output <out.odb> [--config <cfg.json>] | --describe`.
fn diodes_on_ports(mut args: impl Iterator<Item = String>) -> Result<(), Fail> {
    let (mut input, mut output, mut config) = (None, None, None);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--input" | "-i" => input = args.next(),
            "--output" | "-o" => output = args.next(),
            "--config" | "-c" => config = args.next(),
            "--describe" => {
                println!("{DIODES_ON_PORTS_DESCRIBE}");
                return Ok(());
            }
            "-h" | "--help" => {
                eprintln!("usage: vyges-opendb diodes-on-ports --input <in.odb> --output <out.odb> --config <cfg.json>");
                return Ok(());
            }
            other => return Err(format!("diodes-on-ports: unknown argument: {other}").into()),
        }
    }
    let input = input.ok_or("diodes-on-ports: --input <in.odb> required")?;
    let output = output.ok_or("diodes-on-ports: --output <out.odb> required")?;
    let cfg: DiodesOnPortsConfig = match config {
        Some(p) => serde_json::from_str(&std::fs::read_to_string(&p)?)?,
        None => DiodesOnPortsConfig::default(),
    };

    let mut db = Db::open(&input)?;
    let n = match &cfg.diodes_on_ports {
        Some(spec) => eco::diodes_on_ports(&mut db, spec)?,
        None => 0,
    };
    db.write(&output)?;
    eprintln!("diodes-on-ports: inserted {n} diode(s), {input} -> {output}");
    Ok(())
}

const CELL_FREQUENCY_TABLES_DESCRIBE: &str = r#"{
  "step": "cell-frequency-tables",
  "summary": "Report instance count per master cell as JSON (read-only).",
  "librelane_equivalent": "Odb.CellFrequencyTables",
  "unix_only": true,
  "args": [
    { "name": "--input", "kind": "input", "type": "path", "required": true, "description": "input .odb design" }
  ],
  "output": "JSON array of { master, count } on stdout, most-used first"
}"#;

/// `cell-frequency-tables --input <f.odb> | --describe` — read-only report to stdout (JSON).
fn cell_frequency_tables(mut args: impl Iterator<Item = String>) -> Result<(), Fail> {
    let mut input = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--input" | "-i" => input = args.next(),
            "--describe" => {
                println!("{CELL_FREQUENCY_TABLES_DESCRIBE}");
                return Ok(());
            }
            "-h" | "--help" => {
                eprintln!("usage: vyges-opendb cell-frequency-tables --input <f.odb>");
                return Ok(());
            }
            other => return Err(format!("cell-frequency-tables: unknown argument: {other}").into()),
        }
    }
    let input = input.ok_or("cell-frequency-tables: --input <f.odb> required")?;
    let db = Db::open(&input)?;
    println!("{}", serde_json::to_string_pretty(&report::cell_frequency_table(&db))?);
    Ok(())
}

const REPORT_DISCONNECTED_PINS_DESCRIBE: &str = r#"{
  "step": "report-disconnected-pins",
  "summary": "Report instance pins + ports that carry no net, as JSON (read-only).",
  "librelane_equivalent": "Odb.ReportDisconnectedPins",
  "unix_only": true,
  "args": [
    { "name": "--input", "kind": "input", "type": "path", "required": true, "description": "input .odb design" }
  ],
  "output": "JSON array of strings on stdout: \"inst/pin\" and \"port:name\""
}"#;

/// `report-disconnected-pins --input <f.odb> | --describe` — read-only report to stdout (JSON).
fn report_disconnected_pins(mut args: impl Iterator<Item = String>) -> Result<(), Fail> {
    let mut input = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--input" | "-i" => input = args.next(),
            "--describe" => {
                println!("{REPORT_DISCONNECTED_PINS_DESCRIBE}");
                return Ok(());
            }
            "-h" | "--help" => {
                eprintln!("usage: vyges-opendb report-disconnected-pins --input <f.odb>");
                return Ok(());
            }
            other => return Err(format!("report-disconnected-pins: unknown argument: {other}").into()),
        }
    }
    let input = input.ok_or("report-disconnected-pins: --input <f.odb> required")?;
    let db = Db::open(&input)?;
    let pins = report::disconnected_pins(&db);
    eprintln!("report-disconnected-pins: {} disconnected", pins.len());
    println!("{}", serde_json::to_string_pretty(&pins)?);
    Ok(())
}

#[derive(Deserialize, Default)]
struct PowerConnectionsConfig {
    #[serde(rename = "SET_POWER_CONNECTIONS", default)]
    set_power_connections: Vec<eco::PowerConnection>,
}

const SET_POWER_CONNECTIONS_DESCRIBE: &str = r#"{
  "step": "set-power-connections",
  "summary": "Wire instance pins to (power) nets in a .odb (database surgery).",
  "librelane_equivalent": "Odb.SetPowerConnections",
  "unix_only": true,
  "args": [
    { "name": "--input",  "kind": "input",  "type": "path", "required": true,  "description": "input .odb design" },
    { "name": "--output", "kind": "output", "type": "path", "required": true,  "description": "output .odb" },
    { "name": "--config", "kind": "config", "type": "path", "required": false, "description": "JSON with SET_POWER_CONNECTIONS (default: no-op)" }
  ],
  "config_schema": {
    "SET_POWER_CONNECTIONS": {
      "type": "array",
      "item": {
        "instance": { "type": "string", "description": "instance name" },
        "pin":      { "type": "string", "description": "power/ground pin, e.g. VPWR" },
        "net":      { "type": "string", "description": "net to connect it to, e.g. VDD" }
      }
    }
  }
}"#;

/// `set-power-connections --input <in.odb> --output <out.odb> [--config <cfg.json>] | --describe`.
fn set_power_connections(mut args: impl Iterator<Item = String>) -> Result<(), Fail> {
    let (mut input, mut output, mut config) = (None, None, None);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--input" | "-i" => input = args.next(),
            "--output" | "-o" => output = args.next(),
            "--config" | "-c" => config = args.next(),
            "--describe" => {
                println!("{SET_POWER_CONNECTIONS_DESCRIBE}");
                return Ok(());
            }
            "-h" | "--help" => {
                eprintln!("usage: vyges-opendb set-power-connections --input <in.odb> --output <out.odb> --config <cfg.json>");
                return Ok(());
            }
            other => return Err(format!("set-power-connections: unknown argument: {other}").into()),
        }
    }
    let input = input.ok_or("set-power-connections: --input <in.odb> required")?;
    let output = output.ok_or("set-power-connections: --output <out.odb> required")?;
    let cfg: PowerConnectionsConfig = match config {
        Some(p) => serde_json::from_str(&std::fs::read_to_string(&p)?)?,
        None => PowerConnectionsConfig::default(),
    };
    let mut db = Db::open(&input)?;
    let n = eco::set_power_connections(&mut db, &cfg.set_power_connections)?;
    db.write(&output)?;
    eprintln!("set-power-connections: connected {n} pin(s), {input} -> {output}");
    Ok(())
}

#[derive(Deserialize, Default)]
struct ObstructionsConfig {
    #[serde(rename = "OBSTRUCTIONS", default)]
    obstructions: Vec<eco::Obstruction>,
}

const ADD_OBSTRUCTIONS_DESCRIBE: &str = r#"{
  "step": "add-obstructions",
  "summary": "Add routing/PDN obstruction rectangles to a .odb (database surgery).",
  "librelane_equivalent": "Odb.AddPDNObstructions / Odb.AddRoutingObstructions",
  "unix_only": true,
  "args": [
    { "name": "--input",  "kind": "input",  "type": "path", "required": true,  "description": "input .odb design" },
    { "name": "--output", "kind": "output", "type": "path", "required": true,  "description": "output .odb" },
    { "name": "--config", "kind": "config", "type": "path", "required": false, "description": "JSON with OBSTRUCTIONS (default: no-op)" }
  ],
  "config_schema": {
    "OBSTRUCTIONS": {
      "type": "array",
      "item": {
        "layer": { "type": "string",  "description": "tech layer name, e.g. met1" },
        "llx":   { "type": "integer", "description": "lower-left x (DBU)" },
        "lly":   { "type": "integer", "description": "lower-left y (DBU)" },
        "urx":   { "type": "integer", "description": "upper-right x (DBU)" },
        "ury":   { "type": "integer", "description": "upper-right y (DBU)" }
      }
    }
  }
}"#;

/// `add-obstructions --input <in.odb> --output <out.odb> [--config <cfg.json>] | --describe`.
fn add_obstructions(mut args: impl Iterator<Item = String>) -> Result<(), Fail> {
    let (mut input, mut output, mut config) = (None, None, None);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--input" | "-i" => input = args.next(),
            "--output" | "-o" => output = args.next(),
            "--config" | "-c" => config = args.next(),
            "--describe" => {
                println!("{ADD_OBSTRUCTIONS_DESCRIBE}");
                return Ok(());
            }
            "-h" | "--help" => {
                eprintln!("usage: vyges-opendb add-obstructions --input <in.odb> --output <out.odb> --config <cfg.json>");
                return Ok(());
            }
            other => return Err(format!("add-obstructions: unknown argument: {other}").into()),
        }
    }
    let input = input.ok_or("add-obstructions: --input <in.odb> required")?;
    let output = output.ok_or("add-obstructions: --output <out.odb> required")?;
    let cfg: ObstructionsConfig = match config {
        Some(p) => serde_json::from_str(&std::fs::read_to_string(&p)?)?,
        None => ObstructionsConfig::default(),
    };
    let mut db = Db::open(&input)?;
    let n = eco::add_obstructions(&mut db, &cfg.obstructions)?;
    db.write(&output)?;
    eprintln!("add-obstructions: added {n} obstruction(s), {input} -> {output}");
    Ok(())
}

/// `remove-obstructions --input <in.odb> --output <out.odb>` — clear all obstructions.
fn remove_obstructions(mut args: impl Iterator<Item = String>) -> Result<(), Fail> {
    let (mut input, mut output) = (None, None);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--input" | "-i" => input = args.next(),
            "--output" | "-o" => output = args.next(),
            "-h" | "--help" => {
                eprintln!("usage: vyges-opendb remove-obstructions --input <in.odb> --output <out.odb>");
                return Ok(());
            }
            other => return Err(format!("remove-obstructions: unknown argument: {other}").into()),
        }
    }
    let input = input.ok_or("remove-obstructions: --input <in.odb> required")?;
    let output = output.ok_or("remove-obstructions: --output <out.odb> required")?;
    let mut db = Db::open(&input)?;
    let n = eco::remove_obstructions(&mut db);
    db.write(&output)?;
    eprintln!("remove-obstructions: removed {n} obstruction(s), {input} -> {output}");
    Ok(())
}

const WRITE_VERILOG_HEADER_DESCRIBE: &str = r#"{
  "step": "write-verilog-header",
  "summary": "Emit a Verilog module header (ports + directions) from a .odb (read-only).",
  "librelane_equivalent": "Odb.WriteVerilogHeader",
  "unix_only": true,
  "args": [
    { "name": "--input",  "kind": "input",  "type": "path", "required": true,  "description": "input .odb design" },
    { "name": "--output", "kind": "output", "type": "path", "required": false, "description": "write here instead of stdout" }
  ],
  "output": "Verilog module header text"
}"#;

/// `write-verilog-header --input <f.odb> [--output <f.v>] | --describe`.
fn write_verilog_header(mut args: impl Iterator<Item = String>) -> Result<(), Fail> {
    let (mut input, mut output) = (None, None);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--input" | "-i" => input = args.next(),
            "--output" | "-o" => output = args.next(),
            "--describe" => {
                println!("{WRITE_VERILOG_HEADER_DESCRIBE}");
                return Ok(());
            }
            "-h" | "--help" => {
                eprintln!("usage: vyges-opendb write-verilog-header --input <f.odb> [--output <f.v>]");
                return Ok(());
            }
            other => return Err(format!("write-verilog-header: unknown argument: {other}").into()),
        }
    }
    let input = input.ok_or("write-verilog-header: --input <f.odb> required")?;
    let header = report::verilog_header(&Db::open(&input)?);
    match output {
        Some(p) => std::fs::write(&p, header)?,
        None => print!("{header}"),
    }
    Ok(())
}

const APPLY_ECO_PLAN_DESCRIBE: &str = r#"{
  "step": "apply-eco-plan",
  "summary": "Replay a timing-repair ECO plan (vyges-eco-plan-v1, as emitted by vyges-sta-si) into the design. All-or-nothing: any failing fix rolls the whole plan back. Does NOT legalize — run detailed placement, re-extract parasitics and re-time afterwards.",
  "librelane_equivalent": null,
  "unix_only": true,
  "args": [
    { "name": "--input", "kind": "input", "type": "path", "required": true, "description": "input .odb design" },
    { "name": "--plan", "kind": "input", "type": "path", "required": true, "description": "ECO plan JSON (vyges-eco-plan-v1)" },
    { "name": "--output", "kind": "output", "type": "path", "required": true, "description": "output .odb" },
    { "name": "--any-design", "kind": "flag", "type": "bool", "required": false, "description": "skip the plan/design name check" }
  ],
  "output": "JSON { applied, inserted: [names] } on stdout"
}"#;

/// `apply-eco-plan --input <in.odb> --plan <p.json> --output <out.odb> | --describe`.
fn apply_eco_plan(mut args: impl Iterator<Item = String>) -> Result<(), Fail> {
    let (mut input, mut plan, mut output) = (None, None, None);
    let mut strict = true;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--input" | "-i" => input = args.next(),
            "--plan" | "-p" => plan = args.next(),
            "--output" | "-o" => output = args.next(),
            "--any-design" => strict = false,
            "--describe" => {
                println!("{APPLY_ECO_PLAN_DESCRIBE}");
                return Ok(());
            }
            "-h" | "--help" => {
                eprintln!("usage: vyges-opendb apply-eco-plan --input <in.odb> --plan <plan.json> --output <out.odb> [--any-design]");
                return Ok(());
            }
            other => return Err(format!("apply-eco-plan: unknown argument: {other}").into()),
        }
    }
    let input = input.ok_or("apply-eco-plan: --input <in.odb> required")?;
    let plan_path = plan.ok_or("apply-eco-plan: --plan <plan.json> required")?;
    let output = output.ok_or("apply-eco-plan: --output <out.odb> required")?;

    let plan_json = std::fs::read_to_string(&plan_path)
        .map_err(|e| format!("apply-eco-plan: cannot read {plan_path}: {e}"))?;
    let mut db = Db::open(&input)?;
    // OpenDB logs to stdout; keep this subcommand's stdout parseable.
    let (res, logs) = db.with_captured_logs(|db| {
        vyges_opendb::eco::apply_eco_plan(db, &plan_json, strict)
    });
    if !logs.is_empty() {
        eprint!("{logs}");
    }
    let applied = res?;
    db.write(&output)?;
    println!(
        "{}",
        serde_json::json!({ "applied": applied.applied, "inserted": applied.inserted })
    );
    Ok(())
}

const READ_3DBLOX_DESCRIBE: &str = r#"{
  "name": "read-3dblox",
  "summary": "read a 3Dblox 2.5D/3D assembly description into an OpenDB database",
  "maturity": "experimental",
  "provenance_limitations": [
      "Nested instance paths and virtual bonds (`bot: ~`) are read and reported as unrepresented.",
      "Polygonal regions collapse to their bounding rectangle; each loss is reported by name.",
      "One technology per database: a stack whose dies use different processes cannot be fully represented."
  ],
  "invocation": {
    "args_template": ["read-3dblox", "--input", "{input}", "--output", "{output}"],
    "optional": [ { "arg": "into", "flag": "--into" } ],
    "emits_json": false
  },
  "inputs": {
    "type": "object",
    "required": ["input", "output"],
    "properties": {
      "input":  { "type": "string", "description": "3Dblox assembly file (.3dbx)" },
      "output": { "type": "string", "description": "database to write" },
      "into":   { "type": "string", "description": "start from this database instead of an empty one" }
    }
  },
  "artifacts": [ { "role": "odb", "field": "output" } ]
}
"#;

const CHECK_3DBLOX_DESCRIBE: &str = r#"{
  "step": "check-3dblox",
  "summary": "3D/chiplet structural sign-off lint over a multi-die assembly: logical connectivity, floating chips, overlapping dies, unused internal_ext regions, connection-region overlap and mating-surface gap vs connection thickness, bump alignment, and alignment markers. Read-only: reports violations as markers, never modifies the design.",
  "librelane_equivalent": null,
  "unix_only": true,
  "args": [
    { "name": "--input", "kind": "input", "type": "path", "required": true, "description": "input .odb design" }
  ],
  "output": "JSON { violations, categories: [{ category, count, markers: [{ name, comment }] }] } on stdout; exit 0 regardless of findings"
}"#;

/// `check-3dblox --input <f.odb> | --describe`.
/// Read a 3Dblox assembly into a database.
///
/// Without this the reader was library-only: everything needed to load a `.3dbx` existed, and no
/// one holding one could do anything with the shipped binary. `read-3dblox` + `check-3dblox` is
/// the pipeline — an assembly description in, structural findings out.
///
/// `--into` starts from an existing database (so a stack can be read on top of collateral that
/// is already loaded); without it the read starts from an empty one, which is the common case.
///
/// Behind `gen-write` because building an assembly means constructing chips through the generated
/// setter surface. Release builds enable it; a default build does not, and says so.
#[cfg(feature = "gen-write")]
fn read_3dblox(mut args: impl Iterator<Item = String>) -> Result<(), Fail> {
    let (mut input, mut output, mut into) = (None, None, None);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--input" | "-i" => input = args.next(),
            "--output" | "-o" => output = args.next(),
            "--into" => into = args.next(),
            "--describe" => {
                println!("{READ_3DBLOX_DESCRIBE}");
                return Ok(());
            }
            "-h" | "--help" => {
                eprintln!(
                    "usage: vyges-opendb read-3dblox --input <f.3dbx> --output <out.odb> \
                     [--into <in.odb>]"
                );
                return Ok(());
            }
            other => return Err(format!("read-3dblox: unknown argument: {other}").into()),
        }
    }
    let input = input.ok_or("read-3dblox: --input <f.3dbx> required")?;
    let output = output.ok_or("read-3dblox: --output <out.odb> required")?;
    let mut db = match &into {
        Some(p) => Db::open(p)?,
        None => Db::new(),
    };
    let lossy = db.read_3dblox(&input)?;
    db.write(&output)?;
    // What the format said and the database could not hold. Silence here would be the lie: a
    // polygonal bond region squared off to its bounding box is a different assembly.
    if !lossy.is_empty() {
        eprintln!(
            "read-3dblox: {} element(s) the database cannot represent:",
            lossy.len()
        );
        for l in &lossy {
            eprintln!("  {l}");
        }
    }
    eprintln!("read-3dblox: {input} -> {output}");
    Ok(())
}

#[cfg(not(feature = "gen-write"))]
fn read_3dblox(mut args: impl Iterator<Item = String>) -> Result<(), Fail> {
    // --describe must answer on any build: a caller discovering the command surface should not
    // have to guess whether it exists from a build flag it cannot see.
    for a in args.by_ref() {
        if a == "--describe" {
            println!("{READ_3DBLOX_DESCRIBE}");
            return Ok(());
        }
    }
    Err("read-3dblox requires a build with --features gen-write (constructing an assembly \
         uses the L2/write surface); released binaries have it"
        .into())
}

/// Collect `check_3dblox` markers as (category, name) pairs.
///
/// Shared with `check-3dblox` rather than reimplemented: the seven category names are a list the
/// linter owns, and two copies of it would drift the moment upstream adds an eighth.
#[cfg(unix)]
fn blox_findings(db: &Db) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for check in BLOX_CHECKS {
        let path = format!("3DBlox/{check}");
        let count = vyges_opendb::registry::get(db, "dbMarkerCategory", "get_marker_count",
                                                &[path.clone()])
            .ok()
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        for i in 0..count {
            let name = vyges_opendb::registry::get(db, "dbMarker", "get_name",
                                                   &[path.clone(), i.to_string()])
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default();
            out.push((check.to_string(), name));
        }
    }
    out
}

/// Draw an assembly. Accepts the interchange file directly, so going from a `.3dbx` someone sent
/// you to a picture is one command rather than three.
///
/// A `.odb` needs `--top` because the database has no getter for the top chip — the name is not
/// recoverable from the file, so guessing it would mean drawing an empty page and calling it a
/// clean assembly.
#[cfg(all(unix, feature = "gen-write"))]
fn view_3dblox(mut args: impl Iterator<Item = String>) -> Result<(), Fail> {
    use vyges_opendb::view3d::{to_png, to_svg, Assembly3d};
    let (mut input, mut output, mut top) = (None, None, None);
    let mut scale = 2.0f64;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--input" | "-i" => input = args.next(),
            "--output" | "-o" => output = args.next(),
            "--top" => top = args.next(),
            "--scale" => {
                let v = args.next().ok_or("view-3dblox: --scale needs a number")?;
                scale = v
                    .parse()
                    .map_err(|_| format!("view-3dblox: --scale: not a number: {v}"))?;
            }
            "--describe" => {
                println!("{VIEW_3DBLOX_DESCRIBE}");
                return Ok(());
            }
            "-h" | "--help" => {
                eprintln!(
                    "usage: vyges-opendb view-3dblox --input <f.3dbx|f.odb> \
                     --output <out.svg|out.png> [--top <chip>] [--scale <n>]"
                );
                return Ok(());
            }
            other => return Err(format!("view-3dblox: unknown argument: {other}").into()),
        }
    }
    let input = input.ok_or("view-3dblox: --input <f.3dbx|f.odb> required")?;
    let output = output.ok_or("view-3dblox: --output <out.svg|out.png> required")?;
    // Extension picks the format. A --format flag that could disagree with the filename is a
    // way to write PNG bytes into a file called .svg, which nothing downstream will open.
    let png = match std::path::Path::new(&output)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => true,
        Some("svg") | None => false,
        Some(other) => {
            return Err(format!(
                "view-3dblox: unknown output format '.{other}' — use .svg or .png"
            )
            .into())
        }
    };

    let is_dbx = input.ends_with(".3dbx");
    let (mut db, top) = if is_dbx {
        // The assembly file names its own design, so --top is redundant here.
        let raw = std::fs::read_to_string(&input)?;
        let dbx = vyges_opendb::blox::parse_dbx(&input, &raw).map_err(|e| format!("{e}"))?;
        let name = dbx.design_name.clone();
        let mut db = Db::new();
        let (r, logs) = db.with_captured_logs(|db| db.read_3dblox(&input));
        if !logs.is_empty() {
            eprint!("{logs}");
        }
        r?;
        (db, top.unwrap_or(name))
    } else {
        let top = top.ok_or(
            "view-3dblox: --top <chip> required for a .odb input (the database has no top-chip \
             getter; read-3dblox prints the design name)",
        )?;
        (Db::open(&input)?, top)
    };

    // Lint first so the drawing can carry the findings. Failing to lint is not a reason to
    // refuse a picture — a malformed assembly is exactly when someone wants to look at it.
    let (violations, logs) = db.with_captured_logs(|db| db.check_3dblox());
    if !logs.is_empty() {
        eprint!("{logs}");
    }
    let findings = match violations {
        Ok(_) => blox_findings(&db),
        Err(e) => {
            eprintln!("view-3dblox: lint skipped ({e}); drawing geometry only");
            Vec::new()
        }
    };

    let asm = Assembly3d::read(&db, &top)?.with_findings(findings);
    if asm.dies.is_empty() {
        eprintln!(
            "view-3dblox: warning: no chip instances under top chip '{top}' — the drawing will \
             be empty. Is --top the assembly rather than a die?"
        );
    }
    // A database with no precision set would divide every dimension by zero and print `inf`.
    let dbu = match db.dbu_per_micron() {
        0 => 1000.0,
        d => f64::from(d),
    };
    if png {
        std::fs::write(&output, to_png(&asm, dbu, scale))?;
    } else {
        std::fs::write(&output, to_svg(&asm, dbu))?;
    }
    eprintln!(
        "view-3dblox: {input} -> {output} ({} die(s), {} bond(s), {} finding(s))",
        asm.dies.len(),
        asm.bonds.len(),
        asm.findings.len()
    );
    Ok(())
}

#[cfg(all(unix, not(feature = "gen-write")))]
fn view_3dblox(mut args: impl Iterator<Item = String>) -> Result<(), Fail> {
    for a in args.by_ref() {
        if a == "--describe" {
            println!("{VIEW_3DBLOX_DESCRIBE}");
            return Ok(());
        }
    }
    Err("view-3dblox requires a build with --features gen-write; released binaries have it".into())
}

const BLOX_CHECKS: [&str; 7] = [
    "Logical Connectivity",
    "Floating chips",
    "Overlapping chips",
    "Unused internal_ext",
    "Connection regions",
    "Bump Alignment",
    "Alignment Markers",
];

const VIEW_3DBLOX_DESCRIBE: &str = r#"{
  "name": "view-3dblox",
  "summary": "draw a chiplet assembly as SVG or PNG: cross-section, plan, and linter findings",
  "maturity": "experimental",
  "provenance_limitations": [
      "The Z axis is exaggerated so the stack is legible; the factor is printed on the drawing and dimensions must not be measured off it.",
      "Geometry only: no routing, no bumps drawn individually, no per-die layer stack.",
      "A .odb input needs --top because the database has no top-chip getter."
  ],
  "invocation": {
    "args_template": ["view-3dblox", "--input", "{input}", "--output", "{output}"],
    "optional": [ { "arg": "top", "flag": "--top" }, { "arg": "scale", "flag": "--scale" } ],
    "emits_json": false
  },
  "inputs": {
    "type": "object",
    "required": ["input", "output"],
    "properties": {
      "input":  { "type": "string", "description": "3Dblox assembly (.3dbx) or database (.odb)" },
      "output": { "type": "string", "description": "file to write; .svg or .png picks the format" },
      "top":    { "type": "string", "description": "top chip name; required for .odb input" },
      "scale":  { "type": "number", "default": 2.0, "description": "PNG device pixels per drawing unit; ignored for SVG" }
    }
  },
  "artifacts": [ { "role": "drawing", "field": "output" } ]
}
"#;

/// Check one die-to-die interface from two bump maps.
///
/// Bump maps rather than a database because that is what a user *has*: two dies hardened in
/// separate runs produce two `.bmap` files, and "do these interfaces agree?" is answerable before
/// either die is placed into an assembly.
#[cfg(unix)]
fn check_d2d(mut args: impl Iterator<Item = String>) -> Result<(), Fail> {
    use vyges_opendb::d2d::{check, check_placed, BumpMap, Placement, Transform};
    let (mut top, mut bottom, mut tol) = (None, None, None);
    let mut input = None;
    let mut tf = Transform::default();
    let num = |a: Option<String>, what: &str| -> Result<f64, Fail> {
        let v = a.ok_or_else(|| format!("check-d2d: {what} needs a number"))?;
        v.parse::<f64>()
            .map_err(|_| format!("check-d2d: {what}: not a number: {v}").into())
    };
    while let Some(a) = args.next() {
        match a.as_str() {
            "--input" | "-i" => input = args.next(),
            "--top" => top = args.next(),
            "--bottom" => bottom = args.next(),
            "--offset-x" => tf.dx = num(args.next(), "--offset-x")?,
            "--offset-y" => tf.dy = num(args.next(), "--offset-y")?,
            "--flip-x" => tf.flip_x = true,
            "--tolerance" => tol = Some(num(args.next(), "--tolerance")?),
            "--describe" => {
                println!("{CHECK_D2D_DESCRIBE}");
                return Ok(());
            }
            "-h" | "--help" => {
                eprintln!(
                    "usage: vyges-opendb check-d2d --input <stack.3dbx>\n   \
                     or: vyges-opendb check-d2d --top <a.bmap> --bottom <b.bmap> \
                     [--offset-x <um>] [--offset-y <um>] [--flip-x] [--tolerance <um>]"
                );
                return Ok(());
            }
            other => return Err(format!("check-d2d: unknown argument: {other}").into()),
        }
    }
    // Assembly mode. The placement comes from the file, so nothing about how the dies sit has
    // to be asserted on the command line — which is where the two-file form is easiest to get
    // wrong, and where getting it wrong reports a dead interface as clean.
    if let Some(input) = input {
        if top.is_some() || bottom.is_some() {
            return Err("check-d2d: --input reads both sides from the assembly; \
                        drop --top/--bottom"
                .into());
        }
        let pairs = vyges_opendb::blox::bonded_pairs(&input)?;
        let mut interfaces = Vec::new();
        let mut violations = 0usize;
        let mut skipped = Vec::new();
        for p in &pairs {
            let load = |s: &vyges_opendb::blox::BondedSide| -> Result<Option<(BumpMap, Placement)>, Fail> {
                let (Some(bmap), Some((w, h))) = (&s.bmap, s.design_area) else {
                    return Ok(None);
                };
                Ok(Some((
                    BumpMap::load(bmap).map_err(|e| format!("check-d2d: {bmap}: {e}"))?,
                    Placement {
                        orient: s.orient.clone(),
                        loc_x: s.loc.0,
                        loc_y: s.loc.1,
                        die_w: w,
                        die_h: h,
                    },
                )))
            };
            // A bond whose surfaces declare no bump map is not a failure — most `internal`
            // regions carry none — but it is also not checked, and saying so is the difference
            // between "clean" and "not looked at".
            let (Some((tm, tp)), Some((bm, bp))) = (load(&p.top)?, load(&p.bottom)?) else {
                skipped.push(format!(
                    "{}: no bump map on {} or {}{}",
                    p.connection,
                    p.top.region,
                    p.bottom.region,
                    if p.top.design_area.is_none() || p.bottom.design_area.is_none() {
                        " (or the chiplet declares no design_area)"
                    } else {
                        ""
                    }
                ));
                continue;
            };
            let r = check_placed(&tm, &tp, &bm, &bp, tol).map_err(|e| format!("check-d2d: {e}"))?;
            violations += r.violations();
            let mut j = r.to_json();
            j["connection"] = serde_json::json!(p.connection);
            j["top"] = serde_json::json!(format!("{}.{}", p.top.inst, p.top.region));
            j["bottom"] = serde_json::json!(format!("{}.{}", p.bottom.inst, p.bottom.region));
            interfaces.push(j);
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "violations": violations,
                "interfaces_checked": interfaces.len(),
                "interfaces_skipped": skipped,
                "interfaces": interfaces,
            }))?
        );
        for s in &skipped {
            eprintln!("check-d2d: not checked — {s}");
        }
        return Ok(());
    }

    let top = top.ok_or("check-d2d: --input <stack.3dbx>, or --top and --bottom, required")?;
    let bottom = bottom.ok_or("check-d2d: --bottom <b.bmap> required")?;

    let report = check(
        &BumpMap::load(&top).map_err(|e| format!("check-d2d: {top}: {e}"))?,
        &BumpMap::load(&bottom).map_err(|e| format!("check-d2d: {bottom}: {e}"))?,
        tf,
        tol,
    );
    println!("{}", serde_json::to_string_pretty(&report.to_json())?);
    // A malformed line is not a violation, but it does mean the check saw less than the file
    // holds — silence there would overstate the coverage of a clean result.
    if !report.parse_errors.is_empty() {
        eprintln!(
            "check-d2d: {} unparseable line(s); those bumps were not checked",
            report.parse_errors.len()
        );
    }
    Ok(())
}

const CHECK_D2D_DESCRIBE: &str = r#"{
  "name": "check-d2d",
  "summary": "check a die-to-die interface from two bump maps: unmated bumps, misalignment, net and cell mismatch",
  "maturity": "experimental",
  "provenance_limitations": [
      "With --input the frame comes from the assembly. In the two-file form the relative placement is NOT inferred — pass --offset-x/--offset-y/--flip-x. Either way the frame used is echoed in the report.",
      "A bonded pair whose regions declare no bmap is listed under interfaces_skipped, not counted as clean.",
      "Compares bump maps, not extracted layout: it checks what the maps claim, not what was fabricated.",
      "Default tolerance is half the smaller bump pitch, derived from the maps; --tolerance overrides."
  ],
  "invocation": {
    "args_template": ["check-d2d", "--input", "{input}"],
    "optional": [
      { "arg": "top",      "flag": "--top" },
      { "arg": "bottom",   "flag": "--bottom" },
      { "arg": "offset_x", "flag": "--offset-x" },
      { "arg": "offset_y", "flag": "--offset-y" },
      { "arg": "flip_x",   "flag": "--flip-x", "kind": "flag" },
      { "arg": "tolerance","flag": "--tolerance" }
    ],
    "emits_json": true
  },
  "inputs": {
    "type": "object",
    "properties": {
      "input":     { "type": "string", "description": "3Dblox assembly (.3dbx) — checks every bonded pair, deriving each die's frame from its placement" },
      "top":       { "type": "string", "description": "bump map of the upper die (.bmap)" },
      "bottom":    { "type": "string", "description": "bump map of the lower die (.bmap)" },
      "offset_x":  { "type": "number", "description": "shift the bottom map, microns" },
      "offset_y":  { "type": "number", "description": "shift the bottom map, microns" },
      "flip_x":    { "type": "boolean", "description": "mirror the bottom map in X (face-to-face bonding)" },
      "tolerance": { "type": "number", "description": "match radius in microns; default is half the bump pitch" }
    }
  }
}
"#;

fn check_3dblox(mut args: impl Iterator<Item = String>) -> Result<(), Fail> {
    let mut input = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--input" | "-i" => input = args.next(),
            "--describe" => {
                println!("{CHECK_3DBLOX_DESCRIBE}");
                return Ok(());
            }
            "-h" | "--help" => {
                eprintln!("usage: vyges-opendb check-3dblox --input <f.odb>");
                return Ok(());
            }
            other => return Err(format!("check-3dblox: unknown argument: {other}").into()),
        }
    }
    let input = input.ok_or("check-3dblox: --input <f.odb> required")?;
    let mut db = Db::open(&input)?;
    // OpenDB writes its human-readable warnings to stdout, which would interleave with the JSON
    // below and make this subcommand unparseable. Capture them and re-emit on stderr, where
    // diagnostics belong when stdout is a data channel.
    let (violations, logs) = db.with_captured_logs(|db| db.check_3dblox());
    if !logs.is_empty() {
        eprint!("{logs}");
    }
    let violations = violations?;

    // Report the findings themselves, not just counts. The markers live in the in-memory
    // database and are never written back, so a caller could not fetch them in a second
    // command — this output has to be self-contained to be useful.
    let get = |class: &str, field: &str, keys: &[String]| {
        vyges_opendb::registry::get(&db, class, field, keys).ok()
    };
    let mut categories = Vec::new();
    for check in [
        "Logical Connectivity",
        "Floating chips",
        "Overlapping chips",
        "Unused internal_ext",
        "Connection regions",
        "Bump Alignment",
        "Alignment Markers",
    ] {
        let path = format!("3DBlox/{check}");
        let count = get("dbMarkerCategory", "get_marker_count", &[path.clone()])
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        if count == 0 {
            continue; // checks that passed are omitted rather than listed as zero
        }
        let markers: Vec<serde_json::Value> = (0..count)
            .map(|i| {
                let keys = [path.clone(), i.to_string()];
                serde_json::json!({
                    "name": get("dbMarker", "get_name", &keys),
                    "comment": get("dbMarker", "get_comment", &keys),
                })
            })
            .collect();
        categories.push(serde_json::json!({
            "category": check, "count": count, "markers": markers,
        }));
    }
    println!(
        "{}",
        serde_json::json!({ "violations": violations, "categories": categories })
    );
    Ok(())
}

const REPORT_WIRE_LENGTH_DESCRIBE: &str = r#"{
  "step": "report-wire-length",
  "summary": "Report the total routed wire length (DBU) as JSON (read-only).",
  "librelane_equivalent": "Odb.ReportWireLength",
  "unix_only": true,
  "args": [
    { "name": "--input", "kind": "input", "type": "path", "required": true, "description": "input .odb design" }
  ],
  "output": "JSON { total_wire_length_dbu } on stdout"
}"#;

/// `report-wire-length --input <f.odb> | --describe`.
fn report_wire_length(mut args: impl Iterator<Item = String>) -> Result<(), Fail> {
    let mut input = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--input" | "-i" => input = args.next(),
            "--describe" => {
                println!("{REPORT_WIRE_LENGTH_DESCRIBE}");
                return Ok(());
            }
            "-h" | "--help" => {
                eprintln!("usage: vyges-opendb report-wire-length --input <f.odb>");
                return Ok(());
            }
            other => return Err(format!("report-wire-length: unknown argument: {other}").into()),
        }
    }
    let input = input.ok_or("report-wire-length: --input <f.odb> required")?;
    let total = Db::open(&input)?.total_wire_length();
    println!("{{ \"total_wire_length_dbu\": {total} }}");
    Ok(())
}

const REPORT_CONNECTIVITY_DESCRIBE: &str = r#"{
  "step": "report-connectivity",
  "summary": "Dump the netlist connectivity graph (per-net sig-type, special flag, and pins) as JSON, highest-degree net first (read-only).",
  "librelane_equivalent": null,
  "unix_only": true,
  "args": [
    { "name": "--input", "kind": "input", "type": "path", "required": true, "description": "input .odb design" }
  ],
  "output": "JSON array of { net, sig_type, special, iterms, bterms, degree } on stdout"
}"#;

/// `report-connectivity --input <f.odb> | --describe` — read-only netlist graph dump (JSON).
fn report_connectivity(mut args: impl Iterator<Item = String>) -> Result<(), Fail> {
    let mut input = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--input" | "-i" => input = args.next(),
            "--describe" => {
                println!("{REPORT_CONNECTIVITY_DESCRIBE}");
                return Ok(());
            }
            "-h" | "--help" => {
                eprintln!("usage: vyges-opendb report-connectivity --input <f.odb>");
                return Ok(());
            }
            other => return Err(format!("report-connectivity: unknown argument: {other}").into()),
        }
    }
    let input = input.ok_or("report-connectivity: --input <f.odb> required")?;
    let db = Db::open(&input)?;
    println!("{}", serde_json::to_string_pretty(&report::net_connectivity(&db))?);
    Ok(())
}

#[derive(Deserialize, Default)]
struct IoPlacementConfig {
    #[serde(rename = "CUSTOM_IO_PLACEMENT", default)]
    custom_io_placement: Vec<eco::IoPlacement>,
}

const CUSTOM_IO_PLACEMENT_DESCRIBE: &str = r#"{
  "step": "custom-io-placement",
  "summary": "Place I/O port pins at fixed locations/layers in a .odb (database surgery).",
  "librelane_equivalent": "Odb.CustomIOPlacement",
  "unix_only": true,
  "args": [
    { "name": "--input",  "kind": "input",  "type": "path", "required": true,  "description": "input .odb design" },
    { "name": "--output", "kind": "output", "type": "path", "required": true,  "description": "output .odb" },
    { "name": "--config", "kind": "config", "type": "path", "required": false, "description": "JSON with CUSTOM_IO_PLACEMENT (default: no-op)" }
  ],
  "config_schema": {
    "CUSTOM_IO_PLACEMENT": {
      "type": "array",
      "item": {
        "port":  { "type": "string",  "description": "port (bterm) name" },
        "layer": { "type": "string",  "description": "tech layer, e.g. met3" },
        "llx":   { "type": "integer", "description": "lower-left x (DBU)" },
        "lly":   { "type": "integer", "description": "lower-left y (DBU)" },
        "urx":   { "type": "integer", "description": "upper-right x (DBU)" },
        "ury":   { "type": "integer", "description": "upper-right y (DBU)" }
      }
    }
  }
}"#;

/// `custom-io-placement --input <in.odb> --output <out.odb> [--config <cfg.json>] | --describe`.
fn custom_io_placement(mut args: impl Iterator<Item = String>) -> Result<(), Fail> {
    let (mut input, mut output, mut config) = (None, None, None);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--input" | "-i" => input = args.next(),
            "--output" | "-o" => output = args.next(),
            "--config" | "-c" => config = args.next(),
            "--describe" => {
                println!("{CUSTOM_IO_PLACEMENT_DESCRIBE}");
                return Ok(());
            }
            "-h" | "--help" => {
                eprintln!("usage: vyges-opendb custom-io-placement --input <in.odb> --output <out.odb> --config <cfg.json>");
                return Ok(());
            }
            other => return Err(format!("custom-io-placement: unknown argument: {other}").into()),
        }
    }
    let input = input.ok_or("custom-io-placement: --input <in.odb> required")?;
    let output = output.ok_or("custom-io-placement: --output <out.odb> required")?;
    let cfg: IoPlacementConfig = match config {
        Some(p) => serde_json::from_str(&std::fs::read_to_string(&p)?)?,
        None => IoPlacementConfig::default(),
    };
    let mut db = Db::open(&input)?;
    let n = eco::custom_io_placement(&mut db, &cfg.custom_io_placement)?;
    db.write(&output)?;
    eprintln!("custom-io-placement: placed {n} port(s), {input} -> {output}");
    Ok(())
}

const WRITE_DEF_DESCRIBE: &str = r#"{
  "step": "write-def",
  "summary": "Export a placed design to a DEF 5.8 file (libodb v1 LEF/DEF I/O).",
  "librelane_equivalent": "odb write_def",
  "unix_only": true,
  "args": [
    { "name": "--input",  "kind": "input",  "type": "path", "required": true, "description": "input .odb design" },
    { "name": "--output", "kind": "output", "type": "path", "required": true, "description": "output .def file" }
  ]
}"#;

/// `write-def --input <f.odb> --output <f.def> | --describe`.
fn write_def(mut args: impl Iterator<Item = String>) -> Result<(), Fail> {
    let (mut input, mut output) = (None, None);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--input" | "-i" => input = args.next(),
            "--output" | "-o" => output = args.next(),
            "--describe" => {
                println!("{WRITE_DEF_DESCRIBE}");
                return Ok(());
            }
            "-h" | "--help" => {
                eprintln!("usage: vyges-opendb write-def --input <f.odb> --output <f.def>");
                return Ok(());
            }
            other => return Err(format!("write-def: unknown argument: {other}").into()),
        }
    }
    let input = input.ok_or("write-def: --input <f.odb> required")?;
    let output = output.ok_or("write-def: --output <f.def> required")?;
    Db::open(&input)?.write_def(&output)?;
    eprintln!("write-def: {input} -> {output}");
    Ok(())
}

const READ_DEF_DESCRIBE: &str = r#"{
  "step": "read-def",
  "summary": "Import a DEF into an existing design (its tech/libs) — libodb v1 LEF/DEF I/O.",
  "librelane_equivalent": "odb read_def",
  "unix_only": true,
  "args": [
    { "name": "--input",  "kind": "input",  "type": "path", "required": true, "description": "input .odb (provides tech + libs)" },
    { "name": "--def",    "kind": "input",  "type": "path", "required": true, "description": "DEF file to import" },
    { "name": "--output", "kind": "output", "type": "path", "required": true, "description": "output .odb" }
  ]
}"#;

/// `read-def --input <in.odb> --def <f.def> --output <out.odb> | --describe`.
fn read_def(mut args: impl Iterator<Item = String>) -> Result<(), Fail> {
    let (mut input, mut def, mut output) = (None, None, None);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--input" | "-i" => input = args.next(),
            "--def" => def = args.next(),
            "--output" | "-o" => output = args.next(),
            "--describe" => {
                println!("{READ_DEF_DESCRIBE}");
                return Ok(());
            }
            "-h" | "--help" => {
                eprintln!("usage: vyges-opendb read-def --input <in.odb> --def <f.def> --output <out.odb>");
                return Ok(());
            }
            other => return Err(format!("read-def: unknown argument: {other}").into()),
        }
    }
    let input = input.ok_or("read-def: --input <in.odb> required")?;
    let def = def.ok_or("read-def: --def <f.def> required")?;
    let output = output.ok_or("read-def: --output <out.odb> required")?;
    let mut db = Db::open(&input)?;
    db.read_def(&def, "default")?;
    db.write(&output)?;
    eprintln!("read-def: {input} + {def} -> {output}");
    Ok(())
}

const APPLY_DEF_TEMPLATE_DESCRIBE: &str = r#"{
  "step": "apply-def-template",
  "summary": "Apply a template DEF's floorplan (DIEAREA/TRACKS/ROWS/COMPONENTS/PINS) to a design.",
  "librelane_equivalent": "Odb.ApplyDEFTemplate",
  "unix_only": true,
  "args": [
    { "name": "--input",    "kind": "input",  "type": "path", "required": true, "description": "input .odb design" },
    { "name": "--template", "kind": "input",  "type": "path", "required": true, "description": "template DEF (floorplan)" },
    { "name": "--output",   "kind": "output", "type": "path", "required": true, "description": "output .odb" }
  ]
}"#;

/// `apply-def-template --input <in.odb> --template <f.def> --output <out.odb> | --describe`.
fn apply_def_template(mut args: impl Iterator<Item = String>) -> Result<(), Fail> {
    let (mut input, mut template, mut output) = (None, None, None);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--input" | "-i" => input = args.next(),
            "--template" => template = args.next(),
            "--output" | "-o" => output = args.next(),
            "--describe" => {
                println!("{APPLY_DEF_TEMPLATE_DESCRIBE}");
                return Ok(());
            }
            "-h" | "--help" => {
                eprintln!("usage: vyges-opendb apply-def-template --input <in.odb> --template <f.def> --output <out.odb>");
                return Ok(());
            }
            other => return Err(format!("apply-def-template: unknown argument: {other}").into()),
        }
    }
    let input = input.ok_or("apply-def-template: --input <in.odb> required")?;
    let template = template.ok_or("apply-def-template: --template <f.def> required")?;
    let output = output.ok_or("apply-def-template: --output <out.odb> required")?;
    let mut db = Db::open(&input)?;
    db.read_def(&template, "floorplan")?;
    db.write(&output)?;
    eprintln!("apply-def-template: {input} + {template} -> {output}");
    Ok(())
}

// ---- generic instrumentation surface (get / set / fields) --------------------------------
// Drive the whole machine-generated accessor surface (scripts/generate-bindings.py) by name,
// so `vyges mcp` reaches it through three stable subcommands instead of hundreds.

const FIELDS_DESCRIBE: &str = r#"{
  "step": "fields",
  "summary": "List the generated instrumentation fields (class, field, value/keys) for discovery.",
  "unix_only": true,
  "args": [
    { "name": "--class",    "kind": "filter", "type": "string", "required": false, "description": "restrict to one dbClass" },
    { "name": "--writable", "kind": "flag",   "type": "bool",   "required": false, "description": "list settable fields (needs gen-write)" }
  ],
  "output": "JSON array of { class, field, value|values, keys } on stdout"
}"#;

const GET_DESCRIBE: &str = r#"{
  "step": "get",
  "summary": "Read any generated field by (class, field) with string-encoded addressing keys.",
  "unix_only": true,
  "args": [
    { "name": "--input", "kind": "input",  "type": "path",   "required": true,  "description": "input .odb design" },
    { "name": "--class", "kind": "select", "type": "string", "required": true,  "description": "dbClass, e.g. dbInst" },
    { "name": "--field", "kind": "select", "type": "string", "required": true,  "description": "field name, e.g. get_orient" },
    { "name": "--key",   "kind": "key",    "type": "string", "required": false, "description": "addressing key (repeatable, in order)" }
  ],
  "output": "the field value as JSON on stdout"
}"#;

const SET_DESCRIBE: &str = r#"{
  "step": "set",
  "summary": "Apply a generated setter by (class, field). Requires a --features gen-write build (L2/write).",
  "unix_only": true,
  "level": "L2",
  "args": [
    { "name": "--input",  "kind": "input",  "type": "path",   "required": true,  "description": "input .odb design" },
    { "name": "--output", "kind": "output", "type": "path",   "required": true,  "description": "output .odb" },
    { "name": "--class",  "kind": "select", "type": "string", "required": true,  "description": "dbClass" },
    { "name": "--field",  "kind": "select", "type": "string", "required": true,  "description": "setter field, e.g. set_weight" },
    { "name": "--key",    "kind": "key",    "type": "string", "required": false, "description": "addressing key (repeatable, in order)" },
    { "name": "--value",  "kind": "value",  "type": "string", "required": false, "description": "value to set (repeatable, in order)" }
  ],
  "output": "writes the edited .odb; a one-line confirmation on stderr"
}"#;

/// `fields [--class <dbClass>] [--writable] | --describe` — discovery over the generated surface.
fn fields(mut args: impl Iterator<Item = String>) -> Result<(), Fail> {
    let (mut class, mut writable) = (None, false);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--class" => class = args.next(),
            "--writable" => writable = true,
            "--describe" => {
                println!("{FIELDS_DESCRIBE}");
                return Ok(());
            }
            "-h" | "--help" => {
                eprintln!("usage: vyges-opendb fields [--class <dbClass>] [--writable]");
                return Ok(());
            }
            other => return Err(format!("fields: unknown argument: {other}").into()),
        }
    }
    if writable {
        return list_write_fields(class.as_deref());
    }
    let items: Vec<_> = vyges_opendb::registry::FIELDS
        .iter()
        .filter(|f| class.as_deref().map_or(true, |c| c == f.class))
        .map(|f| serde_json::json!({ "class": f.class, "field": f.field, "value": f.value, "keys": f.keys }))
        .collect();
    println!("{}", serde_json::to_string_pretty(&items)?);
    Ok(())
}

#[cfg(feature = "gen-write")]
fn list_write_fields(class: Option<&str>) -> Result<(), Fail> {
    let items: Vec<_> = vyges_opendb::registry::WRITE_FIELDS
        .iter()
        .filter(|f| class.map_or(true, |c| c == f.class))
        .map(|f| serde_json::json!({ "class": f.class, "field": f.field, "values": f.values, "keys": f.keys }))
        .collect();
    println!("{}", serde_json::to_string_pretty(&items)?);
    Ok(())
}

#[cfg(not(feature = "gen-write"))]
fn list_write_fields(_class: Option<&str>) -> Result<(), Fail> {
    Err("writable fields require a build with --features gen-write (L2/write governance gate)".into())
}

/// `get --input <f.odb> --class <c> --field <f> [--key <k>]... | --describe`.
fn get(mut args: impl Iterator<Item = String>) -> Result<(), Fail> {
    let (mut input, mut class, mut field, mut keys) = (None, None, None, Vec::new());
    while let Some(a) = args.next() {
        match a.as_str() {
            "--input" | "-i" => input = args.next(),
            "--class" => class = args.next(),
            "--field" => field = args.next(),
            "--key" => {
                if let Some(k) = args.next() {
                    keys.push(k);
                }
            }
            "--describe" => {
                println!("{GET_DESCRIBE}");
                return Ok(());
            }
            "-h" | "--help" => {
                eprintln!("usage: vyges-opendb get --input <f.odb> --class <c> --field <f> [--key <k>]...");
                return Ok(());
            }
            other => return Err(format!("get: unknown argument: {other}").into()),
        }
    }
    let input = input.ok_or("get: --input <f.odb> required")?;
    let class = class.ok_or("get: --class <dbClass> required")?;
    let field = field.ok_or("get: --field <name> required")?;
    let db = Db::open(&input)?;
    let value = vyges_opendb::registry::get(&db, &class, &field, &keys)?;
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

/// `set --input <in.odb> --output <out.odb> --class <c> --field <f> [--key <k>]... [--value <v>]...`
/// Gated behind `gen-write` (L2/write governance).
#[cfg(feature = "gen-write")]
fn set(mut args: impl Iterator<Item = String>) -> Result<(), Fail> {
    let (mut input, mut output, mut class, mut field) = (None, None, None, None);
    let (mut keys, mut values): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
    while let Some(a) = args.next() {
        match a.as_str() {
            "--input" | "-i" => input = args.next(),
            "--output" | "-o" => output = args.next(),
            "--class" => class = args.next(),
            "--field" => field = args.next(),
            "--key" => {
                if let Some(k) = args.next() {
                    keys.push(k);
                }
            }
            "--value" => {
                if let Some(v) = args.next() {
                    values.push(v);
                }
            }
            "--describe" => {
                println!("{SET_DESCRIBE}");
                return Ok(());
            }
            "-h" | "--help" => {
                eprintln!("usage: vyges-opendb set --input <in> --output <out> --class <c> --field <f> [--key <k>]... [--value <v>]...");
                return Ok(());
            }
            other => return Err(format!("set: unknown argument: {other}").into()),
        }
    }
    let input = input.ok_or("set: --input <in.odb> required")?;
    let output = output.ok_or("set: --output <out.odb> required")?;
    let class = class.ok_or("set: --class <dbClass> required")?;
    let field = field.ok_or("set: --field <name> required")?;
    let mut db = Db::open(&input)?;
    vyges_opendb::registry::set(&mut db, &class, &field, &keys, &values)?;
    db.write(&output)?;
    eprintln!("set: {class}.{field} <- {values:?} -> {output}");
    Ok(())
}

#[cfg(not(feature = "gen-write"))]
fn set(mut args: impl Iterator<Item = String>) -> Result<(), Fail> {
    if args.any(|a| a == "--describe") {
        println!("{SET_DESCRIBE}");
        return Ok(());
    }
    Err("`set` requires a build with --features gen-write (L2/write governance gate)".into())
}
