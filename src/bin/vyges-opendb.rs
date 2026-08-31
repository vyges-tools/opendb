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
vyges opendb — OpenROAD's OpenDB (libodb) design database

usage:
  vyges opendb <command> [options]

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
  view-3dblox               --input <f.3dbx|f.odb> --output <out.svg|out.png> [--heatmap]
                            [--top <chip>] [--scale <n>]
                      Draw the assembly: cross-section + plan, with any check-3dblox
                      findings listed on it. Format follows the output extension.
                      --heatmap shades MEASURED die-to-die misalignment onto the
                      plan view (from check-d2d); it is not a yield prediction.
  check-d2d                 --input <stack.3dbx> | --top <a.bmap> --bottom <b.bmap>
                            [--offset-x <um>] [--offset-y <um>] [--flip-x]
                            [--tolerance <um>]
                      Check a die-to-die interface: unmated bumps, misalignment, net
                      and bump-cell mismatch across the bond. Emits JSON.
  check-3d-nets             --input <stack.3dbx> [--tolerance <um>] [--no-tsv-inference]
                      Check net continuity across the whole stack: a net a die cannot
                      carry from one face to the other, and nets the bonding shorts
                      together. Emits JSON.
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

  import                    --lef <tech.lef> [--lef <lib.lef>]... [--def <f.def>]
                            [--verilog <f.v>] --output <out.odb>
                      Build a database from LEF + DEF or a structural Verilog netlist,
                      starting from nothing. The FIRST --lef creates the tech.

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
        "check-3d-nets" => check_3d_nets(args),
        "check-3dblox" => check_3dblox(args),
        "apply-eco-plan" => apply_eco_plan(args),
        "report-connectivity" => report_connectivity(args),
        "custom-io-placement" => custom_io_placement(args),
        "write-def" => write_def(args),
        "read-def" => read_def(args),
        "import" => import(args),
        "apply-def-template" => apply_def_template(args),
        "fields" => fields(args),
        "get" => get(args),
        "set" => set(args),
        "-V" | "--version" => {
            println!("vyges-opendb {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        "--describe" => {
            println!("{TOOL_DESCRIBE}");
            Ok(())
        }
        "" | "-h" | "--help" => {
            print!("{USAGE}");
            Ok(())
        }
        other => Err(format!("unknown command '{other}'. Try 'vyges-opendb --help'.").into()),
    }
}

