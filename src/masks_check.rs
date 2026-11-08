// SPDX-License-Identifier: Apache-2.0
//! Same-mask rule checks on multi-mask layers: a layer's LEF58 SAMEMASK rules (read by
//! `vyges_loom::lef58_samemask`, since odb drops them) applied pair by pair to its coloured shapes.
//!
//! Checked:
//! * `SPACING s SAMEMASK`: two same-mask shapes keep at least `s`.
//! * `SPACINGTABLE TWOWIDTHS SAMEMASK`: the required spacing is looked up by the two shapes'
//!   widths and their parallel run, with odb's own TWOWIDTHS rule (`_dbTechLayer::getTwIdx`,
//!   `dbTechLayer::findTwSpacing`) — the lookup odb applies to the classic table.
//!
//! The governing requirement of a pair is the largest of these; a gap under it is reported once,
//! naming the rule. Gaps are edge to edge, Euclidean (a corner pair at its corner distance); width
//! is a shape's smaller side.
//!
//! Also checked, on the merged-shape model of [`crate::masks_geom`], as the reference detailed
//! router checks the same rules WITHOUT `SAMEMASK` (drt `FlexGC_metspc.cpp`, `FlexGC_eol.cpp`):
//! * `FORBIDDENSPACING … SAMEMASK` ([`check_forbidden_spc_main`]);
//! * `EOLEXTENSIONSPACING … SAMEMASK` ([`check_metal_end_of_line_ext`]).
//!
//! No reference checks the SAMEMASK forms (odb drops them), so "same mask" is applied the way a
//! mask-split layer is checked: each mask's shapes are a layer of their own, merged per net, and
//! the reference rule runs on that layer unchanged. Correlated against drt on the forms without
//! SAMEMASK. A form drt itself refuses (forbidden spacing without TWOEDGES, PRL or WIDTH, or with
//! WITHIN) is counted in [`SameMaskRulesDbu::not_checked`], so a report never reads as clean for a
//! rule nobody looked at.

use std::collections::BTreeSet;

use crate::masks_geom::{
    axis_distance, generalized_intersect, intersects, set_points, square_distance, turns_left, union_area_in, Dir,
    LayerModel, Rect,
};
use vyges_loom::lef58_samemask::{EolExtensionSpacing, ForbiddenSpacing, SameMaskRules, TwoWidthsTable};

/// A TWOWIDTHS table in DBU, laid out as odb keeps the classic one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TwoWidthsDbu {
    /// Row widths (odb `two_widths_sp_idx_`).
    pub widths: Vec<i32>,
    /// Row parallel run lengths; -1 for a row without PRL (odb lefin's `defaultPrl`).
    pub prls: Vec<i32>,
    /// `spacing[row][col]` (odb `two_widths_sp_spacing_`).
    pub spacing: Vec<Vec<i32>>,
}

fn dbu(um: f64, per_um: f64) -> i32 {
    (um * per_um).round() as i32
}

impl TwoWidthsDbu {
    /// The table in DBU; `None` for an empty table (the reader never builds one).
    #[must_use]
    pub fn from_um(t: &TwoWidthsTable, per_um: f64) -> Option<Self> {
        if t.rows.is_empty() {
            return None;
        }
        Some(TwoWidthsDbu {
            widths: t.rows.iter().map(|r| dbu(r.width, per_um)).collect(),
            prls: t.rows.iter().map(|r| r.prl.map_or(-1, |p| dbu(p, per_um))).collect(),
            spacing: t.rows.iter().map(|r| r.spacings.iter().map(|&s| dbu(s, per_um)).collect()).collect(),
        })
    }

    /// odb `_dbTechLayer::getTwIdx`: the row of the last width BELOW `width` (lower_bound, then
    /// one back unless at the first row), then back further while the row's PRL exceeds `prl`.
    #[must_use]
    pub fn index(&self, width: i32, prl: i32) -> usize {
        let pos = self.widths.partition_point(|&w| w < width);
        let mut idx = pos.saturating_sub(1) as isize;
        while idx >= 0 {
            if prl >= self.prls[idx as usize] {
                return idx as usize;
            }
            idx -= 1;
        }
        0
    }

    /// odb `dbTechLayer::findTwSpacing`: rows and columns by each width, PRL clamped at 0.
    #[must_use]
    pub fn find(&self, width1: i32, width2: i32, prl: i32) -> i32 {
        let prl = prl.max(0);
        let (r, c) = (self.index(width1, prl), self.index(width2, prl));
        self.spacing.get(r).and_then(|row| row.get(c)).copied().unwrap_or(0)
    }

    fn max_entry(&self) -> i32 {
        self.spacing.iter().flatten().copied().max().unwrap_or(0)
    }
}

