# rawtl-linear-video

Rust driver for the end-to-end linear time-lapse system: enumerate a directory
of CFA/LinearRaw DNG frames, call the accepted `dng-linear-frame` producer,
freeze the sequence white balance, run the legacy V4 deflicker node (enabled by
default), encode a lossless 16-bit scene-linear FFV1 video, validate it by full
decode roundtrip, audit the encoded result independently, render a
display-referred sRGB review preview, and publish both deliverables atomically.

The crate has no external Rust dependencies, no async runtime and no Python in
the product path. Child processes are invoked with `std::process::Command`
using argv vectors (no shell).

## Build

```sh
cd pipeline
cargo clean          # avoid stale build artifacts
cargo build --release
cargo test
```

The binary is `pipeline/target/release/rawtl-linear-video`.

## Run

```sh
rawtl-linear-video --input <DNG_DIR> --output <RUN_DIR> \
  [--fps 30] \
  [--dng-linear-frame /abs/path/to/dng-linear-frame] \
  [--ffmpeg /opt/homebrew/opt/ffmpeg-full/bin/ffmpeg] \
  [--ffprobe /opt/homebrew/opt/ffmpeg-full/bin/ffprobe] \
  [--converter "/Applications/Adobe DNG Converter.app/Contents/MacOS/Adobe DNG Converter"] \
  [--converter-timeout 120] \
  [--strength 1.0] [--period 60] [--brightness smooth|uniform] \
  [--estimator pairwise|legacy] [--ev-limits none|LOW,HIGH] \
  [--roi X,Y,W,H] [--exclude-roi X0,Y0,X1,Y1 ...] [--step START:EV ...] \
  [--temporal-quality report|off]
```

Defaults:

- `--dng-linear-frame native/build/dng-linear-frame` (the documented native
  build location; pass the matched build path when using a work-directory build)
- `--ffmpeg /opt/homebrew/opt/ffmpeg-full/bin/ffmpeg`
- `--ffprobe /opt/homebrew/opt/ffmpeg-full/bin/ffprobe`
- `--fps 30` — the frozen legacy default (`legacy/python/src/rawtl/cli.py:91`,
  `Fraction(30)`). Rational rates such as `30000/1001` are accepted; the
  positive-rate and `<= 1000` bounds match the legacy parser.
- Deflicker defaults (`legacy/python/src/rawtl/cli.py:95-114`, amended
  2026-11-11): strength 1.0, period 60, brightness smooth, estimator
  pairwise, unbounded EV, full-frame ROI, no exclusions or confirmed steps.
  The publication gates of the frozen legacy CLI (`on_review=error`,
  `temporal_quality=strict`) were removed by owner decision: an incomplete
  estimate falls back to the global-reference curve and the temporal audit
  records its verdict without blocking publication
  (`temporal_quality=report`). `--strength 0` disables the node entirely.

`--converter` and `--converter-timeout` are forwarded to the native producer;
by default the native tool uses the Adobe DNG Converter application path and
its own 120-second timeout.

`--output` must be absent or empty. Input and output must not be equal or
contain each other in either direction. The input directory is only read.

## Output layout

```
<output>/
  frames/frame_%06d.png   RGB16 scene-linear ACEScg/AP1/D60 (code/16384)
  frames/frame_%06d.json  native producer sidecar (route, WB provenance, range, ...)
  analysis/baseline/      per-frame 0 EV analysis sidecars (deflicker input)
  analysis/applied/       per-frame applied-EV analysis sidecars (accepted round)
  analysis/probe/         gain-probe measurements (when executed)
  logs/                   per-pass native, encoder, roundtrip and audit transcripts
  exposure_report.json    per-frame decisions, statuses, probe/audit summaries
  exposure-curve.csv      legacy write_curve fields, one row per frame
  temporal_audit.json     independent encoded-video audit (when executed)
  pairs.csv, frames.csv   audit pair/frame evidence
  manifest.json           run provenance, frozen WB, fps provenance, encoder
                          parameters, probe results, roundtrip/preview checks and
                          the deflicker block (config, status counts, audit summary)
  linear-acescg-16bit-ffv1.mkv
  srgb-preview-h264.mp4
```

Intermediate encoder outputs are written under `<output>/.rawtl-partial/` and
are removed after successful publication. If validation or encoding fails, the
directory is retained with that distinguishable name and the error message
names it; no file is ever created under a final deliverable name before
validation succeeds.

## Ordering

