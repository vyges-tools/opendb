# vyges-opendb

A safe, ergonomic Rust API over OpenROAD's OpenDB (`libodb`), built on the low-level
[`vyges-opendb-lib`](https://github.com/vyges-tools/opendb-lib) FFI — no Tcl, no SWIG, no
OpenROAD engines. Runs on linux/x86_64, linux/arm64, and macOS/Apple Silicon.

> Part of Vyges Loom. This is the layer Loom steps and the ECO applier use — idiomatic Rust
> over the in-memory design database, with the OpenROAD engines driven separately.

## API

```rust
use vyges_opendb::Db;

let mut db = Db::open("design.odb")?;
println!("{} — {} insts", db.block_name(), db.num_insts());

// ECO: insert a buffer on a pin (legalization delegated to the engines separately)
let buf = db.find_master("buf");
db.insert_buffer("inst42", "A", &buf, "eco_buf0", 10_000, 10_000)?;

db.write("design_eco.odb")?;
```

- `&self` for reads, `&mut self` for edits — the borrow checker enforces no read-while-mutate.
- Errors are typed (`vyges_opendb::Error`) and carry the OpenDB message.
- Write primitives: `create_net`, `create_inst`, `set_inst_location`, `connect`, `disconnect`,
  plus the composed `insert_buffer` — the `InsertECOBuffers` building blocks.

## 3D / chiplet reads

The ODB 3D-IC schema is exposed alongside the flat one, through the same generic `get` /
`fields` surface as everything else: chips and chip insts, bonding regions, bumps, 3D
connections/nets/paths, and the **unfolded** model — the stack flattened to absolute
positions, where a bump reports its real x/y/z in the assembly.

Keys follow the hierarchy: a `dbChip` by name, a `dbChipInst` by **parent chip + inst** (inst
names are unique only within their parent), a `dbChipBump` by chip + region + index, and the
unfolded classes by the slash-joined chip-inst path.

```sh
vyges-opendb get -i stack.odb --class dbChip --field get_chip_type --key stack
# "HIER"

# a chip inst takes both keys, parent first
vyges-opendb get -i stack.odb --class dbChipInst --field get_loc_z --key stack --key u_top
# 3000

# a bump's ABSOLUTE position in the assembled stack (unfolded: path, region idx, bump idx)
vyges-opendb get -i stack.odb --class dbUnfoldedChipBumpInst \
  --field get_global_position_z --key u_top --key 0 --key 0
```

Three behaviours worth knowing before you rely on them:

- **Chip type comes back UPPERCASE** — `DIE`, `RDL`, `IP`, `SUBSTRATE`, `HIER` — matching the
  rest of our enums (`dbSigType` → `"SIGNAL"`) and OpenROAD's Python bindings. odb ships no
  `getString()` for this type, so the mapping is generated. **The 3Dblox *file* format spells
  these lowercase** (`die`, `hier`); that is the `.3dbv` writer's representation, not the
  database API's, so the two differ deliberately. Anything reading or writing 3Dblox files
  needs the lowercase form — don't reconcile them.
- **The unfolded classes answer straight after `open`,** even though they are derived and never
  stored — OpenDB rebuilds them on read whenever the database holds more than one chip. They
  come back empty if the database's top chip is not the assembly, since that is where the
  builder starts.
- **Place chip insts with `place_chip_inst`, not the raw setters.** `dbChipInst::setLoc` is
  orientation-dependent: it stores a delta computed against the orientation current at the time
  of the call, and `get_loc_*` re-applies whatever the orientation is now — so placing a chip
  and *then* re-orienting it silently moves it, with no error.
  `db.place_chip_inst(chip, inst, orient, x, y, z)` orients first and then places, so the
  location reads back exactly as passed. (Under `--features gen-write`.)

## 3D structural sign-off

Three commands: read an assembly, check it, look at it.

```sh
vyges-opendb read-3dblox  -i stack.3dbx -o stack.odb   # 3Dblox assembly -> database
vyges-opendb check-3dblox -i stack.odb                 # database -> findings
vyges-opendb view-3dblox  -i stack.3dbx -o stack.svg   # -> cross-section + plan drawing
```

`read-3dblox` reads a **3Dblox** file — the 2.5D/3D interchange format, `.3dbx` for the assembly
and `.3dbv` for the chiplet definitions it includes — and builds the corresponding chips, regions
and die-to-die connections.

What it cannot represent, it **names**:

```
read-3dblox: 1 element(s) the database cannot represent:
  connection soc_to_virtual (virtual, no bottom)
```

Three known losses, all reported rather than dropped: virtual bonds (`bot: ~`) have no bottom die
to attach to; a non-rectangular region collapses to its bounding rectangle; and a stack whose dies
are on different processes cannot be fully held, because a database carries **one** technology.
That last one is upstream's limit, not ours, and it is the reason this is honest about being an
assembly *description* rather than a heterogeneous stack.

Requires a build with `--features gen-write` — constructing an assembly goes through the L2/write
surface. Released binaries are built with it.

### The linter

`check-3dblox` runs OpenDB's 3D linter over a chiplet assembly — logical connectivity, floating
chips, overlapping dies, unused `internal_ext` regions, connection-region overlap and
mating-surface gap versus connection thickness, bump alignment, and alignment markers.

```sh
vyges-opendb check-3dblox -i stack.odb
```

```json
{ "violations": 2,
  "categories": [
    { "category": "Floating chips", "count": 1,
      "markers": [ { "name": "u_base", "comment": "Isolated chip set starting with u_base" } ] },
    { "category": "Connection regions", "count": 1,
      "markers": [ { "name": "stack:bond0, u_base.regions.back",
                     "comment": "Invalid connection bond0: u_top/front (faces BOTTOM) to u_base/back (faces BOTTOM)" } ] } ] }
```

The report is self-contained — findings, not just counts — because the markers live in the
in-memory database and are never written back, so a second command could not fetch them.
OpenDB's own `[WARNING ODB-nnnn]` lines go to **stderr**, leaving stdout parseable.

From Rust the same detail is reachable through the ordinary marker accessors — no new read path
— addressed by a slash path, since the 3D categories nest under a top category on the chip:

```rust
let db = Db::open("stack.odb")?;
if db.check_3dblox()? > 0 {
    let why = registry::get(&db, "dbMarker", "get_comment",
                            &["3DBlox/Connection regions".into(), "0".into()])?;
}
```

It is a **checker, not a repairer**: it annotates the in-memory database with markers and never
modifies the design, so `Db::check_3dblox` takes `&self` and nothing is persisted unless you
write the database out. Re-running is idempotent.

### The drawing

The linter says *what* is wrong. `view-3dblox` says *where* — one self-contained SVG, no server
and no GUI toolkit, which opens in a browser, commits to a repo and embeds in a report.

```sh
vyges-opendb view-3dblox -i stack.3dbx -o stack.svg    # or -i stack.odb --top <chip>
vyges-opendb view-3dblox -i stack.3dbx -o stack.png --scale 2
```

The output extension picks the format. **SVG** is exact and diffable, so it is what belongs in a
repo or a CI artifact; **PNG** is what you paste into a slide, a web page or a message. Both come
from the same scene, so they cannot drift into being pictures of different things.

![Example assembly drawing](docs/example-stack.png)

*An interposer, an offset logic die, and a flipped memory die — regenerate with
`cargo run --features gen-write --example draw_stack`. The memory die sits above the bond plane,
which is why the linter reports it floating; the drawing is how you see that at a glance.*

It draws two views, because one is not enough. A **plan** view shows footprints and overhang; it
cannot show stacking order, die thickness, bond gaps, or **which face is bonded** — and those are
the entire subject of an assembly. So the primary view is the **cross-section**, the drawing a
package engineer reads, with the plan below it and any linter findings listed underneath.

A die at `MZ` is flipped: its FRONT faces down. Drawing it like an `R0` die would be a plausible
picture of the wrong assembly, so flipped dies are labelled and their front edge drawn on the
correct side.

**Why this is small when a layout viewer is not.** A routed block is millions of polygons, which
is why viewing one needs a tile server and a raster pyramid. An assembly is a handful of dies,
each a box — tens of rectangles. Different problem, three orders of magnitude apart.

The Z axis is scaled to fit the page and **prints its own factor** (`exaggerated 12×` /
`compressed 0.45×`). Every package cross-section is drawn with a non-uniform Z scale; the
difference is saying so on the drawing, so nobody measures a bond gap off the picture.

Both 3D construction commands need `--features gen-write`. Released binaries have it.

## Die-to-die interface checking

A 2.5D/3D assembly lives or dies on whether the bumps on two mating faces line up and carry the
same signals. `check-d2d` compares two **bump maps** — the `.bmap` files a 3Dblox `.3dbv` points
at — and reports what does not agree.

```sh
vyges-opendb check-d2d --top logic.bmap --bottom interposer.bmap \
    --offset-x -120.5 --flip-x
```

```json
{ "violations": 5,
  "by_kind": { "unmated": 1, "misaligned": 1, "net-mismatch": 2, "cell-mismatch": 1 },
  "top_bumps": 4, "bottom_bumps": 3, "matched": 3,
  "tolerance_um": 19.9995, "tolerance_source": "derived from bump pitch",
  "transform": { "dx_um": -120.5, "dy_um": 0.0, "flip_x": true },
  "findings": [
    { "kind": "net-mismatch",
      "message": "bt1 carries d2d_tx1 but mates with bb1 carrying d2d_tx2" },
    { "kind": "unmated",
      "message": "top bump bt3 (d2d_tx3) at (10.000, 50.000) has no mating bump on the bottom die" } ] }
```

### Why this is not already covered

`check-3dblox` has a `Logical Connectivity` check aimed at the same thing, and its inner loop is:

```cpp
auto it = bot_bumps.find(p);   // std::map<Point,...>, exact integer-DBU equality
if (it == bot_bumps.end()) {
    continue;                  // no bump at that exact point -> skipped, silently
}
```

It compares only pairs that already coincide exactly, and skips anything without a counterpart.
Its sibling `checkNetConnectivity` is an empty function body. Measured on assemblies built for the
purpose — the numbers come from `cargo run --features gen-write --example d2d_gap`, not from
reading the source:

| interface | `check-3dblox` | `check-d2d` |
| --- | --- | --- |
| a top bump with **no mating bump at all** | 0 | **1** |
| everything mated and exactly aligned | 0 | 0 |
| a mating pair off by **1 DBU** (1 nm) | 0 | **1** |
| a mating pair off by **5 µm** | 0 | **2** |

Rows 1, 3 and 4 are dead or mis-wired silicon and all report clean today. Row 2 is the control.

### What it checks

- **unmated** — a bump with no counterpart. A signal that leaves one die and arrives nowhere.
- **misaligned** — a pair close enough to be intended mates, but not coincident; the distance is
  reported. These are exactly what upstream skips.
- **net-mismatch** — mated bumps carrying different net names. The interface is wired to the
  wrong signal.
- **cell-mismatch** — a microbump mating with a C4.

Both sides are walked, so an unmated *bottom* bump is reported too — it is just as dead.

### Two things it deliberately does not do

**It does not infer the placement.** Two bump maps are each in their own die's coordinates, and
nothing in the files says how the dies sit relative to each other. Pass `--offset-x` / `--offset-y`
(microns) and `--flip-x` for a face-to-face bond. The transform used is echoed in every report,
because "no violations" means nothing without knowing what frame it was computed in — and a
checker that guessed an alignment and then declared everything matched would be worse than none.

**It does not pick a tolerance out of the air.** The default match radius is half the smaller of
the two bump pitches, derived from the maps themselves: anything nearer to a bump than that is
nearer to it than to its neighbour, so a match cannot be ambiguous. `--tolerance` overrides, and
the report says which was used. With fewer than two bumps there is no pitch to derive, so it
requires coincidence and says so.

A malformed line does not abort the file — a bump map is machine-generated but hand-edited often
enough, and refusing to check 4,095 good bumps because line 812 has five columns would make the
tool useless exactly when it is most needed. Bad lines are reported, and their bumps are not
counted as checked.

## Build & test

```sh
cargo test
```

The first build compiles a standalone `libodb` via `vyges-opendb-lib` (which sparse-checks-out the
pinned OpenROAD subtree and builds it — see that crate for details). Deps: a C++20 compiler +
`cmake boost zlib abseil spdlog fmt`.

## Status

Read + ECO write path over the db core (v0), plus a 3D/chiplet path: read a 3Dblox assembly,
lint it, draw it. LEF/DEF/GDS I/O and richer traversal follow the
`vyges-opendb-lib` roadmap. OpenROAD is BSD-3-Clause; this crate is Apache-2.0 (see NOTICE).
