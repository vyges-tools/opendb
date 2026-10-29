// SPDX-License-Identifier: Apache-2.0
//! OASIS (SEMI P39) over the GDS element model: [`encode`] writes a layout, [`decode`] reads one.
//!
//! odb has no OASIS writer or reader, so this is our own code. The writer follows KLayout's
//! (`dbOASISWriter`) in its defaults and its order of work, so that KLayout can serve as the
//! independent check on every file it writes: strict mode, CBLOCK compression, the five
//! standard property names given ids before anything else, the property-string and text-string
//! tables at the front, the cells bottom-up, and the cell-name table at the end carrying each
//! cell's `S_CELL_OFFSET`.
//!
//! ⚠️ **What OASIS cannot carry, and so the round trip GDS → OASIS → GDS does not keep:**
//! - a text's presentation and transform (OASIS TEXT has neither), and its size;
//! - a character a text string cannot hold: text is an a-string (printable ASCII), so a newline
//!   or anything else outside it is written as `*`, as KLayout writes it;
//! - a BOX record as distinct from a rectangular BOUNDARY (both are a RECTANGLE);
//! - a round-ended path, which is written as a flush path and a CIRCLE at each end;
//! - which AREF axis was the column one, when one of the counts is 1.
//!
//! ⛔ **Refused, as KLayout refuses outside its permissive mode:** a path of odd width and a
//! polygon of fewer than three points. OASIS stores a path's half-width, so an odd width has no
//! encoding, and a silently rounded width is a different layout.

use crate::gds::{GdsElement, Prop, STrans};
use std::collections::HashMap;

/// A layout to write, or one read: the grid (database units per micron) and the cells.
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    pub unit: f64,
    pub cells: Vec<Cell>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Cell {
    pub name: String,
    pub elements: Vec<GdsElement>,
}

#[derive(Debug, Clone, Copy)]
pub struct WriteOptions {
    /// 0: identical shapes gathered into irregular repetitions only; 1: also runs of three or
    /// more at one pitch along an axis, stacked into 2-D arrays where they line up.
    pub compression_level: u8,
    /// Deflate each cell and each table into a CBLOCK.
    pub cblocks: bool,
}

impl Default for WriteOptions {
    fn default() -> Self {
        WriteOptions { compression_level: 1, cblocks: true }
    }
}

const MAGIC: &[u8] = b"%SEMI-OASIS\r\n";
/// A CBLOCK is closed and a new one opened once its buffer passes this, at a record boundary.
const CBLOCK_BUFFER_SIZE: usize = 1024 * 1024;
/// A CBLOCK is written only if it saves more than its own header costs.
const COMPRESSION_OVERHEAD: usize = 4;

const S_CELL_OFFSET: &str = "S_CELL_OFFSET";
const S_GDS_PROPERTY: &str = "S_GDS_PROPERTY";
const S_MAX_SIGNED_INTEGER_WIDTH: &str = "S_MAX_SIGNED_INTEGER_WIDTH";
const S_MAX_UNSIGNED_INTEGER_WIDTH: &str = "S_MAX_UNSIGNED_INTEGER_WIDTH";
const S_TOP_CELL: &str = "S_TOP_CELL";
/// In strict mode these names are given ids 0.. in this order before anything is written.
const STD_PROPNAMES: [&str; 5] =
    [S_CELL_OFFSET, S_GDS_PROPERTY, S_MAX_SIGNED_INTEGER_WIDTH, S_MAX_UNSIGNED_INTEGER_WIDTH, S_TOP_CELL];

type P = (i64, i64);

fn add(a: P, b: P) -> P {
    (a.0 + b.0, a.1 + b.1)
}

fn sub(a: P, b: P) -> P {
    (a.0 - b.0, a.1 - b.1)
}

/// A repetition, the origin excluded: a lattice `i·a + j·b` (`i < n`, `j < m`), or a list of
/// displacements from the origin.
#[derive(Debug, Clone, PartialEq)]
enum Rep {
    Regular { a: P, b: P, n: u64, m: u64 },
    Irregular(Vec<P>),
}

impl Rep {
    /// Every displacement, the origin first.
    fn points(&self) -> Vec<P> {
        match self {
            Rep::Regular { a, b, n, m } => {
                let mut v = Vec::with_capacity((n * m) as usize);
                for j in 0..*m as i64 {
                    for i in 0..*n as i64 {
                        v.push((i * a.0 + j * b.0, i * a.1 + j * b.1));
                    }
                }
                v
            }
            Rep::Irregular(pts) => std::iter::once((0, 0)).chain(pts.iter().copied()).collect(),
        }
    }
}

/// A property value as written: the two kinds a GDS property needs.
#[derive(Debug, Clone, PartialEq)]
enum PropVal {
    UInt(u64),
    Str(String),
}

// ---------------------------------------------------------------------------------------------
// Primitive encoders

/// Bytes going out, in chunks: each chunk is one CBLOCK (or plain bytes when not compressed).
#[derive(Default)]
struct Sink {
    chunks: Vec<Vec<u8>>,
    /// Split into a new chunk at a record boundary once the current one passes the CBLOCK size.
    split: bool,
}

impl Sink {
    fn cur(&mut self) -> &mut Vec<u8> {
        if self.chunks.is_empty() {
            self.chunks.push(Vec::new());
        }
        self.chunks.last_mut().unwrap()
    }
    fn record_id(&mut self, id: u8) {
        if self.split && self.cur().len() > CBLOCK_BUFFER_SIZE {
            self.chunks.push(Vec::new());
        }
        self.cur().push(id);
    }
    fn byte(&mut self, b: u8) {
        self.cur().push(b);
    }
    fn bytes(&mut self, b: &[u8]) {
        self.cur().extend_from_slice(b);
    }
    fn len(&self) -> usize {
        self.chunks.iter().map(Vec::len).sum()
    }
    /// Unsigned integer: seven bits a byte, least significant first, high bit set on all but the last.
    fn uint(&mut self, mut n: u64) {
        loop {
            let mut b = (n & 0x7f) as u8;
            n >>= 7;
            if n > 0 {
                b |= 0x80;
            }
            self.byte(b);
            if n == 0 {
                break;
            }
        }
    }
    /// Signed integer: the magnitude shifted up one, the sign in the low bit.
    fn sint(&mut self, n: i64) {
        if n < 0 {
            self.uint((n.unsigned_abs() << 1) | 1);
        } else {
            self.uint((n as u64) << 1);
        }
    }
    /// Real: a whole number of magnitude at least 0.5 as type 0 (positive) or 1 (negative),
    /// anything else as type 7, an 8-byte IEEE double.
    fn real(&mut self, d: f64) {
        if d.abs() >= 0.5 && ((d + 0.5).floor() - d).abs() < 1e-10 && d.abs() < i64::MAX as f64 {
            if d < 0.0 {
                self.byte(1);
                self.uint((-d + 0.5).floor() as u64);
            } else {
                self.byte(0);
                self.uint((d + 0.5).floor() as u64);
            }
        } else {
            self.byte(7);
            self.bytes(&d.to_bits().to_le_bytes());
        }
    }
    fn string(&mut self, s: &str) {
        self.uint(s.len() as u64);
        self.bytes(s.as_bytes());
    }
    /// g-delta: along one of the eight octangular directions as one integer `(len << 4) |
    /// (dir << 1)` (0 E, 1 N, 2 W, 3 S, 4 NE, 5 NW, 6 SW, 7 SE); otherwise `(|x| << 2) | 1`
    /// with bit 1 the sign of x, then y signed.
    fn gdelta(&mut self, (x, y): P) {
        if x == -y || x == y || x == 0 || y == 0 {
            let (l, dir) = if x > 0 {
                (x, if y == 0 { 0 } else if y < 0 { 7 } else { 4 })
            } else if x == 0 {
                if y < 0 { (-y, 3) } else { (y, 1) }
            } else {
                (-x, if y == 0 { 2 } else if y < 0 { 6 } else { 5 })
            };
            self.uint(((l as u64) << 4) | (dir << 1));
        } else {
            let d = if x < 0 { (x.unsigned_abs() << 2) | 3 } else { ((x as u64) << 2) | 1 };
            self.uint(d);
            self.sint(y);
        }
    }
}

fn gcd(a: i64, b: i64) -> i64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// The string type of a property value: 0 an a-string, 1 a b-string, 2 an n-string. An empty
/// string is an a-string; a space makes an a-string of what would be an n-string; anything
/// unprintable makes a b-string.
fn string_type(s: &str) -> u8 {
    if s.is_empty() {
        return 0;
    }
    let mut is_nstring = true;
    for c in s.bytes() {
        if c == 0x20 {
            is_nstring = false;
        } else if !(0x20..=0x7e).contains(&c) {
            return 1;
        }
    }
    if is_nstring { 2 } else { 0 }
}

/// An n-string (cell and property names) or a-string (text) with each character it may not
/// hold replaced by `*`: a space in an n-string, anything unprintable in either.
fn n_or_astring(s: &str, nstring: bool) -> String {
    if nstring && s.is_empty() {
        return "*".into();
    }
    s.chars().map(|c| if (c == ' ' && nstring) || !(' '..='~').contains(&c) { '*' } else { c }).collect()
}

// ---------------------------------------------------------------------------------------------
// The compressor

/// Points ordered by y, then x (KLayout `vector_cmp_x`: runs along x cluster together).
fn cmp_x(a: &P, b: &P) -> std::cmp::Ordering {
    (a.1, a.0).cmp(&(b.1, b.0))
}

/// Points ordered by x, then y (`vector_cmp_y`).
fn cmp_y(a: &P, b: &P) -> std::cmp::Ordering {
    a.cmp(b)
}

/// The placements of one shape: each base point to write it at, with the repetition there.
///
/// ⛔ **No repetition holds a displacement of zero.** KLayout's reader drops a zero step from an
/// irregular repetition and collapses a zero-pitch lattice to one copy, so a copy written as one
/// is a copy KLayout does not see (two identical pin labels at one point, in sky130 cells). So
/// the k-th copy of each point goes through its own pass, and within a pass every point is new.
fn compress(level: u8, disps: Vec<P>) -> Vec<(P, Option<Rep>)> {
    let mut seen: HashMap<P, usize> = HashMap::new();
    let mut passes: Vec<Vec<P>> = Vec::new();
    for d in disps {
        let k = seen.entry(d).or_insert(0);
        if *k == passes.len() {
            passes.push(Vec::new());
        }
        passes[*k].push(d);
        *k += 1;
    }
    passes.into_iter().flat_map(|p| compress_distinct(level, p)).collect()
}

/// [`compress`] over distinct points.
///
/// Level 0, or fewer than ten copies: one irregular repetition over all of them, sorted.
/// Level 1: first runs of three or more at one pitch in y, then in x over what is left; runs of
/// equal pitch and count stacked in the other axis become 2-D arrays; the rest one irregular
/// repetition. (KLayout's `Compressor::flush` at level 1; its default level 2 also searches
/// second and further neighbours and weighs a cost, which this does not.)
fn compress_distinct(level: u8, mut disps: Vec<P>) -> Vec<(P, Option<Rep>)> {
    let mut out = Vec::new();
    if level < 1 || disps.len() < 10 {
        disps.sort_by(cmp_x);
    } else {
        // Level 1 counts no distinct coordinates, so y runs come first.
        let xfirst = false;
        for xypass in 0..2 {
            let xrep = xfirst == (xypass == 0);
            let mut d = std::mem::take(&mut disps);
            d.sort_by(if xrep { cmp_x } else { cmp_y });
            let mut reps: Vec<(P, (i64, u64))> = Vec::new();
            let mut i = 0;
            while i < d.len() {
                let mut j = i + 1;
                let mut dxy = (0, 0);
                let mut nxy = 1u64;
                if j < d.len() {
                    dxy = if xrep { (d[j].0 - d[i].0, 0) } else { (0, d[j].1 - d[i].1) };
                    while j < d.len() && d[j] == add(d[j - 1], dxy) {
                        j += 1;
                        nxy += 1;
                    }
                }
                // No cost is weighed at level 1, so a run must be three long to be worth it.
                if nxy < 3 {
                    disps.push(d[i]);
                    i += 1;
                } else {
                    reps.push((d[i], (if xrep { dxy.0 } else { dxy.1 }, nxy)));
                    i = j;
                }
            }
            // Stack the runs: in y first, keeping single runs for the x pass, which writes all.
            for xypass2 in [true, false] {
                reps.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| if xypass2 { cmp_y(&a.0, &b.0) } else { cmp_x(&a.0, &b.0) }));
                let mut kept = Vec::new();
                let mut r = 0;
                while r < reps.len() {
                    let mut rr = r + 1;
                    let mut dxy2 = (0, 0);
                    if rr < reps.len() {
                        dxy2 = if xypass2 { (0, reps[rr].0 .1 - reps[r].0 .1) } else { (reps[rr].0 .0 - reps[r].0 .0, 0) };
                    }
                    let mut nxy2 = 1u64;
                    let mut dxy2n = dxy2;
                    while rr < reps.len() && reps[rr].1 == reps[r].1 && reps[rr].0 == add(reps[r].0, dxy2n) {
                        nxy2 += 1;
                        rr += 1;
                        dxy2n = add(dxy2n, dxy2);
                    }
                    if nxy2 < 2 && xypass2 {
                        kept.push(reps[r]);
                    } else {
                        let (pitch, n) = reps[r].1;
                        let a = if xrep { (pitch, 0) } else { (0, pitch) };
                        out.push((reps[r].0, Some(Rep::Regular { a, b: dxy2, n, m: nxy2 })));
                    }
                    r = rr;
                }
                reps = kept;
            }
        }
    }
    if disps.len() > 1 {
        let p0 = disps[0];
        out.push((p0, Some(Rep::Irregular(disps[1..].iter().map(|&p| sub(p, p0)).collect()))));
    } else if let Some(&p) = disps.first() {
        out.push((p, None));
    }
    out
}

