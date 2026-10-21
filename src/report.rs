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

/// Every instance pin (`inst/pin`) and port (`port:name`) with no net — a raw list. LibreLane's
/// `Odb.ReportDisconnectedPins` counts by its own rules instead: see [`disconnected_pin_report`].
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

/// A terminal as `disconnected_pins.py`'s `Port`: its direction and signal type as odb names them
/// (`INPUT`/`OUTPUT`/`INOUT`/…, `SIGNAL`/`POWER`/`GROUND`/…) and whether it is connected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Terminal {
    pub name: String,
    pub io_type: String,
    pub sig_type: String,
    pub connected: bool,
}

/// One module's row (`write_disconnected_pins`): reported only when it has a disconnected pin.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DisconnectedRow {
    /// The block's or the instance's name.
    pub name: String,
    pub power_pins_connected: Vec<String>,
    pub power_pins_disconnected: Vec<String>,
    pub signal_pins_connected: Vec<String>,
    pub signal_pins_disconnected: Vec<String>,
    pub disconnected: usize,
    pub critical: usize,
}

/// `Odb.ReportDisconnectedPins` for the whole design: the two metrics, the rows of the full table,
/// and the `[ERROR]` lines the script writes to stderr.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DisconnectedReport {
    /// `design__disconnected_pin__count`.
    pub disconnected_pin_count: usize,
    /// `design__critical_disconnected_pin__count` — what `Checker.DisconnectedPins` gates on.
    pub critical_disconnected_pin_count: usize,
    /// Modules looked at: the block (unless ignored) and each instance not skipped.
    pub modules_checked: usize,
    pub rows: Vec<DisconnectedRow>,
    pub errors: Vec<String>,
}

/// `Module(...)` in `disconnected_pins.py`: one block or instance, its terminals in odb order.
/// Transcribed as the script reads, quirks kept:
/// - no `POWER` terminal ⇒ an error and a disconnected `<ANY POWER PIN>` (INOUT, POWER) added;
///   the GROUND twin never fires — the script starts `ground_found = True`;
/// - terminals are counted by direction; an `INOUT` by POWER / GROUND / any other type; any other
///   direction (FEEDTHRU) is counted nowhere;
/// - critical, for the block: every input, output, power or ground inout when none of that kind is
///   connected (inputs and outputs only if there are any; power and ground whatever their count);
///   for an instance, the FIRST of: disconnected inputs; else every output when none is connected;
///   else disconnected power inouts; else disconnected ground inouts.
///
/// Returns (row, errors).
pub fn disconnected_module(name: &str, terminals: &[Terminal], is_block: bool) -> (DisconnectedRow, Vec<String>) {
    let mut ports: Vec<Terminal> = terminals.to_vec();
    let mut errors = Vec::new();
    let power_found = ports.iter().any(|t| t.sig_type == "POWER");
    if !power_found {
        errors.push(format!("[ERROR] Macro/instance {name} has no power pins- add it to IGNORE_DISCONNECTED_MODULES if this is intentional"));
        ports.push(Terminal { name: "<ANY POWER PIN>".into(), io_type: "INOUT".into(), sig_type: "POWER".into(), connected: false });
    }
    #[derive(Default)]
    struct Kind {
        n: usize,
        connected: usize,
    }
    let (mut inputs, mut outputs, mut power, mut ground, mut other) = (Kind::default(), Kind::default(), Kind::default(), Kind::default(), Kind::default());
    for t in &ports {
        let k = match (t.io_type.as_str(), t.sig_type.as_str()) {
            ("INPUT", _) => &mut inputs,
            ("OUTPUT", _) => &mut outputs,
            ("INOUT", "POWER") => &mut power,
            ("INOUT", "GROUND") => &mut ground,
            ("INOUT", _) => &mut other,
            _ => continue,
        };
        k.n += 1;
        k.connected += usize::from(t.connected);
    }
    if outputs.n != 0 && outputs.connected == 0 {
        errors.push(format!("[ERROR] No outputs of macro/instance '{name}' are connected- add it to IGNORE_DISCONNECTED_MODULES if this is intentional"));
    }
    if inputs.connected != inputs.n && !is_block {
        errors.push(format!("[ERROR] Some inputs of instance '{name}' are not connected- add it to IGNORE_DISCONNECTED_MODULES if this is intentional"));
    }
    let disconnected = [&inputs, &outputs, &power, &ground, &other].iter().map(|k| k.n - k.connected).sum();
    let critical = if is_block {
        let mut c = 0;
        if inputs.connected == 0 && inputs.n != 0 {
            c += inputs.n;
        }
        if outputs.connected == 0 && outputs.n != 0 {
            c += outputs.n;
        }
        if power.connected == 0 {
            c += power.n;
        }
        if ground.connected == 0 {
            c += ground.n;
        }
        c
    } else if inputs.connected != inputs.n {
        inputs.n - inputs.connected
    } else if outputs.connected == 0 {
        outputs.n
    } else if power.n != power.connected {
        power.n - power.connected
    } else if ground.n != ground.connected {
        ground.n - ground.connected
    } else {
        0
    };
    let names = |supply: bool, connected: bool| -> Vec<String> {
        ports.iter().filter(|t| (t.sig_type == "POWER" || t.sig_type == "GROUND") == supply && (supply || t.sig_type == "SIGNAL") && t.connected == connected).map(|t| t.name.clone()).collect()
    };
    let row = DisconnectedRow {
        name: name.to_string(),
        power_pins_connected: names(true, true),
        power_pins_disconnected: names(true, false),
        signal_pins_connected: names(false, true),
        signal_pins_disconnected: names(false, false),
        disconnected,
        critical,
    };
    (row, errors)
}

