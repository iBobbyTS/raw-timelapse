# rawtl-linear-video

Rust driver for the RAW time-lapse product: enumerate a directory of RAW/DNG
frames, convert each source once into a run-local LinearRaw cache, measure and
solve a best-effort exposure plan in one sequential node, stream each developed
frame through scene-linear colour conversion into FFmpeg, check the encoded
file against container metadata (full verification is opt-in via
`verify_output`), and publish **one video file and one run log**.

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
rawtl-linear-video --input /data/banff --profile default
rawtl-linear-video --input /data/banff --profile /absolute/path/to/profile.json \
  --output /data/delivery/timelapse.mov
```

Both `--input` (a RAW or DNG sequence directory) and `--profile` are required.
The sole built-in profile, `default`, selects the Log from the first input and
exports at 60 fps in ProRes 4444 MOV at source dimensions. A custom profile
is selected by its absolute JSON file path and controls the processing
and delivery settings. Its `fps` field accepts
a positive integer (for example `60`) or a rational string (for example
`"60000/1001"`), up to 1000 fps. When omitted, `fps` defaults to 60.

Omit `--output` to write beside the source directory. With the default profile,
`/data/banff.v2/` produces `/data/banff.v2.mov` and `/data/banff.v2.log`. An
explicit video path must match the profile's container. Existing videos and
logs are never overwritten. Use `--help` or `--version` for CLI information.

## Lens correction

`lens_correction auto|off` defaults to `auto`. Prepare freezes original calibration,
Converter component state and installed LCP parameters; Develop applies only missing
components through an independent camera-linear module. Supported embedded models
win over LCP even when identity; already-applied components are never stacked.
`off` disables only the new module, not corrections baked upstream.

Prepare auto requires ExifTool (default `/opt/homebrew/bin/exiftool`, override with
`exiftool /absolute/executable`). The `lens_profile_dirs` list
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
consumers default to recovery off for diagnostic development.

## Output

```
<video file>     the deliverable, published by one rename after a container
                 metadata check (verify_output adds frame counting by decode
                 and a complete decode)
<stem>.log       the single run log
```

A normal run leaves exactly those two files beside whatever already existed in
the directory. Nothing else is created there.

The log is line-oriented; every entry carries `wall=<epoch seconds>` and
`elapsed=<monotonic seconds>` followed by `kind=...` and bounded fields:

- `run.start`, `profile`, `settings`, `inventory`, `graph`, `encoder`, `colour`
- `prepare`, `measure`, `develop` (frame, EV, applied/own white balance,
  pixel byte count and duration; prepare timing depends on its mode)
- `prepare.sequence` (frame count, actual ADC calls, conversion,
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

## Internal processing graph

The profile configures one internal typed DAG in the project-defined processing order.

```text
source -> convert-cache --prepared references--> measure-solve -> apply-ev
                                                        |            |
                                                        |            +-> color -> encode
                                                        +-> (EV plan)      ^
                                                                           |
                       delete-cache (waits apply-ev frame completion) -----+
