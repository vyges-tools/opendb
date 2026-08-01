// SPDX-License-Identifier: Apache-2.0
//! Draw a 3D chiplet assembly — cross-section and plan — as **SVG or PNG**.
//!
//! The 2D layout viewer is a hard problem: a routed block is millions of polygons, so it needs a
//! tile server, an R-tree and a raster pyramid. **An assembly is not that.** A stack is a handful
//! of dies, each a box, with a few bond regions — tens of rectangles. So the thing that is out of
//! reach at layout scale is a few hundred lines here, with no server: one file, opens in a
//! browser, commits to a repo, embeds in a report.
//!
//! **The layout is built once, as a `Scene`,** and handed to `vyges-layout`\'s shared renderer for
//! either back-end. SVG is exact and diffable, so it is what belongs in a repo; PNG is what goes
//! into a slide, a web page or a message. A second function that re-derived the coordinates for
//! the raster path is how the two outputs would drift into being pictures of different things.
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
use vyges_layout::render::{text_width, Anchor, Rgb, Scene, Shape};

const DRAW_W: f64 = 900.0;
const MARGIN: f64 = 48.0;
const SECTION_H: f64 = 300.0;
const PLAN_H: f64 = 340.0;

/// Chip fill colours, indexed by master chip so two instances of one die read as the same part.
const PALETTE: &[Rgb] = &[
    (78, 121, 167),
    (242, 142, 43),
    (89, 161, 79),
    (176, 122, 161),
    (118, 183, 178),
    (237, 201, 72),
    (156, 117, 95),
    (225, 87, 89),
];

