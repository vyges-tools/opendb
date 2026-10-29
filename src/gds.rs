//! GDSII through OpenDB's own GDS model and its reader and writer (odb `gdsin` / `gdsout`).
//!
//! [`Db::gds_read`] is odb's `GDSReader::read_gds`, [`Db::gds_write`] its `GDSWriter::write_gds`,
//! and the elements are odb's `dbGDS*` objects, unchanged. What this module adds is the one thing
//! the pinned OpenROAD does not have: [`write_gds`], a placed and routed block streamed out as GDS.
//! Nothing in odb converts a block to a `dbGDSLib`, and no command exposes the GDS model; open
//! flows stream out with another tool.
//!
//! [`write_gds`] follows the KLayout stream-out LibreLane runs (`scripts/klayout/stream_out.py`),
//! step for step: read the design with its LEFs and the PDK's LEF/DEF layer map, keep the cells
//! the design instantiates and take their contents from the standard-cell GDS, refuse when a cell
//! has no GDS, copy the top cell's tree into a fresh library, write it. It is scored by a per-layer
//! XOR against that stream-out of the same DEF, flattened. Geometry must agree, not hierarchy:
//! KLayout places each via as a `VIA_<name>` cell, this writes the via's boxes into the top cell.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use crate::{path_str, sys, Db, Error, Result};

/// A GDS layer and datatype.
pub type Ld = (i16, i16);

/// A GDS library inside a [`Db`]: the handle [`Db::gds_read`] and [`Db::gds_create_lib`] return.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GdsLib(usize);

/// A reference's transform: reflected about the x axis first, then magnified, then rotated
/// `angle` degrees counter-clockwise — the GDS order of operations (odb `dbGDSSTrans`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct STrans {
    pub flip_x: bool,
    pub mag: f64,
    pub angle: f64,
}

impl STrans {
    pub const IDENTITY: STrans = STrans { flip_x: false, mag: 1.0, angle: 0.0 };
}

/// A property: attribute number and value (`PROPATTR` / `PROPVALUE`).
pub type Prop = (i16, String);

/// One element of a GDS structure, as odb's `dbGDS*` objects hold it.
#[derive(Debug, Clone, PartialEq)]
pub enum GdsElement {
    /// A polygon; `xy` repeats its first point last, as a GDS BOUNDARY does.
    Boundary { layer: i16, datatype: i16, xy: Vec<(i32, i32)>, props: Vec<Prop> },
    Box { layer: i16, datatype: i16, bounds: (i32, i32, i32, i32), props: Vec<Prop> },
    Path { layer: i16, datatype: i16, path_type: i16, width: i32, xy: Vec<(i32, i32)>, props: Vec<Prop> },
    SRef { structure: String, origin: (i32, i32), trans: STrans, props: Vec<Prop> },
    ARef {
        structure: String,
        origin: (i32, i32),
        lr: (i32, i32),
        ul: (i32, i32),
        rows: i16,
        columns: i16,
        trans: STrans,
        props: Vec<Prop>,
    },
    /// `presentation` is (vertical, horizontal): 0 top/left, 1 middle/centre, 2 bottom/right.
    Text { layer: i16, datatype: i16, origin: (i32, i32), presentation: (u8, u8), trans: STrans, text: String, props: Vec<Prop> },
}

impl GdsElement {
    /// A rectangle as a closed BOUNDARY.
    pub fn rect(ld: Ld, x0: i32, y0: i32, x1: i32, y1: i32) -> GdsElement {
        GdsElement::Boundary { layer: ld.0, datatype: ld.1, xy: vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1), (x0, y0)], props: Vec::new() }
    }
}

/// The flat encoding the bridge carries (see the shim): element kinds and integers, the reals of
/// each transform, and the strings (reference names, text, property values).
#[derive(Default)]
struct Flat {
    ints: Vec<i64>,
    reals: Vec<f64>,
    strs: Vec<String>,
}

