# Implementation evidence and environment

Task-by-task development log for the RAW time-lapse CLI. Tasks T0–T10 are conventional commits; no SFD workflow.

## T0 baseline (2026-10-30)

- Repository default branch: `main`; initial commit contained `LICENSE` and a minimal `README.md`.
- Available isolated execution environment: Linux x86_64, Python 3.13.5 (project target 3.12), NumPy 2.3.5, SciPy 1.17.0, Pillow 12.3.0, pytest 9.0.2.
- FFmpeg and ffprobe: Debian FFmpeg 7.1.5, libx264 enabled. No detected /dev/dri GPU device.
- rawpy: **not installed** in this isolated environment. No real user RAW files are present. RAW rendering, camera colour (including R6 Mark III CR3) and hardware encoders are **NOT TESTED**, not presumed functional.
- Offline execution environment cannot install dependencies from package registries. Synthetic algorithm and FFmpeg CPU tests can still run.
- Input sorting contract: first directory level, case-sensitive Unicode lexical filename order.
- Exposure normative design: linear RGB under fixed render settings; 12×8 tiles; 5% trimmed mean, Huber-Whittaker trends; spatial consensus; explicit-only steps; distinct requested/applied/render EV; bounded gain validation; conservative review states.
- Video target: 160,000,000 bits/s (20 decimal MB/s), H.264 8-bit yuv420p, fixed fps. Hardware preference NVENC > Windows AMF/Linux AMD VAAPI > macOS VideoToolbox > software x264.

## Verification log

Append concrete commands, results and remaining limitations as each implementation task completes. Never equate mock results with real camera or GPU validation.

## T1–T10 implementation status

- T1: src-layout package, CLI and Python type/protocol contracts.
- T2: flat deterministic inventory, strict metadata equality and source manifest.
- T3: fixed four-channel WB, LibRaw linear 16-bit sRGB render, conservative near-clipped proxy, sRGB OETF, even-dimension edge padding and no-op frame processing hook.
- T4: ROI tiled linear photometry, 5% trimmed means, five-diagonal Huber-Whittaker IRLS, cross-tile consensus, explicit EV steps and conservative missing-value handling.
- T5: representative RAW ±0.25EV gain probes, frozen-target comparison, max-two bounded feedback updates.
- T6: actual target-dimension encoder probe, platform priority and CPU fallback, bounded rawvideo pipe, H.264 output verification using ffprobe.
- T7: staged video/PNG output and iterative end-to-end streaming service.
- T8: CLI status output, run.log, strict JSON/CSV diagnostic exports.
- T9: synthetic/mocked algorithm, state, cancellation, FFmpeg integration tests, platform GitHub Actions workflow.
- T10: documentation and version pins.

**Verification distinction**: GitHub Actions can verify Python and synthetic CPU software encoding on CI; it does not prove GPU hardware encoding or real-camera CR3 compatibility. No user RAW files were included. Camera model / white-balance / colour correctness remain unverified until real source sequences are supplied.

## RAW API note

rawpy 0.27.1 has no guaranteed public camera make/model field; optional ExifTool can read it in batches, otherwise `UNKNOWN_CAMERA` with an explicit warning is reported. Raw pattern / matrix fingerprints catch some incompatible sequences but are not a model identity guarantee. Three-colour sensors may report an unused fourth WB coefficient as zero; V1 infers G2=G only when `num_colors==3` and validates the resulting four positive entries. Real-camera validation is still required.

## Quantitative synthetic deflicker limits

A severe **300-frame** case with 25% moving photometric pollution and 0.015 EV observation noise measured **0.0278 EV RMSE** on reliable frames in Linux, macOS, and Windows CI. A strict 0.06 EV spatial MAD threshold rejected two terminal frames as `REVIEW_REQUIRED`. This is a genuine short-sequence limitation, not a reason to lower the algorithm's reliability threshold. Its regression target is RMSE <0.035 EV, P95 <0.065 EV, and no more than 3 rejected edge frames.

The separately retained **900-frame** synthetic benchmark (96 tiles, same contamination/noise) retains the stricter original objective: RMSE <0.025 EV, P95 <0.04 EV, and at least 897 reliable frames. These are synthetic tests, not real camera or LRT comparisons.
## Uniform brightness mode (2026-10-31)

`--brightness smooth|uniform` (default `smooth`, the scene-cut deflicker above). `uniform` holds one constant output level: each frame's common level (median of step-adjusted valid tiles, same >=max(8,ceil(K/4)) tile floor) is corrected to the global median level with the existing strength scaling, [+/-2] EV clipping (status `LIMITED`), review semantics and frozen-target validation; scene cuts become report metadata. Real day->dusk ramps and splice jumps are deliberately flattened. Estimator branch in `estimate_exposure`; wired through `ProcessingRequest.brightness`, service config report and CLI.

Verification: `pytest -q` with ffmpeg on PATH: **56 passed** (new tests/test_brightness_uniform.py plus a SplicedRaw uniform end-to-end asserting the +1.25 EV median splice jump is fully removed). Real dataset with `--brightness uniform`: exit 0, run_status OK, status counts {UNIFORM: 119}, requested/applied EV range [-1.854, +1.359] (no LIMITED), output level spread 0.0000 EV, gain probe and frozen-target validation passed, VideoToolbox 119 frames verified.
## Scene-cut segmentation (2026-10-31, macOS, real DJI DNG data)

