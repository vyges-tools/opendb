// SPDX-License-Identifier: Apache-2.0
//! Reading 3Dblox interchange files (`.3dbv` / `.3dbx`) into OpenDB.
//!
//! **Phase 1: geometry only.** Chips, regions, chip instances and connections are built; the
//! external collateral a `.3dbv` points at — `APR_tech_file`, `LEF_file`, `DEF_file`,
//! `liberty_file`, `verilog_file`, `bmap` — is *not* read. That is a deliberate boundary, not an
//! oversight, and it has one consequence worth stating rather than discovering: **every chip
//! shares the database's default tech**, where the format's whole point is that each die may
//! carry its own. Enough to read an assembly and lint its structure; not enough to time it.
//!
//! Why this exists at all rather than linking upstream's reader: that path was measured and
//! abandoned — it reaches a Verilog parser and a Liberty reader through OpenSTA at a moving
//! submodule pin, and drags Tcl into a library whose premise is not having it.
//!
//! Upstream's `src/odb/src/3dblox/` (BSD-3-Clause, The OpenROAD Authors) is the authority for
//! what these files mean; this is an independent reader of the same format, not a port.
mod model;
mod parse;
mod preprocess;

pub use model::{Assembly, BloxError, ChipletDef, Connection, Dbv, Dbx, Header, Region, RegionRef};
pub use parse::{parse_dbv, parse_dbx};

use crate::{Db, Error, Result};
use std::collections::BTreeMap;

impl From<BloxError> for Error {
    fn from(e: BloxError) -> Error {
        Error::Odb(e.to_string())
    }
}

/// Read a `.3dbx` assembly and the `.3dbv` definitions it includes.
///
/// Only parses — see [`Db::read_3dblox`] to build a database from the result.
pub fn read_assembly(dbx_path: &str) -> Result<Assembly> {
    let raw = std::fs::read_to_string(dbx_path)
        .map_err(|e| Error::Odb(format!("{dbx_path}: {e}")))?;
    let dbx = parse_dbx(dbx_path, &raw)?;

    let mut defs: BTreeMap<String, ChipletDef> = BTreeMap::new();
    for inc in &dbx.includes {
        let text = std::fs::read_to_string(inc).map_err(|e| Error::Odb(format!("{inc}: {e}")))?;
        for c in parse_dbv(inc, &text)?.chiplets {
            defs.insert(c.name.clone(), c);
        }
    }
    // An instance referencing a definition we never saw is the difference between "unsupported
    // include" and "typo", and only the reader can tell them apart.
    for i in &dbx.insts {
        if !defs.contains_key(&i.reference) {
            return Err(Error::Odb(format!(
                "{dbx_path}: instance `{}` references chiplet `{}`, which none of the included \
                 .3dbv files defines ({} include(s) read)",
                i.name,
                i.reference,
                dbx.includes.len()
            )));
        }
    }
    Ok(Assembly { dbx, defs, lossy_regions: Vec::new() })
}

/// Convert microns to DBU the way the database does — round, never truncate.
fn dbu(microns: f64, dbu_per_micron: i32) -> i32 {
    (microns * dbu_per_micron as f64).round() as i32
}

