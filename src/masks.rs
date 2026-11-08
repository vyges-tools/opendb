// SPDX-License-Identifier: Apache-2.0
//! Mask colouring of routed wires on multi-patterned (multi-mask) routing layers.
//!
//! A multi-patterned layer is routed one way and on its tracks; its masks then alternate track by
//! track, so the mask of a wire is the mask of the track it sits on. This pass assigns exactly
//! that and **reports, never forces**, everything that does not fit:
//!
//! * a shape off every track, or running the wrong way, is left as it is (coloured if it already
//!   was, uncoloured otherwise) and reported;
//! * a shape that already carries a mask keeps it; a mask that disagrees with its track's is
//!   reported;
//! * after colouring, every pair of shapes on one layer closer than the same-mask spacing is a
//!   conflict edge: a conflict between two shapes of the SAME mask is reported (on one track, that
//!   is a line-end pair), and on a two-mask layer an odd cycle of conflict edges — which no
//!   assignment of two masks can satisfy — is reported with its shapes.
//!
//! The colours are written back through odb's own wire encoder (`dbWireEncoder::setColor` /
//! `setViaColor`), so odb's DEF writer emits `MASK` and the GDS stream-out maps them to datatypes.
//!
//! The track-parity rule and the policy of reporting odd cycles rather than colouring them follow
//! the approach of OpenROAD PR #10857 (unmerged) and the upstream guidance to route multi-patterned
//! layers one way on-grid and colour by alternating tracks; the code here is our own. Unlike that
//! PR, the track is taken from the block's track grid (with its DEF `MASK`/`SAMEMASK`), not from
//! the layer pitch, and an off-track shape is reported instead of snapped to the nearest track.

use std::collections::{BTreeMap, HashMap, VecDeque};

use crate::{Db, Result, ViaMask, WireMaskState};
use vyges_loom::lef58_samemask::SameMaskRules;

// ---- the colour grid ----------------------------------------------------------------------------

/// One track pattern across a layer's preferred direction, as the track grid stores it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrackPattern {
    pub origin: i32,
    pub count: i32,
    pub step: i32,
    /// The DEF `TRACKS ... MASK` of the first track; 0 when the tracks carry none.
    pub first_mask: u8,
    /// DEF `SAMEMASK`: every track of the pattern has the first track's mask.
    pub same_mask: bool,
}

/// A multi-mask routing layer as the colouring sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaskLayer {
    pub name: String,
    pub number: i64,
    pub num_masks: u8,
    /// Preferred direction horizontal: its tracks are Y coordinates and its wires run in X.
    pub horizontal: bool,
    /// The patterns ACROSS the preferred direction (Y patterns for a horizontal layer).
    pub tracks: Vec<TrackPattern>,
    /// Required spacing between two shapes of the same mask.
    pub same_mask_spacing: i32,
    /// Where [`MaskLayer::same_mask_spacing`] came from.
    pub spacing_source: SpacingSource,
}

/// The origin of a layer's same-mask spacing, most specific first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpacingSource {
    /// `--same-mask-spacing` / [`ColourOptions::same_mask_spacing`].
    Option,
    /// The layer's LEF58 SAMEMASK rules ([`SameMaskRules::min_spacing`]).
    Lef58SameMask,
    /// No same-mask rule known: the layer's base SPACING (else its table at minimum width).
    BaseSpacing,
}

impl SpacingSource {
    /// The word written in the report.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            SpacingSource::Option => "option",
            SpacingSource::Lef58SameMask => "lef58_samemask",
            SpacingSource::BaseSpacing => "base_spacing",
        }
    }
}

/// Where a coordinate falls on a layer's track grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackHit {
    /// On a track: its pattern, its index in the pattern and its mask.
    On { pattern: usize, index: i32, mask: u8 },
    Off,
}

impl MaskLayer {
    /// The track at `coord` (a Y for a horizontal layer) and its mask. Tracks without a DEF mask
    /// start at mask 1. The first pattern holding the coordinate decides.
    ///
    /// DEF rule: `MASK n` is the mask of a pattern's first track and the masks then cycle
    /// (1, 2, 1, 2 … for two masks); with `SAMEMASK` every track has mask `n`.
    pub fn track_at(&self, coord: i32) -> TrackHit {
        for (k, p) in self.tracks.iter().enumerate() {
            if p.step <= 0 || p.count <= 0 {
                continue;
            }
            let d = coord as i64 - p.origin as i64;
            if d < 0 || d % p.step as i64 != 0 || d / p.step as i64 >= p.count as i64 {
                continue;
            }
            let index = (d / p.step as i64) as i32;
            let first = if p.first_mask == 0 { 1 } else { p.first_mask };
            let mask = if p.same_mask {
                first
            } else {
                ((first as i64 - 1 + index as i64) % self.num_masks as i64) as u8 + 1
            };
            return TrackHit::On { pattern: k, index, mask };
        }
        TrackHit::Off
    }

    /// The coordinate that names a shape's track: Y for a horizontal layer, X for a vertical one.
    fn across(&self, p: (i32, i32)) -> i32 {
        if self.horizontal {
            p.1
        } else {
            p.0
        }
    }

    /// The colour track parity gives a point, `None` off the grid.
    fn mask_at(&self, p: (i32, i32)) -> Option<u8> {
        match self.track_at(self.across(p)) {
            TrackHit::On { mask, .. } => Some(mask),
            TrackHit::Off => None,
        }
    }
}

// ---- a wire, as the decoder walks it ------------------------------------------------------------