/// One layer's same-mask rules in DBU.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SameMaskRulesDbu {
    /// The largest `SPACING s SAMEMASK`, if any.
    pub spacing: Option<i32>,
    pub two_widths: Vec<TwoWidthsDbu>,
    /// `FORBIDDENSPACING … SAMEMASK` rules of a form the reference checks.
    pub forbidden: Vec<ForbiddenDbu>,
    /// `EOLEXTENSIONSPACING … SAMEMASK` rules.
    pub eol_extension: Vec<EolExtDbu>,
    /// Rules stated but not checked here, by kind, with their count.
    pub not_checked: Vec<(&'static str, usize)>,
}

/// The rule that sets a pair's requirement.
pub const RULE_SPACING: &str = "SPACING SAMEMASK";
pub const RULE_TWO_WIDTHS: &str = "SPACINGTABLE TWOWIDTHS SAMEMASK";

impl SameMaskRulesDbu {
    #[must_use]
    pub fn from_um(r: &SameMaskRules, per_um: f64) -> Self {
        let mut not_checked = Vec::new();
        let forbidden: Vec<ForbiddenDbu> = r.forbidden.iter().filter_map(|f| ForbiddenDbu::from_um(f, per_um)).collect();
        if forbidden.len() < r.forbidden.len() {
            not_checked.push((NOT_CHECKED_FORBIDDEN_FORM, r.forbidden.len() - forbidden.len()));
        }
        let eol_extension: Vec<EolExtDbu> =
            r.eol_extension.iter().filter_map(|e| EolExtDbu::from_um(e, per_um)).collect();
        if eol_extension.len() < r.eol_extension.len() {
            not_checked.push(("EOLEXTENSIONSPACING SAMEMASK (no entry)", r.eol_extension.len() - eol_extension.len()));
        }
        SameMaskRulesDbu {
            spacing: r.spacing.iter().map(|&s| dbu(s, per_um)).max(),
            two_widths: r.two_widths.iter().filter_map(|t| TwoWidthsDbu::from_um(t, per_um)).collect(),
            forbidden,
            eol_extension,
            not_checked,
        }
    }

    /// True when there is a pair rule (SPACING, TWOWIDTHS) to check.
    #[must_use]
    pub fn has_checked_rules(&self) -> bool {
        self.spacing.is_some() || !self.two_widths.is_empty()
    }

    /// True when there is a shape rule (forbidden spacing, EOL extension) to check.
    #[must_use]
    pub fn has_shape_rules(&self) -> bool {
        !self.forbidden.is_empty() || !self.eol_extension.is_empty()
    }

    /// EOL-extension rules checked without the parts the reference has no model of.
    #[must_use]
    pub fn approximated(&self) -> usize {
        self.eol_extension.iter().filter(|e| e.unmodelled).count()
    }

    /// The farthest any checked rule can reach: the search radius for candidate pairs.
    #[must_use]
    pub fn max_required(&self) -> i32 {
        let tw = self.two_widths.iter().map(TwoWidthsDbu::max_entry).max().unwrap_or(0);
        self.spacing.unwrap_or(0).max(tw)
    }

    /// The governing requirement for two rectangles and the rule it comes from (the first rule
    /// in the order SPACING, TWOWIDTHS on a tie). `None` when no checked rule applies.
    #[must_use]
    pub fn required(&self, a: &[i32; 4], b: &[i32; 4]) -> Option<(i32, &'static str)> {
        let mut best = self.spacing.map(|s| (s, RULE_SPACING));
        let (wa, wb) = (width(a), width(b));
        let p = prl(a, b);
        for t in &self.two_widths {
            let s = t.find(wa, wb, p);
            if best.is_none_or(|(b, _)| s > b) {
                best = Some((s, RULE_TWO_WIDTHS));
            }
        }
        best
    }
}

/// A rectangle's width: its smaller side.
#[must_use]
pub fn width(r: &[i32; 4]) -> i32 {
    (r[2] - r[0]).min(r[3] - r[1])
}

/// Parallel run length of two rectangles: the larger of their X and Y projection overlaps,
/// negative when they overlap in neither (a corner pair).
#[must_use]
pub fn prl(a: &[i32; 4], b: &[i32; 4]) -> i32 {
    let ox = a[2].min(b[2]) - a[0].max(b[0]);
    let oy = a[3].min(b[3]) - a[1].max(b[1]);
    ox.max(oy)
}

/// A forbidden-spacing rule the reference would check is refused in any other form: drt's
/// `io.cpp` skips one without TWOEDGES, without PRL or without WIDTH, or with WITHIN (DRT-0619..622).
pub const NOT_CHECKED_FORBIDDEN_FORM: &str =
    "FORBIDDENSPACING SAMEMASK (needs TWOEDGES, PRL and WIDTH and no WITHIN, as the reference)";
