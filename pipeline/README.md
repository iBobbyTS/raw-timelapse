# rawtl-linear-video

Rust driver for the RAW time-lapse product: enumerate a directory of RAW/DNG
frames, convert each source once into a run-local LinearRaw cache, measure and
solve a best-effort exposure plan in one sequential node, stream each developed
frame through scene-linear colour conversion into FFmpeg, check the encoded
file against container metadata (full verification is opt-in via
`--verify-output`), and publish **one video file and one run log**.

The deflicker node is enabled by default. There is no report directory, manifest,
CSV, audit JSON or per-frame log in the product output: the log beside the video
is the operational record.

The crate has no external Rust dependencies, no async runtime and no Python in
the product path. Child processes are invoked with `std::process::Command` using
argv vectors (no shell).

## Build

```sh
cargo clean --manifest-path pipeline/Cargo.toml
cargo build --release --manifest-path pipeline/Cargo.toml
cargo clean --manifest-path pipeline/Cargo.toml
cargo test --manifest-path pipeline/Cargo.toml
```

The binary is `pipeline/target/release/rawtl-linear-video`.

## Run

```sh
rawtl-linear-video --input <RAW_DIR> --output <VIDEO_FILE> \
  [--graph <FILE>] [--fps 30] \
  [--white-balance first-frame|per-frame] \
  [--dng-linear-frame /abs/path/to/dng-linear-frame] \
  [--ffmpeg /opt/homebrew/opt/ffmpeg-full/bin/ffmpeg] \
  [--ffprobe /opt/homebrew/opt/ffmpeg-full/bin/ffprobe] \
  [--preview-profile <ID>] [--output-cat profile|bradford|cat02] [--log-reference-gain N] \
  [--converter "/Applications/Adobe DNG Converter.app/Contents/MacOS/Adobe DNG Converter"] \
  [--converter-timeout 120] \
  [--strength 1.0] [--period 60] [--brightness smooth|uniform] \
  [--estimator pairwise|legacy] [--ev-limits none|LOW,HIGH] \
  [--roi X,Y,W,H] [--exclude-roi X0,Y0,X1,Y1 ...] [--step START:EV ...] \
  [--encoder prores_ks|libx264|ffv1] [--pix-fmt FMT] [--container mov|mp4|matroska] \
  [--codec-profile VALUE] [--crf N|--qscale N|--bitrate BPS] \
  [--output-option K=V ...] [--resize WxH]
```

`--output` names the video file. Its extension must match the resolved container
(default ProRes 4444 in MOV, so `.mov`). The run log is written beside it with
the same stem: `--output /data/timelapse.mov` produces `timelapse.mov` and
`timelapse.log`. Neither file is ever overwritten; a retry with the same name
exits with the output-guard error.

Defaults:

- `--dng-linear-frame native/build/dng-linear-frame` (pass the matched build
  path when using a work-directory build)
- `--ffmpeg` / `--ffprobe` at `/opt/homebrew/opt/ffmpeg-full/bin/`
- `--preview-profile srgb-preview` — one of `srgb-preview`, `dji-dlog-dgamut`,
  `dji-dlog2-dgamut2`, `canon-log2-cinema-gamut`, `sony-slog3-sgamut3`,
  `sony-slog3-sgamut3-cine`, `nikon-nlog-bt2020`,
  `canon-log2-cinema-gamut-rev-eng-crm-dev`,
  `canon-log3-cinema-gamut-rev-eng-crm-dev`
