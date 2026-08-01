// SPDX-License-Identifier: Apache-2.0
//! **Die-to-die interface checking** — LVS across a bond, from bump maps.
//!
//! # Why this exists
//!
//! A 2.5D/3D assembly lives or dies on whether the bumps on two mating faces actually line up and
//! carry the same signals. Upstream's `check_3dblox` has a `Logical Connectivity` check that looks
//! at exactly this, and its inner loop is:
//!
//! ```text
//! auto it = bot_bumps.find(p);       // std::map<Point, ...>, exact integer DBU equality
//! if (it == bot_bumps.end()) {
//!     continue;                      // no bump at that exact point -> skip, silently
//! }
//! ```
//!
//! So it compares *only* pairs that land on precisely the same point, and anything without an
//! exact counterpart is skipped rather than reported. Its sibling `checkNetConnectivity` is an
//! empty function body. The consequence was measured on assemblies built for the purpose, not
//! inferred — `check_3dblox` returns **zero violations** for every one of these:
//!
//! | assembly | reported |
//! | --- | --- |
//! | a top bump with **no mating bump at all** | 0 |
//! | a mating pair misaligned by **1 DBU** (1 nm) | 0 |
//! | a mating pair misaligned by **5 µm** | 0 |
//!
//! Each is dead silicon or a shorted interface, and each passes clean. That is the gap this
//! closes. It is not a criticism of the checker so much as a statement of what it is scoped to:
//! it validates agreement where bumps already coincide, and nothing validates that they *do*.
//!
//! # What it works on
//!
//! **Bump maps** — the `.bmap` files a 3Dblox `.3dbv` points at, six whitespace-separated columns:
//!
//! ```text
//! # bumpInstName  bumpCellType  x(um)  y(um)  portName  netName
//! bump_tx0        MICROBUMP     10.0   10.0   tx[0]     d2d_tx0
//! ```
//!
//! That is upstream's own format, taken from its writer rather than guessed at. A `-` means
//! absent. Working from bump maps rather than from a loaded database is deliberate: it is what a
//! user *has*. Two dies hardened in separate runs produce two bump maps, and the question "do
//! these two interfaces agree?" can be answered before either die is placed in an assembly.

use std::collections::HashMap;

/// One bump, in its own die's coordinates. Microns, as the file gives them.
#[derive(Debug, Clone, PartialEq)]
pub struct Bump {
    pub inst: String,
    pub cell: String,
    pub x: f64,
    pub y: f64,
    /// Block port this bump lands on. `None` where the file said `-`.
    pub port: Option<String>,
    pub net: Option<String>,
}

/// A parsed bump map, plus whatever could not be parsed.
///
/// A bad line does not abort the file. A bump map is machine-generated but hand-edited often
/// enough, and refusing to check 4,095 good bumps because line 812 has five columns would make
/// the tool useless exactly when it is most needed.
#[derive(Debug, Clone, Default)]
pub struct BumpMap {
    pub bumps: Vec<Bump>,
    /// `(line number, what was wrong)`.
    pub errors: Vec<(usize, String)>,
}

fn field(s: &str) -> Option<String> {
    if s == "-" {
        None
    } else {
        Some(s.to_string())
    }
}

impl BumpMap {
    pub fn parse(text: &str) -> BumpMap {
        let mut m = BumpMap::default();
        for (i, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let t: Vec<&str> = line.split_whitespace().collect();
            if t.len() != 6 {
                m.errors.push((i + 1, format!("expected 6 columns, found {}", t.len())));
                continue;
            }
            let (Ok(x), Ok(y)) = (t[2].parse::<f64>(), t[3].parse::<f64>()) else {
                m.errors
                    .push((i + 1, format!("coordinates are not numbers: {} {}", t[2], t[3])));
                continue;
            };
            if !x.is_finite() || !y.is_finite() {
                m.errors.push((i + 1, "coordinates are not finite".into()));
                continue;
            }
            m.bumps.push(Bump {
                inst: t[0].to_string(),
                cell: t[1].to_string(),
                x,
                y,
                port: field(t[4]),
                net: field(t[5]),
            });
        }
        m
    }

    pub fn load(path: &str) -> std::io::Result<BumpMap> {
        Ok(BumpMap::parse(&std::fs::read_to_string(path)?))
    }

    /// Bounding box of the bumps, or `None` when empty.
    pub fn bbox(&self) -> Option<(f64, f64, f64, f64)> {
        let first = self.bumps.first()?;
        let mut b = (first.x, first.y, first.x, first.y);
        for p in &self.bumps {
            b.0 = b.0.min(p.x);
            b.1 = b.1.min(p.y);
            b.2 = b.2.max(p.x);
            b.3 = b.3.max(p.y);
        }
        Some(b)
    }

