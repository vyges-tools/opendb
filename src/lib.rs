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
/// The OpenROAD commit the underlying `libodb` is built from, re-exported so every engine can
/// report the pin it is actually running without carrying a SHA of its own.
///
/// 🔑 **This is the inheritance point.** Each engine already depends on this crate, so the pin
/// arrives with no new plumbing: `openroad-pin.yaml` → `opendb-lib` → here → the engine's
/// `--describe` and its report. See [`vyges_opendb_lib::OPENROAD_PIN`] for what it can and cannot
/// tell you — it names the pin a binary was BUILT against, which is exactly what a harness needs
/// to compare against the oracle it is about to launch.
pub use vyges_opendb_lib::OPENROAD_PIN;

pub mod blox;
#[cfg(unix)]
pub mod d2d;
#[cfg(unix)]
pub mod diodes;
pub mod eco;
#[cfg(unix)]
pub mod nets3d;
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

/// One routed shape: a metal rectangle on a layer, or a via joining two layers.
///
/// Layers are odb layer *numbers*; resolve with [`Db::layer_name_by_number`]. Vias carry the
/// pair they join rather than a layer of their own — that pair is what makes the routing a
/// three-dimensional graph instead of a stack of unrelated per-layer pictures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WireShape {
    /// Routing layer number, or -1 for a via.
    pub layer: i64,
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
    pub is_via: bool,
    /// Via's lower layer number, -1 when not a via.
    pub via_bottom: i64,
    /// Via's upper layer number, -1 when not a via.
    pub via_top: i64,
}

impl WireShape {
    /// Does this shape touch or overlap `other` in the plane? Abutment counts: two segments
    /// that merely share an edge are electrically one piece of metal.
    pub fn touches(&self, other: &WireShape) -> bool {
        self.x0 <= other.x1 && other.x0 <= self.x1 && self.y0 <= other.y1 && other.y0 <= self.y1
    }

    /// Does this shape cover the point? Used to anchor a pin to the metal it connects to.
    pub fn contains(&self, x: i32, y: i32) -> bool {
        self.x0 <= x && x <= self.x1 && self.y0 <= y && y <= self.y1
    }
}

/// One box of routed metal or cut, on a named layer.
///
/// Unlike [`WireShape`] a via is not one object here — it has already been decomposed into the
/// boxes it actually occupies, each on its own layer. `is_routing` separates metal (where the
/// area/side ratios apply) from cut layers (which carry their own).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayerBox {
    pub layer: i64,
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
    pub is_routing: bool,
    /// True when this box came from a via's decomposition rather than a wire segment. Behaves
    /// like wire metal; kept for diagnosis.
    pub from_via: bool,
}

impl LayerBox {
    /// Do the two boxes touch or overlap in the plane? Abutment counts.
    pub fn touches(&self, o: &LayerBox) -> bool {
        self.x0 <= o.x1 && o.x0 <= self.x1 && self.y0 <= o.y1 && o.y0 <= self.y1
    }
}

/// Which diffusion-dependent antenna limit curve to read.
///
/// An enum rather than a string so a caller cannot name a curve the database layer does not
/// know: an unrecognised selector would come back as zero points, which is indistinguishable
/// from "this layer states no such limit" — a silent pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffCurve {
    /// `ANTENNADIFFAREARATIO` — partial area.
    Par,
    /// `ANTENNACUMDIFFAREARATIO` — cumulative area.
    Car,
    /// `ANTENNADIFFSIDEAREARATIO` — partial side area. The only curve sky130 states.
    Psr,
    /// `ANTENNACUMDIFFSIDEAREARATIO` — cumulative side area.
    Csr,
    /// `ANTENNAAREADIFFREDUCEPWL` — area reduction as a function of diffusion.
    AreaDiffReduce,
    /// `ANTENNAGATEPLUSDIFF` in PWL form.
    GatePlusDiff,
}

impl DiffCurve {
    pub fn as_str(self) -> &'static str {
        match self {
            DiffCurve::Par => "par",
            DiffCurve::Car => "car",
            DiffCurve::Psr => "psr",
            DiffCurve::Csr => "csr",
            DiffCurve::AreaDiffReduce => "area_diff_reduce",
            DiffCurve::GatePlusDiff => "gate_plus_diff",
        }
    }
}

/// An OpenDB design database (owns a `dbDatabase` + its logger). Unix-only.
#[cfg(unix)]
pub struct Db {
    inner: UniquePtr<sys::OdbDb>,
}

#[cfg(unix)]
/// The lattice that top-layer pins are placed on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopLayerGrid {
    pub layer: String,
    pub x_step: i32,
    pub y_step: i32,
    /// Pin size; a position is legal only if a pin this size fits there.
    pub pin_width: i32,
    pub pin_height: i32,
    /// Clearance a pin must keep from any obstruction.
    pub keepout: i32,
    pub region: (i32, i32, i32, i32),
    /// ⚠️ False means `region` is only an enclosing box. The reference does not handle a
    /// non-rectangular grid either, so this must be checked rather than assumed.
    pub region_is_rect: bool,
}

