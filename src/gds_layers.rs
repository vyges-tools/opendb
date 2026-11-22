// SPDX-License-Identifier: Apache-2.0
//! The layers a finished layout actually uses, and the two tapeout checks made on them.
//!
//! A foundry accepts a tapeout layout only on the GDS layer/datatype pairs it lists (its
//! allowed-layers table), and on a multi-patterned layer only shapes that carry a mask: there,
//! an uncoloured shape has no mask to be written to. Both are properties of the FILE that goes
//! to the foundry, whatever wrote it (our stream-out, another writer, a cell library), so they are
//! checked on that file, read here with no shared code with any writer:
//!
//! * [`census_path`] lists every (layer, datatype) pair the file uses, by element kind
//!   (BOUNDARY, PATH, BOX, TEXT, NODE; a TEXT's pair is its layer and TEXTTYPE, a BOX's its
//!   BOXTYPE, a NODE's its NODETYPE), with the structures that hold them. Structures are read as
//!   defined, not flattened: a pair is used if any structure in the file holds it, referenced or
//!   not.
//! * [`not_allowed`]: the pairs the file uses that are not in the allowed list.
//! * [`uncoloured_targets`] / [`uncoloured_found`]: the pairs a layer map sends an UNCOLOURED
//!   shape of a multi-mask layer to (its entries without `:MASK:n`, minus any pair a masked entry
//!   also uses), and the drawn shapes (BOUNDARY, PATH, BOX; not text) the file holds on them.
//! * [`map_not_allowed`]: the map's own targets outside the allowed list, so a map that would
//!   emit a pair the foundry does not take is caught before any stream-out.
//!
//! Nothing here embeds any foundry's list or map: both are input files, read at run time.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::Path;

pub use crate::gds::Ld;
use crate::gds::{GdsElement, LayerMap};

/// The element kinds that carry a layer.
pub const KINDS: [&str; 5] = ["boundary", "path", "box", "text", "node"];

/// One pair's use in the file.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PairUse {
    /// Elements by kind ([`KINDS`]).
    pub by_kind: BTreeMap<&'static str, usize>,
    /// Structures holding at least one element on the pair.
    pub structures: BTreeSet<String>,
}

impl PairUse {
    /// Drawn elements: BOUNDARY, PATH and BOX (text and nodes are no metal).
    pub fn drawn(&self) -> usize {
        ["boundary", "path", "box"].iter().map(|k| self.by_kind.get(k).copied().unwrap_or(0)).sum()
    }
}

/// Every pair a layout file uses.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Census {
    pub format: &'static str,
    pub structures: usize,
    pub elements: usize,
    pub pairs: BTreeMap<Ld, PairUse>,
}

impl Census {
    fn add(&mut self, kind: &'static str, ld: Ld, structure: &str) {
        self.elements += 1;
        let u = self.pairs.entry(ld).or_default();
        *u.by_kind.entry(kind).or_default() += 1;
        if !u.structures.contains(structure) {
            u.structures.insert(structure.to_string());
        }
    }
}

// ---- GDSII records ------------------------------------------------------------------------------

const ENDLIB: u8 = 0x04;
const BGNSTR: u8 = 0x05;
const STRNAME: u8 = 0x06;
const ENDSTR: u8 = 0x07;
const BOUNDARY: u8 = 0x08;
const PATH: u8 = 0x09;
const TEXT: u8 = 0x0C;
const LAYER: u8 = 0x0D;
const DATATYPE: u8 = 0x0E;
const ENDEL: u8 = 0x11;
const NODE: u8 = 0x15;
const TEXTTYPE: u8 = 0x16;
const NODETYPE: u8 = 0x2A;
const BOX: u8 = 0x2D;
const BOXTYPE: u8 = 0x2E;

/// The element being read: its kind, layer and type record, as they arrive.
#[derive(Default)]
struct Open {
    kind: Option<&'static str>,
    layer: Option<i16>,
    ty: Option<i16>,
}

fn int2(data: &[u8]) -> Result<i16, String> {
    match data {
        [a, b, ..] => Ok(i16::from_be_bytes([*a, *b])),
        _ => Err("a two-byte integer record holds fewer than two bytes".into()),
    }
}