- `--output-cat profile` — `profile` keeps the selected profile's own fixed
  reference chromatic adaptation. `bradford` or `cat02` name a registered
  algorithm explicitly; the same value as the profile default is idempotent,
  while a different value changes output pixels and is reported as
  `custom-adaptation` (never as the profile's reference match)
- `--fps 30` (rational rates such as `30000/1001` are accepted)
- `--white-balance first-frame` — the first numerically ordered frame's as-shot
  neutral is frozen for the sequence; `per-frame` decodes every frame with its
  own RAW as-shot neutral (see *White balance modes*)
- `--strength 1.0`, `--period 60`, `--brightness smooth`, `--estimator pairwise`,
  unbounded EV, full-frame ROI; `--strength 0` disables the analysis node and
  renders the single pass at 0 EV
- Default encoder: `prores_ks` profile 4, MOV, `yuv444p10le`, limited range, at
  the source geometry. ProRes reports 12-bit decode depth; the input handoff is
  10-bit and no lossless claim is made. `--resize` is the only scaling stage and
  runs in the scene-linear FP32 domain.

## Lens correction

`--lens-correction auto|off` defaults to `auto`. Prepare freezes original calibration,
Converter component state and installed LCP parameters; Develop applies only missing
components through an independent camera-linear module. Supported embedded models
win over LCP even when identity; already-applied components are never stacked.
`off` disables only the new module, not corrections baked upstream.

Prepare auto requires ExifTool (default `/opt/homebrew/bin/exiftool`, override with
`--exiftool /absolute/executable`). Repeatable `--lens-profile-dir /absolute/directory`
replaces default Adobe profile roots. The product still has no Python dependency.
Each developed frame records `kind=lens` with per-component decisions and fingerprints.
Output dimensions remain fixed; samples outside the source are black, with no hidden
auto-crop. Missing/unsupported/ambiguous calibration is reported, not called success.
See [architecture, model coverage and limits](../docs/lens-correction.md).

## RAW highlight recovery

Product development enables camera-domain highlight reconstruction before color
matrices, profile correction and EV; measurement remains observation-only.
No new graph node is introduced. The same-stem log reports `kind=highlight`,
including unsupported decoder endpoint semantics rather than claiming recovery.
See [algorithm and limits](../docs/raw-highlight-recovery.md). Native standalone
consumers default to recovery off for matrix-only diagnostics.

## Output

```
<video file>     the deliverable, published by one rename after a container
                 metadata check (--verify-output adds frame counting by decode
                 and a complete decode)
<stem>.log       the single run log
```

A normal run leaves exactly those two files beside whatever already existed in
the directory. Nothing else is created there.

The log is line-oriented; every entry carries `wall=<epoch seconds>` and
`elapsed=<monotonic seconds>` followed by `kind=...` and bounded fields:

- `run.start`, `settings`, `inventory`, `graph`, `encoder`, `colour`
- `prepare`, `measure`, `develop` (frame, EV, applied/own white balance,
  pixel count and digest, duration; prepare timing depends on its mode)
- `prepare.batch` (batch ID, first/last frame, count, actual ADC calls, conversion,
  verification and total wall time)
- `node` completion events for every declared node and frame
- `delete` events naming each deleted cache DNG
- `exposure` events for an explicit exposure node
- `encoder.command` with the exact FFmpeg argv once
- `timing`, `video` (including `verify=metadata|full`), `publish`,
  `retained-cache` (custom graph left generated cache behind), `run.complete`
- on failure: `node.error` / `run.error` with the error family, a bounded
  Converter/FFmpeg stderr tail and the retained scratch path

The log never contains per-frame metadata documents, matrices, pixel payloads or
a deflicker quality verdict. `stdout`/`stderr` carry the same operational scope:
frame inventory, the white-balance mode, the final video/log paths, and errors.

## Scratch, failure and retry

Each run creates a hidden run-specific scratch directory next to the video
(`.rawtl-<name>-<pid>-<nonce>`). It holds the converted LinearRaw DNG cache and
child-process transcripts. After a successful publication the whole scratch
directory is removed when the graph's delete nodes consumed every run-generated
cache file (the default graph does). A valid custom graph that omits deletion
keeps the generated cache: the scratch is retained, the log records
`kind=retained-cache count=… path=…`, and the original RAW/DNG sources stay
untouched. On failure or a stop request the run stops dispatch, reaps its owned
native/encoder children, keeps the remaining cache, transcripts and any partial
output, names that directory in the single log, and exits non-zero without
publishing a formal video.

A failed run must be re-run from the source. There is no checkpoint, resume or
automatic recovery, and a new run never picks up an earlier run's cache. A long
full-resolution sequence needs disk space for the cache peak beside the output.

## Graph

The product executes one typed DAG (`--graph <file>`, default built-in graph).

```text
source -> convert-cache --prepared references--> measure-solve -> apply-ev
                                                        |            |
                                                        |            +-> color -> encode
                                                        +-> (EV plan)      ^
                                                                           |
                       delete-cache (waits apply-ev frame completion) -----+
```

- `prepare` (`convert-cache`) defaults to one bounded native batch of up to 32 sources
  (`dng-linear-frame --prepare-batch`). Each successful batch publishes source-ordered
  references; it can overlap the next batch with measurements of earlier frames.
- `measure-solve` measures independent frames concurrently, then seals the full
  frame inventory and publishes one EV table. `--strength 0` replaces it with a zero plan.
- `develop` applies the plan EV exactly once and returns owned FP32 pixels; the
  source read is complete before the completion event is emitted.
- `color` performs the single 10-bit 4:4:4 quantization handoff; `encode` is the
  streaming FFmpeg sink.
- `delete` waits for the per-frame develop completion and removes only the
  run-generated cache DNG. Original RAW/DNG sources are never deletion targets.

Set concurrency per node in the graph JSON:

```json
{"id":"convert-cache","kind":"prepare","concurrency":4,"inputs":{"source":"source"}}
```

`prepare`, `develop`, `identity`, `exposure`, `resize`, `color`, and the
measurement phase of `solve` accept integer `concurrency` values from 1 to 64;
omission means 1. Source, references, barrier, zero, encode and delete only
accept 1. Prepare also accepts `batch_size`, an integer in 1..64, omitted=1.
Null, booleans, strings, fractions, overflow, duplicates and declarations on other
node kinds are rejected. With `batch_size=1`, concurrency retains its per-frame
meaning, including old `concurrency=4` graphs. With `batch_size>1`, concurrency
must be 1; conflicting fields fail before RAW work or output creation.

```json
{"id":"convert-cache","kind":"prepare","concurrency":1,"batch_size":32,"inputs":{"source":"source"}}
```

The built-in graph uses Prepare=1/batch_size=32, Solve measurement=8, Develop=4,
and 1 elsewhere. 32 is an engineering initial value, not a measured optimum.
The final Solve operation still runs once.

The previous per-frame 4/8/4 defaults were selected from repeated 1/4/8 real-CR3 comparisons,
isolated-stage controls and a direct 4/4/4 versus 4/8/4 comparison. Develop=8
added memory for little extra benefit; measurement=8 helped in the actual 4/N/4
pipeline. Prepare's result depended on the downstream settings, so the choice
uses the combined pipeline rather than an isolated-stage ranking.

On the tested macOS host (36 GiB RAM, 14 logical CPUs), a full 476-frame R7
sequence at 6984x4660, sRGB/ProRes 4444, without resize, took 2239.07 s with all
capacities 1 and 1484.13 s with 4/8/4. Sampled process-tree RSS peaks were
4.37 GiB and 7.77 GiB respectively. Both published and fully decoded all frames,
with identical source/EV/WB/developed-pixel and decoded-frame results. Outputs
and scratch used the same local APFS volume. The smaller 29-file set was split
into 19 R7 and 10 R6 II frames, resized to 696x464 and 600x400; two rounds used
opposite 1/4/8 orders. Source fingerprinting pre-read the files; caches were not
purged and background load varied. These are observed workloads, not an optimal
setting or a fixed speedup guarantee. Colour remains 1 in the measured default;
custom graphs can set its concurrency independently.

Null, strings, booleans, fractions, out-of-range values and duplicate fields are
configuration errors. `--strength 0` normalizes a valid solve to zero with
concurrency 1. CLI resize overrides preserve the existing node's concurrency;
a newly inserted resize uses 1. There is no global threads option or concurrency
environment variable.

Per-frame stages use the standard-library ordered executor. For each stage,
`accepted_not_delivered = running_or_queued + completed_waiting <= concurrency`.
Completion does not release capacity: ordered commit validates identity and
emits the frame event; downstream acceptance releases the slot. A committed
frame may still occupy its producer while a frame-dependent Delete runs. A ready frame 0 can be committed while later frames are still running. Prepare delivers into a
lightweight reference directory, so it does not wait for every consumer. The
reference directory, features, EV plan and on-disk cache can grow with sequence
length; the concurrency bound is a frame bound, not a byte or disk limit.
Prepare batch mode has one task slot holding at most B input/result references.
It selects the continuous available prefix from the next unreceived frame,
checking each source and every frame/sequence wait. It drains currently emittable
source events first, dispatches a short prefix immediately, and flushes tails
without waiting to fill B. Empty prefixes wait only for real work. The complete
result set passes identity validation before any frame commit; frame 0 commits
first. The slot releases after all references are materialized, without waiting
for measure/develop/delete consumption. Exact frame coverage and no outstanding
batch are required for EOF. Other stages retain their frame capacities.

Heavy frames across the chain are bounded by the sum of stage capacities, plus
native worksets and each transformation's temporary buffers.
Frame 0's white balance is frozen at ordered prepare commit. Solve runs once,
after every measurement and matching EOF is sealed. Per-node logs commit in
source order; different nodes can interleave. Durations measure computation,
not time waiting in ordered slots, and their sum is not wall time. In batch mode,
`kind=prepare duration` measures independent reference materialization/publication
preparation only, excluding shared native conversion, context loading and verification.
`kind=prepare.batch` records those shared costs separately: actual ADC calls,
conversion process wall, output+metadata verification wall and total adapter wall.
Total includes routing, hashes, context/result publication and Rust loading; it
need not equal conversion+verification. Custom tools without native diagnostics
log unknown metrics. Single mode keeps the old per-frame wall-time interpretation.
`mode`, `duration_scope` and stage `capacity_unit=frames|batch_tasks` distinguish
these meanings; never sum per-frame batch durations as conversion cost.

`--converter-timeout` remains a total timeout per actual ADC sub-call (default
120 seconds, 1..3600), including a multi-file invocation. It is not multiplied by
frame count. Filename collisions and argv limits may split a logical batch into
serial calls; cancellation stops later calls. Each batch has a unique private
`cache/batch_NNNNNN` with request/result/contexts, shared records and
`batch-native.log`. Shared records survive per-frame Delete until run scratch
cleanup. Materials remain read-only.

All prepare, measure, develop and FFmpeg processes share the run's multi-task
process owner. Encode runs as a capacity-1 task with one writer to one FFmpeg
stream, so a blocked stdin write leaves the coordinator able to cancel and
collect other completions. Sequence completion requires sealed input, all tasks
finished, ordered commits and all output delivery. Errors or stop signals cancel and reap active tasks; late successes are
not committed. FFmpeg failure cancels every registered producer. Each task has
its own transcript in retained scratch on failure. The existing run log records
`kind=stage` summaries (capacity, accepted/committed/delivered counts and peak
in-flight frames) and `kind=image`/`kind=encode` compute durations. Stage duration
sums include overlapping tasks and must not be interpreted as pipeline wall time.

Events are framework-generated per frame and per sequence and are recorded in
the run log. A custom graph may insert identity/exposure/resize nodes, chain
another frame node, or add waits; an invalid graph, type mismatch, cycle or
illegal event dependency is a configuration error before any RAW work.

[`docs/node-graph-workflow.md`](../docs/node-graph-workflow.md) records the model, default graph, event scopes,
stop/error behaviour, encoder contract and log format in more detail.

## Input formats and ordering

The input directory is one sequence and must contain a single file extension
per directory (case-insensitive). Hidden files (dot-prefixed) and `.xmp`
sidecars are skipped. Frame names are tokenised into alternating text and digit
runs; text compares case-insensitively and digit runs compare numerically, so
`IMG_2` sorts before `IMG_10`. Names without a digit run, names differing only by
case and names differing only in digit spelling (`IMG_1` vs `IMG_01`) are
rejected as ambiguous. There is no lexicographic fallback.

Each frame's route is recorded by the prepared context: `linearraw-sdk` (a
LinearRaw DNG decoded directly), `cfa-converter-linearraw-sdk` (CFA DNG through
the Converter) or `vendor-raw-converter-linearraw-sdk` (vendor RAW such as CR3
through the Converter). All frames must share one resolution; a mismatch is a
hard error with no video produced.