/// One record of [`Db::net_wire_decode`], parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireRec {
    /// A new path (decoder opcode 0) on a layer; opcodes 1–3 (junction, short, virtual wire) are
    /// `Other`: a wire holding them is not rewritten.
    Path(String),
    Point(i32, i32),
    PointExt(i32, i32, i32),
    /// A block via (`block` true) or a tech via, its name and bottom and top layers.
    Via { block: bool, name: String, bottom: String, top: String },
    Rect(i32, i32, i32, i32),
    Rule,
    End,
    Other(i32),
}

/// Parse the decoder records (the `T|` header excluded).
pub fn parse_records(recs: &[String]) -> std::result::Result<Vec<WireRec>, String> {
    let mut out = Vec::with_capacity(recs.len());
    for r in recs {
        let f: Vec<&str> = r.split('|').collect();
        let n = |i: usize| -> std::result::Result<i32, String> {
            f.get(i).and_then(|v| v.parse().ok()).ok_or_else(|| format!("wire record {r}"))
        };
        let st = |i: usize| -> std::result::Result<String, String> {
            f.get(i).map(|v| v.to_string()).ok_or_else(|| format!("wire record {r}"))
        };
        let code: i32 = f[0].parse().map_err(|_| format!("wire record {r}"))?;
        out.push(match code {
            0 => WireRec::Path(st(1)?),
            4 => WireRec::Point(n(1)?, n(2)?),
            5 => WireRec::PointExt(n(1)?, n(2)?, n(3)?),
            6 | 7 => WireRec::Via { block: code == 6, name: st(1)?, bottom: st(2)?, top: st(3)? },
            8 => WireRec::Rect(n(1)?, n(2)?, n(3)?, n(4)?),
            11 => WireRec::Rule,
            12 => WireRec::End,
            c => WireRec::Other(c),
        });
    }
    Ok(out)
}

// ---- findings -----------------------------------------------------------------------------------

/// What the pass reports. Nothing here is ever "fixed" by forcing a colour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finding {
    /// A shape on a multi-mask layer off every track (left as it was).
    OffTrack { net: String, layer: String, at: (i32, i32) },
    /// A wire segment running across the layer's preferred direction (left as it was).
    WrongWay { net: String, layer: String, from: (i32, i32), to: (i32, i32) },
    /// A pre-coloured shape whose mask differs from its track's (kept).
    PrecolouredMismatch { net: String, layer: String, at: (i32, i32), has: u8, track: u8 },
    /// A wire this pass cannot re-encode exactly (left untouched).
    NotRewritten { net: String, why: String },
    /// A shape on a multi-mask layer with no mask after the pass.
    Uncoloured { net: String, layer: String, rect: [i32; 4] },
    /// Two shapes of the same mask closer than the same-mask spacing. `same_track`: they share a
    /// track (a line-end pair).
    SameMaskSpacing { layer: String, mask: u8, a: (String, [i32; 4]), b: (String, [i32; 4]), gap: i64, same_track: bool },
    /// An odd cycle of conflict edges on a two-mask layer: no two-colouring exists.
    OddCycle { layer: String, shapes: Vec<(String, [i32; 4])> },
}

impl Finding {
    pub fn kind(&self) -> &'static str {
        match self {
            Finding::OffTrack { .. } => "off_track",
            Finding::WrongWay { .. } => "wrong_way",
            Finding::PrecolouredMismatch { .. } => "precoloured_mismatch",
            Finding::NotRewritten { .. } => "not_rewritten",
            Finding::Uncoloured { .. } => "uncoloured",
            Finding::SameMaskSpacing { .. } => "same_mask_spacing",
            Finding::OddCycle { .. } => "odd_cycle",
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        use serde_json::json;
        let mut v = match self {
            Finding::OffTrack { net, layer, at } => json!({"net": net, "layer": layer, "at": [at.0, at.1]}),
            Finding::WrongWay { net, layer, from, to } => {
                json!({"net": net, "layer": layer, "from": [from.0, from.1], "to": [to.0, to.1]})
            }
            Finding::PrecolouredMismatch { net, layer, at, has, track } => {
                json!({"net": net, "layer": layer, "at": [at.0, at.1], "mask": has, "track_mask": track})
            }
            Finding::NotRewritten { net, why } => json!({"net": net, "why": why}),
            Finding::Uncoloured { net, layer, rect } => json!({"net": net, "layer": layer, "rect": rect}),
            Finding::SameMaskSpacing { layer, mask, a, b, gap, same_track } => json!({
                "layer": layer, "mask": mask, "a": {"net": a.0, "rect": a.1}, "b": {"net": b.0, "rect": b.1},
                "gap": gap, "same_track": same_track,
            }),
            Finding::OddCycle { layer, shapes } => json!({
                "layer": layer,
                "shapes": shapes.iter().map(|(n, r)| json!({"net": n, "rect": r})).collect::<Vec<_>>(),
            }),
        };
        v["kind"] = json!(self.kind());
        v
    }
}

// ---- stage 1: plan one net's colours ------------------------------------------------------------

/// The colours one net's wire should carry: per record, the metal colour (segment-ending points
/// and patches) and the via colour (vias); `None` where the record keeps what it has.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NetPlan {
    pub color: Vec<Option<u8>>,
    pub via: Vec<Option<ViaMask>>,
    /// Some record's colour differs from what the wire holds.
    pub changed: bool,
}