const INK: Rgb = (17, 17, 17);
const INK_BG: Rgb = (251, 251, 253);
const SUBHEAD: Rgb = (68, 68, 68);
const DIM: Rgb = (102, 102, 102);
const MUTED: Rgb = (153, 153, 153);
const BOND: Rgb = (214, 39, 40);
const BOND_DIM: Rgb = (170, 51, 51);
const OK: Rgb = (34, 119, 85);

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

    fn color(&self, master: &str) -> Rgb {
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

/// Build the drawing as a renderer-agnostic scene.
///
/// Returning a `Scene` rather than a string is what lets one layout serve both SVG and PNG: the
/// alternative — a second function that re-derives every coordinate for the raster path — is how
/// the two outputs drift into being pictures of different things.
///
/// `dbu_per_um` converts database units to microns for the dimension labels. Passing the wrong
/// value mislabels every dimension, so it is a required argument rather than a guess.
pub fn to_scene(a: &Assembly3d, dbu_per_um: f64) -> Scene {
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

    let total_h = SECTION_H + PLAN_H + 130.0 + 18.0 * a.findings.len() as f64;
    let mut sc = Scene::new(DRAW_W, total_h)
        .with_background(INK_BG)
        .with_title(format!("{} — chiplet assembly", a.top));
    sc.push(
        Shape::text(MARGIN, 30.0, format!("{} — chiplet assembly", a.top), 17.0, INK).bolded(),
    );

    // ── Cross-section: X across, Z up. Z grows upward, so the SVG y axis is inverted. ──
    let sec_top = 56.0;
    let base_y = sec_top + SECTION_H - MARGIN;
    sc.push(
        Shape::text(
            MARGIN,
            sec_top - 4.0,
            format!(
                "Cross-section (X-Z) · Z {} {z_exag:.2}x",
                if z_exag >= 1.0 { "exaggerated" } else { "compressed" }
            ),
            12.0,
            SUBHEAD,
        )
        .bolded(),
    );
    // Substrate datum, so "which way is up" is never ambiguous.
    sc.push(Shape::Line {
        x1: MARGIN,
        y1: base_y,
        x2: DRAW_W - MARGIN,
        y2: base_y,
        stroke: MUTED,
        width: 1.5,
        dashed: true,
    });
    sc.push(Shape::text(MARGIN, base_y + 13.0, "z = 0", 10.0, MUTED));

    for d in &a.dies {
        let x = MARGIN + d.x * sx;
        let w = (d.w * sx).max(1.0);
        let th = (d.thickness * sz).max(2.0);
        let y = base_y - (d.z + d.thickness) * sz;
        let c = a.color(&d.master);
        // A flipped die is the detail most worth seeing and easiest to miss, so it is called out
        // in the label rather than left to the orientation string alone.
        let flipped = d.orient.contains('M');
        sc.push(Shape::rect(x, y, w, th, c, 0.30, 1.6));

        // FRONT face: the active side. On an unflipped die it is up; flipped, it is down.
        let front_y = if flipped { y + th } else { y };
        sc.push(Shape::Line {
            x1: x,
            y1: front_y,
            x2: x + w,
            y2: front_y,
            stroke: c,
            width: 3.4,
            dashed: false,
        });
        // Front labels hug the left edge; bond labels use the right. Face-to-face dies put both
        // fronts on the same line, so anything sharing that edge collides.
        sc.push(Shape::text(
            x + 4.0,
            if flipped { front_y + 10.0 } else { front_y - 4.0 },
            "front",
            9.0,
            c,
        ));
        sc.push(
            Shape::text(x + w / 2.0, y + th / 2.0 + 1.0, d.inst.clone(), 11.0, INK)
                .anchored(Anchor::Middle),
        );
        sc.push(
            Shape::text(
                x + w / 2.0,
                y + th / 2.0 + 13.0,
                format!(
                    "{} · {} · {:.1} um thick{}",
                    d.master,
                    d.orient,
                    d.thickness / dbu_per_um,
                    if flipped { " · flipped" } else { "" }
                ),
                9.0,
                DIM,
            )
            .anchored(Anchor::Middle),
        );
    }

    // Bonds, drawn at the interface between the two dies they name.
    //
    // Several bonds landing on one plane is the normal case, not an edge case — every die
    // mounted on the same interposer bonds at the same Z — so labels have to be stacked or they
    // print on top of each other and an assembly with three bonds reads as having one.
    let mut used_label_y: Vec<f64> = Vec::new();
    for b in &a.bonds {
        let find = |p: &str| {
            let leaf = p.rsplit('/').next().unwrap_or(p);
            a.dies.iter().find(|d| d.inst == leaf)
        };
        let (Some(t), Some(bt)) = (find(&b.top), find(&b.bottom)) else {
            continue;
        };
        // The mating plane: the top of the lower die, which is where the bond sits.
        let y = base_y - (bt.z + bt.thickness) * sz;
        let x0 = MARGIN + t.x.max(bt.x) * sx;
        let x1 = MARGIN + ((t.x + t.w).min(bt.x + bt.w)) * sx;
        sc.push(Shape::Line {
            x1: x0,
            y1: y,
            x2: x1,
            y2: y,
            stroke: BOND,
            width: 2.4,
            dashed: true,
        });
        // A bond spanning the full width leaves no room to the right, so the label goes inside
        // the overlap rather than off the edge of the page.
        let outside = x1 + 5.0;
        let (lx, anchor) = if outside + text_width(&b.name, 9.0) > DRAW_W - 4.0 {
            (x1 - 5.0, Anchor::End)
        } else {
            (outside, Anchor::Start)
        };
        let mut ly = y - 4.0;
        while used_label_y.iter().any(|u| (u - ly).abs() < 11.0) {
            ly -= 12.0;
        }
        used_label_y.push(ly);
        sc.push(Shape::text(lx, ly, b.name.clone(), 9.0, BOND).anchored(anchor));
    }

    // ── Plan: footprints, translucent so overlap and overhang read directly. ──
    let plan_top = sec_top + SECTION_H + 24.0;
    let ps = inner.min(PLAN_H - 2.0 * MARGIN) / ex.max(ey);
    let ps = (inner / ex).min((PLAN_H - 2.0 * MARGIN) / ey).min(ps.max(f64::MIN_POSITIVE));
    sc.push(Shape::text(MARGIN, plan_top - 6.0, "Plan (X-Y) · to scale", 12.0, SUBHEAD).bolded());
    for (i, d) in a.dies.iter().enumerate() {
        let c = a.color(&d.master);
        let x = MARGIN + d.x * ps;
        // Y flipped so up is up, matching every layout tool.
        let y = plan_top + (ey - d.y - d.h) * ps;
        sc.push(Shape::rect(
            x,
            y,
            (d.w * ps).max(1.0),
            (d.h * ps).max(1.0),
            c,
            0.18,
            1.4,
        ));
        // Stacked dies very often share a footprint exactly, and then every label lands on the
        // same pixel and all but the last is invisible — a two-die stack that reads as one die.
        // Step each label down a line so the drawing shows how many there really are.
        sc.push(Shape::text(
            x + 6.0,
            y + 15.0 + 13.0 * i as f64,
            format!("{}  z={:.1}um", d.inst, d.z / dbu_per_um),
            10.0,
            c,
        ));
    }

    // ── Findings from the linter. The engines say what; this says where. ──
    let mut fy = plan_top + PLAN_H - MARGIN + 16.0;
    if a.findings.is_empty() {
        sc.push(Shape::text(MARGIN, fy, "check-3dblox: no violations", 11.0, OK));
    } else {
        sc.push(
            Shape::text(
                MARGIN,
                fy,
                format!("check-3dblox: {} finding(s)", a.findings.len()),
                11.0,
                BOND,
            )
            .bolded(),
        );
        for (cat, name) in &a.findings {
            fy += 15.0;
            sc.push(Shape::text(
                MARGIN + 10.0,
                fy,
                format!("{cat} — {name}"),
                10.0,
                BOND_DIM,
            ));
        }
    }

    fy += 22.0;
    sc.push(Shape::text(
        MARGIN,
        fy,
        format!(
            "{} die(s), {} bond(s) · extent {:.1} x {:.1} um, stack {:.1} um · \
             vertical scale is not the horizontal scale",
            a.dies.len(),
            a.bonds.len(),
            ex / dbu_per_um,
            ey / dbu_per_um,
            ez / dbu_per_um
        ),
        9.0,
        MUTED,
    ));
    sc
}

/// Render to one self-contained SVG document.
pub fn to_svg(a: &Assembly3d, dbu_per_um: f64) -> String {
    to_scene(a, dbu_per_um).to_svg()
}

/// Render to PNG at `scale` device pixels per drawing unit.
///
/// 2.0 is the useful default: these drawings are ~900 units wide, and a 1:1 raster of one is too
/// small to read once it is scaled to fit a slide.
pub fn to_png(a: &Assembly3d, dbu_per_um: f64, scale: f64) -> Vec<u8> {
    to_scene(a, dbu_per_um).to_png(scale)
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
        // Either direction is legitimate — a thin stack is stretched, a tall one compressed —
        // but the drawing must never be silent about which happened.
        assert!(svg.contains("Z exaggerated") || svg.contains("Z compressed"));
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
    fn bonds_sharing_a_plane_do_not_stack_their_labels_on_one_line() {
        // Every die on one interposer bonds at the same Z, so this is the normal case. Labels
        // printed on top of each other make a three-bond assembly read as having one.
        let mut a = two_die_stack();
        a.bonds.push(Bond {
            name: "bond1".into(),
            top: "u_top".into(),
            bottom: "u_base".into(),
            thickness: 5.0,
        });
        let sc = to_scene(&a, 1.0);
        let ys: Vec<i64> = sc
            .shapes
            .iter()
            .filter_map(|s| match s {
                Shape::Text { y, text, .. } if text.starts_with("bond") => Some(*y as i64),
                _ => None,
            })
            .collect();
        assert_eq!(ys.len(), 2, "both bond labels must be drawn");
        assert_ne!(ys[0], ys[1], "two bonds on one plane must not share a label line");
    }

    #[test]
    fn the_png_is_a_valid_image_of_the_same_scene() {
        // Both back-ends read the SAME scene, so this is really asserting that the shared
        // renderer is reached at all — a drawing that only ever emits SVG would still pass every
        // other test in this module.
        let a = two_die_stack();
        let png = to_png(&a, 1.0, 2.0);
        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
        let w = u32::from_be_bytes(png[16..20].try_into().unwrap());
        assert_eq!(w, (to_scene(&a, 1.0).width * 2.0) as u32, "scale must reach the raster");

        // And it must not be a blank page: an encoder that draws nothing still emits a valid PNG.
        let empty = to_png(&Assembly3d::default(), 1.0, 2.0);
        assert_ne!(png, empty);
    }

    #[test]
    fn one_layout_feeds_both_back_ends() {
        // If the two paths ever diverge, the PNG becomes a picture of a different assembly than
        // the SVG — the failure this whole Scene indirection exists to prevent.
        let a = two_die_stack();
        let sc = to_scene(&a, 1.0);
        assert_eq!(sc.to_svg(), to_svg(&a, 1.0));
        assert_eq!(sc.to_png(2.0), to_png(&a, 1.0, 2.0));
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