/// Regroup a flat `(layer, x0, y0, x1, y1)` stream into tuples.
fn chunk5(v: Vec<i32>) -> Vec<(i64, i32, i32, i32, i32)> {
    v.chunks(5)
        .filter(|c| c.len() == 5)
        .map(|c| (c[0] as i64, c[1], c[2], c[3], c[4]))
        .collect()
}

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
    /// ⛔ **NOT TRANSACTIONAL** — geometry is not journaled; see [`eco_try`](Self::eco_try).
    /// Set a block terminal's direction — `INPUT` / `OUTPUT` / `INOUT` / `FEEDTHRU`.
    ///
    /// ⛔ **Hand-written, so the Verilog handoff does not require `gen-write`.** It was otherwise
    /// generated into that gated surface, and `vyges-opendb` is the shipped binary.
    /// 🔑 Goes through `dbBTerm::setIoType`, which **journals** the change
    /// (`dbBTerm.cpp:326`) — writing the flag directly would leave an ECO rollback blind.
    pub fn bterm_set_io_type(&mut self, bterm: &str, io_type: &str) -> Result<()> {
        Ok(sys::bterm_set_io_type(self.r(), bterm, io_type)?)
    }

    /// Set the block's DEF output units, as `Verilog2db::makeBlock` does with `tech->getLefUnits()`.
    ///
    /// ⚠️ **This is NOT the database scale.** `dbu_per_micron` comes from the TECH when the block
    /// is created (`dbBlock.cpp:2953`), so micron conversions work without this. `def_units_`
    /// defaults to **100**, is serialized, and is compared in block equality — so a DEF written
    /// without it says 100 where sky130 should say 1000.
    pub fn block_set_def_units(&mut self, units: i32) -> Result<()> {
        Ok(sys::block_set_def_units(self.r(), units)?)
    }

    /// Set the block's bus delimiters, as `Verilog2db::makeBlock` does (`dbReadVerilog.cc:266`).
    ///
    /// ⛔ **Not optional for a design built from Verilog.** Without it bus names do not round-trip:
    /// `a[3]` written back is not the name anything else looks up. Upstream sets `'['` and `']'`
    /// immediately after creating the block, alongside `set_def_units`.
    pub fn set_bus_delimiters(&mut self, left: char, right: char) -> Result<()> {
        Ok(sys::block_set_bus_delimiters(self.r(), &left.to_string(), &right.to_string())?)
    }

    /// Read a LEF. **The first one read creates the tech**; later ones add libraries to it.
    ///
    /// 🔑 Transcribes `read_lef`'s own rule (`OpenRoad.tcl:7`): with neither `-tech` nor `-library`
    /// given, a library is always made and a tech only when the database has none. Names come from
    /// the file's rootname, as the Tcl does.
    ///
    /// ⛔ **This is what lets a design be built without OpenROAD at all** — `read_def` needs a tech
    /// and libs, and nothing on our side could create them before.
    pub fn read_lef(&mut self, lef_path: impl AsRef<Path>) -> Result<()> {
        Ok(sys::read_lef(self.r(), &path_str(lef_path)?)?)
    }

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

    // ---- antenna ratio inputs (routed substrate) ----------------------------
    // Numerator and denominator for the antenna check in `vyges-ant`. Read off the ROUTED
    // database — the substrate where `RepairAntennas` can still act — not off a GDS.
    //
    // Areas are DBU²; the LEF states gate area in µm², so a caller computing a ratio must
    // divide by `dbu_per_micron()²` first. Mixing them silently is a ~10⁶ error on sky130.

    /// How many routing layers this net has metal on (0 if unrouted or unknown).
    pub fn num_net_wire_layers(&self, net: &str) -> usize { sys::num_net_wire_layers(self.r(), net) }
    /// Name of the net's `i`-th routing layer (empty past the end). Order is stable.
    pub fn nth_net_wire_layer(&self, net: &str, i: usize) -> String { sys::nth_net_wire_layer(self.r(), net, i) }
    /// Metal area of `net` on `layer`, in DBU². Zero if the net has no metal there.
    ///
    /// v0 bound: overlapping shapes on one layer are double-counted (raw rectangle sum, not a
    /// union). Conservative — it over-reports area, hence the ratio, hence never hides a
    /// violation — but it is a real difference from a union-area computation.
    pub fn net_wire_area_on_layer(&self, net: &str, layer: &str) -> i64 { sys::net_wire_area_on_layer(self.r(), net, layer) }
    /// Metal perimeter of `net` on `layer`, in DBU. Side area = this × [`Db::layer_thickness`].
    pub fn net_wire_perimeter_on_layer(&self, net: &str, layer: &str) -> i64 { sys::net_wire_perimeter_on_layer(self.r(), net, layer) }
    /// Layer thickness in DBU from the LEF, or 0 when the LEF states none — which a caller
    /// must not read as a zero-thickness layer. Without it, side-area ratios are unavailable.
    pub fn layer_thickness(&self, layer: &str) -> i32 { sys::layer_thickness(self.r(), layer) }
    /// Gate area (µm²) from the pin's antenna model — the ratio's denominator. Zero when the
    /// pin has no model, which means *not applicable*, not "a gate of zero area".
    pub fn mterm_antenna_gate_area(&self, master: &str, term: &str) -> f64 { sys::mterm_antenna_gate_area(self.r(), master, term) }
    /// Diffusion area (µm²) on the pin — the index into the diff-ratio PWL curves.
    ///
    /// Note the asymmetry in odb: gate area lives on the pin's antenna *model*, diffusion area
    /// directly on the `dbMTerm`. A pin can therefore carry a diffusion area while having no
    /// antenna model at all.
    pub fn mterm_antenna_diff_area(&self, master: &str, term: &str) -> f64 { sys::mterm_antenna_diff_area(self.r(), master, term) }

    /// Every routed shape on `net`, with the connectivity needed to walk it as a graph.
    ///
    /// The per-layer area accessors answer "how much metal does this net have on this layer",
    /// which is the wrong question for an antenna check: the charge a gate collects comes only
    /// from the metal *reachable from that gate* over layers at or below the one being
    /// deposited. Two gates on one net can sit on different branches and see very different
    /// metal until a higher layer joins them.
    ///
    /// One call per net rather than per shape — a per-shape accessor re-walks the wire on every
    /// query, which is quadratic exactly on the big nets that matter.
    pub fn net_wire_shapes(&self, net: &str) -> Vec<WireShape> {
        sys::net_wire_shapes(self.r(), net)
            .chunks_exact(8)
            .map(|c| WireShape {
                layer: c[0],
                x0: c[1] as i32,
                y0: c[2] as i32,
                x1: c[3] as i32,
                y1: c[4] as i32,
                is_via: c[5] != 0,
                via_bottom: c[6],
                via_top: c[7],
            })
            .collect()
    }

    /// Every routed box of a net, with vias **decomposed** onto the layers they occupy.
    ///
    /// [`Db::net_wire_shapes`] reports a via as one bounding box tagged with the pair it joins,
    /// which loses what matters here: a via is a cut plus an enclosure on the layer below and
    /// another above. On a net routed at met1 and up, the via enclosure is the *only* metal on
    /// li1 — where the standard-cell pins are. Without this there is no geometry on the layer
    /// the pins live on.
    pub fn net_wire_boxes(&self, net: &str) -> Vec<LayerBox> {
        sys::net_wire_boxes(self.r(), net)
            .chunks_exact(7)
            .map(|c| LayerBox {
                layer: c[0],
                x0: c[1] as i32,
                y0: c[2] as i32,
                x1: c[3] as i32,
                y1: c[4] as i32,
                is_routing: c[5] != 0,
                from_via: c[6] != 0,
            })
            .collect()
    }

    /// Layer name for an odb layer number (empty when unknown).
    pub fn layer_name_by_number(&self, number: i64) -> String {
        sys::layer_name_by_number(self.r(), number)
    }

    /// Where an instance pin sits, for anchoring a gate to the shape graph.
    ///
    /// `None` when odb cannot place the pin — which a caller must treat as "cannot attribute",
    /// never as the origin. Silently anchoring an unplaced pin at (0,0) would attach it to
    /// whatever happens to be routed near the die corner.
    pub fn iterm_avg_xy(&self, inst: &str, pin: &str) -> Option<(i32, i32)> {
        let (mut x, mut y) = (0i32, 0i32);
        sys::iterm_avg_xy(self.r(), inst, pin, &mut x, &mut y).then_some((x, y))
    }

    /// The pin's own metal, in placed coordinates — every ROUTING-layer box of the terminal
    /// with the instance's transform applied.
    ///
    /// [`Db::iterm_avg_xy`] says where a pin roughly is; this says what it physically touches,
    /// which is what decides the conductor it joins. Matching by proximity instead merges
    /// conductors that are electrically separate.
    pub fn iterm_pin_boxes(&self, inst: &str, pin: &str) -> Vec<WireShape> {
        sys::iterm_pin_boxes(self.r(), inst, pin)
            .chunks_exact(5)
            .map(|c| WireShape {
                layer: c[0],
                x0: c[1] as i32,
                y0: c[2] as i32,
                x1: c[3] as i32,
                y1: c[4] as i32,
                is_via: false,
                via_bottom: -1,
                via_top: -1,
            })
            .collect()
    }

    /// A diffusion-dependent antenna limit curve.
    ///
    /// LEF states antenna limits either as plain ratios or as these PWL curves, where the limit
    /// is a function of the diffusion area connected to the net. Some technologies — sky130
    /// among them — state *only* the PWL form, so a checker that reads only the plain ratios
    /// finds no limits at all there.
    pub fn layerantenna_diff_pwl(&self, layer: &str, which: DiffCurve) -> Vec<(f64, f64)> {
        // The selector comes from the enum, never from a caller's string, so the underlying
        // "unknown curve" error is unreachable by construction. It exists so that a typo in
        // the FFI layer is loud rather than reading as "this layer states no limit".
        let n = sys::layerantenna_num_diff_pwl(self.r(), layer, which.as_str())
            .expect("DiffCurve always names a curve the shim knows");
        (0..n)
            .map(|i| {
                (
                    sys::layerantenna_diff_pwl_index(self.r(), layer, which.as_str(), i).unwrap_or(0.0),
                    sys::layerantenna_diff_pwl_ratio(self.r(), layer, which.as_str(), i).unwrap_or(0.0),
                )
            })
            .collect()
    }

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
    /// ⛔ **NOT TRANSACTIONAL** — geometry is not journaled; see [`eco_try`](Self::eco_try).
    pub fn create_chip(&mut self, name: &str, tech: &str, chip_type: &str) -> Result<()> {
        Ok(sys::chip_create(self.r(), name, tech, chip_type)?)
    }

    /// Give a chip its own `dbBlock` — the die's design. The top chip needs one for the
    /// block-level accessors to resolve through it; a die needs one to hold the instances its
    /// bumps wrap.
    /// ⛔ **NOT TRANSACTIONAL** — geometry is not journaled; see [`eco_try`](Self::eco_try).
    pub fn create_chip_block(&mut self, chip: &str, name: &str) -> Result<()> {
        Ok(sys::chip_block_create(self.r(), chip, name)?)
    }

    /// Instantiate `master_chip` inside `parent_chip`. See the ordering note above: the
    /// master's regions and bumps must already exist.
    /// ⛔ **NOT TRANSACTIONAL** — geometry is not journaled; see [`eco_try`](Self::eco_try).
    pub fn create_chip_inst(&mut self, parent_chip: &str, master_chip: &str, name: &str) -> Result<()> {
        Ok(sys::chip_inst_create(self.r(), parent_chip, master_chip, name)?)
    }

    /// Create a bonding region on a chip. `side` is `FRONT` | `BACK` | `INTERNAL` |
    /// `INTERNAL_EXT`; `layer` is a tech-layer name, or empty for none.
    /// ⛔ **NOT TRANSACTIONAL** — geometry is not journaled; see [`eco_try`](Self::eco_try).
    pub fn create_chip_region(&mut self, chip: &str, name: &str, side: &str, layer: &str) -> Result<()> {
        Ok(sys::chip_region_create(self.r(), chip, name, side, layer)?)
    }

    /// Set a region's footprint. Without it the region has no extent, and the checks that
    /// reason about footprints have nothing to test.
    /// ⛔ **NOT TRANSACTIONAL** — geometry is not journaled; see [`eco_try`](Self::eco_try).
    pub fn set_chip_region_box(&mut self, chip: &str, region: &str, x1: i32, y1: i32, x2: i32, y2: i32) -> Result<()> {
        Ok(sys::chip_region_set_box(self.r(), chip, region, x1, y1, x2, y2)?)
    }

    /// Place a bump on a region, wrapping an instance in that chip's own block.
    /// ⛔ **NOT TRANSACTIONAL** — geometry is not journaled; see [`eco_try`](Self::eco_try).
    pub fn create_chip_bump(&mut self, chip: &str, region: &str, inst: &str) -> Result<()> {
        Ok(sys::chip_bump_create(self.r(), chip, region, inst)?)
    }

    /// Bond two region instances together with a physical thickness.
    ///
    /// odb takes a *path* of chip instances on each side, to name a region inside a nested
    /// assembly. This binds the direct case — one hop per side — which covers a bond between
    /// two chips in the same parent. Deeper paths are expressible upstream and not here yet.
    /// ⛔ **NOT TRANSACTIONAL** — geometry is not journaled; see [`eco_try`](Self::eco_try).
    #[allow(clippy::too_many_arguments)]
    pub fn create_chip_conn(&mut self, name: &str, parent_chip: &str, top_inst: &str, top_region: &str,
                            bottom_inst: &str, bottom_region: &str, thickness: i32) -> Result<()> {
        Ok(sys::chip_conn_create(self.r(), name, parent_chip, top_inst, top_region, bottom_inst, bottom_region, thickness)?)
    }

    /// Create a logical net spanning bump instances across chips.
    /// ⛔ **NOT TRANSACTIONAL** — geometry is not journaled; see [`eco_try`](Self::eco_try).
    pub fn create_chip_net(&mut self, chip: &str, name: &str) -> Result<()> {
        Ok(sys::chip_net_create(self.r(), chip, name)?)
    }

    /// Create a named traversal route through the assembly.
    /// ⛔ **NOT TRANSACTIONAL** — geometry is not journaled; see [`eco_try`](Self::eco_try).
    pub fn create_chip_path(&mut self, chip: &str, name: &str) -> Result<()> {
        Ok(sys::chip_path_create(self.r(), chip, name)?)
    }

    /// Associate a bump instance with a logical 3D net.
    ///
    /// The bump is addressed by its position in `(chip_inst, region)`. Needed because the
    /// logical-connectivity check compares the nets of physically aligned bump pairs — without
    /// any net associations it has nothing to disagree about and passes on any design.
    /// ⛔ **NOT TRANSACTIONAL** — geometry is not journaled; see [`eco_try`](Self::eco_try).
    pub fn add_chip_net_bump(&mut self, chip: &str, net: &str, chip_inst: &str, region: &str, bump_index: usize) -> Result<()> {
        Ok(sys::chip_net_add_bump(self.r(), chip, net, chip_inst, region, bump_index)?)
    }

    /// Declare an alignment-marker rule between two masters, with a tolerance in DBU.
    ///
    /// The alignment-marker check returns immediately when no rule exists, so a design without
    /// one is not so much clean as unexamined.
    /// ⛔ **NOT TRANSACTIONAL** — geometry is not journaled; see [`eco_try`](Self::eco_try).
    pub fn create_alignment_marker_rule(&mut self, master_a: &str, master_b: &str, tolerance: i32) -> Result<()> {
        Ok(sys::alignment_marker_rule_create(self.r(), master_a, master_b, tolerance)?)
    }

    /// Root the assembly at `chip`.
    ///
    /// **Required before any unfolded query or lint.** The unfolded builder starts from the top
    /// chip and walks its chip instances, so with it left pointing at a flat design every
    /// unfolded table reads empty — and nothing says why.
    /// ⛔ **NOT TRANSACTIONAL** — geometry is not journaled; see [`eco_try`](Self::eco_try).
    pub fn set_top_chip(&mut self, chip: &str) -> Result<()> {
        Ok(sys::set_top_chip(self.r(), chip)?)
    }

    /// Build a named technology from a LEF file.
    ///
    /// This is how a chiplet gets its **own** technology, from the `APR_tech_file` its `.3dbv`
    /// names — the mechanism that lets dies from different processes share one database.
    /// ⛔ **NOT TRANSACTIONAL** — geometry is not journaled; see [`eco_try`](Self::eco_try).
    pub fn tech_from_lef(&mut self, name: &str, lef_path: &str) -> Result<()> {
        Ok(sys::tech_from_lef(self.r(), name, lef_path)?)
    }

    /// Create the database's technology, carrying only the precision already set.
    ///
    /// odb refuses to create a `DIE` chip without a technology; a geometry-only 3Dblox read has
    /// no LEF to build a real one from, so this is the placeholder that lets the model exist.
    /// ⛔ **NOT TRANSACTIONAL** — geometry is not journaled; see [`eco_try`](Self::eco_try).
    pub fn create_tech(&mut self, name: &str) -> Result<()> {
        Ok(sys::tech_create(self.r(), name)?)
    }

    /// Database precision, in DBU per micron (0 when unset).
    pub fn dbu_per_micron(&self) -> i32 { sys::dbu_per_micron(self.r()) }

    /// Set the database precision. A 3Dblox header declares the precision its micron
    /// coordinates are written at, and reading one has to reconcile the two.
    /// ⛔ **NOT TRANSACTIONAL** — geometry is not journaled; see [`eco_try`](Self::eco_try).
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
    /// ⛔ **NOT TRANSACTIONAL** — geometry is not journaled; see [`eco_try`](Self::eco_try).
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
    /// 🔒 **Transactional** — rolled back by [`eco_try`](Self::eco_try).
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
    /// # ⚠️ What rolls back, and what does not
    ///
    /// **The netlist rolls back. The geometry does not.**
    ///
    /// | | |
    /// | --- | --- |
    /// | 🔒 **Journaled** | instances (create, destroy, move, reorient, master swap), nets, terminals and their connectivity, the module hierarchy, parasitics, routing guides |
    /// | ⛔ **NOT journaled** | blockages, obstructions, fills, rows, special wires, wires, vias, regions — and anything outside the database, such as a file you wrote or state you cached |
    ///
    /// Every mutating method on this type is marked with which of the two it is. **Those markers
    /// are established by measurement**, not by inspection — see `tests/transactional.rs` in
    /// `vyges-opendb-lib`, which performs each edit inside an ECO, rolls it back, and asserts
    /// whether it returned.
    ///
    /// ⟹ **An engine that writes geometry and wants transaction semantics must undo that
    /// geometry itself.** [`truncate_blockages`](Self::truncate_blockages) is the pattern:
    /// record a baseline count before the edits, and destroy back down to it afterwards.
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
    /// ⚠️ **Rolls back the netlist only.** Geometry written inside `f` — blockages, fills, rows,
    /// obstructions, special wires — SURVIVES. See [`eco_begin`](Self::eco_begin) for the full
    /// split and for how to undo geometry by hand.
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
    /// 🔒 **Transactional** — rolled back by [`eco_try`](Self::eco_try).
    pub fn create_net(&mut self, name: &str) -> Result<()> {
        Ok(sys::create_net(self.r(), name)?)
    }
    /// Read the cell masters from a LEF into a named library.
    ///
    /// [`Db::tech_from_lef`] reads a LEF's layers; this reads its cells. A bump map names cell
    /// types, and those masters live in the `LEF_file` a `.3dbv` already points at.
    /// ⛔ **NOT TRANSACTIONAL** — geometry is not journaled; see [`eco_try`](Self::eco_try).
    pub fn lib_from_lef(&mut self, lib: &str, tech: &str, lef_path: &str) -> Result<()> {
        Ok(sys::lib_from_lef(self.r(), lib, tech, lef_path)?)
    }

    /// A placeholder bump master for a cell type no available LEF defines.
    ///
    /// **Pass `0, 0` unless you know the real geometry.** odb reads a bump's position from the
    /// instance bounding-box *centre* (`dbUnfoldedChipBumpInst::getGlobalPosition`) while a bump
    /// map records its *origin* (`BmapWriter`), so at any other size the two disagree by half the
    /// master and a map written out then read back moves.
    /// ⛔ **NOT TRANSACTIONAL** — geometry is not journaled; see [`eco_try`](Self::eco_try).
    pub fn create_bump_master(&mut self, name: &str, width: i32, height: i32) -> Result<()> {
        Ok(sys::bump_master_create(self.r(), name, width, height)?)
    }

    /// `dbITerm::isOutputSignal(io=true)` — OUTPUT **or INOUT**, and not a supply pin.
    pub fn iterm_is_output_signal(&self, inst: &str, pin: &str) -> bool {
        sys::iterm_is_output_signal(self.r(), inst, pin)
    }

    /// `dbBTerm::getFirstPinLocation` — centre of the first PLACED pin box.
    ///
    /// ⚠️ `None` means the port has no placed pin, which is not the same as being at the origin;
    /// the reference distinguishes them and so must any caller.
    pub fn bterm_first_pin_location(&self, bterm: &str) -> Option<(i32, i32)> {
        let (mut x, mut y) = (0, 0);
        sys::bterm_first_pin_location(self.r(), bterm, &mut x, &mut y).then_some((x, y))
    }

    /// Does the LEF state an antenna DIFFUSION area for this pin?
    pub fn mterm_has_diff_area(&self, master: &str, term: &str) -> bool {
        sys::mterm_has_diff_area(self.r(), master, term)
    }

    /// Does the LEF state an antenna GATE area for this pin?
    pub fn mterm_has_gate_area(&self, master: &str, term: &str) -> bool {
        sys::mterm_has_gate_area(self.r(), master, term)
    }

    /// A PAD-class site in the `FAKE_IO` library — `ICeWall::makeFakeSite`.
    ///
    /// For a pad ring whose LEF defines no IO site of its own. The library is found-or-created, so
    /// repeated calls share one `FAKE_IO` rather than accumulating libraries.
    ///
    /// ⚠️ **`width` and `height` are DATABASE UNITS.** Upstream's `make_fake_io_site` takes microns
    /// and converts with `ord::microns_to_dbu` in Tcl; the conversion belongs at the command
    /// boundary, not here.
    /// ⛔ **NOT TRANSACTIONAL** — library and site creation are not journaled; see
    /// [`eco_try`](Self::eco_try).
    pub fn create_fake_site(&mut self, name: &str, width: i32, height: i32) -> Result<()> {
        Ok(sys::fake_site_create(self.r(), name, width, height)?)
    }

    /// 🔒 **Transactional** — rolled back by [`eco_try`](Self::eco_try).
    pub fn create_inst(&mut self, master: &str, name: &str) -> Result<()> {
        Ok(sys::create_inst(self.r(), master, name)?)
    }
    /// 🔒 **Transactional** — rolled back by [`eco_try`](Self::eco_try).
    pub fn set_inst_location(&mut self, inst: &str, x: i32, y: i32) -> Result<()> {
        Ok(sys::set_inst_location(self.r(), inst, x, y)?)
    }
    /// Set an instance's orientation (`R0`/`R90`/`R180`/`R270`/`MX`/`MY`/`MXR90`/`MYR90`).
    /// 🔒 **Transactional** — rolled back by [`eco_try`](Self::eco_try).
    pub fn set_inst_orient(&mut self, inst: &str, orient: &str) -> Result<()> {
        Ok(sys::set_inst_orient(self.r(), inst, orient)?)
    }
    /// Add a routing/PDN obstruction rectangle on `layer` (DBU). Errors if the layer is unknown.
    /// ⛔ **NOT TRANSACTIONAL** — geometry is not journaled; see [`eco_try`](Self::eco_try).
    pub fn add_obstruction(&mut self, layer: &str, x1: i32, y1: i32, x2: i32, y2: i32) -> Result<()> {
        Ok(sys::add_obstruction(self.r(), layer, x1, y1, x2, y2)?)
    }
    /// Number of obstructions currently in the block.
    pub fn num_obstructions(&self) -> usize { sys::num_obstructions(self.r()) }
    /// Destroy all obstructions; returns the count removed.
    /// ⛔ **NOT TRANSACTIONAL** — geometry is not journaled; see [`eco_try`](Self::eco_try).
    pub fn clear_obstructions(&mut self) -> usize { sys::clear_obstructions(self.r()) }

    // ---- floorplan write path (the primitives `vyges-ifp` composes) ----

    /// Set the die area, in DBU.
    /// ⛔ **NOT TRANSACTIONAL** — geometry is not journaled; see [`eco_try`](Self::eco_try).
    pub fn set_die_area(&mut self, x1: i32, y1: i32, x2: i32, y2: i32) -> Result<()> {
        Ok(sys::block_set_die_area(self.r(), x1, y1, x2, y2)?)
    }
    /// Set a **non-rectangular** die outline, as a flat `[x0, y0, x1, y1, ...]` list of vertices.
    ///
    /// The read side ([`die_area_polygon`](Self::die_area_polygon)) already existed; this is the
    /// write side, and there is no generated equivalent because `setDieArea` is overloaded on
    /// `odb::Polygon`. A floorplan whose die is a rectangle should keep using
    /// [`set_die_area`](Self::set_die_area) — this is for the polygon form of the command.
    ///
    /// ⚠️ Fewer than 4 vertices, or an odd number of coordinates, is an error rather than a
    /// stored degenerate outline.
    pub fn set_die_area_polygon(&mut self, xy: &[i32]) -> Result<()> {
        Ok(sys::block_set_die_area_polygon(self.r(), xy)?)
    }
    /// Set the core area, in DBU. Note a floorplan usually wants
    /// [`set_core_area_from_rows`](Self::set_core_area_from_rows) instead: the core a design
    /// really has is the one its rows cover, not the one that was asked for.
    /// ⛔ **NOT TRANSACTIONAL** — geometry is not journaled; see [`eco_try`](Self::eco_try).
    pub fn set_core_area(&mut self, x1: i32, y1: i32, x2: i32, y2: i32) -> Result<()> {
        Ok(sys::block_set_core_area(self.r(), x1, y1, x2, y2)?)
    }
    /// Replace the core area with the extent of the rows — odb's `setCoreArea(computeCoreArea())`.
    /// ⛔ **NOT TRANSACTIONAL** — geometry is not journaled; see [`eco_try`](Self::eco_try).
    pub fn set_core_area_from_rows(&mut self) -> Result<()> {
        Ok(sys::block_set_core_area_from_rows(self.r())?)
    }
    /// The extent the rows cover, as `[x_min, y_min, x_max, y_max]` in DBU, **without** storing
    /// it. Empty when there are no rows, which is an answer rather than a zero rectangle.
    pub fn compute_core_area(&self) -> Result<Vec<i32>> {
        Ok(sys::block_compute_core_area(self.r())?)
    }
    /// The technology's manufacturing grid in DBU, or `None` when it states none.
    ///
    /// odb reports "none" as 0; passing that on as a number would make every coordinate look
    /// already-snapped, so absence is kept distinct from a grid of 1.
    pub fn manufacturing_grid(&self) -> Result<Option<i32>> {
        let g = sys::tech_manufacturing_grid(self.r())?;
        Ok(if g > 0 { Some(g) } else { None })
    }
    /// Create a row of `num_sites` copies of `site` at `spacing`, starting at (`x`, `y`) in DBU.
    /// `orient` is an odb orientation (`R0`, `MX`, …) and `direction` is `HORIZONTAL`/`VERTICAL`.
    /// An unknown site is an error, not a skipped row.
    /// ⛔ **NOT TRANSACTIONAL** — geometry is not journaled; see [`eco_try`](Self::eco_try).
    #[allow(clippy::too_many_arguments)]
    pub fn create_row(&mut self, name: &str, site: &str, x: i32, y: i32, orient: &str,
                      direction: &str, num_sites: i32, spacing: i32) -> Result<()> {
        Ok(sys::row_create(self.r(), name, site, x, y, orient, direction, num_sites, spacing)?)
    }
    /// Number of rows in the block.
    pub fn num_rows(&self) -> Result<usize> { Ok(sys::num_rows(self.r())?) }
    /// Destroy all rows; returns the count removed. A floorplan is rebuilt, not appended to.
    /// ⛔ **NOT TRANSACTIONAL** — geometry is not journaled; see [`eco_try`](Self::eco_try).
    pub fn clear_rows(&mut self) -> Result<usize> { Ok(sys::clear_rows(self.r())?) }
    /// Number of sites across every library the database has loaded.
    pub fn num_sites(&self) -> Result<usize> { Ok(sys::num_sites(self.r())?) }
    /// Name of the `i`th site, in library order.
    pub fn nth_site_name(&self, i: usize) -> Result<String> { Ok(sys::nth_site_name(self.r(), i)?) }
    /// Every site name the loaded libraries define — what to offer when a caller names one that
    /// does not exist.
    pub fn site_names(&self) -> Result<Vec<String>> {
        (0..self.num_sites()?).map(|i| self.nth_site_name(i)).collect()
    }
    /// Cut the rows around placed macros — odb's own `cutRows`, not a reimplementation.
    ///
    /// `blockage_insts` names the instances to cut around; choosing them (and reporting the ones
    /// skipped) is the caller's job, because that is engine policy rather than database
    /// mechanics. An unknown instance name is an error.
    ///
    /// `min_row_height` gates odb's narrow-region cutting: **0 disables it**, which is what
    /// upstream's own `ifp` callers pass. A caller wanting rows too short for an endcap removed
    /// asks for it explicitly.
    pub fn cut_rows(
        &mut self,
        min_row_width: i32,
        min_row_height: i32,
        blockage_insts: &[String],
        halo_x: i32,
        halo_y: i32,
    ) -> Result<()> {
        Ok(sys::block_cut_rows(self.r(), min_row_width, min_row_height, blockage_insts,
                               halo_x, halo_y)?)
    }
    /// Cut the rows against the block's **own blockages** — odb's `cutRows` again, over the input
    /// `ifp` gives it.
    ///
    /// [`Self::cut_rows`] cuts around instances the caller names, because `tap` picks its
    /// blockages from placed macros. `ifp` picks nothing: `makeRows` ends by passing every
    /// `dbBlockage` in the block, unconditionally, and `cutRows` returns at once when there are
    /// none — so on a design that declares no blockage this is a no-op.
    ///
    /// `min_row_height` gates odb's narrow-region cutting; **0 disables it**, and 0 is what
    /// upstream's own `ifp` passes, as are both halos.
    pub fn cut_rows_at_blockages(
        &mut self,
        min_row_width: i32,
        min_row_height: i32,
        halo_x: i32,
        halo_y: i32,
    ) -> Result<()> {
        Ok(sys::block_cut_rows_at_blockages(self.r(), min_row_width, min_row_height,
                                            halo_x, halo_y)?)
    }
    /// A group's type: `PHYSICAL_CLUSTER`, `VOLTAGE_DOMAIN`, `POWER_DOMAIN` or `VISUAL_DEBUG`.
    ///
    /// ⛔ **There is no proxy for this.** `ifp`'s `updateVoltageDomain` rebuilds the rows crossing
    /// a VOLTAGE_DOMAIN or POWER_DOMAIN group and leaves every other group alone; a physical
    /// cluster may carry a region and a name just the same, so guessing from those holds only on
    /// the designs that happen to be at hand. An unknown group name is an error rather than an
    /// empty string, which would read as a type.
    pub fn group_get_type(&self, group: &str) -> Result<String> {
        Ok(sys::group_get_type(self.r(), group)?)
    }
    /// Does the technology have a single-site-width master? Decides whether tapcell placement
    /// may leave one-site gaps.
    pub fn has_one_site_master(&self) -> bool {
        sys::has_one_site_master(self.r())
    }
    /// Number of masters across every loaded library.
    pub fn num_masters(&self) -> Result<usize> {
        Ok(sys::num_masters(self.r())?)
    }
    /// Name of the `i`th master. Empty when out of range.
    pub fn nth_master_name(&self, i: usize) -> Result<String> {
        Ok(sys::nth_master_name(self.r(), i)?)
    }
    /// Every master name, with its LEF class string (`CORE`, `ENDCAP`,
    /// `ENDCAP_LEF58_LEFTBOTTOMCORNER`, …).
    ///
    /// The type is what answers "which cell is the bottom-left endcap?" — a name substring
    /// cannot, and a library need not name its cells helpfully.
    pub fn masters_with_types(&self) -> Result<Vec<(String, String)>> {
        (0..self.num_masters()?)
            .map(|i| {
                let n = self.nth_master_name(i)?;
                let t = self.master_get_type(&n)?;
                Ok((n, t))
            })
            .collect()
    }
    /// A master's LEF class string. Empty when the master is unknown.
    pub fn master_get_type(&self, master: &str) -> Result<String> {
        Ok(sys::master_get_type(self.r(), master)?)
    }
    /// An instance's bounding box in placed coordinates, `[x_min, y_min, x_max, y_max]`.
    /// Empty when the instance is unknown. Reflects orientation, which origin + master size does
    /// not.
    pub fn inst_bbox(&self, inst: &str) -> Result<Vec<i32>> {
        Ok(sys::inst_bbox(self.r(), inst)?)
    }
    /// Create a **physical-only** instance: a cell in the layout but not the netlist, which is
    /// what every tap, endcap and filler is. Use this rather than [`create_inst`](Self::create_inst)
    /// for anything a placer inserts, or the cell lands in the hierarchy.
    /// ⛔ **NOT TRANSACTIONAL** — geometry is not journaled; see [`eco_try`](Self::eco_try).
    pub fn create_physical_inst(&mut self, master: &str, name: &str) -> Result<()> {
        Ok(sys::create_physical_inst(self.r(), master, name)?)
    }
    /// Routing track coordinates on a layer: `(x tracks, y tracks)`.
    ///
    /// Every legal pin slot sits on a track, so this is the foundation of pin placement. A layer
    /// with no track grid — a cut layer, say — reports empty vectors, which is an answer rather
    /// than an error.
    /// Add an X track pattern to `layer`'s grid, creating the grid if it has none.
    ///
    /// 🔑 **Mirrors `dbTrackGrid::addGridPatternX`, and the find-or-create of
    /// `InitFloorplan::makeTracks`** — repeated calls for one layer ADD patterns to the same grid.
    /// ⚠️ `origin` and `step` are DATABASE UNITS, not microns.
    pub fn add_track_pattern_x(&mut self, layer: &str, origin: i32, count: i32, step: i32)
        -> Result<()> {
        Ok(sys::add_track_pattern_x(self.r(), layer, origin, count, step)?)
    }

    /// Add a Y track pattern to `layer`'s grid, creating the grid if it has none.
    /// ⚠️ `origin` and `step` are DATABASE UNITS, not microns.
    pub fn add_track_pattern_y(&mut self, layer: &str, origin: i32, count: i32, step: i32)
        -> Result<()> {
        Ok(sys::add_track_pattern_y(self.r(), layer, origin, count, step)?)
    }

    pub fn track_grid(&self, layer: &str) -> Result<(Vec<i32>, Vec<i32>)> {
        Ok((sys::track_grid_x(self.r(), layer)?, sys::track_grid_y(self.r(), layer)?))
    }
    /// Track **patterns** on a layer as `(origin, count, step)`, x and y.
    ///
    /// [`track_grid`](Self::track_grid) answers "where are the tracks"; this is the grid itself.
    /// Pin placement indexes tracks by number from a pattern's origin, and a layer may carry
    /// several patterns with different pitches.
    pub fn track_patterns(&self, layer: &str) -> Result<(Vec<(i32, i32, i32)>, Vec<(i32, i32, i32)>)> {
        let trip = |v: Vec<i32>| -> Vec<(i32, i32, i32)> {
            v.chunks(3).filter(|c| c.len() == 3).map(|c| (c[0], c[1], c[2])).collect()
        };
        Ok((
            trip(sys::track_patterns_x(self.r(), layer)?),
            trip(sys::track_patterns_y(self.r(), layer)?),
        ))
    }
    /// A **master's own** shapes, in master coordinates: `(layer number, x0, y0, x1, y1)`.
    ///
    /// Master coordinates, not placed ones — a placer asks "would this cell fit here" about a cell
    /// it has not put anywhere yet, so it needs the shapes before any transform.
    ///
    /// Obstructions and pin shapes are separate because they are treated differently: an
    /// obstruction on an OVERLAP-type layer is the cell's true **outline**, while pin shapes take
    /// part in the per-layer clearance check.
    pub fn master_obstruction_boxes(&self, master: &str) -> Result<Vec<(i64, i32, i32, i32, i32)>> {
        Ok(chunk5(sys::master_obstruction_boxes(self.r(), master)?))
    }

    /// A master's pin shapes, in master coordinates: `(layer number, x0, y0, x1, y1)`.
    pub fn master_pin_boxes(&self, master: &str) -> Result<Vec<(i64, i32, i32, i32, i32)>> {
        Ok(chunk5(sys::master_pin_boxes(self.r(), master)?))
    }

    /// How many MPins one terminal of a master has.
    pub fn num_mpins(&self, master: &str, term: &str) -> usize {
        self.num_mterm_get_m_pins(master, term)
    }

    /// Pin rectangles of **one MPin** of one terminal: `(layer number, x0, y0, x1, y1)` in master
    /// coordinates.
    ///
    /// 🔑 [`Self::mterm_pin_boxes`] flattens every MPin of the terminal into one list, which loses
    /// the grouping. A rule that asks *which layer does this pin sit on* reads the answer from the
    /// **MPin's first box**, not from the box under examination — so on a terminal with two MPins
    /// on different layers, the flattened form answers a different question and never says so.
    pub fn mpin_boxes(
        &self,
        master: &str,
        term: &str,
        idx: usize,
    ) -> Result<Vec<(i64, i32, i32, i32, i32)>> {
        Ok(chunk5(sys::mpin_boxes(self.r(), master, term, idx)?))
    }

    /// The instance's halo as `(left, bottom, right, top, is_soft)`, or `None` if it has none.
    ///
    /// ⚠️ **The four numbers are per-side DISTANCES, not a rectangle** — odb stores them in a
    /// `dbBox` and the field names read like coordinates. A hard halo is floored by the command's
    /// base halo side by side; a soft one is taken as it stands.
    pub fn inst_halo(&self, inst: &str) -> Result<Option<(i64, i64, i64, i64, bool)>> {
        let v = sys::inst_halo(self.r(), inst)?;
        if v.len() < 5 {
            return Ok(None);
        }
        Ok(Some((v[0], v[1], v[2], v[3], v[4] != 0)))
    }
    /// Every terminal of a master, as `(name, signal type)` — `POWER`, `GROUND`, `SIGNAL`, …
    ///
    /// 🔑 **The signal type is why this exists.** Selecting a master's supply pins — which is how
    /// `dpl` decides whether a cell's power rails line up with a row's — needs the `dbSigType`,
    /// and no other accessor here carries it for a master terminal.
    ///
    /// ⚠️ Empty for an unknown master, which is indistinguishable from a master with no terminals.
    /// Check the master exists first if that distinction matters.
    pub fn master_mterms(&self, master: &str) -> Result<Vec<(String, String)>> {
        Ok(sys::master_mterms(self.r(), master)?
            .into_iter()
            .filter_map(|row| {
                let (n, t) = row.split_once('\t')?;
                Some((n.to_string(), t.to_string()))
            })
            .collect())
    }

    /// Pin rectangles of **one** terminal, as `(layer number, x0, y0, x1, y1)` in master coordinates.
    ///
    /// ⚠️ [`Self::master_pin_boxes`] merges every terminal's shapes together, which throws away
    /// which terminal a shape belongs to. Connection by abutment turns on exactly that.
    pub fn mterm_pin_boxes(&self, master: &str, term: &str) -> Result<Vec<(i64, i32, i32, i32, i32)>> {
        Ok(chunk5(sys::mterm_pin_boxes(self.r(), master, term)?))
    }

    /// The same pin rectangles, but **without** the ones a POLYGON port decomposes into.
    ///
    /// ⚠️ `dbMPin::getGeometry()` includes decomposed polygons by default, so a caller that also
    /// reads the polygons back would count an octagonal pad twice — once as an octagon, once as
    /// the staircase of rectangles it splits into. Pair this with [`Db::iterm_pin_polygons`].
    pub fn mterm_pin_boxes_excluding_polygons(
        &self,
        master: &str,
        term: &str,
    ) -> Result<Vec<(i64, i32, i32, i32, i32)>> {
        Ok(chunk5(sys::mterm_pin_boxes_excluding_polygons(self.r(), master, term)?))
    }

    /// The POLYGON ports of an instance terminal, **already through the instance transform**.
    ///
    /// Returned as `(layer number, points)` per polygon. The transform is applied inside the
    /// binding, so nothing here has to reimplement `dbTransform` on a polygon.
    pub fn iterm_pin_polygons(&self, iterm: &str) -> Result<Vec<(i64, Vec<(i32, i32)>)>> {
        let flat = sys::iterm_pin_polygons(self.r(), iterm)?;
        let mut out = Vec::new();
        let mut i = 0usize;
        while i + 1 < flat.len() {
            let layer = flat[i] as i64;
            let n = flat[i + 1].max(0) as usize;
            i += 2;
            if i + 2 * n > flat.len() {
                break;
            }
            out.push((layer, (0..n).map(|k| (flat[i + 2 * k], flat[i + 2 * k + 1])).collect()));
            i += 2 * n;
        }
        Ok(out)
    }

    /// Start a fresh special wire on a net, so the wires written after it group separately.
    ///
    /// 🔑 The RDL router creates one `dbSWire` per **segment**, not one per net. The grouping is
    /// visible in the DEF — each swire prints its own status keyword and its remaining wires print
    /// `NEW` — so a writer that creates one per net emits a different file for identical geometry.
    pub fn new_swire(&mut self, net: &str, fixed: bool) -> Result<()> {
        sys::net_new_swire(self.r(), net, fixed)?;
        Ok(())
    }

    /// A boolean property on an instance terminal — `Some(value)`, or `None` when absent.
    ///
    /// ⚠️ **Absent is not false.** Callers that treat a missing property as `false` invert the
    /// meaning of every terminal that simply never had one.
    pub fn iterm_bool_property(&self, iterm: &str, name: &str) -> Result<Option<bool>> {
        Ok(match sys::iterm_bool_property(self.r(), iterm, name)? {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        })
    }

    /// The same property on the **master** terminal, which a whole library shares.
    pub fn mterm_bool_property(
        &self,
        master: &str,
        term: &str,
        name: &str,
    ) -> Result<Option<bool>> {
        Ok(match sys::mterm_bool_property(self.r(), master, term, name)? {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        })
    }

    /// Grow (or, with a negative margin, shrink) a polygon — `odb::Polygon::bloat`.
    ///
    /// 🔑 The result is a **closed, counter-clockwise** ring, and for a non-rectilinear polygon the
    /// offset is not a per-edge shift. This calls the library the reference calls rather than
    /// reimplementing it, so the two cannot drift apart.
    pub fn polygon_bloat(&self, points: &[(i32, i32)], margin: i32) -> Result<Vec<(i32, i32)>> {
        let flat: Vec<i32> = points.iter().flat_map(|&(x, y)| [x, y]).collect();
        let out = sys::polygon_bloat(&flat, margin)?;
        Ok(out.chunks_exact(2).map(|c| (c[0], c[1])).collect())
    }

    /// A layer's **type** — `ROUTING`, `CUT`, `OVERLAP`, and so on.
    ///
    /// ⚠️ The type, not the name. A layer named `OVERLAP` in one technology is a coincidence; the
    /// type is what marks an obstruction as an outline rather than as metal.
    pub fn layer_get_type(&self, layer: &str) -> Result<String> {
        Ok(sys::layer_get_type(self.r(), layer)?)
    }

    /// The die outline as a closed polygon of `(x, y)` points.
    ///
    /// ⚠️ **A rectangle reports five points**, the last repeating the first. More than five means a
    /// genuinely rectilinear die, and that count is exactly how the reference decides whether to
    /// place pins on four edges or on an arbitrary outline — so it is a branch condition, not a
    /// description.
    pub fn die_area_polygon(&self) -> Result<Vec<(i32, i32)>> {
        Ok(sys::die_area_polygon(self.r())?
            .chunks(2)
            .filter(|c| c.len() == 2)
            .map(|c| (c[0], c[1]))
            .collect())
    }

    /// The **top-layer pin grid**, if the design defines one
    /// (`define_pin_shape_pattern`): the layer, the step in each direction, the pin size, the
    /// keepout, the region's enclosing rectangle, and whether that region really is a rectangle.
    ///
    /// Pins assigned to this grid sit on a 2-D lattice *inside* the die rather than on its
    /// boundary — a different placement surface, not a variation on the edges.
    pub fn bterm_top_layer_grid(&self) -> Result<Option<TopLayerGrid>> {
        let v = sys::bterm_top_layer_grid(self.r())?;
        let [x_step, y_step, pin_width, pin_height, keepout, x0, y0, x1, y1] = v[..] else {
            return Ok(None);
        };
        Ok(Some(TopLayerGrid {
            layer: sys::bterm_top_layer_grid_layer(self.r())?,
            x_step,
            y_step,
            pin_width,
            pin_height,
            keepout,
            region: (x0, y0, x1, y1),
            region_is_rect: sys::bterm_top_layer_grid_is_rect(self.r())?,
        }))
    }

    /// Regions no pin may occupy — `exclude_io_pin_region`, as `(x0, y0, x1, y1)`.
    ///
    /// Read back from the block for the same reason constraints are: the design records them, so
    /// a placer told them separately could disagree with what it reads. A degenerate rectangle is
    /// an interval along one edge.
    pub fn blocked_regions_for_pins(&self) -> Result<Vec<(i32, i32, i32, i32)>> {
        Ok(sys::blocked_regions_for_pins(self.r())?
            .chunks(4)
            .filter(|c| c.len() == 4)
            .map(|c| (c[0], c[1], c[2], c[3]))
            .collect())
    }

    /// Every metal shape of every port already placed **fixed**: `(layer number, x0, y0, x1, y1)`.
    ///
    /// A fixed port is not ours to move — and the positions its metal covers are not ours to fill
    /// either. Resolve layer numbers with [`layer_name_by_number`](Self::layer_name_by_number).
    pub fn fixed_bterm_shapes(&self) -> Result<Vec<(i64, i32, i32, i32, i32)>> {
        Ok(sys::fixed_bterm_shapes(self.r())?
            .chunks(5)
            .filter(|c| c.len() == 5)
            .map(|c| (c[0], c[1] as i32, c[2] as i32, c[3] as i32, c[4] as i32))
            .collect())
    }

    /// The **pin groups** the design declares: ports that must land on adjacent slots, and
    /// whether their declared order matters.
    ///
    /// A group is a placement primitive, not a hint: its members occupy a contiguous run of slots,
    /// so a group that cannot find one has to be handled rather than spread out.
    pub fn bterm_groups(&self) -> Result<Vec<(Vec<String>, bool)>> {
        (0..sys::num_bterm_groups(self.r())?)
            .map(|i| Ok((sys::nth_bterm_group(self.r(), i)?, sys::nth_bterm_group_ordered(self.r(), i)?)))
            .collect()
    }

    /// A port's **constraint region**, if the design declares one: `(x0, y0, x1, y1)`.
    ///
    /// `set_io_pin_constraint -region` writes this onto the port in the database rather than
    /// passing it to a tool, so it survives a write/read cycle and is read back here — a placer
    /// must not be told the constraints separately, or the two can disagree.
    ///
    /// ⚠️ **Read the rectangle's shape, not just its extent.** A DEGENERATE rectangle (zero width
    /// or zero height) is an interval along one die edge, which is the ordinary case. One with
    /// real area is a top-layer region, an entirely different placement path.
    pub fn bterm_constraint_region(&self, bterm: &str) -> Result<Option<(i32, i32, i32, i32)>> {
        let v = sys::bterm_constraint_region(self.r(), bterm)?;
        Ok(match v[..] {
            [x0, y0, x1, y1] => Some((x0, y0, x1, y1)),
            _ => None,
        })
    }

    /// Every routing layer name, in stack order, with its routing direction
    /// (`HORIZONTAL`/`VERTICAL`/`NONE`).
    ///
    /// Direction is not in the generated surface, and it decides how a fill shape is oriented and
    /// which axis a line-end spacing applies to.
    pub fn layers_with_direction(&self) -> Result<Vec<(String, String)>> {
        (0..sys::num_layers(self.r())?)
            .map(|i| {
                let n = sys::nth_layer_name(self.r(), i)?;
                let d = sys::layer_direction(self.r(), &n)?;
                Ok((n, d))
            })
            .collect()
    }
    /// Every shape of every **placed** instance: `(layer number, x0, y0, x1, y1)` in placed
    /// coordinates.
    ///
    /// This is the design's own metal — pins and internal routing — and it is what density fill
    /// must not land on. Resolve layer numbers with [`layer_name_by_number`](Self::layer_name_by_number).
    pub fn inst_shapes(&self) -> Result<Vec<(i64, i32, i32, i32, i32)>> {
        Ok(sys::inst_shapes(self.r())?
            .chunks(5)
            .filter(|c| c.len() == 5)
            .map(|c| (c[0], c[1] as i32, c[2] as i32, c[3] as i32, c[4] as i32))
            .collect())
    }
    /// Every **special**-wire box as `(layer number, x0, y0, x1, y1)`, with via enclosures
    /// decomposed onto the layers they occupy.
    ///
    /// Special wires are the power grid. They are a separate collection from routed signal wires,
    /// and anything reasoning about occupied metal needs both.
    pub fn swire_boxes(&self) -> Result<Vec<(i64, i32, i32, i32, i32)>> {
        Ok(sys::swire_boxes(self.r())?
            .chunks(5)
            .filter(|c| c.len() == 5)
            .map(|c| (c[0], c[1] as i32, c[2] as i32, c[3] as i32, c[4] as i32))
            .collect())
    }
    /// Obstruction rectangles as `(layer number, x0, y0, x1, y1)`.
    pub fn obstruction_boxes(&self) -> Result<Vec<(i64, i32, i32, i32, i32)>> {
        Ok(sys::obstruction_boxes(self.r())?
            .chunks(5)
            .filter(|c| c.len() == 5)
            .map(|c| (c[0], c[1] as i32, c[2] as i32, c[3] as i32, c[4] as i32))
            .collect())
    }
    /// Placement blockage rectangles as `(x0, y0, x1, y1)`.
    ///
    /// ⚠️ Distinct from [`Self::obstruction_boxes`], which are *routing* obstructions and carry a
    /// layer. A placement blockage has no layer: it forbids cells outright, everywhere in its box.
    pub fn blockage_boxes(&self) -> Result<Vec<(i32, i32, i32, i32)>> {
        Ok(sys::blockage_boxes(self.r())?
            .chunks(4)
            .filter(|c| c.len() == 4)
            .map(|c| (c[0], c[1], c[2], c[3]))
            .collect())
    }
    /// Create a PLACEMENT blockage over a rectangle, optionally associated with an instance.
    ///
    /// 🔑 `mpl` creates one soft blockage per placed macro over its **haloed** bbox
    /// (`HierRTLMP::commitMacroPlacementToDb`), and `block_macro_channels` creates them around
    /// fixed macros. 34 of upstream's 36 `mpl` DEF goldens carry a `BLOCKAGES` section, so this
    /// is on the main path, not a corner.
    ///
    /// `inst` empty means "not associated with an instance". A non-empty name that does not
    /// resolve is an error, never a silently unassociated blockage.
    /// ⛔ **NOT TRANSACTIONAL** — geometry is not journaled; see [`eco_try`](Self::eco_try).
    pub fn create_blockage(
        &mut self,
        x1: i32,
        y1: i32,
        x2: i32,
        y2: i32,
        inst: &str,
        soft: bool,
    ) -> Result<()> {
        Ok(sys::blockage_create(self.r(), x1, y1, x2, y2, inst, soft)?)
    }
    /// Number of placement blockages in the block.
    pub fn num_blockages(&self) -> Result<usize> {
        Ok(sys::num_blockages(self.r())?)
    }
    /// Destroy the blockage at `idx`.
    ///
    /// ⛔ **NOT TRANSACTIONAL** — see [`eco_try`](Self::eco_try). Undo with
    /// [`truncate_blockages`](Self::truncate_blockages).
    pub fn destroy_blockage(&mut self, idx: usize) -> Result<()> {
        Ok(sys::blockage_destroy(self.r(), idx)?)
    }
    /// Destroy every blockage beyond the first `keep`, restoring a recorded baseline.
    ///
    /// 🔑 **This is the manual half of a rollback.** Record `num_blockages()` before the edits;
    /// call this with that number to undo them. Works backwards, because destroying one shifts
    /// the indices of those after it.
    /// ⛔ **NOT TRANSACTIONAL** — geometry is not journaled; see [`eco_try`](Self::eco_try).
    pub fn truncate_blockages(&mut self, keep: usize) -> Result<usize> {
        let have = self.num_blockages()?;
        for idx in (keep..have).rev() {
            self.destroy_blockage(idx)?;
        }
        Ok(have.saturating_sub(keep))
    }
    /// Create a fill rectangle. `mask` 0 means "no mask", which is what a single-mask layer wants.
    #[allow(clippy::too_many_arguments)]
    pub fn create_fill(
        &mut self,
        needs_opc: bool,
        mask: u32,
        layer: &str,
        x1: i32,
        y1: i32,
        x2: i32,
        y2: i32,
    ) -> Result<()> {
        Ok(sys::fill_create(self.r(), needs_opc, mask, layer, x1, y1, x2, y2)?)
    }
    /// Number of fill shapes in the block.
    pub fn num_fills(&self) -> Result<usize> {
        Ok(sys::num_fills(self.r())?)
    }
    /// Destroy every fill; returns the count removed. Fill is regenerated, not patched.
    /// ⛔ **NOT TRANSACTIONAL** — geometry is not journaled; see [`eco_try`](Self::eco_try).
    pub fn clear_fills(&mut self) -> Result<usize> {
        Ok(sys::clear_fills(self.r())?)
    }
    /// Destroy an instance. Errors when it does not exist, so a typo cannot pass as a no-op.
    /// Delete a net. Its terminals are left disconnected rather than deleted.
    /// 🔒 **Transactional** — rolled back by [`eco_try`](Self::eco_try).
    pub fn destroy_net(&mut self, net: &str) -> Result<()> {
        Ok(sys::net_destroy(self.r(), net)?)
    }
    /// Append a special-wire box to a net, making the wire container if it has none.
    pub fn add_swire_box(
        &mut self,
        net: &str,
        layer: &str,
        rect: (i32, i32, i32, i32),
        fixed: bool,
    ) -> Result<()> {
        Ok(sys::swire_add_box(self.r(), net, fixed, layer, rect.0, rect.1, rect.2, rect.3)?)
    }
    /// The cut rectangle a `VIARULE GENERATE` declares for one of its layers.
    ///
    /// ⚠️ Returns `None` where the rule declares no rectangle — a via layer rule may carry an
    /// enclosure or a spacing without one, so an absent rect is ordinary rather than an error.
    pub fn via_layer_rule_rect(&self, gen_idx: usize, layer_idx: usize) -> Option<(i32, i32, i32, i32)> {
        let v = sys::techvialayerrule_rect(self.r(), gen_idx, layer_idx).ok()?;
        (v.len() == 4).then(|| (v[0], v[1], v[2], v[3]))
    }
    /// Create a **generated via** with explicit cut geometry, or leave an existing one of that
    /// name untouched.
    ///
    /// ⚠️ `cut_spacing` is **edge-to-edge**, which is what the database stores — not the
    /// centre-to-centre pitch a via generator works in. Passing the pitch spreads every cut array
    /// by one cut width.
    #[allow(clippy::too_many_arguments)]
    pub fn create_generated_via(
        &mut self,
        name: &str,
        rule: &str,
        layers: (&str, &str, &str),
        cut: (i32, i32),
        cut_spacing: (i32, i32),
        bottom_enclosure: (i32, i32),
        top_enclosure: (i32, i32),
        rows: i32,
        columns: i32,
        // The via's own centre offset. ⚠️ A via built from a technology-declared one carries
        // the offset of that via's cut extent; one built from a generate rule is centred already
        // and passes (0, 0).
        origin: (i32, i32),
    ) -> Result<()> {
        Ok(sys::via_create_generated(
            self.r(), name, rule, layers.0, layers.1, layers.2,
            cut.0, cut.1, cut_spacing.0, cut_spacing.1,
            bottom_enclosure.0, bottom_enclosure.1, top_enclosure.0, top_enclosure.1,
            rows, columns, origin.0, origin.1,
        )?)
    }
    /// Place a via on a net's special wire.
    ///
    /// ⚠️ A via is placed by its **centre**, unlike [`Self::add_swire_box`], which is given corners.
    pub fn add_swire_via(
        &mut self,
        net: &str,
        via: &str,
        at: (i32, i32),
        fixed: bool,
        shape: &str,
    ) -> Result<()> {
        Ok(sys::swire_add_via(self.r(), net, fixed, via, at.0, at.1, shape)?)
    }
    /// **LEF 5.5 `SPACINGTABLE PARALLELRUNLENGTH`** — the spacing a wire of `width` must keep from
    /// its neighbours when it runs alongside them for `prl`.
    ///
    /// ⚠️ Both arguments matter and both are looked up as thresholds: the table is indexed by the
    /// widest `WIDTH` row at or below `width` and the longest run-length column at or below `prl`,
    /// so a long wide wire takes a far larger spacing than its layer's nominal one. Nangate45's
    /// metal4 gives 280 for a minimum wire and 1800 for a 1um wire running the height of the core.
    ///
    /// Returns 0 where the layer declares no such table.
    /// The spacing the database itself would use for a shape of this width running `length`
    /// alongside another — `dbTechLayer::getSpacing(width, length)`.
    ///
    /// 🔑 **Three rules and a default, not one table:** the V5.4 range rules, then the V5.5
    /// `SPACINGTABLE` which overwrites them, then the smallest V5.4 rule whose range ends below
    /// the width, and only then the layer's plain `SPACING`.
    ///
    /// ⚠️ [`Self::layer_find_v55_spacing`] is the second of those alone. Reading it as "the
    /// spacing" gives a keep-out short by whatever the range rules add; patching that gap with the
    /// plain `SPACING` skips the range rules instead, which is a different wrong answer.
    pub fn layer_get_spacing_for(&self, layer: &str, width: i32, length: i32) -> Result<i32> {
        Ok(sys::layer_get_spacing_for(self.r(), layer, width, length)?)
    }
    pub fn layer_find_v55_spacing(&self, layer: &str, width: i32, prl: i32) -> Result<i32> {
        Ok(sys::layer_find_v55_spacing(self.r(), layer, width, prl)?)
    }
    /// The `SPACINGTABLE TWOWIDTHS` answer for two shapes of the given widths running `prl`
    /// alongside each other; `0` where the layer declares no such table.
    ///
    /// ⚠️ **A companion to [`Self::layer_find_v55_spacing`], not an alternative.** A layer may
    /// declare both and a keep-out must take the LARGER of the two — OpenROAD's own
    /// `TechLayer::getSpacing` is `max(getSpacing(width, length), findTwSpacing(width, width,
    /// length))`. Asking one alone gives a halo short by whatever the other rule adds, which
    /// shows up as a shape cut a few hundred units further along than it should be.
    pub fn layer_find_tw_spacing(
        &self,
        layer: &str,
        width1: i32,
        width2: i32,
        prl: i32,
    ) -> Result<i32> {
        Ok(sys::layer_find_tw_spacing(self.r(), layer, width1, width2, prl)?)
    }

    /// The layer's LEF58 `WIDTHTABLE` rules as `(is_wrong_direction, widths)`, widths in database
    /// units, in the order the technology states them.
    ///
    /// ⚠️ **A table is a whitelist, and `WRONGDIRECTION` says WHICH shapes it whitelists** — a
    /// plain table constrains a wire running the layer's own way, a `WRONGDIRECTION` one
    /// constrains a wire running across it. The two never apply to the same shape, so a reader
    /// that treats the flag as an extra condition rather than an inversion checks the wrong
    /// shapes in both directions.
    ///
    /// ℹ️ Returned raw. Whether a given width is legal depends on the shape's direction and on
    /// the table's last entry as well as on membership, and that rule lives with its caller.
    pub fn layer_width_tables(&self, layer: &str) -> Result<Vec<(bool, Vec<i32>)>> {
        let n = sys::num_width_table_rules(self.r(), layer)?;
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            out.push((
                sys::width_table_rule_is_wrong_direction(self.r(), layer, i)?,
                sys::width_table_rule_widths(self.r(), layer, i)?,
            ));
        }
        Ok(out)
    }
    /// The minimum area a shape on this layer must have, in square database units.
    ///
    /// ⚠️ Where LEF58 `AREA` rules exist the **largest** of them governs and the layer's own
    /// `AREA` is ignored entirely rather than combined with them. A rule of 0 is skipped rather
    /// than treated as a minimum.
    ///
    /// Returns 0 where the layer sets no minimum, which is the common case.
    pub fn layer_min_area(&self, layer: &str) -> Result<i64> {
        Ok(sys::layer_min_area(self.r(), layer)?)
    }
    /// Every box of a named tech via as `(layer_number, x0, y0, x1, y1)`.
    ///
    /// A tech via is fixed geometry declared by the technology — cut boxes on its cut layer and
    /// metal boxes on its two routing layers. Empty when no via of that name exists.
    pub fn tech_via_boxes(&self, via: &str) -> Result<Vec<(i64, i32, i32, i32, i32)>> {
        Ok(sys::tech_via_boxes(self.r(), via)?
            .chunks(5)
            .filter(|c| c.len() == 5)
            .map(|c| (c[0] as i64, c[1], c[2], c[3], c[4]))
            .collect())
    }
    /// Every box of one block pin as `(layer_number, x0, y0, x1, y1)`.
    ///
    /// ⚠️ Distinct from the pin's bounding box, which carries no layer: a pin may have boxes on
    /// several layers, and an obstruction belongs to the layer its box is on.
    pub fn bpin_layer_boxes(&self, bterm: &str, pin: usize) -> Result<Vec<(i64, i32, i32, i32, i32)>> {
        Ok(sys::bpin_layer_boxes(self.r(), bterm, pin)?
            .chunks(5)
            .filter(|c| c.len() == 5)
            .map(|c| (c[0] as i64, c[1], c[2], c[3], c[4]))
            .collect())
    }
    /// Every boundary rectangle of a named region.
    ///
    /// 🔑 **A region may be more than one rectangle.** DEF allows a non-rectangular region,
    /// expressed as several boxes, so this returns all of them — taking only the first silently
    /// shrinks the domain a grid is built for.
    ///
    /// ⚠️ `num_region_get_boundaries` on the generated bridge gives the COUNT and nothing else;
    /// this is the geometry.
    pub fn region_boundaries(&self, region: &str) -> Result<Vec<(i32, i32, i32, i32)>> {
        Ok(sys::region_boundaries(self.r(), region)?
            .chunks(4)
            .filter(|c| c.len() == 4)
            .map(|c| (c[0], c[1], c[2], c[3]))
            .collect())
    }
    /// One net's existing **special**-wire shapes as
    /// `(layer number, x0, y0, x1, y1, wire shape type, octilinear)`.
    ///
    /// The type is the DEF wire shape: `0` NONE, `1` RING, `2` PADRING, `3` BLOCKRING, `4` STRIPE,
    /// `5` FOLLOWPIN, `6` IOWIRE, `7` COREWIRE, `8` BLOCKWIRE, `9` BLOCKAGEWIRE, `10` FILLWIRE,
    /// `11` DRCFILL. It decides what a shape may be connected to, so it cannot be discarded: a
    /// stripe and a ring are landing targets where a follow pin is not.
    ///
    /// Via boxes are decomposed onto the layers their metal occupies, at the via's own placement.
    ///
    /// ⚠️ **An octilinear box's corners are its bounding box, not its metal** — the shape is a
    /// 45-degree segment somewhere inside. Treat it as occupied space and never as a target.
    ///
    /// ⚠️ Distinct from [`Self::swire_boxes`], which flattens every net together and carries
    /// neither the net nor the type. That answers *is this occupied*; this one also answers
    /// *whose is it, and of what kind*.
    pub fn net_swire_shapes(&self, net: &str) -> Result<Vec<(i64, i32, i32, i32, i32, i32, bool)>> {
        Ok(sys::net_swire_shapes(self.r(), net)?
            .chunks(7)
            .filter(|c| c.len() == 7)
            .map(|c| {
                (
                    c[0],
                    c[1] as i32,
                    c[2] as i32,
                    c[3] as i32,
                    c[4] as i32,
                    c[5] as i32,
                    c[6] != 0,
                )
            })
            .collect())
    }
    /// Every **special**-wire box of one net as
    /// `(layer number, swire is FIXED, box is OCTILINEAR, x0, y0, x1, y1, width)`.
    ///
    /// 🔑 **The discriminator is the SWIRE's wire type, not the box's shape type.**
    /// `RDLRouter::populateObstructions` inserts every swire box on the routing layer as an
    /// obstruction and skips only `swire->getWireType() != dbWireType::FIXED` on the nets it is
    /// routing now — because those are the wires it is about to destroy and rebuild. A FIXED swire
    /// on a net being routed therefore still blocks that net's own router.
    ///
    /// ⚠️ [`Self::net_swire_shapes`] cannot answer this: it reports each box's
    /// `getWireShapeType()` (IOWIRE, STRIPE, …), a different field, and it flattens an octagon to
    /// its bounding rectangle. Here an OCTILINEAR box gives the octagon's **centre-low and
    /// centre-high points and its width**, so the true shape can be rebuilt; a rectangle gives its
    /// box and a width of 0.
    #[allow(clippy::type_complexity)]
    pub fn net_swire_obstacles(
        &self,
        net: &str,
    ) -> Result<Vec<(i64, bool, bool, i32, i32, i32, i32, i32)>> {
        Ok(sys::net_swire_obstacles(self.r(), net)?
            .chunks(8)
            .filter(|c| c.len() == 8)
            .map(|c| {
                (c[0], c[1] != 0, c[2] != 0, c[3] as i32, c[4] as i32,
                 c[5] as i32, c[6] as i32, c[7] as i32)
            })
            .collect())
    }
    /// A tech via's bottom (`"bottom"`) or top (`"top"`) routing layer name.
    pub fn tech_via_layer(&self, via: &str, which: &str) -> Result<String> {
        Ok(sys::tech_via_layer(self.r(), via, which)?)
    }
    /// Every LEF58 `ARRAYSPACING` rule this cut layer states, as
    /// `(cut class, parallel overlap, long array, array width, cut spacing, (cuts, spacing) pairs)`.
    ///
    /// A rule says that once a via holds enough cuts in a row they must be grouped: `cuts` per
    /// group at the ordinary pitch, with `spacing` between groups.
    ///
    /// ⚠️ **An empty cut class means EVERY class, not none** — `ViaGenerator::isCutClass` returns
    /// true whenever either side is null.
    ///
    /// ⚠️ **A zero `array_width` means the rule states no width limit**, and a zero `cut_spacing`
    /// means it states none and the via keeps the pitch it already had. Neither is a limit of
    /// nothing: the reference skips a rule only on `getArrayWidth() != 0 && getArrayWidth() > width`.
    #[allow(clippy::type_complexity)]
    pub fn layer_array_spacing_rules(
        &self,
        layer: &str,
    ) -> Result<Vec<(String, bool, bool, i32, i32, Vec<(i32, i32)>)>> {
        let n = sys::num_array_spacing_rules(self.r(), layer)?;
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            out.push((
                sys::array_spacing_rule_cut_class(self.r(), layer, i)?,
                sys::array_spacing_rule_is_parallel_overlap(self.r(), layer, i)?,
                sys::array_spacing_rule_is_long_array(self.r(), layer, i)?,
                sys::array_spacing_rule_array_width(self.r(), layer, i)?,
                sys::array_spacing_rule_cut_spacing(self.r(), layer, i)?,
                sys::array_spacing_rule_cuts_spacing(self.r(), layer, i)?
                    .chunks(2)
                    .filter(|c| c.len() == 2)
                    .map(|c| (c[0], c[1]))
                    .collect(),
            ));
        }
        Ok(out)
    }
    /// A cut layer's LEF 5.4 adjacent-cut spacing rules, as `(cuts, spacing, except_same_pgnet)`.
    ///
    /// 🔑 **`SPACING <s> ADJACENTCUTS <n> WITHIN <w>` widens a via array once enough of its cuts
    /// are adjacent** — the cut layer's plain `SPACING` no longer applies. A rule stating no
    /// adjacent-cut clause comes back with zero cuts and is not a rule of this kind.
    ///
    /// ⚠️ `WITHIN` is not returned because the generator never consults it: it decides adjacency
    /// from the array's own shape, not from a distance.
    pub fn layer_v54_adjacent_cut_rules(&self, layer: &str) -> Result<Vec<(u32, i32, bool)>> {
        let n = sys::num_v54_spacing_rules(self.r(), layer)?;
        let mut out = Vec::new();
        for i in 0..n {
            let cuts = sys::v54_spacing_rule_adjacent_cuts(self.r(), layer, i)?;
            if cuts == 0 {
                continue;
            }
            out.push((
                cuts,
                sys::v54_spacing_rule_adjacent_spacing(self.r(), layer, i)?,
                sys::v54_spacing_rule_adjacent_except_same_pgnet(self.r(), layer, i)?,
            ));
        }
        Ok(out)
    }
    /// How many LEF58 cut spacing tables this cut layer states.
    ///
    /// ⚠️ Where a cut layer states no `SPACING` of its own — every ASAP7 cut layer — this table is
    /// the only place a cut pitch comes from, so an empty answer is not the same as "no spacing".
    pub fn num_cut_spacing_table_rules(&self, layer: &str) -> Result<usize> {
        Ok(sys::num_cut_spacing_table_rules(self.r(), layer)?)
    }
    /// The largest spacing this rule states for a cut class against itself, `MIN` strategy.
    ///
    /// Used for a **square** cut, where the side/end distinction cannot be drawn.
    pub fn cut_spacing_table_max_spacing(&self, layer: &str, idx: usize, cls: &str) -> Result<i32> {
        Ok(sys::cut_spacing_table_max_spacing(self.r(), layer, idx, cls)?)
    }
    /// The spacing for a cut class against itself, each side flagged as a SIDE or an END.
    ///
    /// ⚠️ **The flags are crossed against the axis being asked about.** The x pitch is asked with
    /// the *y* side flag, because a cut's `dx` is its side when `dx > dy`.
    pub fn cut_spacing_table_spacing(
        &self,
        layer: &str,
        idx: usize,
        cls: &str,
        side1: bool,
        side2: bool,
    ) -> Result<i32> {
        Ok(sys::cut_spacing_table_spacing(self.r(), layer, idx, cls, side1, side2)?)
    }
    /// Whether this rule measures centre-to-edge, in which case its value is already a spacing.
    pub fn cut_spacing_table_is_center_and_edge(
        &self,
        layer: &str,
        idx: usize,
        cls: &str,
    ) -> Result<bool> {
        Ok(sys::cut_spacing_table_is_center_and_edge(self.r(), layer, idx, cls)?)
    }
    /// Whether this rule measures centre-to-centre, in which case a cut must be subtracted.
    pub fn cut_spacing_table_is_center_to_center(
        &self,
        layer: &str,
        idx: usize,
        cls: &str,
    ) -> Result<bool> {
        Ok(sys::cut_spacing_table_is_center_to_center(self.r(), layer, idx, cls)?)
    }
    /// A special-wire box carrying an explicit **shape annotation** — `"FOLLOWPIN"`, `"STRIPE"`,
    /// `"RING"`, and so on.
    ///
    /// ⚠️ [`Self::add_swire_box`] writes `IOWIRE`, which is what a wire with no particular role is.
    /// Every wire in a power grid has one, and a DEF comparison sees the difference: identical
    /// geometry under the wrong annotation matches nothing.
    pub fn add_swire_box_shaped(
        &mut self,
        net: &str,
        layer: &str,
        rect: (i32, i32, i32, i32),
        fixed: bool,
        shape: &str,
    ) -> Result<()> {
        Ok(sys::swire_add_box_shaped(
            self.r(), net, fixed, layer, rect.0, rect.1, rect.2, rect.3, shape,
        )?)
    }
    /// A **diagonal** (octilinear) special wire — a 45-degree route segment.
    ///
    /// ⛔ **Endpoints and a width, not a rectangle.** `rect` here is the segment's two ends, and
    /// `width` is carried separately; the reference's `dbSBox::create` takes a different overload
    /// for this and deliberately discards the bounding box it computes for axis-aligned wires.
    /// Passing a bbox to [`add_swire_box`](Self::add_swire_box) instead puts a rectangle where a
    /// 45 belongs.
    ///
    /// ⚠️ Without this a caller has no way to express a diagonal at all, and skipping the piece
    /// leaves the straight wires either side of it not meeting — a **disconnected** net rather
    /// than a slightly shorter one.
    /// ⛔ **NOT TRANSACTIONAL** — geometry is not journaled; see [`eco_try`](Self::eco_try).
    pub fn add_swire_octilinear(
        &mut self,
        net: &str,
        layer: &str,
        from: (i32, i32),
        to: (i32, i32),
        width: i32,
        fixed: bool,
        shape: &str,
    ) -> Result<()> {
        Ok(sys::swire_add_box_octilinear(
            self.r(), net, fixed, layer, from.0, from.1, to.0, to.1, width, shape,
        )?)
    }
    /// Remove a net's **routed** special wires, keeping any marked fixed.
    /// ⛔ **NOT TRANSACTIONAL** — geometry is not journaled; see [`eco_try`](Self::eco_try).
    pub fn clear_routed_swires(&mut self, net: &str) -> Result<usize> {
        Ok(sys::swire_clear_routed(self.r(), net)?)
    }
    /// The database's own identifier for a terminal.
    ///
    /// ⚠️ It reflects **creation order**, so it cannot be reconstructed from geometry or names.
    /// Reference tools use it to settle ordering ties, where any other tie-break gives a different
    /// — and equally self-consistent — answer.
    pub fn iterm_id(&self, inst: &str, pin: &str) -> Result<u32> {
        Ok(sys::iterm_get_id(self.r(), inst, pin)?)
    }
    /// The database's own identifier for an instance. Maps keyed by instance iterate in this order.
    pub fn inst_id(&self, inst: &str) -> Result<u32> {
        Ok(sys::inst_get_id(self.r(), inst)?)
    }
    /// Create a block terminal on a net.
    /// 🔒 **Transactional** — rolled back by [`eco_try`](Self::eco_try).
    pub fn create_bterm(&mut self, net: &str, name: &str) -> Result<()> {
        Ok(sys::bterm_create(self.r(), net, name)?)
    }
    /// Add a pin shape to a block terminal, returning the new pin's index.
    ///
    /// ⚠️ The pin and its box are made in one call because a `dbBPin` has no name — there is no
    /// way to address the pin in between. The index returned is what the `bpin_*` accessors take.
    pub fn create_bterm_pin(
        &mut self,
        bterm: &str,
        layer: &str,
        rect: (i32, i32, i32, i32),
    ) -> Result<usize> {
        Ok(sys::bterm_create_pin(self.r(), bterm, layer, rect.0, rect.1, rect.2, rect.3)?)
    }
    /// 🔒 **Transactional** — rolled back by [`eco_try`](Self::eco_try).
    pub fn destroy_inst(&mut self, inst: &str) -> Result<()> {
        Ok(sys::destroy_inst(self.r(), inst)?)
    }
    /// The `i`th row's `(bbox, site, orientation)`, addressed by INDEX.
    ///
    /// ⚠️ **Use this, not the by-name `row_get_*` accessors, to walk rows.** Row names are **not
    /// unique** — one upstream test case has 699 rows over 692 names — and the by-name accessors
    /// return the first match, so a walk by name silently reads one row's geometry for another
    /// and loses the rest.
    pub fn nth_row(&self, i: usize) -> Result<Option<(Vec<i32>, String, String)>> {
        let bbox = sys::nth_row_bbox(self.r(), i)?;
        if bbox.len() != 4 {
            return Ok(None);
        }
        Ok(Some((bbox, sys::nth_row_site(self.r(), i)?, sys::nth_row_orient(self.r(), i)?)))
    }
    /// Remove a block terminal outright.
    ///
    /// 🔑 **Only a terminal this run created and then left empty should go.** One the design
    /// arrived with is not a generator's to remove, empty or not.
    /// 🔒 **Transactional** — rolled back by [`eco_try`](Self::eco_try).
    pub fn bterm_destroy(&mut self, bterm: &str) -> Result<()> {
        Ok(sys::bterm_destroy(self.r(), bterm)?)
    }
    /// Remove a terminal's generator-owned pin geometry, keeping anything placed FIXED.
    ///
    /// ⚠️ **FIXED is the exemption and it is the whole rule.** A generator republishing a supply
    /// terminal owns what it produced last time and must leave alone what a person placed.
    /// Returns how many pins were removed.
    /// 🔒 **Transactional** — rolled back by [`eco_try`](Self::eco_try).
    pub fn bterm_clear_unfixed_bpins(&mut self, bterm: &str) -> Result<usize> {
        Ok(sys::bterm_clear_unfixed_bpins(self.r(), bterm)?)
    }
    /// Add one rectangle to a terminal's pin geometry; `false` if it was already there.
    ///
    /// 🔑 **Every rectangle goes on ONE pin object, not one pin per rectangle.** A `dbBPin` is a
    /// DEF `PORT`, so a terminal built pin-per-rectangle writes several PORT blocks with separate
    /// placements where one PORT carrying several LAYER lines is meant — a difference visible in
    /// the DEF for geometry that is otherwise identical.
    pub fn bterm_add_pin_box(
        &mut self,
        bterm: &str,
        layer: &str,
        rect: (i32, i32, i32, i32),
    ) -> Result<bool> {
        Ok(sys::bterm_add_pin_box(self.r(), bterm, layer, rect.0, rect.1, rect.2, rect.3)?)
    }
    /// The direction the `i`th row's sites step in — `"HORIZONTAL"` or `"VERTICAL"`.
    ///
    /// ⚠️ **A row's direction is stated, not measured.** A single-site row's bounding box says
    /// nothing about which way it runs, and the rails laid along it follow the stated direction.
    pub fn nth_row_direction(&self, i: usize) -> Result<String> {
        Ok(sys::nth_row_direction(self.r(), i)?)
    }
    /// Name of the `i`th row. Empty when out of range.
    ///
    /// odb hands rows back in **reverse creation order**; anything that numbers or iterates rows
    /// inherits that.
    pub fn nth_row_name(&self, i: usize) -> Result<String> {
        Ok(sys::nth_row_name(self.r(), i)?)
    }
    /// Every row name, in odb's order.
    pub fn row_names(&self) -> Result<Vec<String>> {
        (0..self.num_rows()?).map(|i| self.nth_row_name(i)).collect()
    }
    /// A site's class (`CORE`, `PAD`, …). Empty when the site is unknown.
    pub fn site_get_class(&self, site: &str) -> Result<String> {
        Ok(sys::site_get_class(self.r(), site)?)
    }
    /// A hybrid site's row pattern as `(site name, orientation)`, in order.
    ///
    /// Empty when the site declares no pattern, which is the ordinary single-height case — the
    /// caller reads "no pattern" from an empty vector rather than having to ask twice.
    pub fn row_pattern(&self, site: &str) -> Result<Vec<(String, String)>> {
        (0..sys::site_row_pattern_len(self.r(), site)?)
            .map(|i| {
                Ok((
                    sys::site_row_pattern_site(self.r(), site, i)?,
                    sys::site_row_pattern_orient(self.r(), site, i)?,
                ))
            })
            .collect()
    }
    /// Place a port (`bterm`) pin box on `layer` at the given DBU rectangle. Errors on unknown
    /// bterm/layer.
    /// 🔒 **Transactional** — rolled back by [`eco_try`](Self::eco_try).
    pub fn place_bterm(&mut self, bterm: &str, layer: &str, x1: i32, y1: i32, x2: i32, y2: i32) -> Result<()> {
        Ok(sys::place_bterm(self.r(), bterm, layer, x1, y1, x2, y2)?)
    }
    /// 🔒 **Transactional** — rolled back by [`eco_try`](Self::eco_try).
    pub fn connect(&mut self, inst: &str, pin: &str, net: &str) -> Result<()> {
        Ok(sys::connect(self.r(), inst, pin, net)?)
    }
    /// 🔒 **Transactional** — rolled back by [`eco_try`](Self::eco_try).
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