/// `Odb.ReportDisconnectedPins` (`disconnected_pins.py` `main`): the block unless its name is
/// ignored, then every instance unless its master is ignored or its name starts `clkload` (CTS's
/// dummy clock loads). An instance pin is connected when it has a net; a port when its net reaches
/// at least one instance pin.
pub fn disconnected_pin_report(db: &Db, ignore_modules: &[String]) -> DisconnectedReport {
    let mut report = DisconnectedReport { disconnected_pin_count: 0, critical_disconnected_pin_count: 0, modules_checked: 0, rows: Vec::new(), errors: Vec::new() };
    let add = |report: &mut DisconnectedReport, name: &str, terminals: Vec<Terminal>, is_block: bool| {
        let (row, errors) = disconnected_module(name, &terminals, is_block);
        report.modules_checked += 1;
        report.disconnected_pin_count += row.disconnected;
        report.critical_disconnected_pin_count += row.critical;
        report.errors.extend(errors);
        if row.disconnected != 0 {
            report.rows.push(row);
        }
    };
    let block = db.block_name();
    if !ignore_modules.contains(&block) {
        let terminals = db
            .bterm_names()
            .into_iter()
            .map(|b| {
                let net = db.bterm_net(&b);
                let connected = !net.is_empty() && !db.net_iterms(&net).is_empty();
                Terminal { io_type: db.bterm_get_io_type(&b), sig_type: db.bterm_get_sig_type(&b), connected, name: b }
            })
            .collect();
        add(&mut report, &block, terminals, true);
    }
    for inst in db.inst_names() {
        if ignore_modules.contains(&db.inst_master(&inst)) || inst.starts_with("clkload") {
            continue;
        }
        let terminals = db
            .iterm_names(&inst)
            .into_iter()
            .map(|p| Terminal { io_type: db.iterm_get_io_type(&inst, &p), sig_type: db.iterm_get_sig_type(&inst, &p), connected: !db.net_of(&inst, &p).is_empty(), name: p })
            .collect();
        add(&mut report, &inst, terminals, false);
    }
    report
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

/// One routed net and its wire length, as `wire_lengths.py` lists it.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct WireLengthRow {
    pub net: String,
    /// `dbWire::getLength`, in DBU.
    pub length_dbu: i64,
    /// The length in microns, exactly as the script's `Decimal(length) / Decimal(dbunits)` prints.
    pub length_um: String,
}

/// LibreLane's `Odb.ReportWireLength` (`wire_lengths.py`) plus `Checker.WireLength`.
#[derive(Debug, Clone, Serialize)]
pub struct WireLengthReport {
    /// `route__wirelength__max`, in microns.
    pub max_length_um: f64,
    /// Every net with a wire, longest first.
    pub rows: Vec<WireLengthRow>,
    pub total_wire_length_dbu: u64,
}