/// Plan the colours of one net from its records and the colour state at each (`state` is
/// [`Db::net_wire_decode_masks`] without the header). A record's colour becomes its track's,
/// unless it is already coloured (kept), off-track or wrong-way (left, reported).
pub fn plan_net(
    net: &str,
    layers: &BTreeMap<String, MaskLayer>,
    recs: &[WireRec],
    state: &[WireMaskState],
    findings: &mut Vec<Finding>,
) -> NetPlan {
    let mut plan = NetPlan {
        color: state.iter().map(|s| s.color).collect(),
        via: state.iter().map(|s| s.via).collect(),
        changed: false,
    };
    let mut layer: Option<&MaskLayer> = None;
    let mut layer_name = String::new();
    let mut prev: Option<(i32, i32)> = None;
    for (i, r) in recs.iter().enumerate() {
        let have = state.get(i).copied().unwrap_or_default();
        match r {
            WireRec::Path(l) => {
                layer_name = l.clone();
                layer = layers.get(l);
                prev = None;
            }
            WireRec::Point(x, y) | WireRec::PointExt(x, y, _) => {
                let p = (*x, *y);
                if let (Some(q), Some(ml)) = (prev, layer) {
                    let want = segment_mask(net, ml, q, p, have.color, findings);
                    if want != have.color {
                        plan.color[i] = want;
                        plan.changed = true;
                    }
                }
                prev = Some(p);
            }
            WireRec::Via { bottom, top, .. } => {
                let Some(at) = prev else { continue };
                let digit = |name: &str, has: u8, findings: &mut Vec<Finding>| -> u8 {
                    let Some(ml) = layers.get(name) else { return has };
                    match (has, ml.mask_at(at)) {
                        (0, Some(m)) => m,
                        (0, None) => {
                            findings.push(Finding::OffTrack { net: net.into(), layer: name.into(), at });
                            0
                        }
                        (h, Some(m)) if h != m => {
                            findings.push(Finding::PrecolouredMismatch {
                                net: net.into(), layer: name.into(), at, has: h, track: m,
                            });
                            h
                        }
                        (h, _) => h,
                    }
                };
                let old = have.via.unwrap_or_default();
                let new = ViaMask {
                    bottom: digit(bottom, old.bottom, findings),
                    cut: old.cut,
                    top: digit(top, old.top, findings),
                };
                let want = (new != ViaMask::default()).then_some(new);
                if want != have.via {
                    plan.via[i] = want;
                    plan.changed = true;
                }
                // The via moves the path to its other layer; the next point continues from here.
                let other = if *bottom == layer_name { top } else { bottom };
                layer_name = other.clone();
                layer = layers.get(other);
            }
            WireRec::Rect(..) => {
                let (Some(at), Some(ml)) = (prev, layer) else { continue };
                let want = match (have.color, ml.mask_at(at)) {
                    (Some(c), _) => Some(c),
                    (None, Some(m)) => Some(m),
                    (None, None) => {
                        findings.push(Finding::OffTrack { net: net.into(), layer: layer_name.clone(), at });
                        None
                    }
                };
                if want != have.color {
                    plan.color[i] = want;
                    plan.changed = true;
                }
            }
            WireRec::Rule | WireRec::End | WireRec::Other(_) => {}
        }
    }
    plan
}

/// The mask a segment from `q` to `p` should carry on `ml`, given the mask it has.
fn segment_mask(
    net: &str,
    ml: &MaskLayer,
    q: (i32, i32),
    p: (i32, i32),
    has: Option<u8>,
    findings: &mut Vec<Finding>,
) -> Option<u8> {
    let along_ok = if ml.horizontal { q.1 == p.1 } else { q.0 == p.0 };
    if !along_ok {
        findings.push(Finding::WrongWay { net: net.into(), layer: ml.name.clone(), from: q, to: p });
        return has;
    }
    match (has, ml.mask_at(p)) {
        (Some(h), Some(m)) if h != m => {
            findings.push(Finding::PrecolouredMismatch { net: net.into(), layer: ml.name.clone(), at: p, has: h, track: m });
            Some(h)
        }
        (Some(h), _) => Some(h),
        (None, Some(m)) => Some(m),
        (None, None) => {
            findings.push(Finding::OffTrack { net: net.into(), layer: ml.name.clone(), at: p });
            None
        }
    }
}

// ---- stage 2: re-encode the wire with its planned colours ---------------------------------------

