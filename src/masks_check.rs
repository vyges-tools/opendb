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
//! naming the rule. `FORBIDDENSPACING … SAMEMASK` and `EOLEXTENSIONSPACING … SAMEMASK` are NOT
//! checked here: they are counted per layer ([`SameMaskRulesDbu::not_checked`]) so a report can
//! never read as clean for a rule nobody looked at.
//!
//! Gaps are edge to edge, Euclidean (a corner pair at its corner distance); width is a shape's
//! smaller side.

use vyges_loom::lef58_samemask::{SameMaskRules, TwoWidthsTable};

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
        if !r.forbidden.is_empty() {
            not_checked.push(("FORBIDDENSPACING SAMEMASK", r.forbidden.len()));
        }
        if !r.eol_extension.is_empty() {
            not_checked.push(("EOLEXTENSIONSPACING SAMEMASK", r.eol_extension.len()));
        }
        SameMaskRulesDbu {
            spacing: r.spacing.iter().map(|&s| dbu(s, per_um)).max(),
            two_widths: r.two_widths.iter().filter_map(|t| TwoWidthsDbu::from_um(t, per_um)).collect(),
            not_checked,
        }
    }

    /// True when there is a rule this module checks.
    #[must_use]
    pub fn has_checked_rules(&self) -> bool {
        self.spacing.is_some() || !self.two_widths.is_empty()
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
        assert_eq!(r.not_checked, vec![("FORBIDDENSPACING SAMEMASK", 1)]);
        let thin = [0, 0, 1000, 50];
        let thin2 = [0, 150, 1000, 200];
        assert_eq!(r.required(&thin, &thin2), Some((80, RULE_SPACING)));
        let wide = [0, 300, 1000, 500];
        let wide2 = [0, 650, 1000, 850];
        assert_eq!(r.required(&wide, &wide2), Some((150, RULE_TWO_WIDTHS)));
        assert_eq!(r.required(&thin, &wide), Some((100, RULE_TWO_WIDTHS)));
    }

    #[test]
    fn widths_and_runs() {
        assert_eq!(width(&[0, 0, 1000, 50]), 50);
        assert_eq!(prl(&[0, 0, 1000, 50], &[200, 100, 700, 150]), 500);
        assert_eq!(prl(&[0, 0, 100, 50], &[150, 100, 250, 150]), -50);
    }
}
