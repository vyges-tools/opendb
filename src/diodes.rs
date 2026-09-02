//! Antenna-diode insertion — `Odb.FuzzyDiodePlacement` / `Odb.PortDiodePlacement`.
//!
//! Transcribed from LibreLane `scripts/odbpy/diodes.py` (`DiodeInserter`).
//!
//! ⛔ **This performs NO antenna analysis.** No ratio is computed and no LEF antenna rule is read.
//! It is a **geometric heuristic**: a net whose bounding-box half-perimeter exceeds a threshold
//! gets a diode on each of its instance pins. Deciding a diode from a *computed* ratio is
//! `OpenROAD.RepairAntennas`, a different step. This distinction cost this programme a whole
//! "next engine" argument — these steps were scheduled as `ant` work for weeks on the strength of
//! their names.
//! ✅ **CORRELATED 2026-09-02 — 648 of 648 diodes identical.** On `skywater130_caravel`'s golden
//! DEF (1731 rows, 648 diodes at `--threshold 50 --side-strategy source --port-protect in`), every
//! diode matches LibreLane 2.4.6 by NAME, COORDINATE and ORIENTATION, with none extra on either
//! side.
//!
//! ⚠️ The only apparent difference was notation: we report odb's `R0`, the DEF writes `N`. Same
//! orientation, different spelling — normalising the DEF tokens (`N`→`R0`, `FS`→`MX`, …) makes it
//! exact. A comparison that skipped that step would have reported 648 failures.
//!
//! ℹ️ **Version provenance.** Correlated against `2.4.6`. `diodes.py` is byte-identical in `3.0.2`
//! except that `--threshold` became `required=True` there, so the *"200 × minimum site width"*
//! default this engine applies is `2.4.6`'s behaviour; `3.0.2` moved that decision up into the
//! step's config. Applying it here is a convenience, not a divergence in the algorithm — pass
//! `--threshold` and both versions agree exactly.
//!
//! ℹ️ **The reference under-reports its own work.** It printed *"Inserted 0 diodes"* for this run
//! while creating 648, because its counter is only written by the std-cell path and every target
//! here is a pad macro. Ours counts the plan.
use crate::Db;

/// Split an `inst/pin` entry as [`Db::net_iterms`] returns it.
///
/// ⚠️ **Split at the LAST `/`.** Instance names are hierarchical (`top/u_cpu/u_reg`), so splitting
/// at the first separator would name a nonexistent instance and silently find no master.
fn split_iterm(s: &str) -> Option<(&str, &str)> {
    s.rsplit_once('/')
}

/// Which port polarities force a diode regardless of span — the reference's `--port-protect`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PortProtect {
    None,
    In,
    Out,
    Both,
}

impl PortProtect {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "none" => Self::None,
            "in" => Self::In,
            "out" => Self::Out,
            "both" => Self::Both,
            _ => return None,
        })
    }
    fn polarities(self) -> &'static [&'static str] {
        match self {
            Self::None => &[],
            Self::In => &["INPUT"],
            Self::Out => &["OUTPUT"],
            Self::Both => &["INPUT", "OUTPUT"],
        }
    }
}

/// Which side of the target instance a diode goes on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SideStrategy {
    Source,
    Pin,
    Balanced,
    Random,
}

impl SideStrategy {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "source" => Self::Source,
            "pin" => Self::Pin,
            "balanced" => Self::Balanced,
            "random" => Self::Random,
            _ => return None,
        })
    }
}

/// One diode the placer decided on.
#[derive(serde::Serialize, Debug, Clone, PartialEq)]
pub struct Diode {
    pub name: String,
    pub net: String,
    pub target: String,
    pub pin: String,
    pub x: i32,
    pub y: i32,
    pub orient: String,
}

/// `(max(y) - min(y)) + (max(x) - min(x))` over the net's pins.
///
/// ⚠️ **Despite the reference's name this is a bounding-box HALF-PERIMETER, not a Manhattan
/// distance between two points.** Transcribed as-is; the name is theirs.
fn net_span(db: &Db, net: &str) -> i64 {
    let (mut xs, mut ys) = (Vec::new(), Vec::new());
    for bt in db.net_bterms(net) {
        if let Some((x, y)) = db.bterm_first_pin_location(&bt) {
            xs.push(x as i64);
            ys.push(y as i64);
        }
    }
    for e in db.net_iterms(net) {
        let Some((inst, pin)) = split_iterm(&e) else { continue };
        let (x, y) = pin_position(db, inst, pin);
        xs.push(x as i64);
        ys.push(y as i64);
    }
    if xs.is_empty() {
        return 0;
    }
    let (xmax, xmin) = (*xs.iter().max().unwrap(), *xs.iter().min().unwrap());
    let (ymax, ymin) = (*ys.iter().max().unwrap(), *ys.iter().min().unwrap());
    (ymax - ymin) + (xmax - xmin)
}