```

- `prepare` (`convert-cache`) prepares the full sequence with
  `dng-linear-frame --prepare-sequence`. Adobe DNG Converter runs once for all
  inputs that need it. Validated references are published in source order.
- `measure-solve` measures independent frames concurrently, then seals the full
  frame inventory and publishes one EV table. `strength 0` replaces it with a zero plan.
- `develop` applies the plan EV exactly once and returns owned FP32 pixels; the
  source read is complete before the completion event is emitted.
- `color` performs the single 4:4:4 quantization handoff (10- or 12-bit by
  `signal_bits`); `encode` is the streaming FFmpeg sink.
- `delete` waits for the per-frame develop completion and removes only the
  run-generated cache DNG. Original RAW/DNG sources are never deletion targets.

Develop, identity, exposure, resize, color and the measurement phase of Solve
accept integer `concurrency` from 1 to 64. Prepare and the remaining nodes
accept only 1. The built-in graph uses Solve measurement=8, Develop=4 and
Color=4. The final Solve operation runs once.

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
configuration errors. `strength 0` normalizes a valid solve to zero with
concurrency 1. Profile resize settings preserve the existing node's concurrency;
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
Prepare waits for the sealed source inventory and launches one sequence task.
When Prepare directly feeds Solve on macOS, it can make frames available for
early measurement while Converter is still running. After a writable
descriptor disappears, the file must stay unchanged with no observed writer
for five seconds. Solve admits only the ready prefix in frame order and shares
its worker limit between early and final measurements. The paths and
provisional results are stored in scratch. Prepare validates the full sequence
before final publication. Matching source identity, DNG hash and white balance
allow reuse of early measurements. The log records early admission and reuse.

Prepare hashes RAW inputs and sealed Converter outputs once. Internal consumers
check file identity, length and modification/change times without rereading the
contents for integrity. An early candidate is hashed once before publication
and compared with the sealed hash before reuse. Native checks its captured file
state before and after decode to catch writes or replacement during measurement.

Heavy frames across the chain are bounded by the sum of stage capacities, plus
native worksets and each transformation's temporary buffers.
Frame 0's white balance is frozen at ordered prepare commit. Solve runs once,
after every measurement and matching EOF is sealed. Per-node logs commit in
source order; different nodes can interleave. Durations measure computation,
not time waiting in ordered slots, and their sum is not wall time.
`kind=prepare.sequence` records the converter call count, conversion and
verification wall time, and total Prepare time. The sequence Converter timeout
defaults to 15 seconds multiplied by the number of frames sent to Converter and
can be overridden with `converter_timeout`.
Filename collisions or long paths use short aliases inside the run scratch.
If even the alias list exceeds the OS command-line limit, Prepare fails before
starting Converter. The scratch keeps one `cache/linear` directory, the exact
`cache/converter.argv` invocation, per-frame provenance records and the
`cache/prepare-native.log` transcript.

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
per directory (case-insensitive). Hidden files and known non-RAW companions
(XMP, text/Markdown notes, JPEG/PNG/HEIF previews and THM thumbnails) are skipped. Frame names are tokenised into alternating text and digit
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
  frame as the native `--white-balance R,G,B` override; each later sidecar must report `override`
  and exactly that triple. The log's `develop` entries show the frozen value and
  `wb_source=override`.
- `per-frame`: every frame is decoded with its own RAW as-shot neutral and the
  pipeline passes no `white_balance` override. Every `develop` entry must show
  `wb_source=as-shot` with applied equal to the frame's own neutral.

In both modes each frame keeps its own `as_shot_neutral`; a producer that lies
about its source or applied value is rejected at the frame layer. The deflicker
node measures exposure brightness only and never smooths white balance.

## Encoding contract

The encoder receives the planar 4:4:4 target signal from the colour node at the
run's `signal_bits` depth (`yuv444p10le` default, `yuv444p12le` with
`signal_bits 12`; limited-range 12-bit codes follow H.273's 256..3760
luma / 256..3840 chroma scaling). Without an explicit `pix_fmt`, prores_ks
and ffv1 follow the handoff depth; prores_ks + 12 is rejected before any RAW
work because its encoder input is 10-bit only.
The FFmpeg argv is fixed by the pipeline (input pipe, frame rate, geometry,
frame count, colour/range and a single output); `output_options` accepts only
the explicitly supported keys (`-preset -tune -vendor -threads -g -bf -level`)
and foreign inputs such as `-i`, another output, filters or geometry overrides
are rejected. Unsupported encoder/pixel-format/container combinations and
configuration conflicts are rejected before any RAW preparation; no encoder or
pixel format is silently substituted.

Publication is one rename after output verification. By default FFmpeg reads
the input stream description and one encoded packet without a full decode.
Codec, pixel format, dimensions and available conforming metadata are checked.
The default mode reports the pipeline frame total. With `verify_output` enabled,
the same FFmpeg invocation strictly decodes the whole video and verifies the
decoded frame count. Check failure leaves the scratch partial and a non-zero
exit. Legacy `ffprobe` settings are accepted for compatibility and ignored.

## Colour/Log profiles

The profile’s `output_color` field selects the output colour/Log rendering. The
built-in `default` profile uses sRGB (`srgb-preview`). Canon Cinema Gamut/CLog profiles
are signalled per field and are
never disguised as BT.2020 transfer/primaries. ProRes delivery is limited range
because MOV `colr/nclc` cannot carry a range field and ProRes decoders assume
video range; other encoders are validated against the configured pixel format
and range. No tone or gamut mapping and no dither are applied; either handoff
depth still quantizes exactly once at the encoder input.

| id | transfer / gamut | YCbCr matrix |
| --- | --- | --- |
| `srgb-preview` | sRGB / Rec.709 | bt709 |
| `sony-slog3-sgamut3` | S-Log3 / S-Gamut3 | bt2020nc |
| `sony-slog3-sgamut3-cine` | S-Log3 / S-Gamut3.Cine | bt2020nc |
| `clog2` | clog2 / Cinema Gamut | bt2020nc |
| `clog3` | clog3 / Cinema Gamut | bt2020nc |
| `nikon-nlog-bt2020` | N-Log / BT.2020 | bt2020nc |
| `dji-dlog-dgamut` | D-Log / D-Gamut | bt2020nc |
| `dji-dlog2-dgamut2` | D-Log 2 / D-Gamut 2 (pinned DCTL, CAT02) | bt2020nc |

Set `"output_color": "auto-log"` in the processing profile to select one of
these profiles for the whole sequence. The pipeline uses ExifTool to read the
original first file in numeric input order before the encoder preflight. The
`exiftool` setting also applies when lens correction is off. The selected
profile supplies its usual CAT and reference-gain defaults. Explicit
`output_cat` and `log_reference_gain` settings still apply.

| First input camera | Selected `output_color` |
| --- | --- |
| Canon R1, R3, R5, R5 Mark II, R5 C, R6 Mark II, R6 Mark III, R6 V, R7, R8, R8 Mark II, C50, C70, C80, C300 Mark II, C300 Mark III, C400, C500 Mark II, C700 (including PL, GS PL, FF and FF PL) | `clog2` |
| Other Canon cameras | `clog3` |
| DJI Pocket 4P (`PP-041`) | `dji-dlog2-dgamut2` |
| Other DJI cameras | `dji-dlog-dgamut` |
| Nikon | `nikon-nlog-bt2020` |
| Sony | `sony-slog3-sgamut3` |

The `kind=auto-log` event records the source, make, model and chosen profile.
An unsupported or missing make, or a missing Canon or DJI model, requires an
explicit `output_color` setting.

Input development is selected with `camera_color auto|generic|canon-cr3`
(default `auto`). After the existing single-extension input enumeration:

| selection | resolved input recipe |
| --- | --- |
| auto, original CR3 sequence | Canon CR3; native verifies Canon identity and calibration metadata for every frame |
| auto, other input formats | generic |
| generic | generic for any supported input |
| canon-cr3 | requires an original Canon CR3 sequence |

A mixed-extension directory is rejected before recipe selection in every mode.
The recipe is frozen before prepare and shared by measure and develop. The
same input and settings produce the same calibrated scene-linear ACEScg frames
for every output profile, including sRGB. Canon calibration errors fail the run,
as do mixed camera models in a calibrated sequence. Use `camera_color generic`
to keep the uncalibrated SDK scale, including for CR3 workflows that used it
under non-Canon outputs before this change.

All development uses the selected camera profile's valid embedded HueSatMap when
present, in linear ProPhoto/D50 before scene exposure. HSM is independent of output
Log/preview profiles and camera recipe selection, with no disable option. Absence
leaves the matrix result intact; malformed tables fail rather than silently falling
back. Measure, scan and delivery share this rule. LookTable, ToneCurve and
BaselineExposure remain skipped.

Generic development uses SDK CameraToPCS (ForwardMatrix when present, SDK inverse
ColorMatrix fallback otherwise), with the selected WB already included. It applies
no Canon exposure calibration and reports `uncalibrated`, including when the user
sets a reference gain. Invalid or singular profile transforms fail explicitly.
Input D50→D60 adaptation remains Bradford; it is independent of output color.

The Canon CR3 input recipe applies original CR3 NormalWhite/fixed 1.8 exposure
calibration for every output. Use `clog2` or `clog3` as the output colour setting. CLog2 uses the published Canon curve, numerically verified
against the recovered SDK formula.
Both Canon encodings multiply native scene-linear values by 0.9 exactly once before
their reflection-domain curve. `camera_color canon-cr3` explicitly selects the
input recipe with any output profile.
Other output curves, gamuts, tags and default output matrices retain their definitions.
Original D-Log now defaults to CAT02 (ACES white/D60 to D65), replacing output
Bradford rather than stacking adaptations. `output_cat bradford` remains available;
input D50 to D60 adaptation remains unchanged. D-Log 2 keeps its separate DCTL chain.

The output chromatic adaptation belongs to `OutputProfile`, not to the transfer
curve or the input CAT. `output_cat profile` (default) uses the profile's fixed
reference realization: `dji-dlog-dgamut` uses standard CAT02, `dji-dlog2-dgamut2`
pins its own DCTL chain, and other profiles keep the published Bradford
D60→D65 matrix and composition order bit-for-bit. An explicit
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
(max 2.5e-4) is preserved, never normalised away. `output_cat profile` (or `cat02`)
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

`log_reference_gain N` is a positive finite output gain for every profile,
default 1 except original D-Log, which defaults to `auto` with desired gain 4
(+2 EV). An explicit numeric value, including `1`, overrides this default. The
resolved gain is applied once before the output transfer function and never enters
measurement or native EV. The independent `curve_input_scale` is 0.9 for the two
Canon Log 2/3 curves and 1 for other profiles; it is a curve input unit conversion,
not camera calibration. This replaces the historical internal contract field
`native_scene_to_reflection_scale`. Native calibration metadata no longer reports
`scene_to_reflection` as a camera calibration factor.

`log_reference_gain auto` (camera Log profiles only; `srgb-preview` is rejected)
resolves one sequence-wide gain instead. After the exposure plan exists and before
the first colour conversion, every frame is developed once more through the exact
delivery image chain (exposure, resize, colour-boundary crop) and the target-gamut
linear extremes are reduced with the converter's own arithmetic. The applied gain
is `min(desired, positive headroom, negative toe)`: the desired brightening is 4
(+2 EV) for D-Log / D-Log 2 and 8 (+3 EV) for every other camera Log curve, and the two
headroom bounds keep every scanned sample inside the transfer's encodable domain,
so an auto run reports `clipped_total=0` by construction. Both bounds are upper
bounds on the gain, so a positive result always exists; an all-black sequence
keeps the desired gain. The cost is one extra develop pass over the sequence
(transcripts under the `scan` stage; scratch lifetime unchanged). One
`kind=reference_gain` run-log line records requested/desired/safe bounds, the
applied gain and EV, `limited_by` (`desired`, `positive-headroom` or
`negative-toe`), scanned sample count and duration. The gain is a delivery
parameter like the numeric form: it never enters measurement or native EV, and
the encoded metadata carries the resolved number.

Rust and native must be rebuilt together for `color_contract_version=2` (native
color algorithm revision 4, output pipeline revision 5). Requests specify
`--color-contract-version 2 --camera-recipe <generic|canon-cr3>` plus explicit
`--camera-color-method dng-camera-to-pcs`
and `--camera-calibration <none|canon-normal-white-fixed-v1>`. The reader checks
requested/effective recipe and policies, profile presence/applied/skip states,
input CAT and matrices, calibration evidence, source/prepared identity, WB, EV,
FP32 framing, geometry and finite samples, EOF and successful native exit. Old contracts
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
  samples are rejected. Pixels become one
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
| 20 | required executable (native tool/FFmpeg) unavailable |
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
