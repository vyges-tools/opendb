// SPDX-License-Identifier: Apache-2.0
//! The block's netlist as structural Verilog: its ports, its wires, one instance per cell with
//! each connected pin, and an `assign` where an output port's net is named otherwise — the text a
//! timer, a simulator or an equivalence checker reads back.
//!
//! Written as the reference writes it, so the two compare line for line:
//! - ports: the scalar ones in database order, then each bus (bits grouped by `name[i]`), by name,
//!   most significant bit first — a master's pins the same way;
//! - wires: every net that is not a port and not a supply net, in database order; bits of a bus
//!   declared once, as `wire [hi:lo] name;`, after the scalars, by name;
//! - instances sorted by name; a pin with no net left out; a bus pin as `{msb, …, lsb}` with an
//!   unconnected bit as `_NCn` (declared as a wire);
//! - names escaped (`\name ` with its terminating space) wherever Verilog needs it.
//!
//! Supply pins and nets are left out unless asked for (`include_pwr_gnd`).

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::Db;

/// A bus bit's base and index: `name[3]` → (`name`, 3), not when the bracket is escaped (`\]`).
fn bus_bit(name: &str) -> Option<(&str, i64)> {
    let b = name.as_bytes();
    let len = b.len();
    if len < 4 || b[len - 2] == b'\\' || b[len - 1] != b']' {
        return None;
    }
    let left = name.rfind('[')?;
    if left + 1 < len && b[left + 1].is_ascii_digit() {
        let digits: String = name[left + 1..].chars().take_while(|c| c.is_ascii_digit()).collect();
        Some((&name[..left], digits.parse().ok()?))
    } else {
        None
    }
}

/// A database name as a Verilog identifier. `brackets`: a bracket forces escaping (a port's
/// name); otherwise only characters outside `[A-Za-z0-9_]` do. A database escape (`\x`) is
/// dropped, `\\` kept as one; an escaped identifier ends with a space.
fn verilog_name(name: &str, brackets: bool) -> String {
    let mut out = String::from("\\");
    let mut escaped = false;
    let mut chars = name.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            escaped = true;
            match chars.peek() {
                Some('\\') => {
                    out.push('\\');
                    chars.next();
                }
                Some(_) => {}
                None => out.push('\\'),
            }
        } else {
            let ident = ch.is_ascii_alphanumeric() || ch == '_';
            if !ident || (brackets && (ch == '[' || ch == ']')) {
                escaped = true;
            }
            out.push(ch);
        }
    }
    if name.chars().next().is_some_and(|c| !(c.is_ascii_alphabetic() || c == '_')) {
        escaped = true;
    }
    if escaped {
        out.push(' ');
        out
    } else {
        name.to_string()
    }
}

/// A net's Verilog name: a bus bit as `base[i]`, else the name escaped as a port's is.
fn net_name(name: &str) -> String {
    match bus_bit(name) {
        Some((base, i)) => format!("{}[{i}]", verilog_name(base, false)),
        None => verilog_name(name, true),
    }
}

/// One port of a cell after bus grouping: a scalar, or a bus of bits most significant first.
enum Port {
    Scalar(String),
    Bus { name: String, from: i64, to: i64, bits: Vec<String> },
}

/// Group bus bits into buses: the scalars in the order given, then each bus by name, its bits
/// most significant first (`from` the highest index).
fn group_ports(names: &[String]) -> Vec<Port> {
    let mut out = Vec::new();
    let mut buses: BTreeMap<String, Vec<(i64, String)>> = BTreeMap::new();
    for n in names {
        match bus_bit(n) {
            Some((base, i)) => buses.entry(base.to_string()).or_default().push((i, n.clone())),
            None => out.push(Port::Scalar(n.clone())),
        }
    }
    for (name, mut bits) in buses {
        bits.sort_by_key(|b| std::cmp::Reverse(b.0));
        let (from, to) = (bits[0].0, bits[bits.len() - 1].0);
        out.push(Port::Bus { name, from, to, bits: bits.into_iter().map(|(_, n)| n).collect() });
    }
    out
}

fn is_supply(sig_type: &str) -> bool {
    sig_type == "POWER" || sig_type == "GROUND"
}

fn direction(io_type: &str, sig_type: &str) -> &'static str {
    if is_supply(sig_type) {
        return "inout";
    }
    match io_type {
        "INPUT" => "input",
        "OUTPUT" => "output",
        _ => "inout",
    }
}

