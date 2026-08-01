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