    /// Smallest centre-to-centre distance between any two bumps — the pitch.
    ///
    /// Used to derive a default matching tolerance from the design itself rather than from a
    /// constant someone picked. Capped at 4096 bumps of work per side so a large map cannot turn
    /// a check into a hang; beyond that the sample is more than enough to establish a pitch.
    pub fn min_pitch(&self) -> Option<f64> {
        let n = self.bumps.len().min(4096);
        if n < 2 {
            return None;
        }
        let mut best = f64::INFINITY;
        for i in 0..n {
            for j in i + 1..n {
                let (dx, dy) = (self.bumps[i].x - self.bumps[j].x, self.bumps[i].y - self.bumps[j].y);
                best = best.min((dx * dx + dy * dy).sqrt());
            }
        }
        (best.is_finite() && best > 0.0).then_some(best)
    }
}

/// How the bottom map is brought into the top map's frame before comparing.
///
/// Nothing here is inferred. Two bump maps are each in their own die's coordinates, and there is
/// no way to know from the files alone how the dies are placed — so the transform is stated by
/// the caller and echoed in the report. A checker that guessed an alignment and then reported
/// everything as matching would be worse than no checker.
#[derive(Debug, Clone, Default)]
pub struct Transform {
    pub dx: f64,
    pub dy: f64,
    /// Mirror the bottom map in X about its own bounding-box centre — the face-to-face case,
    /// where flipping a die reverses the handedness of its bump field. Getting this wrong makes
    /// every bump miss, which is at least loud; *omitting* it when it was needed is the quiet
    /// failure, so the report always says whether it was applied.
    pub flip_x: bool,
}

impl Transform {
    fn apply(&self, m: &BumpMap) -> Vec<Bump> {
        let mirror = self.flip_x.then(|| m.bbox()).flatten().map(|(x0, _, x1, _)| x0 + x1);
        m.bumps
            .iter()
            .map(|b| Bump {
                x: match mirror {
                    Some(sum) => sum - b.x + self.dx,
                    None => b.x + self.dx,
                },
                y: b.y + self.dy,
                ..b.clone()
            })
            .collect()
    }
}

/// Where a die sits in the assembly, so its bump map can be brought into the global frame.
///
/// The XY mapping per orientation was **measured** against `dbUnfoldedChipBumpInst`'s own global
/// positions rather than derived from the name, because the names do not mean what they look
/// like: `MZ` flips the die's *face* and leaves X and Y alone. The X mirror people expect from
/// "flipped" comes from the `MY` component, so a face-to-face die is usually `MZ_MY`, not `MZ`.
/// Assuming otherwise silently compares two dies in mirrored frames and reports a dead interface
/// as clean.
#[derive(Debug, Clone, PartialEq)]
pub struct Placement {
    pub orient: String,
    /// Instance location in the assembly, microns.
    pub loc_x: f64,
    pub loc_y: f64,
    /// The die's own extent, microns — mirrors are taken about it.
    pub die_w: f64,
    pub die_h: f64,
}

impl Placement {
    /// Map a point in the die's own frame to the assembly frame.
    ///
    /// `None` for an orientation string this has not been verified against. odb silently treats
    /// an unrecognised orientation as `R0`, so guessing here would place a die wrongly and then
    /// report the interface as clean — refusing is the only safe answer.
    pub fn map_point(&self, x: f64, y: f64) -> Option<(f64, f64)> {
        let (w, h) = (self.die_w, self.die_h);
        // `MZ` is the face flip; it does not touch XY, so the XY mapping is that of its base.
        let base = self.orient.strip_prefix("MZ_").unwrap_or(&self.orient);
        let base = if base == "MZ" { "R0" } else { base };
        let (lx, ly) = match base {
            "R0" => (x, y),
            "R90" => (h - y, x),
            "R180" => (w - x, h - y),
            "R270" => (y, w - x),
            "MX" => (x, h - y),
            "MY" => (w - x, y),
            "MXR90" => (y, x),
            "MYR90" => (h - y, w - x),
            _ => return None,
        };
        Some((lx + self.loc_x, ly + self.loc_y))
    }

    /// Whether this orientation is one the mapping has been verified for.
    pub fn is_supported(&self) -> bool {
        self.map_point(0.0, 0.0).is_some()
    }

    /// Bring a bump map into the assembly frame.
    pub fn apply(&self, m: &BumpMap) -> Option<BumpMap> {
        let bumps = m
            .bumps
            .iter()
            .map(|b| {
                self.map_point(b.x, b.y).map(|(x, y)| Bump {
                    x,
                    y,
                    ..b.clone()
                })
            })
            .collect::<Option<Vec<_>>>()?;
        Some(BumpMap {
            bumps,
            errors: m.errors.clone(),
        })
    }