Motivation: a real 119-frame input (DJI AC004 DNGs, filename range TIMELAPSE_0003-0862 kept in 10 disjoint runs) aborted with `exposure estimate incomplete` — 56/119 frames REVIEW_REQUIRED because splice jumps (+1.56 EV median, 0.82 EV cross-tile MAD at the 0079→0719 boundary) poisoned the single-timeline Whittaker trend, and tail edge gaps are never interpolated by design.

Change: `estimate_exposure` now detects content jumps between adjacent frames on step-adjusted tile log-luminance (cut iff >=max(8,ceil(K/4)) common valid tiles AND |median delta| >= 0.20 EV AND scaled cross-tile MAD >= 0.08 EV; module constants `SCENE_CUT_MEDIAN_EV` / `SCENE_CUT_TILE_MAD_EV`). Each segment is smoothed and anchored independently; segments <8 frames pass through as SHORT_SEQUENCE_NO_DEFLICKER; confirmed steps are subtracted before detection so they never cut. Reports gain `scene_cuts`; `frames[].segment_id` now comes from the estimate (was the unused `FrameRef.segment_id`). Real-data pair statistics behind the thresholds: within-run max |median| 0.17 EV / max MAD 0.077 EV; detected cuts at indices 77 (1.559/0.818), 79 (0.278/0.095), 80 (0.243/0.089).

Verification (local, ffmpeg-full 8.0.1 via /opt/homebrew/opt/ffmpeg-full/bin — the formula is installed unlinked, so test/acceptance commands need that PATH prefix or an explicit `--ffmpeg`):
- `pytest -q` with ffmpeg on PATH: **50 passed, 0 skipped** (previously 38 passed / 6 skipped without PATH); new tests in tests/test_scene_cut_segmentation.py plus a SplicedRaw end-to-end in tests/test_service_integration.py.
- Real dataset: `python -m rawtl run /Volumes/4T/temp/test -o /Volumes/4T/temp/scene-cut-run --ffmpeg /opt/homebrew/opt/ffmpeg-full/bin/ffmpeg` → exit 0, run_status OK, scene_cuts [77,79,80], status counts {OK:114, INTERPOLATED_ESTIMATE:2, SHORT_SEQUENCE_NO_DEFLICKER:3}, zero REVIEW_REQUIRED, gain probe and frozen-target validation passed, VideoToolbox H.264 3648x2736, 119 frames verified by ffprobe.

Known limitation: a splice statistically indistinguishable from common flicker (e.g., median -0.088 EV / MAD 0.012 at 0856→0862) is not cut; the trend absorbs it. Thresholds are calibrated constants, not a universal splice detector; contract updated to V2.

## Synthetic end-to-end on software encoder
Alongside the disabled-exposure RAW mock test, a regression now exercises **30 mock RAWs, automatic exposure ON**, fixed gain probe, per-tile time-smoothing, FFmpeg libx264 output, `ffprobe` frame count and frozen-target validation. CPU software encoding is actually executed in CI; no physical RAW sensor or real NVENC/AMF/VAAPI/VideoToolbox accelerator is implied. The final result always restores the selected candidate's encoder, video metadata and per-frame render parameters together.

## 2026-11-05 — real-output flicker baseline (F0) and independent temporal audit (F1)
External review of the uploaded 119-frame uniform-mode output measured residual common-mode flicker that the pipeline's own gain/frozen-target checks cannot see (those checks are self-consistent with whatever correction was proposed). Established:

- **F0 baseline.** Local reproduction of the review's full-precision decode audit matches every statistic to 1e-8 EV (first 77 frames: adjacent P95 0.0678 EV, max 0.0999 EV, 3-frame RMS 0.0262 EV, 9 events over 0.05 EV; events 37/38/40/73/75/76 reproduce sign and magnitude). A full 119-frame rerun at the reviewed commit produces per-frame applied EV identical to the uploaded report (max diff 0.0); the MP4 differs only by VideoToolbox run-to-run non-determinism and matches all audit statistics to 5 decimals. Benchmark config committed under benchmarks/dji-ac004-indoor/; hashes/capture times stay in the local untracked manifest.
- **F1 audit.** src/rawtl/quality/ implements the fixed-camera paired-photometric detector (RGB48 decode, declared inverse transfer, linear-light reduce, matched-support tile pairs; NumPy/SciPy only). On the uploaded video it reports the same 18 candidates with event values within 6e-4 EV of the reference implementation. New rawtl audit-video subcommand audits any existing MP4 read-only; rawtl run now gates publication on the audit (--temporal-quality strict/report/off) and reports execution_status, gain_response, render_temporal_quality (explicitly NOT_INDEPENDENT for now) and encoded_temporal_quality. 76 tests pass with FFmpeg on PATH.

## 2026-11-05 — DNG semantics and continuity (F3)

Controlled byte-preserving probes (rawtl probe-dng; +1 EV BaselineExposure patch and OpcodeList3 tag neutralisation on temp copies) show this backend build (rawpy 0.27.1 / LibRaw 0.22.1) executes NEITHER: 0.0000 EV pixel delta on both probes. Real-data acceptance on the 119-frame benchmark: 9 capture-time gaps (76->77 = 38400 s), 10 segments, BaselineExposure present in all 119 frames with 76 unique values spanning [0.96, 3.04] EV, OpcodeList3 present in all frames with WarpRectilinear constant (1 unique payload) and GainMap unique per frame (119 payloads). Adobe-reference development and an opcode-capable backend remain not_tested. Estimation statistics now never cross confirmed time gaps.
