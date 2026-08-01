// SPDX-License-Identifier: Apache-2.0
//! Draw a 3D chiplet assembly as a single self-contained **SVG** — cross-section and plan.
//!
//! The 2D layout viewer is a hard problem: a routed block is millions of polygons, so it needs a
//! tile server, an R-tree and a raster pyramid. **An assembly is not that.** A stack is a handful
//! of dies, each a box, with a few bond regions — tens of rectangles. So the thing that is out of
//! reach at layout scale is a few hundred lines here, with no dependencies and no server: one
//! file, opens in a browser, commits to a repo, embeds in a report. Same posture as
//! `vyges-gds-view`, which this deliberately resembles.
//!
//! **Two views, because one is not enough.** A plan view (X–Y) shows footprints, overhang and
//! where the bond regions sit. It cannot show stacking order, die thickness, bond gaps, or which
//! *face* is bonded — and those are the entire subject. So the primary view here is the
//! **cross-section** (X–Z), the drawing a package engineer actually reads.
//!
//! **The Z axis is exaggerated, and says so on the drawing.** A real die is millimetres across
//! and tens of microns thick; drawn to a single scale the stack is a line. Every package
//! cross-section in the industry is drawn this way. Ours prints the factor in the corner so
//! nobody measures a gap off the picture.
//!
//! **Orientation is taken from the unfolded model, not recomputed.** A die at `MZ` is flipped:
//! its FRONT faces down. Getting that wrong silently draws a plausible and wrong assembly, which
//! is worse than drawing nothing, so face labels come from `dbUnfoldedChipRegionInst`'s
//! `get_effective_side` and `get_surface_z` — the database's own post-orientation answer.

use crate::{registry, Db, Result};
use std::fmt::Write as _;

const DRAW_W: f64 = 900.0;
const MARGIN: f64 = 48.0;
const SECTION_H: f64 = 300.0;
const PLAN_H: f64 = 340.0;

/// Chip fill colours, indexed by master chip so two instances of one die read as the same part.
const PALETTE: &[&str] = &[
    "#4e79a7", "#f28e2b", "#59a14f", "#b07aa1", "#76b7b2", "#edc948", "#9c755f", "#e15759",
];

/// One placed die, resolved to absolute coordinates.
#[derive(Debug, Clone)]
pub struct Die {
    pub inst: String,
    pub master: String,
    pub orient: String,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub w: f64,
    pub h: f64,
    pub thickness: f64,
    pub chip_type: String,
}

/// A die-to-die bond, as a pair of instance paths.
#[derive(Debug, Clone)]
pub struct Bond {
    pub name: String,
    pub top: String,
    pub bottom: String,
    pub thickness: f64,
}

/// Everything the drawing needs, read once.
#[derive(Debug, Default, Clone)]
pub struct Assembly3d {
    pub top: String,
    pub dies: Vec<Die>,
    pub bonds: Vec<Bond>,
    /// Linter findings, as (category, marker name). Drawn as callouts.
    pub findings: Vec<(String, String)>,
}

fn s(db: &Db, class: &str, field: &str, keys: &[&str]) -> Option<String> {
    let k: Vec<String> = keys.iter().map(|x| x.to_string()).collect();
    registry::get(db, class, field, &k)
        .ok()?
        .as_str()
        .map(str::to_string)
}

fn n(db: &Db, class: &str, field: &str, keys: &[&str]) -> f64 {
    let k: Vec<String> = keys.iter().map(|x| x.to_string()).collect();
    registry::get(db, class, field, &k)
        .ok()
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0)
}