/// The op stream [`Db::net_write_wire`] takes for these records with these colours, or why the
/// wire cannot be re-encoded exactly (a junction, short or virtual wire, a terminal op, a rule op
/// that does not follow a path).
///
/// odb's encoder rules the stream must respect: a colour is state until the next colour op, and
/// adding a via clears the METAL colour (not the via colour) — so after each via the metal state
/// is "none" again.
///
/// ⛔ **odb's DEF writer writes a wire `MASK` only before the 2nd, 4th, 6th … point of a path**
/// (`defout_impl.cpp`, `point_cnt % 2 == 0`; a via does not restart the count), and the DEF
/// reader's colour is sticky, so a colour on an odd-numbered point is not just lost in the DEF but
/// read back as the previous segment's. A coloured segment that would end on an odd point is
/// therefore started as a new path at its first point: the same rectangles (each segment keeps
/// its end extensions), one segment per path, which is also how a router writes its paths.
pub fn encode_ops(
    recs: &[WireRec],
    plan: &NetPlan,
) -> std::result::Result<(Vec<i32>, Vec<String>), String> {
    fn name_ix(n: &str, names: &mut Vec<String>) -> i32 {
        match names.iter().position(|m| m == n) {
            Some(k) => k as i32,
            None => {
                names.push(n.to_string());
                names.len() as i32 - 1
            }
        }
    }
    let mut ops = Vec::new();
    let mut names: Vec<String> = Vec::new();
    let (mut cur_color, mut cur_via): (Option<u8>, Option<ViaMask>) = (None, None);
    // The running path: its layer, its rule flag, how many points it has, the last point (as
    // the op that wrote it).
    let (mut layer, mut ndr, mut points) = (String::new(), 0, 0usize);
    let mut last: Vec<i32> = Vec::new();
    for (i, r) in recs.iter().enumerate() {
        match r {
            WireRec::Path(l) => {
                ndr = matches!(recs.get(i + 1), Some(WireRec::Rule)) as i32;
                layer = l.clone();
                ops.extend([0, name_ix(l, &mut names), ndr]);
                points = 0;
            }
            WireRec::Rule => {
                if !matches!(i.checked_sub(1).and_then(|k| recs.get(k)), Some(WireRec::Path(_))) {
                    return Err("a rule op not right after a path".into());
                }
            }
            WireRec::Point(..) | WireRec::PointExt(..) => {
                let op: Vec<i32> = match r {
                    WireRec::Point(x, y) => vec![1, *x, *y],
                    WireRec::PointExt(x, y, e) => vec![2, *x, *y, *e],
                    _ => unreachable!("matched as a point above"),
                };
                if points > 0 {
                    let want = plan.color[i];
                    if (points + 1) % 2 == 1 && (want.is_some() || cur_color.is_some()) {
                        ops.extend([0, name_ix(&layer, &mut names), ndr]);
                        ops.extend(&last);
                        points = 1;
                    }
                    if want != cur_color {
                        match want {
                            Some(c) => ops.extend([6, c as i32]),
                            None => ops.push(7),
                        }
                        cur_color = want;
                    }
                }
                ops.extend(&op);
                last = op;
                points += 1;
            }
            WireRec::Via { block, name, bottom, top } => {
                let want = plan.via[i];
                if want != cur_via {
                    match want {
                        Some(v) => ops.extend([8, v.bottom as i32, v.cut as i32, v.top as i32]),
                        None => ops.push(9),
                    }
                    cur_via = want;
                }
                ops.extend([if *block { 4 } else { 3 }, name_ix(name, &mut names)]);
                cur_color = None; // dbWireEncoder::addVia / addTechVia end with clearColor()
                layer = if *bottom == layer { top.clone() } else { bottom.clone() };
            }
            WireRec::Rect(a, b, c, d) => {
                let want = plan.color[i];
                if want != cur_color {
                    match want {
                        Some(c) => ops.extend([6, c as i32]),
                        None => ops.push(7),
                    }
                    cur_color = want;
                }
                ops.extend([5, *a, *b, *c, *d]);
            }
            WireRec::End => break,
            WireRec::Other(c) => return Err(format!("decoder op {c} is not re-encoded")),
        }
    }
    Ok((ops, names))
}

// ---- stage 3: conflicts after colouring ---------------------------------------------------------

/// A shape on a multi-mask layer after colouring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaskShape {
    pub net: String,
    pub rect: [i32; 4],
    /// 0: none.
    pub mask: u8,
}

/// Squared edge-to-edge gap of two rectangles; `None` when they touch or overlap.
fn gap2(a: &[i32; 4], b: &[i32; 4]) -> Option<i64> {
    let dx = (a[0] as i64 - b[2] as i64).max(b[0] as i64 - a[2] as i64).max(0);
    let dy = (a[1] as i64 - b[3] as i64).max(b[1] as i64 - a[3] as i64).max(0);
    (dx > 0 || dy > 0).then_some(dx * dx + dy * dy)
}