// ---------------------------------------------------------------------------------------------
// The writer

/// The modal variables, reset at each cell (and before each cell-name property).
#[derive(Default)]
struct Modal {
    repetition: Option<Rep>,
    placement_cell: Option<u64>,
    placement_x: i64,
    placement_y: i64,
    layer: Option<u64>,
    datatype: Option<u64>,
    textlayer: Option<u64>,
    texttype: Option<u64>,
    text_x: i64,
    text_y: i64,
    text_string: Option<String>,
    geometry_x: i64,
    geometry_y: i64,
    geometry_w: Option<i64>,
    geometry_h: Option<i64>,
    polygon_point_list: Option<Vec<P>>,
    path_halfwidth: Option<i64>,
    path_start_extension: Option<i64>,
    path_end_extension: Option<i64>,
    path_point_list: Option<Vec<P>>,
    circle_radius: Option<i64>,
    last_property_name: Option<String>,
    last_property_is_sprop: Option<bool>,
    last_value_list: Option<Vec<PropVal>>,
}

struct Writer {
    sink: Sink,
    m: Modal,
    /// The layer and datatype of the shapes being written.
    layer: u64,
    datatype: u64,
    propnames: HashMap<String, u64>,
    propstrings: HashMap<String, u64>,
    propstring_order: Vec<String>,
    textstrings: HashMap<String, u64>,
    textstring_order: Vec<String>,
    /// For error messages: the cell being written.
    cell: String,
}

impl Writer {
    fn new(split: bool) -> Writer {
        Writer {
            sink: Sink { chunks: Vec::new(), split },
            m: Modal::default(),
            layer: 0,
            datatype: 0,
            propnames: STD_PROPNAMES.iter().enumerate().map(|(i, n)| (n.to_string(), i as u64)).collect(),
            propstrings: HashMap::new(),
            propstring_order: Vec::new(),
            textstrings: HashMap::new(),
            textstring_order: Vec::new(),
            cell: String::new(),
        }
    }

    fn reset_modal_variables(&mut self) {
        self.m = Modal::default();
    }

    fn propstring_id(&mut self, s: &str) -> u64 {
        if let Some(&id) = self.propstrings.get(s) {
            return id;
        }
        let id = self.propstring_order.len() as u64;
        self.propstrings.insert(s.to_string(), id);
        self.propstring_order.push(s.to_string());
        id
    }

    fn textstring_id(&mut self, s: &str) -> u64 {
        if let Some(&id) = self.textstrings.get(s) {
            return id;
        }
        let id = self.textstring_order.len() as u64;
        self.textstrings.insert(s.to_string(), id);
        self.textstring_order.push(s.to_string());
        id
    }

    fn write_repetition(&mut self, rep: &Rep) {
        if self.m.repetition.as_ref() == Some(rep) {
            self.sink.byte(0); // reuse
            return;
        }
        self.m.repetition = Some(rep.clone());
        match rep {
            Rep::Irregular(pts) => {
                let mut g = 0;
                for &(x, y) in pts {
                    for c in [x.abs(), y.abs()] {
                        if c != 0 {
                            g = if g == 0 { c } else { gcd(g, c) };
                        }
                    }
                }
                if g <= 1 {
                    self.sink.byte(10);
                    self.sink.uint(pts.len() as u64 - 1);
                    g = 1;
                } else {
                    self.sink.byte(11);
                    self.sink.uint(pts.len() as u64 - 1);
                    self.sink.uint(g as u64);
                }
                let mut last = (0, 0);
                for &p in pts {
                    let d = sub(p, last);
                    last = p;
                    self.sink.gdelta((d.0 / g, d.1 / g));
                }
            }
            &Rep::Regular { a, b, n, m } => {
                let (mut a, mut b, mut amax, mut bmax) = (a, b, n, m);
                if amax == 1 || bmax == 1 {
                    if bmax == 1 {
                        b = a;
                        bmax = amax;
                    }
                    if b.0 == 0 && b.1 >= 0 {
                        self.sink.byte(3);
                        self.sink.uint(bmax - 2);
                        self.sink.uint(b.1 as u64);
                    } else if b.1 == 0 && b.0 >= 0 {
                        self.sink.byte(2);
                        self.sink.uint(bmax - 2);
                        self.sink.uint(b.0 as u64);
                    } else {
                        self.sink.byte(9);
                        self.sink.uint(bmax - 2);
                        self.sink.gdelta(b);
                    }
                } else if b.0 == 0 && b.1 >= 0 && a.1 == 0 && a.0 >= 0 {
                    self.sink.byte(1);
                    self.sink.uint(amax - 2);
                    self.sink.uint(bmax - 2);
                    self.sink.uint(a.0 as u64);
                    self.sink.uint(b.1 as u64);
                } else if b.1 == 0 && b.0 >= 0 && a.0 == 0 && a.1 >= 0 {
                    std::mem::swap(&mut a, &mut b);
                    std::mem::swap(&mut amax, &mut bmax);
                    self.sink.byte(1);
                    self.sink.uint(amax - 2);
                    self.sink.uint(bmax - 2);
                    self.sink.uint(a.0 as u64);
                    self.sink.uint(b.1 as u64);
                } else {
                    self.sink.byte(8);
                    self.sink.uint(amax - 2);
                    self.sink.uint(bmax - 2);
                    self.sink.gdelta(a);
                    self.sink.gdelta(b);
                }
            }
        }
    }

    /// A point list, relative to the element's start and excluding it. Manhattan lists that
    /// alternate starting horizontal (type 0) or vertical (type 1) are written as one signed
    /// length per edge, and for a polygon the last point is implied by the closing edges;
    /// anything else is type 4, one g-delta per point.
    fn write_pointlist(&mut self, pointlist: &[P], for_polygons: bool) {
        let mut plast = (0, 0);
        let mut ty: i32 = -1;
        let mut hvlast: i32 = -1;
        for p in pointlist {
            let hv = if p.0 == plast.0 {
                1
            } else if p.1 == plast.1 {
                0
            } else {
                ty = -1;
                break;
            };
            if ty == -1 {
                ty = hv;
            } else if hv != (hvlast == 0) as i32 {
                ty = -1;
                break;
            }
            hvlast = hv;
            plast = *p;
        }
        // A polygon's closing edge must be the one its last edge turns into.
        if for_polygons && ty >= 0 {
            if hvlast != ty {
                ty = -1;
            } else if plast.0 == 0 {
                if hvlast != 0 {
                    ty = -1;
                }
            } else if plast.1 == 0 {
                if hvlast != 1 {
                    ty = -1;
                }
            } else {
                ty = -1;
            }
        }
        if ty == 0 || ty == 1 {
            self.sink.byte(ty as u8);
            let implicit = for_polygons as usize;
            self.sink.uint((pointlist.len() - implicit) as u64);
            let mut plast = (0, 0);
            for p in &pointlist[..pointlist.len() - implicit] {
                let mut d = p.0 - plast.0;
                if d == 0 {
                    d = p.1 - plast.1;
                }
                self.sink.sint(d);
                plast = *p;
            }
        } else {
            self.sink.byte(4);
            self.sink.uint(pointlist.len() as u64);
            let mut plast = (0, 0);
            for p in pointlist {
                self.sink.gdelta(sub(*p, plast));
                plast = *p;
            }
        }
    }

    fn write_props(&mut self, props: &[Prop]) {
        let mut sorted: Vec<&Prop> = props.iter().collect();
        sorted.sort_by_key(|p| p.0);
        for (attr, value) in sorted {
            let pvl = vec![PropVal::UInt(*attr as u16 as u64), PropVal::Str(value.clone())];
            self.write_property_def(S_GDS_PROPERTY, &pvl, true);
        }
    }

    /// PROPERTY, or PROPERTY-REPEAT when name, values and standard flag all repeat the last.
    fn write_property_def(&mut self, name: &str, pvl: &[PropVal], sflag: bool) {
        let same_name = self.m.last_property_name.as_deref() == Some(name);
        let same_value = self.m.last_value_list.as_deref() == Some(pvl);
        let same_sflag = self.m.last_property_is_sprop == Some(sflag);
        if same_name && same_value && same_sflag {
            self.sink.record_id(29);
            return;
        }
        self.sink.record_id(28);
        let mut info = sflag as u8;
        if same_value {
            info |= 0x08;
        } else if pvl.len() >= 15 {
            info |= 0xf0;
        } else {
            info |= (pvl.len() as u8) << 4;
        }
        if !same_name {
            // Strict mode: every name is written by id; only the standard ones are used.
            let id = self.propnames[name];
            self.sink.byte(info | 0x06);
            self.sink.uint(id);
            self.m.last_property_name = Some(name.to_string());
        } else {
            self.sink.byte(info);
        }
        if !same_value {
            if pvl.len() >= 15 {
                self.sink.uint(pvl.len() as u64);
            }
            for v in pvl {
                match v {
                    PropVal::UInt(u) => {
                        self.sink.byte(8);
                        self.sink.uint(*u);
                    }
                    PropVal::Str(s) => {
                        let id = self.propstring_id(s);
                        self.sink.byte(13 + string_type(s));
                        self.sink.uint(id);
                    }
                }
            }
            self.m.last_value_list = Some(pvl.to_vec());
        }
        self.m.last_property_is_sprop = Some(sflag);
    }

    fn write_box(&mut self, (x, y): P, (w, h): P, props: &[Prop], rep: Option<&Rep>) {
        let mut info = 0u8;
        if self.m.layer != Some(self.layer) {
            info |= 0x01;
        }
        if self.m.datatype != Some(self.datatype) {
            info |= 0x02;
        }
        if w == h {
            info |= 0x80; // square
        } else if self.m.geometry_h != Some(h) {
            info |= 0x20;
        }
        if self.m.geometry_w != Some(w) {
            info |= 0x40;
        }
        if self.m.geometry_x != x {
            info |= 0x10;
        }
        if self.m.geometry_y != y {
            info |= 0x08;
        }
        if rep.is_some() {
            info |= 0x04;
        }
        self.sink.record_id(20);
        self.sink.byte(info);
        self.layer_datatype(info);
        self.m.geometry_w = Some(w);
        self.m.geometry_h = Some(h);
        if info & 0x40 != 0 {
            self.sink.uint(w as u64);
        }
        if info & 0x20 != 0 {
            self.sink.uint(h as u64);
        }
        self.geometry_xy(info, x, y);
        self.tail(info, props, rep);
    }

    fn write_polygon(&mut self, start: P, pointlist: Vec<P>, props: &[Prop], rep: Option<&Rep>) -> Result<(), String> {
        if pointlist.len() < 2 {
            return Err(format!(
                "polygons with less than three points cannot be written to OASIS files (cell {}, position {}, {} DBU)",
                self.cell, start.0, start.1
            ));
        }
        let mut info = 0u8;
        if self.m.layer != Some(self.layer) {
            info |= 0x01;
        }
        if self.m.datatype != Some(self.datatype) {
            info |= 0x02;
        }
        if self.m.geometry_x != start.0 {
            info |= 0x10;
        }
        if self.m.geometry_y != start.1 {
            info |= 0x08;
        }
        if self.m.polygon_point_list.as_ref() != Some(&pointlist) {
            info |= 0x20;
        }
        if rep.is_some() {
            info |= 0x04;
        }
        self.sink.record_id(21);
        self.sink.byte(info);
        self.layer_datatype(info);
        if info & 0x20 != 0 {
            self.write_pointlist(&pointlist, true);
            self.m.polygon_point_list = Some(pointlist);
        }
        self.geometry_xy(info, start.0, start.1);
        self.tail(info, props, rep);
        Ok(())
    }

    fn write_circle(&mut self, (x, y): P, r: i64, props: &[Prop], rep: Option<&Rep>) {
        let mut info = 0u8;
        if self.m.layer != Some(self.layer) {
            info |= 0x01;
        }
        if self.m.datatype != Some(self.datatype) {
            info |= 0x02;
        }
        if self.m.circle_radius != Some(r) {
            info |= 0x20;
        }
        if self.m.geometry_x != x {
            info |= 0x10;
        }
        if self.m.geometry_y != y {
            info |= 0x08;
        }
        if rep.is_some() {
            info |= 0x04;
        }
        self.sink.record_id(27);
        self.sink.byte(info);
        self.layer_datatype(info);
        if info & 0x20 != 0 {
            self.m.circle_radius = Some(r);
            self.sink.uint(r as u64);
        }
        self.geometry_xy(info, x, y);
        self.tail(info, props, rep);
    }

