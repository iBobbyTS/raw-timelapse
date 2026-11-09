# rawtl-linear-video

Rust driver for the minimal end-to-end linear time-lapse system: enumerate a
directory of CFA/LinearRaw DNG frames, call the accepted `dng-linear-frame`
producer once per frame, freeze the sequence white balance, encode a lossless
16-bit scene-linear FFV1 video, validate it by full decode roundtrip, render a
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
  [--converter-timeout 120]
```

Defaults:

- `--dng-linear-frame native/build/dng-linear-frame` (the documented native
  build location; pass the matched build path when using a work-directory build)
- `--ffmpeg /opt/homebrew/opt/ffmpeg-full/bin/ffmpeg`
- `--ffprobe /opt/homebrew/opt/ffmpeg-full/bin/ffprobe`
- `--fps 30` — the frozen legacy default (`legacy/python/src/rawtl/cli.py:91`,
  `Fraction(30)`). Rational rates such as `30000/1001` are accepted; the
  positive-rate and `<= 1000` bounds match the legacy parser.

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
  logs/frame_%06d.log     native command line, exit status, stdout/stderr, timing
  logs/encode-linear.log
  logs/roundtrip.log
  logs/render-preview.log
  logs/encode-preview.log
  manifest.json           run provenance, frozen WB, fps provenance, encoder
                          parameters, probe results, roundtrip and preview checks
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
encodes with libx264
(`-preset medium -crf 18`) and tags
`bt709`/`bt709`/`iec61966-2-1`/`tv`. The preview is validated with ffprobe
(`h264`, `yuv420p`, dimensions, frame count, frame rate) and a full decode to
`null`.

Unit tests assert neutral (`0.18 → 118,118,118`), a non-neutral reference
(`[0.5,0.25,0.125] → [216,129,89]`), the two-sided clipping policy, the sRGB
transfer, and that treating AP1 values as display sRGB is rejected by the
non-neutral test.

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

## Tests

`cargo test` runs unit tests plus integration tests that use the real ffmpeg at
the default path and a fake native producer (a shell script writing tiny
16-bit PNG fixtures and schema_version 1 sidecars). The fake producer covers:
numeric ordering (`IMG_1, IMG_2, IMG_10`), frozen-WB propagation and detection
of a producer that ignores the override, fail-fast on a bad frame, encoder
failure after bytes are written, ffprobe rejection, roundtrip mismatch, and
directory guards. Real-material runs are opt-in:

```sh
pipeline/target/release/rawtl-linear-video \
  --input /Users/ibobby/Temp/tl \
  --output outputs/linear-video-pipeline-e2e \
  --dng-linear-frame <matched dng-linear-frame build>
```

## Non-goals

No deflicker/exposure estimation, plugin ABI, graph engine, events, Stage 1
cache, official Log output, GPU, packaging, or Python runtime. The frozen
legacy Python implementation remains read-only and is not a numerical oracle
for this ACEScg chain.
