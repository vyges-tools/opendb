// SPDX-License-Identifier: Apache-2.0
//! ECO steps over the design database — Loom-native equivalents of LibreLane's `Odb.*`
//! surgery steps. These mutate the database only; legalization (incremental routing /
//! detailed placement) is delegated to the OpenROAD engines as separate flow steps.

use serde::Deserialize;

use crate::{Db, Error, Result};

/// One entry of the `INSERT_ECO_BUFFERS` config — the LibreLane-compatible shape.
#[derive(Debug, Clone, Deserialize)]
pub struct EcoBuffer {
    /// `"instance/pin"` — the pin to buffer.
    pub target: String,
    /// Buffer master cell name.
    pub buffer: String,
}

/// Apply `InsertECOBuffers`: for each spec, splice its buffer onto the target pin, placing
/// the buffer at the target instance's location. Returns the number of buffers inserted.
///
/// Mirrors LibreLane's `Odb.InsertECOBuffers` (`eco_buffer.py`) database surgery — the
/// downstream `grt` incremental-route + `dpl` legalization is a separate engine step.
pub fn insert_eco_buffers(db: &mut Db, specs: &[EcoBuffer]) -> Result<usize> {
    for (i, spec) in specs.iter().enumerate() {
        // rsplit: a hierarchical instance name contains '/', and pin names do not — splitting
        // at the FIRST separator would silently target a wrong (or nonexistent) instance.
        let (inst, pin) = spec.target.rsplit_once('/').ok_or_else(|| {
            Error::Odb(format!("bad target '{}' (expected inst/pin)", spec.target))
        })?;
        let (x, y) = db.inst_location(inst);
        let name = format!("eco_buffer_{i}");
        db.insert_buffer(inst, pin, &spec.buffer, &name, x, y)?;
    }
    Ok(specs.len())
}

/// One entry of the `INSERT_ECO_DIODES` config — the LibreLane-compatible shape.
#[derive(Debug, Clone, Deserialize)]
pub struct EcoDiode {
    /// `"instance/pin"` — the pin whose net gets an antenna diode.
    pub target: String,
    /// Antenna-diode master cell name.
    pub diode: String,
}

/// Apply `InsertECODiodes`: for each spec, tie its antenna diode onto the target pin's net,
/// placing the diode at the target instance's location. Returns the number of diodes inserted.
///
/// Mirrors LibreLane's `Odb.InsertECODiodes` database surgery — a diode is a leaf tied onto an
/// existing net (no rewiring, unlike a buffer). Downstream legalization is a separate engine step.
pub fn insert_eco_diodes(db: &mut Db, specs: &[EcoDiode]) -> Result<usize> {
    for (i, spec) in specs.iter().enumerate() {
        // rsplit: a hierarchical instance name contains '/', and pin names do not — splitting
        // at the FIRST separator would silently target a wrong (or nonexistent) instance.
        let (inst, pin) = spec.target.rsplit_once('/').ok_or_else(|| {
            Error::Odb(format!("bad target '{}' (expected inst/pin)", spec.target))
        })?;
        let (x, y) = db.inst_location(inst);
        let name = format!("eco_diode_{i}");
        db.insert_diode(inst, pin, &spec.diode, &name, x, y)?;
    }
    Ok(specs.len())
}

/// One entry of the `MANUAL_GLOBAL_PLACEMENT` config: an instance and its origin (DBU).
#[derive(Debug, Clone, Deserialize)]
pub struct GlobalPlacement {
    pub instance: String,
    pub x: i32,
    pub y: i32,
}

/// Apply `ManualGlobalPlacement`: set each listed instance's origin. Returns the count placed.
/// Mirrors LibreLane's `Odb.ManualGlobalPlacement` — fixes specific cells before global placement.
pub fn manual_global_placement(db: &mut Db, specs: &[GlobalPlacement]) -> Result<usize> {
    for spec in specs {
        db.set_inst_location(&spec.instance, spec.x, spec.y)?;
    }
    Ok(specs.len())
}

/// One entry of the `MANUAL_MACRO_PLACEMENT` config: instance, origin (DBU), and orientation.
#[derive(Debug, Clone, Deserialize)]
pub struct MacroPlacement {
    pub instance: String,
    pub x: i32,
    pub y: i32,
    /// `R0`/`R90`/`R180`/`R270`/`MX`/`MY`/`MXR90`/`MYR90`; omitted leaves the orientation as-is.
    #[serde(default)]
    pub orient: Option<String>,
}