impl Flat {
    fn xy(&mut self, xy: &[(i32, i32)]) {
        self.ints.push(xy.len() as i64);
        for &(x, y) in xy {
            self.ints.extend([x as i64, y as i64]);
        }
    }
    fn trans(&mut self, t: &STrans) {
        self.reals.extend([if t.flip_x { 1.0 } else { 0.0 }, t.mag, t.angle]);
    }
    fn props(&mut self, props: &[Prop]) {
        self.ints.push(props.len() as i64);
        for (a, v) in props {
            self.ints.push(*a as i64);
            self.strs.push(v.clone());
        }
    }
    fn push(&mut self, e: &GdsElement) {
        match e {
            GdsElement::Boundary { layer, datatype, xy, props } => {
                self.ints.extend([0, *layer as i64, *datatype as i64]);
                self.xy(xy);
                self.props(props);
            }
            GdsElement::Box { layer, datatype, bounds: (x0, y0, x1, y1), props } => {
                self.ints.extend([1, *layer as i64, *datatype as i64, *x0 as i64, *y0 as i64, *x1 as i64, *y1 as i64]);
                self.props(props);
            }
            GdsElement::Path { layer, datatype, path_type, width, xy, props } => {
                self.ints.extend([2, *layer as i64, *datatype as i64, *path_type as i64, *width as i64]);
                self.xy(xy);
                self.props(props);
            }
            GdsElement::SRef { structure, origin, trans, props } => {
                self.ints.extend([3, origin.0 as i64, origin.1 as i64]);
                self.trans(trans);
                self.strs.push(structure.clone());
                self.props(props);
            }
            GdsElement::ARef { structure, origin, lr, ul, rows, columns, trans, props } => {
                self.ints.push(4);
                for p in [origin, lr, ul] {
                    self.ints.extend([p.0 as i64, p.1 as i64]);
                }
                self.ints.extend([*rows as i64, *columns as i64]);
                self.trans(trans);
                self.strs.push(structure.clone());
                self.props(props);
            }
            GdsElement::Text { layer, datatype, origin, presentation, trans, text, props } => {
                self.ints.extend([5, *layer as i64, *datatype as i64, origin.0 as i64, origin.1 as i64]);
                self.ints.extend([presentation.0 as i64, presentation.1 as i64]);
                self.trans(trans);
                self.strs.push(text.clone());
                self.props(props);
            }
        }
    }
}

/// Reads the flat encoding back into elements.
struct Unflat<'a> {
    ints: &'a [i64],
    reals: &'a [f64],
    strs: &'a [String],
    i: usize,
    r: usize,
    s: usize,
}

impl Unflat<'_> {
    fn int(&mut self) -> Result<i64> {
        let v = *self.ints.get(self.i).ok_or_else(|| Error::Odb("gds: element encoding truncated".to_string()))?;
        self.i += 1;
        Ok(v)
    }
    fn i16(&mut self) -> Result<i16> {
        Ok(self.int()? as i16)
    }
    fn point(&mut self) -> Result<(i32, i32)> {
        Ok((self.int()? as i32, self.int()? as i32))
    }
    fn xy(&mut self) -> Result<Vec<(i32, i32)>> {
        let n = self.int()?;
        (0..n).map(|_| self.point()).collect()
    }
    fn trans(&mut self) -> Result<STrans> {
        let t = self.reals.get(self.r..self.r + 3).ok_or_else(|| Error::Odb("gds: transform encoding truncated".to_string()))?;
        self.r += 3;
        Ok(STrans { flip_x: t[0] != 0.0, mag: t[1], angle: t[2] })
    }
    fn str(&mut self) -> Result<String> {
        let v = self.strs.get(self.s).ok_or_else(|| Error::Odb("gds: string encoding truncated".to_string()))?.clone();
        self.s += 1;
        Ok(v)
    }
    fn props(&mut self) -> Result<Vec<Prop>> {
        let n = self.int()?;
        (0..n).map(|_| Ok((self.i16()?, self.str()?))).collect()
    }
    fn element(&mut self) -> Result<GdsElement> {
        Ok(match self.int()? {
            0 => {
                let (layer, datatype) = (self.i16()?, self.i16()?);
                GdsElement::Boundary { layer, datatype, xy: self.xy()?, props: self.props()? }
            }
            1 => {
                let (layer, datatype) = (self.i16()?, self.i16()?);
                let (a, b) = (self.point()?, self.point()?);
                GdsElement::Box { layer, datatype, bounds: (a.0, a.1, b.0, b.1), props: self.props()? }
            }
            2 => {
                let (layer, datatype, path_type, width) = (self.i16()?, self.i16()?, self.i16()?, self.int()? as i32);
                GdsElement::Path { layer, datatype, path_type, width, xy: self.xy()?, props: self.props()? }
            }
            3 => {
                let origin = self.point()?;
                let trans = self.trans()?;
                GdsElement::SRef { structure: self.str()?, origin, trans, props: self.props()? }
            }
            4 => {
                let (origin, lr, ul) = (self.point()?, self.point()?, self.point()?);
                let (rows, columns) = (self.i16()?, self.i16()?);
                let trans = self.trans()?;
                GdsElement::ARef { structure: self.str()?, origin, lr, ul, rows, columns, trans, props: self.props()? }
            }
            5 => {
                let (layer, datatype) = (self.i16()?, self.i16()?);
                let origin = self.point()?;
                let presentation = (self.int()? as u8, self.int()? as u8);
                let trans = self.trans()?;
                GdsElement::Text { layer, datatype, origin, presentation, trans, text: self.str()?, props: self.props()? }
            }
            k => return Err(Error::Odb(format!("gds: unknown element kind {k}"))),
        })
    }
}

