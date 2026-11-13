// SPDX-License-Identifier: Apache-2.0
//! Print a database's netlist in creation order — instances, nets with their iterms and bterms as
//! odb lists them, and terminals — in the same line format as the reference-side Tcl dump the
//! hierarchical-import gate compares against:
//!
//! ```text
//! == INSTS
//! INST <name> <master>
//! == NETS
//! NET <name> sig=<type> iterms=<inst/pin ...> bterms=<port ...>
//! == BTERMS
//! BTERM <name> <io type> net=<net>
//! ```
//!
//! `cargo run --release --example dump_netlist -- <f.odb> [--dont-touch]`; `--dont-touch` adds
//! ` dnt=0|1` to each instance line.

use vyges_opendb::Db;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args.next().ok_or("usage: dump_netlist <f.odb> [--dont-touch]")?;
    let dnt = args.next().as_deref() == Some("--dont-touch");
    let db = Db::open(&path)?;
    let mut out = std::io::BufWriter::new(std::io::stdout().lock());
    use std::io::Write;
    writeln!(out, "== INSTS")?;
    for i in db.inst_names() {
        if dnt {
            writeln!(out, "INST {i} {} dnt={}", db.inst_master(&i), u8::from(db.inst_is_do_not_touch(&i)))?;
        } else {
            writeln!(out, "INST {i} {}", db.inst_master(&i))?;
        }
    }
    writeln!(out, "== NETS")?;
    for n in db.net_names() {
        let iterms = db.net_iterms(&n).join(" ");
        let bterms = db.net_bterms(&n).join(" ");
        writeln!(out, "NET {n} sig={} iterms={iterms} bterms={bterms}", db.net_sigtype(&n))?;
    }
    writeln!(out, "== BTERMS")?;
    for b in db.bterm_names() {
        writeln!(out, "BTERM {b} {} net={}", db.bterm_get_io_type(&b), db.bterm_net(&b))?;
    }
    Ok(())
}