/// Census of a GDSII stream, record by record (the file is never held whole). An element with
/// no layer or no type record, a record cut short, or a stream without ENDLIB is an error: a
/// census of a file that could not be read to its end would say less than the file holds.
pub fn census_gds(mut r: impl Read) -> Result<Census, String> {
    let mut c = Census { format: "gds", ..Census::default() };
    let mut structure = String::new();
    let mut el = Open::default();
    let mut head = [0u8; 4];
    let mut data = Vec::new();
    let mut offset: u64 = 0;
    loop {
        match r.read_exact(&mut head) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Err(format!("GDS stream ends at byte {offset} without ENDLIB"));
            }
            Err(e) => return Err(format!("GDS read at byte {offset}: {e}")),
        }
        let len = u16::from_be_bytes([head[0], head[1]]) as usize;
        let rec = head[2];
        if len < 4 {
            return Err(format!("GDS record at byte {offset} has length {len} (below its own header)"));
        }
        data.resize(len - 4, 0);
        r.read_exact(&mut data).map_err(|e| format!("GDS record at byte {offset} cut short: {e}"))?;
        offset += len as u64;
        match rec {
            ENDLIB => return Ok(c),
            BGNSTR => c.structures += 1,
            STRNAME => {
                structure = String::from_utf8_lossy(&data).trim_end_matches('\0').to_string();
            }
            ENDSTR => structure.clear(),
            BOUNDARY => el = Open { kind: Some("boundary"), ..Open::default() },
            PATH => el = Open { kind: Some("path"), ..Open::default() },
            TEXT => el = Open { kind: Some("text"), ..Open::default() },
            NODE => el = Open { kind: Some("node"), ..Open::default() },
            BOX => el = Open { kind: Some("box"), ..Open::default() },
            LAYER if el.kind.is_some() => el.layer = Some(int2(&data)?),
            DATATYPE | TEXTTYPE | NODETYPE | BOXTYPE if el.kind.is_some() => el.ty = Some(int2(&data)?),
            ENDEL => {
                if let Some(kind) = el.kind {
                    match (el.layer, el.ty) {
                        (Some(l), Some(t)) => c.add(kind, (l, t), &structure),
                        _ => return Err(format!("GDS {kind} in structure '{structure}' ending at byte {offset} has no layer or no type")),
                    }
                }
                el = Open::default();
            }
            _ => {}
        }
    }
}

/// Census of an OASIS file, from the decoded layout ([`crate::oasis::decode`]).
pub fn census_oasis(bytes: &[u8]) -> Result<Census, String> {
    let layout = crate::oasis::decode(bytes)?;
    let mut c = Census { format: "oasis", structures: layout.cells.len(), ..Census::default() };
    for cell in &layout.cells {
        for e in &cell.elements {
            let (kind, ld) = match e {
                GdsElement::Boundary { layer, datatype, .. } => ("boundary", (*layer, *datatype)),
                GdsElement::Path { layer, datatype, .. } => ("path", (*layer, *datatype)),
                GdsElement::Box { layer, datatype, .. } => ("box", (*layer, *datatype)),
                GdsElement::Text { layer, datatype, .. } => ("text", (*layer, *datatype)),
                GdsElement::SRef { .. } | GdsElement::ARef { .. } => continue,
            };
            c.add(kind, ld, &cell.name);
        }
    }
    Ok(c)
}

/// Census of a layout file: OASIS by its extension ([`crate::oasis::is_oasis_path`]), else GDSII.
pub fn census_path(path: impl AsRef<Path>) -> Result<Census, String> {
    let p = path.as_ref();
    if crate::oasis::is_oasis_path(p) {
        let bytes = std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))?;
        census_oasis(&bytes)
    } else {
        let f = std::fs::File::open(p).map_err(|e| format!("{}: {e}", p.display()))?;
        census_gds(std::io::BufReader::with_capacity(1 << 20, f)).map_err(|e| format!("{}: {e}", p.display()))
    }
}

// ---- the checks ---------------------------------------------------------------------------------

/// The pairs the file uses that are not in `allowed`.
pub fn not_allowed(c: &Census, allowed: &BTreeSet<Ld>) -> Vec<Ld> {
    c.pairs.keys().filter(|ld| !allowed.contains(ld)).copied().collect()
}

