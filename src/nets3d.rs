// SPDX-License-Identifier: Apache-2.0
//! **Through-stack net continuity** — does a signal entering a chiplet stack arrive anywhere?
//!
//! # Why this exists
//!
//! [`crate::d2d`] checks one bond: are these two mating faces aligned and carrying the same
//! signals? That is a local question, and a stack can pass it at every interface and still be
//! dead. A signal leaves die A's front, crosses into die B's back, and must reach die B's front to
//! continue to die C. If B has no TSVs, it does not — and every bond in that stack is individually
//! perfect, so nothing reports it:
//!
//! | | reports |
//! | --- | --- |
//! | `check_3dblox` *Logical Connectivity* | clean — it compares only bumps at exactly coincident points |
//! | `check_3dblox` `checkNetConnectivity` | clean — an empty function body upstream |
//! | `check-d2d` | clean — each bond is correctly mated and correctly netted |
//! | this | `severed`, naming the die the net cannot cross |
//!
//! # What it works on
//!
//! The **assembly and its bump maps** — the same inputs as `check-d2d --input`, for the same
//! reason: they are what a user has. The net name is a column of a `.bmap`
//! (`bumpInstName bumpCellType x y portName netName`), the bonded region pairs come from the
//! `.3dbx`, and whether a die can pass a signal through comes from its `tsv` flag in the `.3dbv`.
//!
//! It is deliberately **not** driven from a loaded database, even though `dbChipNet` exists there.
//! Every traversal edge that path needs is unbridged at our pin — `dbUnfoldedChipNet::
//! getConnectedBumps`, `dbChipRegion::getChipBumps`, the `dbUnfoldedChipConn` region relations —
//! and on the write side `dbChipBump::setNet` is unbridged too, so a database *we* build carries
//! no chip nets at all. A checker over that would report every stack clean, which is the one
//! outcome worse than no checker.
//!
//! # A net name is per die, and that bounds what can be concluded
//!
//! The `netName` column names a net in **that die's own netlist**. Two instances of one chiplet both
//! carry a bump called `VDD`, and those are two different nets until something joins them; what
//! joins die nets into assembly nets is the `.3dbx`'s `external.verilog_file`, which this layer does
//! not read. So net identity is taken from the **graph** — same name within one die, plus whatever
//! the bonding physically mates — and never from name equality across unbonded dies.
//!
//! That is not a detail. An earlier draft grouped by name globally and reported 38 split nets on
//! upstream's own `example.3dbx`, which instantiates one chiplet twice: every `VDD`, `VSS` and
//! `soc_io[n]` appeared to be a net in two pieces. They were two nets that share a name, on an
//! assembly where not one bonded surface carries a bump map. Findings are therefore scoped to what
//! a die or a bond can settle by itself:
//!
//! - **within one die** — a net on both faces that nothing joins between them is `severed`;
//! - **across one bond** — two names the bonding shorts together is `net-merged`;
//! - **anything needing an assembly netlist** — declined, not guessed.
//!
//! # What it does not do
//!
//! It does not invent TSV geometry. `tsv` is a per-die boolean; neither 3Dblox's chiplet header nor
//! odb carries TSV locations, so a through-path inside a die is inferred from **net names matching
//! across the die's two faces** — sound, because both names are in the same netlist — and nothing
//! else. That is still a convention rather than a standard, so every finding says which rule
//! produced it and [`Options::tsv_inference`] turns it off.
//!
//! It also declines to conclude anything a **skipped** bond could explain. A bond whose surfaces
//! declare no bump map is unchecked, and a finding that would disappear if that bond turned out to
//! be fine is reported as unresolved rather than as a defect.

use crate::blox::{read_assembly, RegionRef};
use crate::d2d::{self, BumpMap, Placement};
use crate::{Error, Result};
use std::collections::{BTreeMap, BTreeSet};

/// One bump, resolved to where it sits in the assembly.
#[derive(Debug, Clone)]
pub struct Node {
    /// Chip-instance path, joined with `/` for a nested assembly.
    pub inst: String,
    pub chiplet: String,
    pub region: String,
    /// `front` | `back` | `internal` | `internal_ext`, lowercased as the file spells it.
    pub side: String,
    pub bump: String,
    pub net: Option<String>,
}

impl Node {
    /// `inst/region/bump` — how a finding names a bump so a reader can find it.
    pub fn path(&self) -> String {
        format!("{}/{}/{}", self.inst, self.region, self.bump)
    }
}

/// A placed die, and whether it can carry a signal from one face to the other.
#[derive(Debug, Clone)]
pub struct ChipInst {
    pub inst: String,
    pub chiplet: String,
    pub tsv: bool,
}