/// Apply `ManualMacroPlacement`: place each macro at its origin + orientation. Returns the count.
/// Mirrors LibreLane's `Odb.ManualMacroPlacement` (macros are placed + oriented before the flow).
pub fn manual_macro_placement(db: &mut Db, specs: &[MacroPlacement]) -> Result<usize> {
    for spec in specs {
        db.set_inst_location(&spec.instance, spec.x, spec.y)?;
        if let Some(orient) = &spec.orient {
            db.set_inst_orient(&spec.instance, orient)?;
        }
    }
    Ok(specs.len())
}

/// One entry of the `SET_POWER_CONNECTIONS` config: wire an instance pin to a (power) net.
#[derive(Debug, Clone, Deserialize)]
pub struct PowerConnection {
    pub instance: String,
    pub pin: String,
    pub net: String,
}

/// Apply `SetPowerConnections`: connect each listed instance pin to its (special/power) net.
/// Returns the count. Mirrors LibreLane's `Odb.SetPowerConnections` — here the PWR/GND pin→net
/// mapping is provided explicitly (from Flow IR) rather than auto-derived from the PDN.
pub fn set_power_connections(db: &mut Db, specs: &[PowerConnection]) -> Result<usize> {
    for spec in specs {
        db.connect(&spec.instance, &spec.pin, &spec.net)?;
    }
    Ok(specs.len())
}

/// One obstruction rectangle on a tech layer (DBU corners).
#[derive(Debug, Clone, Deserialize)]
pub struct Obstruction {
    pub layer: String,
    pub llx: i32,
    pub lly: i32,
    pub urx: i32,
    pub ury: i32,
}

/// Add obstruction rectangles. Covers LibreLane's `Odb.AddPDNObstructions` **and**
/// `Odb.AddRoutingObstructions` — both create `dbObstruction`s; the PDN-vs-routing distinction is
/// only which layers you list. Returns the count added.
pub fn add_obstructions(db: &mut Db, specs: &[Obstruction]) -> Result<usize> {
    for o in specs {
        db.add_obstruction(&o.layer, o.llx, o.lly, o.urx, o.ury)?;
    }
    Ok(specs.len())
}

/// Remove all obstructions. Covers `Odb.RemovePDNObstructions` / `Odb.RemoveRoutingObstructions`.
/// Returns the count removed.
pub fn remove_obstructions(db: &mut Db) -> usize {
    db.clear_obstructions()
}

/// One entry of the `CUSTOM_IO_PLACEMENT` config: a port pin box on a layer (DBU corners).
#[derive(Debug, Clone, Deserialize)]
pub struct IoPlacement {
    pub port: String,
    pub layer: String,
    pub llx: i32,
    pub lly: i32,
    pub urx: i32,
    pub ury: i32,
}

/// Apply `CustomIOPlacement`: place each listed port pin at its rectangle. Returns the count.
/// Mirrors LibreLane's `Odb.CustomIOPlacement` — fixes specific I/O pin locations/layers.
pub fn custom_io_placement(db: &mut Db, specs: &[IoPlacement]) -> Result<usize> {
    for s in specs {
        db.place_bterm(&s.port, &s.layer, s.llx, s.lly, s.urx, s.ury)?;
    }
    Ok(specs.len())
}

/// The `DIODES_ON_PORTS` config: the diode cell, and optionally specific ports (default: all).
#[derive(Debug, Clone, Deserialize)]
pub struct DiodesOnPorts {
    /// Antenna-diode master cell name.
    pub diode: String,
    /// Specific port (bterm) names; empty = every port that carries a net.
    #[serde(default)]
    pub ports: Vec<String>,
}

/// Apply `DiodesOnPorts`: tie an antenna diode onto each selected port's net, placed at the
/// port's first-pin location. Returns the number of diodes inserted (unconnected ports skipped).
/// Mirrors LibreLane's `Odb.DiodesOnPorts` — blanket antenna protection on I/O ports.
pub fn diodes_on_ports(db: &mut Db, spec: &DiodesOnPorts) -> Result<usize> {
    let ports = if spec.ports.is_empty() {
        db.bterm_names()
    } else {
        spec.ports.clone()
    };
    let mut n = 0;
    for (i, port) in ports.iter().enumerate() {
        let net = db.bterm_net(port);
        if net.is_empty() {
            continue; // unconnected port — nothing to protect
        }
        let (x, y) = db.bterm_location(port);
        db.insert_diode_on_net(&net, &spec.diode, &format!("port_diode_{i}"), x, y)?;
        n += 1;
    }
    Ok(n)
}

