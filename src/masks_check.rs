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
//! * `EOLEXTENSIONSPACING … SAMEMASK` ([`check_metal_end_of_line_ext`]);
//! * `SPACING … ENDOFLINE … SAMEMASK` ([`check_metal_end_of_line_eol`]). odb keeps this form and
//!   the reference checks it mask-blind; here it is checked per mask, with two readings of the
//!   reference that look like defects made explicit ([`EolWithin`], [`EolTwoSides`]).
//!
//! And, with no reference at all (odb keeps only the raw text, so the rule is read from the
//! layer's string property): `JOINTCORNERSPACING … SAMEMASK`, as the LEF/DEF 5.8 reference text
//! defines it ([`crate::joint_corner`]).
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
use crate::joint_corner::{check_joint_corner_spacing, JointCornerDbu};
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
    /// `SPACING … ENDOFLINE … SAMEMASK` rules ([`check_metal_end_of_line_eol`]; odb keeps this
    /// form and the reference checks it mask-blind, so they are read from the database).
    pub eol_spacing: Vec<EolSpacingDbu>,
    /// `JOINTCORNERSPACING … SAMEMASK` rules ([`crate::joint_corner`]; odb keeps them only as
    /// raw layer text, so they are added from the database, not from this reader).
    pub joint_corner: Vec<JointCornerDbu>,
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
            eol_spacing: Vec::new(),
            joint_corner: Vec::new(),
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
        !self.forbidden.is_empty()
            || !self.eol_extension.is_empty()
            || !self.eol_spacing.is_empty()
            || !self.joint_corner.is_empty()
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
    /// Per row: the entry's `MINLENGTH l [TWOSIDES]` as `(l, two_sides)`, `None` when absent.
    pub min_length: Vec<Option<(i32, bool)>>,
    /// An entry states a second EXTENSION value (which no reference models) or a TWOSIDES with no
    /// MINLENGTH; checked without it.
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
        // TWOSIDES only qualifies a MINLENGTH; one without it is reported, as is a second value.
        let unmodelled = e.entries.iter().any(|x| x.extension.len() > 1 || (x.two_sides && x.min_length.is_none()));
        let min_length = e.entries.iter().map(|x| x.min_length.map(|l| (dbu(l, per_um), x.two_sides))).collect();
        Some(EolExtDbu { spacing: dbu(e.spacing, per_um), parallel_only: e.parallel_only, rows, exts, min_length, unmodelled })
    }

    /// drt `fr1DLookupTbl::find` with `lowerBound = false`: inside the rows, the entry AFTER the
    /// last row not above `len` (upper_bound), so a length in `[row0, row1)` takes row 1's
    /// value; below the first row the first value, above the last the last.
    #[must_use]
    pub fn find(&self, len: i32) -> i32 {
        // past the end only for len == last row, which no EOL edge has (len < max_row)
        self.exts.get(self.row_index(len)).or(self.exts.last()).copied().unwrap_or(0)
    }

    /// The row an end of length `len` falls in, as [`EolExtDbu::find`] looks it up.
    fn row_index(&self, len: i32) -> usize {
        let (first, last) = (self.rows[0], self.rows[self.rows.len() - 1]);
        if len >= first && len <= last {
            self.rows.partition_point(|&r| r <= len).min(self.rows.len())
        } else if len < first {
            0
        } else {
            self.rows.len() - 1
        }
    }

    /// The `MINLENGTH l [TWOSIDES]` that governs an end of length `len`: its row's, found as the
    /// extension is (interpretation: the reference has no per-row length clause).
    #[must_use]
    pub fn min_length_for(&self, len: i32) -> Option<(i32, bool)> {
        let i = self.row_index(len);
        self.min_length.get(i).or(self.min_length.last()).copied().flatten()
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

std::thread_local! {
    static CAPTURE: std::cell::RefCell<Option<Vec<String>>> = const { std::cell::RefCell::new(None) };
}

/// Run `f` with this thread's trace lines collected instead of printed: the same `VYT` lines
/// `VYGES_MASKS_TRACE` prints, in call order. Tests compare them with the reference's trace.
pub fn capture_trace<R>(f: impl FnOnce() -> R) -> (R, Vec<String>) {
    let saved = CAPTURE.with(|c| c.replace(Some(Vec::new())));
    let r = f();
    let lines = CAPTURE.with(|c| c.replace(saved)).unwrap_or_default();
    (r, lines)
}

fn capturing() -> bool {
    CAPTURE.with(|c| c.borrow().is_some())
}

fn emit(line: String) {
    let pushed = CAPTURE.with(|c| c.borrow_mut().as_mut().map(|v| v.push(line.clone())).is_some());
    if !pushed {
        eprintln!("{line}");
    }
}

macro_rules! vyt {
    ($($a:tt)*) => {
        if capturing() || tracing() {
            emit(format!($($a)*));
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
        for (ci, con) in rules.eol_spacing.iter().enumerate() {
            check_metal_end_of_line_eol(model, e, con, ci, nets, &mut out);
        }
    }
    // no reference stage: the LEF/DEF reference's joint corner spacing, after the reference's two
    for (ci, con) in rules.joint_corner.iter().enumerate() {
        check_joint_corner_spacing(model, con, ci, &mut out);
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
    // Kept as the reference has it, though unreachable here: the query box spans the rectangle's
    // own extent along its direction, so a candidate's distance along that axis is always 0 and
    // only the other axis's run length is read (a mutant removing this cannot be observed).
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

/// drt `checkMetalEndOfLine_eol_hasMinMaxLength`, its MINLENGTH branch: an end qualifies when its
/// previous OR next side edge is at least `min_len` long; with TWOSIDES, when BOTH are. Closed
/// comparisons.
///
/// 🔑 Interpretation. The reference applies this to a different rule (the LEF58 SPACING ENDOFLINE
/// `MINLENGTH l [TWOSIDES]` clause); its EOL-extension grammar has no MINLENGTH or TWOSIDES at all
/// and drops such a rule. Here it qualifies the end of an EOL-extension rule, at the point the
/// reference qualifies an end of its EOL spacing rule: after the end test, on the triggering end
/// only (the far edge's own end test in the helper does not apply it). TWOSIDES is taken as
/// written; the reference's EOL-spacing reader passes a different flag for it, so there a
/// `MINLENGTH l TWOSIDES` is evaluated one-sided.
fn check_metal_end_of_line_eol_has_min_max_length(
    model: &LayerModel,
    e: usize,
    (is_max, len, two_sides): (bool, i32, bool),
    nets: &[String],
) -> bool {
    let ed = &model.edges[e];
    let prev = model.edges[ed.prev].len();
    let next = model.edges[ed.next].len();
    let res = if is_max { has_max_length(prev, next, len, two_sides) } else { has_min_length(prev, next, len, two_sides) };
    vyt!(
        "VYT ML_EDGE {}({},{})-({},{})d{} prev={} next={} max={} len={} two={} res={}",
        net_name(model, ed.pin, nets), ed.low.0, ed.low.1, ed.high.0, ed.high.1, ed.dir() as i32, prev, next,
        is_max as i32, len, two_sides as i32, res as i32
    );
    res
}

/// The MINLENGTH test on the two side-edge lengths of an end.
#[must_use]
pub fn has_min_length(prev: i32, next: i32, min_len: i32, two_sides: bool) -> bool {
    if two_sides {
        return prev >= min_len && next >= min_len;
    }
    prev >= min_len || next >= min_len
}

/// The MAXLENGTH test on the two side-edge lengths of an end (drt
/// `checkMetalEndOfLine_eol_hasMinMaxLength`, `isMaxLength`): closed, both sides with TWOSIDES,
/// either side without.
#[must_use]
pub fn has_max_length(prev: i32, next: i32, max_len: i32, two_sides: bool) -> bool {
    if two_sides {
        return prev <= max_len && next <= max_len;
    }
    prev <= max_len || next <= max_len
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
    if let Some(ml) = con.min_length_for(ed.len()) {
        if !check_metal_end_of_line_eol_has_min_max_length(model, e, (false, ml.0, ml.1), nets) {
            return;
        }
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

// ---- SPACING ENDOFLINE … SAMEMASK (drt FlexGC_eol.cpp, checkMetalEndOfLine_eol) -----------------

/// The rule name in markers and findings.
pub const RULE_EOL_SPACING: &str = "SPACING ENDOFLINE SAMEMASK";

/// How the WITHIN of a SAMEMASK end-of-line rule is read. The default is [`EolWithin::AsWritten`]:
/// a divergence from the reference by decision (drt divergence register D8; the foundry kit's
/// SAMEMASK end-of-line rules state the lateral window on purpose). [`EolWithin::Reference`]
/// stays for correlation against the unpatched reference.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EolWithin {
    /// As the reference reads it: drt `frLef58SpacingEndOfLineWithinConstraint::getEolWithin`
    /// returns 0 for a SAMEMASK rule, whatever WITHIN says (reported upstream as
    /// OpenROAD#12182). For correlation against the unpatched reference only.
    Reference,
    /// As written in the LEF (the LEF/DEF reference text says nothing of WITHIN changing with
    /// SAMEMASK); the reference patched with vyges-openroad-patches upstream-fix 0002 reads it so.
    /// The default.
    #[default]
    AsWritten,
}

/// How `MINLENGTH l TWOSIDES` (or MAXLENGTH) of an end-of-line rule is read. The default is
/// [`EolTwoSides::AsWritten`], a divergence from the reference by decision (drt divergence
/// register D8); [`EolTwoSides::Reference`] stays for correlation against the unpatched reference.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EolTwoSides {
    /// As the reference reads it: drt `io.cpp` passes the PARALLELEDGE TWOEDGES flag where
    /// TWOSIDES belongs, so `MINLENGTH l TWOSIDES` without TWOEDGES is evaluated one-sided
    /// (reported upstream as OpenROAD#12181). For correlation against the unpatched reference only.
    Reference,
    /// As written: TWOSIDES means both sides (the LEF/DEF reference text); the reference patched
    /// with vyges-openroad-patches upstream-fix 0001 reads it so. The default.
    #[default]
    AsWritten,
}

/// `MINLENGTH l` / `MAXLENGTH l` of an end-of-line rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EolMinMax {
    pub max: bool,
    pub length: i32,
    /// TWOSIDES as written.
    pub two_sides: bool,
    /// PARALLELEDGE … TWOEDGES (always false here: PARALLELEDGE is not checked).
    pub two_edges: bool,
}

/// `ENDTOEND s [EXTENSION e]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EolEndToEnd {
    pub space: i32,
    pub extension: Option<i32>,
}

/// `SPACING s ENDOFLINE w WITHIN wi SAMEMASK [ENDTOEND …] [MINLENGTH | MAXLENGTH …]` in DBU, read
/// from odb's rule (odb keeps this form; the reference checks it mask-blind).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EolSpacingDbu {
    pub eol_space: i32,
    pub eol_width: i32,
    /// WITHIN as written.
    pub within: i32,
    pub end_to_end: Option<EolEndToEnd>,
    pub min_max: Option<EolMinMax>,
    pub within_policy: EolWithin,
    pub two_sides_policy: EolTwoSides,
}

impl EolSpacingDbu {
    /// drt `getEolWithin` under the chosen reading.
    #[must_use]
    pub fn eol_within(&self) -> i32 {
        match self.within_policy {
            EolWithin::Reference => 0,
            EolWithin::AsWritten => self.within,
        }
    }

    /// drt `isTwoSides` under the chosen reading.
    #[must_use]
    pub fn two_sides(&self) -> bool {
        self.min_max.is_some_and(|m| match self.two_sides_policy {
            EolTwoSides::Reference => m.two_edges,
            EolTwoSides::AsWritten => m.two_sides,
        })
    }
}

/// The layer's SAMEMASK end-of-line rules from odb, and the ones not checked by clause. Rules
/// without SAMEMASK are the reference's (checked mask-blind there) and are not read.
pub fn eol_spacing_rules(
    db: &crate::Db,
    layer: &str,
    within_policy: EolWithin,
    two_sides_policy: EolTwoSides,
) -> (Vec<EolSpacingDbu>, Vec<(&'static str, usize)>) {
    let mut out = Vec::new();
    let mut nc: Vec<(&'static str, usize)> = Vec::new();
    for i in 0..db.num_layer_get_tech_layer_spacing_eol_rules(layer) {
        if !db.spacingeolrule_is_same_mask_valid(layer, i) {
            continue;
        }
        let unmodelled: [(&'static str, bool); 18] = [
            ("SPACING ENDOFLINE SAMEMASK EXACTWIDTH", db.spacingeolrule_is_exact_width_valid(layer, i)),
            ("SPACING ENDOFLINE SAMEMASK EXCEPTEXACTWIDTH", db.spacingeolrule_is_except_exact_width_valid(layer, i)),
            ("SPACING ENDOFLINE SAMEMASK FILLCONCAVECORNER", db.spacingeolrule_is_fill_concave_corner_valid(layer, i)),
            ("SPACING ENDOFLINE SAMEMASK EQUALRECTWIDTH", db.spacingeolrule_is_equal_rect_width_valid(layer, i)),
            ("SPACING ENDOFLINE SAMEMASK WRONGDIRSPACING", db.spacingeolrule_is_wrong_dir_spacing_valid(layer, i)),
            ("SPACING ENDOFLINE SAMEMASK OPPOSITEWIDTH", db.spacingeolrule_is_opposite_width_valid(layer, i)),
            ("SPACING ENDOFLINE SAMEMASK wrongDirWithin", db.spacingeolrule_is_wrong_dir_within_valid(layer, i)),
            ("SPACING ENDOFLINE SAMEMASK WITHCUT", db.spacingeolrule_is_withcut_valid(layer, i)),
            ("SPACING ENDOFLINE SAMEMASK ENCLOSUREEND", db.spacingeolrule_is_enclosure_end_valid(layer, i)),
            ("SPACING ENDOFLINE SAMEMASK ENDPRLSPACING", db.spacingeolrule_is_end_prl_spacing_valid(layer, i)),
            ("SPACING ENDOFLINE SAMEMASK ENDTOEND cut spaces", db.spacingeolrule_is_cut_spaces_valid(layer, i)),
            ("SPACING ENDOFLINE SAMEMASK ENDTOEND wrong-direction extension", db.spacingeolrule_is_wrong_dir_extension_valid(layer, i)),
            ("SPACING ENDOFLINE SAMEMASK OTHERENDWIDTH", db.spacingeolrule_is_other_end_width_valid(layer, i)),
            ("SPACING ENDOFLINE SAMEMASK PARALLELEDGE", db.spacingeolrule_is_parallel_edge_valid(layer, i)),
            ("SPACING ENDOFLINE SAMEMASK ENCLOSECUT", db.spacingeolrule_is_enclose_cut_valid(layer, i)),
            ("SPACING ENDOFLINE SAMEMASK TOCONCAVECORNER", db.spacingeolrule_is_to_concave_corner_valid(layer, i)),
            ("SPACING ENDOFLINE SAMEMASK TONOTCHLENGTH", db.spacingeolrule_is_to_notch_length_valid(layer, i)),
            ("SPACING ENDOFLINE SAMEMASK MINADJACENTLENGTH", db.spacingeolrule_is_min_adjacent_length_valid(layer, i)),
        ];
        if let Some((clause, _)) = unmodelled.iter().find(|(_, on)| *on) {
            match nc.iter_mut().find(|(c, _)| c == clause) {
                Some(e) => e.1 += 1,
                None => nc.push((clause, 1)),
            }
            continue;
        }
        let end_to_end = db.spacingeolrule_is_end_to_end_valid(layer, i).then(|| EolEndToEnd {
            space: db.spacingeolrule_get_end_to_end_space(layer, i),
            extension: db.spacingeolrule_is_extension_valid(layer, i).then(|| db.spacingeolrule_get_extension(layer, i)),
        });
        // drt io: MINLENGTH wins when both are given (`if (min) … else max`)
        let min_max = if db.spacingeolrule_is_min_length_valid(layer, i) {
            Some((false, db.spacingeolrule_get_min_length(layer, i)))
        } else if db.spacingeolrule_is_max_length_valid(layer, i) {
            Some((true, db.spacingeolrule_get_max_length(layer, i)))
        } else {
            None
        }
        .map(|(max, length)| EolMinMax {
            max,
            length,
            two_sides: db.spacingeolrule_is_two_sides_valid(layer, i),
            two_edges: db.spacingeolrule_is_two_edges_valid(layer, i),
        });
        out.push(EolSpacingDbu {
            eol_space: db.spacingeolrule_get_eol_space(layer, i),
            eol_width: db.spacingeolrule_get_eol_width(layer, i),
            within: db.spacingeolrule_get_eol_within(layer, i),
            end_to_end,
            min_max,
            within_policy,
            two_sides_policy,
        });
    }
    (out, nc)
}

/// drt `checkMetalEndOfLine_eol_hasParallelEdge_oneDir_getParallelEdgeRect`: the one-DBU strip
/// on the inside of an edge.
fn get_parallel_edge_rect(model: &LayerModel, e: usize) -> Rect {
    let ed = &model.edges[e];
    let (l, h) = (ed.low, ed.high);
    match ed.dir() {
        Dir::E => [l.0, l.1, h.0, h.1 + 1],
        Dir::W => [h.0, h.1 - 1, l.0, l.1],
        Dir::N => [l.0 - 1, l.1, h.0, h.1],
        Dir::S => [h.0, h.1, l.0 + 1, l.1],
    }
}

/// `gtl::intersects(a, b, false)`: the interiors overlap.
fn overlaps(a: &Rect, b: &Rect) -> bool {
    a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3]
}

/// drt `tmpPoly &= getPolygons(layer, false)` non-empty: some ROUTE (not fixed) shape of the net
/// overlaps `region` with area.
fn has_route_in(model: &LayerModel, net: usize, region: &Rect) -> bool {
    model.route_shapes.get(&net).is_some_and(|v| v.iter().any(|r| overlaps(r, region)))
}

/// drt `checkMetalEndOfLine_eol_isEolEdge` for an end-of-line rule: shorter than the EOL width,
/// both corners convex.
fn check_metal_end_of_line_eol_is_eol_edge(model: &LayerModel, e: usize, con: &EolSpacingDbu) -> bool {
    let ed = &model.edges[e];
    if ed.len() >= con.eol_width {
        return false;
    }
    turns_left(&model.edges[ed.prev], ed) && turns_left(ed, &model.edges[ed.next])
}

/// drt `qualifiesAsEol`: the end test, whether the end has route shapes (`hasRoute`), the
/// parallel-edge test (no PARALLELEDGE here: true) and the min/max length test, as
/// `(qualifies, hasRoute)`; `hasRoute` is set before the length test, as the reference sets it.
fn qualifies_as_eol(model: &LayerModel, e: usize, con: &EolSpacingDbu, nets: &[String]) -> (bool, bool) {
    if !check_metal_end_of_line_eol_is_eol_edge(model, e, con) {
        return (false, false);
    }
    let ed = &model.edges[e];
    let mut has_route = false;
    if !ed.fixed {
        has_route = has_route_in(model, model.net_of(ed.pin), &get_parallel_edge_rect(model, e));
    }
    if let Some(m) = con.min_max {
        if !check_metal_end_of_line_eol_has_min_max_length(model, e, (m.max, m.length, con.two_sides()), nets) {
            return (false, has_route);
        }
    }
    (true, has_route)
}

/// drt `checkMetalEndOfLine_eol_hasEol_getQueryBox` for an end (`isEolEdge` true): the box
/// `eolSpace` out from the edge and `eolWithin` past its ends; ENDTOEND raises the space to its
/// own and, with EXTENSION, makes the within twice the extension.
fn check_metal_end_of_line_eol_has_eol_get_query_box(model: &LayerModel, e: usize, con: &EolSpacingDbu) -> Rect {
    let ed = &model.edges[e];
    let mut within = con.eol_within();
    let mut space = con.eol_space;
    if let Some(ee) = con.end_to_end {
        space = space.max(ee.space);
        if let Some(x) = ee.extension {
            within = x * 2;
        }
    }
    let (l, h) = (ed.low, ed.high);
    match ed.dir() {
        Dir::E => [l.0 - within, l.1 - space, h.0 + within, h.1],
        Dir::W => [h.0 - within, h.1, l.0 + within, l.1 + space],
        Dir::N => [l.0, l.1 - within, h.0 + space, h.1 + within],
        Dir::S => [h.0 - space, h.1 - within, l.0, l.1 + within],
    }
}

/// drt `checkMetalEndOfLine_eol`: an end of line against every edge in its query box.
pub fn check_metal_end_of_line_eol(
    model: &LayerModel,
    e: usize,
    con: &EolSpacingDbu,
    ci: usize,
    nets: &[String],
    out: &mut BTreeSet<Marker>,
) {
    let ed = &model.edges[e];
    let eol = check_metal_end_of_line_eol_is_eol_edge(model, e, con);
    vyt!(
        "VYT EL_EDGE {}({},{})-({},{})d{} fixed={} eol={}",
        net_name(model, ed.pin, nets), ed.low.0, ed.low.1, ed.high.0, ed.high.1, ed.dir() as i32, ed.fixed as i32, eol as i32
    );
    if !eol {
        return;
    }
    let (ok, has_route) = qualifies_as_eol(model, e, con, nets);
    vyt!("VYT EL_Q ok={} route={}", ok as i32, has_route as i32);
    if ok {
        check_metal_end_of_line_eol_has_eol(model, e, con, has_route, ci, nets, out);
    }
}

/// drt `checkMetalEndOfLine_eol_hasEol`.
fn check_metal_end_of_line_eol_has_eol(
    model: &LayerModel,
    e: usize,
    con: &EolSpacingDbu,
    has_route: bool,
    ci: usize,
    nets: &[String],
    out: &mut BTreeSet<Marker>,
) {
    let query = check_metal_end_of_line_eol_has_eol_get_query_box(model, e, con);
    let results: Vec<usize> = (0..model.edges.len()).filter(|&k| intersects(&model.edges[k].rect(), &query)).collect();
    vyt!("VYT EL_QUERY query={} n={}", r4(&query), results.len());
    for k in results {
        check_metal_end_of_line_eol_has_eol_check(model, e, k, &query, con, has_route, ci, nets, out);
    }
}

/// drt `checkMetalEndOfLine_eol_hasEol_check`: one edge in the box against the end.
#[allow(clippy::too_many_arguments)]
fn check_metal_end_of_line_eol_has_eol_check(
    model: &LayerModel,
    e: usize,
    k: usize,
    query: &Rect,
    con: &EolSpacingDbu,
    mut has_route: bool,
    ci: usize,
    nets: &[String],
    out: &mut BTreeSet<Marker>,
) {
    let (a, b) = (&model.edges[e], &model.edges[k]);
    vyt!(
        "VYT EL_PAIR e2={}({},{})-({},{})d{} f={}{}",
        net_name(model, b.pin, nets), b.low.0, b.low.1, b.high.0, b.high.1, b.dir() as i32, a.fixed as i32, b.fixed as i32
    );
    if a.pin == b.pin {
        vyt!("VYT EL_PAIR skip samepin");
        return;
    }
    if a.fixed && b.fixed {
        vyt!("VYT EL_PAIR skip fixed");
        return;
    }
    if a.dir() as i32 + b.dir() as i32 != 7 {
        vyt!("VYT EL_PAIR skip notopposite");
        return;
    }
    let trigger = get_parallel_edge_rect(model, k);
    if !overlaps(query, &trigger) {
        vyt!("VYT EL_PAIR skip noarea");
        return;
    }
    if !has_route && !b.fixed {
        let region = [query[0].max(trigger[0]), query[1].max(trigger[1]), query[2].min(trigger[2]), query[3].min(trigger[3])];
        has_route = has_route_in(model, model.net_of(b.pin), &region);
    }
    if !has_route {
        vyt!("VYT EL_PAIR skip noroute");
        return;
    }
    if !check_metal_end_of_line_eol_has_eol_end_to_end_helper(model, e, k, con) {
        vyt!("VYT EL_PAIR skip e2e");
        return;
    }
    check_metal_end_of_line_eol_has_eol_helper(model, e, k, ci, out);
}

/// `gtl::bloat(rect, orient, amount)`: grown by `amount` on both sides along the edge's own
/// orientation.
fn bloat_along(r: &Rect, horizontal: bool, amount: i32) -> Rect {
    if horizontal {
        [r[0] - amount, r[1], r[2] + amount, r[3]]
    } else {
        [r[0], r[1] - amount, r[2], r[3] + amount]
    }
}

/// drt `checkMetalEndOfLine_eol_hasEol_endToEndHelper`: whether the two edges are closer than the
/// governing space with a positive parallel run (the end grown by the within, or both ends by
/// the ENDTOEND extension when the other edge is an end too).
fn check_metal_end_of_line_eol_has_eol_end_to_end_helper(model: &LayerModel, e1: usize, e2: usize, con: &EolSpacingDbu) -> bool {
    let (a, b) = (&model.edges[e1], &model.edges[e2]);
    let mut space = con.eol_space;
    let mut rect1 = a.rect();
    let mut rect2 = b.rect();
    let a_h = a.low.1 == a.high.1;
    let b_h = b.low.1 == b.high.1;
    let edge2_is_eol = check_metal_end_of_line_eol_is_eol_edge(model, e2, con);
    match con.end_to_end {
        Some(ee) if edge2_is_eol => {
            space = ee.space;
            if let Some(x) = ee.extension {
                rect1 = bloat_along(&rect1, a_h, x);
                rect2 = bloat_along(&rect2, b_h, x);
            }
        }
        _ => rect1 = bloat_along(&rect1, a_h, con.eol_within()),
    }
    let m = generalized_intersect(&rect1, &rect2);
    let (mut prl_x, mut prl_y) = (m[2] - m[0], m[3] - m[1]);
    let (dist_x, dist_y) = (axis_distance(&rect1, &rect2, true), axis_distance(&rect1, &rect2, false));
    let res = if dist_x == 0 && dist_y == 0 {
        false
    } else {
        if dist_x != 0 {
            prl_x = -prl_x;
        }
        if dist_y != 0 {
            prl_y = -prl_y;
        }
        prl_x.max(prl_y) > 0 && dist_x.max(dist_y) < space
    };
    vyt!(
        "VYT EL_E2E eol2={} space={} r1={} r2={} prl=({},{}) dist=({},{}) res={}",
        edge2_is_eol as i32, space, r4(&rect1), r4(&rect2), prl_x, prl_y, dist_x, dist_y, res as i32
    );
    res
}

/// drt `checkMetalEndOfLine_eol_hasEol_helper`: the marker between the two edges, unless a
/// maximal rectangle fills part of it.
fn check_metal_end_of_line_eol_has_eol_helper(model: &LayerModel, e1: usize, e2: usize, ci: usize, out: &mut BTreeSet<Marker>) {
    let (a, b) = (&model.edges[e1], &model.edges[e2]);
    let marker = generalized_intersect(&get_parallel_edge_rect(model, e1), &get_parallel_edge_rect(model, e2));
    let mut bloated = marker;
    if (marker[2] - marker[0]) as i64 * (marker[3] - marker[1]) as i64 == 0 {
        bloated = bloat_along(&marker, matches!(a.dir(), Dir::W | Dir::E), 1);
    }
    let blocked = query_max_rectangle(model, &bloated).into_iter().any(|r| overlaps(&bloated, &model.rects[r].rect));
    vyt!("VYT EL_HELP marker={} blocked={}", r4(&marker), blocked as i32);
    if blocked {
        return;
    }
    vyt!("VYT EL_MARK");
    let nets: BTreeSet<usize> = [a.pin, b.pin].iter().map(|&q| model.net_of(q)).collect();
    out.insert(Marker { rule: RULE_EOL_SPACING, index: ci, bbox: marker, nets });
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
        let t = EolExtDbu { spacing: 80, parallel_only: false, rows: vec![40, 60], exts: vec![50, 30], min_length: vec![None, None], unmodelled: false };
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

    /// The reference's MINLENGTH test (`checkMetalEndOfLine_eol_hasMinMaxLength`, MINLENGTH branch):
    /// one side edge at least `l` is enough; with TWOSIDES both must be. Both comparisons closed.
    #[test]
    fn min_length_is_one_side_or_both_and_closed() {
        assert!(has_min_length(100, 50, 100, false));
        assert!(has_min_length(50, 100, 100, false));
        assert!(!has_min_length(99, 99, 100, false));
        assert!(has_min_length(100, 100, 100, true));
        assert!(!has_min_length(100, 99, 100, true));
        assert!(!has_min_length(99, 100, 100, true));
        // MAXLENGTH, closed: either side at most the length, or both with TWOSIDES
        assert!(has_max_length(100, 150, 100, false));
        assert!(has_max_length(150, 100, 100, false));
        assert!(!has_max_length(101, 101, 100, false));
        assert!(has_max_length(100, 100, 100, true));
        assert!(!has_max_length(100, 101, 100, true));
        assert!(!has_max_length(101, 100, 100, true));
    }

    /// Each row carries its own MINLENGTH; an end takes the row its length selects, as it takes
    /// the extension (upper-bound lookup): a 50 end in [40, 60) takes the second row's.
    #[test]
    fn min_length_follows_the_extension_row() {
        let t = EolExtDbu {
            spacing: 80,
            parallel_only: false,
            rows: vec![40, 60],
            exts: vec![50, 30],
            min_length: vec![Some((10, false)), Some((20, true))],
            unmodelled: false,
        };
        assert_eq!(t.min_length_for(30), Some((10, false)));
        assert_eq!(t.min_length_for(50), Some((20, true)));
        assert_eq!(t.find(50), 30);
        let none = EolExtDbu { min_length: vec![None, Some((20, true))], ..t };
        assert_eq!(none.min_length_for(30), None);
    }

    /// A SAMEMASK end-of-line rule in DBU for the unit tests: SPACING 0.07 ENDOFLINE 0.06, read as
    /// the reference reads it, no MINLENGTH.
    fn eol_con(end_to_end: Option<EolEndToEnd>) -> EolSpacingDbu {
        EolSpacingDbu {
            eol_space: 70,
            eol_width: 60,
            within: 20,
            end_to_end,
            min_max: None,
            within_policy: EolWithin::Reference,
            two_sides_policy: EolTwoSides::Reference,
        }
    }

    /// Every end-of-line marker of the shapes `(net, rect)`, as `(bbox, nets)`.
    fn eol_markers(shapes: &[(usize, Rect)], con: &EolSpacingDbu) -> Vec<(Rect, Vec<usize>)> {
        use crate::masks_geom::Shape;
        let v: Vec<Shape> = shapes.iter().map(|&(net, rect)| Shape { net, rect, fixed: false }).collect();
        let m = LayerModel::build(&v);
        let nets: Vec<String> = (0..4).map(|k| format!("n{k}")).collect();
        let mut out = BTreeSet::new();
        for e in 0..m.edges.len() {
            check_metal_end_of_line_eol(&m, e, con, 0, &nets, &mut out);
        }
        out.into_iter().map(|mk| (mk.bbox, mk.nets.into_iter().collect())).collect()
    }

    /// drt `isEolEdge`: shorter than the EOL width (strict) with BOTH corners convex. The short
    /// edge of a step whose previous corner is concave is no end.
    #[test]
    fn an_end_is_short_with_both_corners_convex() {
        use crate::masks_geom::Shape;
        let con = eol_con(None);
        let top = |w: i32| {
            let m = LayerModel::build(&[Shape { net: 0, rect: [0, 0, w, 500], fixed: false }]);
            let e = (0..m.edges.len()).find(|&k| m.edges[k].low.1 == 500 && m.edges[k].high.1 == 500).expect("a top edge");
            check_metal_end_of_line_eol_is_eol_edge(&m, e, &con)
        };
        assert!(top(59));
        assert!(!top(60), "60 is not under 60");
        // (50, 150) S to (50, 100) then W: concave; W then S at (0, 100): convex
        let m = LayerModel::build(&[
            Shape { net: 0, rect: [0, 0, 100, 100], fixed: false },
            Shape { net: 0, rect: [50, 100, 100, 150], fixed: false },
        ]);
        let e = (0..m.edges.len()).find(|&k| m.edges[k].low == (50, 100) && m.edges[k].high == (0, 100)).expect("the step");
        assert!(!check_metal_end_of_line_eol_is_eol_edge(&m, e, &con));
    }

    /// drt `hasEol_getQueryBox`: ENDTOEND raises the query's space to its own only when larger
    /// (`max`); a wide neighbour 0.06 from an end is in reach of SPACING 0.07 under ENDTOEND 0.05.
    #[test]
    fn end_to_end_never_shrinks_the_query() {
        let con = eol_con(Some(EolEndToEnd { space: 50, extension: None }));
        let got = eol_markers(&[(0, [0, 0, 50, 1000]), (1, [-200, 1060, 300, 1200])], &con);
        assert_eq!(got, [([0, 1000, 50, 1060], vec![0, 1])]);
    }

    /// drt `endToEndHelper`: the space is strict (`dist < space`) and the run must be positive.
    /// Under ENDTOEND 0.08 the query reaches 0.08, but a non-end neighbour is held to SPACING
    /// 0.07: 0.07 away is clean, 0.069 fires. With ENDTOEND EXTENSION the query reaches 0.02 past
    /// the end but the end is not grown for a non-end neighbour, so a wide bar starting exactly
    /// at the end's side has run 0: clean.
    #[test]
    fn the_space_is_strict_and_the_run_positive() {
        let con = eol_con(Some(EolEndToEnd { space: 80, extension: None }));
        assert!(eol_markers(&[(0, [0, 0, 50, 1000]), (1, [-200, 1070, 300, 1200])], &con).is_empty());
        assert_eq!(eol_markers(&[(0, [0, 0, 50, 1000]), (1, [-200, 1069, 300, 1200])], &con).len(), 1);
        let con = eol_con(Some(EolEndToEnd { space: 80, extension: Some(10) }));
        assert!(eol_markers(&[(0, [0, 0, 50, 1000]), (1, [50, 1040, 500, 1100])], &con).is_empty());
        assert_eq!(eol_markers(&[(0, [0, 0, 50, 1000]), (1, [49, 1040, 500, 1100])], &con).len(), 1);
    }

    /// drt `hasEol_check`, in the reference's order: two fixed edges are skipped (`skip fixed`)
    /// before anything else is looked at; an end of a fixed shape is reported only when the other
    /// net has a ROUTE shape where the other edge meets the query (`hasRoute`), here not: the
    /// other edge runs along a fixed pin and its route wire starts past the query. A fixed end
    /// takes no route from its own net (`if (!edge->isFixed())`).
    #[test]
    fn fixed_ends_need_a_route_on_the_other_side() {
        use crate::masks_geom::Shape;
        let con = eol_con(None);
        let nets = vec!["p".to_string(), "q".to_string()];
        let run = |shapes: &[Shape]| {
            let m = LayerModel::build(shapes);
            capture_trace(|| {
                let mut out = BTreeSet::new();
                for e in 0..m.edges.len() {
                    check_metal_end_of_line_eol(&m, e, &con, 0, &nets, &mut out);
                }
                out.len()
            })
        };
        let end = Shape { net: 0, rect: [0, 0, 50, 1000], fixed: true };
        let (n, lines) = run(&[end, Shape { net: 1, rect: [-200, 1060, 300, 1200], fixed: true }]);
        assert_eq!(n, 0);
        assert!(lines.iter().any(|l| l == "VYT EL_PAIR skip fixed"), "{lines:?}");
        let pin = Shape { net: 1, rect: [0, 1060, 50, 1200], fixed: true };
        let (n, lines) = run(&[end, pin, Shape { net: 1, rect: [50, 1060, 500, 1100], fixed: false }]);
        assert_eq!(n, 0);
        assert!(lines.iter().any(|l| l == "VYT EL_PAIR skip noroute"), "{lines:?}");
        let (n, _) = run(&[end, pin, Shape { net: 1, rect: [40, 1060, 500, 1100], fixed: false }]);
        assert_eq!(n, 1, "the route wire reaches into the query");
        // a route wire of p's own net INSIDE the fixed end does not count: a fixed end never
        // looks for route on its own side
        let inner = Shape { net: 0, rect: [0, 900, 50, 1000], fixed: false };
        let (n, _) = run(&[end, inner, pin, Shape { net: 1, rect: [50, 1060, 500, 1100], fixed: false }]);
        assert_eq!(n, 0);
    }

    /// drt `hasEol_helper`: a marker of no area is grown by 1 before the max-rectangle test, so a
    /// shape sitting on its line blocks it; without that shape the two ends are reported.
    #[test]
    fn a_line_marker_is_grown_before_the_blocking_test() {
        let con = eol_con(Some(EolEndToEnd { space: 80, extension: Some(10) }));
        let ends = [(0, [0, 0, 50, 1000]), (1, [50, 1040, 100, 2000])];
        assert_eq!(eol_markers(&ends, &con), [([50, 1000, 50, 1040], vec![0, 1])]);
        // the same through the layer's shape-rule pass
        let rules = SameMaskRulesDbu { eol_spacing: vec![con], ..SameMaskRulesDbu::default() };
        assert!(rules.has_shape_rules());
        let m = LayerModel::build(&ends.map(|(net, rect)| crate::masks_geom::Shape { net, rect, fixed: false }));
        let nets = vec!["n0".to_string(), "n1".to_string()];
        let got: Vec<Rect> = check_shape_rules(&m, false, &rules, &nets).iter().map(|mk| mk.bbox).collect();
        assert_eq!(got, [[50, 1000, 50, 1040]]);
        let mut blocked = ends.to_vec();
        blocked.push((2, [50, 1010, 80, 1030]));
        assert!(!eol_markers(&blocked, &con).iter().any(|(_, n)| *n == [0, 1]));
    }

    #[test]
    fn widths_and_runs() {
        assert_eq!(width(&[0, 0, 1000, 50]), 50);
        assert_eq!(prl(&[0, 0, 1000, 50], &[200, 100, 700, 150]), 500);
        assert_eq!(prl(&[0, 0, 100, 50], &[150, 100, 250, 150]), -50);
    }
}