/// Two bumps the bonding joins, as indices into the node list.
#[derive(Debug, Clone, Copy)]
pub struct Mate {
    pub bond: usize,
    pub top: usize,
    pub bottom: usize,
}

/// A bonding surface: `(chip instance, region)`.
pub type Surface = (String, String);

/// A bond that names two surfaces but could not be checked — typically because neither declares a
/// bump map. Carried into the analysis rather than only into the report: a finding that would
/// vanish if this bond turned out to be fine must not be stated as a defect.
#[derive(Debug, Clone)]
pub struct Unchecked {
    pub bond: String,
    pub top: Surface,
    pub bottom: Surface,
}

/// Per-bond summary — how many pairs mated, and under what tolerance.
#[derive(Debug, Clone)]
pub struct BondSummary {
    pub name: String,
    pub top: String,
    pub bottom: String,
    pub matched: usize,
    pub tolerance_um: f64,
    pub tolerance_source: &'static str,
}

#[derive(Debug, Clone)]
pub enum Finding {
    /// A net reaches a die on one face and cannot leave the other. The headline case: dead
    /// silicon that every existing check passes.
    Severed {
        net: String,
        inst: String,
        chiplet: String,
        tsv: bool,
        /// The two faces of that die the net lands on without being joined between them.
        faces: (String, String),
    },
    /// A net on both faces of one die that only an **unchecked** bond could be joining. Not a
    /// defect and not clean: the assembly does not say, because a bond carries no bump map.
    Unresolved { net: String, inst: String, blocking: Vec<String> },
    /// Two differently named nets joined by the bonding — a short across an interface.
    ///
    /// `check-d2d` reports this per interface as `net-mismatch`; here it is one finding for the
    /// net group, naming every bond that contributes, because a merge that happens at three
    /// separate bonds is one wrong net rather than three wrong interfaces.
    NetMerged {
        nets: Vec<String>,
        bonds: Vec<String>,
        /// One mated pair that does the merging, so the finding is locatable.
        example: (String, String),
    },
    /// A die declares TSVs and no net crosses it. Not a failure — but either the flag is wrong or
    /// a through-connection was intended and lost, and both are worth knowing before tapeout.
    TsvUnused { inst: String, chiplet: String },
}

impl Finding {
    pub fn kind(&self) -> &'static str {
        match self {
            Finding::Severed { .. } => "severed",
            Finding::Unresolved { .. } => "unresolved",
            Finding::NetMerged { .. } => "net-merged",
            Finding::TsvUnused { .. } => "tsv-unused",
        }
    }

    /// Whether this finding sets the exit code.
    ///
    /// `tsv-unused` does not: it is an observation about intent, and a checker that failed CI over
    /// one would be ignored rather than heeded. `unresolved` does not either — it means the
    /// assembly did not say, and failing a build over missing input rather than over a defect is
    /// how a checker teaches people to pass `--no-verify`.
    pub fn is_violation(&self) -> bool {
        matches!(self, Finding::Severed { .. } | Finding::NetMerged { .. })
    }

    pub fn message(&self) -> String {
        match self {
            Finding::Severed { net, inst, chiplet, tsv, faces } => format!(
                "net {net} lands on {inst}/{} and {inst}/{} but is not joined between them — \
                 chiplet {chiplet} declares {}",
                faces.0,
                faces.1,
                if *tsv {
                    "TSVs, so the two faces name the net differently"
                } else {
                    "no TSVs, so nothing carries the net through the die"
                }
            ),
            Finding::Unresolved { net, inst, blocking } => format!(
                "net {net} is not joined across {inst}, but {} was not checked, so this is \
                 undetermined rather than severed",
                blocking.join(", ")
            ),
            Finding::NetMerged { nets, bonds, example } => format!(
                "nets {} are joined by the bonding: {} mates with {} at {}",
                nets.join(" and "),
                example.0,
                example.1,
                bonds.join(", ")
            ),
            Finding::TsvUnused { inst, chiplet } => format!(
                "{inst} ({chiplet}) declares TSVs but no net crosses it"
            ),
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        let mut v = serde_json::json!({ "kind": self.kind(), "message": self.message() });
        let o = v.as_object_mut().expect("json! built an object");
        match self {
            Finding::Severed { net, inst, chiplet, tsv, faces } => {
                o.insert("net".into(), net.clone().into());
                o.insert("chip_inst".into(), inst.clone().into());
                o.insert("chiplet".into(), chiplet.clone().into());
                o.insert("tsv".into(), (*tsv).into());
                o.insert("faces".into(), serde_json::json!([faces.0, faces.1]));
            }
            Finding::Unresolved { net, inst, blocking } => {
                o.insert("net".into(), net.clone().into());
                o.insert("chip_inst".into(), inst.clone().into());
                o.insert("blocking_bonds".into(), serde_json::json!(blocking));
            }
            Finding::NetMerged { nets, bonds, example } => {
                o.insert("nets".into(), serde_json::json!(nets));
                o.insert("bonds".into(), serde_json::json!(bonds));
                o.insert("top".into(), example.0.clone().into());
                o.insert("bottom".into(), example.1.clone().into());
            }
            Finding::TsvUnused { inst, chiplet } => {
                o.insert("chip_inst".into(), inst.clone().into());
                o.insert("chiplet".into(), chiplet.clone().into());
            }
        }
        v
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    /// Match radius in microns; `None` derives it per bond from the bump pitch.
    pub tolerance_um: Option<f64>,
    /// Join a TSV die's two faces where the bump maps agree on a net name. Off makes every
    /// through-path read as severed, which is the honest answer when the maps do not use
    /// consistent names across faces — see the module docs.
    pub tsv_inference: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options { tolerance_um: None, tsv_inference: true }
    }
}