    /// A path: GDS path type 0 (flush) and 4 (odb keeps no extension values, so flush) have no
    /// extension, type 2 a half-width one; type 1 (round) is a flush path with a circle at each
    /// end. A single-point path is a circle (round), a square (half-width ends) or nothing.
    fn write_path(&mut self, path_type: i16, width: i64, xy: &[P], props: &[Prop], rep: Option<&Rep>) -> Result<(), String> {
        let Some(&start) = xy.first() else { return Ok(()) };
        let round = path_type == 1;
        let hw = width / 2;
        if hw * 2 != width {
            return Err(format!(
                "paths with odd width cannot be written to OASIS files (cell {}, position {}, {} DBU)",
                self.cell, start.0, start.1
            ));
        }
        let ext = if path_type == 2 { (hw, hw) } else { (0, 0) };
        let pointlist: Vec<P> = xy[1..].iter().map(|&p| sub(p, start)).collect();
        if pointlist.is_empty() {
            if round {
                self.write_circle(start, hw, props, rep);
            } else if ext.0 > 0 {
                self.write_box((start.0 - hw, start.1 - hw), (width, width), props, rep);
            }
            return Ok(());
        }
        let end = add(start, *pointlist.last().unwrap());
        let mut info = 0u8;
        if self.m.layer != Some(self.layer) {
            info |= 0x01;
        }
        if self.m.datatype != Some(self.datatype) {
            info |= 0x02;
        }
        if self.m.geometry_x != start.0 {
            info |= 0x10;
        }
        if self.m.geometry_y != start.1 {
            info |= 0x08;
        }
        if self.m.path_point_list.as_ref() != Some(&pointlist) {
            info |= 0x20;
        }
        if self.m.path_start_extension != Some(ext.0) || self.m.path_end_extension != Some(ext.1) {
            info |= 0x80;
        }
        if self.m.path_halfwidth != Some(hw) {
            info |= 0x40;
        }
        if rep.is_some() {
            info |= 0x04;
        }
        self.sink.record_id(22);
        self.sink.byte(info);
        self.layer_datatype(info);
        if info & 0x40 != 0 {
            self.m.path_halfwidth = Some(hw);
            self.sink.uint(hw as u64);
        }
        if info & 0x80 != 0 {
            let mut scheme = 0u8;
            if self.m.path_start_extension == Some(ext.0) {
            } else if ext.0 == 0 {
                scheme |= 0x04;
            } else if ext.0 == hw {
                scheme |= 0x08;
            } else {
                scheme |= 0x0c;
            }
            if self.m.path_end_extension == Some(ext.1) {
            } else if ext.1 == 0 {
                scheme |= 0x01;
            } else if ext.1 == hw {
                scheme |= 0x02;
            } else {
                scheme |= 0x03;
            }
            self.sink.byte(scheme);
            if scheme & 0x0c == 0x0c {
                self.sink.sint(ext.0);
            }
            if scheme & 0x03 == 0x03 {
                self.sink.sint(ext.1);
            }
            self.m.path_start_extension = Some(ext.0);
            self.m.path_end_extension = Some(ext.1);
        }
        if info & 0x20 != 0 {
            self.write_pointlist(&pointlist, false);
            self.m.path_point_list = Some(pointlist);
        }
        self.geometry_xy(info, start.0, start.1);
        self.tail(info, props, rep);
        if round {
            self.write_circle(start, hw, props, rep);
            self.write_circle(end, hw, props, rep);
        }
        Ok(())
    }

    fn write_text(&mut self, text: &str, (x, y): P, props: &[Prop], rep: Option<&Rep>) {
        let id = self.textstring_id(text);
        let mut info = 0x20u8; // by reference number
        if self.m.text_string.as_deref() != Some(text) {
            info |= 0x40;
        }
        if self.m.textlayer != Some(self.layer) {
            info |= 0x01;
        }
        if self.m.texttype != Some(self.datatype) {
            info |= 0x02;
        }
        if self.m.text_x != x {
            info |= 0x10;
        }
        if self.m.text_y != y {
            info |= 0x08;
        }
        if rep.is_some() {
            info |= 0x04;
        }
        self.sink.record_id(19);
        self.sink.byte(info);
        if info & 0x40 != 0 {
            self.m.text_string = Some(text.to_string());
            self.sink.uint(id);
        }
        if info & 0x01 != 0 {
            self.m.textlayer = Some(self.layer);
            self.sink.uint(self.layer);
        }
        if info & 0x02 != 0 {
            self.m.texttype = Some(self.datatype);
            self.sink.uint(self.datatype);
        }
        if info & 0x10 != 0 {
            self.m.text_x = x;
            self.sink.sint(x);
        }
        if info & 0x08 != 0 {
            self.m.text_y = y;
            self.sink.sint(y);
        }
        self.tail(info, props, rep);
    }

    /// PLACEMENT, by cell reference number; record 18 with magnification and angle when the
    /// transform is not a multiple of 90 degrees at unit magnification.
    fn write_placement(&mut self, cell: u64, t: &STrans, (x, y): P, props: &[Prop], rep: Option<&Rep>) {
        let (complex, rot) = trans_kind(t);
        let mut info = 0x40u8; // by reference number
        if self.m.placement_cell != Some(cell) {
            info |= 0x80;
        }
        if self.m.placement_x != x {
            info |= 0x20;
        }
        if self.m.placement_y != y {
            info |= 0x10;
        }
        if rep.is_some() {
            info |= 0x08;
        }
        if t.flip_x {
            info |= 0x01;
        }
        if complex {
            self.sink.record_id(18);
            self.sink.byte(info | 0x06);
        } else {
            self.sink.record_id(17);
            self.sink.byte(info | ((rot & 0x03) << 1));
        }
        if info & 0x80 != 0 {
            self.m.placement_cell = Some(cell);
            self.sink.uint(cell);
        }
        if complex {
            self.sink.real(t.mag);
            self.sink.real(t.angle);
        }
        if info & 0x20 != 0 {
            self.m.placement_x = x;
            self.sink.sint(x);
        }
        if info & 0x10 != 0 {
            self.m.placement_y = y;
            self.sink.sint(y);
        }
        if let Some(r) = rep {
            self.write_repetition(r);
        }
        if !props.is_empty() {
            self.write_props(props);
        }
    }

    fn layer_datatype(&mut self, info: u8) {
        if info & 0x01 != 0 {
            self.m.layer = Some(self.layer);
            self.sink.uint(self.layer);
        }
        if info & 0x02 != 0 {
            self.m.datatype = Some(self.datatype);
            self.sink.uint(self.datatype);
        }
    }

    fn geometry_xy(&mut self, info: u8, x: i64, y: i64) {
        if info & 0x10 != 0 {
            self.m.geometry_x = x;
            self.sink.sint(x);
        }
        if info & 0x08 != 0 {
            self.m.geometry_y = y;
            self.sink.sint(y);
        }
    }

    fn tail(&mut self, info: u8, props: &[Prop], rep: Option<&Rep>) {
        if info & 0x04 != 0 {
            self.write_repetition(rep.unwrap());
        }
        if !props.is_empty() {
            self.write_props(props);
        }
    }
}

/// `(complex, rotation in quarter turns)`: complex when magnified or turned off the right angles.
fn trans_kind(t: &STrans) -> (bool, u8) {
    let q = t.angle / 90.0;
    let rq = q.round();
    if (t.mag - 1.0).abs() > 1e-10 || (q - rq).abs() > 1e-10 {
        (true, 0)
    } else {
        (false, (rq as i64).rem_euclid(4) as u8)
    }
}

/// A rectangle given as a closed BOUNDARY: `(x0, y0, w, h)`.
fn as_box(xy: &[(i32, i32)]) -> Option<(P, P)> {
    let hull = hull(xy);
    if hull.len() != 4 {
        return None;
    }
    let (x0, x1) = (hull.iter().map(|p| p.0).min()?, hull.iter().map(|p| p.0).max()?);
    let (y0, y1) = (hull.iter().map(|p| p.1).min()?, hull.iter().map(|p| p.1).max()?);
    if x0 == x1 || y0 == y1 {
        return None;
    }
    for i in 0..4 {
        let (a, b) = (hull[i], hull[(i + 1) % 4]);
        if a.0 != b.0 && a.1 != b.1 {
            return None;
        }
        if !(a.0 == x0 || a.0 == x1) || !(a.1 == y0 || a.1 == y1) {
            return None;
        }
    }
    Some(((x0, y0), (x1 - x0, y1 - y0)))
}

/// A BOUNDARY's points without the repeated closing one.
fn hull(xy: &[(i32, i32)]) -> Vec<P> {
    let mut v: Vec<P> = xy.iter().map(|&(x, y)| (x as i64, y as i64)).collect();
    if v.len() > 1 && v.first() == v.last() {
        v.pop();
    }
    v
}

/// A shape normalized to its own origin, which the compressor gathers identical copies of.
#[derive(Clone, PartialEq, Eq, Hash)]
enum Shape {
    Path { path_type: i16, width: i64, pts: Vec<P> },
    Polygon(Vec<P>),
    Box(P),
    Text(String),
}

impl Shape {
    fn rank(&self) -> u8 {
        match self {
            Shape::Path { .. } => 0,
            Shape::Polygon(_) => 1,
            Shape::Box(_) => 2,
            Shape::Text(_) => 3,
        }
    }
}

/// A reference normalized to its origin: `(cell, flip, mag bits, angle bits, props)`.
type InstKey = (u64, bool, u64, u64, Vec<Prop>);

/// Groups in first-appearance order, each with its displacements.
struct Groups<K> {
    index: HashMap<K, usize>,
    groups: Vec<(K, Vec<P>)>,
}

impl<K: std::hash::Hash + Eq + Clone> Groups<K> {
    fn new() -> Self {
        Groups { index: HashMap::new(), groups: Vec::new() }
    }
    fn add(&mut self, k: K, d: P) {
        let i = *self.index.entry(k.clone()).or_insert_with(|| {
            self.groups.push((k, Vec::new()));
            self.groups.len() - 1
        });
        self.groups[i].1.push(d);
    }
}

fn pt(p: (i32, i32)) -> P {
    (p.0 as i64, p.1 as i64)
}

/// The cells bottom-up: each after every cell it references (depth first, in cell order).
fn bottom_up(layout: &Layout, index: &HashMap<&str, usize>) -> Result<Vec<usize>, String> {
    let n = layout.cells.len();
    let mut state = vec![0u8; n]; // 0 new, 1 on the stack, 2 done
    let mut order = Vec::with_capacity(n);
    fn visit(c: usize, layout: &Layout, index: &HashMap<&str, usize>, state: &mut [u8], order: &mut Vec<usize>) -> Result<(), String> {
        match state[c] {
            2 => return Ok(()),
            1 => return Err(format!("cell {} references itself through its children", layout.cells[c].name)),
            _ => {}
        }
        state[c] = 1;
        for e in &layout.cells[c].elements {
            if let GdsElement::SRef { structure, .. } | GdsElement::ARef { structure, .. } = e {
                let &k = index
                    .get(structure.as_str())
                    .ok_or_else(|| format!("cell {} references {structure}, which is not in the layout", layout.cells[c].name))?;
                visit(k, layout, index, state, order)?;
            }
        }
        state[c] = 2;
        order.push(c);
        Ok(())
    }
    for c in 0..n {
        visit(c, layout, index, &mut state, &mut order)?;
    }
    Ok(order)
}

/// Write a layout as OASIS, in KLayout's order: magic, START, the file's standard properties,
/// the property-name, property-string and text-string tables, the cells bottom-up, the
/// cell-name table with each cell's offset, END.
pub fn encode(layout: &Layout, opts: &WriteOptions) -> Result<Vec<u8>, String> {
    let index: HashMap<&str, usize> = layout.cells.iter().enumerate().map(|(i, c)| (c.name.as_str(), i)).collect();
    if index.len() != layout.cells.len() {
        return Err("two cells share a name".into());
    }
    let order = bottom_up(layout, &index)?;
    let mut w = Writer::new(opts.cblocks);

    // Cell bodies first, so every string they use has an id before the tables are written.
    let mut bodies = Vec::with_capacity(order.len());
    for &c in &order {
        w.cell = layout.cells[c].name.clone();
        w.reset_modal_variables();
        write_cell_body(&mut w, &layout.cells[c], &index, opts.compression_level)?;
        bodies.push((c, std::mem::take(&mut w.sink.chunks)));
    }

    let mut out = Vec::new();
    write_start(&mut out, layout.unit);
    w.reset_modal_variables();
    write_file_properties(&mut w, layout, &order, &index);
    flush_plain(&mut out, &mut w.sink);
    let propnames_pos = write_table(&mut out, opts, |s| {
        for n in STD_PROPNAMES {
            s.record_id(7);
            s.string(&n_or_astring(n, true));
        }
    });
    let propstrings = w.propstring_order.clone();
    let propstrings_pos = write_table(&mut out, opts, |s| {
        for p in &propstrings {
            s.record_id(9);
            s.string(p);
        }
    });
    let textstrings = std::mem::take(&mut w.textstring_order);
    let textstrings_pos = write_table(&mut out, opts, |s| {
        for t in &textstrings {
            s.record_id(5);
            s.string(&n_or_astring(t, false));
        }
    });
    let mut cell_offsets = vec![0u64; layout.cells.len()];
    for (c, chunks) in bodies {
        cell_offsets[c] = out.len() as u64;
        let mut s = Sink::default();
        s.record_id(13); // CELL
        s.uint(c as u64);
        out.extend(s.chunks.concat());
        emit_chunks(&mut out, chunks, opts.cblocks);
    }
    let cellnames_pos = write_cellname_table(&mut out, opts, layout, &cell_offsets);
    write_end(&mut out, [cellnames_pos, textstrings_pos, propnames_pos, propstrings_pos, 0]);
    Ok(out)
}

fn write_start(out: &mut Vec<u8>, unit: f64) {
    out.extend_from_slice(MAGIC);
    let mut s = Sink::default();
    s.record_id(1);
    s.string("1.0");
    s.real(unit);
    s.uint(1); // offset flag: strict mode, the table offsets are in END
    out.extend(s.chunks.concat());
}

