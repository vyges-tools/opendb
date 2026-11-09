// SPDX-License-Identifier: Apache-2.0
//! LEF58 joint corner spacing, same-mask form (`PROPERTY LEF58_JOINTCORNERSPACING
//! "JOINTCORNERSPACING s SAMEMASK JOINTWIDTH jw [MINLENGTH ml] JOINTLENGTH sl [EDGELENGTH el] ;"`),
//! checked on one mask's shapes as a layer of their own (the colour step's pre-screen, see
//! [`crate::masks_check`]). It reports; it never moves a shape.
//!
//! 🔑 No reference implements this rule: odb (pin 4155ac31) builds nothing from the property (an
//! INFO "unsupported property") and keeps its raw text as a layer string property, which is where
//! it is read from ([`crate::Db::layer_string_properties`]); the reference detailed router never
//! sees it. The semantics below are the LEF/DEF 5.8 language reference's (Layer (Routing), "Joint
//! Corner Spacing Rule", and the joint vocabulary of the "Opposite EOL Spacing Rule"), with the
//! choices the text leaves open stated and pinned in the unit tests. No oracle confirms them.
//!
//! Reference text, paraphrased:
//! * the rule is the spacing between two FACING joints of joint corners whose parallel run length
//!   is at most 0;
//! * a joint corner is a CONVEX corner made of two consecutive joints;
//! * a joint is a wire end that is not an end-of-line: an edge (or part of one) whose width is
//!   less than `jw` and whose span is more than `sl`, and which is not an EOL edge; an edge is an
//!   EOL edge (an "end") only when its end-of-line length is at least `ml` along BOTH sides
//!   (with MINLENGTH), so an end shorter than `ml` along a side may be a joint;
//! * EDGELENGTH: the rule applies only if the edge length of BOTH joints is at most `el`;
//! * SAMEMASK: only objects on the same mask.
//!
//! Measurement (interpretation; the text leaves these open):
//! * Shapes are merged per net (as the reference checker's polygons are); a joint is read off the
//!   polygon's maximal rectangles at a convex corner: on each of the corner's two edges, the
//!   joint is a maximal rectangle with that corner as one of its own whose extent ALONG the edge
//!   (the joint's width, and its "edge length") is `< jw` and `<= el`, and whose extent ACROSS
//!   the edge (its span) is `> sl`. This is the reference figure: an L's outer corner, where the
//!   vertical wire ends on the top edge and the horizontal wire ends on the side edge.
//! * The edge carrying a joint must not be an EOL edge: shorter than `jw`, both corners convex,
//!   and (with MINLENGTH) both side edges at least `ml`. A plain wire end is therefore never a
//!   joint, and its corners never joint corners.
//! * Facing: each corner lies in the other's outward quadrant (closed), so the joints' parallel
//!   run length is at most 0 by construction. Spacing is the Euclidean distance between the two
//!   corner points, a violation when STRICTLY less than `s` (as LEF spacing rules read). Any two
//!   joint corners on the mask count, of one net or two.

use std::collections::BTreeSet;

use crate::masks_check::Marker;
use crate::masks_geom::{turns_left, LayerModel, Point};

/// The layer property this module reads.
pub const PROPERTY: &str = "LEF58_JOINTCORNERSPACING";
/// The rule name in markers and findings.
pub const RULE_JOINT_CORNER: &str = "JOINTCORNERSPACING SAMEMASK";
/// A statement without SAMEMASK: a mask-blind rule, not a same-mask one; counted, not checked.
pub const NOT_CHECKED_NO_SAMEMASK: &str = "JOINTCORNERSPACING without SAMEMASK";
/// A statement that does not follow the reference grammar.
pub const NOT_CHECKED_MALFORMED: &str = "JOINTCORNERSPACING (does not parse)";

/// One statement, microns as written.
#[derive(Debug, Clone, PartialEq)]
pub struct JointCorner {
    pub spacing: f64,
    pub same_mask: bool,
    pub joint_width: f64,
    pub min_length: Option<f64>,
    pub span_length: f64,
    pub edge_length: Option<f64>,
}

