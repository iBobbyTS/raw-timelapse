# rawtl-linear-video

Rust driver for the end-to-end linear time-lapse system: enumerate a directory
of DNG frames (LinearRaw or CFA) or of any other RAW format the Adobe DNG
Converter can parse, call the accepted `dng-linear-frame` producer,
apply the selected white balance mode (sequence-frozen first frame by default),
run the legacy V4 deflicker node (enabled by
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
  [--white-balance first-frame|per-frame] \
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
- `--white-balance first-frame` — the default and the historical behaviour: the
  first numerically ordered frame's as-shot neutral is frozen for the whole
  sequence. `per-frame` decodes every frame with its own RAW as-shot neutral
  (see *White balance modes* below).
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
  manifest.json           run provenance, white-balance mode/provenance, fps
                          provenance, encoder parameters, probe results,
                          roundtrip/preview checks and the deflicker block
                          (config, status counts, audit summary)
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

## White balance modes (--white-balance)

`--white-balance` selects how the scene-linear decode derives its camera
neutral. The default, `first-frame`, keeps the historical sequence-frozen
contract and produces byte-identical arguments, manifest and log lines to
earlier runs; `per-frame` follows each RAW frame's own as-shot neutral.

- `first-frame` (default, REQ-008): the first numerically ordered frame is
  decoded with its own AsShotNeutral (`white_balance_source: "as-shot"`). Its
  `applied_white_balance` triple is then passed to every later frame as
  `--white-balance R,G,B` (camera-neutral domain, not reciprocal gains). Each
  later sidecar must report `white_balance_source: "override"` and exactly the
  same applied triple; a mismatch stops the run.
- `per-frame`: every frame is decoded with its own RAW AsShotNeutral. The
  pipeline passes no `--white-balance` to the native producer in any pass —
  deflicker-disabled single pass, baseline analysis, render and gain probe
  alike. Each sidecar must report `white_balance_source: "as-shot"` with
  `applied_white_balance` exactly equal to that frame's `as_shot_neutral`.

In both modes every frame keeps its own `as_shot_neutral`, and any sidecar that
reports `as-shot` must have applied exactly that neutral: a producer that lies
about its source or its applied value is rejected at the frame layer instead of
becoming the frozen reference. `per-frame` renders the camera's per-frame white
balance by design, so colour temperature may drift between frames. The
deflicker node measures and corrects exposure brightness only; it never smooths
or corrects white balance. The chosen mode is recorded in `manifest.json`
(`white_balance.strategy`, with `reference*` fields null in `per-frame` mode)
and in the run summary. No per-frame "auto" white balance is performed:
as-shot *is* the white balance data stored in the RAW.

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

## 预览输出色彩/Log profile

`--preview-profile <ID>` 选择预览输出，默认 `srgb-preview`。预览是 sRGB 或厂商
Log 检视信号，不是测量源。编码器仍为 `h264_videotoolbox`、8bit `yuv420p`、
`-b:v 0 -q:v 65`；FFV1 母版、RAW 显影、去闪、审计均不改变。

| id | 预览文件 | transfer / gamut | range | YCbCr | 标签 primaries / transfer / matrix / full |
| --- | --- | --- | --- | --- | --- |
| `srgb-preview` | `preview-h264-srgb-bt709.mp4` | sRGB / Rec.709 | tv | bt709 | 1 / 13 / 1 / 0（既有行为） |
| `sony-slog3-sgamut3` | `preview-h264-slog3-sgamut3.mp4` | S-Log3 / S-Gamut3 | pc | bt2020nc | 2 / 2 / 9 / 1 |
| `sony-slog3-sgamut3-cine` | `preview-h264-slog3-sgamut3cine.mp4` | S-Log3 / S-Gamut3.Cine | pc | bt2020nc | 2 / 2 / 9 / 1 |
| `canon-log2-cinema-gamut` | `preview-h264-clog2-cgamut.mp4` | Canon Log 2 / Cinema Gamut | pc | bt2020nc | 2 / 2 / 9 / 1 |
| `nikon-nlog-bt2020` | `preview-h264-nlog-bt2020.mp4` | N-Log / BT.2020 | pc | bt2020nc | 9 / 2 / 9 / 1 |
| `dji-dlog-dgamut` | `preview-h264-dlog-dgamut.mp4` | D-Log / D-Gamut | pc | bt2020nc | 2 / 2 / 9 / 1 |

每个 profile 的矩阵、公式、来源和交付约定均记录在 `manifest.json` 的
`preview.color_contract` 中；`preview.emitted_tags` 逐字段记录 H.264 SPS VUI 与
MP4 `colr/nclx` 两层的实际码点。2 表示 unspecified；Nikon 的 9 表示其真实
BT.2020 primaries，matrix 的 9 表示实际采用的 BT.2020 非恒定亮度编码矩阵。

转换顺序不变：PNG RGB16 的 `code/16384` 为 ACEScg AP1/D60 场景线性值，经
XYZ(D60) → Bradford CAT → XYZ(D65) → 目标线性 RGB；逐通道裁切到官方反函数
signal 0/1 的端点，再应用官方 transfer 和 `round(signal*255)`、ties away from
zero 的 8bit 量化。不做 tone/gamut mapping，不加抖动。8bit Log 可能出现 banding；
无 dither 是 owner 决定，10bit/HEVC/ProRes/HLG/PQ 等更丰富输出仅记录为未来方向。

本次交付标签修订（2026-11-09）将厂商 profile 的标签策略从「一律不写通用色彩字段」改为逐字段：写真实
matrix/range，Nikon 另写真实 primaries；没有准确通用码点的厂商 transfer 和其它
primaries 保持 unspecified。`scale=out_color_matrix=bt2020:out_range=pc` 后的
`setparams` 和编码参数保持这些真实字段一致。本机 FFmpeg 8.0.1/VideoToolbox 能直接
写入 SPS VUI；封装用 `-movflags +faststart+write_colr`，确保部分字段未知时仍写 nclx。
sRGB 的滤镜、编码参数和 `+faststart` 保持原样，解码 framemd5 和图像 NAL 回归不变。
`pc` 流允许解码器报告 `yuv420p` 或 `yuvj420p`，仍校验 range 和逐字段标签。

厂商 profile 统一 `bt2020nc` 和 full range 是 **rawtl 交付约定，不是厂商强制要求**。
系数采用 [ITU-R BT.2020-2 (10/2015) Table 4](https://www.itu.int/rec/R-REC-BT.2020-2-201510-I/en)，
码点采用 [H.273 (V4), 07/2024, Table 4](https://www.itu.int/rec/T-REC-H.273-202407-I/en)，
并以 [Khronos Data Format v1.4.0 §15.1.3](https://registry.khronos.org/DataFormat/specs/1.4/dataformat.1.4.html)
交叉核对理想矩阵变换；只有该理想变换可逆，实际 8bit 4:2:0 包含量化、色度下采样和
压缩损失。矩阵标签不代表把厂商 primaries/transfer 转成 BT.2020，也不依据色域包含
关系作选择。

背景证据：[Canon DP-V3010 手册 pp.47/50](https://gdlp01.c-wss.com/gds/7/0300013757/06/dpv3010-im3-en.pdf)
的 Cinema Gamut 监看工作流采用 BT.709 YCbCr；[Sony BVM-HX310 手册 pp.24–25](https://pro.sony/support/res/manuals/4748/2c99048095dffc320734089caca25c96/47481881M.pdf)
要求接收端矩阵匹配信号输出设备；[Nikon N-Log LUT 指南 step 7](https://nps.nikonimaging.com/technical_info/technical_solutions/z9_tips/3d_lut/)
同时说明 limited/full 链路。Nikon 规格给出 BT.2020 primaries，但 NCL/full 交付选择
属于 rawtl；Nikon/DJI 的曲线码域不能证明 full range 是厂商规定。

| profile | 官方公式/色域出处 | 转写与核对 |
| --- | --- | --- |
| Sony S-Gamut3 / S-Gamut3.Cine / S-Log3 | [Technical Summary V1.0](https://www.sony.jp/ls-camera/knowledge/pdf/TechnicalSummary_for-S-Gamut3Cine_S-Gamut3_S-Log3_V1_00.pdf) | S-Log3 正反公式、0/18/90% 码 95/420/598、primaries 与 D65 |
| Canon Log 2 / Cinema Gamut | [Canon Log Gamma Curves (2018-11-01)](https://downloads.canon.com/nw/learn/white-papers/cinema-eos/white-paper-canon-log-gamma-curves.pdf)；[Color Gamut Conversion for DCI-P3+ and Cinema Gamut, PUB. DIM-1102-000 (2013), EN p.2](https://gdlp01.c-wss.com/gds/8/0200003438/01/colorgamutinfo-dim1102.pdf) | 官方 Cinema Gamut 白点 (0.3127,0.3290)、R(0.7400,0.2700)、G(0.1700,1.1400)、B(0.0800,-0.1000)，现有数值不变；colour-science/Canon ACES IDT 保留为交叉核对 |
| Nikon N-Log / BT.2020 | [N-Log Specification Document 1.0.0 (2018-09-01)](http://download.nikonimglib.com/archive3/hDCmK00m9JDI03RPruD74xpoU905/N-Log_Specification_(En)01.pdf) | N-Log 公式、BT.2020 primaries 与 D65，参考码从官方公式推导 |
| DJI D-Log / D-Gamut | [D-Log and D-Gamut White Paper (2017-10-10)](https://dl.djicdn.com/downloads/zenmuse+x7/20171010/D-Log_D-Gamut_Whitepaper.pdf) | 官方公式、0/18/90% 码 95/408/586、primaries/D65；使用由官方原色推导的矩阵，与打印四位小数矩阵差≤1.84e-4，保持中性灰 |

测试逐项覆盖官方参考码、中性灰、往返、独立非中性色块、Sony 双色域差异、两层
标签的独立读取及一致性。每个厂商 profile 均从文件自身标签自动解码到 RGB，和
强制正确矩阵/范围及独立色块参考比较；错误矩阵和错误范围仍须被数值断言拒绝。
真实 NLE/播放器的 Log 解释与显示效果仍需使用者核验，不声称校准显示匹配。

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
   selected white balance mode (the frozen frame-1 override in `first-frame`;
   each frame's own as-shot neutral, with no override, in `per-frame`). Only
   measurements are kept. The
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

## Stage handoffs (typed transports)

Every in-process stage boundary hands its data on through the transport
contract in `pipeline/src/transport/`: a `DiskArtifact`, `MemoryFile` or
`NdArray` payload inside an immutable `DataEnvelope` with a product semantic
type and provenance. Subprocess boundaries (native producer, FFmpeg) remain
path/argv based; only the handoff between pipeline stages is typed.

- **Native producer -> pipeline.** The producer still receives the historical
  argv paths. Its outputs are referenced as `DiskArtifact` (`image/png` for the
  frame PNG, `application/json; charset=utf-8` for the metadata and analysis
  sidecars). Sidecar content is read with `transport::load`, wrapped as a
  `MemoryFile` and validated as `rawtl.frame-metadata@1` /
  `rawtl.analysis-sidecar@1` before the existing parsers run. The envelope
  metadata carries `source.schema_version`, `source.width`, `source.height` and
  (when present) `source.sha256`; the source hash must also appear in the
  provenance, so a sidecar whose declared hash does not match the frame is
  rejected. The measurement consumers receive the bytes of that same validated
  envelope instead of re-reading an untyped file.
- **Preview RGB48 pipe and lossless roundtrip.** Each `rgb48le` frame read from
  the decoder pipe is constructed as an `NdArray` (`U16`, `[height, width, 3]`,
  HWC interleaved, little endian, offset 0) and validated as
  `rawtl.rgb48-frame@1` against the declared width/height and colour metadata;
  a buffer whose length does not match the declared geometry is rejected before
  conversion. The existing `ColorProfile` conversion runs on that validated
  array, and its packed output is validated as `rawtl.rgb24-frame@1` before it
  is written. `verify_roundtrip` constructs and validates the frames from both
  decoders the same way before the byte comparison.
- **Encoded deliverables -> audit/publish.** The FFV1 master and preview video
  are handed on as `DiskArtifact` references (`video/x-matroska`, `video/mp4`)
  validated as `rawtl.video-artifact@1` (`audit.codec`,
  `source.frame_count`). The audit decodes the referenced file, and publication
  renames exactly the validated references.

The typed layer only adds rejections: argv, sidecar/manifest byte formats, exit
codes, log-line semantics and deliverable content are unchanged.

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