/// S_MAX_SIGNED_INTEGER_WIDTH and S_MAX_UNSIGNED_INTEGER_WIDTH (4 bytes), then S_TOP_CELL for
/// each cell no other references, bottom-up.
fn write_file_properties(w: &mut Writer, layout: &Layout, order: &[usize], index: &HashMap<&str, usize>) {
    w.write_property_def(S_MAX_SIGNED_INTEGER_WIDTH, &[PropVal::UInt(4)], true);
    w.write_property_def(S_MAX_UNSIGNED_INTEGER_WIDTH, &[PropVal::UInt(4)], true);
    let mut referenced = vec![false; layout.cells.len()];
    for c in &layout.cells {
        for e in &c.elements {
            if let GdsElement::SRef { structure, .. } | GdsElement::ARef { structure, .. } = e {
                referenced[index[structure.as_str()]] = true;
            }
        }
    }
    for &c in order {
        if !referenced[c] {
            let name = n_or_astring(&layout.cells[c].name, true);
            w.write_property_def(S_TOP_CELL, &[PropVal::Str(name)], true);
        }
    }
}

fn flush_plain(out: &mut Vec<u8>, s: &mut Sink) {
    for c in std::mem::take(&mut s.chunks) {
        out.extend(c);
    }
}

/// One table, in CBLOCKs when they are on: its file position, or 0 when it is empty.
fn write_table(out: &mut Vec<u8>, opts: &WriteOptions, body: impl FnOnce(&mut Sink)) -> u64 {
    let mut s = Sink { chunks: Vec::new(), split: opts.cblocks };
    body(&mut s);
    if s.len() == 0 {
        return 0;
    }
    let pos = out.len() as u64;
    emit_chunks(out, s.chunks, opts.cblocks);
    pos
}

fn emit_chunks(out: &mut Vec<u8>, chunks: Vec<Vec<u8>>, cblocks: bool) {
    for c in chunks {
        if c.is_empty() {
            continue;
        }
        if !cblocks {
            out.extend(c);
            continue;
        }
        let z = miniz_oxide::deflate::compress_to_vec(&c, 6);
        if c.len() > z.len() + COMPRESSION_OVERHEAD {
            let mut s = Sink::default();
            s.record_id(34); // CBLOCK
            s.uint(0); // RFC 1951 deflate
            s.uint(c.len() as u64);
            s.uint(z.len() as u64);
            out.extend(s.chunks.concat());
            out.extend(z);
        } else {
            out.extend(c);
        }
    }
}

/// CELLNAME records in cell order (implicit numbering, as cells are numbered in order), each
/// followed by S_CELL_OFFSET with its CELL record's position, the modal variables reset first.
fn write_cellname_table(out: &mut Vec<u8>, opts: &WriteOptions, layout: &Layout, offsets: &[u64]) -> u64 {
    let mut w = Writer::new(opts.cblocks);
    for (c, cell) in layout.cells.iter().enumerate() {
        w.sink.record_id(3);
        w.sink.string(&n_or_astring(&cell.name, true));
        w.reset_modal_variables();
        w.write_property_def(S_CELL_OFFSET, &[PropVal::UInt(offsets[c])], true);
    }
    if w.sink.len() == 0 {
        return 0;
    }
    let pos = out.len() as u64;
    emit_chunks(out, w.sink.chunks, opts.cblocks);
    pos
}

/// END: the table offsets (cell names, text strings, property names, property strings, layer
/// names, x-names), each flagged strict, then padding to 256 bytes and no validation.
fn write_end(out: &mut Vec<u8>, tables: [u64; 5]) {
    let end = out.len();
    let mut s = Sink::default();
    s.record_id(2);
    for t in tables {
        s.uint(1);
        s.uint(t);
    }
    s.uint(1);
    s.uint(0);
    out.extend(s.chunks.concat());
    while out.len() < end + 254 {
        out.push(0x80);
    }
    out.push(0);
    out.push(0); // validation scheme: none
}

/// One cell's records: the references, then the shapes layer by layer.
fn write_cell_body(w: &mut Writer, cell: &Cell, index: &HashMap<&str, usize>, level: u8) -> Result<(), String> {
    write_insts(w, cell, index);
    write_shapes(w, cell, level)
}

/// References gathered by cell and transform and written with irregular repetitions (KLayout
/// compresses instances at level 0 whatever the shape level); an AREF on its lattice is one
/// placement with a regular repetition, an AREF off it each of its placements.
fn write_insts(w: &mut Writer, cell: &Cell, index: &HashMap<&str, usize>) {
    let mut groups: Groups<InstKey> = Groups::new();
    let mut arrays = Vec::new();
    let key = |s: &str, t: &STrans, props: &[Prop]| -> InstKey {
        let mut p = props.to_vec();
        p.sort_by_key(|p| p.0);
        (index[s] as u64, t.flip_x, t.mag.to_bits(), t.angle.to_bits(), p)
    };
    for e in &cell.elements {
        match e {
            GdsElement::SRef { structure, origin, trans, props } => groups.add(key(structure, trans, props), pt(*origin)),
            GdsElement::ARef { structure, origin, lr, ul, rows, columns, trans, props } => {
                let (o, cols, rows) = (pt(*origin), (*columns).max(1) as i64, (*rows).max(1) as i64);
                let (c, r) = (sub(pt(*lr), o), sub(pt(*ul), o));
                if c.0 % cols != 0 || c.1 % cols != 0 || r.0 % rows != 0 || r.1 % rows != 0 {
                    for j in 0..rows {
                        for i in 0..cols {
                            let d = (o.0 + i * c.0 / cols + j * r.0 / rows, o.1 + i * c.1 / cols + j * r.1 / rows);
                            groups.add(key(structure, trans, props), d);
                        }
                    }
                } else if cols > 1 || rows > 1 {
                    let rep = Rep::Regular { a: (c.0 / cols, c.1 / cols), b: (r.0 / rows, r.1 / rows), n: cols as u64, m: rows as u64 };
                    arrays.push((index[structure.as_str()] as u64, *trans, o, props.clone(), rep));
                } else {
                    groups.add(key(structure, trans, props), o);
                }
            }
            _ => {}
        }
    }
    for ((cell_ref, flip_x, mag, angle, props), disps) in groups.groups {
        let t = STrans { flip_x, mag: f64::from_bits(mag), angle: f64::from_bits(angle) };
        for (p, rep) in compress(0, disps) {
            w.write_placement(cell_ref, &t, p, &props, rep.as_ref());
        }
    }
    for (cell_ref, t, o, props, rep) in arrays {
        w.write_placement(cell_ref, &t, o, &props, Some(&rep));
    }
}