fn num(t: Option<&&str>) -> Option<f64> {
    t.and_then(|s| s.parse::<f64>().ok()).filter(|v| v.is_finite() && *v >= 0.0)
}

/// `JOINTCORNERSPACING s [SAMEMASK] JOINTWIDTH jw [MINLENGTH ml] JOINTLENGTH sl [EDGELENGTH el]`,
/// in that order (the reference grammar); `None` for anything else.
fn read_statement(t: &[&str]) -> Option<JointCorner> {
    let up = |i: usize| t.get(i).map(|s| s.to_ascii_uppercase());
    if up(0).as_deref() != Some("JOINTCORNERSPACING") {
        return None;
    }
    let spacing = num(t.get(1))?;
    let mut i = 2;
    let same_mask = up(i).as_deref() == Some("SAMEMASK");
    if same_mask {
        i += 1;
    }
    if up(i).as_deref() != Some("JOINTWIDTH") {
        return None;
    }
    let joint_width = num(t.get(i + 1))?;
    i += 2;
    let mut min_length = None;
    if up(i).as_deref() == Some("MINLENGTH") {
        min_length = Some(num(t.get(i + 1))?);
        i += 2;
    }
    if up(i).as_deref() != Some("JOINTLENGTH") {
        return None;
    }
    let span_length = num(t.get(i + 1))?;
    i += 2;
    let mut edge_length = None;
    if up(i).as_deref() == Some("EDGELENGTH") {
        edge_length = Some(num(t.get(i + 1))?);
        i += 2;
    }
    (i == t.len()).then_some(JointCorner { spacing, same_mask, joint_width, min_length, span_length, edge_length })
}

/// Read one property value (several `;`-terminated statements): the statements read, and the
/// number that do not parse.
#[must_use]
pub fn read_property(value: &str) -> (Vec<JointCorner>, usize) {
    let (mut out, mut bad) = (Vec::new(), 0);
    for stmt in value.split(';') {
        let toks: Vec<&str> = stmt.split_whitespace().collect();
        if toks.is_empty() {
            continue;
        }
        match read_statement(&toks) {
            Some(r) => out.push(r),
            None => bad += 1,
        }
    }
    (out, bad)
}

/// A same-mask statement in DBU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JointCornerDbu {
    pub spacing: i32,
    pub joint_width: i32,
    pub min_length: Option<i32>,
    pub span_length: i32,
    pub edge_length: Option<i32>,
}

impl JointCornerDbu {
    #[must_use]
    pub fn from_um(r: &JointCorner, per_um: f64) -> Self {
        let d = |um: f64| (um * per_um).round() as i32;
        JointCornerDbu {
            spacing: d(r.spacing),
            joint_width: d(r.joint_width),
            min_length: r.min_length.map(d),
            span_length: d(r.span_length),
            edge_length: r.edge_length.map(d),
        }
    }
}