Frame enumeration is case-insensitive on the `.dng` extension. Names are
tokenised into alternating text and digit runs; text compares
case-insensitively and digit runs compare by numeric value, so `IMG_2` sorts
before `IMG_10`. Names without any digit run, names differing only by case, and
pairs whose keys differ only in digit spelling (`IMG_1` vs `IMG_01`) are
rejected as ambiguous. There is no lexicographic fallback.

## Sequence-frozen white balance (REQ-008)

The first numerically ordered frame is decoded with its own AsShotNeutral
(`white_balance_source: "as-shot"`). Its `applied_white_balance` triple is then
passed to every later frame as `--white-balance R,G,B` (camera-neutral domain,
not reciprocal gains). Each later sidecar must report
`white_balance_source: "override"` and exactly the same applied triple; a
mismatch stops the run. Every frame keeps its own `as_shot_neutral` for
provenance. No per-frame auto white balance is performed.

## Linear video

`ffmpeg -framerate <fps> -start_number 1 -i frames/frame_%06d.png -frames:v N
-c:v ffv1 -level 3 -g 1 -slicecrc 1 -pix_fmt gbrp16le -map_metadata -1
-f matroska`. No resize, frame-rate filter, colour conversion or metadata is
inserted. The values remain `code / 16384` scene-linear ACEScg with the
producer's two-sided clipping policy (negatives to 0, above 65535/16384 to
65535); the per-frame native sidecar records observed scene min/max and clip
counts.

Validation before publication:

1. `ffprobe -count_frames` must report `ffv1`, `gbrp16le`, the expected
   dimensions, frame count and frame rate.
2. Lossless roundtrip: both the sent PNG sequence and the encoded MKV are
   decoded by ffmpeg to `rgb48le`; the frames are compared byte-for-byte and
   the first differing frame/byte/channel/value is reported on failure.

## sRGB preview (declared colour contract)

The preview is display-referred and is not a measurement source:

1. `code / 16384` is scene-linear ACEScg (AP1, D60).
2. AP1/D60 → XYZ(D60) → Bradford CAT → XYZ(D65) → linear sRGB(D65), using the
   explicit matrices recorded in `manifest.json`:
   - AP1/D60 → XYZ/D60;
   - Bradford D60 → D65 (D60 `xy = 0.32168, 0.33767`, D65 `xy = 0.3127, 0.3290`);
   - XYZ/D65 → linear sRGB.
3. Per-channel linear sRGB is clipped to `[0, 1]` (negatives and out-of-gamut
   colours clip; no tone mapping or highlight roll-off).
4. The IEC 61966-2-1 sRGB transfer function is applied, then quantized to 8 bit
   with `round(value * 255)` (ties away from zero).

Conversion runs in Rust on frames decoded by ffmpeg from the sent PNG
sequence. ffmpeg then converts RGB24 to `yuv420p` with an explicit
`scale=out_color_matrix=bt709:out_range=tv` plus
`setparams=color_primaries=bt709:color_trc=iec61966-2-1:colorspace=bt709:range=tv`,
encodes with macOS VideoToolbox H.264
(`-c:v h264_videotoolbox -allow_sw 0 -profile:v high -b:v 0 -q:v 65`) and tags
`bt709`/`bt709`/`iec61966-2-1`/`tv`. The preview is validated with ffprobe
(`h264`, `yuv420p`, dimensions, frame count, frame rate) and a full decode to
`null`.

Unit tests assert neutral (`0.18 → 118,118,118`), a non-neutral reference
(`[0.5,0.25,0.125] → [216,129,89]`), the two-sided clipping policy, the sRGB
transfer, and that treating AP1 values as display sRGB is rejected by the
non-neutral test.

## Deflicker node (legacy V4)

The node is ported from the frozen `backup/main-legacy @ f6d3ae7`
(`src/rawtl/exposure/**`, `service.py`, `quality/{photometry,video_audit}.py`)
onto this synchronous CLI. Domain mapping is declared: the estimator consumes
the analysis projection `0.2126/0.7152/0.0722` over ACEScg AP1/D60 scene-linear
pixels (the same frozen WB as the delivery chain), not legacy's linear-sRGB
render. The two per-tile occupancy series in the analysis sidecar remain
distinct: `saturation_fraction` is the camera-domain gate (EV-invariant) and
`output_clip_fraction` is the delivered-domain clip series (EV-dependent).

Enabled path:

