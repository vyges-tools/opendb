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
pub mod blox;
#[cfg(unix)]
pub mod d2d;
#[cfg(unix)]
pub mod eco;
#[cfg(unix)]
pub mod report;
#[cfg(unix)]
pub mod view3d;

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

    /// An empty database — the starting point when a design is being **built** rather than read,
    /// such as loading a 3Dblox assembly, which brings its own precision and its own chips.
    pub fn new() -> Db { Db { inner: sys::new_db() } }

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

    // ---- 3D / chiplet construction ------------------------------------------
    // Until these, the 3D surface was read-only in practice: a chiplet assembly could be
    // inspected and its chips moved, but the only way to bring one into existence was a
    // separate C++ program. These are odb's own `dbChip*::create` statics, hand-bound because
    // their signatures are heterogeneous enough that the generator cannot reach them.
    //
    // Order matters twice, and neither is enforceable by types:
    //   * a master chip's regions and bumps must exist BEFORE any `dbChipInst` of it is
    //     created — `create` derives the region/bump instances from the master as it stands,
    //     and anything added later is silently not instantiated for that inst;
    //   * [`Db::set_top_chip`] must name the assembly, or every unfolded table reads empty.

    /// Create a `dbChip`. `chip_type` is `DIE` | `RDL` | `IP` | `SUBSTRATE` | `HIER`.
    ///
    /// `tech` selects the chip's own `dbTech` by name — the per-chip tech is what lets dies
    /// from different processes coexist in one database. Empty selects the database default,
    /// which is the single-process case.
    pub fn create_chip(&mut self, name: &str, tech: &str, chip_type: &str) -> Result<()> {
        Ok(sys::chip_create(self.r(), name, tech, chip_type)?)
    }

    /// Give a chip its own `dbBlock` — the die's design. The top chip needs one for the
    /// block-level accessors to resolve through it; a die needs one to hold the instances its
    /// bumps wrap.
    pub fn create_chip_block(&mut self, chip: &str, name: &str) -> Result<()> {
        Ok(sys::chip_block_create(self.r(), chip, name)?)
    }

    /// Instantiate `master_chip` inside `parent_chip`. See the ordering note above: the
    /// master's regions and bumps must already exist.
    pub fn create_chip_inst(&mut self, parent_chip: &str, master_chip: &str, name: &str) -> Result<()> {
        Ok(sys::chip_inst_create(self.r(), parent_chip, master_chip, name)?)
    }

    /// Create a bonding region on a chip. `side` is `FRONT` | `BACK` | `INTERNAL` |
    /// `INTERNAL_EXT`; `layer` is a tech-layer name, or empty for none.
    pub fn create_chip_region(&mut self, chip: &str, name: &str, side: &str, layer: &str) -> Result<()> {
        Ok(sys::chip_region_create(self.r(), chip, name, side, layer)?)
    }

    /// Set a region's footprint. Without it the region has no extent, and the checks that
    /// reason about footprints have nothing to test.
    pub fn set_chip_region_box(&mut self, chip: &str, region: &str, x1: i32, y1: i32, x2: i32, y2: i32) -> Result<()> {
        Ok(sys::chip_region_set_box(self.r(), chip, region, x1, y1, x2, y2)?)
    }

    /// Place a bump on a region, wrapping an instance in that chip's own block.
    pub fn create_chip_bump(&mut self, chip: &str, region: &str, inst: &str) -> Result<()> {
        Ok(sys::chip_bump_create(self.r(), chip, region, inst)?)
    }

    /// Bond two region instances together with a physical thickness.
    ///
    /// odb takes a *path* of chip instances on each side, to name a region inside a nested
    /// assembly. This binds the direct case — one hop per side — which covers a bond between
    /// two chips in the same parent. Deeper paths are expressible upstream and not here yet.
    #[allow(clippy::too_many_arguments)]
    pub fn create_chip_conn(&mut self, name: &str, parent_chip: &str, top_inst: &str, top_region: &str,
                            bottom_inst: &str, bottom_region: &str, thickness: i32) -> Result<()> {
        Ok(sys::chip_conn_create(self.r(), name, parent_chip, top_inst, top_region, bottom_inst, bottom_region, thickness)?)
    }

    /// Create a logical net spanning bump instances across chips.
    pub fn create_chip_net(&mut self, chip: &str, name: &str) -> Result<()> {
        Ok(sys::chip_net_create(self.r(), chip, name)?)
    }

    /// Create a named traversal route through the assembly.
    pub fn create_chip_path(&mut self, chip: &str, name: &str) -> Result<()> {
        Ok(sys::chip_path_create(self.r(), chip, name)?)
    }

    /// Associate a bump instance with a logical 3D net.
    ///
    /// The bump is addressed by its position in `(chip_inst, region)`. Needed because the
    /// logical-connectivity check compares the nets of physically aligned bump pairs — without
    /// any net associations it has nothing to disagree about and passes on any design.
    pub fn add_chip_net_bump(&mut self, chip: &str, net: &str, chip_inst: &str, region: &str, bump_index: usize) -> Result<()> {
        Ok(sys::chip_net_add_bump(self.r(), chip, net, chip_inst, region, bump_index)?)
    }

    /// Declare an alignment-marker rule between two masters, with a tolerance in DBU.
    ///
    /// The alignment-marker check returns immediately when no rule exists, so a design without
    /// one is not so much clean as unexamined.
    pub fn create_alignment_marker_rule(&mut self, master_a: &str, master_b: &str, tolerance: i32) -> Result<()> {
        Ok(sys::alignment_marker_rule_create(self.r(), master_a, master_b, tolerance)?)
    }

    /// Root the assembly at `chip`.
    ///
    /// **Required before any unfolded query or lint.** The unfolded builder starts from the top
    /// chip and walks its chip instances, so with it left pointing at a flat design every
    /// unfolded table reads empty — and nothing says why.
    pub fn set_top_chip(&mut self, chip: &str) -> Result<()> {
        Ok(sys::set_top_chip(self.r(), chip)?)
    }

    /// Build a named technology from a LEF file.
    ///
    /// This is how a chiplet gets its **own** technology, from the `APR_tech_file` its `.3dbv`
    /// names — the mechanism that lets dies from different processes share one database.
    pub fn tech_from_lef(&mut self, name: &str, lef_path: &str) -> Result<()> {
        Ok(sys::tech_from_lef(self.r(), name, lef_path)?)
    }

    /// Create the database's technology, carrying only the precision already set.
    ///
    /// odb refuses to create a `DIE` chip without a technology; a geometry-only 3Dblox read has
    /// no LEF to build a real one from, so this is the placeholder that lets the model exist.
    pub fn create_tech(&mut self, name: &str) -> Result<()> {
        Ok(sys::tech_create(self.r(), name)?)
    }

    /// Database precision, in DBU per micron (0 when unset).
    pub fn dbu_per_micron(&self) -> i32 { sys::dbu_per_micron(self.r()) }

    /// Set the database precision. A 3Dblox header declares the precision its micron
    /// coordinates are written at, and reading one has to reconcile the two.
    pub fn set_dbu_per_micron(&mut self, dbu: i32) {
        sys::set_dbu_per_micron(self.r(), dbu)
    }

    /// Rebuild the derived 3D tables (unfolded chip insts, regions, bumps) from the folded
    /// chip hierarchy.
    ///
    /// **Call this after moving or reorienting a `dbChipInst`, before reading any unfolded
    /// query.** The unfolded tables are derived and never serialised — the reader builds them
    /// on open — so nothing rebuilds them when a chip moves, and [`Db::unfoldedregion_get_surface_z`]
    /// and friends keep answering from the previous placement: no error, no warning, just a
    /// stale number.
    ///
    /// [`Db::check_3dblox`] rebuilds the model itself, so the linter does **not** need this —
    /// measured, because assuming otherwise is the natural mistake. Pinned by a test, since a
    /// change upstream would otherwise turn every lint-after-move into a silently stale answer.
    ///
    /// Errors if there is no top chip.
    pub fn construct_unfolded_model(&mut self) -> Result<()> {
        Ok(sys::construct_unfolded_model(self.r())?)
    }

    /// Replace an instance's library cell in place — the resize / Vt-swap move.
    ///
    /// Returns `false` when OpenDB refuses because the instance is bound to a block hierarchy.
    /// **Errors** when the instance or master is unknown, and when the instance is marked
    /// don't-touch — OpenDB raises there rather than returning `false`, and a don't-touch
    /// instance being quietly resized is precisely what that flag exists to prevent.
    ///
    /// OpenDB **does** check pin compatibility — the new master must carry the same number of
    /// pins with exactly the same names, else the swap is refused with `false`. A resize
    /// therefore cannot silently strand connections.
    ///
    /// It does **not** check *logical* equivalence: same pins is not same function. Picking a
    /// replacement that actually computes the same thing is the caller's job, which is why a
    /// planner needs library equivalence classes before it can drive this move.
    ///
    /// The swap is journaled, so it rolls back with [`eco_undo`](Self::eco_undo).
    pub fn swap_master(&mut self, inst: &str, master: &str) -> Result<bool> {
        Ok(sys::swap_master(self.r(), inst, master)?)
    }

    // ---- ECO journal: try an edit, keep it or put it back ----

    /// Start recording block edits so they can be rolled back.
    ///
    /// Pair with [`eco_commit`](Self::eco_commit) or [`eco_undo`](Self::eco_undo). Prefer
    /// [`eco_try`](Self::eco_try) when the decision is local; use these directly when the
    /// verdict needs work in between — re-timing, for instance — which is the whole point of
    /// the mechanism.
    ///
    /// Only edits OpenDB journals can be undone: object create/delete, connect/disconnect,
    /// swaps and field updates. Anything outside the database (a file you wrote, state you
    /// cached) is *not* rolled back.
    pub fn eco_begin(&mut self) -> Result<()> { Ok(sys::eco_begin(self.r())?) }

    /// Keep the changes recorded since [`eco_begin`](Self::eco_begin).
    pub fn eco_commit(&mut self) -> Result<()> { Ok(sys::eco_commit(self.r())?) }

    /// Roll back the changes recorded since [`eco_begin`](Self::eco_begin).
    pub fn eco_undo(&mut self) -> Result<()> { Ok(sys::eco_undo(self.r())?) }

    /// Whether the current ECO has recorded nothing — i.e. the attempt was a no-op.
    pub fn eco_is_empty(&self) -> Result<bool> { Ok(sys::eco_empty(self.r())?) }

    /// Apply `f` speculatively: **keep** its edits if it returns `Ok(true)`, **roll them back**
    /// if it returns `Ok(false)` — or if it fails.
    ///
    /// The error case is the one that matters. A fix that half-applies and then errors would
    /// leave the design in a state neither the caller nor the timer can reason about, so the
    /// rollback happens on the way out regardless. The original error is returned; if the
    /// rollback *itself* fails, that error is returned instead, because at that point the
    /// database state is the more urgent problem.
    ///
    /// Returns whether the edits were kept.
    pub fn eco_try(&mut self, f: impl FnOnce(&mut Db) -> Result<bool>) -> Result<bool> {
        self.eco_begin()?;
        let verdict = f(self);
        match verdict {
            Ok(true) => {
                self.eco_commit()?;
                Ok(true)
            }
            Ok(false) => {
                self.eco_undo()?;
                Ok(false)
            }
            Err(e) => {
                // roll back first, and let a rollback failure win — a corrupt database is worse
                // news than whatever `f` was complaining about
                self.eco_undo()?;
                Err(e)
            }
        }
    }

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
    pub fn with_captured_logs<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> (T, String) {
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

// Hand-written compositions over the generated setters, where calling them individually has a
// trap the generator cannot know about.
#[cfg(all(unix, feature = "gen-write"))]
impl Db {
    /// Place a chip instance in the 3D stack: set its orientation, then its location.
    ///
    /// **Use this rather than calling `chipinst_set_orient` and `chipinst_set_loc` yourself.**
    /// The two are coupled and order-dependent. `dbChipInst::setLoc` does not store the point it
    /// is given — it orients the master chip's cuboid and stores the *delta* that lands that
    /// cuboid's lower-left-lower corner on the requested point, and the location you read back is
    /// derived by re-applying the **current** orientation. So placing first and re-orienting
    /// afterwards silently moves the chip, with no error and no warning.
    ///
    /// Calling them in this order is the whole point of this method: after it returns,
    /// `chipinst_get_loc_{x,y,z}` reads back exactly the `(x, y, z)` passed in.
    ///
    /// `orient` is a `dbOrientType3D` string — a 2D orientation with an optional `MZ_` prefix
    /// for the Z mirror, e.g. `R0`, `R90`, `MZ` , `MZ_R90`.
    pub fn place_chip_inst(
        &mut self,
        chip: &str,
        inst: &str,
        orient: &str,
        x: i32,
        y: i32,
        z: i32,
    ) -> Result<()> {
        self.chipinst_set_orient(chip, inst, orient)?;
        self.chipinst_set_loc(chip, inst, x, y, z)
    }
}