impl Db {
    /// Read a GDS file into a new library (odb `GDSReader::read_gds`).
    ///
    /// ⚠️ odb's reader is strict: an unknown record inside an element, a duplicate structure, or
    /// a BGNLIB that is not 28 bytes is an error, not something skipped.
    pub fn gds_read(&mut self, path: impl AsRef<Path>) -> Result<GdsLib> {
        sys::clear_diagnostics();
        Ok(GdsLib(sys::gds_read(self.r(), &path_str(path)?)?))
    }

    /// Write a library as GDS: odb `GDSWriter::write_gds`, then [`pad_odd_strings`] over the file.
    ///
    /// ⛔ **The one place this is not upstream's bytes.** odb's writer emits an ASCII record at the
    /// string's own length, and GDSII requires a string of odd length to be padded with a NUL to
    /// an even one: every odd-length cell name (`sky130_fd_sc_hd__a2111o_2`) is a malformed
    /// record that KLayout reads with an "Odd record length" warning and a stricter reader
    /// refuses. odb's own reader drops one trailing NUL, so it reads the padded file the same.
    pub fn gds_write(&self, lib: GdsLib, path: impl AsRef<Path>) -> Result<()> {
        let path = path_str(path)?;
        sys::gds_write(self.r(), lib.0, &path)?;
        let io = |e: std::io::Error| Error::Odb(format!("{path}: {e}"));
        let bytes = std::fs::read(&path).map_err(io)?;
        let padded = pad_odd_strings(&bytes)?;
        if padded.len() != bytes.len() {
            std::fs::write(&path, padded).map_err(io)?;
        }
        Ok(())
    }

    /// An empty library (odb `createEmptyGDSLib`). Its units are odb's defaults until set.
    pub fn gds_create_lib(&mut self, name: &str) -> Result<GdsLib> {
        Ok(GdsLib(sys::gds_create_lib(self.r(), name)?))
    }

    pub fn gds_lib_name(&self, lib: GdsLib) -> Result<String> {
        Ok(sys::gds_lib_name(self.r(), lib.0)?)
    }

    /// `(user units per database unit, metres per database unit)`, the UNITS record.
    pub fn gds_units(&self, lib: GdsLib) -> Result<(f64, f64)> {
        let u = sys::gds_lib_units(self.r(), lib.0)?;
        Ok((u[0], u[1]))
    }

    pub fn gds_set_units(&mut self, lib: GdsLib, uu_per_dbu: f64, dbu_per_meter: f64) -> Result<()> {
        Ok(sys::gds_lib_set_units(self.r(), lib.0, uu_per_dbu, dbu_per_meter)?)
    }

    /// The library's structures, in the order the writer writes them.
    pub fn gds_structures(&self, lib: GdsLib) -> Result<Vec<String>> {
        Ok(sys::gds_structure_names(self.r(), lib.0)?)
    }

    /// Create an empty structure; an error when the name is already in the library.
    pub fn gds_create_structure(&mut self, lib: GdsLib, name: &str) -> Result<()> {
        Ok(sys::gds_structure_create(self.r(), lib.0, name)?)
    }

    pub fn gds_destroy_structure(&mut self, lib: GdsLib, name: &str) -> Result<()> {
        Ok(sys::gds_structure_destroy(self.r(), lib.0, name)?)
    }

    /// A structure's elements in the writer's order: boundaries, boxes, paths, srefs, arefs, texts.
    pub fn gds_elements(&self, lib: GdsLib, structure: &str) -> Result<Vec<GdsElement>> {
        let (mut reals, mut strs) = (Vec::new(), Vec::new());
        let ints = sys::gds_elements(self.r(), lib.0, structure, &mut reals, &mut strs)?;
        let mut u = Unflat { ints: &ints, reals: &reals, strs: &strs, i: 0, r: 0, s: 0 };
        let mut out = Vec::new();
        while u.i < ints.len() {
            out.push(u.element()?);
        }
        Ok(out)
    }