/// Conflict edges on one layer: pairs `(i, j)`, `i < j`, closer than `spacing` (Euclidean, edge
/// to edge). Touching or overlapping shapes are connected metal, not a conflict, when they belong
/// to one net; of two nets they are a short, which is not this pass's to report.
pub fn conflict_edges(shapes: &[MaskShape], spacing: i32) -> Vec<(usize, usize, i64)> {
    let s = spacing.max(1) as i64;
    // Bucket by a grid of `spacing`-sized cells over each shape bloated by the spacing.
    let cell = s.max(1);
    let mut grid: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for (k, sh) in shapes.iter().enumerate() {
        let r = sh.rect;
        for cx in (r[0] as i64).div_euclid(cell)..=(r[2] as i64).div_euclid(cell) {
            for cy in (r[1] as i64).div_euclid(cell)..=(r[3] as i64).div_euclid(cell) {
                grid.entry((cx, cy)).or_default().push(k);
            }
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    for (k, sh) in shapes.iter().enumerate() {
        let r = sh.rect;
        let lo = ((r[0] as i64 - s).div_euclid(cell), (r[1] as i64 - s).div_euclid(cell));
        let hi = ((r[2] as i64 + s).div_euclid(cell), (r[3] as i64 + s).div_euclid(cell));
        for cx in lo.0..=hi.0 {
            for cy in lo.1..=hi.1 {
                let Some(v) = grid.get(&(cx, cy)) else { continue };
                for &j in v {
                    if j <= k {
                        continue;
                    }
                    if let Some(g2) = gap2(&r, &shapes[j].rect) {
                        if g2 < s * s {
                            seen.insert((k, j, g2));
                        }
                    }
                }
            }
        }
    }
    let mut out: Vec<(usize, usize, i64)> = Vec::new();
    for e in seen {
        if out.last().is_some_and(|l| l.0 == e.0 && l.1 == e.1) {
            continue;
        }
        out.push(e);
    }
    out
}

/// Odd cycles of a conflict graph (no two-colouring exists): one cycle per offending component,
/// as node indices in cycle order. BFS from the lowest node of each component; an edge between
/// two nodes at the same depth closes an odd cycle through their lowest common ancestor.
pub fn odd_cycles(n: usize, edges: &[(usize, usize, i64)]) -> Vec<Vec<usize>> {
    let mut adj = vec![Vec::new(); n];
    for &(a, b, _) in edges {
        adj[a].push(b);
        adj[b].push(a);
    }
    let (mut depth, mut parent) = (vec![usize::MAX; n], vec![usize::MAX; n]);
    let mut out = Vec::new();
    for root in 0..n {
        if depth[root] != usize::MAX || adj[root].is_empty() {
            continue;
        }
        depth[root] = 0;
        let mut q = VecDeque::from([root]);
        let mut found: Option<(usize, usize)> = None;
        while let Some(u) = q.pop_front() {
            for &v in &adj[u] {
                if depth[v] == usize::MAX {
                    depth[v] = depth[u] + 1;
                    parent[v] = u;
                    q.push_back(v);
                } else if depth[v] == depth[u] && found.is_none() {
                    found = Some((u, v));
                }
            }
        }
        if let Some((u, v)) = found {
            let (mut a, mut b) = (u, v);
            let (mut left, mut right) = (vec![a], vec![b]);
            while a != b {
                a = parent[a];
                b = parent[b];
                left.push(a);
                if a != b {
                    right.push(b);
                }
            }
            right.reverse();
            left.extend(right);
            out.push(left);
        }
    }
    out
}

/// Check one layer's coloured shapes: uncoloured shapes, same-mask pairs under the spacing, and
/// (two masks) odd cycles.
pub fn check_layer(layer: &MaskLayer, shapes: &[MaskShape], findings: &mut Vec<Finding>) {
    for s in shapes.iter().filter(|s| s.mask == 0) {
        findings.push(Finding::Uncoloured { net: s.net.clone(), layer: layer.name.clone(), rect: s.rect });
    }
    let all = conflict_edges(shapes, layer.same_mask_spacing);
    // Shapes of ONE net that do not touch still conflict; touching ones of one net never reach
    // here (gap2 is None), and touching ones of two nets are shorts, also excluded.
    for &(i, j, g2) in &all {
        let (a, b) = (&shapes[i], &shapes[j]);
        if a.mask != 0 && a.mask == b.mask {
            let same_track = if layer.horizontal {
                a.rect[1] < b.rect[3] && b.rect[1] < a.rect[3]
            } else {
                a.rect[0] < b.rect[2] && b.rect[0] < a.rect[2]
            };
            findings.push(Finding::SameMaskSpacing {
                layer: layer.name.clone(),
                mask: a.mask,
                a: (a.net.clone(), a.rect),
                b: (b.net.clone(), b.rect),
                gap: (g2 as f64).sqrt().round() as i64,
                same_track,
            });
        }
    }
    if layer.num_masks == 2 {
        for cyc in odd_cycles(shapes.len(), &all) {
            findings.push(Finding::OddCycle {
                layer: layer.name.clone(),
                shapes: cyc.into_iter().map(|k| (shapes[k].net.clone(), shapes[k].rect)).collect(),
            });
        }
    }
}

// ---- the pass over a database -------------------------------------------------------------------

/// Options of [`colour_masks`].
#[derive(Debug, Clone, Default)]
pub struct ColourOptions {
    /// Same-mask spacing per layer (DBU), overriding everything else.
    pub same_mask_spacing: BTreeMap<String, i32>,
    /// LEF58 same-mask rules per layer, read from the tech LEF (odb drops them). Microns.
    pub same_mask_rules: BTreeMap<String, SameMaskRules>,
    /// Only check; write no colour.
    pub check_only: bool,
}

/// What [`colour_masks`] did.
#[derive(Debug, Clone, Default)]
pub struct ColourReport {
    pub layers: Vec<MaskLayer>,
    /// Layers whose tracks carry no DEF mask (mask 1 assumed on each pattern's first track).
    pub unmasked_tracks: Vec<String>,
    pub nets_checked: usize,
    pub nets_recoloured: usize,
    /// Shapes on multi-mask layers after the pass, and how many carry a mask.
    pub shapes: usize,
    pub coloured: usize,
    pub findings: Vec<Finding>,
}

/// The block's multi-mask routing layers and their track grids.
pub fn mask_layers(db: &Db, opts: &ColourOptions) -> Result<Vec<MaskLayer>> {
    let mut out = Vec::new();
    for (name, dir) in db.layers_with_direction()? {
        if db.layer_get_type(&name)? != "ROUTING" || db.layer_get_num_masks(&name) < 2 {
            continue;
        }
        let horizontal = dir == "HORIZONTAL";
        let (px, py) = db.track_patterns(&name)?;
        let (mx, my) = db.track_pattern_masks(&name)?;
        let (pats, masks) = if horizontal { (py, my) } else { (px, mx) };
        let tracks = pats
            .iter()
            .enumerate()
            .map(|(k, &(origin, count, step))| {
                let m = masks.get(k).copied().unwrap_or_default();
                TrackPattern { origin, count, step, first_mask: m.first_mask, same_mask: m.same_mask }
            })
            .collect();
        // odb drops the SAMEMASK forms of LEF58 spacing, so they come from the tech LEF
        // through `same_mask_rules`; the option overrides both. Without either, the base rule: a
        // layer with a spacing table and no plain SPACING reads 0 there, so its table at minimum
        // width and no parallel run stands in.
        let lef58 = opts.same_mask_rules.get(&name).and_then(SameMaskRules::min_spacing);
        let dbu = f64::from(db.dbu_per_micron());
        let (spacing, spacing_source) = match (opts.same_mask_spacing.get(&name), lef58) {
            (Some(&s), _) => (s, SpacingSource::Option),
            (None, Some(um)) => ((um * dbu).round() as i32, SpacingSource::Lef58SameMask),
            (None, None) => {
                let base = match db.layer_get_spacing(&name) {
                    0 => db.layer_find_v55_spacing(&name, db.layer_get_width(&name) as i32, 0).unwrap_or(0),
                    s => s,
                };
                (base, SpacingSource::BaseSpacing)
            }
        };
        out.push(MaskLayer {
            number: db.layer_get_number(&name) as i64,
            num_masks: db.layer_get_num_masks(&name).min(3) as u8,
            horizontal,
            tracks,
            same_mask_spacing: spacing,
            spacing_source,
            name,
        });
    }
    Ok(out)
}

/// Colour every regular net's wires on the multi-mask layers by track parity, write the colours
/// back, then check the result. One stage per step, in order.
pub fn colour_masks(db: &mut Db, opts: &ColourOptions) -> Result<ColourReport> {
    let mut rep = ColourReport { layers: mask_layers(db, opts)?, ..ColourReport::default() };
    if rep.layers.is_empty() {
        return Ok(rep);
    }
    rep.unmasked_tracks = rep
        .layers
        .iter()
        .filter(|l| l.tracks.iter().all(|t| t.first_mask == 0))
        .map(|l| l.name.clone())
        .collect();
    let by_name: BTreeMap<String, MaskLayer> = rep.layers.iter().map(|l| (l.name.clone(), l.clone())).collect();
    for net in db.block_get_nets() {
        if db.net_is_special(&net) {
            continue;
        }
        let recs = db.net_wire_decode(&net)?;
        if recs.is_empty() {
            continue;
        }
        rep.nets_checked += 1;
        if !opts.check_only {
            colour_net(db, &net, &recs, &by_name, &mut rep)?;
        }
    }
    check_block(db, &by_name, &mut rep)?;
    Ok(rep)
}

/// Stage: plan, re-encode, write back and verify one net.
fn colour_net(
    db: &mut Db,
    net: &str,
    recs: &[String],
    layers: &BTreeMap<String, MaskLayer>,
    rep: &mut ColourReport,
) -> Result<()> {
    let wire_type = recs[0].strip_prefix("T|").unwrap_or("");
    let parsed = match parse_records(&recs[1..]) {
        Ok(p) => p,
        Err(why) => {
            rep.findings.push(Finding::NotRewritten { net: net.into(), why });
            return Ok(());
        }
    };
    let state = db.net_wire_decode_masks(net)?;
    let state = &state[1.min(state.len())..];
    let plan = plan_net(net, layers, &parsed, state, &mut rep.findings);
    if !plan.changed {
        return Ok(());
    }
    if wire_type != "ROUTED" {
        let why = format!("{wire_type} wire (only ROUTED wires are re-encoded)");
        rep.findings.push(Finding::NotRewritten { net: net.into(), why });
        return Ok(());
    }
    let keep = NetPlan {
        color: state.iter().map(|s| s.color).collect(),
        via: state.iter().map(|s| s.via).collect(),
        changed: false,
    };
    let (ops, names, old_ops, old_names) = match (encode_ops(&parsed, &plan), encode_ops(&parsed, &keep)) {
        (Ok((o, n)), Ok((oo, on))) => (o, n, oo, on),
        (Err(why), _) | (_, Err(why)) => {
            rep.findings.push(Finding::NotRewritten { net: net.into(), why });
            return Ok(());
        }
    };
    let mut before = db.net_wire_boxes(net);
    before.sort_by_key(|b| (b.layer, b.x0, b.y0, b.x1, b.y1, b.from_via));
    db.net_write_wire(net, &ops, &names)?;
    let mut after = db.net_wire_boxes(net);
    after.sort_by_key(|b| (b.layer, b.x0, b.y0, b.x1, b.y1, b.from_via));
    if after != before {
        // Undo: the re-encoded wire must have exactly the old geometry.
        db.net_write_wire(net, &old_ops, &old_names)?;
        let why = "re-encoding changed the wire's geometry; the original wire was restored".to_string();
        rep.findings.push(Finding::NotRewritten { net: net.into(), why });
        return Ok(());
    }
    rep.nets_recoloured += 1;
    Ok(())
}

/// Stage: collect every shape on each multi-mask layer with its mask (routed wires, special
/// wires, block pins) and check it.
fn check_block(db: &Db, layers: &BTreeMap<String, MaskLayer>, rep: &mut ColourReport) -> Result<()> {
    let by_number: BTreeMap<i64, &MaskLayer> = layers.values().map(|l| (l.number, l)).collect();
    let mut shapes: BTreeMap<i64, Vec<MaskShape>> = BTreeMap::new();
    for net in db.block_get_nets() {
        if db.net_is_special(&net) {
            continue;
        }
        let boxes = db.net_wire_boxes(&net);
        let masks = db.net_wire_box_masks(&net)?;
        for (b, &m) in boxes.iter().zip(&masks) {
            if by_number.contains_key(&b.layer) {
                shapes.entry(b.layer).or_default().push(MaskShape { net: net.clone(), rect: [b.x0, b.y0, b.x1, b.y1], mask: m });
            }
        }
    }
    let sw = db.swire_boxes()?;
    let swm = db.swire_box_masks()?;
    let swn = db.swire_box_nets()?;
    for ((b, &m), n) in sw.iter().zip(&swm).zip(&swn) {
        if by_number.contains_key(&b.0) {
            shapes.entry(b.0).or_default().push(MaskShape { net: n.clone(), rect: [b.1, b.2, b.3, b.4], mask: m });
        }
    }
    for t in db.block_get_b_terms() {
        let net = db.bterm_get_net(&t);
        for p in 0..db.num_bterm_get_b_pins(&t) {
            let boxes = db.bpin_layer_boxes(&t, p)?;
            let masks = db.bpin_layer_box_masks(&t, p)?;
            for (b, &m) in boxes.iter().zip(&masks) {
                if by_number.contains_key(&b.0) {
                    shapes.entry(b.0).or_default().push(MaskShape { net: net.clone(), rect: [b.1, b.2, b.3, b.4], mask: m });
                }
            }
        }
    }
    for (num, v) in &shapes {
        let layer = by_number[num];
        rep.shapes += v.len();
        rep.coloured += v.iter().filter(|s| s.mask != 0).count();
        check_layer(layer, v, &mut rep.findings);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(horizontal: bool, tracks: Vec<TrackPattern>) -> MaskLayer {
        MaskLayer { name: "M1".into(), number: 1, num_masks: 2, horizontal, tracks, same_mask_spacing: 60,
                    spacing_source: SpacingSource::Option }
    }

    fn pat(origin: i32, count: i32, step: i32, first_mask: u8, same_mask: bool) -> TrackPattern {
        TrackPattern { origin, count, step, first_mask, same_mask }
    }

    /// DEF `TRACKS ... MASK n`: the first track has mask n and the masks then cycle; `SAMEMASK`
    /// keeps every track on mask n; no MASK starts at 1. Off the pattern is off-track.
    #[test]
    fn track_masks_follow_the_def_track_mask_rule() {
        let l = layer(true, vec![pat(50, 10, 100, 2, false)]);
        assert_eq!(l.track_at(50), TrackHit::On { pattern: 0, index: 0, mask: 2 });
        assert_eq!(l.track_at(150), TrackHit::On { pattern: 0, index: 1, mask: 1 });
        assert_eq!(l.track_at(250), TrackHit::On { pattern: 0, index: 2, mask: 2 });
        assert_eq!(l.track_at(100), TrackHit::Off, "between tracks");
        assert_eq!(l.track_at(1050), TrackHit::Off, "past the last track");
        assert_eq!(l.track_at(-50), TrackHit::Off, "before the first track");
        let same = layer(true, vec![pat(50, 10, 100, 2, true)]);
        assert_eq!(same.track_at(150), TrackHit::On { pattern: 0, index: 1, mask: 2 });
        let none = layer(true, vec![pat(50, 10, 100, 0, false)]);
        assert_eq!(none.track_at(50), TrackHit::On { pattern: 0, index: 0, mask: 1 });
        let three = MaskLayer { num_masks: 3, ..layer(true, vec![pat(0, 10, 10, 3, false)]) };
        let m: Vec<u8> = (0..5).map(|i| match three.track_at(i * 10) {
            TrackHit::On { mask, .. } => mask,
            TrackHit::Off => 0,
        }).collect();
        assert_eq!(m, vec![3, 1, 2, 3, 1], "three masks cycle from the first track's");
    }

    fn st(c: Option<u8>) -> WireMaskState {
        WireMaskState { color: c, via: None }
    }

    /// A segment takes its track's mask; a pre-coloured one keeps its own (and a disagreement is
    /// reported); off-track and wrong-way segments are left uncoloured and reported.
    #[test]
    fn segments_take_their_tracks_mask_and_the_rest_is_reported() {
        let ml = layer(true, vec![pat(50, 10, 100, 1, false)]);
        let layers = BTreeMap::from([("M1".to_string(), ml)]);
        let recs = vec![
            WireRec::Path("M1".into()), WireRec::Point(0, 150), WireRec::Point(900, 150), // track 1: mask 2
            WireRec::Path("M1".into()), WireRec::Point(0, 250), WireRec::Point(900, 250), // pre-coloured 2, track mask 1
            WireRec::Path("M1".into()), WireRec::Point(0, 200), WireRec::Point(900, 200), // off-track
            WireRec::Path("M1".into()), WireRec::Point(0, 50), WireRec::Point(0, 350),    // wrong way
            WireRec::End,
        ];
        let mut state = vec![st(None); recs.len()];
        state[5] = st(Some(2));
        let mut f = Vec::new();
        let plan = plan_net("n", &layers, &recs, &state, &mut f);
        assert!(plan.changed);
        assert_eq!(plan.color[2], Some(2));
        assert_eq!(plan.color[5], Some(2), "kept");
        assert_eq!(plan.color[8], None);
        assert_eq!(plan.color[11], None);
        let kinds: Vec<&str> = f.iter().map(Finding::kind).collect();
        assert_eq!(kinds, vec!["precoloured_mismatch", "off_track", "wrong_way"]);
    }

    /// A via's metal digits take the track masks of the via point on each multi-mask layer it
    /// touches; its cut digit is left as it was.
    #[test]
    fn via_metal_digits_take_their_tracks_masks() {
        let h = MaskLayer { name: "M1".into(), ..layer(true, vec![pat(50, 10, 100, 1, false)]) };
        let v = MaskLayer { name: "M2".into(), number: 2, ..layer(false, vec![pat(50, 10, 100, 2, false)]) };
        let layers = BTreeMap::from([("M1".to_string(), h), ("M2".to_string(), v)]);
        let recs = vec![
            WireRec::Path("M1".into()),
            WireRec::Point(50, 150),
            WireRec::Point(150, 150),
            WireRec::Via { block: false, name: "V12".into(), bottom: "M1".into(), top: "M2".into() },
            WireRec::Point(150, 350),
            WireRec::End,
        ];
        let mut f = Vec::new();
        let plan = plan_net("n", &layers, &recs, &vec![st(None); recs.len()], &mut f);
        assert!(f.is_empty(), "{f:?}");
        // y 150 is M1 track 1 (mask 2); x 150 is M2 track 1 from first mask 2 (mask 1).
        assert_eq!(plan.via[3], Some(ViaMask { bottom: 2, cut: 0, top: 1 }));
        assert_eq!(plan.color[2], Some(2));
        assert_eq!(plan.color[4], Some(1), "the M2 segment after the via");
    }

    /// The re-encoded stream sets a colour only where it changes, re-sets it after a via (odb's
    /// encoder clears the metal colour there) and keeps the via colour as sticky state.
    #[test]
    fn encoding_follows_the_encoders_colour_state() {
        let recs = vec![
            WireRec::Path("M1".into()),
            WireRec::Point(0, 0),
            WireRec::Point(10, 0),
            WireRec::Point(20, 0),
            WireRec::Via { block: false, name: "V".into(), bottom: "M1".into(), top: "M2".into() },
            WireRec::Point(20, 10),
            WireRec::Path("M1".into()),
            WireRec::Rule,
            WireRec::Point(0, 5),
            WireRec::PointExt(5, 5, 7),
            WireRec::End,
        ];
        let mut plan = NetPlan { color: vec![None; recs.len()], via: vec![None; recs.len()], changed: true };
        plan.color[2] = Some(1);
        plan.color[3] = Some(1);
        plan.via[4] = Some(ViaMask { bottom: 1, cut: 0, top: 2 });
        plan.color[5] = Some(1);
        plan.color[9] = Some(2);
        let (ops, names) = encode_ops(&recs, &plan).unwrap();
        assert_eq!(names, vec!["M1".to_string(), "V".to_string(), "M2".to_string()]);
        #[rustfmt::skip]
        assert_eq!(ops, vec![
            0, 0, 0, 1, 0, 0,
            6, 1, 1, 10, 0,
            0, 0, 0, 1, 10, 0,    // 3rd point coloured: a new path from the 2nd
            1, 20, 0,             // still colour 1: no op
            8, 1, 0, 2, 3, 1,     // via colour, then the tech via
            0, 2, 0, 1, 20, 0,    // the 3rd point of the path again: new path on M2
            6, 1, 1, 20, 10,      // colour 1 again: the via cleared it
            0, 0, 1, 1, 0, 5,     // path with the net's rule
            6, 2, 2, 5, 5, 7,
        ]);
        let bad = vec![WireRec::Other(1), WireRec::End];
        let p = NetPlan { color: vec![None; 2], via: vec![None; 2], changed: true };
        assert!(encode_ops(&bad, &p).unwrap_err().contains("op 1"));
    }

    fn sh(net: &str, r: [i32; 4], mask: u8) -> MaskShape {
        MaskShape { net: net.into(), rect: r, mask }
    }

    /// Conflicts are pairs under the spacing, edge to edge (Euclidean at corners); touching shapes
    /// are not conflicts.
    #[test]
    fn conflict_edges_are_pairs_under_the_spacing() {
        let s = vec![
            sh("a", [0, 0, 100, 50], 1),
            sh("b", [150, 0, 250, 50], 1),  // 50 to the right of a
            sh("c", [0, 120, 100, 170], 2), // 70 above a
            sh("d", [140, 80, 200, 120], 2), // corner to a: dx 40, dy 30 -> 50
            sh("e", [100, 0, 140, 50], 1),  // touches a
        ];
        let e: Vec<(usize, usize)> = conflict_edges(&s, 60).into_iter().map(|(a, b, _)| (a, b)).collect();
        assert!(e.contains(&(0, 1)) && e.contains(&(0, 3)));
        assert!(!e.contains(&(0, 2)), "70 >= 60");
        assert!(!e.contains(&(0, 4)), "touching");
    }

    /// Same-mask pairs under the spacing are reported (on one track: a line-end pair); different
    /// masks are not. A K3 on two masks is an odd cycle.
    #[test]
    fn same_mask_pairs_and_odd_cycles_are_reported() {
        let l = layer(true, vec![pat(25, 10, 100, 1, false)]);
        let s = vec![sh("a", [0, 0, 100, 50], 1), sh("b", [140, 0, 240, 50], 1), sh("c", [0, 100, 100, 150], 2)];
        let mut f = Vec::new();
        check_layer(&l, &s, &mut f);
        assert_eq!(f.len(), 1, "{f:?}");
        assert!(matches!(&f[0], Finding::SameMaskSpacing { same_track: true, gap: 40, .. }));
        // The triangle of the PR's odd-cycle test, in our units: three shapes all closer than the
        // spacing to each other.
        let tri = vec![sh("a", [0, 0, 100, 50], 0), sh("b", [0, 90, 100, 140], 0), sh("c", [130, 45, 230, 95], 0)];
        let mut f = Vec::new();
        check_layer(&l, &tri, &mut f);
        let cycles: Vec<&Finding> = f.iter().filter(|x| x.kind() == "odd_cycle").collect();
        assert_eq!(cycles.len(), 1);
        let Finding::OddCycle { shapes, .. } = cycles[0] else { unreachable!() };
        assert_eq!(shapes.len(), 3);
        assert_eq!(f.iter().filter(|x| x.kind() == "uncoloured").count(), 3);
        // An even cycle (a square) is two-colourable: no odd cycle.
        let sq = vec![(0, 1, 1), (1, 2, 1), (2, 3, 1), (3, 0, 1)];
        assert!(odd_cycles(4, &sq).is_empty());
        let pent = vec![(0, 1, 1), (1, 2, 1), (2, 3, 1), (3, 4, 1), (4, 0, 1)];
        let c = odd_cycles(5, &pent);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].len(), 5);
    }
}
