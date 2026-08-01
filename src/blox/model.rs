// SPDX-License-Identifier: Apache-2.0
//! The typed model of what this reader accepts.
//!
//! **This is a description, not a specification.** 3Dblox is the interchange format the open
//! reference database has adopted; we are a consumer of it. There is no schema document we hold,
//! so the authority is upstream's parser (`src/odb/src/3dblox/`, BSD-3-Clause) at the OpenROAD
//! commit this crate pins. These types record what *we* read from it, and nothing here should be
//! quoted as though it defined the format — it would go stale against upstream in silence.
//!
//! Expressing it as types rather than as ad-hoc field lookups is what turns a malformed file into
//! a located error instead of a default value quietly standing in for a missing one.
use std::collections::BTreeMap;
use std::fmt;

/// Versions this reader has been checked against, per file kind.
///
/// A file declaring anything else is **refused rather than parsed**. The two kinds carry
/// independent version lines (upstream's own examples are `1.0` for `.3dbx` and `2.5` for
/// `.3dbv`), and a format that changed under us would otherwise be read with the old meaning
/// and no complaint — the failure mode this whole module is arranged to avoid.
pub const KNOWN_DBX_VERSIONS: &[&str] = &["1.0"];
pub const KNOWN_DBV_VERSIONS: &[&str] = &["2.5"];

#[derive(Debug)]
pub struct BloxError {
    pub file: String,
    pub path: String,
    pub message: String,
}

impl fmt::Display for BloxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.path.is_empty() {
            write!(f, "{}: {}", self.file, self.message)
        } else {
            write!(f, "{} at {}: {}", self.file, self.path, self.message)
        }
    }
}
impl std::error::Error for BloxError {}

pub(crate) fn err(file: &str, path: &str, message: impl Into<String>) -> BloxError {
    BloxError { file: file.into(), path: path.into(), message: message.into() }
}

/// `Header:` — common to both file kinds.
#[derive(Debug, Default, Clone)]
pub struct Header {
    pub version: String,
    pub unit: String,
    /// dbu per micron the file's coordinates are written at.
    pub precision: i32,
    pub includes: Vec<String>,
}

/// A bonding surface on a chiplet definition.
#[derive(Debug, Clone)]
pub struct Region {
    pub name: String,
    /// `front` | `back` | `internal` | `internal_ext` — **lowercase in the file**, uppercase in
    /// the database API. The vocabularies genuinely differ; the conversion is at the boundary.
    pub side: String,
    /// Polygon outline in microns. The database stores a `Rect`, so a non-rectangular outline
    /// loses shape — see `bounding_box`.
    pub coords: Vec<(f64, f64)>,
}

impl Region {
    /// Axis-aligned bounds of the outline, in microns.
    pub fn bounding_box(&self) -> Option<(f64, f64, f64, f64)> {
        let (mut x1, mut y1) = (f64::MAX, f64::MAX);
        let (mut x2, mut y2) = (f64::MIN, f64::MIN);
        for &(x, y) in &self.coords {
            x1 = x1.min(x);
            y1 = y1.min(y);
            x2 = x2.max(x);
            y2 = y2.max(y);
        }
        (x1 <= x2).then_some((x1, y1, x2, y2))
    }

    /// Whether the outline is already a rectangle — so a caller can tell a faithful conversion
    /// from a lossy one instead of assuming.
    pub fn is_rectangular(&self) -> bool {
        match self.bounding_box() {
            None => false,
            Some((x1, y1, x2, y2)) => {
                self.coords.len() == 4
                    && self.coords.iter().all(|&(x, y)| {
                        (x == x1 || x == x2) && (y == y1 || y == y2)
                    })
            }
        }
    }
}

/// `ChipletDef:` entry — the definition of one die.
#[derive(Debug, Clone)]
pub struct ChipletDef {
    pub name: String,
    /// `die` | `rdl` | `ip` | `substrate` | `hier` (lowercase in the file).
    pub chip_type: String,
    pub design_area: Option<(f64, f64)>,
    pub thickness: Option<f64>,
    pub tsv: bool,
    pub regions: Vec<Region>,
}