    /// Add elements to a structure, in order. A reference must name a structure already in the
    /// same library.
    pub fn gds_add(&mut self, lib: GdsLib, structure: &str, elements: &[GdsElement]) -> Result<()> {
        let mut f = Flat::default();
        for e in elements {
            f.push(e);
        }
        Ok(sys::gds_add(self.r(), lib.0, structure, &f.ints, &f.reals, &f.strs)?)
    }

    /// Every structure `roots` reaches, each once: the roots and everything they reference. A
    /// referenced name the library does not hold is returned in the second list.
    fn gds_closure(&self, lib: GdsLib, roots: &BTreeSet<String>) -> Result<(BTreeMap<String, Vec<GdsElement>>, Vec<String>)> {
        let have: BTreeSet<String> = self.gds_structures(lib)?.into_iter().collect();
        let (mut done, mut missing, mut todo) = (BTreeMap::new(), Vec::new(), roots.iter().cloned().collect::<Vec<_>>());
        while let Some(n) = todo.pop() {
            if done.contains_key(&n) || missing.contains(&n) {
                continue;
            }
            if !have.contains(&n) {
                missing.push(n);
                continue;
            }
            let els = self.gds_elements(lib, &n)?;
            for e in &els {
                if let GdsElement::SRef { structure, .. } | GdsElement::ARef { structure, .. } = e {
                    todo.push(structure.clone());
                }
            }
            done.insert(n, els);
        }
        missing.sort();
        Ok((done, missing))
    }
}

/// A GDS stream with every odd-length ASCII record (data type 6) padded with one NUL, its length
/// raised to match. Everything else is copied unchanged; see [`Db::gds_write`].
pub fn pad_odd_strings(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(bytes.len() + bytes.len() / 64);
    let mut i = 0;
    while i < bytes.len() {
        let len = bytes.get(i..i + 4).map(|h| u16::from_be_bytes([h[0], h[1]]) as usize);
        let Some(len) = len.filter(|&l| l >= 4 && i + l <= bytes.len()) else {
            return Err(Error::Odb(format!("gds: malformed record at byte {i}")));
        };
        let rec = &bytes[i..i + len];
        if rec[3] == 0x06 && len % 2 == 1 {
            out.extend_from_slice(&((len + 1) as u16).to_be_bytes());
            out.extend_from_slice(&rec[2..]);
            out.push(0);
        } else {
            out.extend_from_slice(rec);
        }
        i += len;
        // ENDLIB ends the stream; what follows (tape padding) is not records.
        if rec[2] == 0x04 {
            out.extend_from_slice(&bytes[i..]);
            break;
        }
    }
    Ok(out)
}

// ---- the LEF/DEF layer map --------------------------------------------------------------------

/// A KLayout LEF/DEF layer map (`.map`): which GDS layers a LEF/DEF layer and purpose stream to.
///
/// One line is `<layer> <purpose>[,<purpose>...] <gds layer> <gds datatype>`. A layer and purpose
/// may appear on several lines, and then a shape goes to EVERY target: on sky130 a DEF pin on met2
/// lands on 69/20 and on 69/16. `NAME <layer>/<purpose>,... L D` says where label text goes, and
/// `DIEAREA ALL L D` where the die outline goes.
#[derive(Debug, Default, Clone)]
pub struct LayerMap {
    shapes: HashMap<(String, String), Vec<Ld>>,
    names: HashMap<(String, String), Vec<Ld>>,
    diearea: Vec<Ld>,
}

impl LayerMap {
    pub fn parse(text: &str) -> std::result::Result<LayerMap, String> {
        let mut m = LayerMap::default();
        for (n, raw) in text.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let t: Vec<&str> = line.split_whitespace().collect();
            if t.len() < 4 {
                return Err(format!("layer map line {}: expected `<layer> <purposes> <gds layer> <datatype>`: {raw}", n + 1));
            }
            let num = |s: &str| s.parse::<i16>().map_err(|_| format!("layer map line {}: not a number: {s}", n + 1));
            let ld = (num(t[2])?, num(t[3])?);
            match t[0] {
                "NAME" => {
                    for lp in t[1].split(',') {
                        if let Some((l, p)) = lp.split_once('/') {
                            m.names.entry((l.to_string(), p.to_uppercase())).or_default().push(ld);
                        }
                    }
                }
                "DIEAREA" => m.diearea.push(ld),
                layer => {
                    for p in t[1].split(',') {
                        m.shapes.entry((layer.to_string(), p.to_uppercase())).or_default().push(ld);
                    }
                }
            }
        }
        Ok(m)
    }

    /// Where a shape on `layer` with `purpose` goes; empty when the map does not stream it.
    pub fn targets(&self, layer: &str, purpose: &str) -> &[Ld] {
        let k = |p: &str| (layer.to_string(), p.to_string());
        self.shapes.get(&k(purpose)).or_else(|| self.shapes.get(&k("ALL"))).map_or(&[], Vec::as_slice)
    }

    /// Where the label of a shape on `layer` with `purpose` goes.
    pub fn label_targets(&self, layer: &str, purpose: &str) -> &[Ld] {
        self.names.get(&(layer.to_string(), purpose.to_string())).map_or(&[], Vec::as_slice)
    }

    pub fn diearea(&self) -> &[Ld] {
        &self.diearea
    }
}

