// SPDX-License-Identifier: Apache-2.0
//! `vyges-opendb` — a safe, ergonomic Rust API over OpenROAD's OpenDB (`libodb`).
//!
//! Wraps the low-level [`vyges_opendb_lib`] FFI: an owned [`Db`] handle, `&self` for reads and
//! `&mut self` for edits (so Rust's borrow checker enforces no read-while-mutate aliasing),
//! and typed [`Error`]s from the C++ layer. Objects are addressed by name.
//!
//! The write primitives + [`Db::insert_buffer`] are the building blocks for the ECO applier
//! (`InsertECOBuffers`). Legalization (incremental routing / detailed placement) is delegated
//! to the OpenROAD engines separately — this layer only mutates the database.

// The libodb-backed surface (`Db`, `eco`) is unix-only — libodb is not built on non-unix
// targets. `Error`/`Result` stay cross-platform. See vyges-opendb-lib for the rationale.
#[cfg(unix)]
use std::path::Path;
#[cfg(unix)]
use cxx::UniquePtr;
#[cfg(unix)]
use vyges_opendb_lib as sys;

#[cfg(unix)]
pub mod eco;
#[cfg(unix)]
pub mod report;

/// Errors from the OpenDB layer or path handling.
#[derive(Debug)]
pub enum Error {
    /// An error surfaced by the C++ OpenDB layer.
    Odb(String),
    /// A path that is not valid UTF-8.
    NonUtf8Path,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Odb(m) => write!(f, "{m}"),
            Error::NonUtf8Path => write!(f, "path is not valid UTF-8"),
        }
    }
}
impl std::error::Error for Error {}
impl From<cxx::Exception> for Error {
    fn from(e: cxx::Exception) -> Self {
        Error::Odb(e.what().to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(unix)]
fn path_str(p: impl AsRef<Path>) -> Result<String> {
    p.as_ref().to_str().map(str::to_owned).ok_or(Error::NonUtf8Path)
}

// --- centralize libodb's native (C++ utl::Logger) diagnostics through vyges-events ---
#[cfg(unix)]
fn spdlog_level_to_severity(level: i32) -> vyges_events::Severity {
    use vyges_events::Severity::*;
    // spdlog level_enum: trace0 debug1 info2 warn3 err4 critical5 off6(utl `report`)
    match level {
        0 => Trace,
        1 => Debug,
        2 => Info,
        3 => Warn,
        4 | 5 => Error,
        _ => Info,
    }
}

#[cfg(unix)]
fn forward_libodb_log(level: i32, msg: &str) {
    // utl formats "[INFO ODB-0127] <body>" — lift the ODB-0127 id as the clustering code and strip
    // the "[level id]" prefix (the event renderer re-adds its own), leaving a clean body.
    let text = msg.trim_end();
    let (code, body) = match text.strip_prefix('[').and_then(|s| s.split_once(']')) {
        Some((inner, rest)) => (
            inner.split_whitespace().find(|t| t.contains('-')),
            rest.trim_start(),
        ),
        None => (None, text),
    };
    let mut ev = vyges_events::Event::new("vyges-opendb", spdlog_level_to_severity(level), body);
    if let Some(code) = code {
        ev = ev.with_code(code);
    }
    vyges_events::emit(&ev);
}

/// Route libodb's native `utl::Logger` diagnostics (`[INFO ODB-0127] …`) through `vyges-events`,
/// centralizing odb's C++ log output with the rest of the suite's causal trail. Call once at engine
/// start; idempotent. Until called, libodb logs go only to its own stdout (unchanged behavior).
#[cfg(unix)]
pub fn init_events_logging() {
    sys::set_log_sink(forward_libodb_log);
}
/// No-op on non-unix (libodb is unavailable there).
#[cfg(not(unix))]
pub fn init_events_logging() {}

/// An OpenDB design database (owns a `dbDatabase` + its logger). Unix-only.
#[cfg(unix)]
pub struct Db {
    inner: UniquePtr<sys::OdbDb>,
}

#[cfg(unix)]
impl Db {
    /// Read a `.odb` file.
    pub fn open(path: impl AsRef<Path>) -> Result<Db> {
        let inner = sys::open_db(&path_str(path)?)?;
        Ok(Db { inner })
    }

    /// Serialize the database to a `.odb` file.
    pub fn write(&self, path: impl AsRef<Path>) -> Result<()> {
        Ok(sys::write_db(self.r(), &path_str(path)?)?)
    }

    /// Export the design to a DEF file (libodb v1 — DEF 5.8).
    pub fn write_def(&self, path: impl AsRef<Path>) -> Result<()> {
        Ok(sys::write_def(self.r(), &path_str(path)?)?)
    }

    /// Import a DEF into the design. `mode`: `"default"` (design from scratch), `"floorplan"`
    /// (`Odb.ApplyDEFTemplate` — update DIEAREA/TRACKS/ROWS/COMPONENTS/PINS/NETS from a template),
    /// or `"incremental"` (COMPONENTS/PINS only). Non-default modes need an existing design + libs.
    pub fn read_def(&mut self, def_path: impl AsRef<Path>, mode: &str) -> Result<()> {
        Ok(sys::read_def(self.r(), &path_str(def_path)?, mode)?)
    }

    fn r(&self) -> &sys::OdbDb {
        self.inner.as_ref().expect("vyges-opendb: null db handle")
    }

    // ---- read / inspect ------------------------------------------------------
    pub fn block_name(&self) -> String { sys::block_name(self.r()) }
    pub fn num_insts(&self) -> usize { sys::num_insts(self.r()) }
    pub fn num_nets(&self) -> usize { sys::num_nets(self.r()) }
    pub fn num_bterms(&self) -> usize { sys::num_bterms(self.r()) }

    /// Name of the `i`-th instance (empty if out of range).
    pub fn nth_inst_name(&self, i: usize) -> String { sys::nth_inst_name(self.r(), i) }
    /// All instance names.
    pub fn inst_names(&self) -> Vec<String> {
        (0..self.num_insts()).map(|i| self.nth_inst_name(i)).collect()
    }
    /// First library master whose name contains `substr` (empty if none).
    pub fn find_master(&self, substr: &str) -> String { sys::find_master(self.r(), substr) }
    /// First input-signal pin name of `inst` (empty if none).
    pub fn input_pin(&self, inst: &str) -> String { sys::input_pin(self.r(), inst) }
    /// First output-signal pin name of `inst` (empty if none).
    pub fn output_pin(&self, inst: &str) -> String { sys::output_pin(self.r(), inst) }
    /// Net connected to `inst/pin` (empty if unconnected).
    pub fn net_of(&self, inst: &str, pin: &str) -> String { sys::net_of(self.r(), inst, pin) }
    /// Instance origin `(x, y)` in DBU (`(0, 0)` if not found).
    pub fn inst_location(&self, inst: &str) -> (i32, i32) {
        (sys::inst_x(self.r(), inst), sys::inst_y(self.r(), inst))
    }
    /// Name of the `i`-th block port (bterm); empty if out of range.
    pub fn nth_bterm_name(&self, i: usize) -> String { sys::nth_bterm_name(self.r(), i) }
    /// All block port (bterm) names.
    pub fn bterm_names(&self) -> Vec<String> {
        (0..self.num_bterms()).map(|i| self.nth_bterm_name(i)).collect()
    }
    /// Net connected to block port `bterm` (empty if none).
    pub fn bterm_net(&self, bterm: &str) -> String { sys::bterm_net(self.r(), bterm) }
    /// Port first-pin origin `(x, y)` in DBU (`(0, 0)` if none).
    pub fn bterm_location(&self, bterm: &str) -> (i32, i32) {
        (sys::bterm_x(self.r(), bterm), sys::bterm_y(self.r(), bterm))
    }
    /// The master cell name of `inst` (empty if not found).
    pub fn inst_master(&self, inst: &str) -> String { sys::inst_master(self.r(), inst) }
    /// All pin (iterm) names of `inst`.
    pub fn iterm_names(&self, inst: &str) -> Vec<String> {
        (0..sys::num_iterms(self.r(), inst))
            .map(|i| sys::nth_iterm_name(self.r(), inst, i))
            .collect()
    }
    /// Port direction (`INPUT`/`OUTPUT`/`INOUT`/…; empty if not found).
    pub fn bterm_direction(&self, bterm: &str) -> String { sys::bterm_direction(self.r(), bterm) }
    /// Total routed wire length over all nets, in DBU.
    pub fn total_wire_length(&self) -> u64 { sys::total_wire_length(self.r()) }

    /// Run OpenDB's 3D structural lint (`check_3dblox`) over the chiplet assembly and return
    /// the number of violations found — **0 means clean**.
    ///
    /// Covers logical connectivity, floating chips, overlapping dies, unused `INTERNAL_EXT`
    /// regions, connection-region overlap and mating-surface gap versus connection thickness,
    /// bump physical alignment, and alignment markers.
    ///
    /// Violations are filed as ordinary `dbMarker` objects under a `3DBlox` category on the top
    /// chip, one sub-category per check, so the detail is read back through the normal marker
    /// accessors using a slash path — e.g. `3DBlox/Overlapping chips`.
    ///
    /// Takes `&self`: this is a checker, not a repairer. It annotates the in-memory database
    /// with markers and never modifies the design. Errors if there is no top chip.
    pub fn check_3dblox(&self) -> Result<usize> { Ok(sys::check_3dblox(self.r())?) }

    /// Run `f` with OpenDB's diagnostics captured instead of written to its default **stdout**
    /// sink, returning `f`'s value alongside the captured text.
    ///
    /// Needed by anything whose stdout must stay machine-readable — OpenDB writes human-readable
    /// `[WARNING ODB-nnnn] …` lines to stdout, which would otherwise interleave with JSON. The
    /// caller decides where the text goes (stderr, a log, a report field).
    ///
    /// Capture detaches the events forwarder for the duration and restores it afterwards, so
    /// messages emitted inside `f` reach the events trail only through what the caller does with
    /// the returned text.
    pub fn with_captured_logs<T>(&self, f: impl FnOnce(&Self) -> T) -> (T, String) {
        sys::log_capture_begin(self.r());
        let out = f(self);
        (out, sys::log_capture_end(self.r()))
    }
    /// All net names.
    pub fn net_names(&self) -> Vec<String> {
        (0..self.num_nets()).map(|i| sys::nth_net_name(self.r(), i)).collect()
    }
    /// A net's signal type (`SIGNAL`/`POWER`/`GROUND`/`CLOCK`/…; empty if not found).
    pub fn net_sigtype(&self, net: &str) -> String { sys::net_sigtype(self.r(), net) }
    /// Whether `net` is a special (power/routing) net.
    pub fn net_is_special(&self, net: &str) -> bool { sys::net_is_special(self.r(), net) }
    /// The instance pins (`inst/pin`) connected to `net` — the net's instance-side connectivity.
    pub fn net_iterms(&self, net: &str) -> Vec<String> {
        (0..sys::num_net_iterms(self.r(), net))
            .map(|i| sys::nth_net_iterm(self.r(), net, i))
            .collect()
    }
    /// The block ports (bterms) connected to `net`.
    pub fn net_bterms(&self, net: &str) -> Vec<String> {
        (0..sys::num_net_bterms(self.r(), net))
            .map(|i| sys::nth_net_bterm(self.r(), net, i))
            .collect()
    }

    // ---- write primitives ----------------------------------------------------
    pub fn create_net(&mut self, name: &str) -> Result<()> {
        Ok(sys::create_net(self.r(), name)?)
    }
    pub fn create_inst(&mut self, master: &str, name: &str) -> Result<()> {
        Ok(sys::create_inst(self.r(), master, name)?)
    }
    pub fn set_inst_location(&mut self, inst: &str, x: i32, y: i32) -> Result<()> {
        Ok(sys::set_inst_location(self.r(), inst, x, y)?)
    }
    /// Set an instance's orientation (`R0`/`R90`/`R180`/`R270`/`MX`/`MY`/`MXR90`/`MYR90`).
    pub fn set_inst_orient(&mut self, inst: &str, orient: &str) -> Result<()> {
        Ok(sys::set_inst_orient(self.r(), inst, orient)?)
    }
    /// Add a routing/PDN obstruction rectangle on `layer` (DBU). Errors if the layer is unknown.
    pub fn add_obstruction(&mut self, layer: &str, x1: i32, y1: i32, x2: i32, y2: i32) -> Result<()> {
        Ok(sys::add_obstruction(self.r(), layer, x1, y1, x2, y2)?)
    }
    /// Number of obstructions currently in the block.
    pub fn num_obstructions(&self) -> usize { sys::num_obstructions(self.r()) }
    /// Destroy all obstructions; returns the count removed.
    pub fn clear_obstructions(&mut self) -> usize { sys::clear_obstructions(self.r()) }
    /// Place a port (`bterm`) pin box on `layer` at the given DBU rectangle. Errors on unknown
    /// bterm/layer.
    pub fn place_bterm(&mut self, bterm: &str, layer: &str, x1: i32, y1: i32, x2: i32, y2: i32) -> Result<()> {
        Ok(sys::place_bterm(self.r(), bterm, layer, x1, y1, x2, y2)?)
    }
    pub fn connect(&mut self, inst: &str, pin: &str, net: &str) -> Result<()> {
        Ok(sys::connect(self.r(), inst, pin, net)?)
    }
    pub fn disconnect(&mut self, inst: &str, pin: &str) -> Result<()> {
        Ok(sys::disconnect(self.r(), inst, pin)?)
    }

    // ---- composed ECO op -----------------------------------------------------
    /// Insert `buffer_master` (named `buf_name`, placed at `x,y`) on `target_inst/target_pin`.
    ///
    /// The pin's current driver net now feeds the buffer input; the buffer output drives a
    /// fresh net (`{buf_name}_net`) that the target pin is moved onto. Legalization is a
    /// separate, engine-delegated step.
    pub fn insert_buffer(
        &mut self,
        target_inst: &str,
        target_pin: &str,
        buffer_master: &str,
        buf_name: &str,
        x: i32,
        y: i32,
    ) -> Result<()> {
        let driver = self.net_of(target_inst, target_pin);
        if driver.is_empty() {
            return Err(Error::Odb(format!("no net on {target_inst}/{target_pin}")));
        }
        let new_net = format!("{buf_name}_net");
        self.create_net(&new_net)?;
        self.create_inst(buffer_master, buf_name)?;
        self.set_inst_location(buf_name, x, y)?;

        let a = self.input_pin(buf_name);
        let z = self.output_pin(buf_name);
        if a.is_empty() || z.is_empty() {
            return Err(Error::Odb(format!("{buffer_master} lacks an input or output pin")));
        }
        self.connect(buf_name, &a, &driver)?; // buffer input  <- original driver net
        self.connect(buf_name, &z, &new_net)?; // buffer output -> new net
        self.disconnect(target_inst, target_pin)?; // target pin off the original net
        self.connect(target_inst, target_pin, &new_net)?; // target pin -> new net
        Ok(())
    }

    /// Tie an antenna diode (`diode_master`, named `diode_name`, placed at `x,y`) onto the net at
    /// `target_inst/target_pin`.
    ///
    /// Unlike [`insert_buffer`](Self::insert_buffer), a diode is a **leaf**: its single antenna pin
    /// joins the *existing* net — no new net, no rewiring, the original connectivity is unchanged.
    /// This is the ECO antenna-fix primitive (LibreLane `Odb.InsertECODiodes`). Legalization is a
    /// separate, engine-delegated step.
    pub fn insert_diode(
        &mut self,
        target_inst: &str,
        target_pin: &str,
        diode_master: &str,
        diode_name: &str,
        x: i32,
        y: i32,
    ) -> Result<()> {
        let net = self.net_of(target_inst, target_pin);
        if net.is_empty() {
            return Err(Error::Odb(format!("no net on {target_inst}/{target_pin}")));
        }
        self.insert_diode_on_net(&net, diode_master, diode_name, x, y)
    }

    /// Tie an antenna diode onto a named `net` directly (the leaf-tie primitive behind
    /// [`insert_diode`](Self::insert_diode); used for port diodes where the net is known).
    pub fn insert_diode_on_net(
        &mut self,
        net: &str,
        diode_master: &str,
        diode_name: &str,
        x: i32,
        y: i32,
    ) -> Result<()> {
        self.create_inst(diode_master, diode_name)?;
        self.set_inst_location(diode_name, x, y)?;
        // A diode cell's antenna pin is its (single) input-signal pin, e.g. sky130 `DIODE`.
        let pin = self.input_pin(diode_name);
        if pin.is_empty() {
            return Err(Error::Odb(format!("{diode_master} has no input pin to tie the diode")));
        }
        self.connect(diode_name, &pin, net)?; // diode antenna pin -> the net being protected
        Ok(())
    }
}

// Machine-generated read accessors (scripts/generate-bindings.py) — a second `impl Db` block.
#[cfg(unix)]
include!("generated_api.rs");

// Runtime registry over the generated surface: field discovery + get/set dispatch (drives the
// generic `get`/`set`/`fields` CLI subcommands and, through them, `vyges mcp`).
#[cfg(unix)]
pub mod registry {
    include!("generated_registry.rs");
}

// Machine-generated setters — a third `impl Db` block, gated behind `gen-write` (L2/write).
#[cfg(all(unix, feature = "gen-write"))]
include!("generated_write_api.rs");