pub const RULE_FORBIDDEN: &str = "FORBIDDENSPACING SAMEMASK";
pub const RULE_EOL_EXTENSION: &str = "EOLEXTENSIONSPACING SAMEMASK";

/// `FORBIDDENSPACING min max SAMEMASK WIDTH w PRL p TWOEDGES t` in DBU (drt
/// `frLef58ForbiddenSpcConstraint`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForbiddenDbu {
    pub min: i32,
    pub max: i32,
    pub width: i32,
    pub prl: i32,
    pub two_edges: i32,
}

impl ForbiddenDbu {
    /// `None` for a form the reference refuses (see [`NOT_CHECKED_FORBIDDEN_FORM`]).
    #[must_use]
    pub fn from_um(f: &ForbiddenSpacing, per_um: f64) -> Option<Self> {
        if f.within.is_some() {
            return None;
        }
        Some(ForbiddenDbu {
            min: dbu(f.min, per_um),
            max: dbu(f.max, per_um),
            width: dbu(f.width?, per_um),
            prl: dbu(f.prl?, per_um),
            two_edges: dbu(f.two_edges?, per_um),
        })
    }
    /// drt `isPrlValid`: strictly longer than PRL.
    fn is_prl_valid(&self, prl: i32) -> bool {
        prl > self.prl
    }
    /// drt `isWidthValid`: strictly narrower than WIDTH.
    fn is_width_valid(&self, width: i32) -> bool {
        width < self.width
    }
    /// drt `isForbiddenSpc`: within [min, max], both ends included.
    fn is_forbidden_spc(&self, spc: i32) -> bool {
        spc >= self.min && spc <= self.max
    }
}

/// `EOLEXTENSIONSPACING s SAMEMASK [PARALLELONLY] {ENDOFLINE w EXTENSION e}…` in DBU (drt
/// `frLef58EolExtensionConstraint`; its table is `fr1DLookupTbl(rows = w, vals = e,
/// lowerBound = false)`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EolExtDbu {
    pub spacing: i32,
    pub parallel_only: bool,
    pub rows: Vec<i32>,
    pub exts: Vec<i32>,
    /// An entry states MINLENGTH, TWOSIDES or a second EXTENSION value, which the reference's
    /// grammar has no place for; checked without them.
    pub unmodelled: bool,
}

impl EolExtDbu {
    #[must_use]
    pub fn from_um(e: &EolExtensionSpacing, per_um: f64) -> Option<Self> {
        let first = |x: &vyges_loom::lef58_samemask::EolExtensionEntry| x.extension.first().copied();
        let rows: Vec<i32> = e.entries.iter().map(|x| dbu(x.eol_width, per_um)).collect();
        let exts: Vec<i32> = e.entries.iter().map(|x| first(x).map(|v| dbu(v, per_um))).collect::<Option<_>>()?;
        if rows.is_empty() {
            return None;
        }
        let unmodelled = e.entries.iter().any(|x| x.extension.len() > 1 || x.min_length.is_some() || x.two_sides);
        Some(EolExtDbu { spacing: dbu(e.spacing, per_um), parallel_only: e.parallel_only, rows, exts, unmodelled })
    }

    /// drt `fr1DLookupTbl::find` with `lowerBound = false`: inside the rows, the entry AFTER the
    /// last row not above `len` (upper_bound), so a length in `[row0, row1)` takes row 1's
    /// value; below the first row the first value, above the last the last.
    #[must_use]
    pub fn find(&self, len: i32) -> i32 {
        let (first, last) = (self.rows[0], self.rows[self.rows.len() - 1]);
        let idx = if len >= first && len <= last {
            self.rows.partition_point(|&r| r <= len).min(self.rows.len())
        } else if len < first {
            0
        } else {
            self.rows.len() - 1
        };
        // past the end only for len == last row, which no EOL edge has (len < max_row)
        self.exts.get(idx).or(self.exts.last()).copied().unwrap_or(0)
    }
    /// drt `findMax`: the LAST value, not the largest.
    fn find_max(&self) -> i32 {
        self.exts.last().copied().unwrap_or(0)
    }
    /// drt `getMaxRow`: the last row, the EOL width.
    fn max_row(&self) -> i32 {
        self.rows.last().copied().unwrap_or(0)
    }
}

/// One violation, keyed as drt's `addMarker` keys them: (bbox, layer, constraint, source nets).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Marker {
    pub rule: &'static str,
    /// The rule's index in its list on the layer.
    pub index: usize,
    pub bbox: Rect,
    pub nets: BTreeSet<usize>,
}