/// The GDS reference transform for an OpenDB orientation.
///
/// OpenDB's `MX` mirrors about the x axis, `MY` about the y axis, and `MXR90`/`MYR90` mirror then
/// rotate 90. GDS reflects about x first and then rotates, so `MY` is reflect + 180 and `MYR90` is
/// reflect + 270.
pub fn orient_strans(orient: &str) -> Option<STrans> {
    let (flip_x, angle) = match orient {
        "R0" | "N" => (false, 0.0),
        "R90" | "W" => (false, 90.0),
        "R180" | "S" => (false, 180.0),
        "R270" | "E" => (false, 270.0),
        "MX" | "FS" => (true, 0.0),
        "MXR90" | "FW" => (true, 90.0),
        "MY" | "FN" => (true, 180.0),
        "MYR90" | "FE" => (true, 270.0),
        _ => return None,
    };
    Some(STrans { flip_x, mag: 1.0, angle })
}

/// The label point of a pin box: its centre, by integer division as the reference computes it.
pub fn label_point(x0: i32, y0: i32, x1: i32, y1: i32) -> (i32, i32) {
    ((((x0 as i64) + x1 as i64) / 2) as i32, (((y0 as i64) + y1 as i64) / 2) as i32)
}

// ---- stream-out ---------------------------------------------------------------------------------

/// What [`write_gds`] wrote.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct StreamOut {
    /// Structures copied from the cell GDS (the instantiated cells and what they reference).
    pub cells: usize,
    pub instances: usize,
    /// Instances skipped because they are not placed.
    pub unplaced: usize,
    pub shapes: usize,
    pub labels: usize,
}

/// The block in `db` (read with its LEFs and DEF) as GDS at `out`, its cells' contents taken from
/// `cell_gds` (the first file defining a cell wins), its layers from `map`. A cell library or
/// the output named `.oas`/`.oasis` is OASIS ([`crate::oasis`]).
///
/// One stage per step of the reference stream-out, in its order; see the module doc.
pub fn write_gds(db: &mut Db, map: &LayerMap, cell_gds: &[impl AsRef<Path>], out: impl AsRef<Path>) -> Result<StreamOut> {
    let cells = read_cell_gds(db, cell_gds)?;
    let top = db.block_name();
    let lib = create_library(db, &cells, &top)?;
    let mut so = StreamOut { cells: copy_cell_tree(db, &cells, lib)?, ..StreamOut::default() };
    let mut els = Vec::new();
    place_instances(db, &mut els, &mut so);
    stream_routing(db, map, &mut els, &mut so)?;
    stream_pins(db, map, &mut els, &mut so)?;
    stream_die_area(db, map, &mut els, &mut so)?;
    db.gds_create_structure(lib, &top)?;
    db.gds_add(lib, &top, &els)?;
    // An `.oas` output is written by our OASIS writer (odb has none); anything else is GDS.
    db.layout_write(lib, out)?;
    Ok(so)
}

/// The cell libraries, in the order given.
fn read_cell_gds(db: &mut Db, cell_gds: &[impl AsRef<Path>]) -> Result<Vec<GdsLib>> {
    if cell_gds.is_empty() {
        return Err(Error::Odb("write-gds: no cell GDS given".to_string()));
    }
    cell_gds.iter().map(|p| db.layout_read(p)).collect()
}