    pub fn describe(&self) -> String {
        format!(
            "{} at ({:.3}, {:.3}) um, die {:.3} x {:.3} um",
            self.orient, self.loc_x, self.loc_y, self.die_w, self.die_h
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Top,
    Bottom,
}

impl Side {
    pub fn as_str(self) -> &'static str {
        match self {
            Side::Top => "top",
            Side::Bottom => "bottom",
        }
    }
}

#[derive(Debug, Clone)]
pub enum Finding {
    /// A bump with no counterpart within tolerance. Dead silicon: a signal that leaves one die
    /// and arrives nowhere.
    Unmated { side: Side, bump: Bump },
    /// A pair close enough to be intended mates, but not coincident. Upstream skips these
    /// entirely, because they are not at the same point.
    Misaligned {
        top: Bump,
        bottom: Bump,
        distance_um: f64,
    },
    /// Mated bumps carrying different net names — the interface is wired to the wrong signal.
    NetMismatch {
        top: Bump,
        bottom: Bump,
        top_net: Option<String>,
        bottom_net: Option<String>,
    },
    /// Mated bumps of different cell types, e.g. a microbump against a C4.
    CellMismatch { top: Bump, bottom: Bump },
}

impl Finding {
    pub fn kind(&self) -> &'static str {
        match self {
            Finding::Unmated { .. } => "unmated",
            Finding::Misaligned { .. } => "misaligned",
            Finding::NetMismatch { .. } => "net-mismatch",
            Finding::CellMismatch { .. } => "cell-mismatch",
        }
    }

    /// One line, naming the bumps involved. A finding a user cannot locate is not actionable.
    pub fn message(&self) -> String {
        match self {
            Finding::Unmated { side, bump } => format!(
                "{} bump {} ({}) at ({:.3}, {:.3}) has no mating bump{}",
                side.as_str(),
                bump.inst,
                bump.net.as_deref().unwrap_or("no net"),
                bump.x,
                bump.y,
                match side {
                    Side::Top => " on the bottom die",
                    Side::Bottom => " on the top die",
                }
            ),
            Finding::Misaligned {
                top,
                bottom,
                distance_um,
            } => format!(
                "{} and {} are intended mates but are {:.4} um apart \
                 (top ({:.3}, {:.3}), bottom ({:.3}, {:.3}))",
                top.inst, bottom.inst, distance_um, top.x, top.y, bottom.x, bottom.y
            ),
            Finding::NetMismatch {
                top,
                bottom,
                top_net,
                bottom_net,
            } => format!(
                "{} carries {} but mates with {} carrying {}",
                top.inst,
                top_net.as_deref().unwrap_or("no net"),
                bottom.inst,
                bottom_net.as_deref().unwrap_or("no net")
            ),
            Finding::CellMismatch { top, bottom } => format!(
                "{} is a {} but mates with {}, a {}",
                top.inst, top.cell, bottom.inst, bottom.cell
            ),
        }
    }
}

#[derive(Debug, Clone)]
pub struct D2dReport {
    pub top_bumps: usize,
    pub bottom_bumps: usize,
    pub matched: usize,
    pub tolerance_um: f64,
    /// Where the tolerance came from: `derived` from the bump pitch, or `specified`.
    pub tolerance_source: &'static str,
    pub transform: Transform,
    /// How the two maps were brought into a common frame, in words. A clean result is
    /// uninterpretable without it, so it is carried in the report rather than left to the caller
    /// to remember what it passed.
    pub frame: String,
    pub findings: Vec<Finding>,
    /// Parse errors from either file, as `(side, line, message)`.
    pub parse_errors: Vec<(Side, usize, String)>,
}

impl D2dReport {
    pub fn violations(&self) -> usize {
        self.findings.len()
    }

    pub fn count(&self, kind: &str) -> usize {
        self.findings.iter().filter(|f| f.kind() == kind).count()
    }