fn tracing() -> bool {
    static T: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *T.get_or_init(|| std::env::var_os("VYGES_MASKS_TRACE").is_some())
}

macro_rules! vyt {
    ($($a:tt)*) => {
        if tracing() {
            eprintln!($($a)*);
        }
    };
}

fn r4(r: &Rect) -> String {
    format!("({},{},{},{})", r[0], r[1], r[2], r[3])
}

/// The two shape-rule stages on one layer model, in the reference's stage order and loop
/// nesting: `checkMetalSpacing` (layer → net → pin → max rectangle → constraint) for forbidden
/// spacing, then `checkMetalEndOfLine` (layer → net → pin → edge → constraint) for EOL extension.
#[must_use]
pub fn check_shape_rules(
    model: &LayerModel,
    horizontal: bool,
    rules: &SameMaskRulesDbu,
    nets: &[String],
) -> BTreeSet<Marker> {
    let mut out = BTreeSet::new();
    for k in 0..model.rects.len() {
        for (ci, con) in rules.forbidden.iter().enumerate() {
            check_forbidden_spc_main(model, k, con, ci, horizontal, nets, &mut out);
        }
    }
    for e in 0..model.edges.len() {
        for (ci, con) in rules.eol_extension.iter().enumerate() {
            check_metal_end_of_line_ext(model, e, con, ci, nets, &mut out);
        }
    }
    out
}

fn net_name<'a>(model: &LayerModel, pin: usize, nets: &'a [String]) -> &'a str {
    nets.get(model.net_of(pin)).map_or("?", String::as_str)
}

/// The checker's `queryMaxRectangle`: every max rectangle of the layer touching `q`.
fn query_max_rectangle(model: &LayerModel, q: &Rect) -> Vec<usize> {
    (0..model.rects.len()).filter(|&k| intersects(&model.rects[k].rect, q)).collect()
}

/// drt `checkForbiddenSpc_queryBox`: the band beside `rect` at `[min, max]` from it, above
/// (`right`) or below for a horizontal rectangle, right or left for a vertical one.
#[must_use]
pub fn check_forbidden_spc_query_box(rect: &Rect, min: i32, max: i32, is_h: bool, right: bool) -> Rect {
    let mut q = *rect;
    match (is_h, right) {
        (true, true) => (q[1], q[3]) = (rect[3] + min, rect[3] + max),
        (true, false) => (q[1], q[3]) = (rect[1] - max, rect[1] - min),
        (false, true) => (q[0], q[2]) = (rect[2] + min, rect[2] + max),
        (false, false) => (q[0], q[2]) = (rect[0] - max, rect[0] - min),
    }
    q
}

/// drt `checkForbiddenSpc_twoedges`: some other max rectangle within TWOEDGES on BOTH sides.
fn check_forbidden_spc_twoedges(model: &LayerModel, k: usize, con: &ForbiddenDbu, is_h: bool) -> bool {
    let r = &model.rects[k].rect;
    [true, false].iter().all(|&right| {
        let q = check_forbidden_spc_query_box(r, 0, con.two_edges, is_h, right);
        query_max_rectangle(model, &q).into_iter().any(|p| p != k)
    })
}

/// drt `checkForbiddenSpc_main(gcRect*, con)`: one max rectangle against its neighbours.
pub fn check_forbidden_spc_main(
    model: &LayerModel,
    k: usize,
    con: &ForbiddenDbu,
    ci: usize,
    layer_horizontal: bool,
    nets: &[String],
    out: &mut BTreeSet<Marker>,
) {
    let r = &model.rects[k];
    vyt!("VYT FS_RECT {}{} w={} len={} fixed={}", net_name(model, r.pin, nets), r4(&r.rect), r.width(), r.length(), r.fixed as i32);
    if !con.is_width_valid(r.width()) || !con.is_prl_valid(r.length()) {
        vyt!("VYT FS_RECT skip width/len");
        return;
    }
    let mut is_h = layer_horizontal;
    if r.width() != r.length() {
        is_h = r.rect[2] - r.rect[0] > r.rect[3] - r.rect[1];
    }
    if !check_forbidden_spc_twoedges(model, k, con, is_h) {
        vyt!("VYT FS_RECT skip twoedges isH={}", is_h as i32);
        return;
    }
    let mut result = query_max_rectangle(model, &check_forbidden_spc_query_box(&r.rect, con.min, con.max, is_h, true));
    result.extend(query_max_rectangle(model, &check_forbidden_spc_query_box(&r.rect, con.min, con.max, is_h, false)));
    for p in result {
        let pr = &model.rects[p];
        vyt!("VYT FS_CAND {}{}", net_name(model, pr.pin, nets), r4(&pr.rect));
        if p == k {
            continue;
        }
        if pr.width() != pr.length() {
            let ptr_h = pr.rect[2] - pr.rect[0] > pr.rect[3] - pr.rect[1];
            if ptr_h != is_h {
                continue;
            }
        }
        check_forbidden_spc_pair(model, k, p, con, ci, is_h, nets, out);
    }
}