/// The rows of `wire_lengths.py`: every net that HAS a wire (a net without one is left out, not
/// listed as 0), sorted by `getLength()` longest first — Python's sort is stable, so equal
/// lengths keep the block's net order.
pub fn wire_length_report(names: &[String], lengths: &[i64], dbu: i64) -> WireLengthReport {
    let mut rows: Vec<WireLengthRow> = names
        .iter()
        .zip(lengths)
        .filter(|(_, &l)| l >= 0)
        .map(|(n, &l)| WireLengthRow { net: n.clone(), length_dbu: l, length_um: decimal_ratio(l as u64, dbu as u64, 0) })
        .collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.length_dbu));
    let max = rows.first().map_or(0.0, |r| r.length_um.parse::<f64>().unwrap_or(f64::NAN));
    let total = rows.iter().map(|r| r.length_dbu as u64).sum();
    WireLengthReport { max_length_um: max, rows, total_wire_length_dbu: total }
}

/// `num / (den * 1000^shift)` as Python's `Decimal` division of two integers prints it: exact, with
/// no trailing fractional zeros and no point when whole (`12345/1000` is `12.345`, `100000/1000`
/// is `100`). Exact whenever `den` has no prime factor but 2 and 5 — every DEF unit in use; any
/// other is printed from a float.
pub fn decimal_ratio(num: u64, den: u64, shift: u32) -> String {
    let den = den as u128 * 1000u128.pow(shift);
    let (mut d, mut twos, mut fives) = (den, 0u32, 0u32);
    while d > 0 && d % 2 == 0 {
        d /= 2;
        twos += 1;
    }
    while d > 0 && d % 5 == 0 {
        d /= 5;
        fives += 1;
    }
    if d != 1 {
        return format!("{}", num as f64 / den as f64);
    }
    let k = twos.max(fives);
    let scaled = num as u128 * (10u128.pow(k) / den);
    let digits = format!("{scaled:0>width$}", width = k as usize + 1);
    let (int, frac) = digits.split_at(digits.len() - k as usize);
    let frac = frac.trim_end_matches('0');
    if frac.is_empty() {
        int.to_string()
    } else {
        format!("{int}.{frac}")
    }
}