#[derive(Debug, Clone)]
pub struct Nets3dReport {
    pub nodes: usize,
    pub nets: usize,
    /// Connected groups of bumps after bonding and any TSV inference.
    pub groups: usize,
    /// Bumps whose bump map named no net (`-`). Counted, not judged: a supply bump legitimately
    /// carries no signal name, and guessing which is which would invent findings.
    pub unnetted: usize,
    pub bonds: Vec<BondSummary>,
    /// Bonds that were **not** checked, each saying why. Never counted as clean.
    pub interfaces_skipped: Vec<String>,
    /// Bonding surfaces whose bumps could not be placed, each saying why.
    pub regions_skipped: Vec<String>,
    pub findings: Vec<Finding>,
    /// `(bump map path, line, what was wrong)`.
    pub parse_errors: Vec<(String, usize, String)>,
    pub tsv_inference: bool,
}

impl Nets3dReport {
    pub fn violations(&self) -> usize {
        self.findings.iter().filter(|f| f.is_violation()).count()
    }

    pub fn count(&self, kind: &str) -> usize {
        self.findings.iter().filter(|f| f.kind() == kind).count()
    }

    pub fn to_json(&self) -> serde_json::Value {
        let by_kind: BTreeMap<&str, usize> =
            ["severed", "net-merged", "unresolved", "tsv-unused"]
                .into_iter()
                .map(|k| (k, self.count(k)))
                .filter(|(_, n)| *n > 0)
                .collect();
        serde_json::json!({
            "violations": self.violations(),
            "by_kind": by_kind,
            "nets": self.nets,
            "bumps": self.nodes,
            "groups": self.groups,
            "unnetted_bumps": self.unnetted,
            // Always stated: a continuity verdict means nothing without saying which description
            // of the nets produced it.
            "net_source": "bump maps",
            "tsv_inference": self.tsv_inference,
            "interfaces_checked": self.bonds.len(),
            "bonds": self.bonds.iter().map(|b| serde_json::json!({
                "bond": b.name, "top": b.top, "bottom": b.bottom, "matched": b.matched,
                "tolerance_um": b.tolerance_um, "tolerance_source": b.tolerance_source,
            })).collect::<Vec<_>>(),
            "interfaces_skipped": self.interfaces_skipped,
            "regions_skipped": self.regions_skipped,
            "findings": self.findings.iter().map(|f| f.to_json()).collect::<Vec<_>>(),
            "parse_errors": self.parse_errors.iter().map(|(f, l, m)| serde_json::json!({
                "file": f, "line": l, "error": m,
            })).collect::<Vec<_>>(),
        })
    }
}

/// Union-find over bump nodes. The whole analysis is "what is joined to what", and a component
/// walk is the honest way to answer it — a per-net path search would need a direction, and a bond
/// has none.
struct Groups(Vec<usize>);

impl Groups {
    fn new(n: usize) -> Groups {
        Groups((0..n).collect())
    }
    fn root(&mut self, mut i: usize) -> usize {
        while self.0[i] != i {
            let parent = self.0[i];
            self.0[i] = self.0[parent];
            i = self.0[i];
        }
        i
    }
    fn union(&mut self, a: usize, b: usize) -> bool {
        let (ra, rb) = (self.root(a), self.root(b));
        if ra == rb {
            return false;
        }
        self.0[ra] = rb;
        true
    }
}

/// What the graph turned out to mean.
#[derive(Debug, Clone)]
pub struct Analysis {
    pub findings: Vec<Finding>,
    /// Connected groups of bumps, counted over the same graph the findings came from — including
    /// the same-face and TSV joins, so the number in the report cannot disagree with them.
    pub groups: usize,
}