1. **Analysis pass** — every frame is decoded at 0 EV with
   `--no-png --emit-analysis`, using the accepted native producer and the
   unchanged sequence-frozen white balance. Only measurements are kept. The
   native metadata `exposure_ev` is checked at full precision; the analysis
   sidecar's `exposure_ev` is checked at the producer's 10-significant-digit
   decimal precision (exact 10-digit representation or half a decimal ulp), so
   fractional corrections do not trip the seam while materially wrong gains are
   still rejected.
2. **Continuity** — capture-time gaps come from the median nominal interval
   (`gap_factor 1.5`, at least 3 samples). Filename numeric jumps are report
   metadata only. Missing `capture_time` degrades coverage to `filename_only`
   and adds no audit boundaries.
3. **Estimation** — the S02 engine (`rawtl_linear_video::deflicker`) runs the
   whole-sequence V4 state machine: pairwise graph or legacy estimator, smooth
   trend or uniform target, confirmed steps, EV limits, scene-cut metadata and
   interpolated/lone-frame states.
4. **Fail-open review policy (global-reference fallback)** — any frame the
   estimator marks unreliable (`REVIEW_REQUIRED`, no `applied_ev`) is filled
   from the global-reference curve: the per-frame median valid-tile log2 level
   (the engine's `row_level` semantics), smoothed by the same
   Huber-Whittaker trend as the verified path (segment median for `uniform`
   intent), confirmed steps removed before smoothing, split at scene-cut /
   capture-time-gap boundaries, and offset-aligned to the nearest verified
   neighbour estimates so post-correction levels stay continuous across the
   seam. Filled frames carry status `GLOBAL_REFERENCE`, per-frame
   `GLOBAL_REFERENCE_FALLBACK` warnings and a report-level
   `global_reference_fallback` block (holes, anchors, degraded segments). A
   continuity segment without any usable measurement renders its frames at
   0 EV with `GLOBAL_REFERENCE_UNMEASURED` warnings. Publication is never
   blocked by an incomplete estimate. Provenance: the only element borrowed
   from the reverse-engineered LRTimelapse Visual Deflicker is the concept of
   a whole-frame reference curve as the estimator of last resort; measurement,
   smoothing and application are this system's own machinery (no rendered
   previews, no LOESS, no empirical logit calibration, no accuracy deadband).
5. **Gain probe** — skipped when disabled, shorter than 8 frames or without any
   estimated frame; otherwise representative frames (up to five) are analyzed
   at -0.25/+0.25 EV and the per-tile log response must match the requested EV
   (median |error| <= 0.02, P95 <= 0.05). A failed probe is a recorded review
   flag (`GAIN_PROBE_FAILED` warning, probe JSON verbatim), not a publication
   gate; probe-side tool failures remain operational errors (exit 81).
6. **Application pass** — frames are re-decoded with `--exposure-ev <applied>`
   and an analysis sidecar, writing the PNG/metadata pair plus applied-EV
   measurements. `new_output_clipping_fraction` is the per-tile positive
   increase of `output_clip_fraction` over the 0 EV baseline divided by tile
   count.
7. **Render validation and feedback** — the applied measurements are compared
   with the frozen targets (`median <= 0.01 EV`, `P95 <= 0.03 EV`, no severe
   frame) using one shared offset. At most two feedback updates change
   `applied_ev` only (damping 0.75, step 0.25); divergence aborts and keeps the
   best round. A best round that still fails is published with a
   `RENDER_TARGETS_NOT_REACHED` warning and `COMPLETED_WITH_REVIEW` — never
   blocked. All-unobservable validation is reported as
   `NOT_VERIFIED_NO_CONSENSUS` / `NOT_INDEPENDENT_LEGACY`, never as a pass.
8. **Encode / validate / roundtrip / preview** — unchanged from the linear
   pipeline above.
9. **Independent audit** — the temporary linear master is decoded by ffmpeg as
   `rgb48le`, converted with the declared `linear16` inverse transfer
   (`code / 16384`), area-reduced in linear light, and measured on a 24x18 tile
   grid with the same paired photometric mathematics as legacy. Audit
   boundaries are scene cuts union capture-time gaps. The delivered-code
   saturation proxy is `code >= int(0.965 * 65535)` — a literal port applied to
   the 16-bit delivered codes, deliberately not the sidecar's linear-scale
   `16057` series. The verdict is recorded verbatim (`temporal_audit.json`,
   manifest `audit_status`) and a `FAIL` adds a warning plus
   `COMPLETED_WITH_REVIEW`; publication is never blocked. `off` records
   `SKIPPED`. Observation failures are recorded as `UNOBSERVABLE`, never as a
   pass.