/// drt `checkForbiddenSpc_main(rect1, rect2, con, isH)`: the pair's run and gap, then a marker
/// for every third max rectangle lying in the gap between them.
#[allow(clippy::too_many_arguments)]
fn check_forbidden_spc_pair(
    model: &LayerModel,
    k: usize,
    p: usize,
    con: &ForbiddenDbu,
    ci: usize,
    is_h: bool,
    nets: &[String],
    out: &mut BTreeSet<Marker>,
) {
    let (r1, r2) = (&model.rects[k], &model.rects[p]);
    let marker = generalized_intersect(&r1.rect, &r2.rect);
    let (mut prl_x, mut prl_y) = (marker[2] - marker[0], marker[3] - marker[1]);
    let dist_x = axis_distance(&r1.rect, &r2.rect, true);
    let dist_y = axis_distance(&r1.rect, &r2.rect, false);
    vyt!(
        "VYT FS_PAIR r1={}{} r2={}{} isH={} prlX={} prlY={} distX={} distY={}",
        net_name(model, r1.pin, nets), r4(&r1.rect), net_name(model, r2.pin, nets), r4(&r2.rect),
        is_h as i32, prl_x, prl_y, dist_x, dist_y
    );
    if dist_x == 0 && dist_y == 0 {
        vyt!("VYT FS_PAIR skip short");
        return;
    }
    if dist_x != 0 {
        prl_x = -prl_x;
    }
    if dist_y != 0 {
        prl_y = -prl_y;
    }
    if !con.is_prl_valid(if is_h { prl_x } else { prl_y }) {
        vyt!("VYT FS_PAIR skip prl");
        return;
    }
    if !con.is_forbidden_spc(if is_h { dist_y } else { dist_x }) {
        vyt!("VYT FS_PAIR skip spc");
        return;
    }
    vyt!("VYT FS_PAIR marker={}", r4(&marker));
    for t in query_max_rectangle(model, &marker) {
        let tr = &model.rects[t];
        vyt!("VYT FS_THIRD ptr={}{} fixed={}", net_name(model, tr.pin, nets), r4(&tr.rect), tr.fixed as i32);
        if t == k || t == p {
            vyt!("VYT FS_THIRD skip self");
            continue;
        }
        if intersects(&tr.rect, &r1.rect) || intersects(&tr.rect, &r2.rect) {
            vyt!("VYT FS_THIRD skip intersects");
            continue;
        }
        let c = [marker[0].max(tr.rect[0]), marker[1].max(tr.rect[1]), marker[2].min(tr.rect[2]), marker[3].min(tr.rect[3])];
        if c[0] >= c[2] || c[1] >= c[3] {
            vyt!("VYT FS_THIRD skip area0");
            continue;
        }
        if r1.fixed && r2.fixed && tr.fixed {
            vyt!("VYT FS_THIRD skip allfixed");
            continue;
        }
        if !has_route(model, k, &marker) && !has_route(model, p, &marker) && !has_route(model, t, &marker) {
            vyt!("VYT FS_THIRD skip noroute");
            continue;
        }
        vyt!("VYT FS_MARK");
        let nets: BTreeSet<usize> = [r1.pin, r2.pin, tr.pin].iter().map(|&q| model.net_of(q)).collect();
        out.insert(Marker { rule: RULE_FORBIDDEN, index: ci, bbox: marker, nets });
    }
}

/// drt `hasRoute`: part of the rectangle near the marker (the marker bloated by the rectangle's
/// width) is not covered by its net's fixed shapes.
fn has_route(model: &LayerModel, k: usize, marker: &Rect) -> bool {
    let r = &model.rects[k];
    let w = r.width();
    let m = [marker[0] - w, marker[1] - w, marker[2] + w, marker[3] + w];
    let c = [m[0].max(r.rect[0]), m[1].max(r.rect[1]), m[2].min(r.rect[2]), m[3].min(r.rect[3])];
    if c[0] >= c[2] || c[1] >= c[3] {
        return false;
    }
    let target = (c[2] - c[0]) as i64 * (c[3] - c[1]) as i64;
    let fixed = model.fixed_shapes.get(&model.net_of(r.pin)).map_or(0, |f| union_area_in(f, &c));
    fixed < target
}