/// The block as structural Verilog (see the module documentation). `remove_cells`: masters whose
/// instances are left out. A block with hierarchy (module instances) is refused: only a flat
/// netlist is written.
pub fn write_verilog(db: &Db, include_pwr_gnd: bool, remove_cells: &[String]) -> Result<String, String> {
    if db.has_hierarchy() {
        return Err("write-verilog: the block has hierarchical module instances; only a flat netlist is written".into());
    }
    let mut v = String::new();
    // ── the module and its ports ──
    let bterms: Vec<String> = db
        .bterm_names()
        .into_iter()
        .filter(|b| include_pwr_gnd || !is_supply(&db.bterm_get_sig_type(b)))
        .collect();
    let ports = group_ports(&bterms);
    let port_names: HashSet<&str> = bterms.iter().map(String::as_str).collect();
    v.push_str(&format!("module {} (", verilog_name(&db.block_name(), false)));
    let heads: Vec<String> = ports
        .iter()
        .map(|p| match p {
            Port::Scalar(n) => verilog_name(n, true),
            Port::Bus { name, .. } => verilog_name(name, true),
        })
        .collect();
    v.push_str(&heads.join(",\n    "));
    v.push_str(");\n");
    for p in &ports {
        let (first, range, name) = match p {
            Port::Scalar(n) => (n.as_str(), String::new(), verilog_name(n, true)),
            Port::Bus { name, from, to, bits } => (bits[0].as_str(), format!(" [{from}:{to}]"), verilog_name(name, true)),
        };
        let dir = direction(&db.bterm_get_io_type(first), &db.bterm_get_sig_type(first));
        v.push_str(&format!(" {dir}{range} {name};\n"));
    }
    v.push('\n');

    // ── the instances, by name, each with its master's pins ──
    let remove: HashSet<&str> = remove_cells.iter().map(String::as_str).collect();
    let mut insts: Vec<(String, String)> =
        db.inst_names().into_iter().map(|i| (db.inst_master(&i), i)).filter(|(m, _)| !remove.contains(m.as_str())).collect();
    insts.sort_by(|a, b| a.1.cmp(&b.1));
    let mut master_ports: HashMap<String, Vec<Port>> = HashMap::new();
    let mut nc = 0usize;
    let mut body = String::new();
    for (master, inst) in &insts {
        let ports = master_ports.entry(master.clone()).or_insert_with(|| {
            let terms: Vec<String> = db
                .master_get_m_terms(master)
                .into_iter()
                .filter(|t| include_pwr_gnd || !is_supply(&db.mterm_get_sig_type(master, t)))
                .collect();
            group_ports(&terms)
        });
        let mut pins = Vec::new();
        for p in ports.iter() {
            match p {
                Port::Scalar(t) => {
                    let net = db.net_of(inst, t);
                    if !net.is_empty() {
                        pins.push(format!(".{}({})", verilog_name(t, true), net_name(&net)));
                    }
                }
                Port::Bus { name, bits, .. } => {
                    let members: Vec<String> = bits
                        .iter()
                        .map(|b| {
                            let net = db.net_of(inst, b);
                            if net.is_empty() {
                                nc += 1;
                                format!("_NC{nc}")
                            } else {
                                net_name(&net)
                            }
                        })
                        .collect();
                    pins.push(format!(".{}({{{}}})", verilog_name(name, true), members.join(",\n    ")));
                }
            }
        }
        body.push_str(&format!(" {} {} ({});\n", verilog_name(master, false), verilog_name(inst, false), pins.join(",\n    ")));
    }

    // ── the wires: every net that is not a port, in database order; buses by name ──
    let mut buses: BTreeMap<String, (i64, i64)> = BTreeMap::new();
    for net in db.net_names() {
        if !include_pwr_gnd && is_supply(&db.net_sigtype(&net)) || port_names.contains(net.as_str()) {
            continue;
        }
        match bus_bit(&net) {
            Some((base, i)) => {
                let r = buses.entry(base.to_string()).or_insert((i64::MIN, i64::MAX));
                r.0 = r.0.max(i);
                r.1 = r.1.min(i);
            }
            None => v.push_str(&format!(" wire {};\n", net_name(&net))),
        }
    }
    for (name, (hi, lo)) in &buses {
        v.push_str(&format!(" wire [{hi}:{lo}] {};\n", net_name(name)));
    }
    for i in 1..=nc {
        v.push_str(&format!(" wire _NC{i};\n"));
    }
    v.push('\n');
    v.push_str(&body);

    // ── an output port whose net is named otherwise: `assign port = net;` ──
    for b in &bterms {
        let net = db.bterm_net(b);
        let sig = db.net_sigtype(&net);
        let out = db.bterm_get_io_type(b) == "OUTPUT" || (include_pwr_gnd && is_supply(&db.bterm_get_sig_type(b)));
        if !net.is_empty() && (include_pwr_gnd || !is_supply(&sig)) && out && *b != net {
            v.push_str(&format!(" assign {} = {};\n", net_name(b), net_name(&net)));
        }
    }
    v.push_str("endmodule\n");
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Escaping as the reference's Verilog writer escapes: an identifier as is; anything else
    /// behind `\` with a trailing space; a database escape dropped; a bracket in a port's name
    /// escaped, a bus bit's index kept.
    #[test]
    fn names_are_escaped_as_the_reference_escapes_them() {
        assert_eq!(verilog_name("_1434_", false), "_1434_");
        assert_eq!(verilog_name("a.b", false), "\\a.b ");
        assert_eq!(verilog_name("1abc", false), "\\1abc ");
        assert_eq!(net_name("wptr_q\\[2\\]"), "\\wptr_q[2] ");
        assert_eq!(net_name("count_o[3]"), "count_o[3]");
        assert_eq!(net_name("u0/x[1]"), "\\u0/x [1]");
        assert_eq!(verilog_name("p[2]", true), "\\p[2] ");
    }

    /// Bus bits grouped after the scalars, by name, most significant bit first.
    #[test]
    fn ports_group_into_buses_after_the_scalars() {
        let names: Vec<String> = ["b[0]", "clk", "a[1]", "b[1]", "a[0]", "rst"].iter().map(|s| s.to_string()).collect();
        let g = group_ports(&names);
        let shape: Vec<String> = g
            .iter()
            .map(|p| match p {
                Port::Scalar(n) => n.clone(),
                Port::Bus { name, from, to, bits } => format!("{name}[{from}:{to}]={}", bits.join(",")),
            })
            .collect();
        assert_eq!(shape, ["clk", "rst", "a[1:0]=a[1],a[0]", "b[1:0]=b[1],b[0]"]);
    }
}
