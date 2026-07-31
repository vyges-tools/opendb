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

The ODB 3D-IC schema is exposed alongside the flat one: `dbChip` (keyed by chip name) and
`dbChipInst` (keyed by **parent chip + inst name** — inst names are unique only within their
parent), reachable through the same generic `get` / `fields` surface as everything else.

```sh
vyges-opendb get -i stack.odb --class dbChip --field get_chip_type --key stack
# "HIER"

# a chip inst takes both keys, parent first
vyges-opendb get -i stack.odb --class dbChipInst --field get_loc_z --key stack --key u_top
# 3000
```

Two behaviours worth knowing before you rely on them:

- **Chip type comes back UPPERCASE** — `DIE`, `RDL`, `IP`, `SUBSTRATE`, `HIER` — matching the
  rest of our enums (`dbSigType` → `"SIGNAL"`) and OpenROAD's Python bindings. odb ships no
  `getString()` for this type, so the mapping is generated. **The 3Dblox *file* format spells
  these lowercase** (`die`, `hier`); that is the `.3dbv` writer's representation, not the
  database API's, so the two differ deliberately. Anything reading or writing 3Dblox files
  needs the lowercase form — don't reconcile them.
- **`dbChipInst` is read-only, on purpose.** `dbChipInst::setLoc` is orientation-dependent: it
  stores a delta computed against the orientation current at the time of the call, and
  `get_loc_*` re-applies whatever the orientation is now. Re-orienting a placed chip inst
  silently moves it, and the location setter is not marshallable, so a caller could not undo
  it. Rather than ship half of a coupled pair, the write surface withholds both. See the 3D
  section of the [`vyges-opendb-lib` README](https://github.com/vyges-tools/opendb-lib) for
  the full detail and the TODO to expose them together.

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