/// drt `checkMetalEndOfLine_eol_isEolEdge` for an extension rule: shorter than the table's last
/// row, and both its corners convex.
fn is_eol_edge(model: &LayerModel, e: usize, con: &EolExtDbu) -> bool {
    let ed = &model.edges[e];
    if ed.len() >= con.max_row() {
        return false;
    }
    turns_left(&model.edges[ed.prev], ed) && turns_left(ed, &model.edges[ed.next])
}

/// drt `getMetalEolExtRect`: the edge pushed outward by `ext` (N edge east, W north, S west,
/// E south — the polygon's outside).
fn get_metal_eol_ext_rect(model: &LayerModel, e: usize, ext: i32) -> Rect {
    let ed = &model.edges[e];
    let mut r = set_points(ed.low, ed.high);
    match ed.dir() {
        Dir::N => r[2] += ext,
        Dir::W => r[3] += ext,
        Dir::S => r[0] -= ext,
        Dir::E => r[1] -= ext,
    }
    r
}

/// drt `getMetalEolExtQueryRegion`: the extended edge, out to `spacing` on its outside and both
/// ends.
fn get_metal_eol_ext_query_region(dir: Dir, ext_rect: &Rect, spacing: i32) -> Rect {
    let r = ext_rect;
    match dir {
        Dir::N => [r[2], r[1] - spacing, r[2] + spacing, r[3] + spacing],
        Dir::W => [r[0] - spacing, r[3], r[2] + spacing, r[3] + spacing],
        Dir::S => [r[0] - spacing, r[1] - spacing, r[0], r[3] + spacing],
        Dir::E => [r[0] - spacing, r[1] - spacing, r[2] + spacing, r[1]],
    }
}

/// drt `segIntersect`: strict overlap of a (possibly degenerate) rectangle with another, and the
/// rectangle clipped to it.
fn seg_intersect(r1: &Rect, r2: &Rect) -> Option<Rect> {
    let hit = r1[0] < r2[2] && r1[2] > r2[0] && r1[1] < r2[3] && r1[3] > r2[1];
    hit.then(|| [r1[0].max(r2[0]), r1[1].max(r2[1]), r1[2].min(r2[2]), r1[3].min(r2[3])])
}

/// drt `checkMetalEndOfLine_ext`: an EOL edge, extended, against every edge in reach.
pub fn check_metal_end_of_line_ext(
    model: &LayerModel,
    e: usize,
    con: &EolExtDbu,
    ci: usize,
    nets: &[String],
    out: &mut BTreeSet<Marker>,
) {
    let ed = &model.edges[e];
    let eol = is_eol_edge(model, e, con);
    vyt!(
        "VYT EX_EDGE {}({},{})-({},{})d{} fixed={} eol={}",
        net_name(model, ed.pin, nets), ed.low.0, ed.low.1, ed.high.0, ed.high.1, ed.dir() as i32, ed.fixed as i32, eol as i32
    );
    if !eol {
        return;
    }
    let ext = con.find(ed.len());
    vyt!("VYT EX_EDGE len={} ext={}", ed.len(), ext);
    let ext_rect = get_metal_eol_ext_rect(model, e, ext);
    let query = get_metal_eol_ext_query_region(ed.dir(), &ext_rect, con.spacing + con.find_max());
    let results: Vec<usize> = (0..model.edges.len()).filter(|&k| intersects(&model.edges[k].rect(), &query)).collect();
    vyt!("VYT EX_EDGE extRect={} query={} n={}", r4(&ext_rect), r4(&query), results.len());
    for k in results {
        check_metal_end_of_line_ext_helper(model, e, k, &ext_rect, &query, con, ci, nets, out);
    }
}

