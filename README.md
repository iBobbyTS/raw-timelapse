# raw-timelapse

Offline command-line RAW time-lapse processor: deterministic filename ordering, robust exposure smoothing, simple sRGB development, and H.264/MP4 using FFmpeg.

**Status: first implementation.** Synthetic algorithm and software-encoder test coverage is included. Real-world RAW colour and hardware encoding need validation with your own camera and GPU. This is not a colour-managed production RAW grading pipeline.

## Requirements

- Python 3.12+, FFmpeg and ffprobe on your `PATH` (or `--ffmpeg /path/to/ffmpeg`).
- `rawpy`/LibRaw, NumPy, SciPy and Pillow (installed by pip).
- Sufficient storage; the target bit rate is **20 MB/s = 160 Mbps**, not 20 Mbps.

Installation:

```bash
python -m venv .venv
source .venv/bin/activate                # Windows: .venv\\Scripts\\activate
python -m pip install -r requirements.lock
rawtl --help
rawtl probe /path/to/RAW_DIR
```

Alternative: `python -m pip install -e ".[test]"` for current compatible dependency versions.

## Process a sequence

```bash
rawtl run ./raws -o ./out
rawtl run ./raws -o ./out --fps 30000/1001 --period 60 --strength 1.0
rawtl run ./raws -o ./out --brightness uniform
rawtl run ./raws -o ./out --roi 0.10,0.15,0.70,0.60 --steps steps.json
rawtl run ./raws -o ./out --on-review passthrough --save-frames
rawtl run ./raws -o ./out --encoder libx264 --overwrite
```

Use zero-padded filenames such as `IMG_0001.CR3`. The scanner reads **only the first directory level**, filters common RAW extensions (CR2, CR3, NEF, ARW, DNG, RAF, RW2, ORF), and sorts **case-sensitively by the complete filename**. Without zero padding, `IMG_1.CR3, IMG_10.CR3, IMG_2.CR3` remain in exactly that order; EXIF timestamps are never used for sorting. Unsupported or corrupt RAWs are errors, not silently skipped frames.

The input directory must not contain an output directory and the program never modifies RAWs or sidecar metadata.

### Explicit exposure steps

`steps.json` describes *confirmed* exposure jumps, with 0-based frame indices:

```json
[
  {"start_frame":300, "delta_ev":1.0},
  {"start_frame":600, "delta_ev":-0.6666666667}
]
```

Without this file, the algorithm removes **fast flicker**, not persistent photographer-controlled exposure changes. It preserves slower daytime-to-nighttime changes. It cannot inherently distinguish a lightning flash from camera flicker.

### Scene cuts (spliced or culled sequences)

Frames concatenate in filename order; capture time and camera numbering are never inputs. The analyzer detects content jumps (scene cuts) between adjacent frames from the tile photometry itself: a large shift that is **both** common (|median tile delta| >= 0.20 EV) and spatially non-uniform (cross-tile MAD >= 0.08 EV). Each content-continuous run is then smoothed independently with its own exposure anchor, so a splice never poisons neighbouring frames and never blocks the run by itself; runs shorter than 8 frames render without automatic deflicker. A spatially *uniform* jump is common flicker to smooth, not a cut — declare known camera exposure jumps in `steps.json`. Detected cuts appear in `exposure_report.json` as `scene_cuts`, and every frame carries its `segment_id`.

The default period `60` is the approximate half-amplitude time scale **in output frames**, not an averaging window. `--strength 0` disables automatic exposure corrections. Sequences shorter than 8 frames are not automatically deflickered, though explicit steps still apply.

### Review states

The analyzer refuses to invent exposure adjustments when there is insufficient usable background, dynamic coverage, or unsaturated data. Default `--on-review error` stops before exporting a final MP4. To export regardless, use `--on-review passthrough`: frames with no reliable estimate render at **0 EV**, while the report retains their `applied_ev=null` and marks the run `PARTIAL`. Do not interpret `PARTIAL` as verified deflicker.

`requested_ev` is the unconstrained solution, `applied_ev` the safe [-2,+2] EV value, and `render_ev` the actual value sent to the renderer (which may be 0 in passthrough). Gain probes can reject a renderer that responds nonlinearly. The frozen-target validation retries no more than twice and does not flatten targets again.

### Brightness modes

`--brightness smooth` (default) removes flicker while preserving real brightness evolution — daytime-to-nighttime ramps and scene-cut level jumps stay visible. `--brightness uniform` instead holds **one constant brightness** across the whole sequence: each frame's common level (median over valid tiles) is corrected to the global median level, so real dimming/brightening, flicker and splice jumps are all flattened. Corrections still respect the ±2 EV safety limits (frames that cannot reach the target are `LIMITED` in the report), and unmeasurable frames still follow `--on-review`. Uniform mode deliberately discards photographic exposure evolution; use it when the clip must look continuously lit.