/// The output library, in the cell GDS's units, which must be the design's.
fn create_library(db: &mut Db, cells: &[GdsLib], top: &str) -> Result<GdsLib> {
    let (uu, m) = db.gds_units(cells[0])?;
    for &c in &cells[1..] {
        let u = db.gds_units(c)?;
        if !same(u.0, uu) || !same(u.1, m) {
            return Err(Error::Odb(format!("write-gds: cell GDS units {u:?} differ from the first's {:?}", (uu, m))));
        }
    }
    let dbu_m = 1e-6 / db.dbu_per_micron() as f64;
    if !same(m, dbu_m) {
        return Err(Error::Odb(format!("write-gds: the cell GDS database unit is {m} m, the design's {dbu_m} m")));
    }
    let lib = db.gds_create_lib(top)?;
    db.gds_set_units(lib, uu, m)?;
    Ok(lib)
}

fn same(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * a.abs().max(b.abs())
}

/// Copy every cell the design instantiates, and what those cells reference, from the cell GDS.
/// A cell no cell GDS defines is refused, as the reference refuses a ghost cell.
fn copy_cell_tree(db: &mut Db, cells: &[GdsLib], lib: GdsLib) -> Result<usize> {
    let mut want: BTreeSet<String> = db.block_get_insts().iter().map(|i| db.inst_get_master(i)).collect();
    let mut found: BTreeMap<String, Vec<GdsElement>> = BTreeMap::new();
    for &c in cells {
        let (got, missing) = db.gds_closure(c, &want)?;
        for (n, els) in got {
            found.entry(n).or_insert(els);
        }
        want = missing.into_iter().collect();
    }
    if !want.is_empty() {
        let w: Vec<String> = want.into_iter().collect();
        return Err(Error::Odb(format!("write-gds: no GDS for cell(s): {}", w.join(", "))));
    }
    for n in found.keys() {
        db.gds_create_structure(lib, n)?;
    }
    for (n, els) in &found {
        db.gds_add(lib, n, els)?;
    }
    Ok(found.len())
}

/// One reference per placed instance, at its transform's origin.
fn place_instances(db: &Db, els: &mut Vec<GdsElement>, so: &mut StreamOut) {
    for i in db.block_get_insts() {
        if matches!(db.inst_get_placement_status(&i).as_str(), "NONE" | "UNPLACED") {
            so.unplaced += 1;
            continue;
        }
        let orient = db.inst_get_orient(&i);
        let trans = orient_strans(&orient).unwrap_or(STrans::IDENTITY);
        let origin = (db.inst_get_origin_x(&i), db.inst_get_origin_y(&i));
        els.push(GdsElement::SRef { structure: db.inst_get_master(&i), origin, trans, props: Vec::new() });
        so.instances += 1;
    }
}

/// Layer number -> (name, is a cut layer).
fn layer_table(db: &Db) -> Result<HashMap<i64, (String, bool)>> {
    let mut t = HashMap::new();
    for i in 0..sys::num_layers(db.r())? {
        let name = sys::nth_layer_name(db.r(), i)?;
        let cut = db.layer_get_type(&name)? == "CUT";
        t.insert(db.layer_get_number(&name) as i64, (name, cut));
    }
    Ok(t)
}

fn stream_box(map: &LayerMap, layers: &HashMap<i64, (String, bool)>, purpose: &str, b: (i64, i32, i32, i32, i32), els: &mut Vec<GdsElement>, so: &mut StreamOut) {
    let Some((name, cut)) = layers.get(&b.0) else { return };
    // A via's shapes are the VIA purpose on every layer it touches; on a cut layer nothing else is.
    let purpose = if *cut { "VIA" } else { purpose };
    for &ld in map.targets(name, purpose) {
        els.push(GdsElement::rect(ld, b.1, b.2, b.3, b.4));
        so.shapes += 1;
    }
}

/// Signal routing (NET) and the special nets (SPNET), vias decomposed onto their layers.
fn stream_routing(db: &Db, map: &LayerMap, els: &mut Vec<GdsElement>, so: &mut StreamOut) -> Result<()> {
    let layers = layer_table(db)?;
    for n in db.block_get_nets() {
        for b in db.net_wire_boxes(&n) {
            let purpose = if b.from_via { "VIA" } else { "NET" };
            stream_box(map, &layers, purpose, (b.layer, b.x0, b.y0, b.x1, b.y1), els, so);
        }
    }
    for b in db.swire_boxes()? {
        stream_box(map, &layers, "SPNET", b, els, so);
    }
    Ok(())
}

