# Die-to-die fixture — ours, not upstream's

`../example.{3dbv,3dbx}` are copied verbatim from OpenROAD and cannot exercise the die-to-die
check: the one `bmap:` in it sits on `back_reg`, while its only real connection bonds `front_reg`
to `front_reg`, and the `example.bmap` it names is not shipped.

So this assembly is authored here. Two dies bonded face to face:

- `logic` is placed `R0` at (0, 0)
- `mem` is placed **`MZ_MY`** at (0, 0) — flipped so its front faces down, *and* mirrored in X,
  which is what a real face-to-face bond does. `MZ` alone would flip the face and leave the bump
  field in the wrong handedness; that distinction was measured against odb's own unfolded global
  positions, and it is exactly the mistake `check-d2d --input` exists to stop a user making.

`mem_front.bmap` is the correct counterpart. `mem_front_broken.bmap` is the same interface with
four planted defects — one unmated bump, one 1 nm misalignment, one swapped net pair, one bump
cell mismatch.