## Files

```text
out/
  video.mp4              H.264, 8-bit yuv420p, no audio
  exposure_report.json   per-frame EV, errors, pipeline versions, encoder attempts
  exposure_curve.csv     aligned to input filename order
  run.log                CLI progress and errors
  frames/                optional final accepted-frame sRGB PNGs
```

MP4 target bitrate is fixed at **160,000,000 bit/s**; the realized average file bit rate is usually *lower* on easy footage. Default frame rate is 30 fps; any supported rational rate can be supplied.

The encoder selection order is NVIDIA NVENC > AMD AMF (Windows) / AMD VAAPI (Linux AMD device only) > Apple VideoToolbox > software libx264. Each listed codec is **really tested with two full-output-size frames**; a listed codec alone does not prove hardware functionality. When an encoder fails during processing, the sequence restarts at original RAW frame 0 with the next backend. Forced `--encoder` fails rather than silently switching.

<h3>Brightness targets and their provenance</h3>

Every pairwise run reports, per frame, <code>measured_level_ev</code> (the matched-background brightness <code>b</code>, confirmed steps removed in smooth mode) and <code>desired_level_ev</code> (the Huber-Whittaker trend for smooth, or the constant reference level for uniform), with <code>requested = strength * (desired - measured)</code>. Targets are frozen before any render iteration; feedback moves <code>applied_ev</code>, never the target.

Uniform mode holds a <strong>persistent reference</strong>, not a per-frame statistic: the level comes from the pair graph of matched background surfaces. <code>--uniform-reference global</code> (default) stitches disconnected runs to one constant level -- the cross-boundary offset is recorded as an explicit assumption; <code>segment</code> keeps each continuous run at its own level. <code>--period</code> affects smooth mode only and is ignored by uniform. Confirmed exposure steps are preserved by smooth and flattened by uniform, by definition of the two intents.

Reports are <code>schema_version 2</code>: <code>execution_status</code>, <code>gain_response</code> (execution consistency only), <code>render_temporal_quality</code> and <code>encoded_temporal_quality</code> are separate; frames carry measured/desired levels, source capture time and BaselineExposure metadata; JSON output is strictly finite (no NaN/Infinity).

The output colour contract is explicit: pixels are sRGB-OETF-encoded, converted to YUV with a declared BT.709 matrix and limited range, and the stream is tagged accordingly (transfer iec61966-2-1). libx264 writes the full VUI via codec-private parameters; where an encoder drops tags (VideoToolbox on some builds) the run report records <code>colour_contract_warnings</code> instead of staying silent, and <code>verify_video</code> reports the actual average and worst-one-second bitrate.

## Processing details and limitations

- Linear RGB under fixed RAW development settings, one fixed WB for the entire sequence; no auto-brightening or per-frame auto-WB. 16-bit linear LibRaw output is normalized, compensated at RAW development time and finally encoded via the sRGB OETF.
- Exposure photometry uses linear RGB tiles and robust temporal smoothing instead of whole-frame mean luminance. Near-white RGB pixels are only a *proxy* for true sensor saturation.
- Default camera colour transformation is LibRaw's built-in sRGB mapping; no per-camera calibration, lens correction, Log encoding, HDR output or GUI yet. Hooks exist for these future features without expanding the current code path.
- H.264/MP4 is tagged as BT.709 SDR. sRGB and BT.709 transfer functions are not identical; this is **not** stringent colour management.
- Canon EOS R6 Mark III CR3 has publicly reported LibRaw colour-profile risks. Validate actual colour against a known-good converter before using it for critical output. Camera support cannot be certified by RAW decoding alone.
- No video stabilisation, local contrast/color deflicker, optical flow, automatic shot boundaries, advanced white-balance smoothing, highlight reconstruction or RAW-to-Log.
- Memory is bounded per full-resolution frame, while the small N×K measurement matrix covers the whole sequence. Input files are not loaded all at once.
- For 45 MP RAW output expect substantial temporary peak RAM for rawpy, linear RGB, measurement buffers and RGB24 encodes. A 160 Mbps target consumes up to ~1.2 GB/min if attained; allocate space for two temporary candidate MP4s.



## DNG development semantics and source continuity

Real camera DNGs carry per-frame default-development metadata that a raw backend may or may not execute. <code>rawtl</code> never guesses:

