# rawtl-linear-video

Rust driver for the end-to-end linear time-lapse system: enumerate a directory
of DNG frames (LinearRaw or CFA) or of any other RAW format the Adobe DNG
Converter can parse, call the accepted `dng-linear-frame` producer,
apply the selected white balance mode (sequence-frozen first frame by default),
run the legacy V4 deflicker node (enabled by default), stream each accepted
frame through Rust colour conversion into FFmpeg, validate and independently
audit the H.264 result, and publish a single `output.mp4`.

The crate has no external Rust dependencies, no async runtime and no Python in
the product path. Child processes are invoked with `std::process::Command`
using argv vectors (no shell).

## Build

```sh
cargo clean --manifest-path pipeline/Cargo.toml
cargo build --release --manifest-path pipeline/Cargo.toml
cargo test --manifest-path pipeline/Cargo.toml
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
  `sony-slog3-sgamut3-cine`, `nikon-nlog-bt2020`,
  `canon-log2-cinema-gamut-rev-eng-crm-dev`,
  `canon-log3-cinema-gamut-rev-eng-crm-dev`; unknown or empty ids are
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

## Canon Log options

- `canon-log2-cinema-gamut` — **clog2(white paper)**, historical matrix and white-paper curve; shared FP32 handoff.
- `canon-log2-cinema-gamut-rev-eng-crm-dev` — **clog2(rev-eng crm-dev)**,
  Canon CR3 only, SDK CameraToPCS plus its profile HueSatMap; all profiles share signed FP32 AP1/D60 handoff.
- `canon-log3-cinema-gamut-rev-eng-crm-dev` — **clog3 (rev-eng crm-dev)**,
  the independently recovered three-segment Canon Log3 curve, using the same
  CameraToPCS/profile-calibration and FP32 color path as rev-eng Log2. See
  [algorithm, reproduction and evidence](../docs/canon-log3-rev-eng.md).

The two rev-eng Canon options accept `--log-reference-gain N` (positive finite; default 1,
additional to the fixed Canon NormalWhite calibration). This flag is rejected for the other profiles. It applies
once after the Cinema Gamut transform; native user/deflicker EV is already
applied. Every analysis/probe/application/encode pass uses the same camera calibration.
The rev-eng routes apply the interpolated profile HueSatMap in linear ProPhoto
before EV, without LookTable/tone/baseline exposure. Rebuild matched native/Rust
tools; camera algorithm revision3 is required. See [calibration evidence](../docs/canon-profile-calibration.md). The actual video audit divides the reference gain and scene-to-reflection .9 once after inverse
transfer. All Canon options still deliver the existing 8-bit H.264 `output.mp4`.

See [complete algorithm, protocol and evidence](../docs/canon-log2-rev-eng.md)
and [Log2 validation record](../docs/canon-log2-validation.md); Log3 evidence is
linked from its own document. The rev-eng options have not been proven more accurate on a color chart or matched to Canon CR3 official
photo rendering.

## Output layout

```
<output>/
  output.mp4              H.264 in the selected colour/Log profile
  manifest.json           sources, pixel digests, applied EV, geometry, profile,
                          encoder/probe validation and timings
  logs/                   native JSON summaries/byte counts, encoder/audit logs
  exposure_report.json    exposure decisions, measurements, statuses and warnings
  exposure-curve.csv      legacy curve fields, one row per frame
  pairs.csv, frames.csv   audit measurements and source inventory
  temporal_audit.json     when temporal audit executes