/// The pairs `map` sends an uncoloured shape of each layer in `multi_mask` to: its entries for
/// that layer without a mask, any purpose, minus every pair some masked entry (of any layer) also
/// targets — a pair that also receives coloured shapes cannot tell the two apart. Each pair with
/// the layers that send uncoloured shapes there.
pub fn uncoloured_targets(map: &LayerMap, multi_mask: &BTreeSet<String>) -> BTreeMap<Ld, BTreeSet<String>> {
    let masked: BTreeSet<Ld> = map.entries().filter(|e| e.2 != 0).flat_map(|e| e.3.iter().copied()).collect();
    let mut out: BTreeMap<Ld, BTreeSet<String>> = BTreeMap::new();
    for (layer, _purpose, mask, lds) in map.entries() {
        if mask != 0 || !multi_mask.contains(layer) {
            continue;
        }
        for ld in lds.iter().filter(|ld| !masked.contains(ld)) {
            out.entry(*ld).or_default().insert(layer.to_string());
        }
    }
    out
}

/// One pair holding uncoloured drawn shapes of a multi-mask layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Uncoloured {
    pub ld: Ld,
    pub layers: BTreeSet<String>,
    /// BOUNDARY + PATH + BOX elements on the pair.
    pub drawn: usize,
    pub structures: BTreeSet<String>,
}

/// The drawn shapes (not text, not nodes) the file holds on an uncoloured target.
pub fn uncoloured_found(c: &Census, targets: &BTreeMap<Ld, BTreeSet<String>>) -> Vec<Uncoloured> {
    targets
        .iter()
        .filter_map(|(ld, layers)| {
            let u = c.pairs.get(ld)?;
            (u.drawn() > 0).then(|| Uncoloured { ld: *ld, layers: layers.clone(), drawn: u.drawn(), structures: u.structures.clone() })
        })
        .collect()
}

