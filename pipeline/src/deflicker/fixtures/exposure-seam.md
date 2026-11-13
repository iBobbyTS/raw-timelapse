# Saved exposure seam fixture

`exposure-seam.bin` is a 488415-byte, data-only numerical fixture.
SHA-256: `e650283c67a88a7bfe106b58c49671b68dcb01ae70c1ae943578ffab66ee8692`.
It contains no image pixels, RAW data, paths or rendered previews. Source SHA-256
identities are retained for all 476 frames in native inventory order.

The source is the saved schema-2 12x8 signed-FP32/AP1 measurement sequence and
original uniform exposure plan from the November Canmore R7 timelapse. The
historical run-directory label does not identify a second dataset. First source
hash: `e28e90414fc921cdb1e4abc5bd33f4c33d6df8ceac1b8d855e7d86d052f6b2e4`.
The original report SHA-256 is
`a04dc86ecc8dd6ed2d335cf659bd1d70e2791a67fc56ab75c791e169ca29e286`.
The producer implementation at `4c2c5ecb3b86ff9e3bde936367a18e7aa1f44ad7`
was replayed twice on the full saved measurements: identical EV values and
exact agreement with the original report, 1,588 edges, 23 components,
zero persistent tiles and only ten render-target participants.

The fixture freezes the accepted primary edge producer, so it tests downstream
component placement rather than approximating fine-structure sample arrays.
Separate full-data replay and native pixel/encode controls verify the producer
and actual output; this fixture does not claim to test RAW development or
encoding. Primary scalar tile levels and validity remain exact parsed f64
values; edge deltas/weights are the actual producer outputs. No approximate
synthetic structure samples substitute for real measurements.

All integers and IEEE floats are little-endian, fields packed without padding:

- ASCII `EXPOSURE` (8 bytes), u32 frame count, u32 accepted edge count.
- For each frame: 64 ASCII source-hash bytes, then 96 entries of f64
  log-luminance and u8 validity. Invalid luminance is NaN.
- For each edge: u32 earlier frame, u32 later frame, f64 delta EV and f64 weight.
- For each adjacent pair: f64 strict structure-matched raw common change (NaN
  if unavailable), u8 reliable-common-change flag. These historical projections
  are retained as controls, not promoted to primary graph observations.
- For each frame: f64 original applied EV, u8 original render-target eligibility
  (the original `ABSOLUTE_LEVEL` frames; ten in total).

The embedded reader checks the final byte count. It rebuilds the primary graph,
checks the old four correction jumps, exercises non-contiguous membership,
and proves local projected residual <=0.02 EV for every adjacent pair after
joint placement. The old gains fail those same residual assertions. A separate
case perturbs independent component gauge constants and reverses numbering,
requiring identical physical adjacent differences within numerical tolerance.

Old correction differences at zero-based boundaries 45,49,95,211 are
-0.7336516109640834, +0.3946572881147092, +0.34470985938868587,
-0.21951951066982994 EV. These are regression controls, not per-index repair
instructions; production code contains no dataset indices or fixture loading.
