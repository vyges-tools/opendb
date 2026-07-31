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

## Build & test

```sh
cargo test
```

The first build compiles a standalone `libodb` via `vyges-opendb-lib` (which sparse-checks-out the
pinned OpenROAD subtree and builds it — see that crate for details). Deps: a C++20 compiler +
`cmake boost zlib abseil spdlog fmt`.

## Status

Read + ECO write path over the db core (v0). LEF/DEF/GDS I/O and richer traversal follow the
`vyges-opendb-lib` roadmap. OpenROAD is BSD-3-Clause; this crate is Apache-2.0 (see NOTICE).