/// A pin's position — its average XY, or the INSTANCE ORIGIN when odb cannot give one.
///
/// ⚠️ The fallback is `getLocation()`, the instance's origin, which the reference's comment calls
/// "the center coordinate of the instance". It is not the centre, and transcribing the code rather
/// than the comment is the point.
fn pin_position(db: &Db, inst: &str, pin: &str) -> (i32, i32) {
    db.iterm_avg_xy(inst, pin).unwrap_or_else(|| db.inst_location(inst))
}

/// The net's driver position: first INPUT port with a placed pin, else first output-ish iterm.
fn net_source(db: &Db, net: &str) -> Option<(i32, i32)> {
    // 🔑 **Ports are searched BEFORE instance pins**, and the first hit wins in each. Reordering
    // this changes which side `SideStrategy::Source` puts diodes on.
    for bt in db.net_bterms(net) {
        if db.bterm_get_io_type(&bt) != "INPUT" {
            continue;
        }
        if let Some(p) = db.bterm_first_pin_location(&bt) {
            return Some(p);
        }
    }
    for e in db.net_iterms(net) {
        let Some((inst, pin)) = split_iterm(&e) else { continue };
        if !db.iterm_is_output_signal(inst, pin) {
            continue;
        }
        if let Some(p) = db.iterm_avg_xy(inst, pin) {
            return Some(p);
        }
    }
    None
}

fn has_bterm_of(db: &Db, net: &str, kinds: &[&str]) -> bool {
    db.net_bterms(net).iter().any(|bt| kinds.contains(&db.bterm_get_io_type(bt).as_str()))
}

/// Is a diode already on this net? Matched by MASTER NAME and PIN NAME, as the reference does.
fn net_has_diode(db: &Db, net: &str, diode_cell: &str, diode_pin: &str) -> bool {
    db.net_iterms(net)
        .iter()
        .any(|e| match split_iterm(e) {
            Some((inst, pin)) => db.inst_master(inst) == diode_cell && pin == diode_pin,
            None => false,
        })
}

/// Where a diode goes beside a STANDARD-CELL target, and how repeats stack outward.
///
/// `inserted` counts diodes already placed on each `(instance, side)`, so a second diode on the
/// same side sits one diode-width further out. ⚠️ **The reference keys this by instance and side
/// only** — two different pins of the same instance share a counter.
#[allow(clippy::too_many_arguments)]
fn place_stdcell(
    db: &Db,
    inst: &str,
    px: i32,
    src: Option<(i32, i32)>,
    strategy: SideStrategy,
    diode_width: i32,
    inserted: &mut std::collections::HashMap<(String, char), i32>,
    rng: &mut u64,
) -> (i32, i32, String) {
    let inst_pos = db.inst_location(inst);
    let inst_width = db.master_get_width(&db.inst_master(inst)) as i32;

    let mut pos: Option<char> = None;
    match strategy {
        SideStrategy::Source => {
            if let Some(s) = src {
                pos = Some(if s.0 < inst_pos.0 { 'l' } else { 'r' });
            }
        }
        SideStrategy::Pin => {
            // ⚠️ Integer halving, as the reference's `inst_width // 2`.
            pos = Some(if px < inst_pos.0 + inst_width / 2 { 'l' } else { 'r' });
        }
        SideStrategy::Balanced => {
            let th_left = inst_pos.0 + (inst_width as f64 * 0.25) as i32;
            let th_right = inst_pos.0 + (inst_width as f64 * 0.75) as i32;
            if px < th_left {
                pos = Some('l');
            } else if px > th_right {
                pos = Some('r');
            } else if let Some(s) = src {
                pos = Some(if s.0 < inst_pos.0 { 'l' } else { 'r' });
            }
        }
        SideStrategy::Random => {}
    }

    let pos = pos.unwrap_or_else(|| {
        // ⚠️ A coin toss, exactly where the reference has one. Deterministic here (xorshift on a
        // caller-held seed) because a gate cannot score a placer that answers differently each
        // run — the reference's `random.random()` is seeded from the clock.
        *rng ^= *rng << 13;
        *rng ^= *rng >> 7;
        *rng ^= *rng << 17;
        if *rng % 2 == 0 { 'l' } else { 'r' }
    });

    let n = *inserted.get(&(inst.to_string(), pos)).unwrap_or(&0);
    let dx = if pos == 'l' {
        inst_pos.0 - diode_width * (1 + n)
    } else {
        inst_pos.0 + inst_width + diode_width * n
    };
    inserted.insert((inst.to_string(), pos), n + 1);
    (dx, inst_pos.1, db.inst_get_orient(inst))
}

