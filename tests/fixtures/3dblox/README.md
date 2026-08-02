# 3Dblox fixtures — vendored from OpenROAD

These files are copied **verbatim** from
[The-OpenROAD-Project/OpenROAD](https://github.com/The-OpenROAD-Project/OpenROAD),
`src/odb/test/data/`, at the commit this workspace pins (see
`vyges-opendb-lib/openroad-pin.yaml`).

- `example.3dbx`, `example.3dbv` — a two-chiplet assembly and its definitions
- `check_3dblox.ok` — the golden log their own linter test produces

**Licence:** BSD 3-Clause, Copyright (c) 2018-2025, The OpenROAD Authors. Retained here under
that licence; see `THIRD_PARTY_LICENSES.md` at the repository root.

## Why vendored rather than written

Every other 3D test in this crate runs on a design this crate built, so it can only show that we
agree with ourselves. These are files written by someone else, to a format we do not own — the
only way to find out whether the reader understood the format or merely accepted it.

`check_3dblox.ok` is not diffed directly: it is the output of their Tcl scenario, which also
loads the LEF/DEF collateral this reader deliberately skips. It is kept as the reference for what
their linter reports on this design, and the initial-state assertion in it — that the design is
clean before anything is moved — is the one `blox_read.rs` reproduces.

## `example.bmap` is ours, not upstream's

`example.3dbv` declares `bmap: example.bmap` on `back_reg`, and OpenROAD does not ship that file.
Reading their own example therefore always reported a loss — the reader said, correctly, that a
bump map it was told about could not be opened.

So this directory supplies one: an 8x8 field on a 100 um pitch, centred inside `back_reg`'s
955 x 1082 um outline, supply rails on the edge and signals in the middle. Plausible geometry, not
authoritative. With it, upstream's example loads completely and the only element still reported as
unrepresentable is the virtual bond (`bot: ~`), which genuinely has no counterpart to attach to.

The missing-bump-map path is still covered — `cli_3dblox.rs` deletes a map from the `d2d/` fixture
and asserts the loss is reported by name.
