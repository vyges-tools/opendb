// SPDX-License-Identifier: Apache-2.0
//! Print a database's flat and module views, one line per object in odb's orders, so two
//! databases (one edited here, one by another tool) can be compared with `diff`.
//!
//! Usage: cargo run --release --example dump_hierarchy -- <f.odb>

use vyges_opendb::Db;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("usage: dump_hierarchy <f.odb>")?;
    let db = Db::open(&path)?;
    for line in db.dump_hierarchy()? {
        println!("{line}");
    }
    Ok(())
}