```

Analysis and application-pass measurements stay in memory; there are no
`frames/` or `analysis/` directories, raw pixel files or linear master video.
With audit off or deflicker disabled, `pairs.csv` contains only its header and
`frames.csv` contains the source inventory without measured timestamps.

The encoder writes `<output>/.rawtl-partial/output.mp4`. Publication uses one
rename to `output.mp4` after validation, refusing an existing destination.
Successful publication removes the partial directory. Operational failures keep
it and the logs for diagnostics and never publish the final video name.

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
white-balance contract; `per-frame` follows each RAW frame's own as-shot neutral.

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

## Streaming encoding

Analysis, gain probes and feedback application passes call
`--stdout-analysis` (with the requested `--exposure-ev` and ROI). The accepted
EV vector is restored before the final pass. Each final frame calls
`--stdout-frame-f32 --exposure-ev <accepted>`; frame 1's metadata and descriptor
establish source geometry and the WB reference before FFmpeg starts.

All eight profiles use signed FP32 AP1/D60 (`RAWTLST2`, 12 bytes/pixel)
across the validated `rawtl.ap1-f32-frame@1` envelope. Rust uses f64 matrix/curve
arithmetic and fills the final RGB24 backing immediately before FFmpeg stdin.
Negative AP1 and values above 65535/16384 are preserved until target conversion
and final signal clipping. Camera matrix strategy is independent of transport:
only rev-eng selects CameraToPCS and Canon restrictions; other profiles retain
legacy-inverse-cm. One FP32 and one RGB24 frame are held, never the whole sequence.
The existing native RGB48/PNG tools remain explicit compatibility interfaces.
Old outputs may change because early linear quantization/clipping is removed.

The encoder uses `-f rawvideo -pixel_format rgb24 -video_size WxH -framerate FPS
-i pipe:0`, the profile's existing `-vf` filter, `h264_videotoolbox -allow_sw 0
-profile:v high -b:v 0 -q:v 65 -pix_fmt yuv420p`, colour tags and movflags.
The sRGB filter retains its RGB24 to TV-range conversion verbatim. After N
frames stdin is closed and the encoder is reaped. Validation checks nonempty
output, ffprobe codec/geometry/frame count/rate/tags and a full decode. Lossy
H.264 has no lossless roundtrip claim.

## 预览输出色彩/Log profile

`--preview-profile <ID>` 选择预览输出，默认 `srgb-preview`。预览是 sRGB 或厂商
Log 信号；独立审计按所选 profile 反传递后测量。编码器为 `h264_videotoolbox`、8bit `yuv420p`、
`-b:v 0 -q:v 65`；RAW 显影与去闪数学保持。

| id | 预览文件 | transfer / gamut | range | YCbCr | 标签 primaries / transfer / matrix / full |
| --- | --- | --- | --- | --- | --- |
| `srgb-preview` | `output.mp4` | sRGB / Rec.709 | tv | bt709 | 1 / 13 / 1 / 0（既有行为） |
| `sony-slog3-sgamut3` | `output.mp4` | S-Log3 / S-Gamut3 | pc | bt2020nc | 2 / 2 / 9 / 1 |
| `sony-slog3-sgamut3-cine` | `output.mp4` | S-Log3 / S-Gamut3.Cine | pc | bt2020nc | 2 / 2 / 9 / 1 |
| `canon-log2-cinema-gamut` | `output.mp4` | clog2(white paper) / Cinema Gamut | pc | bt2020nc | 2 / 2 / 9 / 1 |
| `canon-log2-cinema-gamut-rev-eng-crm-dev` | `output.mp4` | clog2(rev-eng crm-dev) / Cinema Gamut | pc | bt2020nc | 2 / 2 / 9 / 1 |
| `canon-log3-cinema-gamut-rev-eng-crm-dev` | `output.mp4` | clog3 (rev-eng crm-dev) / Cinema Gamut | pc | bt2020nc | 2 / 2 / 9 / 1 |
| `nikon-nlog-bt2020` | `output.mp4` | N-Log / BT.2020 | pc | bt2020nc | 9 / 2 / 9 / 1 |
| `dji-dlog-dgamut` | `output.mp4` | D-Log / D-Gamut | pc | bt2020nc | 2 / 2 / 9 / 1 |

每个 profile 的矩阵、公式、来源和交付约定均记录在 `manifest.json` 的
`video.color_contract` 中；`video.emitted_tags` 逐字段记录 H.264 SPS VUI 与
MP4 `colr/nclx` 两层的实际码点。2 表示 unspecified；Nikon 的 9 表示其真实
BT.2020 primaries，matrix 的 9 表示实际采用的 BT.2020 非恒定亮度编码矩阵。

所有预设由stdout signed FP32 ACEScg AP1/D60场景线性值开始，经
XYZ(D60) → Bradford CAT → XYZ(D65) → 目标线性 RGB；逐通道裁切到官方反函数
signal 0/1 的端点，再应用官方 transfer 和 `round(signal*255)`、ties away from
zero 的 8bit 量化。不做 tone/gamut mapping，不加抖动。8bit Log 可能出现 banding；
无 dither 是 owner 决定，10bit/HEVC/ProRes/HLG/PQ 等更丰富输出仅记录为未来方向。

本次交付标签修订（2026-11-09）将厂商 profile 的标签策略从「一律不写通用色彩字段」改为逐字段：写真实
matrix/range，Nikon 另写真实 primaries；没有准确通用码点的厂商 transfer 和其它
primaries 保持 unspecified。`scale=out_color_matrix=bt2020:out_range=pc` 后的
`setparams` 和编码参数保持这些真实字段一致。本机 FFmpeg 8.0.1/VideoToolbox 能直接
写入 SPS VUI；封装用 `-movflags +faststart+write_colr`，确保部分字段未知时仍写 nclx。
sRGB 的滤镜、色彩编码参数和 `+faststart` 保持原样。
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
Sources at or below 4096 keep the existing geometry and filter chain. The applied geometry,
algorithm and the exact `-vf` chain are recorded in `manifest.json`
(`video.scale`, `video.encoder_parameters`). Validation checks the applied
output dimensions; `frames.width/height` retain source geometry.

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
`analysis_highlight_fraction` is analysis-only AP1 threshold occupancy (EV-dependent), not final video clipping. Analysis schema is 2, with no fixed FP32 quantum; the historical floor remains an estimator parameter.

Enabled path:

1. **Analysis pass** — every frame is decoded at 0 EV with
   `--stdout-analysis`, using the accepted native producer and the
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
3. **Estimation** — the Rust engine (`rawtl_linear_video::deflicker`) runs the
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
   and `--stdout-analysis`, retaining only in-memory metadata and applied-EV
   measurements. `analysis_highlight_increase_fraction` is the per-tile positive
   increase of `analysis_highlight_fraction` over the 0 EV baseline divided by tile
   count. The threshold is 0.980010986328125, quality `analysis_ap1_threshold`.
7. **Render validation and feedback** — the applied measurements are compared
   with the frozen targets (`median <= 0.01 EV`, `P95 <= 0.03 EV`, no severe
   frame) using one shared offset. At most two feedback updates change
   `applied_ev` only (damping 0.75, step 0.25); divergence aborts and keeps the
   best round. A best round that still fails is published with a
   `RENDER_TARGETS_NOT_REACHED` warning and `COMPLETED_WITH_REVIEW` — never
   blocked. All-unobservable validation is reported as
   `NOT_VERIFIED_NO_CONSENSUS` / `NOT_INDEPENDENT_LEGACY`, never as a pass.
8. **Streaming encode / validate** — the restored accepted EV vector is used
   by a final native decode and the streaming encoder described above.
9. **Independent audit** — the sole MP4 is decoded to `rgb48le`; normalized
   signals pass through the selected profile's inverse transfer before
   target-gamut luminance, f64 BOX reduction and the 24x18 tile measurements.
   Boundaries remain scene cuts union capture-time gaps. The delivered-code
   saturation proxy remains `code >= int(0.965 * 65535)`, distinct from the
   analysis sidecar's linear-scale clip series. A FAIL or audit ERROR records
   warnings and `COMPLETED_WITH_REVIEW`; audit never blocks publication.
   `off` records SKIPPED.
10. **Reports and publication** — `exposure_report.json` and
    `exposure-curve.csv` are written atomically before the manifest and
    publication. Any failure after the guarded output directory exists retains
    a report, including required-executable (exit 20) and source-enumeration
    (exit 10) failures before the first frame (the report may carry an empty
    frame list); guard/usage rejections happen before report initialization and
    produce none. A report-write failure on the failure path does not mask the
    original error. The manifest gains a `deflicker` block with config, status counts,
    probe and audit summaries. Atomic publication is unchanged.

## Audit assumptions and provenance

The `VIDEO_ARTIFACT` envelope's `color.profile` is the authoritative identity.
FFprobe transfer, primaries, matrix and range tags cross-check that identity.
Expected-unspecified vendor Log tags are legal; an explicit conflicting tag
produces audit ERROR with no events. A missing or unresolved profile identity
produces `TEMPORAL_AUDIT_UNOBSERVABLE`, never a guessed curve. Other decoder or
measurement errors remain ERROR. The caller records either outcome in the
report and `temporal_audit.json` without blocking publication.

All eight selected profiles use `color_profile::Transfer::inverse`, the same
curve owner as forward conversion. Linearization recovers target-gamut linear
values within clipping, 8-bit quantization and H.264 losses; it does not undo
those losses or the AP1-to-target transform. Audit luminance uses the target
gamut's XYZ Y weights. Smooth target trajectories can cross quantization bins
and be flagged by the unchanged temporal thresholds. Controlled-decoder tests
assert known EV changes for every profile, alongside encoded-output tests.

The audit records artifact path, byte length, ffprobe tags and profile identity.
Encoded artifact SHA-256 is honestly `NOT_COMPUTED_STD_ONLY`; no pixel digest is
substituted for an artifact digest, and no lossless-content proof is claimed.
Native `pixels_sha256` is verified against each FP32 payload before conversion. Manifest and color pipeline revision are 2; `final_signal_clipping` records final transfer-domain clipping for every profile.

`--strength 0` uses one streaming frame pass at 0 EV, skips analysis/probe/
feedback/audit, and records DISABLED statuses. The native positional PNG mode
remains available independently of this default Rust workflow.

## Stage handoffs (typed transports)

The existing transport registry remains unchanged. Subprocesses exchange
stdout records and stdin pixel bytes; every Rust stage consumes the typed
payload it validates.

- **Native JSON -> measurement:** protocol v1 begins with `RAWTLST1` and
  ordered `[type:u8, zero:u8×3, length:u64 LE, payload]` records. Analysis mode
  requires metadata (0x01) then analysis (0x02). Their bytes become
  `MemoryFile` envelopes, `rawtl.frame-metadata@1` and
  `rawtl.analysis-sidecar@1`; source hash/provenance, geometry, WB and EV
  checks run before the schema-2 measurement parser. Transport envelope @1 is retained; source.schema_version is 2. No sidecar is persisted.
- **Native pixels -> encoder:** frame mode requires 0x01, descriptor 0x03,
  then 0x04 signed FP32 AP1/D60 (`RAWTLST2`). Types, reserved fields, ordering, geometry-derived length,
  truncation and trailing bytes are rejected. The descriptor's native pixel
  digest is independently verified. Pixels become an F32 HWC little-endian
  `rawtl.ap1-f32-frame@1` NdArray; `ColorProfile::convert_f32_frame` fills a unique
  backing for the U8 `rawtl.rgb24-frame@1` envelope written to stdin. Neither
  envelope requires a full-frame copy. Complete records followed by a nonzero
  native exit still fail before encoder writes.
- **Encoded video -> audit/publication:** the sole `video/mp4` DiskArtifact
  becomes `rawtl.video-artifact@1`, with `audit.codec`, `source.frame_count`,
  `color.profile`, `color.transfer` and `color.space`. Audit and publication
  resolve this same validated reference. Manifest frame entries contain source
  identity, source geometry, native pixel digest, applied EV, observed range,
  quantization, duration and log reference; WB detail lives in run metadata
  and per-frame JSON logs.

## Failure semantics and exit codes

Any operational frame/encoder failure stops further dispatch, closes owned
pipe endpoints, terminates live counterparts and reaps both owned children
before returning. Logs retain actual process statuses, JSON summaries and
pixel byte counts; binary pixel payloads are never logged. Primary frame and
encoder failures retain families 30 and 60 respectively. A single final rename
refuses overwrite; `.rawtl-partial` remains on failure.

| Exit | Meaning |
| --- | --- |
| 0 | output.mp4 published |
| 2 | CLI usage error |
| 10 | Input enumeration/ordering error |
| 11 | Output/input overlap or invalid output path |
| 12 | Output directory is not empty |
| 20 | Required executable (native tool/ffmpeg/ffprobe) unavailable |
| 30 | Frame production or frame-metadata verification failure |
| 40 | Reserved legacy encode family; unused by default workflow |
| 50 | Probe/tool output parsing or typed artifact validation failure |
| 60 | H.264 streaming conversion/encode/validation failure |
| 70 | Manifest publication or final rename failure |
| 81 | Gain-probe tool failure (probe verdicts themselves are recorded flags) |
| 82 | Render-validation internal error (target misses are recorded flags) |

Deflicker estimate completeness, probe verdicts, render-target misses and the
encoded-audit verdict never stop a run: they are recorded as warnings, frame
statuses and manifest fields, and the deliverables are published as
`COMPLETED_WITH_REVIEW` (exit 0). The removed exit codes 80/83 belonged to the
frozen legacy gate contract (`--on-review error`, `--temporal-quality strict`).

## Tests

`cargo test --manifest-path pipeline/Cargo.toml` runs numerical/transport
unit tests and integrations with real FFmpeg and a standalone fake native
producer. Coverage includes stdout modes, malformed framing/metadata, short
pixels, mid-run failure, late nonzero native exit, encoder early exit while the
native producer is blocked, bounded completion and no surviving owned child.
Positive tests assert the single-video output layout, numeric ordering, WB,
manifest/profile tags, pixel digests and logs without binary dumps. Feedback
under-execution still exercises multiple rounds and the true update count.
Faithful-gain PASS tests use a uniform target to avoid quantization-bin jumps
in the old short-sequence smooth-target fixture. Controlled decoded-frame
tests measure known ±1 EV changes through production audit for every profile,
including valid unspecified Log tags, explicit conflicts and absent identity.

Real-material runs are opt-in:

```sh
pipeline/target/release/rawtl-linear-video \
  --input /Users/ibobby/Temp/tl \
  --output outputs/linear-video-pipeline-e2e \
  --dng-linear-frame <matched dng-linear-frame build>
```

## Non-goals

No plugin ABI, events, Stage 1 cache, lossless master, GPU, packaging or
Python runtime. The deflicker node uses no Python/SciPy/rawpy. Multi-ROI audit,
chroma-variation analysis and progress callbacks remain out of scope. The frozen
legacy Python implementation remains read-only and is not a numerical oracle for
this ACEScg chain.

Fixed Canon calibration and new LibRaw metadata dependency: [contract and reproduction](../docs/canon-fixed-calibration.md).
