# Through-stack net fixture — ours, not upstream's

Three dies, `u_base` → `u_mid` → `u_top`, all `R0` so the bump fields coincide without any
mirroring. Orientation is not what this fixture is testing — `../d2d/` already covers that — so it
is held constant to isolate the one variable that matters here.

Net `n_thru` runs from `u_base` to `u_top`, which means it has to cross `u_mid`. Net `n_local` runs
from `u_base` to `u_mid` and stops, which is what an ordinary interface net does.

Two assemblies differ in exactly one thing:

- `stack_notsv.3dbx` instantiates `mid_notsv` (`tsv: false`). `n_thru` lands on both faces of the
  middle die and nothing carries it between them, so the net is **severed**. Every bond is
  perfectly mated and correctly netted, so `check-d2d` reports clean on each one and upstream's
  `check_3dblox` reports clean on all of it.
- `stack_tsv.3dbx` instantiates `mid_tsv` (`tsv: true`), and is otherwise identical — same bump
  maps, same placements. This is the control: without it the severed case would only prove that
  the checker fires, not that it fires for the right reason.

`n_local` is in both, and must stay clean in both. A net that ends at a die is not a defect, and a
checker that reported one would be useless on any real stack.
