# Third-party licenses

The `vyges-opendb` binary statically links the following components. Their versions track
the pins in [`vyges-tools/opendb-lib`](https://github.com/vyges-tools/opendb-lib)
(`openroad-pin.yaml`); this file is included in every release archive for binary-distribution
compliance. Full upstream license texts are at the linked repositories.

| Component | Version (pinned) | License | Upstream |
|-----------|------------------|---------|----------|
| OpenROAD OpenDB (`libodb`) | pinned SHA (26Q3) | BSD-3-Clause | https://github.com/The-OpenROAD-Project/OpenROAD |
| fmt        | 11.0.2      | MIT        | https://github.com/fmtlib/fmt |
| spdlog     | 1.15.3      | MIT        | https://github.com/gabime/spdlog |
| Abseil     | 20250127.0  | Apache-2.0 | https://github.com/abseil/abseil-cpp |
| yaml-rust2 | 0.10        | MIT/Apache-2.0 | https://github.com/Ethiraric/yaml-rust2 |
| zlib (dynamic) | system  | zlib       | https://zlib.net |

`vyges-opendb` itself is Apache-2.0 (see `LICENSE`). OpenROAD-derived code (`libodb`) is
BSD-3-Clause; its copyright notice and license text are reproduced per that license — see
`NOTICE` and the `OPENROAD-LICENSE-BSD3.txt` shipped in the `vyges-tools/opendb-lib` libodb bundle.

## Vendored test data

`tests/fixtures/3dblox/example.3dbx`, `example.3dbv` and `check_3dblox.ok` are copied verbatim
from OpenROAD's `src/odb/test/data/` and remain under **BSD-3-Clause, Copyright (c) 2018-2025,
The OpenROAD Authors**. They are third-party inputs used to check that this crate's 3Dblox reader
understands a file it did not write. See `tests/fixtures/3dblox/README.md`.
