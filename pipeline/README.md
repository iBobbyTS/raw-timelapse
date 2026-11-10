# rawtl-linear-video

Rust driver for the end-to-end linear time-lapse system: enumerate a directory
of DNG frames (LinearRaw or CFA) or of any other RAW format the Adobe DNG
Converter can parse, call the accepted `dng-linear-frame` producer,
freeze the sequence white balance, run the legacy V4 deflicker node (enabled by
default), encode a lossless 16-bit scene-linear FFV1 video, validate it by full
decode roundtrip, audit the encoded result independently, render a
display-referred sRGB or camera-Log review preview, and publish both deliverables atomically.

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
rawtl-linear-video --input <RAW_DIR> --output <RUN_DIR> \
  [--fps 30] \
  [--dng-linear-frame /abs/path/to/dng-linear-frame] \
  [--ffmpeg /opt/homebrew/opt/ffmpeg-full/bin/ffmpeg] \
  [--ffprobe /opt/homebrew/opt/ffmpeg-full/bin/ffprobe] \
  [--preview-profile <ID>] \
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
- `--preview-profile srgb-preview` — one of `srgb-preview`,
  `dji-dlog-dgamut`, `canon-log2-cinema-gamut`, `sony-slog3-sgamut3`,
  `sony-slog3-sgamut3-cine`, `nikon-nlog-bt2020`; unknown or empty ids are
  `EXIT_USAGE`
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
  preview-h264-<transfer>-<gamut>.mp4
                          (default: preview-h264-srgb-bt709.mp4)