10. **Reports and publication** — `exposure_report.json` and
    `exposure-curve.csv` are written atomically before the manifest and
    publication. Any failure after the guarded output directory exists retains
    a report, including required-executable (exit 20) and source-enumeration
    (exit 10) failures before the first frame (the report may carry an empty
    frame list); guard/usage rejections happen before report initialization and
    produce none. A report-write failure on the failure path does not mask the
    original error. The manifest gains a `deflicker` block with config, status counts,
    probe and audit summaries. Atomic publication is unchanged.

Declared boundaries: the audit's linear16 analysis assumption (the stream has
no colour tags), legacy float32 area reduction versus this f64 reduction
(semantically equal, not bit-identical), and encoder byte determinism
(Matroska metadata is not stable; FFV1 payload identity is proven by the
lossless roundtrip and framemd5 diagnostics).

`--strength 0` is the REQ-011 regression path: a single pass with the exact
historical native argument shape (no analysis, probe, validation or audit),
`DISABLED` frame records in the report and a disabled manifest block. It
reproduces the pre-deflicker PNG/video content byte-for-byte.

## Failure semantics and exit codes

Any frame failure stops the run immediately: the failing frame name, exit code,
a stderr excerpt and the retained frame directory/log are reported; no video is
produced. A validated video is published by same-directory `rename`; on a
publication failure the already-renamed linear video is rolled back when
possible.

| Exit | Meaning |
| --- | --- |
| 0 | Both deliverables published |
| 2 | CLI usage error |
| 10 | Input enumeration/ordering error |
| 11 | Output/input overlap or invalid output path |
| 12 | Output directory is not empty |
| 20 | Required executable (native tool/ffmpeg/ffprobe) unavailable |
| 30 | Frame production or frame-metadata verification failure |
| 40 | FFV1 encoder failure |
| 50 | ffprobe validation or lossless-roundtrip failure |
| 60 | sRGB preview render/encode/validation failure |
| 70 | Manifest publication or final rename failure |
| 81 | Gain-probe tool failure (probe verdicts themselves are recorded flags) |
| 82 | Render-validation internal error (target misses are recorded flags) |

Deflicker estimate completeness, probe verdicts, render-target misses and the
encoded-audit verdict never stop a run: they are recorded as warnings, frame
statuses and manifest fields, and the deliverables are published as
`COMPLETED_WITH_REVIEW` (exit 0). The removed exit codes 80/83 belonged to the
frozen legacy gate contract (`--on-review error`, `--temporal-quality strict`).

## Tests

`cargo test` runs unit tests plus integration tests that use the real ffmpeg at
the default path and a fake native producer (a standalone Rust program compiled
once by the harness, writing 16-bit PNG fixtures, native metadata and analysis
sidecars). The producer covers numeric ordering (`IMG_1, IMG_2, IMG_10`),
frozen-WB propagation and drift detection, fail-fast, encoder failure after
bytes are written, ffprobe rejection, roundtrip mismatch and directory guards.
Deflicker scenarios cover injected alternating gain (corrected, audit passes),
confirmed steps (requested EV preserved; the legacy shared-offset render check
flags a genuine level step and the run publishes with the warning), EV limits
(`LIMITED` with requested != applied), render under-execution (best round
published with `RENDER_TARGETS_NOT_REACHED`), the global-reference fallback
(unreliable frames get `GLOBAL_REFERENCE` estimates and the run publishes), a
wrong correction that passes the gain probe while the audit records `FAIL`
(deliverables still published), and the `--strength 0` regression path
(fixture-equal PNGs, identical FFV1 content and encoder arguments).
Real-material runs are opt-in:

```sh
pipeline/target/release/rawtl-linear-video \
  --input /Users/ibobby/Temp/tl \
  --output outputs/linear-video-pipeline-e2e \
  --dng-linear-frame <matched dng-linear-frame build>
```

## Non-goals

No plugin ABI, events, Stage 1 cache, official Log output, GPU, packaging or
Python runtime. The deflicker node uses no Python/SciPy/rawpy. Multi-ROI audit,
chroma-variation analysis and progress callbacks remain out of scope. The frozen
legacy Python implementation remains read-only and is not a numerical oracle for
this ACEScg chain.