/// Tool-level contract (`vyges-opendb --describe`). Per-step contracts stay on the steps
/// (`vyges-opendb <step> --describe`); this one declares what the *binary* is and which schema it
/// speaks, so a caller can interrogate it without already knowing a step name — the uniform
/// interrogation point every other Vyges engine already offers.
///
/// Deliberately carries no `invocation`: this binary is not callable as a single operation, and a
/// descriptor without one is ignored by MCP's engine parser rather than exposed as a broken tool.
///
/// The whole crate moves in unison — the step surface is a function of the pinned upstream
/// OpenROAD odb source (`vyges-opendb-lib`, `openroad-pin.yaml`), not an independently versioned
/// list — so `steps` ships and is reviewed with the dispatch table it describes.
const TOOL_DESCRIBE: &str = r#"{
  "schema": "vyges-tool-descriptor/1.1",
  "kind": "multi-step",
  "name": "opendb",
  "summary": "OpenDB (.odb) database surgery, inspection, and D2D/3D checks",
  "describe_per_step": true,
  "steps": [
    "info",
    "insert-eco-buffers",
    "insert-eco-diodes",
    "manual-global-placement",
    "manual-macro-placement",
    "diodes-on-ports",
    "cell-frequency-tables",
    "report-disconnected-pins",
    "set-power-connections",
    "add-obstructions",
    "remove-obstructions",
    "write-verilog-header",
    "report-wire-length",
    "read-3dblox",
    "view-3dblox",
    "check-d2d",
    "check-3d-nets",
    "check-3dblox",
    "apply-eco-plan",
    "report-connectivity",
    "custom-io-placement",
    "write-def",
    "read-def",
    "import",
    "apply-def-template",
    "fields",
    "get",
    "set"
  ]
}"#;

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
        let nameable = !UNNAMEABLE_MARKER_CATEGORIES.contains(&check);
        for i in 0..count {
            let field = if nameable { "get_name" } else { "get_comment" };
            let name = vyges_opendb::registry::get(db, "dbMarker", field,
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
    let mut heatmap = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--input" | "-i" => input = args.next(),
            "--output" | "-o" => output = args.next(),
            "--top" => top = args.next(),
            "--heatmap" => heatmap = true,
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
                     --output <out.svg|out.png> [--top <chip>] [--scale <n>] [--heatmap]"
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

    // When the input is an assembly file we can also check its die-to-die interfaces, and we
    // must: `check_3dblox` does not look at whether bumps mate, so a drawing carrying only its
    // verdict captions a broken interface "no violations". Measured on a released binary —
    // check-d2d reported 5 violations on an assembly whose drawing said it was clean.
    let mut findings = findings;
    // (x, y, separation) in microns, assembly frame — scaled to DBU once `dbu` is known.
    let mut overlay_um: Vec<(f64, f64, f64)> = Vec::new();
    if is_dbx {
        for p in vyges_opendb::blox::bonded_pairs(&input)? {
            let load = |s: &vyges_opendb::blox::BondedSide| {
                let (Some(b), Some((w, h))) = (&s.bmap, s.design_area) else { return None };
                Some((
                    vyges_opendb::d2d::BumpMap::load(b).ok()?,
                    vyges_opendb::d2d::Placement {
                        orient: s.orient.clone(),
                        loc_x: s.loc.0,
                        loc_y: s.loc.1,
                        die_w: w,
                        die_h: h,
                    },
                ))
            };
            let (Some((tm, tp)), Some((bm, bp))) = (load(&p.top), load(&p.bottom)) else {
                continue;
            };
            if let Ok(r) = vyges_opendb::d2d::check_placed(&tm, &tp, &bm, &bp, None) {
                if heatmap {
                    overlay_um.extend(r.samples.iter().map(|s| (s.x, s.y, s.distance_um)));
                }
                for f in &r.findings {
                    findings.push((format!("d2d/{}", f.kind()), f.message()));
                }
            }
        }
    }

    let mut asm = Assembly3d::read(&db, &top)?.with_findings(findings);
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
    if heatmap {
        if overlay_um.is_empty() {
            eprintln!(
                "view-3dblox: --heatmap: no mated bumps to map. A heat map needs bump maps on \
                 both mating faces (.3dbv `bmap:`) and a .3dbx input; drawing without it."
            );
        }
        asm.overlay = vyges_opendb::view3d::Overlay {
            points: overlay_um
                .iter()
                .map(|(x, y, v)| vyges_opendb::view3d::OverlayPoint {
                    x: x * dbu,
                    y: y * dbu,
                    value: *v,
                })
                .collect(),
            label: "misalignment".into(),
            unit: "um".into(),
        };
    }
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

/// Categories whose markers carry a `dbChipBumpInst` source.
///
/// `dbMarker::getName()` switches on its sources' object types and calls `logger->error()` on
/// anything it does not handle — and `dbChipBumpInst` is not handled. `utl::Logger::error` throws,
/// our generated getter is bound infallible, and the process **aborts**. Measured, not feared: a
/// single bump outside its region killed `check-3dblox` outright.
///
/// So for these two, read the comment and leave the name alone. The comment is a stored string
/// and carries the useful text anyway ("Bump is outside its parent region ..."). Both checks add
/// `marker->addSource(bump->getChipBumpInst())`, which is why it is these two and not one.
const UNNAMEABLE_MARKER_CATEGORIES: [&str; 2] = ["Bump Alignment", "Logical Connectivity"];

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
  "summary": "draw a chiplet assembly as SVG or PNG: cross-section, plan, linter findings, and an optional die-to-die misalignment heat map",
  "maturity": "experimental",
  "provenance_limitations": [
      "The Z axis is exaggerated so the stack is legible; the factor is printed on the drawing and dimensions must not be measured off it.",
      "Geometry only: no routing, no bumps drawn individually, no per-die layer stack.",
      "--heatmap shows MEASURED die-to-die misalignment, not predicted yield. Yield needs process inputs (particle density, Cu recess, surface roughness) that no layout carries; this is the layout-side input such a model consumes.",
      "--heatmap needs a .3dbx input with bump maps on both mating faces; without them the drawing is produced without a map and a note is written to stderr.",
      "Heat-map samples are drawn at a legible minimum size, so a dense bump field merges into regions rather than resolving individual bumps.",
      "A .odb input needs --top because the database has no top-chip getter."
  ],
  "invocation": {
    "args_template": ["view-3dblox", "--input", "{input}", "--output", "{output}"],
    "optional": [ { "arg": "top", "flag": "--top" }, { "arg": "scale", "flag": "--scale" }, { "arg": "heatmap", "flag": "--heatmap", "type": "boolean" } ],
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
        return fail_on(violations);
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
    // Non-zero on violations, matching every other sign-off engine in the suite. A checker that
    // always exits 0 cannot gate anything: a CI job would go green over a dead interface, which
    // is the exact failure this command exists to prevent.
    fail_on(report.violations())
}

/// Exit code for a check: 0 clean, 1 when it found something.
fn fail_on(violations: usize) -> Result<(), Fail> {
    if violations > 0 {
        std::process::exit(1);
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
  },
  "output": "JSON on stdout. Two shapes: with --input, { interfaces: [...], interfaces_checked, interfaces_skipped, violations }; with --top/--bottom, one interface object directly. An interface carries { violations, by_kind, top_bumps, bottom_bumps, matched, tolerance_um, tolerance_source, frame, transform, findings, parse_errors }. Every finding is DATA, not only prose: { kind, message, x_um, y_um } always, plus distance_um and signed dx_um/dy_um for 'misaligned', and top/bottom bump objects { inst, cell, x_um, y_um, port, net } for every paired kind. Exits non-zero when violations > 0.",
  "consumers": [
      "vyges-opendb view-3dblox --heatmap shades distance_um onto the plan view.",
      "Signed dx_um/dy_um separate a systematic error (a field displaced one way — placement or thermal expansion) from random overlay scatter; the magnitude alone cannot.",
      "The same fields are the layout-side input a bonding-yield model (e.g. UCLA's YAP, integrated into OpenROAD over a file interface) consumes. This tool measures geometry; it does NOT predict yield, which needs process inputs (particle density, Cu recess, surface roughness) no layout carries."
  ]
}
"#;

const CHECK_3D_NETS_DESCRIBE: &str = r#"{
  "name": "check-3d-nets",
  "summary": "check net continuity across a whole chiplet stack: a net a die cannot carry from one face to the other, and nets the bonding shorts together",
  "maturity": "experimental",
  "provenance_limitations": [
      "Net names come from the .bmap files the assembly points at, not from a netlist or a loaded database — the report always states net_source.",
      "A netName belongs to its own die's netlist, so net identity comes from the graph (same name within one die, plus whatever the bonding mates) and never from name equality across unbonded dies. Anything needing an assembly netlist is declined rather than guessed.",
      "A through-path inside a TSV die is inferred from net names matching across the die's two faces. 3Dblox and odb's 3D chip schema carry only a per-die tsv boolean, no TSV positions; odb can hold TSV shapes on a dbTechLayer of LEF58 type TSV/TSVMETAL, but that is the LEF_file/DEF_file leg this reader does not read. --no-tsv-inference turns the inference off.",
      "A bond whose surfaces declare no bmap, a virtual bond, and a nested instance path are listed under interfaces_skipped, not counted as clean.",
      "Read-only: it never modifies the assembly or any database."
  ],
  "invocation": {
    "args_template": ["check-3d-nets", "--input", "{input}"],
    "optional": [
      { "arg": "tolerance", "flag": "--tolerance" },
      { "arg": "no_tsv_inference", "flag": "--no-tsv-inference", "kind": "flag" }
    ],
    "emits_json": true
  },
  "inputs": {
    "type": "object",
    "required": ["input"],
    "properties": {
      "input":     { "type": "string", "description": "3Dblox assembly (.3dbx)" },
      "tolerance": { "type": "number", "description": "bump match radius in microns; default is half the bump pitch, per bond" },
      "no_tsv_inference": { "type": "boolean", "description": "do not join a TSV die's two faces by matching net name" }
    }
  },
  "output": "JSON { violations, by_kind, nets, bumps, groups, unnetted_bumps, net_source, tsv_inference, interfaces_checked, bonds, interfaces_skipped, regions_skipped, findings, parse_errors } on stdout. Finding kinds: severed and net-merged are violations; unresolved and tsv-unused are informational. Exits non-zero when violations > 0.",
  "consumers": [
      "Complements check-d2d rather than repeating it: check-d2d asks whether one interface is wired right, this asks whether a net is right across the stack."
  ]
}
"#;

/// `check-3d-nets --input <stack.3dbx>`.
///
/// The stack-level counterpart to `check-d2d`. Deliberately `.3dbx`-only: the database has a
/// `dbChipNet` class, but every traversal edge it would need is unbridged at our pin and
/// `dbChipBump::setNet` is too, so a database we built carries no chip nets — a database-driven
/// check would report every stack clean. Refusing a `.odb` and saying why beats that.
fn check_3d_nets(mut args: impl Iterator<Item = String>) -> Result<(), Fail> {
    use vyges_opendb::nets3d::{check_assembly, Options};
    let mut input = None;
    let mut opts = Options::default();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--input" | "-i" => input = args.next(),
            "--tolerance" => {
                let v = args.next().ok_or("check-3d-nets: --tolerance needs a number")?;
                opts.tolerance_um = Some(
                    v.parse::<f64>()
                        .map_err(|_| format!("check-3d-nets: --tolerance: not a number: {v}"))?,
                );
            }
            "--no-tsv-inference" => opts.tsv_inference = false,
            "--describe" => {
                println!("{CHECK_3D_NETS_DESCRIBE}");
                return Ok(());
            }
            "-h" | "--help" => {
                eprintln!(
                    "usage: vyges-opendb check-3d-nets --input <stack.3dbx> \
                     [--tolerance <um>] [--no-tsv-inference]"
                );
                return Ok(());
            }
            other => return Err(format!("check-3d-nets: unknown argument: {other}").into()),
        }
    }
    let input = input.ok_or("check-3d-nets: --input <stack.3dbx> required")?;
    if input.ends_with(".odb") {
        return Err("check-3d-nets: reads a 3Dblox assembly (.3dbx), not a database. Net \
                    continuity needs the per-bump net names, which live in the .bmap files the \
                    assembly points at; the database's own chip nets are not reachable at this \
                    pin (dbUnfoldedChipNet::getConnectedBumps and dbChipRegion::getChipBumps are \
                    not bound, and dbChipBump::setNet is not either, so a database built here \
                    carries none)."
            .into());
    }
    let report = check_assembly(&input, &opts)?;
    println!("{}", serde_json::to_string_pretty(&report.to_json())?);
    // Skipped work goes to stderr as well as into the report: a clean stdout that nobody reads
    // past is exactly how "not looked at" gets mistaken for "looked and found nothing".
    for s in &report.interfaces_skipped {
        eprintln!("check-3d-nets: not checked — {s}");
    }
    for s in &report.regions_skipped {
        eprintln!("check-3d-nets: not placed — {s}");
    }
    if !report.parse_errors.is_empty() {
        eprintln!(
            "check-3d-nets: {} unparseable bump-map line(s); those bumps were not checked",
            report.parse_errors.len()
        );
    }
    fail_on(report.violations())
}

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
        // See UNNAMEABLE_MARKER_CATEGORIES: asking these for a name aborts the process.
        let nameable = !UNNAMEABLE_MARKER_CATEGORIES.contains(&check);
        let markers: Vec<serde_json::Value> = (0..count)
            .map(|i| {
                let keys = [path.clone(), i.to_string()];
                serde_json::json!({
                    "name": if nameable { get("dbMarker", "get_name", &keys) } else { None },
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
    // Non-zero on violations, so this gates CI like the rest of the suite.
    fail_on(violations as usize)
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

const IMPORT_DESCRIBE: &str = r#"{
  "step": "import",
  "summary": "Build a design database from LEF plus a DEF or a structural Verilog netlist, with no OpenROAD in the loop.",
  "inputs": ["lef", "def", "verilog"],
  "outputs": ["odb"]
}"#;

/// `import`: LEF(s) + DEF -> `.odb`, starting from an EMPTY database.
///
/// ⛔ **This closes the last thing only OpenROAD could do.** `read-def` needs a tech and libraries
/// to read against, and until `read_lef` existed nothing on our side could create them — so every
/// chain had to begin with an OpenROAD-built handoff.
///
/// 🔑 **LEF ORDER MATTERS and is the caller's**: the first `--lef` creates the tech, every later one
/// adds a library to it, exactly as `read_lef` does. Give the technology LEF first.
fn import(mut args: impl Iterator<Item = String>) -> Result<(), Fail> {
    let (mut lefs, mut def, mut output) = (Vec::new(), None, None);
    let mut verilog: Option<String> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--lef" => lefs.push(args.next().ok_or("import: --lef needs a FILE")?),
            "--def" => def = args.next(),
            "--verilog" => verilog = args.next(),
            "--output" | "-o" => output = args.next(),
            "--describe" => {
                println!("{IMPORT_DESCRIBE}");
                return Ok(());
            }
            "-h" | "--help" => {
                eprintln!("usage: vyges-opendb import --lef <tech.lef> [--lef <more.lef> ...] \
                           [--def <f.def>] [--verilog <f.v>] --output <out.odb>\n\
                           \n  The FIRST --lef creates the tech; give the technology LEF first.\n\
                           \n  --def and --verilog are alternative ways to bring in the design.");
                return Ok(());
            }
            other => return Err(format!("import: unknown argument: {other}").into()),
        }
    }
    if lefs.is_empty() {
        return Err("import: at least one --lef is required (the first creates the tech)".into());
    }
    let output = output.ok_or("import: --output <out.odb> required")?;

    let mut db = Db::new();
    for lef in &lefs {
        db.read_lef(lef)?;
    }
    if let Some(def) = &def {
        db.read_def(def, "default")?;
    }
    if let Some(v) = &verilog {
        let n = build_from_netlist(&mut db, v)?;
        eprintln!("import: {n} instances built from {v}");
    }
    db.write(&output)?;
    eprintln!(
        "import: {} LEF(s){}{} -> {output}",
        lefs.len(),
        def.as_ref().map(|d| format!(" + {d}")).unwrap_or_default(),
        verilog.as_ref().map(|v| format!(" + {v}")).unwrap_or_default()
    );
    Ok(())
}

/// Build a flat design from a structural Verilog netlist — `Verilog2db`, transcribed.
///
/// 🔑 **The call sequence is upstream's, and it is behaviour rather than detail**
/// (`dbSta/src/dbReadVerilog.cc`):
///
/// 1. `makeBlock` (:238) — a chip if none, `dbBlock::create`, then **`setDefUnits`** from the
///    LEF's units and **`setBusDelimiters('[' , ']')`**. ⚠️ `setDefUnits` is NOT the database
///    scale: `dbu_per_micron` comes from the tech at block creation (`dbBlock.cpp:2953`), and
///    `def_units_` is the DEF OUTPUT units, default 100 — so omitting it writes a DEF saying 100
///    where sky130 should say 1000.
/// 2. `makeChildInsts` (:552) — `dbInst::create(block, master, name)` per instance, the master
///    resolved in the LEF libs. ⛔ A missing master is an ERROR upstream (ORD-2013), not a skip.
/// 3. `makeDbNets` (:700) — nets, then the pins on each.
///
/// ⛔ **Two orderings inside step 3 that only reading the source gets right:**
///   * a **block terminal is created from the NET**, not from the port declaration list, and only
///     when the block has none of that name — then `setIoType`;
///   * a net's pins are **SORTED** before they are connected, which upstream comments as being
///     "for regression stability". Insertion order would otherwise follow parse order.
///
/// ⚠️ **`assign` aliases are two names for ONE net.** OpenSTA resolves them before any `dbNet`
/// exists, so upstream never sees two. Dropping them "breaks connectivity rather than merely
/// thinning it" — loom's own reader says so — because the aliased port ends up with no driver.
fn build_from_netlist(db: &mut Db, path: &str) -> Result<usize, Fail> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("import: {path}: {e}"))?;
    let nl = vyges_loom::netlist::parse(&text).map_err(|e| format!("import: {path}: {e:?}"))?;
    // ⚠️ This reads STRUCTURAL netlists. Handed RTL the reader recovers fragments rather than
    // failing — measured at 2 instances from a module with 4,558 connections — so the count it
    // keeps for exactly this purpose is checked instead of ignored.
    if nl.behavioural > 0 {
        return Err(format!(
            "import: {path} has {} behavioural construct(s); this builds from a STRUCTURAL netlist",
            nl.behavioural
        )
        .into());
    }

    // ⛔ **An ESCAPED identifier keeps its escaping in the DATABASE, though not in loom's name.**
    // The netlist writes `wire \claimed[0] ;` — the brackets are literal, not a bus index — and
    // OpenROAD stores that net as `claimed\[0\]`. loom deliberately canonicalises to `claimed[0]`
    // so the name matches the SPEF and DEF spellings a timer looks up; keeping the backslash there
    // once cost 767 of 14,238 nets and 4,527 coupling references, dropped silently.
    //
    // ⟹ The escaping is re-applied HERE, on the way into the database, from the set loom records.
    // Without it an escaped identifier and a real bus bit of the same spelling become one net.
    let esc = |n: &str| -> String {
        if !nl.escaped_names.contains(n) {
            return n.to_string();
        }
        let mut out = String::with_capacity(n.len() + 4);
        for c in n.chars() {
            // ⛔ **Brackets ONLY — measured.** A dot is NOT escaped: OpenROAD stores
            // `u_adapter.req_addr_q\[0\]`, with the hierarchy separator plain. Escaping it too
            // differed on 1,536 DEF lines for `fft_ctrl_tlul`. The brackets are escaped because
            // they collide with the bus delimiters; the dot collides with nothing.
            if matches!(c, '[' | ']') {
                out.push('\\');
            }
            out.push(c);
        }
        out
    };

    // ---- 1. the block ----------------------------------------------------------------------
    db.create_chip(&nl.module, "", "DIE")?;
    db.create_chip_block(&nl.module, &nl.module)?;
    let units = db.tech_get_lef_units();
    if units > 0 {
        db.block_set_def_units(units)?;
    }
    db.set_bus_delimiters('[', ']')?;

    // ---- `assign a = b` — union-find so a chain (a=b, b=c) collapses to one net -------------
    let mut parent: std::collections::HashMap<String, String> = Default::default();
    fn find(p: &std::collections::HashMap<String, String>, x: &str) -> String {
        let mut cur = x.to_string();
        while let Some(nxt) = p.get(&cur) {
            if *nxt == cur {
                break;
            }
            cur = nxt.clone();
        }
        cur
    }
    let ports: std::collections::HashSet<String> =
        nl.inputs.iter().chain(&nl.outputs).chain(&nl.inouts).cloned().collect();
    for (l, r) in &nl.aliases {
        let (a, b) = (find(&parent, l), find(&parent, r));
        if a != b {
            // ⛔ **The INTERNAL name survives, not the port's — measured, and the opposite of the
            // obvious guess.** For `assign tl_o[0] = net492;` upstream's DEF reads
            // `- tl_o[0] + NET net492`: the port becomes a TERMINAL on the internal net rather than
            // renaming it. That follows from `makeDbNets` creating the bterm from the net it is
            // walking. Keeping the port name instead differed on 452 DEF lines here.
            let (keep, drop) = if ports.contains(&a) {
                (b, a)
            } else if ports.contains(&b) {
                (a, b)
            } else {
                // Neither is a port: either survives, but the choice must be DETERMINISTIC or the
                // same netlist builds two different databases on two runs.
                let (x, y) = if a < b { (a, b) } else { (b, a) };
                (x, y)
            };
            parent.insert(drop, keep);
        }
    }

    // ---- 2. instances, before nets, as upstream does ----------------------------------------
    for inst in &nl.insts {
        db.create_inst(&inst.cell, &esc(&inst.name)).map_err(|e| {
            format!("import: instance {} master {} not found: {e}", inst.name, inst.cell)
        })?;
    }

    // ---- 3. nets, their pins, and the terminals ---------------------------------------------
    let mut pins: std::collections::BTreeMap<String, Vec<(String, String)>> = Default::default();
    for inst in &nl.insts {
        for (pin, net) in &inst.conns {
            if pin.is_empty() {
                // Positional: which pin a position means lives in the LEF, not the netlist.
                return Err(format!(
                    "import: {} has a positional connection; this path needs named pins",
                    inst.name
                )
                .into());
            }
            pins.entry(esc(&find(&parent, net)))
                .or_default()
                .push((esc(&inst.name), pin.clone()));
        }
    }

    // 🔑 **A terminal keeps the PORT's name, on the NET's object** — `dbBTerm::create(db_net,
    // port_name)` (`dbReadVerilog.cc:756`). The two are different whenever an `assign` aliased the
    // port to an internal net: the DEF then reads `- tl_o[0] + NET net492`. Naming the terminal
    // after the net instead produced 443 wrong DEF lines here.
    // port name -> (the net it sits on, its direction)
    let mut io: std::collections::BTreeMap<String, (String, &str)> = Default::default();
    for (list, dir) in [(&nl.inputs, "INPUT"), (&nl.outputs, "OUTPUT"), (&nl.inouts, "INOUT")] {
        for p in list {
            io.insert(esc(p), (esc(&find(&parent, p)), dir));
        }
    }

    let every: std::collections::BTreeSet<String> = pins
        .keys()
        .cloned()
        .chain(io.values().map(|(n, _)| n.clone()))
        .collect();
    for net in &every {
        db.create_net(net)?;
        if let Some(list) = pins.get(net) {
            // ⛔ Sorted, as `makeDbNets` sorts its pins "for regression stability".
            let mut list = list.clone();
            list.sort();
            for (inst, pin) in list {
                db.connect(&inst, &pin, net)?;
            }
        }
    }

    // ⚠️ Terminals AFTER every net exists, since each is created on the net it belongs to.
    for (port, (net, dir)) in &io {
        db.create_bterm(net, port)?;
        db.bterm_set_io_type(port, dir)?;
    }
    Ok(nl.insts.len())
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