/// Shapes layer by layer in first-appearance order; within a layer paths, polygons, boxes and
/// texts, those without properties first, each kind through the compressor.
fn write_shapes(w: &mut Writer, cell: &Cell, level: u8) -> Result<(), String> {
    let mut layers: Groups<(u64, u64)> = Groups::new();
    let mut per_layer: Vec<Groups<(Shape, Vec<Prop>)>> = Vec::new();
    for e in &cell.elements {
        let (ld, shape, d, props) = match e {
            GdsElement::Boundary { layer, datatype, xy, props } => {
                let (shape, d) = match as_box(xy) {
                    Some((o, wh)) => (Shape::Box(wh), o),
                    None => {
                        let h = hull(xy);
                        let Some(&s) = h.first() else { continue };
                        (Shape::Polygon(h[1..].iter().map(|&p| sub(p, s)).collect()), s)
                    }
                };
                ((*layer, *datatype), shape, d, props)
            }
            GdsElement::Box { layer, datatype, bounds: (x0, y0, x1, y1), props } => {
                let (x0, x1) = ((*x0).min(*x1) as i64, (*x0).max(*x1) as i64);
                let (y0, y1) = ((*y0).min(*y1) as i64, (*y0).max(*y1) as i64);
                ((*layer, *datatype), Shape::Box((x1 - x0, y1 - y0)), (x0, y0), props)
            }
            GdsElement::Path { layer, datatype, path_type, width, xy, props } => {
                let Some(&s) = xy.first() else { continue };
                let s = pt(s);
                let pts = xy.iter().map(|&p| sub(pt(p), s)).collect();
                ((*layer, *datatype), Shape::Path { path_type: *path_type, width: *width as i64, pts }, s, props)
            }
            GdsElement::Text { layer, datatype, origin, text, props, .. } => {
                ((*layer, *datatype), Shape::Text(text.clone()), pt(*origin), props)
            }
            GdsElement::SRef { .. } | GdsElement::ARef { .. } => continue,
        };
        let ld = (ld.0 as u16 as u64, ld.1 as u16 as u64);
        if !layers.index.contains_key(&ld) {
            per_layer.push(Groups::new());
        }
        layers.add(ld, (0, 0));
        let li = layers.index[&ld];
        let mut p = props.clone();
        p.sort_by_key(|p| p.0);
        per_layer[li].add((shape, p), d);
    }
    for (li, (ld, _)) in layers.groups.iter().enumerate() {
        w.layer = ld.0;
        w.datatype = ld.1;
        let mut groups: Vec<&((Shape, Vec<Prop>), Vec<P>)> = per_layer[li].groups.iter().collect();
        groups.sort_by_key(|((s, p), _)| (!p.is_empty(), s.rank()));
        for ((shape, props), disps) in groups {
            for (d, rep) in compress(level, disps.clone()) {
                let rep = rep.as_ref();
                match shape {
                    Shape::Box(wh) => w.write_box(d, *wh, props, rep),
                    Shape::Polygon(pl) => w.write_polygon(d, pl.clone(), props, rep)?,
                    Shape::Path { path_type, width, pts } => {
                        let xy: Vec<P> = pts.iter().map(|&p| add(p, d)).collect();
                        w.write_path(*path_type, *width, &xy, props, rep)?
                    }
                    Shape::Text(t) => w.write_text(t, d, props, rep),
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// The reader

struct Bytes<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Bytes<'a> {
    fn eof(&self) -> bool {
        self.i >= self.b.len()
    }
    fn byte(&mut self) -> Result<u8, String> {
        let b = *self.b.get(self.i).ok_or("unexpected end of file")?;
        self.i += 1;
        Ok(b)
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        if self.i + n > self.b.len() {
            return Err("unexpected end of file".into());
        }
        let s = &self.b[self.i..self.i + n];
        self.i += n;
        Ok(s)
    }
    fn uint(&mut self) -> Result<u64, String> {
        let (mut v, mut shift) = (0u64, 0u32);
        loop {
            let b = self.byte()?;
            if shift < 64 {
                v |= ((b & 0x7f) as u64) << shift;
            } else if b & 0x7f != 0 {
                return Err("unsigned integer overflow".into());
            }
            if b & 0x80 == 0 {
                return Ok(v);
            }
            shift += 7;
        }
    }
    fn sint(&mut self) -> Result<i64, String> {
        let u = self.uint()?;
        let m = (u >> 1) as i64;
        Ok(if u & 1 != 0 { -m } else { m })
    }
    fn real(&mut self) -> Result<f64, String> {
        Ok(match self.uint()? {
            0 => self.uint()? as f64,
            1 => -(self.uint()? as f64),
            2 => 1.0 / self.uint()? as f64,
            3 => -1.0 / self.uint()? as f64,
            4 => self.uint()? as f64 / self.uint()? as f64,
            5 => -(self.uint()? as f64 / self.uint()? as f64),
            6 => f32::from_le_bytes(self.take(4)?.try_into().unwrap()) as f64,
            7 => f64::from_le_bytes(self.take(8)?.try_into().unwrap()),
            t => return Err(format!("invalid real type {t}")),
        })
    }
    fn bstring(&mut self) -> Result<Vec<u8>, String> {
        let n = self.uint()? as usize;
        Ok(self.take(n)?.to_vec())
    }
    fn string(&mut self) -> Result<String, String> {
        Ok(String::from_utf8_lossy(&self.bstring()?).into_owned())
    }
    fn gdelta(&mut self) -> Result<P, String> {
        let d = self.uint()?;
        if d & 1 == 0 {
            Ok(octangular((d >> 1) & 7, (d >> 4) as i64))
        } else {
            let x = (d >> 2) as i64;
            let x = if d & 2 != 0 { -x } else { x };
            Ok((x, self.sint()?))
        }
    }
    /// A repetition, read as KLayout reads it: a zero step in an irregular one is no copy, and
    /// a lattice axis of zero pitch has one copy along it.
    fn repetition(&mut self, last: &Option<Rep>) -> Result<Rep, String> {
        let t = self.uint()?;
        let count = |n: u64, d: P| if d == (0, 0) { 1 } else { n };
        Ok(match t {
            0 => last.clone().ok_or("repetition reused before any was given")?,
            1 => {
                let (nx, ny) = (self.uint()? + 2, self.uint()? + 2);
                let (dx, dy) = (self.uint()? as i64, self.uint()? as i64);
                Rep::Regular { a: (dx, 0), b: (0, dy), n: count(nx, (dx, 0)), m: count(ny, (0, dy)) }
            }
            2 => {
                let n = self.uint()? + 2;
                let dx = self.uint()? as i64;
                Rep::Regular { a: (dx, 0), b: (0, 0), n: count(n, (dx, 0)), m: 1 }
            }
            3 => {
                let m = self.uint()? + 2;
                let dy = self.uint()? as i64;
                Rep::Regular { a: (0, 0), b: (0, dy), n: 1, m: count(m, (0, dy)) }
            }
            4..=7 => {
                let n = self.uint()? + 1;
                let g = if t == 5 || t == 7 { self.uint()? as i64 } else { 1 };
                let mut acc = 0i64;
                let mut pts = Vec::with_capacity(n as usize);
                for _ in 0..n {
                    let d = self.uint()? as i64 * g;
                    if d != 0 {
                        acc += d;
                        pts.push(if t < 6 { (acc, 0) } else { (0, acc) });
                    }
                }
                Rep::Irregular(pts)
            }
            8 => {
                let (n, m) = (self.uint()? + 2, self.uint()? + 2);
                let (a, b) = (self.gdelta()?, self.gdelta()?);
                Rep::Regular { a, b, n: count(n, a), m: count(m, b) }
            }
            9 => {
                let n = self.uint()? + 2;
                let a = self.gdelta()?;
                Rep::Regular { a, b: (0, 0), n: count(n, a), m: 1 }
            }
            10 | 11 => {
                let n = self.uint()? + 1;
                let g = if t == 11 { self.uint()? as i64 } else { 1 };
                let mut acc = (0, 0);
                let mut pts = Vec::with_capacity(n as usize);
                for _ in 0..n {
                    let d = self.gdelta()?;
                    if d != (0, 0) {
                        acc = add(acc, (d.0 * g, d.1 * g));
                        pts.push(acc);
                    }
                }
                Rep::Irregular(pts)
            }
            t => return Err(format!("invalid repetition type {t}")),
        })
    }
    /// A point list relative to the element's start, the start excluded; for a polygon of
    /// type 0 or 1 the implied last point included.
    fn pointlist(&mut self, for_polygons: bool) -> Result<Vec<P>, String> {
        let t = self.uint()?;
        let n = self.uint()? as usize;
        let mut pts = Vec::with_capacity(n + 1);
        let mut p = (0, 0);
        match t {
            0 | 1 => {
                for i in 0..n {
                    let d = self.sint()?;
                    let horizontal = (i % 2 == 0) == (t == 0);
                    p = if horizontal { (p.0 + d, p.1) } else { (p.0, p.1 + d) };
                    pts.push(p);
                }
                if for_polygons {
                    // The last edge written turns into the implied one, which closes on the start.
                    let last_horizontal = (n % 2 == 1) == (t == 0);
                    pts.push(if last_horizontal { (p.0, 0) } else { (0, p.1) });
                }
            }
            2 => {
                for _ in 0..n {
                    let d = self.uint()?;
                    p = add(p, octangular(d & 3, (d >> 2) as i64));
                    pts.push(p);
                }
            }
            3 => {
                for _ in 0..n {
                    let d = self.uint()?;
                    p = add(p, octangular(d & 7, (d >> 3) as i64));
                    pts.push(p);
                }
            }
            4 => {
                for _ in 0..n {
                    p = add(p, self.gdelta()?);
                    pts.push(p);
                }
            }
            5 => {
                let mut d = (0, 0);
                for _ in 0..n {
                    d = add(d, self.gdelta()?);
                    p = add(p, d);
                    pts.push(p);
                }
            }
            t => return Err(format!("invalid point list type {t}")),
        }
        Ok(pts)
    }
}

fn octangular(dir: u64, l: i64) -> P {
    match dir {
        0 => (l, 0),
        1 => (0, l),
        2 => (-l, 0),
        3 => (0, -l),
        4 => (l, l),
        5 => (-l, l),
        6 => (-l, -l),
        _ => (l, -l),
    }
}

/// The 26 CTRAPEZOID shapes: each corner as `(x per w, x per h, y per w, y per h)`.
const CTRAPS: [[(i64, i64, i64, i64); 4]; 26] = [
    [(0, 0, 0, 0), (0, 0, 0, 1), (1, -1, 0, 1), (1, 0, 0, 0)],
    [(0, 0, 0, 0), (0, 0, 0, 1), (1, 0, 0, 1), (1, -1, 0, 0)],
    [(0, 0, 0, 0), (0, 1, 0, 1), (1, 0, 0, 1), (1, 0, 0, 0)],
    [(0, 1, 0, 0), (0, 0, 0, 1), (1, 0, 0, 1), (1, 0, 0, 0)],
    [(0, 0, 0, 0), (0, 1, 0, 1), (1, -1, 0, 1), (1, 0, 0, 0)],
    [(0, 1, 0, 0), (0, 0, 0, 1), (1, 0, 0, 1), (1, -1, 0, 0)],
    [(0, 0, 0, 0), (0, 1, 0, 1), (1, 0, 0, 1), (1, -1, 0, 0)],
    [(0, 1, 0, 0), (0, 0, 0, 1), (1, -1, 0, 1), (1, 0, 0, 0)],
    [(0, 0, 0, 0), (0, 0, 0, 1), (1, 0, -1, 1), (1, 0, 0, 0)],
    [(0, 0, 0, 0), (0, 0, -1, 1), (1, 0, 0, 1), (1, 0, 0, 0)],
    [(0, 0, 0, 0), (0, 0, 0, 1), (1, 0, 0, 1), (1, 0, 1, 0)],
    [(0, 0, 1, 0), (0, 0, 0, 1), (1, 0, 0, 1), (1, 0, 0, 0)],
    [(0, 0, 0, 0), (0, 0, 0, 1), (1, 0, -1, 1), (1, 0, 1, 0)],
    [(0, 0, 1, 0), (0, 0, -1, 1), (1, 0, 0, 1), (1, 0, 0, 0)],
    [(0, 0, 0, 0), (0, 0, -1, 1), (1, 0, 0, 1), (1, 0, 1, 0)],
    [(0, 0, 1, 0), (0, 0, 0, 1), (1, 0, -1, 1), (1, 0, 0, 0)],
    [(0, 0, 0, 0), (0, 0, 1, 0), (1, 0, 0, 0), (0, 0, 0, 0)],
    [(0, 0, 0, 0), (0, 0, 1, 0), (1, 0, 1, 0), (0, 0, 0, 0)],
    [(0, 0, 0, 0), (1, 0, 1, 0), (1, 0, 0, 0), (0, 0, 0, 0)],
    [(0, 0, 1, 0), (1, 0, 1, 0), (1, 0, 0, 0), (0, 0, 1, 0)],
    [(0, 0, 0, 0), (0, 1, 0, 1), (0, 2, 0, 0), (0, 0, 0, 0)],
    [(0, 0, 0, 1), (0, 2, 0, 1), (0, 1, 0, 0), (0, 0, 0, 1)],
    [(0, 0, 0, 0), (0, 0, 2, 0), (1, 0, 1, 0), (0, 0, 0, 0)],
    [(1, 0, 0, 0), (0, 0, 1, 0), (1, 0, 2, 0), (1, 0, 0, 0)],
    [(0, 0, 0, 0), (0, 0, 0, 1), (1, 0, 0, 1), (1, 0, 0, 0)],
    [(0, 0, 0, 0), (0, 0, 1, 0), (1, 0, 1, 0), (1, 0, 0, 0)],
];

/// A name or string given by reference number or in place, resolved once the tables are read.
#[derive(Debug, Clone, PartialEq)]
enum NameRef {
    Ref(u64),
    Str(String),
}

#[derive(Debug, Clone, PartialEq)]
enum RVal {
    Real(f64),
    UInt(u64),
    SInt(i64),
    Str(String),
    Ref(u64),
}

struct RawCell {
    name: NameRef,
    elements: Vec<GdsElement>,
    /// Elements whose text (`true`) or referenced cell (`false`) is a reference number.
    fixups: Vec<(usize, bool, u64)>,
}

struct PendingProp {
    cell: usize,
    elements: std::ops::Range<usize>,
    name: NameRef,
    values: Vec<RVal>,
}

#[derive(Default)]
struct ReadModal {
    repetition: Option<Rep>,
    placement_cell: Option<NameRef>,
    placement_x: i64,
    placement_y: i64,
    layer: Option<u64>,
    datatype: Option<u64>,
    textlayer: Option<u64>,
    texttype: Option<u64>,
    text_x: i64,
    text_y: i64,
    text_string: Option<NameRef>,
    geometry_x: i64,
    geometry_y: i64,
    geometry_w: Option<i64>,
    geometry_h: Option<i64>,
    polygon_point_list: Option<Vec<P>>,
    path_halfwidth: Option<i64>,
    path_start_extension: Option<i64>,
    path_end_extension: Option<i64>,
    path_point_list: Option<Vec<P>>,
    ctrapezoid_type: Option<u64>,
    circle_radius: Option<i64>,
    last_property: Option<(NameRef, Vec<RVal>)>,
}

#[derive(Default)]
struct Reader {
    unit: f64,
    started: bool,
    ended: bool,
    tables: [HashMap<u64, String>; 4], // cell names, text strings, property names, property strings
    next: [u64; 4],
    cells: Vec<RawCell>,
    m: ReadModal,
    xy_absolute: bool,
    /// The elements the last element record made, which a following PROPERTY applies to.
    last: Option<std::ops::Range<usize>>,
    props: Vec<PendingProp>,
}

const CELLNAMES: usize = 0;
const TEXTSTRINGS: usize = 1;
const PROPNAMES: usize = 2;
const PROPSTRINGS: usize = 3;

fn need<T: Clone>(v: &Option<T>, what: &str) -> Result<T, String> {
    v.clone().ok_or_else(|| format!("modal variable {what} used before it was set"))
}

impl Reader {
    fn cur(&mut self) -> Result<&mut RawCell, String> {
        self.cells.last_mut().ok_or_else(|| "element outside any cell".into())
    }

    fn xy(&mut self, r: &mut Bytes, info: u8, xbit: u8, ybit: u8, which: u8) -> Result<P, String> {
        let (mx, my) = match which {
            0 => (self.m.geometry_x, self.m.geometry_y),
            1 => (self.m.text_x, self.m.text_y),
            _ => (self.m.placement_x, self.m.placement_y),
        };
        let mut x = mx;
        let mut y = my;
        if info & xbit != 0 {
            let v = r.sint()?;
            x = if self.xy_absolute { v } else { mx + v };
        }
        if info & ybit != 0 {
            let v = r.sint()?;
            y = if self.xy_absolute { v } else { my + v };
        }
        match which {
            0 => (self.m.geometry_x, self.m.geometry_y) = (x, y),
            1 => (self.m.text_x, self.m.text_y) = (x, y),
            _ => (self.m.placement_x, self.m.placement_y) = (x, y),
        }
        Ok((x, y))
    }

    fn rep(&mut self, r: &mut Bytes, present: bool) -> Result<Vec<P>, String> {
        if !present {
            return Ok(vec![(0, 0)]);
        }
        let rep = r.repetition(&self.m.repetition)?;
        self.m.repetition = Some(rep.clone());
        Ok(rep.points())
    }

    fn layer_datatype(&mut self, r: &mut Bytes, info: u8) -> Result<(i16, i16), String> {
        if info & 0x01 != 0 {
            self.m.layer = Some(r.uint()?);
        }
        if info & 0x02 != 0 {
            self.m.datatype = Some(r.uint()?);
        }
        Ok((need(&self.m.layer, "layer")? as i16, need(&self.m.datatype, "datatype")? as i16))
    }

    /// Push one element per displacement; a following PROPERTY applies to all of them.
    fn emit(&mut self, at: &[P], make: impl Fn(P) -> GdsElement) -> Result<(), String> {
        let cell = self.cur()?;
        let start = cell.elements.len();
        for &d in at {
            cell.elements.push(make(d));
        }
        let end = cell.elements.len();
        self.last = Some(start..end);
        Ok(())
    }

    fn polygon(&mut self, at: &[P], (layer, datatype): (i16, i16), base: P, pts: &[P]) -> Result<(), String> {
        self.emit(at, |d| {
            let o = add(base, d);
            let mut xy: Vec<(i32, i32)> = pts.iter().map(|&p| ((o.0 + p.0) as i32, (o.1 + p.1) as i32)).collect();
            xy.push(xy[0]);
            GdsElement::Boundary { layer, datatype, xy, props: Vec::new() }
        })
    }

    fn record(&mut self, r: &mut Bytes) -> Result<(), String> {
        let id = r.uint()?;
        let element_or_property = matches!(id, 17..=29 | 32 | 33 | 34);
        if !element_or_property {
            self.last = None;
        }
        match id {
            0 => {}
            1 => {
                r.string()?;
                self.unit = r.real()?;
                if r.uint()? == 0 {
                    for _ in 0..12 {
                        r.uint()?;
                    }
                }
                self.started = true;
            }
            2 => self.ended = true,
            3..=10 => {
                let table = match id {
                    3 | 4 => CELLNAMES,
                    5 | 6 => TEXTSTRINGS,
                    7 | 8 => PROPNAMES,
                    _ => PROPSTRINGS,
                };
                let s = r.string()?;
                let n = if id % 2 == 0 { r.uint()? } else { self.next[table] };
                self.next[table] = n + 1;
                self.tables[table].insert(n, s);
                self.m.last_property = None;
            }
            11 | 12 => {
                r.string()?;
                for _ in 0..2 {
                    match r.uint()? {
                        0 => {}
                        1..=3 => {
                            r.uint()?;
                        }
                        4 => {
                            r.uint()?;
                            r.uint()?;
                        }
                        t => return Err(format!("invalid interval type {t}")),
                    }
                }
            }
            13 | 14 => {
                let name = if id == 13 { NameRef::Ref(r.uint()?) } else { NameRef::Str(r.string()?) };
                self.cells.push(RawCell { name, elements: Vec::new(), fixups: Vec::new() });
                self.m = ReadModal::default();
                self.xy_absolute = true;
            }
            15 => self.xy_absolute = true,
            16 => self.xy_absolute = false,
            17 | 18 => self.placement(r, id == 18)?,
            19 => self.text(r)?,
            20 => {
                let info = r.byte()?;
                let ld = self.layer_datatype(r, info)?;
                if info & 0x40 != 0 {
                    self.m.geometry_w = Some(r.uint()? as i64);
                }
                if info & 0x80 != 0 {
                    self.m.geometry_h = self.m.geometry_w;
                } else if info & 0x20 != 0 {
                    self.m.geometry_h = Some(r.uint()? as i64);
                }
                let (w, h) = (need(&self.m.geometry_w, "geometry-w")?, need(&self.m.geometry_h, "geometry-h")?);
                let o = self.xy(r, info, 0x10, 0x08, 0)?;
                let at = self.rep(r, info & 0x04 != 0)?;
                self.polygon(&at, ld, o, &[(0, 0), (w, 0), (w, h), (0, h)])?;
            }
            21 => {
                let info = r.byte()?;
                let ld = self.layer_datatype(r, info)?;
                if info & 0x20 != 0 {
                    self.m.polygon_point_list = Some(r.pointlist(true)?);
                }
                let pl = need(&self.m.polygon_point_list, "polygon-point-list")?;
                let o = self.xy(r, info, 0x10, 0x08, 0)?;
                let at = self.rep(r, info & 0x04 != 0)?;
                let pts: Vec<P> = std::iter::once((0, 0)).chain(pl.iter().copied()).collect();
                self.polygon(&at, ld, o, &pts)?;
            }
            22 => self.path(r)?,
            23..=25 => {
                let info = r.byte()?;
                let ld = self.layer_datatype(r, info)?;
                if info & 0x40 != 0 {
                    self.m.geometry_w = Some(r.uint()? as i64);
                }
                if info & 0x20 != 0 {
                    self.m.geometry_h = Some(r.uint()? as i64);
                }
                let da = if id == 23 || id == 24 { r.sint()? } else { 0 };
                let db = if id == 23 || id == 25 { r.sint()? } else { 0 };
                let (w, h) = (need(&self.m.geometry_w, "geometry-w")?, need(&self.m.geometry_h, "geometry-h")?);
                let o = self.xy(r, info, 0x10, 0x08, 0)?;
                let pts = if info & 0x80 != 0 {
                    [(0, da.max(0)), (0, h + db.min(0)), (w, h - db.max(0)), (w, -da.min(0))]
                } else {
                    [(da.max(0), h), (w + db.min(0), h), (w - db.max(0), 0), (-da.min(0), 0)]
                };
                let at = self.rep(r, info & 0x04 != 0)?;
                self.polygon(&at, ld, o, &pts)?;
            }
            26 => {
                let info = r.byte()?;
                let ld = self.layer_datatype(r, info)?;
                if info & 0x80 != 0 {
                    self.m.ctrapezoid_type = Some(r.uint()?);
                }
                if info & 0x40 != 0 {
                    self.m.geometry_w = Some(r.uint()? as i64);
                }
                if info & 0x20 != 0 {
                    self.m.geometry_h = Some(r.uint()? as i64);
                }
                let o = self.xy(r, info, 0x10, 0x08, 0)?;
                let t = need(&self.m.ctrapezoid_type, "ctrapezoid-type")? as usize;
                let table = CTRAPS.get(t).ok_or_else(|| format!("invalid CTRAPEZOID type {t}"))?;
                let (gw, gh) = (self.m.geometry_w.unwrap_or(0), self.m.geometry_h.unwrap_or(0));
                let (mut w, mut h) = (0, 0);
                let mut pts: Vec<P> = table
                    .iter()
                    .map(|&(xw, xh, yw, yh)| {
                        let p = (xw * gw + xh * gh, yw * gw + yh * gh);
                        w = w.max(p.0);
                        h = h.max(p.1);
                        p
                    })
                    .collect();
                // The modal width and height become the shape's extent.
                self.m.geometry_w = Some(w);
                self.m.geometry_h = Some(h);
                if pts[3] == pts[0] {
                    pts.pop();
                }
                let at = self.rep(r, info & 0x04 != 0)?;
                self.polygon(&at, ld, o, &pts)?;
            }
            27 => {
                let info = r.byte()?;
                let (layer, datatype) = self.layer_datatype(r, info)?;
                if info & 0x20 != 0 {
                    self.m.circle_radius = Some(r.uint()? as i64);
                }
                let rad = need(&self.m.circle_radius, "circle-radius")?;
                let o = self.xy(r, info, 0x10, 0x08, 0)?;
                let at = self.rep(r, info & 0x04 != 0)?;
                // A circle is a single-point round path, as KLayout reads it.
                self.emit(&at, |d| {
                    let c = add(o, d);
                    GdsElement::Path {
                        layer,
                        datatype,
                        path_type: 1,
                        width: (2 * rad) as i32,
                        xy: vec![(c.0 as i32, c.1 as i32)],
                        props: Vec::new(),
                    }
                })?;
            }
            28 | 29 => self.property(r, id == 29)?,
            30 | 31 => {
                r.uint()?;
                r.bstring()?;
                if id == 31 {
                    r.uint()?;
                }
            }
            32 => {
                r.uint()?;
                r.bstring()?;
                self.last = None;
            }
            33 => {
                let info = r.byte()?;
                r.uint()?;
                self.layer_datatype(r, info)?;
                r.bstring()?;
                self.xy(r, info, 0x10, 0x08, 0)?;
                self.rep(r, info & 0x04 != 0)?;
                self.last = None;
            }
            34 => {
                if r.uint()? != 0 {
                    return Err("unknown CBLOCK compression type".into());
                }
                let (raw, comp) = (r.uint()? as usize, r.uint()? as usize);
                let data = miniz_oxide::inflate::decompress_to_vec_with_limit(r.take(comp)?, raw)
                    .map_err(|e| format!("CBLOCK does not inflate: {e:?}"))?;
                if data.len() != raw {
                    return Err("CBLOCK inflates to the wrong length".into());
                }
                let mut inner = Bytes { b: &data, i: 0 };
                while !inner.eof() && !self.ended {
                    self.record(&mut inner)?;
                }
            }
            id => return Err(format!("unknown record type {id}")),
        }
        Ok(())
    }

    fn placement(&mut self, r: &mut Bytes, complex: bool) -> Result<(), String> {
        let info = r.byte()?;
        if info & 0x80 != 0 {
            self.m.placement_cell = Some(if info & 0x40 != 0 { NameRef::Ref(r.uint()?) } else { NameRef::Str(r.string()?) });
        }
        let cell = need(&self.m.placement_cell, "placement-cell")?;
        let (mut mag, mut angle) = (1.0, 0.0);
        if complex {
            if info & 0x04 != 0 {
                mag = r.real()?;
            }
            if info & 0x02 != 0 {
                angle = r.real()?;
            }
        } else {
            angle = ((info >> 1) & 0x03) as f64 * 90.0;
        }
        let trans = STrans { flip_x: info & 0x01 != 0, mag, angle };
        let o = self.xy(r, info, 0x20, 0x10, 2)?;
        let rep = if info & 0x08 != 0 {
            let rep = r.repetition(&self.m.repetition)?;
            self.m.repetition = Some(rep.clone());
            Some(rep)
        } else {
            None
        };
        let (structure, fix) = match cell {
            NameRef::Str(s) => (s, None),
            NameRef::Ref(n) => (String::new(), Some(n)),
        };
        let o32 = |p: P| (p.0 as i32, p.1 as i32);
        let cell = self.cur()?;
        let start = cell.elements.len();
        match rep {
            // A lattice is an AREF, when its counts fit one.
            Some(Rep::Regular { a, b, n, m }) if n * m > 1 && n <= i16::MAX as u64 && m <= i16::MAX as u64 => {
                cell.elements.push(GdsElement::ARef {
                    structure,
                    origin: o32(o),
                    lr: o32(add(o, (a.0 * n as i64, a.1 * n as i64))),
                    ul: o32(add(o, (b.0 * m as i64, b.1 * m as i64))),
                    rows: m as i16,
                    columns: n as i16,
                    trans,
                    props: Vec::new(),
                });
            }
            rep => {
                let at = rep.map(|r| r.points()).unwrap_or_else(|| vec![(0, 0)]);
                for d in at {
                    cell.elements.push(GdsElement::SRef { structure: structure.clone(), origin: o32(add(o, d)), trans, props: Vec::new() });
                }
            }
        }
        let end = cell.elements.len();
        if let Some(n) = fix {
            for e in start..end {
                cell.fixups.push((e, false, n));
            }
        }
        self.last = Some(start..end);
        Ok(())
    }

    fn text(&mut self, r: &mut Bytes) -> Result<(), String> {
        let info = r.byte()?;
        if info & 0x40 != 0 {
            self.m.text_string = Some(if info & 0x20 != 0 { NameRef::Ref(r.uint()?) } else { NameRef::Str(r.string()?) });
        }
        if info & 0x01 != 0 {
            self.m.textlayer = Some(r.uint()?);
        }
        if info & 0x02 != 0 {
            self.m.texttype = Some(r.uint()?);
        }
        let s = need(&self.m.text_string, "text-string")?;
        let (layer, datatype) = (need(&self.m.textlayer, "textlayer")? as i16, need(&self.m.texttype, "texttype")? as i16);
        let o = self.xy(r, info, 0x10, 0x08, 1)?;
        let at = self.rep(r, info & 0x04 != 0)?;
        let (text, fix) = match s {
            NameRef::Str(s) => (s, None),
            NameRef::Ref(n) => (String::new(), Some(n)),
        };
        self.emit(&at, |d| {
            let c = add(o, d);
            GdsElement::Text {
                layer,
                datatype,
                origin: (c.0 as i32, c.1 as i32),
                presentation: (0, 0),
                trans: STrans::IDENTITY,
                text: text.clone(),
                props: Vec::new(),
            }
        })?;
        if let Some(n) = fix {
            let range = self.last.clone().unwrap();
            let cell = self.cur()?;
            for e in range {
                cell.fixups.push((e, true, n));
            }
        }
        Ok(())
    }

    /// A path; its extensions, which a GDS path type cannot carry unless they are none or
    /// half the width at both ends, are moved into its end points.
    fn path(&mut self, r: &mut Bytes) -> Result<(), String> {
        let info = r.byte()?;
        let (layer, datatype) = self.layer_datatype(r, info)?;
        if info & 0x40 != 0 {
            self.m.path_halfwidth = Some(r.uint()? as i64);
        }
        let hw = need(&self.m.path_halfwidth, "path-halfwidth")?;
        if info & 0x80 != 0 {
            let scheme = r.uint()?;
            match (scheme >> 2) & 3 {
                1 => self.m.path_start_extension = Some(0),
                2 => self.m.path_start_extension = Some(hw),
                3 => self.m.path_start_extension = Some(r.sint()?),
                _ => {}
            }
            match scheme & 3 {
                1 => self.m.path_end_extension = Some(0),
                2 => self.m.path_end_extension = Some(hw),
                3 => self.m.path_end_extension = Some(r.sint()?),
                _ => {}
            }
        }
        if info & 0x20 != 0 {
            self.m.path_point_list = Some(r.pointlist(false)?);
        }
        let pl = need(&self.m.path_point_list, "path-point-list")?;
        let o = self.xy(r, info, 0x10, 0x08, 0)?;
        let at = self.rep(r, info & 0x04 != 0)?;
        let (se, ee) = (self.m.path_start_extension.unwrap_or(0), self.m.path_end_extension.unwrap_or(0));
        let mut pts: Vec<P> = std::iter::once((0, 0)).chain(pl.iter().copied()).collect();
        let path_type = if se == 0 && ee == 0 {
            0
        } else if se == hw && ee == hw {
            2
        } else {
            let n = pts.len();
            pts[0] = extend(pts[1.min(n - 1)], pts[0], se);
            pts[n - 1] = extend(pts[n.saturating_sub(2)], pts[n - 1], ee);
            0
        };
        self.emit(&at, |d| {
            let b = add(o, d);
            GdsElement::Path {
                layer,
                datatype,
                path_type,
                width: (2 * hw) as i32,
                xy: pts.iter().map(|&p| ((b.0 + p.0) as i32, (b.1 + p.1) as i32)).collect(),
                props: Vec::new(),
            }
        })
    }

    fn property(&mut self, r: &mut Bytes, repeat: bool) -> Result<(), String> {
        let (name, values) = if repeat {
            need(&self.m.last_property, "last-property")?
        } else {
            let info = r.byte()?;
            let name = if info & 0x04 != 0 {
                if info & 0x02 != 0 { NameRef::Ref(r.uint()?) } else { NameRef::Str(r.string()?) }
            } else {
                need(&self.m.last_property, "last-property-name")?.0
            };
            let values = if info & 0x08 != 0 {
                need(&self.m.last_property, "last-value-list")?.1
            } else {
                let mut n = (info >> 4) as u64;
                if n == 15 {
                    n = r.uint()?;
                }
                let mut v = Vec::with_capacity(n as usize);
                for _ in 0..n {
                    v.push(match r.uint()? {
                        0..=7 => {
                            // The type was the real's own first byte (types are below 128).
                            r.i -= 1;
                            RVal::Real(r.real()?)
                        }
                        8 => RVal::UInt(r.uint()?),
                        9 => RVal::SInt(r.sint()?),
                        10..=12 => RVal::Str(r.string()?),
                        13..=15 => RVal::Ref(r.uint()?),
                        t => return Err(format!("invalid property value type {t}")),
                    });
                }
                v
            };
            (name, values)
        };
        self.m.last_property = Some((name.clone(), values.clone()));
        if let Some(range) = self.last.clone() {
            let cell = self.cells.len() - 1;
            self.props.push(PendingProp { cell, elements: range, name, values });
        }
        Ok(())
    }

    fn resolve(&self, table: usize, n: &NameRef) -> Result<String, String> {
        match n {
            NameRef::Str(s) => Ok(s.clone()),
            NameRef::Ref(i) => self.tables[table].get(i).cloned().ok_or_else(|| format!("reference number {i} is not in its table")),
        }
    }
}

/// `p` moved `e` further along the direction from `from` to it.
fn extend(from: P, p: P, e: i64) -> P {
    let (dx, dy) = ((p.0 - from.0) as f64, (p.1 - from.1) as f64);
    let len = (dx * dx + dy * dy).sqrt();
    if len == 0.0 || e == 0 {
        return p;
    }
    (p.0 + (dx * e as f64 / len).round() as i64, p.1 + (dy * e as f64 / len).round() as i64)
}

/// Read an OASIS file: every cell with its elements, repetitions expanded except a lattice of
/// placements (an AREF), GDS properties (S_GDS_PROPERTY) on the elements they follow.
pub fn decode(bytes: &[u8]) -> Result<Layout, String> {
    let body = bytes.strip_prefix(MAGIC).ok_or("not an OASIS file: no magic bytes")?;
    let mut rd = Reader { xy_absolute: true, ..Default::default() };
    let mut r = Bytes { b: body, i: 0 };
    while !r.eof() && !rd.ended {
        rd.record(&mut r).map_err(|e| format!("at byte {}: {e}", r.i + MAGIC.len()))?;
    }
    if !rd.started {
        return Err("no START record".into());
    }
    if !rd.ended {
        return Err("no END record".into());
    }
    let mut cells = Vec::with_capacity(rd.cells.len());
    for c in &rd.cells {
        let mut elements = c.elements.clone();
        for &(e, is_text, n) in &c.fixups {
            let s = rd.resolve(if is_text { TEXTSTRINGS } else { CELLNAMES }, &NameRef::Ref(n))?;
            match &mut elements[e] {
                GdsElement::Text { text, .. } => *text = s,
                GdsElement::SRef { structure, .. } | GdsElement::ARef { structure, .. } => *structure = s,
                _ => {}
            }
        }
        cells.push(Cell { name: rd.resolve(CELLNAMES, &c.name)?, elements });
    }
    for p in &rd.props {
        if rd.resolve(PROPNAMES, &p.name)? != S_GDS_PROPERTY {
            continue;
        }
        let [attr, value] = &p.values[..] else { continue };
        let attr = match attr {
            RVal::UInt(u) => *u as i16,
            RVal::SInt(s) => *s as i16,
            _ => continue,
        };
        let value = match value {
            RVal::Str(s) => s.clone(),
            RVal::Ref(n) => rd.resolve(PROPSTRINGS, &NameRef::Ref(*n))?,
            RVal::UInt(u) => u.to_string(),
            RVal::SInt(s) => s.to_string(),
            RVal::Real(f) => f.to_string(),
        };
        for e in p.elements.clone() {
            match &mut cells[p.cell].elements[e] {
                GdsElement::Boundary { props, .. }
                | GdsElement::Box { props, .. }
                | GdsElement::Path { props, .. }
                | GdsElement::SRef { props, .. }
                | GdsElement::ARef { props, .. }
                | GdsElement::Text { props, .. } => props.push((attr, value.clone())),
            }
        }
    }
    Ok(Layout { unit: rd.unit, cells })
}

/// Whether a path names an OASIS file by its extension (`.oas`, `.oasis`, any case).
pub fn is_oasis_path(path: impl AsRef<std::path::Path>) -> bool {
    path.as_ref()
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("oas") || e.eq_ignore_ascii_case("oasis"))
}

impl crate::Db {
    /// Write a GDS library as OASIS. The grid is the library's database unit.
    pub fn oasis_write(&self, lib: crate::gds::GdsLib, path: impl AsRef<std::path::Path>, opts: &WriteOptions) -> crate::Result<()> {
        let path = path.as_ref();
        let (_, metres_per_dbu) = self.gds_units(lib)?;
        let mut unit = 1e-6 / metres_per_dbu;
        if (unit - unit.round()).abs() < 1e-9 * unit.max(1.0) {
            unit = unit.round();
        }
        let mut cells = Vec::new();
        for name in self.gds_structures(lib)? {
            let elements = self.gds_elements(lib, &name)?;
            cells.push(Cell { name, elements });
        }
        let bytes = encode(&Layout { unit, cells }, opts).map_err(|e| crate::Error::Odb(format!("{}: {e}", path.display())))?;
        std::fs::write(path, bytes).map_err(|e| crate::Error::Odb(format!("{}: {e}", path.display())))
    }

    /// Read an OASIS file into a new GDS library named after the file, in micron user units.
    pub fn oasis_read(&mut self, path: impl AsRef<std::path::Path>) -> crate::Result<crate::gds::GdsLib> {
        let path = path.as_ref();
        let err = |e: String| crate::Error::Odb(format!("{}: {e}", path.display()));
        let bytes = std::fs::read(path).map_err(|e| err(e.to_string()))?;
        let layout = decode(&bytes).map_err(err)?;
        if layout.unit.is_nan() || layout.unit <= 0.0 {
            return Err(err(format!("invalid grid {} per micron", layout.unit)));
        }
        let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("LIB");
        let lib = self.gds_create_lib(name)?;
        self.gds_set_units(lib, 1.0 / layout.unit, 1e-6 / layout.unit)?;
        for c in &layout.cells {
            self.gds_create_structure(lib, &c.name)?;
        }
        for c in &layout.cells {
            self.gds_add(lib, &c.name, &c.elements)?;
        }
        Ok(lib)
    }

    /// Read a layout file, OASIS or GDS by its extension.
    pub fn layout_read(&mut self, path: impl AsRef<std::path::Path>) -> crate::Result<crate::gds::GdsLib> {
        if is_oasis_path(&path) {
            self.oasis_read(path)
        } else {
            self.gds_read(path)
        }
    }

    /// Write a library as OASIS or GDS by the path's extension.
    pub fn layout_write(&self, lib: crate::gds::GdsLib, path: impl AsRef<std::path::Path>) -> crate::Result<()> {
        if is_oasis_path(&path) {
            self.oasis_write(lib, path, &WriteOptions::default())
        } else {
            self.gds_write(lib, path)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(f: impl FnOnce(&mut Sink)) -> Vec<u8> {
        let mut s = Sink::default();
        f(&mut s);
        s.chunks.concat()
    }

    fn rd(b: &[u8]) -> Bytes<'_> {
        Bytes { b, i: 0 }
    }

    /// Integers are seven bits a byte, low first; a signed one carries its sign in bit 0.
    #[test]
    fn integers_are_base_128_with_the_sign_in_the_low_bit() {
        assert_eq!(enc(|s| s.uint(0)), [0]);
        assert_eq!(enc(|s| s.uint(127)), [0x7f]);
        assert_eq!(enc(|s| s.uint(128)), [0x80, 0x01]);
        assert_eq!(enc(|s| s.uint(300)), [0xac, 0x02]);
        assert_eq!(enc(|s| s.sint(1)), [0x02]);
        assert_eq!(enc(|s| s.sint(-1)), [0x03]);
        assert_eq!(enc(|s| s.sint(-64)), [0x81, 0x01]);
        for v in [0i64, 1, -1, 63, -64, 1 << 40, -(1 << 40), i32::MIN as i64] {
            assert_eq!(rd(&enc(|s| s.sint(v))).sint().unwrap(), v);
        }
    }

    /// A whole real of magnitude at least 0.5 is written as an integer (type 0, or 1 when
    /// negative); 0.5 and anything fractional as an 8-byte double (type 7).
    #[test]
    fn whole_reals_are_integers_and_the_rest_doubles() {
        assert_eq!(enc(|s| s.real(1000.0)), [0, 0xe8, 0x07]);
        assert_eq!(enc(|s| s.real(-90.0)), [1, 90]);
        assert_eq!(enc(|s| s.real(0.0))[0], 7);
        assert_eq!(enc(|s| s.real(0.5))[0], 7);
        assert_eq!(enc(|s| s.real(1.5))[0], 7);
        for v in [1000.0, -90.0, 0.0, 0.25, 1e-3, 12.5] {
            assert_eq!(rd(&enc(|s| s.real(v))).real().unwrap(), v);
        }
    }

    /// A g-delta along an octangular direction is one integer `(len << 4) | (dir << 1)`, E N W S
    /// NE NW SW SE = 0..7; any other is `(|x| << 2) | sign | 1` and then y.
    #[test]
    fn gdeltas_use_the_short_form_on_the_eight_directions() {
        assert_eq!(enc(|s| s.gdelta((5, 0))), [5 << 4]);
        assert_eq!(enc(|s| s.gdelta((0, 5))), [(5 << 4) | 2]);
        assert_eq!(enc(|s| s.gdelta((-5, 0))), [(5 << 4) | 4]);
        assert_eq!(enc(|s| s.gdelta((0, -5))), [(5 << 4) | 6]);
        assert_eq!(enc(|s| s.gdelta((3, 3))), [(3 << 4) | 8]);
        assert_eq!(enc(|s| s.gdelta((-3, 3))), [(3 << 4) | 10]);
        assert_eq!(enc(|s| s.gdelta((-3, -3))), [(3 << 4) | 12]);
        assert_eq!(enc(|s| s.gdelta((3, -3))), [(3 << 4) | 14]);
        assert_eq!(enc(|s| s.gdelta((3, 1))), [(3 << 2) | 1, 2]);
        assert_eq!(enc(|s| s.gdelta((-3, 1))), [(3 << 2) | 3, 2]);
        for p in [(0, 0), (7, 0), (0, -9), (4, 4), (-4, 4), (3, 1), (-3, -100), (1 << 20, -7)] {
            assert_eq!(rd(&enc(|s| s.gdelta(p))).gdelta().unwrap(), p);
        }
    }

    fn pointlist_bytes(pl: &[P], poly: bool) -> Vec<u8> {
        let mut w = Writer::new(false);
        w.write_pointlist(pl, poly);
        w.sink.chunks.concat()
    }

    /// A Manhattan polygon alternating from horizontal is type 0 and drops its last point,
    /// which the closing edges imply; the reader puts it back.
    #[test]
    fn a_manhattan_polygon_drops_the_point_its_closing_edges_imply() {
        // An L: start (0,0) → (10,0) → (10,5) → (5,5) → (5,10) → (0,10).
        let pl = vec![(10, 0), (10, 5), (5, 5), (5, 10), (0, 10)];
        let b = pointlist_bytes(&pl, true);
        assert_eq!(b[0], 0, "horizontal first");
        assert_eq!(b[1], 4, "five points, one implied");
        assert_eq!(rd(&b).pointlist(true).unwrap(), pl);
        // Vertical first.
        let pl = vec![(0, 10), (5, 10), (5, 5), (10, 5), (10, 0)];
        let b = pointlist_bytes(&pl, true);
        assert_eq!(b[0], 1);
        assert_eq!(rd(&b).pointlist(true).unwrap(), pl);
        // A path keeps every point.
        let pl = vec![(10, 0), (10, 5)];
        let b = pointlist_bytes(&pl, false);
        assert_eq!(&b[..2], &[0, 2]);
        assert_eq!(rd(&b).pointlist(false).unwrap(), pl);
    }

    /// A list that is not alternating Manhattan, or a polygon whose last edge does not close
    /// on the start, is type 4: g-deltas, every point.
    #[test]
    fn anything_else_is_a_list_of_gdeltas() {
        let tri = vec![(10, 0), (5, 7)];
        let b = pointlist_bytes(&tri, true);
        assert_eq!(&b[..2], &[4, 2]);
        assert_eq!(rd(&b).pointlist(true).unwrap(), tri);
        // Two horizontal edges in a row.
        let b = pointlist_bytes(&[(5, 0), (10, 0), (10, 5)], false);
        assert_eq!(b[0], 4);
    }

    /// Fewer than ten copies, or level 0: one irregular repetition, ordered by y then x.
    #[test]
    fn a_small_group_is_one_irregular_repetition_sorted_by_y_then_x() {
        // By y then x: (3,0) (5,0) (0,1); by x then y it would start at (0,1).
        let out = compress(1, vec![(5, 0), (0, 1), (3, 0)]);
        assert_eq!(out, vec![((3, 0), Some(Rep::Irregular(vec![(2, 0), (-3, 1)])))]);
        assert_eq!(compress(1, vec![(4, 4)]), vec![((4, 4), None)]);
        let row: Vec<P> = (0..12).map(|i| (i * 10, 0)).collect();
        assert!(matches!(&compress(0, row)[..], [(_, Some(Rep::Irregular(_)))]));
    }

    /// Level 1: a 4×3 lattice is found as columns in y and stacked in x into one 2-D array;
    /// a stray copy is left to the irregular repetition.
    #[test]
    fn level_one_finds_a_lattice_and_leaves_the_strays() {
        let mut d: Vec<P> = Vec::new();
        for i in 0..4 {
            for j in 0..3 {
                d.push((i * 100, j * 50));
            }
        }
        d.push((7, 7));
        let out = compress(1, d);
        assert_eq!(out.len(), 2, "{out:?}");
        assert_eq!(out[0], ((0, 0), Some(Rep::Regular { a: (0, 50), b: (100, 0), n: 3, m: 4 })));
        assert_eq!(out[1], ((7, 7), None));
        // Every copy is placed exactly once.
        let mut all: Vec<P> = out.iter().flat_map(|(p, r)| r.as_ref().map(|r| r.points()).unwrap_or(vec![(0, 0)]).into_iter().map(move |d| add(*p, d))).collect();
        all.sort();
        assert_eq!(all.len(), 13);
        all.dedup();
        assert_eq!(all.len(), 13);
    }

    /// A copy at the same point as another is never a zero step: it goes in a pass of its own,
    /// so every copy is placed once and no repetition holds (0, 0).
    #[test]
    fn duplicate_copies_are_never_a_zero_displacement() {
        for level in [0, 1] {
            let mut d: Vec<P> = (0..12).map(|i| (i * 10, 0)).collect();
            d.extend([(0, 0), (0, 0), (30, 0)]);
            let out = compress(level, d);
            let mut all = Vec::new();
            for (p, r) in &out {
                match r {
                    Some(Rep::Irregular(pts)) => assert!(pts.iter().all(|&q| q != (0, 0)), "{out:?}"),
                    Some(Rep::Regular { a, b, n, m }) => {
                        assert!(*a != (0, 0) || *n == 1, "{out:?}");
                        assert!(*b != (0, 0) || *m == 1, "{out:?}");
                    }
                    None => {}
                }
                all.extend(r.as_ref().map(|r| r.points()).unwrap_or(vec![(0, 0)]).into_iter().map(|q| add(*p, q)));
            }
            all.sort();
            let mut want: Vec<P> = (0..12).map(|i| (i * 10, 0)).chain([(0, 0), (0, 0), (30, 0)]).collect();
            want.sort();
            assert_eq!(all, want, "level {level}");
        }
    }

    /// Read as KLayout reads it: a zero step in an irregular repetition is no copy, a zero-pitch
    /// lattice axis one copy.
    #[test]
    fn a_zero_displacement_is_read_as_no_copy() {
        let rep = rd(&enc(|s| {
            s.uint(10);
            s.uint(1);
            s.gdelta((0, 0));
            s.gdelta((5, 0));
        }))
        .repetition(&None)
        .unwrap();
        assert_eq!(rep.points(), vec![(0, 0), (5, 0)]);
        let rep = rd(&enc(|s| {
            s.uint(2);
            s.uint(3);
            s.uint(0);
        }))
        .repetition(&None)
        .unwrap();
        assert_eq!(rep.points(), vec![(0, 0)]);
    }

    /// A run of two is not worth an array at level 1: runs need three.
    #[test]
    fn level_one_needs_runs_of_three() {
        let d: Vec<P> = (0..10).map(|i| (i * 1000 + (i % 2) * 7, (i / 2) * 3)).collect();
        let out = compress(1, d);
        assert!(out.iter().all(|(_, r)| !matches!(r, Some(Rep::Regular { .. }))), "{out:?}");
    }

    fn rep_bytes(rep: &Rep) -> Vec<u8> {
        let mut w = Writer::new(false);
        w.write_repetition(rep);
        w.sink.chunks.concat()
    }

    /// Lattices: orthogonal with positive pitches type 1 (either axis order), one row along x
    /// type 2, along y type 3, along anything else type 9, oblique type 8; and each reads back.
    #[test]
    fn repetitions_take_the_shortest_type_and_read_back() {
        let cases = [
            (Rep::Regular { a: (10, 0), b: (0, 20), n: 3, m: 4 }, 1),
            (Rep::Regular { a: (0, 20), b: (10, 0), n: 4, m: 3 }, 1),
            (Rep::Regular { a: (10, 0), b: (0, 0), n: 3, m: 1 }, 2),
            (Rep::Regular { a: (0, 7), b: (0, 0), n: 5, m: 1 }, 3),
            (Rep::Regular { a: (3, 3), b: (0, 0), n: 5, m: 1 }, 9),
            (Rep::Regular { a: (10, 2), b: (1, 20), n: 3, m: 2 }, 8),
            (Rep::Irregular(vec![(3, 0), (5, 1)]), 10),
            (Rep::Irregular(vec![(30, 0), (50, 10)]), 11),
        ];
        for (rep, ty) in cases {
            let b = rep_bytes(&rep);
            assert_eq!(b[0], ty, "{rep:?}");
            let mut want = rep.points();
            let mut got = rd(&b).repetition(&None).unwrap().points();
            want.sort();
            got.sort();
            assert_eq!(got, want, "{rep:?}");
        }
        // The same repetition again is a reuse, one zero byte.
        let mut w = Writer::new(false);
        let rep = Rep::Irregular(vec![(3, 0)]);
        w.write_repetition(&rep);
        let n = w.sink.len();
        w.write_repetition(&rep);
        assert_eq!(w.sink.len(), n + 1);
        assert_eq!(*w.sink.chunks[0].last().unwrap(), 0);
    }

    /// Extension scheme: two bits for each end, 01 none, 10 half the width, 11 given; 00 the
    /// modal value, written only when it changed.
    #[test]
    fn a_path_names_its_extensions_by_scheme() {
        let mut w = Writer::new(false);
        w.write_path(2, 20, &[(0, 0), (100, 0)], &[], None).unwrap();
        let b = w.sink.chunks.concat();
        // record, info (E W P X? Y? R D L), layer, datatype, half-width, scheme.
        assert_eq!(b[0], 22);
        assert_eq!(b[1] & 0xc0, 0xc0);
        assert_eq!(b[5], 0x0a, "half-width at both ends");
        let mut w = Writer::new(false);
        w.write_path(0, 20, &[(0, 0), (100, 0)], &[], None).unwrap();
        assert_eq!(w.sink.chunks.concat()[5], 0x05, "flush at both ends");
        let mut w = Writer::new(false);
        assert!(w.write_path(0, 21, &[(0, 0), (100, 0)], &[], None).is_err(), "odd width refused");
    }

    /// A square RECTANGLE sets S and writes its width alone; a rectangular BOUNDARY is a
    /// RECTANGLE, anything else (or a degenerate rectangle) a polygon.
    #[test]
    fn a_square_writes_its_width_once_and_a_rectangular_boundary_is_a_rectangle() {
        let mut w = Writer::new(false);
        w.write_box((0, 0), (170, 170), &[], None);
        assert_eq!(w.sink.chunks.concat(), [20, 0xc3, 0, 0, 0xaa, 0x01]);
        let mut w = Writer::new(false);
        w.write_box((0, 0), (170, 100), &[], None);
        assert_eq!(w.sink.chunks.concat(), [20, 0x63, 0, 0, 0xaa, 0x01, 100]);
        assert_eq!(as_box(&[(0, 0), (10, 0), (10, 5), (0, 5), (0, 0)]), Some(((0, 0), (10, 5))));
        assert_eq!(as_box(&[(10, 5), (0, 5), (0, 0), (10, 0), (10, 5)]), Some(((0, 0), (10, 5))));
        assert_eq!(as_box(&[(0, 0), (10, 0), (12, 5), (0, 5), (0, 0)]), None);
        assert_eq!(as_box(&[(0, 0), (10, 0), (10, 0), (0, 0), (0, 0)]), None);
        assert_eq!(as_box(&[(0, 0), (10, 0), (0, 5), (10, 5), (0, 0)]), None, "a bow tie");
    }

    /// A GDS property is S_GDS_PROPERTY (id 1) with an unsigned attribute and a string by
    /// reference; the same property again is PROPERTY-REPEAT.
    #[test]
    fn a_gds_property_is_s_gds_property_and_repeats_as_one_byte() {
        let mut w = Writer::new(false);
        w.write_props(&[(1, "abc".into())]);
        let b = w.sink.chunks.concat();
        assert_eq!(b, [28, 0x27, 1, 8, 1, 15, 0]);
        w.write_props(&[(1, "abc".into())]);
        assert_eq!(*w.sink.chunks.concat().last().unwrap(), 29);
        assert_eq!(string_type("a b"), 0);
        assert_eq!(string_type("ab\x01"), 1);
        assert_eq!(string_type(""), 0);
    }

    fn sample() -> Layout {
        let mut inv = Vec::new();
        for i in 0..12 {
            inv.push(GdsElement::rect((68, 20), i * 460, 0, i * 460 + 170, 170));
        }
        inv.push(GdsElement::Boundary { layer: 66, datatype: 20, xy: vec![(0, 0), (100, 0), (50, 80), (0, 0)], props: vec![(1, "tri".into())] });
        inv.push(GdsElement::Path { layer: 68, datatype: 20, path_type: 2, width: 140, xy: vec![(0, 0), (0, 500), (300, 500)], props: Vec::new() });
        inv.push(GdsElement::Text { layer: 68, datatype: 5, origin: (85, 85), presentation: (0, 0), trans: STrans::IDENTITY, text: "A".into(), props: Vec::new() });
        let mut top = vec![
            GdsElement::SRef { structure: "inv".into(), origin: (0, 0), trans: STrans::IDENTITY, props: Vec::new() },
            GdsElement::SRef { structure: "inv".into(), origin: (5000, 2720), trans: STrans { flip_x: true, mag: 1.0, angle: 180.0 }, props: Vec::new() },
            GdsElement::ARef {
                structure: "inv".into(),
                origin: (0, 10000),
                lr: (3 * 1380, 10000),
                ul: (0, 10000 + 2 * 2720),
                rows: 2,
                columns: 3,
                trans: STrans::IDENTITY,
                props: Vec::new(),
            },
        ];
        top.push(GdsElement::SRef { structure: "inv".into(), origin: (9, 9), trans: STrans { flip_x: false, mag: 2.0, angle: 45.0 }, props: vec![(7, "x".into())] });
        Layout { unit: 1000.0, cells: vec![Cell { name: "top".into(), elements: top }, Cell { name: "inv".into(), elements: inv }] }
    }

    /// Every element reads back as written, but for what OASIS cannot carry: repetitions come
    /// back expanded and a rectangle as a BOUNDARY.
    #[test]
    fn a_layout_reads_back_as_written() {
        for opts in [WriteOptions::default(), WriteOptions { compression_level: 0, cblocks: false }] {
            let l = sample();
            let b = encode(&l, &opts).unwrap();
            let back = decode(&b).unwrap();
            assert_eq!(back.unit, 1000.0);
            assert_eq!(back.cells.len(), 2);
            for c in &l.cells {
                let got = &back.cells.iter().find(|x| x.name == c.name).unwrap().elements;
                let canon = |v: &[GdsElement]| {
                    let mut s: Vec<String> = v.iter().map(|e| format!("{e:?}")).collect();
                    s.sort();
                    s
                };
                assert_eq!(canon(got), canon(&c.elements), "cell {}", c.name);
            }
        }
    }

    /// The file ends with END at 256 bytes from its start, and END's offsets point at the
    /// tables; each S_CELL_OFFSET points at its CELL record.
    #[test]
    fn end_is_256_bytes_and_cell_offsets_point_at_cell_records() {
        let l = sample();
        let b = encode(&l, &WriteOptions { compression_level: 1, cblocks: false }).unwrap();
        let end = b.len() - 256;
        assert_eq!(b[end], 2);
        let mut r = rd(&b[end + 1..]);
        let mut offs = Vec::new();
        for _ in 0..6 {
            assert_eq!(r.uint().unwrap(), 1);
            offs.push(r.uint().unwrap() as usize);
        }
        assert_eq!(b[offs[0]], 3, "cell-name table");
        assert_eq!(b[offs[1]], 5, "text-string table");
        assert_eq!(b[offs[2]], 7, "property-name table");
        assert_eq!(b[offs[3]], 9, "property-string table");
        // The cell-name table: CELLNAME "top", then S_CELL_OFFSET pointing at CELL 0.
        let mut r = rd(&b[offs[0] + 1..]);
        assert_eq!(r.string().unwrap(), "top");
        assert_eq!(r.uint().unwrap(), 28);
        assert_eq!(r.byte().unwrap(), 0x17);
        assert_eq!(r.uint().unwrap(), 0);
        assert_eq!(r.uint().unwrap(), 8);
        let at = r.uint().unwrap() as usize;
        assert_eq!(&b[at..at + 2], &[13, 0]);
        // inv is written before top, which references it.
        let mut r = rd(&b[offs[0] + 1..]);
        r.string().unwrap();
        for _ in 0..5 {
            r.uint().unwrap();
        }
        assert_eq!(r.uint().unwrap(), 3);
        assert_eq!(r.string().unwrap(), "inv");
        let mut r2 = r;
        for _ in 0..4 {
            r2.uint().unwrap();
        }
        assert!((r2.uint().unwrap() as usize) < at, "inv before top");
    }

    /// Refused as KLayout refuses: a cell referencing one not in the layout, and a polygon of
    /// fewer than three points.
    #[test]
    fn what_has_no_encoding_is_refused() {
        let mut l = sample();
        l.cells[0].elements.push(GdsElement::SRef { structure: "nope".into(), origin: (0, 0), trans: STrans::IDENTITY, props: Vec::new() });
        assert!(encode(&l, &WriteOptions::default()).unwrap_err().contains("nope"));
        let l = Layout { unit: 1000.0, cells: vec![Cell { name: "c".into(), elements: vec![GdsElement::Boundary { layer: 1, datatype: 0, xy: vec![(0, 0), (5, 5), (0, 0)], props: Vec::new() }] }] };
        assert!(encode(&l, &WriteOptions::default()).unwrap_err().contains("three points"));
    }

    /// Path extensions a GDS path type cannot carry move into the end points.
    #[test]
    fn explicit_extensions_move_into_the_end_points() {
        assert_eq!(extend((0, 0), (100, 0), 30), (130, 0));
        assert_eq!(extend((100, 0), (0, 0), 30), (-30, 0));
        assert_eq!(extend((0, 0), (0, 0), 30), (0, 0));
    }

    /// CTRAPEZOID type 0 is a right trapezoid with its slant at the top right.
    #[test]
    fn ctrapezoid_corners_follow_the_table() {
        let (w, h) = (10, 4);
        let pts: Vec<P> = CTRAPS[0].iter().map(|&(a, b, c, d)| (a * w + b * h, c * w + d * h)).collect();
        assert_eq!(pts, vec![(0, 0), (0, 4), (6, 4), (10, 0)]);
    }
}