    pub fn to_json(&self) -> serde_json::Value {
        let by_kind: HashMap<&str, usize> = ["unmated", "misaligned", "net-mismatch", "cell-mismatch"]
            .into_iter()
            .map(|k| (k, self.count(k)))
            .filter(|(_, n)| *n > 0)
            .collect();
        serde_json::json!({
            "violations": self.violations(),
            "by_kind": by_kind,
            "top_bumps": self.top_bumps,
            "bottom_bumps": self.bottom_bumps,
            "matched": self.matched,
            "tolerance_um": self.tolerance_um,
            "tolerance_source": self.tolerance_source,
            "frame": self.frame,
            "transform": {
                "dx_um": self.transform.dx,
                "dy_um": self.transform.dy,
                "flip_x": self.transform.flip_x,
            },
            "findings": self.findings.iter().map(|f| serde_json::json!({
                "kind": f.kind(),
                "message": f.message(),
            })).collect::<Vec<_>>(),
            "parse_errors": self.parse_errors.iter().map(|(s, l, m)| serde_json::json!({
                "side": s.as_str(), "line": l, "error": m,
            })).collect::<Vec<_>>(),
        })
    }
}

/// Round to a grid cell for the exact-match pass. 1 nm, well below any bump geometry.
fn key(x: f64, y: f64) -> (i64, i64) {
    ((x * 1000.0).round() as i64, (y * 1000.0).round() as i64)
}

/// Beyond this many unmatched bumps per side, skip the O(n²) nearest-neighbour pass.
///
/// An interface with thousands of bumps that fail to coincide is already catastrophically wrong;
/// spending quadratic time to describe *how* wrong helps nobody, and a checker that hangs on bad
/// input is worse than one that reports plainly.
const NEAREST_PASS_CAP: usize = 4096;

/// Check one die-to-die interface.
pub fn check(top: &BumpMap, bottom: &BumpMap, tf: Transform, tolerance_um: Option<f64>) -> D2dReport {
    let tb: Vec<Bump> = top.bumps.clone();
    let bb: Vec<Bump> = tf.apply(bottom);

    // A tolerance taken from the design beats one taken from a constant. Half the smaller of the
    // two pitches: anything closer than that to a bump is nearer to it than to its neighbour, so
    // a match cannot be ambiguous.
    let (tolerance, source) = match tolerance_um {
        Some(t) => (t.max(0.0), "specified"),
        None => match (top.min_pitch(), bottom.min_pitch()) {
            (Some(a), Some(b)) => (a.min(b) / 2.0, "derived from bump pitch"),
            (Some(a), None) | (None, Some(a)) => (a / 2.0, "derived from bump pitch"),
            // One bump a side, or none: nothing to derive a pitch from, so require coincidence
            // and say that is what happened.
            (None, None) => (0.0, "exact (too few bumps to derive a pitch)"),
        },
    };

    let mut findings = Vec::new();
    let mut bottom_used = vec![false; bb.len()];
    let mut pairs: Vec<(usize, usize, f64)> = Vec::new();

    // Pass 1 — exact coincidence. This is the overwhelmingly common case in a correct design,
    // and it keeps the quadratic pass below down to the genuinely suspect bumps.
    let mut index: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for (j, b) in bb.iter().enumerate() {
        index.entry(key(b.x, b.y)).or_default().push(j);
    }
    let mut top_unmatched = Vec::new();
    for (i, t) in tb.iter().enumerate() {
        match index
            .get(&key(t.x, t.y))
            .and_then(|c| c.iter().copied().find(|j| !bottom_used[*j]))
        {
            Some(j) => {
                bottom_used[j] = true;
                pairs.push((i, j, 0.0));
            }
            None => top_unmatched.push(i),
        }
    }

    // Pass 2 — nearest neighbour among what is left, so a near miss is diagnosed as a misalignment
    // rather than as two unrelated orphans. This is the case upstream skips.
    let leftover_bottom: Vec<usize> = (0..bb.len()).filter(|j| !bottom_used[*j]).collect();
    let capped = top_unmatched.len() > NEAREST_PASS_CAP || leftover_bottom.len() > NEAREST_PASS_CAP;
    if !capped && tolerance > 0.0 {
        for &i in &top_unmatched {
            let t = &tb[i];
            let mut best: Option<(usize, f64)> = None;
            for &j in &leftover_bottom {
                if bottom_used[j] {
                    continue;
                }
                let (dx, dy) = (t.x - bb[j].x, t.y - bb[j].y);
                let d = (dx * dx + dy * dy).sqrt();
                if d <= tolerance && best.map_or(true, |(_, bd)| d < bd) {
                    best = Some((j, d));
                }
            }
            if let Some((j, d)) = best {
                bottom_used[j] = true;
                pairs.push((i, j, d));
            }
        }
    }

    let matched_top: std::collections::HashSet<usize> = pairs.iter().map(|(i, _, _)| *i).collect();

    // Findings on the pairs.
    for &(i, j, d) in &pairs {
        let (t, b) = (&tb[i], &bb[j]);
        if d > 0.0 {
            findings.push(Finding::Misaligned {
                top: t.clone(),
                bottom: b.clone(),
                distance_um: d,
            });
        }
        if t.net != b.net {
            findings.push(Finding::NetMismatch {
                top: t.clone(),
                bottom: b.clone(),
                top_net: t.net.clone(),
                bottom_net: b.net.clone(),
            });
        }
        if t.cell != b.cell {
            findings.push(Finding::CellMismatch {
                top: t.clone(),
                bottom: b.clone(),
            });
        }
    }

    // And the orphans on both sides. Both matter: an unmated *bottom* bump is just as dead as an
    // unmated top one, and a check that only walked the top map would miss half of them.
    for (i, t) in tb.iter().enumerate() {
        if !matched_top.contains(&i) {
            findings.push(Finding::Unmated {
                side: Side::Top,
                bump: t.clone(),
            });
        }
    }
    for (j, b) in bb.iter().enumerate() {
        if !bottom_used[j] {
            findings.push(Finding::Unmated {
                side: Side::Bottom,
                bump: b.clone(),
            });
        }
    }

    let mut parse_errors: Vec<(Side, usize, String)> = top
        .errors
        .iter()
        .map(|(l, m)| (Side::Top, *l, m.clone()))
        .collect();
    parse_errors.extend(bottom.errors.iter().map(|(l, m)| (Side::Bottom, *l, m.clone())));

    D2dReport {
        top_bumps: tb.len(),
        bottom_bumps: bb.len(),
        matched: pairs.len(),
        tolerance_um: tolerance,
        tolerance_source: source,
        frame: format!(
            "bottom map shifted by ({:.3}, {:.3}) um{}",
            tf.dx,
            tf.dy,
            if tf.flip_x { ", mirrored in X" } else { "" }
        ),
        transform: tf,
        findings,
        parse_errors,
    }
}