- <code>rawtl probe-dng FILE</code> runs controlled byte-preserving probes (a +1 EV BaselineExposure patch and an OpcodeList3 tag neutralisation on temporary copies) and reports whether <em>this backend build</em> actually applies them. On the reference machine's rawpy 0.27.1 / LibRaw 0.22.1 both are provably <strong>ignored</strong> (0.0000 EV pixel delta).
- Every <code>run</code> report includes <code>render_semantics</code> (backend versions, <code>baseline_application_owner</code>, opcode support status, per-opcode payload variation) and <code>source_continuity</code> (nominal interval, capture-time gaps, filename-gap suspicions). GPS and serial numbers are never requested.
- Statistics and exposure trends never cross confirmed capture-time gaps; scene cuts, time gaps, filename suspicions and user-confirmed steps are reported as distinct kinds.
- Adobe-reference development and an opcode-capable backend are listed as <code>not_tested</code> on machines without them; no "full DNG support" is claimed.

Interpretation note for this dataset: the camera raises BaselineExposure (+1.96 EV over the first 77 frames) almost exactly opposite to the sensor's darkening. Since the backend ignores the tag, that intended compensation is absent from the render; the exposure policy tasks decide explicitly whether to reconstruct it, so it is never double-applied.

### Estimators: matched-background pair graph (default) and legacy

<code>rawtl run --estimator pairwise</code> (default) builds a sparse graph of frame nodes connected by robust matched-background brightness differences (lags 1/2/4/8/16 within continuous runs; structure-, colour- and saturation-gated tiles; median + Huber reweighting -- never two independent medians subtracted). A sparse Huber-IRLS solve yields relative brightness <code>b</code> per connected component; targets are defined on <code>b</code>, and tiles under <code>--exclude-rois</code> never participate in fitting (independent acceptance areas). <code>--estimator legacy</code> keeps the previous per-frame-median tile + Whittaker-residual path as a documented baseline for comparison; metrics from the two paths are never mixed.

## Independent output audit (gain pass is not "no flicker")

The render pipeline checks that the RAW backend *executed* the requested EV
(gain probe / frozen-target validation). That says nothing about whether the
corrections themselves were the right ones: a wrong correction curve is still
executed faithfully. The run report therefore separates four statuses:

- <code>execution_status</code> — did the pipeline complete;
- <code>gain_response</code> — did the RAW backend execute the requested EV;
- <code>render_temporal_quality</code> — independent render-domain temporal targets (tracked separately while the legacy frozen-target check remains self-consistent by construction);
- <code>encoded_temporal_quality</code> — an **independent detector** that decodes the produced MP4, inverts the transfer function, reduces in linear light and measures common-mode brightness steps, three-frame residuals, slow-drift guards and candidate jump events on matched tile support. Chroma variation is reported as <code>NOT_IMPLEMENTED</code>, never as 0.

<code>rawtl run</code> audits the encoded video before publishing it. <code>--temporal-quality strict</code> (default) refuses to publish a video whose audit FAILs (<code>TEMPORAL_QUALITY_FAILED</code>, exit 7); <code>report</code> publishes it as <code>COMPLETED_WITH_REVIEW</code> with the event list; <code>off</code> records <code>SKIPPED</code> — never a pass. This gate is independent of the input-metering <code>--on-review</code> switch.

Any existing video can be audited read-only, no RAWs required:

    rawtl audit-video out/video.mp4 -o audit_out \
        --transfer srgb \
        --rois benchmarks/dji-ac004-indoor/rois.json \
        --segments benchmarks/dji-ac004-indoor/segments.json \
        --report out/exposure_report.json

<code>--transfer</code> is mandatory when the stream carries no usable SDR transfer tag; it is never inferred from the pixel format, and HDR/Log streams are rejected instead of silently audited. Statistics never cross source-discontinuity boundaries given via <code>--segments</code>; 1–7-frame runs are reported <code>SHORT_CONTEXT</code> and unmeasurable stretches <code>UNOBSERVABLE</code> rather than zero. See [flicker baseline](docs/flicker-baseline.md) for the real-output evidence this detector reproduced.

## Tests

```bash
python -m pytest -q
rawtl --help
rawtl run --help
rawtl probe --help
```

Tests cover synthetic exposure metrics, input sorting, fixed WB/exposure options via mocks, gain probe, encoder selection, software FFmpeg integration, reporting and cancellation. Test results from a cloud runner are **not** a substitute for real camera and GPU validation.

See [implementation evidence](docs/implementation-notes.md) and [exposure contract](docs/exposure-contract.md).

Licence: see [LICENSE](LICENSE).