/// Every block-pin box on PIN, and the pin's name on its layer's LABEL at the box centre.
fn stream_pins(db: &Db, map: &LayerMap, els: &mut Vec<GdsElement>, so: &mut StreamOut) -> Result<()> {
    let layers = layer_table(db)?;
    for t in db.block_get_b_terms() {
        for p in 0..db.num_bterm_get_b_pins(&t) {
            let boxes = db.bpin_layer_boxes(&t, p)?;
            for &b in &boxes {
                stream_box(map, &layers, "PIN", b, els, so);
            }
            for (name, b) in port_label_boxes(&layers, &boxes) {
                for &(layer, datatype) in map.label_targets(name, "LABEL") {
                    let origin = label_point(b.1, b.2, b.3, b.4);
                    els.push(GdsElement::Text { layer, datatype, origin, presentation: (0, 0), trans: STrans::IDENTITY, text: t.clone(), props: Vec::new() });
                    so.labels += 1;
                }
            }
        }
    }
    Ok(())
}

/// One pin box: layer number, then x0, y0, x1, y1.
type Box4 = (i64, i32, i32, i32, i32);

/// The box each layer of one pin port is labelled at: the LAST box on that layer, one label per
/// layer, layers in name order.
///
/// The KLayout DEF reader collects a port's shapes per layer and, when the port ends, puts one
/// label per layer at the centre of the last shape collected on it (layers keyed by name in an
/// ordered map). A port is an odb `dbBPin`, and odb returns a port's rectangles in the DEF's own
/// order (the reader inserts them reversed, each insert prepends), so "last" is the same box from
/// a DEF read here as from the DEF KLayout reads. KLayout also splits a layer by MASK; the boxes
/// carry none here, so a multi-patterned pin layer would get one label where KLayout writes one
/// per mask.
fn port_label_boxes<'a>(layers: &'a HashMap<i64, (String, bool)>, boxes: &[Box4]) -> Vec<(&'a str, Box4)> {
    let mut last: BTreeMap<&str, Box4> = BTreeMap::new();
    for &b in boxes {
        if let Some((name, _)) = layers.get(&b.0) {
            last.insert(name.as_str(), b);
        }
    }
    last.into_iter().collect()
}