```

Intermediate encoder outputs are written under `<output>/.rawtl-partial/` and
are removed after successful publication. If validation or encoding fails, the
directory is retained with that distinguishable name and the error message
names it; no file is ever created under a final deliverable name before
validation succeeds.

## Input formats and ordering

The input directory is one sequence and must contain a single file extension
per directory (case-insensitive). Hidden files (dot-prefixed) and `.xmp`
sidecars are skipped; every other regular file is a candidate frame. A
directory mixing extensions (for example `.CR3` and `.dng`) is rejected with
the input-error exit; there is no vendor/extension whitelist inside a single
extension, because the native producer routes each frame structurally and the
Adobe DNG Converter is the arbiter of whether a vendor RAW is supported. Files
that the Converter rejects fail the run with its transcript; nothing is silently
skipped.

Frame names are tokenised into alternating text and digit runs; text compares
case-insensitively and digit runs compare by numeric value, so `IMG_2` sorts
before `IMG_10`. Names without any digit run, names differing only by case, and
pairs whose keys differ only in digit spelling (`IMG_1` vs `IMG_01`) are
rejected as ambiguous. There is no lexicographic fallback.

Each frame's format class is recorded in its sidecar route: `linearraw-sdk`
(LinearRaw DNG, direct SDK), `cfa-converter-linearraw-sdk` (CFA DNG via the
Converter), or `vendor-raw-converter-linearraw-sdk` (not a recognized DNG:
non-TIFF containers such as CR3, TIFF containers without DNGVersion such as
CR2/NEF/ARW, or ordinary images — the Converter decides). All frames must share
one resolution; a mismatch is a hard decode-time error with no video produced.

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

## Preview output colour/Log profile

The preview is display-referred (sRGB) or a camera-Log review signal, and is
never a measurement source. `--preview-profile <ID>` selects one registered
profile; the default is `srgb-preview`. The encoder is unchanged
(`h264_videotoolbox`, 8-bit `yuv420p`, `-b:v 0 -q:v 65`); only the CPU colour
mapping, the delivery range, the YCbCr coding matrix and the colour tags vary.

| id | preview file | transfer | gamut | range | YCbCr matrix | tags |
| --- | --- | --- | --- | --- | --- | --- |
| `srgb-preview` | `preview-h264-srgb-bt709.mp4` | IEC 61966-2-1 | sRGB/Rec.709 | tv | bt709 | bt709/bt709/iec61966-2-1 |
| `sony-slog3-sgamut3` | `preview-h264-slog3-sgamut3.mp4` | Sony S-Log3 | S-Gamut3 | pc | bt2020nc | unspecified |
| `sony-slog3-sgamut3-cine` | `preview-h264-slog3-sgamut3cine.mp4` | Sony S-Log3 | S-Gamut3.Cine | pc | bt2020nc | unspecified |
| `canon-log2-cinema-gamut` | `preview-h264-clog2-cgamut.mp4` | Canon Log 2 | Cinema Gamut | pc | bt2020nc | unspecified |
| `nikon-nlog-bt2020` | `preview-h264-nlog-bt2020.mp4` | Nikon N-Log | BT.2020 | pc | bt2020nc | unspecified |
| `dji-dlog-dgamut` | `preview-h264-dlog-dgamut.mp4` | DJI D-Log | D-Gamut | pc | bt2020nc | unspecified |

Every profile fixes the same pipeline shape, with per-profile constants and
provenance recorded in `manifest.json` (`preview.color_contract`):

1. `code / 16384` is scene-linear ACEScg (AP1, D60); the source PNG has no OETF.
2. AP1/D60 → XYZ(D60) → Bradford CAT → XYZ(D65) → target linear RGB. All six
   gamuts are D65-referenced. The staged matrices and the composed matrix are
   recorded per profile; the `srgb-preview` composed matrix is bit-identical to
   the pre-existing sRGB path.
3. Per-channel target-linear values clip to the official inverse endpoints
   (for example S-Log3 and Canon Log 2 accept slightly negative scene values
   and clip at the value that maps to code 0; highlights clip at code 1). No
   tone mapping, no implicit roll-off.
4. The official transfer curve is applied, then 8-bit quantization is
   `round(signal * 255)` with ties away from zero and **no dither**.
5. ffmpeg converts RGB24 to `yuv420p` with the profile's matrix/range and writes
   the profile's tags. The sRGB profile keeps the historical
   `scale=out_color_matrix=bt709:out_range=tv` plus
   `setparams=color_primaries=bt709:color_trc=iec61966-2-1:colorspace=bt709:range=tv`
   and the `bt709`/`bt709`/`iec61966-2-1`/`tv` encoder tags. Camera-Log
   profiles use `scale=out_color_matrix=bt2020:out_range=pc` plus
   `setparams=color_primaries=unknown:color_trc=unknown:colorspace=unknown:range=pc`;
   only the real signal range is written, because §6.4 forbids fabricating a
   generic colour tag when the relevant metadata is absent. ffmpeg 8 reports
   full-range 8-bit 4:2:0 H.264 as `yuvj420p`; validation accepts that for `pc`
   profiles and still asserts `color_range=pc`.

Per-profile sources (transcribed into `pipeline/src/color_profile.rs`):

| profile | vendor document | transcribed fields |
| --- | --- | --- |
| Sony S-Gamut3 / S-Gamut3.Cine / S-Log3 | *Technical Summary for S-Gamut3.Cine/S-Log3 and S-Gamut3/S-Log3*, V1.0, <https://www.sony.jp/ls-camera/knowledge/pdf/TechnicalSummary_for-S-Gamut3Cine_S-Gamut3_S-Log3_V1_00.pdf> | S-Log3 forward/inverse formula and constants; 10-bit 0/18/90% codes 95/420/598; both gamuts' primaries and D65 white; FAQ Q4 full range |
| Canon Log 2 / Cinema Gamut | *Description of the Canon Log, Canon Log 2 and Canon Log 3 Gamma Curves*, 2018-11-01, <https://downloads.canon.com/nw/learn/white-papers/cinema-eos/white-paper-canon-log-gamma-curves.pdf>; *Wide Color Gamuts* white paper | Canon Log 2 forward/inverse formula; 10-bit 0/18/100%/800%/1600% codes 95/407/575/797/871; FAQ Q6 full range. Cinema Gamut primaries are not printed in the official white papers; they are cross-checked against colour-science (citing Canon's EOS C500 firmware page) and Canon's ACES linear Cinema Gamut IDT |
| Nikon N-Log / BT.2020 | *N-Log Specification Document*, 1.0.0, 2018-09-01, <http://download.nikonimglib.com/archive3/hDCmK00m9JDI03RPruD74xpoU905/N-Log_Specification_(En)01.pdf> | N-Log forward/inverse formula; BT.2020 primaries and D65 white. Reference codes derive from the official formula over the full 10-bit domain |
| DJI D-Log / D-Gamut | *D-Log and D-Gamut White Paper*, 2017-10-10, <https://dl.djicdn.com/downloads/zenmuse+x7/20171010/D-Log_D-Gamut_Whitepaper.pdf> | D-Log forward/inverse formula; 10-bit 0/18/90% codes 95/408/586; D-Gamut primaries, D65 white and printed matrices. The derived matrices agree with the printed 4-decimal matrices to 1.84e-4 and are used so neutral grey is exact |

Honest boundaries:

- The vendor documents do not publish YCbCr coding coefficients for these
  Log/gamut pairs. `bt2020nc` is an implementation choice for the wide-gamut
  profiles (recorded in the manifest, verified end-to-end); it is not claimed as
  a vendor delivery requirement.
- Nikon and DJI do not use the literal phrase "full range"; `pc` follows the
  published 10-bit code domain (0..1023) and the absence of a stated legal-range
  remap.
- 8-bit Log output has a real quantization/banding tradeoff; no dither is added
  by owner decision. 10-bit, HEVC/ProRes and HLG/PQ outputs are recorded future
  directions, not implemented.
- The preview remains an eye-check deliverable, not a calibrated display match
  or a measurement source.

Unit tests assert every profile's official reference codes (18% grey plus
published endpoints), independently derived non-neutral patches, the Sony
S-Gamut3 vs S-Gamut3.Cine distinction (AP1 green differs by 59 codes in red),
neutral-grey preservation, forward/inverse round trips, explicit two-sided
domain clipping, quantization, and CLI rejection of unknown/empty profile ids.
Integration tests run all six profiles through the real render → encode →
decode path and compare a known patch against independently derived 8-bit
references; the srgb default regression compares decoder `framemd5` and image
NALs against the pre-change binary.

The macOS hardware H.264 encoder rejects source widths above 4096 (measured:
4096x2304 succeeds, 5000x3000 fails with `cannot create compression session:
-12903`; owner decision 2026-11-09). Sources wider than 4096 are therefore
proportionally downscaled to 4096 width with lanczos resampling in the RGB
domain, prepended to the existing colour-matrix conversion; the height is
`round(source_height * 4096 / source_width)` raised to the nearest even value.
The FFV1 master is never scaled and keeps full resolution. Sources at or below
4096 keep the historical filter chain and encode path. The applied geometry,
algorithm and the exact `-vf` chain are recorded in `manifest.json`
(`preview.scale`, `preview.encoder_parameters`), and preview validation checks
the computed preview dimensions, not the master dimensions.

The sRGB reference unit tests assert neutral (`0.18 → 118,118,118`), a
non-neutral reference (`[0.5,0.25,0.125] → [216,129,89]`), the two-sided
clipping policy, the sRGB transfer, that treating AP1 values as display sRGB is
rejected by the non-neutral test, and the preview downscale geometry/filter
policy (no scale at or below 4096, proportional lanczos scale above it,
even-height chroma alignment).

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
| 60 | Preview render/encode/validation failure |
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