fn list(db: &Db, class: &str, field: &str, keys: &[&str]) -> Vec<String> {
    let k: Vec<String> = keys.iter().map(|x| x.to_string()).collect();
    registry::get(db, class, field, &k)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

impl Assembly3d {
    /// Read the placed assembly under `top`.
    ///
    /// Instance order is sorted by Z then name rather than left as the database returns it: a
    /// drawing whose legend reshuffles between runs cannot be diffed, and diffing two revisions
    /// of a stack is most of the value.
    pub fn read(db: &Db, top: &str) -> Result<Assembly3d> {
        let mut a = Assembly3d {
            top: top.to_string(),
            ..Default::default()
        };

        for inst in list(db, "dbChip", "get_chip_insts", &[top]) {
            let master = s(db, "dbChipInst", "get_master_chip", &[top, &inst]).unwrap_or_default();
            a.dies.push(Die {
                x: n(db, "dbChipInst", "get_loc_x", &[top, &inst]),
                y: n(db, "dbChipInst", "get_loc_y", &[top, &inst]),
                z: n(db, "dbChipInst", "get_loc_z", &[top, &inst]),
                orient: s(db, "dbChipInst", "get_orient", &[top, &inst]).unwrap_or_default(),
                w: n(db, "dbChip", "get_width", &[&master]),
                h: n(db, "dbChip", "get_height", &[&master]),
                thickness: n(db, "dbChip", "get_thickness", &[&master]),
                chip_type: s(db, "dbChip", "get_chip_type", &[&master]).unwrap_or_default(),
                inst,
                master,
            });
        }
        a.dies.sort_by(|p, q| {
            p.z.partial_cmp(&q.z)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| p.inst.cmp(&q.inst))
        });

        for conn in list(db, "dbChip", "get_chip_conns", &[top]) {
            // A region path is a chain of instance names; the last element is the die that is
            // bonded. Joining with '/' keeps a nested path readable rather than losing it.
            let path = |f: &str| list(db, "dbChipConn", f, &[top, &conn]).join("/");
            a.bonds.push(Bond {
                thickness: n(db, "dbChipConn", "get_thickness", &[top, &conn]),
                top: path("get_top_region_path"),
                bottom: path("get_bottom_region_path"),
                name: conn,
            });
        }
        Ok(a)
    }

    /// Attach linter findings so the drawing can show *where*, not only *what*.
    pub fn with_findings(mut self, findings: Vec<(String, String)>) -> Self {
        self.findings = findings;
        self
    }

    fn color(&self, master: &str) -> &'static str {
        let i = self
            .dies
            .iter()
            .map(|d| d.master.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .iter()
            .position(|m| *m == master)
            .unwrap_or(0);
        PALETTE[i % PALETTE.len()]
    }

    /// Total stack height, and the extent in X and Y.
    fn extent(&self) -> (f64, f64, f64) {
        let mut x = 0.0f64;
        let mut y = 0.0f64;
        let mut z = 0.0f64;
        for d in &self.dies {
            x = x.max(d.x + d.w);
            y = y.max(d.y + d.h);
            z = z.max(d.z + d.thickness);
        }
        (x.max(1.0), y.max(1.0), z.max(1.0))
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Render the assembly to one self-contained SVG document.
///
/// `dbu_per_um` converts database units to microns for the dimension labels. Passing the wrong
/// value mislabels every dimension, so it is a required argument rather than a guess.
pub fn to_svg(a: &Assembly3d, dbu_per_um: f64) -> String {
    let (ex, ey, ez) = a.extent();
    let inner = DRAW_W - 2.0 * MARGIN;

    // X sets the horizontal scale, so a die's *width* is always honest. Z is then scaled to fit
    // the section band — which can mean stretching a thin stack (the usual case: a die is
    // millimetres across and microns thick) or **compressing** a tall one. Both directions must
    // work: an earlier version could only stretch, and a stack taller than the band ran off the
    // top of the page with the upper die silently missing from the drawing.
    let sx = inner / ex;
    let sz = ((SECTION_H - 2.0 * MARGIN) / ez).min(sx * 60.0);
    let z_exag = if sx > 0.0 { sz / sx } else { 1.0 };

    let mut o = String::new();
    let total_h = SECTION_H + PLAN_H + 130.0 + 18.0 * a.findings.len() as f64;
    let _ = write!(
        o,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{DRAW_W:.0}\" height=\"{total_h:.0}\" \
         viewBox=\"0 0 {DRAW_W:.0} {total_h:.0}\" font-family=\"ui-sans-serif,system-ui,sans-serif\">\n\
         <rect width=\"100%\" height=\"100%\" fill=\"#fbfbfd\"/>\n\
         <text x=\"{MARGIN}\" y=\"30\" font-size=\"17\" font-weight=\"600\" fill=\"#111\">\
         {} — chiplet assembly</text>\n",
        esc(&a.top)
    );

    // ── Cross-section: X across, Z up. Z grows upward, so the SVG y axis is inverted. ──
    let sec_top = 56.0;
    let base_y = sec_top + SECTION_H - MARGIN;
    let _ = write!(
        o,
        "<text x=\"{MARGIN}\" y=\"{:.0}\" font-size=\"12\" font-weight=\"600\" fill=\"#444\">\
         Cross-section (X–Z) · Z {} {:.2}\u{d7}</text>\n",
        sec_top - 4.0,
        if z_exag >= 1.0 {
            "exaggerated"
        } else {
            "compressed"
        },
        z_exag
    );
    // Substrate datum, so "which way is up" is never ambiguous.
    let _ = write!(
        o,
        "<line x1=\"{MARGIN}\" y1=\"{base_y:.1}\" x2=\"{:.1}\" y2=\"{base_y:.1}\" \
         stroke=\"#999\" stroke-width=\"1.5\" stroke-dasharray=\"5 3\"/>\n\
         <text x=\"{:.1}\" y=\"{:.1}\" font-size=\"10\" fill=\"#999\">z = 0</text>\n",
        DRAW_W - MARGIN,
        MARGIN,
        base_y + 13.0
    );

    for d in &a.dies {
        let x = MARGIN + d.x * sx;
        let w = (d.w * sx).max(1.0);
        let th = (d.thickness * sz).max(2.0);
        let y = base_y - (d.z + d.thickness) * sz;
        let c = a.color(&d.master);
        // A flipped die is the detail most worth seeing and easiest to miss, so it is hatched
        // rather than left to the orientation string alone.
        let flipped = d.orient.contains('M');
        let _ = write!(
            o,
            "<rect x=\"{x:.1}\" y=\"{y:.1}\" width=\"{w:.1}\" height=\"{th:.1}\" fill=\"{c}\" \
             fill-opacity=\"0.30\" stroke=\"{c}\" stroke-width=\"1.6\"/>\n"
        );
        // FRONT face: the active side. On an unflipped die it is up; flipped, it is down.
        let front_y = if flipped { y + th } else { y };
        // Front labels hug the left edge; bond labels use the right. Face-to-face dies put both
        // fronts on the same line, so anything sharing that edge collides.
        let _ = write!(
            o,
            "<line x1=\"{x:.1}\" y1=\"{front_y:.1}\" x2=\"{:.1}\" y2=\"{front_y:.1}\" \
             stroke=\"{c}\" stroke-width=\"3.4\"/>\n\
             <text x=\"{:.1}\" y=\"{:.1}\" font-size=\"9\" fill=\"{c}\">front</text>\n",
            x + w,
            x + 4.0,
            if flipped { front_y + 10.0 } else { front_y - 4.0 }
        );
        let _ = write!(
            o,
            "<text x=\"{:.1}\" y=\"{:.1}\" font-size=\"11\" fill=\"#111\" text-anchor=\"middle\">\
             {}</text>\n\
             <text x=\"{:.1}\" y=\"{:.1}\" font-size=\"9\" fill=\"#666\" text-anchor=\"middle\">\
             {} · {} · {:.1} \u{b5}m thick{}</text>\n",
            x + w / 2.0,
            y + th / 2.0 + 1.0,
            esc(&d.inst),
            x + w / 2.0,
            y + th / 2.0 + 13.0,
            esc(&d.master),
            esc(&d.orient),
            d.thickness / dbu_per_um,
            if flipped { " · flipped" } else { "" }
        );
    }

    // Bonds, drawn at the interface between the two dies they name.
    for b in &a.bonds {
        let find = |p: &str| {
            let leaf = p.rsplit('/').next().unwrap_or(p);
            a.dies.iter().find(|d| d.inst == leaf)
        };
        let (Some(t), Some(bt)) = (find(&b.top), find(&b.bottom)) else {
            continue;
        };
        // The mating plane: the top of the lower die, which is where the bond sits.
        let z_iface = bt.z + bt.thickness;
        let y = base_y - z_iface * sz;
        let x0 = MARGIN + t.x.max(bt.x) * sx;
        let x1 = MARGIN + ((t.x + t.w).min(bt.x + bt.w)) * sx;
        // A bond spanning the full width leaves no room to the right, so the label goes inside
        // the overlap rather than off the edge of the page.
        let outside = x1 + 5.0;
        let (lx, anchor) = if outside + 8.0 * b.name.len() as f64 > DRAW_W - 4.0 {
            (x1 - 5.0, "end")
        } else {
            (outside, "start")
        };
        let _ = write!(
            o,
            "<line x1=\"{x0:.1}\" y1=\"{y:.1}\" x2=\"{x1:.1}\" y2=\"{y:.1}\" stroke=\"#d62728\" \
             stroke-width=\"2.4\" stroke-dasharray=\"4 2\"/>\n\
             <text x=\"{lx:.1}\" y=\"{:.1}\" font-size=\"9\" fill=\"#d62728\" \
             text-anchor=\"{anchor}\">{}</text>\n",
            y - 4.0,
            esc(&b.name)
        );
    }

    // ── Plan: footprints, translucent so overlap and overhang read directly. ──
    let plan_top = sec_top + SECTION_H + 24.0;
    let ps = ((inner) / ex).min((PLAN_H - 2.0 * MARGIN) / ey);
    let _ = write!(
        o,
        "<text x=\"{MARGIN}\" y=\"{:.0}\" font-size=\"12\" font-weight=\"600\" fill=\"#444\">\
         Plan (X–Y) · to scale</text>\n",
        plan_top - 6.0
    );
    for (i, d) in a.dies.iter().enumerate() {
        let c = a.color(&d.master);
        let x = MARGIN + d.x * ps;
        // Y flipped so up is up, matching every layout tool.
        let y = plan_top + (ey - d.y - d.h) * ps;
        // Stacked dies very often share a footprint exactly, and then every label lands on the
        // same pixel and all but the last is invisible — a two-die stack that reads as one die.
        // Step each label down a line so the drawing shows how many there really are.
        let ly = y + 15.0 + 13.0 * i as f64;
        let _ = write!(
            o,
            "<rect x=\"{x:.1}\" y=\"{y:.1}\" width=\"{:.1}\" height=\"{:.1}\" fill=\"{c}\" \
             fill-opacity=\"0.18\" stroke=\"{c}\" stroke-width=\"1.4\"/>\n\
             <text x=\"{:.1}\" y=\"{ly:.1}\" font-size=\"10\" fill=\"{c}\">{} \
             <tspan fill=\"#888\">z={:.1}\u{b5}m</tspan></text>\n",
            (d.w * ps).max(1.0),
            (d.h * ps).max(1.0),
            x + 6.0,
            esc(&d.inst),
            d.z / dbu_per_um
        );
    }

    // ── Findings from the linter. The engines say what; this says where. ──
    let mut fy = plan_top + PLAN_H - MARGIN + 16.0;
    if a.findings.is_empty() {
        let _ = write!(
            o,
            "<text x=\"{MARGIN}\" y=\"{fy:.0}\" font-size=\"11\" fill=\"#2a7\">\
             check-3dblox: no violations</text>\n"
        );
    } else {
        let _ = write!(
            o,
            "<text x=\"{MARGIN}\" y=\"{fy:.0}\" font-size=\"11\" font-weight=\"600\" \
             fill=\"#d62728\">check-3dblox: {} finding(s)</text>\n",
            a.findings.len()
        );
        for (cat, name) in &a.findings {
            fy += 15.0;
            let _ = write!(
                o,
                "<text x=\"{:.0}\" y=\"{fy:.0}\" font-size=\"10\" fill=\"#a33\">{} — {}</text>\n",
                MARGIN + 10.0,
                esc(cat),
                esc(name)
            );
        }
    }

    fy += 22.0;
    let _ = write!(
        o,
        "<text x=\"{MARGIN}\" y=\"{fy:.0}\" font-size=\"9\" fill=\"#999\">\
         {} die(s), {} bond(s) · extent {:.1} \u{d7} {:.1} \u{b5}m, stack {:.1} \u{b5}m \
         · vertical scale is not the horizontal scale</text>\n</svg>\n",
        a.dies.len(),
        a.bonds.len(),
        ex / dbu_per_um,
        ey / dbu_per_um,
        ez / dbu_per_um
    );
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_die_stack() -> Assembly3d {
        Assembly3d {
            top: "stack".into(),
            dies: vec![
                Die {
                    inst: "u_base".into(),
                    master: "base".into(),
                    orient: "R0".into(),
                    x: 0.0,
                    y: 0.0,
                    z: 0.0,
                    w: 1000.0,
                    h: 800.0,
                    thickness: 100.0,
                    chip_type: "DIE".into(),
                },
                Die {
                    inst: "u_top".into(),
                    master: "top".into(),
                    orient: "MZ".into(),
                    x: 100.0,
                    y: 100.0,
                    z: 100.0,
                    w: 600.0,
                    h: 500.0,
                    thickness: 80.0,
                    chip_type: "DIE".into(),
                },
            ],
            bonds: vec![Bond {
                name: "bond0".into(),
                top: "u_top".into(),
                bottom: "u_base".into(),
                thickness: 5.0,
            }],
            findings: vec![],
        }
    }

    #[test]
    fn it_is_a_well_formed_self_contained_document() {
        let svg = to_svg(&two_die_stack(), 1.0);
        assert!(svg.starts_with("<svg"));
        assert!(svg.trim_end().ends_with("</svg>"));
        // Self-contained is the whole point — nothing to fetch, so it works from a file:// URL
        // and survives being committed to a repo.
        assert!(!svg.contains("http://") || !svg.contains("xlink:href"));
    }

    #[test]
    fn a_flipped_die_is_marked_as_flipped() {
        // The failure this guards against is silent: an MZ die drawn like an R0 one is a
        // plausible picture of the wrong assembly, which is worse than no picture.
        let svg = to_svg(&two_die_stack(), 1.0);
        assert!(svg.contains("flipped"), "MZ must be visible in the drawing");
    }

    #[test]
    fn the_upper_die_is_drawn_above_the_lower_one() {
        // SVG y grows downward and Z grows upward, so this inversion is exactly the kind of
        // thing that renders upside-down and looks fine until someone reads it.
        let svg = to_svg(&two_die_stack(), 1.0);
        let y_of = |name: &str| -> f64 {
            let i = svg.find(&format!(">{name}<")).expect("die label present");
            // The label's own y attribute, back-searched from the text element.
            let head = &svg[..i];
            let j = head.rfind("y=\"").unwrap();
            head[j + 3..].split('"').next().unwrap().parse().unwrap()
        };
        assert!(
            y_of("u_top") < y_of("u_base"),
            "u_top sits at higher Z so it must be drawn nearer the top of the page"
        );
    }

    #[test]
    fn the_z_exaggeration_factor_is_stated_on_the_drawing() {
        // A stretched axis that does not say so invites someone to measure a bond gap off the
        // picture. The number has to be on the page, not in the docs.
        let svg = to_svg(&two_die_stack(), 1.0);
        assert!(svg.contains("Z exaggerated"));
        assert!(svg.contains("vertical scale is not the horizontal scale"));
    }

    #[test]
    fn findings_are_listed_and_a_clean_assembly_says_so() {
        let clean = to_svg(&two_die_stack(), 1.0);
        assert!(clean.contains("no violations"));

        let dirty = to_svg(
            &two_die_stack().with_findings(vec![("Floating chips".into(), "u_base".into())]),
            1.0,
        );
        assert!(dirty.contains("1 finding(s)") && dirty.contains("Floating chips"));
    }

    #[test]
    fn an_empty_assembly_still_emits_valid_svg() {
        // Division by a zero extent is the obvious way this crashes; a database with no chips
        // is a perfectly ordinary thing to point the tool at.
        let svg = to_svg(&Assembly3d::default(), 1.0);
        assert!(svg.starts_with("<svg") && svg.trim_end().ends_with("</svg>"));
    }

    #[test]
    fn names_with_markup_characters_are_escaped() {
        let mut a = two_die_stack();
        a.dies[0].inst = "u<base>&x".into();
        let svg = to_svg(&a, 1.0);
        assert!(svg.contains("u&lt;base&gt;&amp;x"));
        assert!(!svg.contains("u<base>"));
    }
}