/// A layer's properties read: the same-mask statements in DBU and the not-checked counts by
/// reason.
#[must_use]
pub fn rules_from_properties(props: &[(String, String)], per_um: f64) -> (Vec<JointCornerDbu>, Vec<(&'static str, usize)>) {
    let (mut out, mut no_mask, mut bad) = (Vec::new(), 0, 0);
    for (_, v) in props.iter().filter(|(k, _)| k.eq_ignore_ascii_case(PROPERTY)) {
        let (rules, b) = read_property(v);
        bad += b;
        for r in rules {
            if r.same_mask {
                out.push(JointCornerDbu::from_um(&r, per_um));
            } else {
                no_mask += 1;
            }
        }
    }
    let nc = [(NOT_CHECKED_NO_SAMEMASK, no_mask), (NOT_CHECKED_MALFORMED, bad)].into_iter().filter(|x| x.1 > 0).collect();
    (out, nc)
}

/// An EOL edge in the rule's sense: shorter than `jw`, both corners convex and, with MINLENGTH,
/// both side edges at least `ml` long (closed). Such an edge is an end, never a joint.
#[must_use]
pub fn is_eol_edge(model: &LayerModel, e: usize, con: &JointCornerDbu) -> bool {
    let ed = &model.edges[e];
    let (p, n) = (&model.edges[ed.prev], &model.edges[ed.next]);
    if ed.len() >= con.joint_width || !turns_left(p, ed) || !turns_left(ed, n) {
        return false;
    }
    con.min_length.is_none_or(|ml| p.len() >= ml && n.len() >= ml)
}

/// The extent of a joint along and across a horizontal (`along_x`) or vertical edge: a joint
/// when along `< jw` (and `<= el`) and across `> sl`.
#[must_use]
pub fn is_joint_extent(along: i32, across: i32, con: &JointCornerDbu) -> bool {
    along < con.joint_width && con.edge_length.is_none_or(|el| along <= el) && across > con.span_length
}

/// Whether edge `e` carries a joint at its corner `v`: some maximal rectangle of the same pin
/// has `v` as a corner and the joint's extent on that edge; and the edge is not an EOL edge.
fn has_joint_at(model: &LayerModel, e: usize, v: Point, con: &JointCornerDbu) -> bool {
    let ed = &model.edges[e];
    if is_eol_edge(model, e, con) {
        return false;
    }
    let horizontal = ed.low.1 == ed.high.1;
    model.rects.iter().filter(|r| r.pin == ed.pin).any(|r| {
        let q = r.rect;
        let corner = (v.0 == q[0] || v.0 == q[2]) && (v.1 == q[1] || v.1 == q[3]);
        let (w, h) = (q[2] - q[0], q[3] - q[1]);
        corner && if horizontal { is_joint_extent(w, h, con) } else { is_joint_extent(h, w, con) }
    })
}

/// A joint corner: its point, the outward quadrant `(sx, sy)` (each ±1) and its pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JointCornerAt {
    pub at: Point,
    pub quadrant: (i32, i32),
    pub pin: usize,
}

/// The outward normal of an edge (the interior is on its left).
fn outward(model: &LayerModel, e: usize) -> (i32, i32) {
    let ed = &model.edges[e];
    let (dx, dy) = ((ed.high.0 - ed.low.0).signum(), (ed.high.1 - ed.low.1).signum());
    (dy, -dx)
}

/// Every joint corner of the model: a convex vertex (the turn from the edge ending there into
/// the edge starting there is to the left) with a joint on both edges.
#[must_use]
pub fn joint_corners(model: &LayerModel, con: &JointCornerDbu) -> Vec<JointCornerAt> {
    let mut out = Vec::new();
    for (e, ed) in model.edges.iter().enumerate() {
        let v = ed.low;
        if !turns_left(&model.edges[ed.prev], ed) {
            continue;
        }
        if has_joint_at(model, ed.prev, v, con) && has_joint_at(model, e, v, con) {
            let (a, b) = (outward(model, ed.prev), outward(model, e));
            out.push(JointCornerAt { at: v, quadrant: (a.0 + b.0, a.1 + b.1), pin: ed.pin });
        }
    }
    out
}

/// `b` lies in `a`'s outward quadrant (closed).
#[must_use]
pub fn in_quadrant(a: &JointCornerAt, b: Point) -> bool {
    (b.0 - a.at.0) * a.quadrant.0 >= 0 && (b.1 - a.at.1) * a.quadrant.1 >= 0
}