// ---- ECO plan: replay what the timer decided -----------------------------------------------

/// Schema tag this applier understands. A plan carrying anything else is refused rather than
/// interpreted — a mis-read plan edits a design, so guessing is not an acceptable failure mode.
pub const PLAN_SCHEMA: &str = "vyges-eco-plan-v1";

/// One fix from an ECO plan, as emitted by the timing planner.
#[derive(Debug, Clone, Deserialize)]
pub struct PlanFix {
    /// `"insert_delay"` or `"resize"`.
    pub op: String,
    /// `"inst/pin"` for an insertion; the instance name for a resize.
    pub target: String,
    pub inst: String,
    #[serde(default)]
    pub pin: String,
    pub cell: String,
    /// Instance name the planner chose for the new cell. Honoured verbatim so a plan is
    /// deterministic and its effects are traceable back to it.
    #[serde(default)]
    pub name: String,
}

/// An ECO plan produced by the timing planner.
///
/// The planner and this applier are joined by a **file**, not a library call: one lives in the
/// timer and one in the database layer, and neither should have to link the other. It also
/// makes the plan reviewable before it touches a design.
#[derive(Debug, Clone, Deserialize)]
pub struct EcoPlan {
    pub schema: String,
    pub design: String,
    #[serde(default)]
    pub fixes: Vec<PlanFix>,
}

/// What replaying a plan did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedPlan {
    pub applied: usize,
    /// Instance names of the cells this created, in plan order.
    pub inserted: Vec<String>,
}

/// Replay a timing-repair plan into the database, **all or nothing**.
///
/// The whole plan is applied inside a single ECO journal: if any fix fails, everything already
/// applied is rolled back and the error is returned. A partially-applied plan is the one
/// outcome worth ruling out — the design would no longer match either the plan or the timing
/// that justified it, while looking like it worked.
///
/// `strict_design` refuses a plan whose `design` does not match the block. Plans are files and
/// files get moved around; applying one to the wrong block would be silent corruption.
///
/// Legalization is **not** performed and never is here: inserted cells sit at their target's
/// location and overlap it. Run detailed placement, re-extract parasitics and re-time before
/// believing any number — the plan's predicted slacks were estimates against the pre-repair
/// parasitics.
pub fn apply_eco_plan(db: &mut Db, plan_json: &str, strict_design: bool) -> Result<AppliedPlan> {
    let plan: EcoPlan = serde_json::from_str(plan_json)
        .map_err(|e| Error::Odb(format!("cannot parse ECO plan: {e}")))?;
    if plan.schema != PLAN_SCHEMA {
        return Err(Error::Odb(format!(
            "unsupported ECO plan schema '{}' (expected '{PLAN_SCHEMA}')",
            plan.schema
        )));
    }
    if strict_design {
        let block = db.block_name();
        if !plan.design.is_empty() && plan.design != block {
            return Err(Error::Odb(format!(
                "plan targets design '{}' but this database holds '{block}'",
                plan.design
            )));
        }
    }

    let mut inserted = Vec::new();
    let fixes = plan.fixes.clone();
    db.eco_try(|db| {
        for (i, fix) in fixes.iter().enumerate() {
            match fix.op.as_str() {
                "insert_delay" => {
                    // rsplit for the same reason as above: hierarchical instance names contain '/'
                    let (inst, pin) = match fix.target.rsplit_once('/') {
                        Some(v) => v,
                        None => (fix.inst.as_str(), fix.pin.as_str()),
                    };
                    let name = if fix.name.is_empty() {
                        format!("vy_eco_{i}")
                    } else {
                        fix.name.clone()
                    };
                    let (x, y) = db.inst_location(inst);
                    db.insert_buffer(inst, pin, &fix.cell, &name, x, y)?;
                    inserted.push(name);
                }
                // Resize needs dbInst::swapMaster, which is not bound yet. Refuse loudly: a
                // silently skipped fix would leave the design timing-inconsistent with the plan.
                other => {
                    return Err(Error::Odb(format!(
                        "ECO plan fix #{i}: unsupported op '{other}'"
                    )))
                }
            }
        }
        Ok(true)
    })?;

    Ok(AppliedPlan { applied: inserted.len(), inserted })
}