## White balance modes

- `first-frame` (default): the first numerically ordered frame is decoded with
  its own AsShotNeutral (`as-shot`). Its applied triple is passed to every later
  frame as `--white-balance R,G,B`; each later sidecar must report `override`
  and exactly that triple. The log's `develop` entries show the frozen value and
  `wb_source=override`.
- `per-frame`: every frame is decoded with its own RAW as-shot neutral and the
  pipeline passes no `--white-balance` override. Every `develop` entry must show
  `wb_source=as-shot` with applied equal to the frame's own neutral.

In both modes each frame keeps its own `as_shot_neutral`; a producer that lies
about its source or applied value is rejected at the frame layer. The deflicker
node measures exposure brightness only and never smooths white balance.

## Encoding contract

The encoder receives planar 10-bit 4:4:4 (`yuv444p10le`) from the colour node.
The FFmpeg argv is fixed by the pipeline (input pipe, frame rate, geometry,
frame count, colour/range and a single output); `--output-option` accepts only
the explicitly supported keys (`-preset -tune -vendor -threads -g -bf -level`)
and foreign inputs such as `-i`, another output, filters or geometry overrides
are rejected. Unsupported encoder/pixel-format/container combinations and
configuration conflicts are rejected before any RAW preparation; no encoder or
pixel format is silently substituted.