/// drt `checkMetalEndOfLine_ext_helper`: one edge against the extended EOL edge.
#[allow(clippy::too_many_arguments)]
fn check_metal_end_of_line_ext_helper(
    model: &LayerModel,
    e1: usize,
    e2: usize,
    rect1: &Rect,
    query: &Rect,
    con: &EolExtDbu,
    ci: usize,
    nets: &[String],
    out: &mut BTreeSet<Marker>,
) {
    let (a, b) = (&model.edges[e1], &model.edges[e2]);
    vyt!(
        "VYT EX_PAIR e1={}({},{})-({},{})d{} e2={}({},{})-({},{})d{} f={}{}",
        net_name(model, a.pin, nets), a.low.0, a.low.1, a.high.0, a.high.1, a.dir() as i32,
        net_name(model, b.pin, nets), b.low.0, b.low.1, b.high.0, b.high.1, b.dir() as i32, a.fixed as i32, b.fixed as i32
    );
    if a.fixed && b.fixed {
        vyt!("VYT EX_PAIR skip fixed");
        return;
    }
    if a.pin == b.pin {
        vyt!("VYT EX_PAIR skip samepin");
        return;
    }
    let mut rect2 = b.rect();
    let marker_rect = rect2;
    if is_eol_edge(model, e2, con) {
        rect2 = get_metal_eol_ext_rect(model, e2, con.find(b.len()));
        if con.parallel_only {
            // the side of the extended edge facing edge 1
            let [x0, y0, x1, y1] = rect2;
            rect2 = match a.dir() {
                Dir::S => [x1, y0, x1, y1],
                Dir::E => [x0, y1, x1, y1],
                Dir::N => [x0, y0, x0, y1],
                Dir::W => [x0, y0, x1, y0],
            };
        }
    } else if con.parallel_only && a.dir() as i32 + b.dir() as i32 != 7 {
        vyt!("VYT EX_PAIR skip notopposite");
        return;
    }
    vyt!("VYT EX_PAIR rect2={}", r4(&rect2));
    let Some(clipped) = seg_intersect(&rect2, query) else {
        vyt!("VYT EX_PAIR skip nointersect");
        return;
    };
    let d2 = square_distance(rect1, &clipped);
    vyt!("VYT EX_PAIR clipped={} d2={}", r4(&clipped), d2);
    if d2 >= con.spacing as i64 * con.spacing as i64 {
        vyt!("VYT EX_PAIR skip far");
        return;
    }
    vyt!("VYT EX_MARK");
    let bbox = generalized_intersect(&marker_rect, &a.rect());
    let nets: BTreeSet<usize> = [a.pin, b.pin].iter().map(|&q| model.net_of(q)).collect();
    out.insert(Marker { rule: RULE_EOL_EXTENSION, index: ci, bbox, nets });
}

#[cfg(test)]
mod tests {
    use super::*;
    use vyges_loom::lef58_samemask::SameMaskRules;

    fn rules(props: &[(&str, &str)]) -> SameMaskRulesDbu {
        let mut r = SameMaskRules::default();
        for (n, v) in props {
            r.read_property(n, v);
        }
        SameMaskRulesDbu::from_um(&r, 1000.0)
    }

    /// odb `getTwIdx`: a width selects the row of the last row-width strictly below it (row 0
    /// at or under the first); then rows are walked back while their PRL exceeds the run. A row
    /// without PRL (-1) takes any run.
    #[test]
    fn two_widths_lookup_follows_odb() {
        let t = TwoWidthsDbu {
            widths: vec![0, 100, 200],
            prls: vec![-1, -1, 500],
            spacing: vec![vec![10, 20, 30], vec![20, 40, 50], vec![30, 50, 90]],
        };
        assert_eq!(t.index(0, 0), 0);
        assert_eq!(t.index(50, 0), 0);
        assert_eq!(t.index(100, 0), 0, "lower_bound lands ON 100, one back is row 0");
        assert_eq!(t.index(150, 0), 1);
        assert_eq!(t.index(300, 0), 1, "row 2 needs a run of 500");
        assert_eq!(t.index(300, 500), 2);
        assert_eq!(t.find(300, 300, 600), 90);
        assert_eq!(t.find(150, 300, 0), 40);
        assert_eq!(t.find(150, 50, -20), 20, "a negative run is clamped at 0");
    }

    #[test]
    fn the_governing_rule_is_the_largest_requirement() {
        let r = rules(&[
            ("LEF58_SPACING", "SPACING 0.08 SAMEMASK ;"),
            ("LEF58_SPACINGTABLE", "SPACINGTABLE TWOWIDTHS SAMEMASK WIDTH 0 0.07 0.1 WIDTH 0.1 0.1 0.15 ;"),
            ("LEF58_FORBIDDENSPACING", "FORBIDDENSPACING 0.2 0.25 SAMEMASK ;"),
        ]);
        assert_eq!(r.spacing, Some(80));
        assert_eq!(r.max_required(), 150);
        assert_eq!(r.not_checked, vec![(NOT_CHECKED_FORBIDDEN_FORM, 1)], "no TWOEDGES, PRL or WIDTH");
        let thin = [0, 0, 1000, 50];
        let thin2 = [0, 150, 1000, 200];
        assert_eq!(r.required(&thin, &thin2), Some((80, RULE_SPACING)));
        let wide = [0, 300, 1000, 500];
        let wide2 = [0, 650, 1000, 850];
        assert_eq!(r.required(&wide, &wide2), Some((150, RULE_TWO_WIDTHS)));
        assert_eq!(r.required(&thin, &wide), Some((100, RULE_TWO_WIDTHS)));
    }