/// Decide what the graph means. Separated from loading so the decisions — severed versus
/// merged, unresolved — are testable without files, which is where the subtlety is.
pub fn analyze(
    nodes: &[Node],
    chips: &[ChipInst],
    mates: &[Mate],
    bond_names: &[String],
    unchecked: &[Unchecked],
    opts: &Options,
) -> Analysis {
    /// Build the connectivity graph. `extra` is the optimistic pass's assumed edges; the returned
    /// merge evidence and set of dies a net crossed describe the graph that was built.
    #[allow(clippy::type_complexity)]
    fn build<'a>(
        nodes: &'a [Node],
        chips: &'a [ChipInst],
        mates: &[Mate],
        opts: &Options,
        extra: &[Unchecked],
    ) -> (Groups, Vec<(usize, usize, usize)>, BTreeSet<&'a str>) {
        let mut g = Groups::new(nodes.len());
        let mut merges = Vec::new();
        let mut crossed = BTreeSet::new();

        // Within one face of one die, a shared net name is not an inference: a `netName` is a name
        // in that die's own netlist, so two bumps carrying it are the same net, joined by the die's
        // routing. Without this an interface that lands one signal on two bumps looks disconnected,
        // which power and wide buses do on every real design.
        let mut same_face: BTreeMap<(&str, &str, &str), Vec<usize>> = BTreeMap::new();
        for (i, n) in nodes.iter().enumerate() {
            if let Some(net) = n.net.as_deref() {
                same_face.entry((n.inst.as_str(), n.side.as_str(), net)).or_default().push(i);
            }
        }
        for group in same_face.values() {
            for w in group.windows(2) {
                g.union(w[0], w[1]);
            }
        }

        // Crossing from one face to the other is the different question, and the one that needs
        // TSVs. Position is deliberately not used — a TSV field need not align with the bump field
        // above it, and inventing a coincidence rule would manufacture findings.
        if opts.tsv_inference {
            for c in chips.iter().filter(|c| c.tsv) {
                let mut by_net: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
                for (i, n) in nodes.iter().enumerate() {
                    if n.inst == c.inst {
                        if let Some(net) = n.net.as_deref() {
                            by_net.entry(net).or_default().push(i);
                        }
                    }
                }
                for group in by_net.values() {
                    for w in group.windows(2) {
                        if nodes[w[0]].side != nodes[w[1]].side && g.union(w[0], w[1]) {
                            crossed.insert(c.inst.as_str());
                        }
                    }
                }
            }
        }

        for m in mates {
            g.union(m.top, m.bottom);
            if nodes[m.top].net != nodes[m.bottom].net {
                merges.push((m.bond, m.top, m.bottom));
            }
        }

        // The optimistic pass only: assume every unchecked bond mates everything on its two
        // surfaces. Deliberately cruder than a real pairing — the point is to be as generous as
        // possible about what an unexamined bond might connect.
        for u in extra {
            let on = |s: &Surface| -> Vec<usize> {
                (0..nodes.len())
                    .filter(|&i| nodes[i].inst == s.0 && nodes[i].region == s.1)
                    .collect()
            };
            let (t, b) = (on(&u.top), on(&u.bottom));
            let all: Vec<usize> = t.iter().chain(b.iter()).copied().collect();
            for w in all.windows(2) {
                g.union(w[0], w[1]);
            }
        }
        (g, merges, crossed)
    }

    let (mut g, merge_evidence, traversed) = build(nodes, chips, mates, opts, &[]);
    // What would still be true even if every unchecked bond turned out perfect. Only findings that
    // survive this are stated as defects.
    let mut optimistic = (!unchecked.is_empty())
        .then(|| build(nodes, chips, mates, opts, unchecked).0);

    // Nets are keyed by (die instance, name), never by name alone: the same name on two dies is
    // two nets until the bonding joins them, and only the assembly netlist — which this layer does
    // not read — could say otherwise. Everything below is therefore a question one die or one bond
    // can settle. BTreeMap throughout: a findings list that reorders between runs cannot be diffed.
    let mut by_die_net: BTreeMap<(&str, &str), Vec<usize>> = BTreeMap::new();
    for (i, n) in nodes.iter().enumerate() {
        if let Some(net) = n.net.as_deref() {
            by_die_net.entry((n.inst.as_str(), net)).or_default().push(i);
        }
    }

    let mut severed = Vec::new();
    let mut unresolved = Vec::new();
    for ((inst, net), members) in &by_die_net {
        // A net stopping at a die is not a defect — that die may be the destination. It is a defect
        // when the net lands on *both* faces of one die and nothing joins them: the die's own
        // netlist says these are one net, and no path inside the die carries it.
        let mut faces: BTreeMap<usize, &str> = BTreeMap::new();
        for &i in members.iter() {
            faces.entry(g.root(i)).or_insert(&nodes[i].side);
        }
        if faces.len() < 2 {
            continue;
        }
        let Some(c) = chips.iter().find(|c| c.inst == *inst) else { continue };
        if let Some(o) = optimistic.as_mut() {
            let generous: BTreeSet<usize> = members.iter().map(|&i| o.root(i)).collect();
            if generous.len() < 2 {
                unresolved.push(Finding::Unresolved {
                    net: (*net).to_string(),
                    inst: (*inst).to_string(),
                    blocking: unchecked.iter().map(|u| u.bond.clone()).collect(),
                });
                continue;
            }
        }
        let mut sides = faces.values().map(|s| s.to_string()).collect::<Vec<_>>();
        sides.sort();
        severed.push(Finding::Severed {
            net: (*net).to_string(),
            inst: (*inst).to_string(),
            chiplet: c.chiplet.clone(),
            tsv: c.tsv,
            faces: (sides[0].clone(), sides[1].clone()),
        });
    }

    // A merged net is a property of a group, not of a bond: report it once for the group and name
    // every bond that contributes.
    let mut merged_groups: BTreeMap<usize, Vec<(usize, usize, usize)>> = BTreeMap::new();
    for &(bond, top, bottom) in &merge_evidence {
        merged_groups.entry(g.root(top)).or_default().push((bond, top, bottom));
    }
    let mut merged = Vec::new();
    for (root, evidence) in &merged_groups {
        let nets: BTreeSet<&str> = nodes
            .iter()
            .enumerate()
            .filter(|(i, _)| g.root(*i) == *root)
            .filter_map(|(_, n)| n.net.as_deref())
            .collect();
        if nets.len() < 2 {
            continue;
        }
        let bonds: BTreeSet<&str> = evidence
            .iter()
            .map(|(b, _, _)| bond_names.get(*b).map(String::as_str).unwrap_or("?"))
            .collect();
        let (_, top, bottom) = evidence[0];
        merged.push(Finding::NetMerged {
            nets: nets.into_iter().map(str::to_string).collect(),
            bonds: bonds.into_iter().map(str::to_string).collect(),
            example: (nodes[top].path(), nodes[bottom].path()),
        });
    }

    let unused: Vec<Finding> = chips
        .iter()
        .filter(|c| c.tsv && !traversed.contains(c.inst.as_str()))
        .map(|c| Finding::TsvUnused { inst: c.inst.clone(), chiplet: c.chiplet.clone() })
        .collect();

    // Severest first, and informational last. A reader who stops after one line should have read
    // the worst thing in the report.
    let mut findings = severed;
    findings.extend(merged);
    findings.extend(unresolved);
    findings.extend(unused);

    let groups: BTreeSet<usize> = (0..nodes.len()).map(|i| g.root(i)).collect();
    Analysis { findings, groups: groups.len() }
}