/// A parsed `.3dbv` — chiplet definitions.
#[derive(Debug, Default)]
pub struct Dbv {
    pub header: Header,
    pub chiplets: Vec<ChipletDef>,
}

/// `Stack:` placement of one chiplet instance.
#[derive(Debug, Clone, Default)]
pub struct Placement {
    pub loc: (f64, f64),
    pub z: f64,
    pub orient: String,
}

/// `ChipletInst:` + its `Stack:` entry, joined.
#[derive(Debug, Clone)]
pub struct ChipletInst {
    pub name: String,
    pub reference: String,
    pub placement: Placement,
}

/// `Connection:` entry. `bot` is optional — upstream's own example carries `bot: ~`, a
/// deliberately virtual bond with no counterpart.
#[derive(Debug, Clone)]
pub struct Connection {
    pub name: String,
    pub top: RegionRef,
    pub bot: Option<RegionRef>,
    pub thickness: f64,
}

/// `inst.regions.name`, or `a/b.regions.name` for a region inside a nested assembly.
#[derive(Debug, Clone)]
pub struct RegionRef {
    pub inst_path: Vec<String>,
    pub region: String,
}

impl RegionRef {
    pub fn parse(file: &str, path: &str, text: &str) -> Result<RegionRef, BloxError> {
        let Some((lhs, region)) = text.split_once(".regions.") else {
            return Err(err(file, path, format!("expected `<inst>.regions.<region>`, got `{text}`")));
        };
        let inst_path: Vec<String> = lhs.split('/').map(str::to_string).collect();
        if inst_path.iter().any(|p| p.is_empty()) || region.is_empty() {
            return Err(err(file, path, format!("malformed region reference `{text}`")));
        }
        Ok(RegionRef { inst_path, region: region.to_string() })
    }
}

/// A parsed `.3dbx` — the assembly.
#[derive(Debug, Default)]
pub struct Dbx {
    pub header: Header,
    pub design_name: String,
    pub insts: Vec<ChipletInst>,
    pub connections: Vec<Connection>,
    /// Definition files this one pulls in, already resolved against its own directory.
    pub includes: Vec<String>,
}

/// Everything the loader needs: one assembly plus the definitions it references.
#[derive(Debug, Default)]
pub struct Assembly {
    pub dbx: Dbx,
    pub defs: BTreeMap<String, ChipletDef>,
    /// Regions whose outline was not a rectangle and so lost shape on the way into the database.
    pub lossy_regions: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(coords: &[(f64, f64)]) -> Region {
        Region { name: "r".into(), side: "front".into(), coords: coords.to_vec() }
    }

    #[test]
    fn a_rectangle_is_recognised_and_an_l_shape_is_not() {
        // The distinction exists so the loader can report shape loss rather than silently
        // squaring off a polygon.
        assert!(region(&[(0., 0.), (9., 0.), (9., 5.), (0., 5.)]).is_rectangular());
        assert!(!region(&[(0., 0.), (9., 0.), (9., 5.), (4., 5.), (4., 8.), (0., 8.)]).is_rectangular());
    }

    #[test]
    fn the_bounding_box_covers_a_non_rectangular_outline() {
        let r = region(&[(1., 1.), (9., 1.), (9., 5.), (4., 5.), (4., 8.), (1., 8.)]);
        assert_eq!(r.bounding_box(), Some((1., 1., 9., 8.)));
    }

    #[test]
    fn a_region_reference_splits_into_an_instance_path_and_a_region() {
        let r = RegionRef::parse("f", "", "soc_inst.regions.front_reg").unwrap();
        assert_eq!(r.inst_path, vec!["soc_inst"]);
        assert_eq!(r.region, "front_reg");
        let n = RegionRef::parse("f", "", "a/b.regions.to_interposer").unwrap();
        assert_eq!(n.inst_path, vec!["a", "b"], "nested assemblies use / between instances");
    }

    #[test]
    fn a_reference_without_the_regions_marker_is_an_error_not_a_guess() {
        assert!(RegionRef::parse("f", "Connection.c.top", "soc_inst.front_reg").is_err());
    }
}