Publication is one rename after the output check. By default the check reads
ffprobe container metadata only: codec/profile/pixel format/geometry/rate,
per-field colour tags, range where the container can express it, and the frame
count when the container declares one (MOV/ProRes declares none, so the logged
count is then the encoder's own). `--verify-output` additionally counts frames
by decoding and decodes the entire deliverable to prove it is playable. Check
failure leaves only the scratch partial and a non-zero exit.

## Colour/Log profiles

`--preview-profile <ID>` selects the output colour/Log rendering (default
`srgb-preview`). Canon Cinema Gamut/CLog profiles are signalled per field and are
never disguised as BT.2020 transfer/primaries. ProRes delivery is limited range
because MOV `colr/nclc` cannot carry a range field and ProRes decoders assume
video range; other encoders are validated against the configured pixel format
and range. No tone or gamut mapping and no dither are applied; 10-bit handoff
still quantizes once at the encoder input.

| id | transfer / gamut | YCbCr matrix |
| --- | --- | --- |
| `srgb-preview` | sRGB / Rec.709 | bt709 |
| `sony-slog3-sgamut3` | S-Log3 / S-Gamut3 | bt2020nc |
| `sony-slog3-sgamut3-cine` | S-Log3 / S-Gamut3.Cine | bt2020nc |
| `canon-log2-cinema-gamut` | clog2 (white paper) / Cinema Gamut | bt2020nc |
| `canon-log2-cinema-gamut-rev-eng-crm-dev` | clog2 (rev-eng crm-dev) / Cinema Gamut | bt2020nc |
| `canon-log3-cinema-gamut-rev-eng-crm-dev` | clog3 (rev-eng crm-dev) / Cinema Gamut | bt2020nc |
| `nikon-nlog-bt2020` | N-Log / BT.2020 | bt2020nc |
| `dji-dlog-dgamut` | D-Log / D-Gamut | bt2020nc |
| `dji-dlog2-dgamut2` | D-Log 2 / D-Gamut 2 (pinned DCTL, CAT02) | bt2020nc |

Input development is selected with `--camera-color auto|generic|canon-clog3`
(default `auto`). After the existing single-extension input enumeration:

| selection | resolved input recipe |
| --- | --- |
| auto, CLog3 profile, all original inputs CR3 | Canon CLog3; native verifies Canon metadata for every frame |
| auto, any other profile or non-CR3 inputs | generic |
| generic, any profile | generic |
| canon-clog3 | requires CLog3 and original Canon CR3; otherwise rejected |

A mixed-extension directory is rejected before recipe selection in every mode.
The recipe is frozen before prepare and shared by measure and develop. A failed
Canon request never retries as generic; Canon sequences still reject mixed models.

Generic development uses SDK CameraToPCS (ForwardMatrix when present, SDK inverse
ColorMatrix fallback otherwise), with the selected WB already included. It skips
and reports profile HueSatMap, LookTable, ToneCurve and BaselineExposure. It applies
no Canon exposure calibration and reports `uncalibrated`, including when the user
sets a reference gain. Invalid or singular profile transforms fail explicitly.
Input D50→D60 adaptation remains Bradford; it is independent of output color.

Only the Canon CLog3 recipe applies the validated profile HueSatMap and original
CR3 NormalWhite/fixed 1.8 exposure calibration. CLog2RevEng now uses generic input
by default: its changed input pixels/measurement are an intentional migration.
Existing output curves, gamuts, tags and the default Bradford output matrices
retain their definitions.

The output chromatic adaptation belongs to `OutputProfile`, not to the transfer
curve or the input CAT. `--output-cat profile` (default) uses the profile's fixed
reference realization; the pre-existing profiles keep the published Bradford
D60→D65 matrix and composition order bit-for-bit, while `dji-dlog2-dgamut2`
pins its own DCTL chain. An explicit
`bradford`/`cat02` equal to the profile default is the same idempotent reference
selection. A different explicit method uses the standard derivation with column
vectors — `M_out = inverse(M_target_to_XYZ) × CAT × M_AP1_to_XYZ`, where
`CAT = inverse(B) × diag((B·W_to)/(B·W_from)) × B`, whites at Y=1, full
adaptation — and is labeled `custom-adaptation`; it is not claimed to match the
vendor/reference chain. Bradford and CAT02 have independent fixed cone matrices.
The gamut's `white_xy` participates in the composition, so target white is
reported and used rather than assumed D65. Invalid method names, white points,
singular/non-finite matrices and unknown selections are rejected before RAW
work; per-pixel conversion uses coefficients frozen at startup. The `colour` log
line reports `output_cat_requested`, `output_cat_method`,
`output_cat_realization` (`reference`/`custom-adaptation`), source/target whites
and a stable transform fingerprint. Adding an algorithm to the static CAT
registry needs no transfer or native change.

### D-Log 2 / D-Gamut 2 (`dji-dlog2-dgamut2`)

This profile implements the fixed three-segment D-Log 2 curve and the pinned
D-Gamut 2 -> ACES AP0 (CAT02) DCTL chain; it is independent of the original
`dji-dlog-dgamut` profile. Curve constants are `H=475`,
`A=16.285770761945304`, `K1=0.059439938321493`, `B1=0.304985337243402`,
`K2=2.960935245492250`, `B2=0.148314799066323`, `T=0.028961695254132`. The curve
runs in linear reflection units (`curve_input_scale = 1`) and its encodable
domain is `[D(0), D(1)] = [-0.021128829113842125, 475]`; values outside it are
clipped to that domain per channel, which for this monotone curve is equivalent
to clipping the encoded signal to `[0, 1]`.

The default output CAT is the DCTL's real chain, not Bradford:
`AP1/D60 -> AP0/D60 (ACES v1.3 chromaticities) -> XYZ/D60 -> the inverse of the
DCTL's printed CAT02 D65->D60 constant (XYZ/D60 -> XYZ/D65, as required by
inverse(M_DG2_AP0) x M_AP1_AP0) -> XYZ/D65 -> D-Gamut 2`. Its three four-decimal
matrices are used digit for digit and the rounded chain's white-axis residual
(max 2.5e-4) is preserved, never normalised away. `--output-cat profile` (or `cat02`)
selects this reference realization; another method derives a
`custom-adaptation` matrix and does not claim the reference chain. The colour
contract also exposes the printed `cat02_d65_to_d60`, `xyz60_to_acesap0`,
`dgamut2_to_acesap0` and derived `acesap1_to_acesap0` matrices for traceability.

The full-range theoretical 10-bit codes (0 -> 64, T -> 152, 0.18 -> 312,
0.9 -> 456, 46.08 -> 812, 184.32 -> 937, 475 -> 1023) describe the curve only;
they are not camera ISO/EI clipping points and not limited-range Y codes. ProRes
still delivers limited range (`yuv444p10le` handoff, 64..940), FFV1 delivers the
profile's full range, and both use one resolved range for quantization and
encoder. Tags follow the vendor Log policy (primaries/transfer unspecified,
`bt2020nc` coding), which does not mean BT.2020 primaries. `reference_status`
remains `unverified`: this is a math implementation, not a claim of matching DJI
camera exposure or in-camera video.

Every successful run logs one `kind=clip` line with `clipped_low`,
`clipped_high`, `clipped_total` and `total_channel_samples` (`unit=channel_samples`,
three samples per pixel), accumulated from ordered colour commits only. Frames
that fail or never commit are excluded; an incomplete run publishes no video and
no clip summary. The clipping counters are not an added per-frame audit file.

`--log-reference-gain N` is a positive finite output gain for every profile,
default 1. It is applied once before the output transfer function and never enters
measurement or native EV. The independent `curve_input_scale` is 0.9 for the two
rev-eng Canon curves and 1 for other profiles; it is a curve input unit conversion,
not camera calibration. This replaces the historical internal contract field
`native_scene_to_reflection_scale`. Native calibration metadata no longer reports
`scene_to_reflection` as a camera calibration factor.

Rust and native must be rebuilt together for `color_contract_version=2` (native
color algorithm revision 4, output pipeline revision 5). Requests specify
`--color-contract-version 2 --camera-recipe <generic|canon-clog3>` plus explicit
`--camera-color-method dng-camera-to-pcs`, `--profile-correction <none|hue-sat-map>`
and `--camera-calibration <none|canon-normal-white-fixed-v1>`. The reader checks
requested/effective recipe and policies, profile presence/applied/skip states,
input CAT and matrices, calibration evidence, source/prepared identity, WB, EV,
FP32 framing/hash/finite samples, EOF and successful native exit. Old contracts
fail; prepared v2 and pixel framing are unchanged. The native legacy matrix mode
is retained only for explicit historical tests/tools, outside product auto routing.
See [Log2](../docs/canon-log2-rev-eng.md), [Log3](../docs/canon-log3-rev-eng.md)
and [historical calibration evidence](../docs/canon-fixed-calibration.md).

## Deflicker node (internal contract)

1. **Prepare** — one Converter/SDK conversion per source into the scratch cache.
   The original path remains the identity authority for Canon calibration,
   white balance and capture metadata; the cache path is never substituted.
2. **Measure** — every frame is analysed at 0 EV through the prepared context
   (`--stdout-analysis`) and only O(N × small features) measurements are kept.
   Capture-time gaps from the median nominal interval are the only continuity
   boundaries; filenames add none.
3. **Solve** — the accepted V4 estimator (pairwise or legacy, smooth or uniform,
   confirmed steps, EV limits, scene-cut metadata) publishes one complete,
   finite frame-ID plan. A frame without reliable pairwise evidence receives a
   finite internal fallback; incompleteness never blocks the run and is never
   surfaced as a user-facing quality verdict.
4. **Render** — each frame is decoded once more at its planned EV
   (`--stdout-frame-f32`), converted to the single 10-bit 4:4:4 handoff and
   pushed in source order to one FFmpeg stream through a bounded image chain.
5. **Delete** — the develop completion event releases that frame's cache DNG
   immediately; no encoder acknowledgement is awaited.

There is no gain probe, render-feedback round, independent encoded-video audit
or quality report in the product path. Development measurement still reuses the
same measurement/estimation engine in its own tests.

## Stage handoffs (typed transports)

Subprocesses exchange stdout records and stdin pixel bytes; every Rust stage
consumes the typed payload it validates.

- **Native JSON → measurement:** protocol v1 begins with `RAWTLST1` and ordered
  records; analysis mode requires metadata (0x01) then analysis (0x02). The
  bytes become `rawtl.frame-metadata@1` / `rawtl.analysis-sidecar@1` MemoryFile
  envelopes; source hash/provenance, geometry, WB and EV checks run before the
  schema-2 measurement parser. Metadata is machine data, not a log dump.
- **Native pixels → encoder:** frame mode requires metadata (0x01), descriptor
  (0x03) and signed FP32 AP1/D60 pixels (0x04, `RAWTLST2`). Types, reserved
  fields, ordering, geometry length, truncation, trailing bytes, non-finite
  samples and the descriptor's pixel digest are all rejected. Pixels become one
  owned FP32 frame, then the colour node fills the 10-bit target signal.
- **Encoded video → publication:** the sole encoded file becomes
  `rawtl.video-artifact@1`; validation and the final rename consume that same
  reference. No second consumer or audit path exists.

## Exit codes

| Exit | Meaning |
| --- | --- |
| 0 | video published |
| 2 | CLI usage/configuration error (including the removed run-directory form) |
| 10 | input enumeration/ordering error |
| 11 | output/input overlap or invalid output path |
| 12 | existing video or log would be overwritten |
| 20 | required executable (native tool/ffmpeg/ffprobe) unavailable |
| 30 | frame production or frame-metadata verification failure |
| 50 | graph/encoder configuration or typed artifact validation failure |
| 60 | streaming encoder/validation failure; cancelled encoder |
| 70 | run-log or publication/rename failure |
| 130 | stop request (SIGINT/SIGTERM); scratch retained |

## Tests

`cargo test --manifest-path pipeline/Cargo.toml` runs numerical/transport unit
tests, the graph scheduler tests, and CLI integrations with real FFmpeg, a
standalone fake native producer and one guarded real-CR3 journey (it is skipped
with a printed note when the staged material, Converter or native build is
absent). Coverage includes the one-video/one-log surface, scratch cleanup,
retained failure scratch, overwrite refusal, Converter/FFmpeg failure reporting,
container/extension mismatch, removed quality flags, WB modes, graph
configuration/deletion ordering, resolved EV plans, degraded photometry and
typed handoff defects.

## Non-goals

No GUI, plugin ABI, arbitrary script nodes, cross-run recovery, automatic
publication, full-sequence in-memory image caches, EXR caches or a second
encoding backend. The frozen legacy Python implementation remains read-only
research material and is not a runtime dependency.