/// Where one bonding surface's bumps live in the node list, plus its field in the assembly frame.
struct SurfaceBumps {
    base: usize,
    map: BumpMap,
}

/// Check every net in a 3Dblox assembly.
pub fn check_assembly(dbx_path: &str, opts: &Options) -> Result<Nets3dReport> {
    let asm = read_assembly(dbx_path)?;

    let mut nodes: Vec<Node> = Vec::new();
    let mut chips: Vec<ChipInst> = Vec::new();
    let mut surfaces: BTreeMap<Surface, SurfaceBumps> = BTreeMap::new();
    let mut regions_skipped = Vec::new();
    let mut parse_errors = Vec::new();
    let mut unnetted = 0usize;

    for inst in &asm.dbx.insts {
        let Some(def) = asm.defs.get(&inst.reference) else { continue };
        chips.push(ChipInst {
            inst: inst.name.clone(),
            chiplet: def.name.clone(),
            tsv: def.tsv,
        });
        for region in def.regions.iter().filter(|r| r.bmap.is_some()) {
            let path = region.bmap.as_deref().expect("filtered on Some");
            let Some((w, h)) = def.design_area else {
                // Without the die's extent a mirrored orientation cannot be resolved, and
                // resolving it wrongly would place the whole field somewhere it is not.
                regions_skipped.push(format!(
                    "{}.{}: chiplet {} declares no design_area, so its bumps cannot be placed",
                    inst.name, region.name, def.name
                ));
                continue;
            };
            let map = BumpMap::load(path).map_err(|e| Error::Odb(format!("{path}: {e}")))?;
            for (line, why) in &map.errors {
                parse_errors.push((path.to_string(), *line, why.clone()));
            }
            let at = Placement {
                orient: inst.placement.orient.clone(),
                loc_x: inst.placement.loc.0,
                loc_y: inst.placement.loc.1,
                die_w: w,
                die_h: h,
            };
            // odb silently treats an unrecognised orientation as R0. Inheriting that would place
            // a die wrongly and then report the stack connected.
            let global = at.apply(&map).ok_or_else(|| {
                Error::Odb(format!(
                    "{}: unsupported orientation `{}`",
                    inst.name, inst.placement.orient
                ))
            })?;
            let base = nodes.len();
            for b in &global.bumps {
                if b.net.is_none() {
                    unnetted += 1;
                }
                nodes.push(Node {
                    inst: inst.name.clone(),
                    chiplet: def.name.clone(),
                    region: region.name.clone(),
                    side: region.side.to_lowercase(),
                    bump: b.inst.clone(),
                    net: b.net.clone(),
                });
            }
            surfaces.insert(
                (inst.name.clone(), region.name.clone()),
                SurfaceBumps { base, map: global },
            );
        }
    }

    let mut mates: Vec<Mate> = Vec::new();
    let mut bond_names: Vec<String> = Vec::new();
    let mut bonds: Vec<BondSummary> = Vec::new();
    let mut interfaces_skipped = Vec::new();
    // Bonds that name two real surfaces and could not be checked. These are not merely reported:
    // they bound what the analysis is allowed to conclude, since a finding an unchecked bond could
    // explain is undetermined rather than a defect.
    let mut unchecked: Vec<Unchecked> = Vec::new();

    for conn in &asm.dbx.connections {
        // A virtual bond names no counterpart, and a nested path is not resolved here. Both are
        // reported rather than dropped: "we did not look" and "we looked and found nothing" are
        // different answers.
        let Some(bot) = &conn.bot else {
            interfaces_skipped.push(format!("{}: virtual bond (bot: ~), no second surface", conn.name));
            continue;
        };
        let leaf = |r: &RegionRef| -> Option<(String, String)> {
            Some((r.inst_path.last()?.clone(), r.region.clone()))
        };
        if conn.top.inst_path.len() != 1 || bot.inst_path.len() != 1 {
            interfaces_skipped.push(format!("{}: nested instance path", conn.name));
            continue;
        }
        let (Some(tk), Some(bk)) = (leaf(&conn.top), leaf(bot)) else {
            interfaces_skipped.push(format!("{}: could not resolve both surfaces", conn.name));
            continue;
        };
        let (Some(ts), Some(bs)) = (surfaces.get(&tk), surfaces.get(&bk)) else {
            interfaces_skipped.push(format!(
                "{}: no bump map on {}.{} or {}.{}",
                conn.name, tk.0, tk.1, bk.0, bk.1
            ));
            unchecked.push(Unchecked { bond: conn.name.clone(), top: tk, bottom: bk });
            continue;
        };
        let (tolerance, source) = d2d::derive_tolerance(&ts.map, &bs.map, opts.tolerance_um);
        let pairs = d2d::mate(&ts.map.bumps, &bs.map.bumps, tolerance);
        let bond = bond_names.len();
        for (i, j, _) in &pairs {
            mates.push(Mate { bond, top: ts.base + i, bottom: bs.base + j });
        }
        bonds.push(BondSummary {
            name: conn.name.clone(),
            top: format!("{}.{}", tk.0, tk.1),
            bottom: format!("{}.{}", bk.0, bk.1),
            matched: pairs.len(),
            tolerance_um: tolerance,
            tolerance_source: source,
        });
        bond_names.push(conn.name.clone());
    }

    let analysis = analyze(&nodes, &chips, &mates, &bond_names, &unchecked, opts);
    let nets: BTreeSet<&str> = nodes.iter().filter_map(|n| n.net.as_deref()).collect();

    Ok(Nets3dReport {
        nodes: nodes.len(),
        nets: nets.len(),
        groups: analysis.groups,
        unnetted,
        bonds,
        interfaces_skipped,
        regions_skipped,
        findings: analysis.findings,
        parse_errors,
        tsv_inference: opts.tsv_inference,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(inst: &str, region: &str, side: &str, bump: &str, net: Option<&str>) -> Node {
        Node {
            inst: inst.into(),
            chiplet: format!("{inst}_def"),
            region: region.into(),
            side: side.into(),
            bump: bump.into(),
            net: net.map(str::to_string),
        }
    }

    fn chip(inst: &str, tsv: bool) -> ChipInst {
        ChipInst { inst: inst.into(), chiplet: format!("{inst}_def"), tsv }
    }

    /// Three dies: base -> mid -> top, with `n_thru` needing to cross `mid`.
    fn three_die(tsv: bool) -> (Vec<Node>, Vec<ChipInst>, Vec<Mate>, Vec<String>) {
        let nodes = vec![
            node("u_base", "front", "front", "bs0", Some("n_thru")), // 0
            node("u_mid", "back", "back", "md_b0", Some("n_thru")),  // 1
            node("u_mid", "front", "front", "md_f0", Some("n_thru")), // 2
            node("u_top", "back", "back", "tp0", Some("n_thru")),    // 3
        ];
        let chips = vec![chip("u_base", false), chip("u_mid", tsv), chip("u_top", false)];
        let mates = vec![
            Mate { bond: 0, top: 1, bottom: 0 },
            Mate { bond: 1, top: 3, bottom: 2 },
        ];
        (nodes, chips, mates, vec!["bond0".into(), "bond1".into()])
    }

    #[test]
    fn a_net_that_cannot_cross_a_die_without_tsvs_is_severed() {
        // Both bonds are perfectly mated and correctly netted, so check-d2d reports clean on each
        // and upstream's connectivity check reports clean on all of it. The signal still stops.
        let (n, c, m, b) = three_die(false);
        let f = analyze(&n, &c, &m, &b, &[], &Options::default()).findings;
        assert_eq!(f.len(), 1, "{:?}", f.iter().map(|x| x.message()).collect::<Vec<_>>());
        assert_eq!(f[0].kind(), "severed");
        let j = f[0].to_json();
        assert_eq!(j["net"], "n_thru");
        assert_eq!(j["chip_inst"], "u_mid", "the finding must name the die that cannot pass it");
        assert_eq!(j["tsv"], false);
        assert_eq!(j["faces"], serde_json::json!(["back", "front"]));
        assert!(f[0].is_violation());
    }

    #[test]
    fn the_same_stack_with_tsvs_is_clean() {
        // The control. Without this the severed test proves only that the checker fires.
        let (n, c, m, b) = three_die(true);
        let f = analyze(&n, &c, &m, &b, &[], &Options::default()).findings;
        assert!(f.is_empty(), "{:?}", f.iter().map(|x| x.message()).collect::<Vec<_>>());
    }

    #[test]
    fn turning_off_tsv_inference_reports_the_through_path_rather_than_assuming_it() {
        // Joining faces by matching net name is a convention, not a standard. A user whose maps do
        // not follow it must be able to say so, and get the conservative answer.
        let (n, c, m, b) = three_die(true);
        let f = analyze(&n, &c, &m, &b, &[], &Options { tolerance_um: None, tsv_inference: false }).findings;
        assert_eq!(f.iter().filter(|x| x.kind() == "severed").count(), 1);
        // The die still declares TSVs, and with inference off nothing crossed it.
        assert_eq!(f.iter().filter(|x| x.kind() == "tsv-unused").count(), 1);
    }

    #[test]
    fn a_net_ending_at_a_die_is_not_a_finding() {
        // The false positive that would make this checker useless. `n_local` runs from base to mid
        // and stops, which is what an interface net does — mid is the destination.
        let nodes = vec![
            node("u_base", "front", "front", "bs1", Some("n_local")),
            node("u_mid", "back", "back", "md_b1", Some("n_local")),
        ];
        let chips = vec![chip("u_base", false), chip("u_mid", false)];
        let mates = vec![Mate { bond: 0, top: 1, bottom: 0 }];
        let f = analyze(&nodes, &chips, &mates, &["bond0".into()], &[], &Options::default()).findings;
        assert!(f.is_empty(), "{:?}", f.iter().map(|x| x.message()).collect::<Vec<_>>());
    }

    #[test]
    fn two_bumps_carrying_one_net_on_one_face_are_the_same_net_not_a_split() {
        // A `netName` names a net in that die's own netlist, so two bumps carrying it are joined by
        // the die's routing whether or not both are bonded. Missing this reported a split net for
        // every wide bus and every supply — the false positive that would have made the checker
        // unusable on a real design.
        let nodes = vec![
            node("u_a", "front", "front", "a0", Some("n")),
            node("u_a", "front", "front", "a1", Some("n")),
            node("u_b", "back", "back", "b0", Some("n")),
        ];
        let chips = vec![chip("u_a", false), chip("u_b", false)];
        let mates = vec![Mate { bond: 0, top: 2, bottom: 0 }];
        let a = analyze(&nodes, &chips, &mates, &["bond0".into()], &[], &Options::default());
        assert!(a.findings.is_empty(), "{:?}", a.findings.iter().map(|x| x.message()).collect::<Vec<_>>());
        assert_eq!(a.groups, 1, "one net, one group — a1 rides along on the die's own routing");
    }

    #[test]
    fn the_same_net_name_on_two_unbonded_dies_is_two_nets_not_one_in_pieces() {
        // The bug this exists to prevent, found by running the checker on upstream's own
        // example.3dbx: it instantiates one chiplet twice, so every VDD, VSS and soc_io[n] appeared
        // to be a single net split in half, and the tool reported 38 violations on an assembly
        // where not one bonded surface carries a bump map. A netName is a name in ITS OWN die's
        // netlist; equality across unbonded dies means nothing.
        let nodes = vec![
            node("u_soc", "back_reg", "back", "bump_r0c0", Some("VDD")),
            node("u_soc_dup", "back_reg", "back", "bump_r0c0", Some("VDD")),
        ];
        let chips = vec![chip("u_soc", false), chip("u_soc_dup", false)];
        let f = analyze(&nodes, &chips, &[], &[], &[], &Options::default()).findings;
        assert!(f.is_empty(), "{:?}", f.iter().map(|x| x.message()).collect::<Vec<_>>());
    }

    #[test]
    fn a_lone_bump_is_not_judged_because_nothing_says_where_its_net_should_go() {
        // Which is also why there is no `dangling` kind: a bump with no counterpart on a bond is
        // exactly what check-d2d reports as `unmated`, and a bump on a surface no bond touches is
        // simply outside what the assembly describes.
        let nodes = vec![node("u_top", "back", "back", "tp_spare", Some("n_spare"))];
        let f = analyze(&nodes, &[chip("u_top", false)], &[], &[], &[], &Options::default()).findings;
        assert!(f.is_empty(), "{:?}", f.iter().map(|x| x.message()).collect::<Vec<_>>());
    }

    #[test]
    fn a_finding_an_unchecked_bond_could_explain_is_unresolved_not_severed() {
        // The stack is the severed one, but bond1 declared no bump map — so whether the net gets
        // out of the middle die's front face is not something this assembly says. Calling that a
        // defect would be an accusation the input does not support.
        let (n, c, m, _) = three_die(false);
        let unchecked = vec![Unchecked {
            bond: "bond1".into(),
            top: ("u_mid".into(), "front".into()),
            bottom: ("u_base".into(), "front".into()),
        }];
        let f = analyze(&n, &c, &m, &["bond0".into()], &unchecked, &Options::default()).findings;
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].kind(), "unresolved");
        assert!(!f[0].is_violation(), "missing input must not fail a build");
        assert_eq!(f[0].to_json()["blocking_bonds"], serde_json::json!(["bond1"]));
    }

    #[test]
    fn two_nets_joined_by_the_bonding_are_one_finding_naming_every_bond() {
        // check-d2d reports this per interface. A net shorted at three bonds is one wrong net, so
        // reporting it three times buries the fact that it is a single defect.
        let nodes = vec![
            node("u_a", "front", "front", "a0", Some("n0")),
            node("u_b", "back", "back", "b0", Some("n1")),
            node("u_a", "front", "front", "a1", Some("n0")),
            node("u_b", "back", "back", "b1", Some("n1")),
        ];
        let chips = vec![chip("u_a", false), chip("u_b", false)];
        let mates = vec![
            Mate { bond: 0, top: 1, bottom: 0 },
            Mate { bond: 1, top: 3, bottom: 2 },
        ];
        let f = analyze(&nodes, &chips, &mates, &["bond0".into(), "bond1".into()], &[], &Options::default()).findings;
        let merged: Vec<&Finding> = f.iter().filter(|x| x.kind() == "net-merged").collect();
        assert_eq!(merged.len(), 1, "one group, one finding");
        let j = merged[0].to_json();
        assert_eq!(j["nets"], serde_json::json!(["n0", "n1"]));
        assert_eq!(j["bonds"], serde_json::json!(["bond0", "bond1"]));
    }

    #[test]
    fn a_tsv_die_no_net_crosses_is_reported_but_does_not_fail_the_run() {
        let nodes = vec![
            node("u_base", "front", "front", "bs0", Some("n")),
            node("u_mid", "back", "back", "md0", Some("n")),
        ];
        let chips = vec![chip("u_base", false), chip("u_mid", true)];
        let mates = vec![Mate { bond: 0, top: 1, bottom: 0 }];
        let f = analyze(&nodes, &chips, &mates, &["bond0".into()], &[], &Options::default()).findings;
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].kind(), "tsv-unused");
        assert!(!f[0].is_violation(), "an observation about intent must not gate CI");
    }

    #[test]
    fn unnetted_bumps_are_counted_rather_than_judged() {
        // A supply bump legitimately carries no net name. Treating one as a dangling signal would
        // bury the real findings under the power delivery.
        let nodes = vec![
            node("u_a", "front", "front", "a0", None),
            node("u_b", "back", "back", "b0", None),
        ];
        let chips = vec![chip("u_a", false), chip("u_b", false)];
        let f = analyze(&nodes, &chips, &[], &[], &[], &Options::default()).findings;
        assert!(f.is_empty());
    }
}