/// Check an interface whose two sides are **placed in an assembly**.
///
/// This is the form that removes the tool's largest caveat. In the two-file form the caller has to
/// know and pass how the dies sit relative to each other; here the assembly already says, so the
/// frame is derived rather than asserted — and a wrong guess about, say, whether a flipped die
/// mirrors in X stops being possible.
///
/// `Err` names the orientation if either side carries one the mapping has not been verified for.
/// odb treats an unrecognised orientation as `R0`, so proceeding would place a die wrongly and
/// then report the interface clean.
pub fn check_placed(
    top: &BumpMap,
    top_at: &Placement,
    bottom: &BumpMap,
    bottom_at: &Placement,
    tolerance_um: Option<f64>,
) -> std::result::Result<D2dReport, String> {
    let tg = top_at
        .apply(top)
        .ok_or_else(|| format!("unsupported orientation `{}` on the top die", top_at.orient))?;
    let bg = bottom_at.apply(bottom).ok_or_else(|| {
        format!("unsupported orientation `{}` on the bottom die", bottom_at.orient)
    })?;
    // Both sides are already global, so no further transform is applied.
    let mut r = check(&tg, &bg, Transform::default(), tolerance_um);
    r.frame = format!(
        "assembly frame — top {} ; bottom {}",
        top_at.describe(),
        bottom_at.describe()
    );
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOP: &str = "\
# bumpInstName bumpCellType x y portName netName
bt0 MICROBUMP 10.0 10.0 tx[0] d2d_tx0
bt1 MICROBUMP 50.0 10.0 tx[1] d2d_tx1
bt2 MICROBUMP 90.0 10.0 tx[2] d2d_tx2
";
    const BOTTOM: &str = "\
bb0 MICROBUMP 10.0 10.0 rx[0] d2d_tx0
bb1 MICROBUMP 50.0 10.0 rx[1] d2d_tx1
bb2 MICROBUMP 90.0 10.0 rx[2] d2d_tx2
";

    fn run(top: &str, bottom: &str) -> D2dReport {
        check(
            &BumpMap::parse(top),
            &BumpMap::parse(bottom),
            Transform::default(),
            None,
        )
    }

    #[test]
    fn a_matching_interface_is_clean() {
        let r = run(TOP, BOTTOM);
        assert_eq!(r.violations(), 0, "{:?}", r.findings.iter().map(|f| f.message()).collect::<Vec<_>>());
        assert_eq!(r.matched, 3);
        assert_eq!(r.top_bumps, 3);
    }

    #[test]
    fn the_upstream_format_parses_including_comments_and_absent_fields() {
        let m = BumpMap::parse("# a comment\n\nb0 CELL 1.5 -2.5 - -\n");
        assert!(m.errors.is_empty());
        assert_eq!(m.bumps.len(), 1);
        assert_eq!(m.bumps[0].x, 1.5);
        assert_eq!(m.bumps[0].y, -2.5);
        assert_eq!(m.bumps[0].port, None, "'-' means absent, not a port called '-'");
        assert_eq!(m.bumps[0].net, None);
    }

    #[test]
    fn a_bad_line_does_not_lose_the_good_ones() {
        // A bump map is machine-generated but hand-edited often enough. Refusing to check the
        // whole interface because one line is malformed makes the tool useless exactly when it
        // is most needed.
        let m = BumpMap::parse("b0 C 1 1 p n\nbroken line\nb1 C 2 2 p n\nb2 C x y p n\n");
        assert_eq!(m.bumps.len(), 2);
        assert_eq!(m.errors.len(), 2);
        assert_eq!(m.errors[0].0, 2, "the line number must be the file's, not the bump's");
        assert!(m.errors[1].1.contains("not numbers"));
    }

    // ── The three cases upstream reports zero for ───────────────────────────────────────────

    #[test]
    fn a_bump_with_no_mate_is_reported() {
        // Measured: check_3dblox returns 0 violations for this. It is dead silicon.
        let r = run(TOP, "bb0 MICROBUMP 10.0 10.0 rx[0] d2d_tx0\n");
        assert_eq!(r.count("unmated"), 2);
        assert!(r.findings.iter().any(|f| f.message().contains("bt1")));
        assert!(r.findings.iter().any(|f| f.message().contains("bt2")));
    }

    #[test]
    fn a_pair_off_by_a_nanometre_is_reported_as_misaligned_not_skipped() {
        // Upstream matches on exact integer DBU equality and `continue`s on a miss, so a 1 nm
        // offset makes the pair invisible rather than suspect.
        let r = run(TOP, "bb0 MICROBUMP 10.001 10.0 rx[0] d2d_tx0\nbb1 MICROBUMP 50.0 10.0 rx[1] d2d_tx1\nbb2 MICROBUMP 90.0 10.0 rx[2] d2d_tx2\n");
        assert_eq!(r.count("misaligned"), 1);
        assert_eq!(r.count("unmated"), 0, "a near miss is a misalignment, not two orphans");
        let m = r.findings.iter().find(|f| f.kind() == "misaligned").unwrap().message();
        assert!(m.contains("bt0") && m.contains("bb0"), "{m}");
    }

    #[test]
    fn a_pair_off_by_microns_is_still_caught() {
        let r = run(TOP, "bb0 MICROBUMP 15.0 10.0 rx[0] d2d_tx0\nbb1 MICROBUMP 50.0 10.0 rx[1] d2d_tx1\nbb2 MICROBUMP 90.0 10.0 rx[2] d2d_tx2\n");
        // 5 um against a 40 um pitch is inside the derived tolerance, so it is a misalignment.
        assert_eq!(r.count("misaligned"), 1);
    }

    #[test]
    fn a_mate_carrying_the_wrong_net_is_reported() {
        let r = run(TOP, "bb0 MICROBUMP 10.0 10.0 rx[0] d2d_tx1\nbb1 MICROBUMP 50.0 10.0 rx[1] d2d_tx0\nbb2 MICROBUMP 90.0 10.0 rx[2] d2d_tx2\n");
        assert_eq!(r.count("net-mismatch"), 2, "swapped signals are two wrong bumps");
    }

    #[test]
    fn a_mismatched_bump_cell_is_reported() {
        let r = run(TOP, "bb0 C4 10.0 10.0 rx[0] d2d_tx0\nbb1 MICROBUMP 50.0 10.0 rx[1] d2d_tx1\nbb2 MICROBUMP 90.0 10.0 rx[2] d2d_tx2\n");
        assert_eq!(r.count("cell-mismatch"), 1);
        assert!(r.findings[0].message().contains("C4"));
    }

    // ── Transform ──────────────────────────────────────────────────────────────────────────

    #[test]
    fn an_offset_brings_a_shifted_map_into_frame() {
        let shifted = "bb0 MICROBUMP 110.0 10.0 rx[0] d2d_tx0\nbb1 MICROBUMP 150.0 10.0 rx[1] d2d_tx1\nbb2 MICROBUMP 190.0 10.0 rx[2] d2d_tx2\n";
        assert!(run(TOP, shifted).violations() > 0, "unshifted, nothing should mate");

        let r = check(
            &BumpMap::parse(TOP),
            &BumpMap::parse(shifted),
            Transform { dx: -100.0, dy: 0.0, flip_x: false },
            None,
        );
        assert_eq!(r.violations(), 0);
        assert_eq!(r.matched, 3);
    }

    #[test]
    fn flip_x_mirrors_a_face_to_face_die() {
        // Flipping a die reverses the handedness of its bump field. Omitting the flip when it was
        // needed is the quiet failure — every bump misses — so this has to work.
        let mirrored = "bb0 MICROBUMP 90.0 10.0 rx[0] d2d_tx0\nbb1 MICROBUMP 50.0 10.0 rx[1] d2d_tx1\nbb2 MICROBUMP 10.0 10.0 rx[2] d2d_tx2\n";
        let plain = run(TOP, mirrored);
        assert!(plain.count("net-mismatch") > 0, "unmirrored, the nets should cross");

        let r = check(
            &BumpMap::parse(TOP),
            &BumpMap::parse(mirrored),
            Transform { dx: 0.0, dy: 0.0, flip_x: true },
            None,
        );
        assert_eq!(r.violations(), 0, "{:?}", r.findings.iter().map(|f| f.message()).collect::<Vec<_>>());
    }

    #[test]
    fn the_transform_is_reported_rather_than_left_implicit() {
        // A reader has to be able to tell what frame the result was computed in; "clean" means
        // nothing without it.
        let r = check(
            &BumpMap::parse(TOP),
            &BumpMap::parse(BOTTOM),
            Transform { dx: 1.0, dy: 2.0, flip_x: true },
            None,
        );
        let j = r.to_json();
        assert_eq!(j["transform"]["dx_um"], 1.0);
        assert_eq!(j["transform"]["flip_x"], true);
    }

    // ── Tolerance ──────────────────────────────────────────────────────────────────────────

    #[test]
    fn the_default_tolerance_comes_from_the_bump_pitch() {
        // Half the pitch: anything nearer to a bump than that is nearer to it than to its
        // neighbour, so a match cannot be ambiguous. Better than a constant someone picked.
        let r = run(TOP, BOTTOM);
        assert_eq!(r.tolerance_um, 20.0, "40 um pitch -> 20 um");
        assert_eq!(r.tolerance_source, "derived from bump pitch");
    }

    #[test]
    fn too_few_bumps_to_derive_a_pitch_says_so_rather_than_inventing_one() {
        let r = run("b0 C 1 1 p n\n", "b1 C 1 1 p n\n");
        assert_eq!(r.tolerance_um, 0.0);
        assert!(r.tolerance_source.contains("too few"));
        assert_eq!(r.violations(), 0, "coincident bumps still match exactly");
    }

    #[test]
    fn an_explicit_tolerance_overrides_the_derived_one() {
        let near = "bb0 MICROBUMP 15.0 10.0 rx[0] d2d_tx0\nbb1 MICROBUMP 50.0 10.0 rx[1] d2d_tx1\nbb2 MICROBUMP 90.0 10.0 rx[2] d2d_tx2\n";
        let tight = check(&BumpMap::parse(TOP), &BumpMap::parse(near), Transform::default(), Some(1.0));
        // Outside a 1 um tolerance the two bumps are not mates at all, so they are orphans.
        assert_eq!(tight.count("misaligned"), 0);
        assert_eq!(tight.count("unmated"), 2);
        assert_eq!(tight.tolerance_source, "specified");
    }

    // ── Placement ──────────────────────────────────────────────────────────────────────────

    fn die(orient: &str) -> Placement {
        Placement {
            orient: orient.into(),
            loc_x: 0.0,
            loc_y: 0.0,
            die_w: 50.0,
            die_h: 40.0,
        }
    }

    #[test]
    fn the_orientation_mapping_matches_what_odb_actually_does() {
        // These expectations are not derived from the names — they were read out of
        // `dbUnfoldedChipBumpInst::getGlobalPosition` for a die of 50 x 40 with a bump at
        // (2.84, 3.36). See examples/probe_orient.rs. Deriving them from the names is exactly
        // how you conclude that MZ mirrors X, which it does not.
        let (x, y) = (2.84, 3.36);
        for (orient, want) in [
            ("R0", (2.84, 3.36)),
            ("R90", (36.64, 2.84)),
            ("R180", (47.16, 36.64)),
            ("R270", (3.36, 47.16)),
            ("MX", (2.84, 36.64)),
            ("MY", (47.16, 3.36)),
            ("MXR90", (3.36, 2.84)),
            ("MYR90", (36.64, 47.16)),
        ] {
            let got = die(orient).map_point(x, y).expect(orient);
            assert!(
                (got.0 - want.0).abs() < 1e-9 && (got.1 - want.1).abs() < 1e-9,
                "{orient}: got {got:?}, odb gives {want:?}"
            );
            // MZ is the face flip and must not change XY at all.
            let mz = die(&format!("MZ_{orient}")).map_point(x, y).expect(orient);
            assert_eq!(mz, got, "MZ_{orient} must have the same XY as {orient}");
        }
        assert_eq!(die("MZ").map_point(x, y), Some((x, y)), "MZ alone leaves XY alone");
    }

    #[test]
    fn an_unverified_orientation_is_refused_rather_than_treated_as_r0() {
        // odb silently falls back to R0 for an unrecognised orientation — measured. Inheriting
        // that would place a die wrongly and then call the interface clean.
        assert!(die("SIDEWAYS").map_point(1.0, 1.0).is_none());
        assert!(!die("SIDEWAYS").is_supported());
        assert!(die("MZ_MY").is_supported());
    }

    #[test]
    fn a_placement_offset_lands_the_die_where_the_assembly_puts_it() {
        let p = Placement { orient: "R0".into(), loc_x: 100.0, loc_y: 5.0, die_w: 50.0, die_h: 40.0 };
        assert_eq!(p.map_point(1.0, 2.0), Some((101.0, 7.0)));
    }

    #[test]
    fn a_face_to_face_pair_checks_clean_in_the_assembly_frame() {
        // The logic die R0, the memory die MZ_MY above it — the real face-to-face case. The
        // memory map's X values mirror about its own die, and the check has to undo that.
        let logic = BumpMap::parse("l0 MB 10.0 10.0 t0 n0
l1 MB 40.0 10.0 t1 n1
");
        let mem = BumpMap::parse("m0 MB 40.0 10.0 r0 n0
m1 MB 10.0 10.0 r1 n1
");
        let r = check_placed(&mem, &die("MZ_MY"), &logic, &die("R0"), None).unwrap();
        assert_eq!(r.violations(), 0, "{:?}", r.findings.iter().map(|f| f.message()).collect::<Vec<_>>());
        assert!(r.frame.contains("MZ_MY"), "the frame must name the placements it used");
    }

    #[test]
    fn using_mz_where_mz_my_was_meant_is_loud() {
        // The single most likely modelling mistake, and it must not look clean.
        let logic = BumpMap::parse("l0 MB 10.0 10.0 t0 n0
l1 MB 40.0 10.0 t1 n1
");
        let mem = BumpMap::parse("m0 MB 40.0 10.0 r0 n0
m1 MB 10.0 10.0 r1 n1
");
        let r = check_placed(&mem, &die("MZ"), &logic, &die("R0"), None).unwrap();
        assert_eq!(r.count("net-mismatch"), 2, "the interface reads as reversed");
    }

    #[test]
    fn an_unsupported_orientation_names_itself_in_the_error() {
        let m = BumpMap::parse("b0 MB 1.0 1.0 p n
");
        let e = check_placed(&m, &die("SIDEWAYS"), &m, &die("R0"), None).unwrap_err();
        assert!(e.contains("SIDEWAYS") && e.contains("top"), "{e}");
        let e = check_placed(&m, &die("R0"), &m, &die("SIDEWAYS"), None).unwrap_err();
        assert!(e.contains("bottom"), "{e}");
    }

    // ── Degenerate input ───────────────────────────────────────────────────────────────────

    #[test]
    fn empty_maps_are_a_clean_report_not_a_panic() {
        let r = run("", "");
        assert_eq!(r.violations(), 0);
        assert_eq!(r.matched, 0);
        assert!(r.to_json()["violations"] == 0);
    }

    #[test]
    fn one_empty_side_reports_every_bump_on_the_other_as_unmated() {
        let r = run(TOP, "");
        assert_eq!(r.count("unmated"), 3);
        let r = run("", BOTTOM);
        assert_eq!(r.count("unmated"), 3);
        assert!(r.findings[0].message().starts_with("bottom bump"));
    }

    #[test]
    fn duplicate_bumps_at_one_point_do_not_both_claim_the_same_mate() {
        // Two bumps stacked on one coordinate is itself a defect; the check must not paper over
        // it by matching both against a single counterpart.
        let r = run(
            "bt0 C 10.0 10.0 p n\nbt1 C 10.0 10.0 p n\n",
            "bb0 C 10.0 10.0 p n\n",
        );
        assert_eq!(r.matched, 1);
        assert_eq!(r.count("unmated"), 1);
    }

    #[test]
    fn parse_errors_reach_the_report_rather_than_being_swallowed() {
        let r = run("garbage\nbt0 C 10 10 p n\n", "bb0 C 10 10 p n\n");
        assert_eq!(r.parse_errors.len(), 1);
        assert_eq!(r.parse_errors[0].0, Side::Top);
        assert_eq!(r.to_json()["parse_errors"].as_array().unwrap().len(), 1);
    }
}