/// `RDLSegment::preprocess`'s geometry — does a segment's source terminal already touch this
/// destination, and if only within spacing, what stubs bridge the gap?
///
/// Takes no database handle: it is pure geometry, so it can be tested without a design.
///
/// Returns `(verdict, stubs)`:
/// - `0` — no contact even bloated; the segment needs a route.
/// - `1` — the shapes **interact** as they are: upstream locks the segment and writes **no wire at
///   all**.
/// - `2` — they interact once bloated by `min_dist` (the LAYER's spacing, not the command's
///   `-spacing`): locked, and `stubs` are written in place of a route.
///
/// 🔑 **Bound, not reimplemented, and `interact` is why.** `polygon_90_set_data::interact` is TOUCH
/// connectivity — it decomposes the set, builds a touch graph through `touch_90_operation` (the
/// same scanline machinery behind `connectivity_extraction_90`), and keeps WHOLE polygons whose
/// label meets the other set. Two shapes sharing an edge with zero gap interact while their
/// intersection is empty, so writing `a & b` instead looks like an obvious simplification and
/// silently routes a pair the reference locks. `tests/rdl_preprocess.rs` fails on that substitution.
#[allow(clippy::type_complexity)]
pub fn rdl_preprocess(
    source_rects: &[(i32, i32, i32, i32)],
    dest_rects: &[(i32, i32, i32, i32)],
    min_dist: i32,
) -> Result<(i32, Vec<(i32, i32, i32, i32)>)> {
    let flat = |v: &[(i32, i32, i32, i32)]| -> Vec<i32> {
        v.iter().flat_map(|r| [r.0, r.1, r.2, r.3]).collect()
    };
    let out = sys::rdl_preprocess(&flat(source_rects), &flat(dest_rects), min_dist)?;
    let Some((&verdict, rest)) = out.split_first() else {
        return Ok((0, Vec::new()));
    };
    Ok((verdict, rest.chunks_exact(4).map(|c| (c[0], c[1], c[2], c[3])).collect()))
}