/// The map's own entries whose target is outside `allowed`, one
/// `<layer> <purpose>[:MASK:<n>] <gds layer> <datatype>` line each, sorted.
pub fn map_not_allowed(map: &LayerMap, allowed: &BTreeSet<Ld>) -> Vec<String> {
    map.clone().restrict_to(allowed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One GDS record.
    fn rec(ty: u8, dt: u8, data: &[u8]) -> Vec<u8> {
        let mut v = ((data.len() + 4) as u16).to_be_bytes().to_vec();
        v.extend([ty, dt]);
        v.extend(data);
        v
    }

    fn i2(v: i16) -> Vec<u8> {
        v.to_be_bytes().to_vec()
    }

    /// A stream with one structure `TOP` holding the given (kind record, type record, layer, type).
    fn stream(elements: &[(u8, u8, i16, i16)], end: bool) -> Vec<u8> {
        let mut s = rec(0x00, 0x02, &i2(600));
        s.extend(rec(0x01, 0x02, &[0; 24]));
        s.extend(rec(0x02, 0x06, b"LIB\0"));
        s.extend(rec(BGNSTR, 0x02, &[0; 24]));
        s.extend(rec(STRNAME, 0x06, b"TOP\0"));
        for &(kind, ty, l, d) in elements {
            s.extend(rec(kind, 0x00, &[]));
            s.extend(rec(LAYER, 0x02, &i2(l)));
            s.extend(rec(ty, 0x02, &i2(d)));
            s.extend(rec(0x10, 0x03, &[0; 8]));
            s.extend(rec(ENDEL, 0x00, &[]));
        }
        s.extend(rec(ENDSTR, 0x00, &[]));
        if end {
            s.extend(rec(ENDLIB, 0x00, &[]));
        }
        s
    }

    // Each element kind's pair is its LAYER and its own type record (DATATYPE, TEXTTYPE,
    // BOXTYPE, NODETYPE); values above 255 are read whole.
    #[test]
    fn the_census_reads_each_kinds_layer_and_type_record() {
        let s = stream(
            &[(BOUNDARY, DATATYPE, 15, 0), (PATH, DATATYPE, 15, 0), (BOX, BOXTYPE, 15, 236), (TEXT, TEXTTYPE, 15, 20), (NODE, NODETYPE, 4373, 4021)],
            true,
        );
        let c = census_gds(s.as_slice()).unwrap();
        assert_eq!((c.structures, c.elements), (1, 5));
        assert_eq!(c.pairs.keys().copied().collect::<Vec<_>>(), [(15, 0), (15, 20), (15, 236), (4373, 4021)]);
        assert_eq!(c.pairs[&(15, 0)].drawn(), 2);
        assert_eq!(c.pairs[&(15, 20)].drawn(), 0, "text is no drawn shape");
        assert_eq!(c.pairs[&(4373, 4021)].drawn(), 0, "a node is no drawn shape");
        assert_eq!(c.pairs[&(15, 236)].structures.iter().collect::<Vec<_>>(), ["TOP"]);
    }

    // A file that cannot be read to ENDLIB is an error, never a shorter census.
    #[test]
    fn a_stream_cut_short_is_an_error() {
        let s = stream(&[(BOUNDARY, DATATYPE, 1, 0)], false);
        assert!(census_gds(s.as_slice()).unwrap_err().contains("without ENDLIB"));
        let full = stream(&[(BOUNDARY, DATATYPE, 1, 0)], true);
        assert!(census_gds(&full[..full.len() - 6]).is_err());
        let mut no_type = rec(BGNSTR, 0x02, &[0; 24]);
        no_type.extend(rec(BOUNDARY, 0x00, &[]));
        no_type.extend(rec(LAYER, 0x02, &i2(1)));
        no_type.extend(rec(ENDEL, 0x00, &[]));
        no_type.extend(rec(ENDLIB, 0x00, &[]));
        assert!(census_gds(no_type.as_slice()).unwrap_err().contains("no layer or no type"));
    }

    // A pair outside the allowed list is reported; a pair in it is not.
    #[test]
    fn pairs_outside_the_allowed_list_are_reported() {
        let c = census_gds(stream(&[(BOUNDARY, DATATYPE, 15, 0), (BOUNDARY, DATATYPE, 15, 52), (TEXT, TEXTTYPE, 15, 20)], true).as_slice()).unwrap();
        let allowed: BTreeSet<Ld> = [(15, 0), (15, 20)].into();
        assert_eq!(not_allowed(&c, &allowed), [(15, 52)]);
        let all: BTreeSet<Ld> = [(15, 0), (15, 20), (15, 52)].into();
        assert!(not_allowed(&c, &all).is_empty());
    }

    // Rule: on a multi-mask layer an uncoloured shape goes to the map's entries WITHOUT a mask
    // (KLayout's reading, as `gds::LayerMap`); a pair a masked entry also uses is not one of them.
    // Only the multi-mask layers count; text on such a pair is a label, not metal.
    #[test]
    fn uncoloured_targets_are_the_unmasked_entries_of_multi_mask_layers() {
        let map = LayerMap::parse(
            "M1 NET,PIN 10 0\nM1 NET:MASK:1,PIN:MASK:1 10 1\nM1 NET:MASK:2 10 2\nM1 FILL 10 2\n\
             M2 NET 20 0\nM3 NET 30 0\nNAME M1/PIN 10 10\n",
        )
        .unwrap();
        let mm: BTreeSet<String> = ["M1".to_string(), "M2".to_string()].into();
        let t = uncoloured_targets(&map, &mm);
        assert_eq!(t.keys().copied().collect::<Vec<_>>(), [(10, 0), (20, 0)], "10/2 also takes mask 2; M3 is single-mask");
        let c = census_gds(stream(&[(BOUNDARY, DATATYPE, 10, 1), (TEXT, TEXTTYPE, 20, 0), (BOUNDARY, DATATYPE, 30, 0)], true).as_slice()).unwrap();
        assert!(uncoloured_found(&c, &t).is_empty(), "coloured metal, a label, single-mask metal");
        let c = census_gds(stream(&[(BOUNDARY, DATATYPE, 10, 0), (PATH, DATATYPE, 10, 0), (BOX, BOXTYPE, 20, 0)], true).as_slice()).unwrap();
        let found = uncoloured_found(&c, &t);
        assert_eq!(found.iter().map(|u| (u.ld, u.drawn)).collect::<Vec<_>>(), [((10, 0), 2), ((20, 0), 1)]);
        assert_eq!(found[0].layers.iter().collect::<Vec<_>>(), ["M1"]);
    }

    // The map's own targets outside the allowed list, each named by its entry.
    #[test]
    fn map_targets_outside_the_allowed_list_are_named() {
        let map = LayerMap::parse("M1 NET 10 0\nM1 FILL:MASK:1 10 52\nNAME M1/PIN 10 10\n").unwrap();
        let allowed: BTreeSet<Ld> = [(10, 0), (10, 10)].into();
        assert_eq!(map_not_allowed(&map, &allowed), ["M1 FILL:MASK:1 10 52"]);
    }
}