/// The die outline as one polygon on each DIEAREA target.
fn stream_die_area(db: &Db, map: &LayerMap, els: &mut Vec<GdsElement>, so: &mut StreamOut) -> Result<()> {
    let mut xy = db.die_area_polygon()?;
    if xy.is_empty() {
        return Ok(());
    }
    if xy.first() != xy.last() {
        xy.push(xy[0]);
    }
    for &(layer, datatype) in map.diearea() {
        els.push(GdsElement::Boundary { layer, datatype, xy: xy.clone(), props: Vec::new() });
        so.shapes += 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SKY130_LINES: &str = "# comment\n\
        li1     NET,SPNET,VIA            67  20\n\
        mcon    VIA,LEFPIN,PIN           67  44\n\
        met2    LEFPIN,NET,SPNET,PIN,VIA 69  20\n\
        met2    LEFPIN,PIN               69  16\n\
        NAME    met2/LABEL,met2/LEFPIN   69  5\n\
        DIEAREA ALL                      235 4\n";

    /// A purpose listed on two lines streams to both: a sky130 DEF pin on met2 is 69/20 and 69/16.
    #[test]
    fn a_purpose_on_two_lines_streams_to_both_targets() {
        let m = LayerMap::parse(SKY130_LINES).unwrap();
        assert_eq!(m.targets("met2", "PIN"), [(69, 20), (69, 16)]);
        assert_eq!(m.targets("met2", "NET"), [(69, 20)]);
        assert_eq!(m.targets("mcon", "VIA"), [(67, 44)]);
        assert_eq!(m.label_targets("met2", "LABEL"), [(69, 5)]);
        assert_eq!(m.diearea(), [(235, 4)]);
        assert!(m.targets("met2", "FILL").is_empty(), "a purpose the map does not list is not streamed");
        assert!(LayerMap::parse("met1 NET 68\n").is_err());
    }

    /// Every OpenDB orientation maps a cell corner where OpenDB puts it.
    #[test]
    fn strans_places_a_corner_where_opendb_does() {
        // OpenDB's transform of (1, 2) per orientation, before translation.
        let odb = [("R0", (1, 2)), ("R90", (-2, 1)), ("R180", (-1, -2)), ("R270", (2, -1)),
                   ("MX", (1, -2)), ("MY", (-1, 2)), ("MXR90", (2, 1)), ("MYR90", (-2, -1))];
        for (o, want) in odb {
            let t = orient_strans(o).unwrap();
            let (x, y) = if t.flip_x { (1.0, -2.0) } else { (1.0, 2.0) };
            let (s, c) = t.angle.to_radians().sin_cos();
            let got = ((x * c - y * s).round() as i32, (x * s + y * c).round() as i32);
            assert_eq!(got, want, "{o}");
        }
        assert!(orient_strans("bogus").is_none());
    }

    #[test]
    fn a_label_sits_at_the_integer_centre_of_its_box() {
        assert_eq!(label_point(333200, 519515, 333340, 520000), (333270, 519757));
    }

    /// KLayout's DEF reader labels a pin port once per layer, at the LAST shape on that layer, not
    /// once per shape: a supply pin with four met5 straps and four met4 straps gets two labels.
    #[test]
    fn a_port_is_labelled_once_per_layer_at_its_last_box() {
        let layers: HashMap<i64, (String, bool)> =
            [(10, ("met4".to_string(), false)), (11, ("met5".to_string(), false)), (9, ("via3".to_string(), true))].into();
        let boxes = [(11, 0, 0, 10, 2), (11, 0, 20, 10, 22), (10, 0, 0, 2, 30), (99, 0, 0, 1, 1), (10, 8, 0, 10, 30), (11, 0, 40, 10, 42)];
        assert_eq!(port_label_boxes(&layers, &boxes), vec![("met4", (10, 8, 0, 10, 30)), ("met5", (11, 0, 40, 10, 42))]);
        assert!(port_label_boxes(&layers, &[]).is_empty());
    }

    /// An odd-length string gains one NUL and its length says so; even strings, other records and
    /// what follows ENDLIB are untouched.
    #[test]
    fn odd_strings_are_padded_and_nothing_else_moves() {
        let rec = |t: u8, d: u8, data: &[u8]| {
            let mut v = ((4 + data.len()) as u16).to_be_bytes().to_vec();
            v.extend([t, d]);
            v.extend_from_slice(data);
            v
        };
        let mut src = rec(0x06, 0x06, b"abc");
        src.extend(rec(0x06, 0x06, b"ab"));
        src.extend(rec(0x0d, 0x02, &[0, 68]));
        src.extend(rec(0x04, 0x00, &[]));
        src.extend([0u8; 6]);
        let mut want = rec(0x06, 0x06, b"abc\0");
        want.extend(rec(0x06, 0x06, b"ab"));
        want.extend(rec(0x0d, 0x02, &[0, 68]));
        want.extend(rec(0x04, 0x00, &[]));
        want.extend([0u8; 6]);
        assert_eq!(pad_odd_strings(&src).unwrap(), want);
        assert!(pad_odd_strings(&[0, 9, 6, 6, b'a']).is_err(), "a record running past the end is refused");
    }

    /// Every element kind survives the flat encoding the bridge carries.
    #[test]
    fn the_flat_encoding_round_trips_every_kind() {
        let t = STrans { flip_x: true, mag: 2.0, angle: 90.0 };
        let p = vec![(12, "test".to_string())];
        let els = vec![
            GdsElement::rect((68, 20), 0, 0, 10, 20),
            GdsElement::Box { layer: 3, datatype: 4, bounds: (0, 0, 1000, 1000), props: p.clone() },
            GdsElement::Path { layer: 68, datatype: 20, path_type: 2, width: 140, xy: vec![(0, 0), (500, 0)], props: vec![] },
            GdsElement::SRef { structure: "a".into(), origin: (5, -7), trans: t, props: p.clone() },
            GdsElement::ARef { structure: "b".into(), origin: (1, 2), lr: (30, 2), ul: (1, 40), rows: 4, columns: 3, trans: t, props: vec![] },
            GdsElement::Text { layer: 69, datatype: 5, origin: (9, 9), presentation: (1, 2), trans: t, text: "Y".into(), props: p },
        ];
        let mut f = Flat::default();
        for e in &els {
            f.push(e);
        }
        let mut u = Unflat { ints: &f.ints, reals: &f.reals, strs: &f.strs, i: 0, r: 0, s: 0 };
        let back: Vec<GdsElement> = (0..els.len()).map(|_| u.element().unwrap()).collect();
        assert_eq!(back, els);
        assert_eq!((u.i, u.r, u.s), (f.ints.len(), f.reals.len(), f.strs.len()), "nothing left over");
        let mut short = Unflat { ints: &f.ints[..5], reals: &f.reals, strs: &f.strs, i: 0, r: 0, s: 0 };
        assert!(short.element().is_err(), "a truncated encoding is an error, not a short element");
    }
}