#[cfg(test)]
mod surface_tests {
    //! ⛔ **A subcommand that dispatches but is not in `--help` does not exist to the user, and
    //! does not exist to the docs** — `vyges-cli`'s mdbook pages are generated FROM `--help`.
    //! `import` shipped in v0.1.34 dispatching correctly and documented nowhere; it was found by
    //! a person reading the help, which is not a gate. This is the gate.

    use super::{TOOL_DESCRIBE, USAGE};

    /// Every `"name" => fn(args)` arm of `run()`'s dispatch, minus the flag arms.
    fn dispatched() -> Vec<String> {
        let src = include_str!("vyges-opendb.rs");
        let body = src
            .split_once("fn run() -> Result<(), Fail> {")
            .expect("run() moved")
            .1;
        body.lines()
            .take_while(|l| !l.contains("\"-V\""))
            .filter_map(|l| {
                let l = l.trim();
                let name = l.strip_prefix('"')?.split_once("\" => ")?.0;
                // ⚠️ Only real command names: skip anything that is not a plain word.
                name.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
                    .then(|| name.to_string())
            })
            .collect()
    }

    #[test]
    fn every_dispatched_subcommand_is_in_the_help_text() {
        let missing: Vec<_> = dispatched()
            .into_iter()
            .filter(|c| !USAGE.contains(&format!("\n  {c} ")) && !USAGE.contains(&format!("\n  {c}\n")))
            .collect();
        assert!(missing.is_empty(), "not documented in --help: {missing:?}");
    }

    #[test]
    fn every_dispatched_subcommand_is_in_the_describe_list() {
        let missing: Vec<_> = dispatched()
            .into_iter()
            .filter(|c| !TOOL_DESCRIBE.contains(&format!("\"{c}\"")))
            .collect();
        assert!(missing.is_empty(), "not listed by --describe: {missing:?}");
    }

    #[test]
    fn the_gate_fires_on_an_undocumented_name() {
        // 🔑 A check that cannot fail proves nothing: confirm it rejects a name nobody documented.
        assert!(!USAGE.contains("\n  frobnicate "));
        assert!(!TOOL_DESCRIBE.contains("\"frobnicate\""));
    }
}