/// The rule on one mask's model: every pair of facing joint corners closer than the spacing
/// (Euclidean, strict) is a marker, its box spanned by the two corners.
pub fn check_joint_corner_spacing(model: &LayerModel, con: &JointCornerDbu, ci: usize, out: &mut BTreeSet<Marker>) {
    let corners = joint_corners(model, con);
    let s2 = i64::from(con.spacing) * i64::from(con.spacing);
    for (i, a) in corners.iter().enumerate() {
        for b in &corners[i + 1..] {
            if !in_quadrant(a, b.at) || !in_quadrant(b, a.at) {
                continue;
            }
            let (dx, dy) = (i64::from(b.at.0 - a.at.0), i64::from(b.at.1 - a.at.1));
            if dx * dx + dy * dy >= s2 {
                continue;
            }
            let bbox = [a.at.0.min(b.at.0), a.at.1.min(b.at.1), a.at.0.max(b.at.0), a.at.1.max(b.at.1)];
            let nets: BTreeSet<usize> = [a.pin, b.pin].iter().map(|&p| model.net_of(p)).collect();
            out.insert(Marker { rule: RULE_JOINT_CORNER, index: ci, bbox, nets });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::masks_geom::Shape;

    /// The reference figure's rule, in DBU (1000 per micron): JOINTCORNERSPACING 0.05 JOINTWIDTH
    /// 0.07 JOINTLENGTH 0.06 EDGELENGTH 0.1.
    fn fig() -> JointCornerDbu {
        JointCornerDbu { spacing: 50, joint_width: 70, min_length: None, span_length: 60, edge_length: Some(100) }
    }

    fn model(rects: &[(usize, [i32; 4])]) -> LayerModel {
        LayerModel::build(&rects.iter().map(|&(net, rect)| Shape { net, rect, fixed: false }).collect::<Vec<_>>())
    }

    /// The figure's lower L: a 0.09 x 0.04 bar whose right end sits on a 0.04 wide, 0.08 tall leg
    /// (the leg's top flush with the bar's top), so its outer corner at (90, 80) points NE.
    fn lower_l(net: usize, dx: i32, dy: i32) -> Vec<(usize, [i32; 4])> {
        vec![(net, [dx, dy + 40, dx + 90, dy + 80]), (net, [dx + 50, dy, dx + 90, dy + 80])]
    }

    /// The figure's upper L, mirrored: its outer corner at (dx, dy) points SW.
    fn upper_l(net: usize, dx: i32, dy: i32) -> Vec<(usize, [i32; 4])> {
        vec![(net, [dx, dy, dx + 90, dy + 40]), (net, [dx, dy, dx + 40, dy + 80])]
    }

    #[test]
    fn statements_follow_the_reference_grammar() {
        let (r, bad) = read_property(
            "JOINTCORNERSPACING 0.05 SAMEMASK JOINTWIDTH 0.07 MINLENGTH 0.02 JOINTLENGTH 0.06 EDGELENGTH 0.1 ;
             JOINTCORNERSPACING 0.05 JOINTWIDTH 0.07 JOINTLENGTH 0.06 ;
             JOINTCORNERSPACING 0.05 JOINTLENGTH 0.06 JOINTWIDTH 0.07 ;
             JOINTCORNERSPACING 0.05 SAMEMASK JOINTWIDTH 0.07 JOINTLENGTH 0.06 EXTRA ;",
        );
        assert_eq!(bad, 2, "out of order, and a trailing token");
        assert_eq!(r.len(), 2);
        assert_eq!(
            r[0],
            JointCorner { spacing: 0.05, same_mask: true, joint_width: 0.07, min_length: Some(0.02), span_length: 0.06, edge_length: Some(0.1) }
        );
        assert!(!r[1].same_mask && r[1].min_length.is_none() && r[1].edge_length.is_none());
        let props = vec![
            (PROPERTY.to_string(), "JOINTCORNERSPACING 0.05 SAMEMASK JOINTWIDTH 0.07 JOINTLENGTH 0.06 ; JOINTCORNERSPACING 0.05 JOINTWIDTH 0.07 JOINTLENGTH 0.06 ; JUNK ;".to_string()),
            ("LEF58_OTHER".to_string(), "JOINTCORNERSPACING 0.05 SAMEMASK JOINTWIDTH 0.07 JOINTLENGTH 0.06 ;".to_string()),
        ];
        let (dbu, nc) = rules_from_properties(&props, 1000.0);
        assert_eq!(dbu, vec![JointCornerDbu { spacing: 50, joint_width: 70, min_length: None, span_length: 60, edge_length: None }]);
        assert_eq!(nc, vec![(NOT_CHECKED_NO_SAMEMASK, 1), (NOT_CHECKED_MALFORMED, 1)]);
    }

    /// Figure a): the two L's outer corners face each other with no common parallel run, 0.04
    /// apart diagonally (under 0.05): a violation. Moved to exactly 0.05 (30, 40): clean (strict).
    #[test]
    fn the_reference_figure_is_a_violation_and_strict_at_the_spacing() {
        let mut rs = lower_l(0, 0, 0);
        rs.extend(upper_l(1, 90 + 24, 80 + 32)); // 0.04 apart: (24, 32)
        let m = model(&rs);
        let jc = joint_corners(&m, &fig());
        assert_eq!(jc.len(), 2, "{jc:?}");
        assert!(jc.iter().any(|c| c.at == (90, 80) && c.quadrant == (1, 1)));
        assert!(jc.iter().any(|c| c.at == (114, 112) && c.quadrant == (-1, -1)));
        let mut out = BTreeSet::new();
        check_joint_corner_spacing(&m, &fig(), 0, &mut out);
        assert_eq!(out.len(), 1);
        let mk = out.iter().next().unwrap();
        assert_eq!((mk.bbox, mk.nets.len()), ([90, 80, 114, 112], 2));

        let mut rs = lower_l(0, 0, 0);
        rs.extend(upper_l(1, 90 + 30, 80 + 40)); // exactly 0.05
        let mut out = BTreeSet::new();
        check_joint_corner_spacing(&model(&rs), &fig(), 0, &mut out);
        assert!(out.is_empty(), "spacing equal to the rule is clean");
        let mut rs = lower_l(0, 0, 0);
        rs.extend(upper_l(1, 90 + 30, 80 + 39)); // just under
        let mut out = BTreeSet::new();
        check_joint_corner_spacing(&model(&rs), &fig(), 0, &mut out);
        assert_eq!(out.len(), 1);
    }

    /// Corners that do not face each other are not a pair: the upper L moved so that the lower
    /// corner is not in its outward quadrant (shifted left past it), and two corners pointing the
    /// same way.
    #[test]
    fn only_facing_corners_pair() {
        let mut rs = lower_l(0, 0, 0);
        rs.extend(upper_l(1, 80, 100)); // corner at (80, 100): left of (90, 80)
        let mut out = BTreeSet::new();
        check_joint_corner_spacing(&model(&rs), &fig(), 0, &mut out);
        assert!(out.is_empty(), "{out:?}");
        let mut rs = lower_l(0, 0, 0);
        rs.extend(lower_l(1, 100, 100)); // both NE
        let mut out = BTreeSet::new();
        check_joint_corner_spacing(&model(&rs), &fig(), 0, &mut out);
        assert!(out.is_empty(), "{out:?}");
    }

    /// The joint conditions, one at a time, on the lower L's outer corner: a leg no longer than
    /// JOINTLENGTH has no span; a leg as wide as JOINTWIDTH is not a joint; a joint longer than
    /// EDGELENGTH is not one; a plain wire end is an EOL edge, never a joint.
    #[test]
    fn joint_conditions() {
        let c = fig();
        assert!(is_joint_extent(40, 61, &c));
        assert!(!is_joint_extent(40, 60, &c), "span must exceed JOINTLENGTH");
        assert!(!is_joint_extent(70, 80, &c), "width must be under JOINTWIDTH");
        let tight = JointCornerDbu { edge_length: Some(39), ..c };
        assert!(!is_joint_extent(40, 80, &tight), "edge length must not exceed EDGELENGTH");
        assert!(is_joint_extent(40, 80, &JointCornerDbu { edge_length: Some(40), ..c }), "closed");
        assert!(is_joint_extent(40, 80, &JointCornerDbu { edge_length: None, ..c }));

        // a short leg (span 0.06): no joint on the top edge, so no joint corner
        let m = model(&[(0, [0, 40, 90, 80]), (0, [50, 20, 90, 80])]);
        assert!(joint_corners(&m, &c).is_empty());
        // a straight wire: its corners are EOL corners
        let m = model(&[(0, [0, 0, 40, 300])]);
        assert!(joint_corners(&m, &c).is_empty());
        let top = m.edges.iter().position(|e| e.low.1 == 300 && e.high.1 == 300).unwrap();
        assert!(is_eol_edge(&m, top, &c));
    }

    /// MINLENGTH: an end is an EOL edge only when BOTH side edges reach MINLENGTH; a stub end
    /// with a short side is not an end, so its corner can be a joint corner.
    #[test]
    fn min_length_decides_whether_an_end_is_an_end() {
        let c = JointCornerDbu { min_length: Some(30), ..fig() };
        // a 0.04 wide wire end with sides 100 and 20 long: one side under MINLENGTH
        let m = model(&[(0, [0, 0, 40, 100]), (0, [40, 0, 200, 80])]);
        let top = m.edges.iter().position(|e| e.low == (40, 100) && e.high == (0, 100)).unwrap();
        assert_eq!((m.edges[m.edges[top].prev].len(), m.edges[m.edges[top].next].len()), (20, 100));
        assert!(!is_eol_edge(&m, top, &c), "a side under MINLENGTH: not an end");
        assert!(is_eol_edge(&m, top, &fig()), "without MINLENGTH it is an end");
        let c2 = JointCornerDbu { min_length: Some(20), ..fig() };
        assert!(is_eol_edge(&m, top, &c2), "closed: a side equal to MINLENGTH counts");
    }

    /// Same net counts as well as two nets, and the marker names one net then.
    #[test]
    fn one_net_counts() {
        let mut rs = lower_l(0, 0, 0);
        rs.extend(upper_l(0, 90 + 24, 80 + 32));
        let mut out = BTreeSet::new();
        check_joint_corner_spacing(&model(&rs), &fig(), 3, &mut out);
        assert_eq!(out.len(), 1);
        let mk = out.iter().next().unwrap();
        assert_eq!((mk.nets.len(), mk.index, mk.rule), (1, 3, RULE_JOINT_CORNER));
    }

    /// The EOL edge test: shorter than JOINTWIDTH (strict) and BOTH corners convex. A 0.07 wide
    /// end is not an EOL edge under JOINTWIDTH 0.07; a short edge with one concave corner is not
    /// one either, whichever end the concave corner is at.
    #[test]
    fn an_eol_edge_is_short_with_both_corners_convex() {
        let c = fig();
        let m = model(&[(0, [0, 0, 70, 300])]);
        let top = m.edges.iter().position(|e| e.low.1 == 300 && e.high.1 == 300).unwrap();
        assert!(!is_eol_edge(&m, top, &c), "70 is not under 70");
        let m = model(&[(0, [0, 0, 69, 300])]);
        let top = m.edges.iter().position(|e| e.low.1 == 300 && e.high.1 == 300).unwrap();
        assert!(is_eol_edge(&m, top, &c));
        // a step: the edge at y = 50 runs west from a convex corner into a concave one, the edge
        // at x = 40 runs north from that concave corner to a convex one
        let m = model(&[(0, [0, 0, 100, 50]), (0, [0, 50, 40, 100])]);
        let step = m.edges.iter().position(|e| e.low == (100, 50) && e.high == (40, 50)).unwrap();
        let riser = m.edges.iter().position(|e| e.low == (40, 50) && e.high == (40, 100)).unwrap();
        assert!(!is_eol_edge(&m, step, &c), "next corner concave");
        assert!(!is_eol_edge(&m, riser, &c), "previous corner concave");
    }

    /// A short squarish piece (sides between JOINTLENGTH and JOINTWIDTH) has corners whose
    /// rectangles meet both joint extents, but its edges are EOL edges: no joint corner, so two
    /// such pieces facing each other closer than the spacing are clean.
    #[test]
    fn an_eol_edge_carries_no_joint() {
        let m = model(&[(0, [0, 0, 65, 65]), (1, [89, 97, 154, 162])]);
        assert!(joint_corners(&m, &fig()).is_empty());
        let mut out = BTreeSet::new();
        check_joint_corner_spacing(&m, &fig(), 0, &mut out);
        assert!(out.is_empty());
    }

    /// A joint is read off a rectangle with the corner as one of its OWN corners: a qualifying
    /// rectangle elsewhere in the polygon does not make a joint here. The short-leg L (no joint on
    /// the top edge) with a tall stub on the bar's far end still has no joint corner at (90, 80).
    ///
    /// Swapping which extent is "along" for horizontal and vertical edges is an equivalent change:
    /// a convex corner always has one edge of each, so the swap only exchanges which rectangle
    /// serves which edge (a mutant doing so survives as equivalent).
    #[test]
    fn a_joint_belongs_to_the_corner() {
        let m = model(&[(0, [0, 40, 90, 80]), (0, [50, 20, 90, 80]), (0, [0, 80, 40, 200])]);
        assert!(m.rects.iter().any(|r| r.rect == [0, 40, 40, 200]), "{:?}", m.rects);
        assert!(!joint_corners(&m, &fig()).iter().any(|c| c.at == (90, 80)));
        // rectangles of OTHER pins with that corner give a wide block no joints: a 0.09 x 0.08
        // block whose corner (90, 80) touches a tall net-1 bar and a long net-2 bar
        let m = model(&[(0, [0, 0, 90, 80]), (1, [90, 80, 130, 300]), (2, [90, 80, 300, 120])]);
        assert!(!joint_corners(&m, &fig()).iter().any(|c| c.at == (90, 80) && m.pins[c.pin].net == 0));
    }

    /// Both edges must carry a joint: a 0.08 thick bar (no joint along its top) on a narrow leg
    /// (a joint along its top edge), its corner (200, 80) facing the figure's upper L 0.04 away,
    /// is clean.
    #[test]
    fn one_joint_is_not_a_joint_corner() {
        let mut rs = vec![(0, [0, 0, 200, 80]), (0, [160, -100, 200, 80])];
        rs.extend(upper_l(1, 200 + 24, 80 + 32));
        let mut out = BTreeSet::new();
        check_joint_corner_spacing(&model(&rs), &fig(), 0, &mut out);
        assert!(out.is_empty(), "{out:?}");
    }

    /// Only a CONVEX vertex is a joint corner. A hand-built L whose concave vertex (40, 40) is
    /// given rectangles meeting both joint extents there is still no joint corner.
    #[test]
    fn a_concave_vertex_is_never_a_joint_corner() {
        use crate::masks_geom::{Edge, MaxRect, Pin};
        let pts = [(0, 0), (200, 0), (200, 40), (40, 40), (40, 200), (0, 200)];
        let n = pts.len();
        let edges = (0..n)
            .map(|k| Edge { low: pts[k], high: pts[(k + 1) % n], fixed: false, pin: 0, prev: (k + n - 1) % n, next: (k + 1) % n })
            .collect();
        let rects = vec![
            MaxRect { rect: [40, 40, 80, 250], fixed: false, pin: 0 },
            MaxRect { rect: [40, 40, 250, 80], fixed: false, pin: 0 },
        ];
        let m = LayerModel { pins: vec![Pin { net: 0 }], edges, rects, ..LayerModel::default() };
        assert!(!turns_left(&m.edges[2], &m.edges[3]), "(40, 40) is concave");
        assert!(joint_corners(&m, &fig()).is_empty());
    }

    /// Facing is mutual: two corners pointing the same way, 0.036 apart, are no pair, whichever
    /// comes first. Facing corners aligned on an axis (parallel run exactly 0) are a pair: the
    /// quadrant is closed.
    #[test]
    fn facing_is_mutual_and_closed() {
        for (first, second) in [((0, 0), (30, 30)), ((30, 30), (0, 0))] {
            let mut rs = lower_l(0, first.0, first.1);
            rs.extend(lower_l(1, second.0, second.1));
            let mut out = BTreeSet::new();
            check_joint_corner_spacing(&model(&rs), &fig(), 0, &mut out);
            assert!(out.is_empty(), "{out:?}");
        }
        let mut rs = upper_l(0, 200, 200);
        rs.extend(upper_l(1, 170, 170));
        let mut out = BTreeSet::new();
        check_joint_corner_spacing(&model(&rs), &fig(), 0, &mut out);
        assert!(out.is_empty(), "{out:?}");
        let mut rs = lower_l(0, 0, 0);
        rs.extend(upper_l(1, 90, 120));
        let mut out = BTreeSet::new();
        check_joint_corner_spacing(&model(&rs), &fig(), 0, &mut out);
        assert_eq!(out.iter().map(|m| m.bbox).collect::<Vec<_>>(), [[90, 80, 90, 120]]);
    }
}