// Building the database needs the write surface (chip dimensions are generated setters), so the
// loader follows `gen-write`. Parsing does not, and stays available either way — reading a file
// and inspecting it is useful without the ability to construct anything.
#[cfg(feature = "gen-write")]
impl Db {
    /// Read a 3Dblox assembly into this database (geometry only — see the module docs).
    ///
    /// Returns the names of regions whose outline was **not** rectangular, and so lost shape:
    /// the database stores a `Rect`, the format allows a polygon, and squaring one off silently
    /// would be a lie about the design. Callers that care should look at the list.
    pub fn read_3dblox(&mut self, dbx_path: &str) -> Result<Vec<String>> {
        let mut asm = read_assembly(dbx_path)?;

        // The header declares the precision its (micron) coordinates are written at. Adopt it
        // when the database has none, and otherwise require the database to be a whole multiple
        // of it — the same rule odb applies, because a mismatch silently rescales the design.
        let file_precision = asm.dbx.header.precision;
        let db_dbu = self.dbu_per_micron();
        let dbu_per_micron = if db_dbu <= 0 {
            self.set_dbu_per_micron(file_precision);
            file_precision
        } else {
            if file_precision > db_dbu || db_dbu % file_precision != 0 {
                return Err(Error::Odb(format!(
                    "{dbx_path}: file precision {file_precision} is not compatible with the \
                     database's {db_dbu} dbu/micron (must divide it)"
                )));
            }
            db_dbu
        };

        // The design top gets no block and no tech, matching upstream's own
        // `createDesignTopChiplet`: a geometry-only read has no LEF to give it a tech, and
        // manufacturing an empty block here would be inventing structure the file never stated.
        // odb refuses a DIE chip without a technology, and a geometry-only read has no LEF to
        // build one from. A placeholder carrying just the precision is what lets the model
        // exist; Phase 2's per-chiplet APR_tech_file is what replaces it with the real thing.
        if db_dbu <= 0 {
            self.create_tech("blox_placeholder")?;
        }

        let top = asm.dbx.design_name.clone();
        self.create_chip(&top, "", "HIER")?;

        // Definitions before instances: `dbChipInst::create` derives its region instances from
        // the master as it stands, so a region added afterwards is silently not instantiated.
        for def in asm.defs.values() {
            self.create_chip(&def.name, "", &def.chip_type.to_uppercase())?;
            if let Some((w, h)) = def.design_area {
                self.chip_set_width(&def.name, dbu(w, dbu_per_micron))?;
                self.chip_set_height(&def.name, dbu(h, dbu_per_micron))?;
            }
            if let Some(t) = def.thickness {
                self.chip_set_thickness(&def.name, dbu(t, dbu_per_micron))?;
            }
            for r in &def.regions {
                self.create_chip_region(&def.name, &r.name, &r.side.to_uppercase(), "")?;
                if let Some((x1, y1, x2, y2)) = r.bounding_box() {
                    self.set_chip_region_box(
                        &def.name,
                        &r.name,
                        dbu(x1, dbu_per_micron),
                        dbu(y1, dbu_per_micron),
                        dbu(x2, dbu_per_micron),
                        dbu(y2, dbu_per_micron),
                    )?;
                    if !r.is_rectangular() {
                        asm.lossy_regions.push(format!("{}/{}", def.name, r.name));
                    }
                }
            }
        }

        for i in &asm.dbx.insts {
            self.create_chip_inst(&top, &i.reference, &i.name)?;
            let p = &i.placement;
            self.place_chip_inst(
                &top,
                &i.name,
                &p.orient,
                dbu(p.loc.0, dbu_per_micron),
                dbu(p.loc.1, dbu_per_micron),
                dbu(p.z, dbu_per_micron),
            )?;
        }

        for c in &asm.dbx.connections {
            // A virtual bond (`bot: ~`) names no counterpart. odb's create wants two region
            // instances, so it cannot be expressed yet; skipping it silently would understate
            // the design, so it is reported through the same channel as shape loss.
            let Some(bot) = &c.bot else {
                asm.lossy_regions.push(format!("connection {} (virtual, no bottom)", c.name));
                continue;
            };
            // Phase 1 handles the direct case; a nested path needs the list-valued create.
            if c.top.inst_path.len() != 1 || bot.inst_path.len() != 1 {
                asm.lossy_regions
                    .push(format!("connection {} (nested instance path)", c.name));
                continue;
            }
            self.create_chip_conn(
                &c.name,
                &top,
                &c.top.inst_path[0],
                &c.top.region,
                &bot.inst_path[0],
                &bot.region,
                dbu(c.thickness, dbu_per_micron),
            )?;
        }

        self.set_top_chip(&top)?;
        self.construct_unfolded_model()?;
        Ok(asm.lossy_regions)
    }
}