/// Where a diode goes when the target is a MACRO: the closest point on any row.
///
/// ⚠️ **`dy` is the row's `yMin`, never its centre**, and the distance is measured to that yMin —
/// so a tall row is "closer" than its geometry suggests. Transcribed as written.
fn place_macro(db: &Db, px: i32, py: i32) -> Option<(i32, i32, String)> {
    let mut best: Option<(i64, i32, i32, String)> = None;
    for i in 0..db.num_rows().unwrap_or(0) {
        let Ok(Some((bbox, _, orient))) = db.nth_row(i) else { continue };
        let (xmin, ymin, xmax) = (bbox[0], bbox[1], bbox[2]);
        let dx = px.clamp(xmin, xmax);
        let dy = ymin;
        let d = (px as i64 - dx as i64).abs() + (py as i64 - dy as i64).abs();
        if best.as_ref().is_none_or(|b| b.0 > d) {
            best = Some((d, dx, dy, orient));
        }
    }
    best.map(|(_, x, y, o)| (x, y, o))
}

/// Options for [`plan`], mirroring the reference's CLI.
pub struct Options {
    pub diode_cell: String,
    pub diode_pin: String,
    pub threshold_dbu: i64,
    pub side_strategy: SideStrategy,
    pub port_protect: PortProtect,
    pub seed: u64,
}

/// Decide every diode, in the reference's order, without touching the database.
///
/// 🔑 **`execute()`'s call sequence IS the behaviour**, and each guard below is a `continue` in the
/// reference, in this order:
///
/// 1. skip `isSpecial` nets;
/// 2. skip nets that already carry a diode;
/// 3. find the source position (used only to pick a side);
/// 4. ⛔ **if the net touches ANY INPUT/OUTPUT port, it is skipped UNLESS that port's polarity is
///    in `--port-protect`** — so `--port-protect none` skips every I/O net whatever its span, and
///    the default `in` skips output-only ports. This guard runs BEFORE the span test;
/// 5. skip if span < threshold **and not** forced by (4);
/// 6. otherwise a diode on EVERY iterm of the net.
pub fn plan(db: &Db, opts: &Options) -> Vec<Diode> {
    let mut out = Vec::new();
    let mut inserted: std::collections::HashMap<(String, char), i32> = Default::default();
    let mut taken: std::collections::HashSet<String> =
        (0..db.num_insts()).map(|i| db.nth_inst_name(i)).collect();
    let mut rng = opts.seed | 1;
    let diode_width = db.master_get_width(&opts.diode_cell) as i32;
    let diode_site = db.master_get_site(&opts.diode_cell);

    for net in db.net_names() {
        if db.net_is_special(&net) {
            continue;
        }
        if net_has_diode(db, &net, &opts.diode_cell, &opts.diode_pin) {
            continue;
        }
        let src = net_source(db, &net);

        let mut forced = false;
        if has_bterm_of(db, &net, &["INPUT", "OUTPUT"]) {
            forced = has_bterm_of(db, &net, opts.port_protect.polarities());
            if !forced {
                continue;
            }
        }
        if net_span(db, &net) < opts.threshold_dbu && !forced {
            continue;
        }

        for e in db.net_iterms(&net) {
            let Some((inst, pin)) = split_iterm(&e) else { continue };
            let (px, py) = pin_position(db, inst, pin);
            let placed = if db.master_get_site(&db.inst_master(inst)) == diode_site {
                Some(place_stdcell(db, inst, px, src, opts.side_strategy, diode_width,
                                   &mut inserted, &mut rng))
            } else {
                place_macro(db, px, py)
            };
            let Some((dx, dy, orient)) = placed else { continue };

            // The reference's name, with the same `_N` suffixing when one already exists.
            let base = format!("ANTENNA_{inst}_{pin}");
            let mut name = base.clone();
            let mut counter = 0;
            while taken.contains(&name) {
                counter += 1;
                name = format!("{base}_{counter}");
            }
            taken.insert(name.clone());
            out.push(Diode { name, net: net.clone(), target: inst.to_string(),
                             pin: pin.to_string(), x: dx, y: dy, orient });
        }
    }
    out
}