    /// drt `fr1DLookupTbl::find`, `lowerBound = false` (upper_bound): rows [40, 60] → a length
    /// below 40 takes row 0, one in [40, 60) row 1, one past 60 the last; `findMax` is the LAST
    /// value even when an earlier one is larger.
    #[test]
    fn the_extension_table_looks_up_as_the_reference() {
        let t = EolExtDbu { spacing: 80, parallel_only: false, rows: vec![40, 60], exts: vec![50, 30], unmodelled: false };
        assert_eq!(t.find(30), 50);
        assert_eq!(t.find(40), 30, "upper_bound: the row after");
        assert_eq!(t.find(50), 30);
        assert_eq!(t.find(70), 30);
        assert_eq!(t.find_max(), 30);
        assert_eq!(t.max_row(), 60);
    }

    /// drt's forbidden-spacing predicates: the run must EXCEED PRL, the width be UNDER WIDTH,
    /// the gap lie in [min, max] with both ends forbidden.
    #[test]
    fn forbidden_predicates_are_strict_where_the_reference_is() {
        let f = ForbiddenDbu { min: 200, max: 300, width: 100, prl: 150, two_edges: 250 };
        assert!(!f.is_prl_valid(150) && f.is_prl_valid(151));
        assert!(!f.is_width_valid(100) && f.is_width_valid(99));
        assert!(f.is_forbidden_spc(200) && f.is_forbidden_spc(300));
        assert!(!f.is_forbidden_spc(199) && !f.is_forbidden_spc(301));
    }

    /// The query band beside a rectangle (drt `checkForbiddenSpc_queryBox`).
    #[test]
    fn forbidden_query_boxes() {
        let r = [0, 0, 1000, 50];
        assert_eq!(check_forbidden_spc_query_box(&r, 200, 300, true, true), [0, 250, 1000, 350]);
        assert_eq!(check_forbidden_spc_query_box(&r, 200, 300, true, false), [0, -300, 1000, -200]);
        assert_eq!(check_forbidden_spc_query_box(&r, 200, 300, false, true), [1200, 0, 1300, 50]);
        assert_eq!(check_forbidden_spc_query_box(&r, 200, 300, false, false), [-300, 0, -200, 50]);
    }

    /// drt `hasRoute`: a max rectangle "has route" near a marker when the part of it within the
    /// marker bloated by its width is not all covered by its net's fixed shapes.
    #[test]
    fn has_route_looks_only_near_the_marker() {
        use crate::masks_geom::{LayerModel, Shape};
        // one net: fixed [0, 1000] and routed [1000, 1600], merged into one max rectangle
        let m = LayerModel::build(&[
            Shape { net: 0, rect: [0, 0, 1000, 50], fixed: true },
            Shape { net: 0, rect: [1000, 0, 1600, 50], fixed: false },
        ]);
        assert_eq!(m.rects.len(), 1);
        assert!(!m.rects[0].fixed);
        assert!(!has_route(&m, 0, &[100, 50, 900, 300]), "near the marker it is all fixed");
        assert!(has_route(&m, 0, &[100, 50, 1100, 300]), "the routed part reaches the marker");
        assert!(has_route(&m, 0, &[960, 50, 990, 300]), "bloated by the width 50 it reaches 1040");
    }

    /// drt `checkMetalEndOfLine_ext_helper` skips a pair of FIXED edges (and two edges of one
    /// pin): two fixed ends 50 apart, extended, are not reported; one routed end is.
    #[test]
    fn eol_extension_skips_two_fixed_edges() {
        use crate::masks_geom::{LayerModel, Shape};
        let mut r = SameMaskRules::default();
        r.read_property("LEF58_EOLEXTENSIONSPACING", "EOLEXTENSIONSPACING 0.08 SAMEMASK ENDOFLINE 0.06 EXTENSION 0.03 ;");
        let rules = SameMaskRulesDbu::from_um(&r, 1000.0);
        let nets = vec!["p".to_string(), "q".to_string()];
        let run = |fixed_q: bool| {
            let m = LayerModel::build(&[
                Shape { net: 0, rect: [0, 0, 50, 1000], fixed: true },
                Shape { net: 1, rect: [0, 1050, 50, 2000], fixed: fixed_q },
            ]);
            check_shape_rules(&m, false, &rules, &nets).len()
        };
        assert_eq!(run(true), 0);
        assert!(run(false) > 0);
    }

    #[test]
    fn widths_and_runs() {
        assert_eq!(width(&[0, 0, 1000, 50]), 50);
        assert_eq!(prl(&[0, 0, 1000, 50], &[200, 100, 700, 150]), 500);
        assert_eq!(prl(&[0, 0, 100, 50], &[150, 100, 250, 150]), -50);
    }
}