/// `to_si` in `wire_lengths.py` (the flow step passes `--human-readable`): microns, divided by
/// 1000 while the value is 1000 or more, as µm, mm or m.
pub fn wire_length_si(num: u64, den: u64) -> String {
    let units = ["µm", "mm", "m"];
    let mut unit = 0usize;
    // value >= 1000 <=> num >= 1000 * den at this unit
    while unit < units.len() - 1 && num as u128 >= den as u128 * 1000u128.pow(unit as u32 + 1) {
        unit += 1;
    }
    format!("{}{}", decimal_ratio(num, den, unit as u32), units[unit])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(name: &str, io: &str, sig: &str, connected: bool) -> Terminal {
        Terminal { name: name.into(), io_type: io.into(), sig_type: sig.into(), connected }
    }

    // Rule (disconnected_pins.py, instance_critical_disconnected_pin_count): an `if/elif` chain —
    // disconnected inputs are critical, and then a disconnected power pin is NOT counted as well.
    #[test]
    fn an_instance_counts_only_the_first_critical_kind() {
        let pins = [t("A", "INPUT", "SIGNAL", false), t("X", "OUTPUT", "SIGNAL", true), t("VPWR", "INOUT", "POWER", false), t("VGND", "INOUT", "GROUND", true)];
        let (row, errors) = disconnected_module("u1", &pins, false);
        assert_eq!((row.disconnected, row.critical), (2, 1), "the input only; the power pin is disconnected but not critical");
        assert_eq!(errors.len(), 1, "some inputs not connected: {errors:?}");
        let pins = [t("A", "INPUT", "SIGNAL", true), t("X", "OUTPUT", "SIGNAL", true), t("VPWR", "INOUT", "POWER", false), t("VGND", "INOUT", "GROUND", false)];
        assert_eq!(disconnected_module("u1", &pins, false).0.critical, 1, "power before ground");
        let pins = [t("A", "INPUT", "SIGNAL", true), t("X", "OUTPUT", "SIGNAL", false), t("Y", "OUTPUT", "SIGNAL", false), t("VPWR", "INOUT", "POWER", true)];
        assert_eq!(disconnected_module("u1", &pins, false).0.critical, 2, "every output when none is connected");
    }

    // Rule (Module.__init__): no POWER terminal adds a disconnected `<ANY POWER PIN>` and an error;
    // the ground check never fires (`ground_found = True` from the start). For the BLOCK, power
    // inouts with none connected are all critical whatever their count — so that pin is critical.
    #[test]
    fn a_missing_power_pin_is_added_and_a_missing_ground_pin_is_not() {
        let ports = [t("in", "INPUT", "SIGNAL", true), t("out", "OUTPUT", "SIGNAL", true)];
        let (row, errors) = disconnected_module("top", &ports, true);
        assert_eq!((row.disconnected, row.critical), (1, 1));
        assert_eq!(row.power_pins_disconnected, ["<ANY POWER PIN>"]);
        assert_eq!(errors.len(), 1, "power only, never ground: {errors:?}");
        assert!(errors[0].contains("has no power pins"));
    }

    // Rule (top_module_critical_disconnected_pin_count): the block's inputs are critical only when
    // NONE is connected — one connected input makes the rest non-critical.
    #[test]
    fn the_block_needs_one_connection_of_each_kind() {
        let ports = [t("a", "INPUT", "SIGNAL", true), t("b", "INPUT", "SIGNAL", false), t("y", "OUTPUT", "SIGNAL", false), t("VDD", "INOUT", "POWER", true), t("VSS", "INOUT", "GROUND", true)];
        let (row, _) = disconnected_module("top", &ports, true);
        assert_eq!((row.disconnected, row.critical), (2, 1), "b is not critical; y is, as the only output");
    }

    // Rule (PortStats): only INPUT, OUTPUT and INOUT are counted; an INOUT of another signal type
    // (CLOCK, ANALOG) is an "other" inout — disconnected, never critical; FEEDTHRU is not counted.
    #[test]
    fn other_inouts_count_but_are_never_critical() {
        let pins = [t("A", "INPUT", "SIGNAL", true), t("X", "OUTPUT", "SIGNAL", true), t("VPWR", "INOUT", "POWER", true), t("AN", "INOUT", "ANALOG", false), t("F", "FEEDTHRU", "SIGNAL", false)];
        let (row, _) = disconnected_module("u1", &pins, false);
        assert_eq!((row.disconnected, row.critical), (1, 0));
    }

    /// Rule (`wire_lengths.py`): lengths are `Decimal(getLength) / Decimal(dbunits)`, printed exact;
    /// the flow step's `--human-readable` steps to mm / m at 1000.
    #[test]
    fn wire_lengths_print_as_python_decimal() {
        assert_eq!(decimal_ratio(12345, 1000, 0), "12.345");
        assert_eq!(decimal_ratio(100000, 1000, 0), "100");
        assert_eq!(decimal_ratio(1, 4000, 0), "0.00025");
        assert_eq!(decimal_ratio(0, 1000, 0), "0");
        assert_eq!(wire_length_si(999_999, 1000), "999.999µm");
        assert_eq!(wire_length_si(1_500_000, 1000), "1.5mm");
        assert_eq!(wire_length_si(1_234_567, 1000), "1.234567mm");
        assert_eq!(wire_length_si(2_000_000_000, 1000), "2m");
    }

    /// Rule: nets without a wire are not rows; longest first, equal lengths in net order.
    #[test]
    fn wire_length_rows_longest_first_stable() {
        let names: Vec<String> = ["a", "b", "c", "d"].iter().map(|s| s.to_string()).collect();
        let r = wire_length_report(&names, &[500, -1, 2000, 500], 1000);
        let order: Vec<&str> = r.rows.iter().map(|x| x.net.as_str()).collect();
        assert_eq!(order, ["c", "a", "d"]);
        assert_eq!((r.max_length_um, r.total_wire_length_dbu), (2.0, 3000));
    }
}
