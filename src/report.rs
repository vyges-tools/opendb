// SPDX-License-Identifier: Apache-2.0
//! Read-only audit/report steps over the design database — Loom-native equivalents of LibreLane's
//! read-only `Odb.*` reporting steps. These never mutate the database; output is structured (JSON).

use serde::Serialize;
use std::collections::HashMap;

use crate::Db;

/// One row of a cell-frequency table.
#[derive(Debug, Clone, Serialize)]
pub struct CellFreq {
    pub master: String,
    pub count: usize,
}

/// `CellFrequencyTables`: count instances per master cell, most-used first (ties by name).
/// Mirrors LibreLane's `Odb.CellFrequencyTables`.
pub fn cell_frequency_table(db: &Db) -> Vec<CellFreq> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for inst in db.inst_names() {
        let m = db.inst_master(&inst);
        if !m.is_empty() {
            *counts.entry(m).or_default() += 1;
        }
    }
    let mut rows: Vec<CellFreq> =
        counts.into_iter().map(|(master, count)| CellFreq { master, count }).collect();
    rows.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.master.cmp(&b.master)));
    rows
}

/// `ReportDisconnectedPins`: every instance pin (`inst/pin`) and port (`port:name`) with no net.
/// Mirrors LibreLane's `Odb.ReportDisconnectedPins`.
pub fn disconnected_pins(db: &Db) -> Vec<String> {
    let mut out = Vec::new();
    for inst in db.inst_names() {
        for pin in db.iterm_names(&inst) {
            if db.net_of(&inst, &pin).is_empty() {
                out.push(format!("{inst}/{pin}"));
            }
        }
    }
    for port in db.bterm_names() {
        if db.bterm_net(&port).is_empty() {
            out.push(format!("port:{port}"));
        }
    }
    out
}

/// One net's connectivity: its type, special flag, and the pins it touches.
#[derive(Debug, Clone, Serialize)]
pub struct NetConn {
    pub net: String,
    pub sig_type: String,
    pub special: bool,
    /// Instance pins (`inst/pin`) on the net.
    pub iterms: Vec<String>,
    /// Block ports on the net.
    pub bterms: Vec<String>,
    /// Total pin count (fanout+1): `iterms + bterms`.
    pub degree: usize,
}

/// Connectivity graph: one `NetConn` per net (its sig-type, special flag, and the pins it touches),
/// highest-degree net first. This is the core instrumentation primitive — a netlist connectivity
/// dump that higher layers turn into fanout histograms, high-fanout-net reports, clock/power-net
/// audits, etc. Read-only; no LibreLane counterpart (it's an odb-native traversal).
pub fn net_connectivity(db: &Db) -> Vec<NetConn> {
    let mut rows: Vec<NetConn> = db
        .net_names()
        .into_iter()
        .map(|net| {
            let iterms = db.net_iterms(&net);
            let bterms = db.net_bterms(&net);
            let degree = iterms.len() + bterms.len();
            NetConn {
                sig_type: db.net_sigtype(&net),
                special: db.net_is_special(&net),
                iterms,
                bterms,
                degree,
                net,
            }
        })
        .collect();
    rows.sort_by(|a, b| b.degree.cmp(&a.degree).then_with(|| a.net.cmp(&b.net)));
    rows
}

/// `WriteVerilogHeader`: a Verilog module header (`module <name>(...); input/output ...`) built
/// from the block's ports + directions. Mirrors LibreLane's `Odb.WriteVerilogHeader` (header only —
/// no cell instantiations). Returns the Verilog text.
pub fn verilog_header(db: &Db) -> String {
    let ports = db.bterm_names();
    let mut v = format!("module {} (\n", db.block_name());
    for (i, p) in ports.iter().enumerate() {
        let comma = if i + 1 < ports.len() { "," } else { "" };
        v.push_str(&format!("  {p}{comma}\n"));
    }
    v.push_str(");\n");
    for p in &ports {
        let dir = match db.bterm_direction(p).as_str() {
            "INPUT" => "input",
            "OUTPUT" => "output",
            _ => "inout",
        };
        v.push_str(&format!("  {dir} {p};\n"));
    }
    v.push_str("endmodule\n");
    v
}

/// One cell's antenna-property gaps — `Odb.Check{Macro,Design}AntennaProperties`.
#[derive(serde::Serialize, Debug, Default, PartialEq)]
pub struct AntennaProperties {
    pub cell: String,
    pub inout: Vec<String>,
    pub input: Vec<String>,
    pub output: Vec<String>,
}

/// Which pins of `cells` have no antenna information in the LEF.
///
/// Transcribed from LibreLane `scripts/odbpy/check_antenna_properties.py::check_cells`. The rule,
/// per pin, skipping `GROUND`/`POWER`/`ANALOG`:
///
/// | direction | flagged when | what it suggests |
/// | --- | --- | --- |
/// | `INOUT` | no diffusion **and** no gate area | the pin may be disconnected |
/// | `INPUT` | no gate area | may not be connected to a gate |
/// | `OUTPUT` | no diffusion area | may not be driven |
///
/// 🔑 **It reads only PRESENCE, never the values** — `len(diff_area)`, `len(gate_area)` — which is
/// why the bridge exposes predicates rather than the coordinate lists.
///
/// ⛔ **DELIBERATE DIVERGENCE: the reference checks only the FIRST cell.** Its `return report` sits
/// INSIDE the `for cell in odb_cells` loop (`check_antenna_properties.py:68`, indented to the loop
/// body at line 25), so `check_cells` returns after one iteration and every later cell is silently
/// unexamined. Both `Odb.CheckMacroAntennaProperties` and `Odb.CheckDesignAntennaProperties` call
/// it, so a design handing it several macros has one checked.
///
/// ⚠️ **We check ALL of them, on purpose**, and this is the one place the programme's
/// transcribe-the-reference rule is knowingly set aside. The reasons are narrow and do not
/// generalise: there is no golden to match here (the step emits a YAML report, not geometry), and
/// a checker that stops after one cell is the `vacuous` class — a pass word from a run that did
/// not do the job. Reproducing it would ship a checker that does not check.
///
/// ✅ **MEASURED against the reference 2026-09-02**, not merely read. Given `-c PADCELL_SIG_H
/// -c PADCELL_SIG_V -c PADCELL_VDDIO_H`, LibreLane 2.4.6 emits **one** cell; we emit three. On the
/// cell both emit, every list is identical:
///
/// ```text
///   PADCELL_SIG_V   inout  [PAD]            == ref
///                   input  [A, RETN, SNS]   == ref
///                   output [OE, PU, Y]      == ref
/// ```
///
/// ⟹ The RULE is transcribed exactly; only the loop bound differs, and it differs on purpose.
pub fn antenna_properties(db: &crate::Db, cells: &[String]) -> Vec<AntennaProperties> {
    cells
        .iter()
        .map(|cell| {
            let mut e = AntennaProperties { cell: cell.clone(), ..Default::default() };
            for term in db.master_get_m_terms(cell) {
                // The reference skips supply and analog pins before anything else.
                if matches!(db.mterm_get_sig_type(cell, &term).as_str(),
                            "GROUND" | "POWER" | "ANALOG") {
                    continue;
                }
                let diff = db.mterm_has_diff_area(cell, &term);
                let gate = db.mterm_has_gate_area(cell, &term);
                match db.mterm_get_io_type(cell, &term).as_str() {
                    "INOUT" if !(diff || gate) => e.inout.push(term),
                    "INPUT" if !gate => e.input.push(term),
                    "OUTPUT" if !diff => e.output.push(term),
                    _ => {}
                }
            }
            e
        })
        .collect()
}
