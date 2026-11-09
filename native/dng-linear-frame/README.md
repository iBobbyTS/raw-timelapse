# dng-linear-frame

C++20 single-frame producer for S01 / T08 (T06/T07/T09 semantics). macOS only.

## Build

Requires CMake ≥3.24, Apple clang, libpng, libjpeg, zlib and libjxl (including
`libjxl_threads`). The Adobe SDK and XMP core are compiled from the supplied
sources; libjpeg/libjxl/libpng are linked from the local dependency installation.
No Python is used by the tool. Python 3 is used only by tests and setup examples.

Unpack the **licensed** SDK zip into `native/vendor/dng-sdk-1.7.1/`, stripping
its outer `dng_sdk_1_7_1/` directory. The directory must contain `dng_sdk/`,
`xmp/`, `libjpeg/`, `libjxl/`, and the license/specification files. Expected ZIP
SHA-256: `c1d3a545ed45b150e59af2e6aa0695c1551f0e2bed34ad87436898a05194d2b1`.
The SDK tree is ignored and must never be committed. For example, from repo root:

```sh
python3 - <<'PY'
import hashlib, pathlib, zipfile
source = pathlib.Path('/Users/ibobby/Downloads/dng_sdk_1_7_1_2768_20261013.zip')
assert hashlib.sha256(source.read_bytes()).hexdigest() == 'c1d3a545ed45b150e59af2e6aa0695c1551f0e2bed34ad87436898a05194d2b1'
dest = pathlib.Path('native/vendor/dng-sdk-1.7.1')
assert not dest.exists(), 'Use the existing SDK; do not overwrite vendor sources'
with zipfile.ZipFile(source) as archive:
    for member in archive.infolist():
        if member.is_dir():
            continue
        parts = pathlib.PurePosixPath(member.filename).parts
        assert parts[0] == 'dng_sdk_1_7_1' and '..' not in parts
        target = dest.joinpath(*parts[1:])
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(archive.read(member))
PY
cmake -S native/dng-linear-frame -B native/build -DCMAKE_BUILD_TYPE=Release \
  -DCMAKE_PREFIX_PATH=/opt/homebrew
cmake --build native/build -j 8
ctest --test-dir native/build --output-on-failure
```

`DNG_SDK_ROOT` can override the SDK location. No build writes to the SDK tree. CMake prefers Unix dependency installations
over frameworks to avoid mixing Mono headers with Homebrew libraries. If
reusing an older build directory, clear its cached PNG/JPEG include selections
or configure a new build directory.
XMP core is required by the SDK's unguarded JXL code; file scanning and document
operations are disabled. VC5 support is disabled. No SDK sources are patched.

## Interface for the sequence consumer

```sh
native/build/dng-linear-frame input.dng output.png output.json \
  [--white-balance R,G,B] [--converter /absolute/executable] \
  [--converter-timeout SECONDS] [--exposure-ev EV] \
  [--emit-analysis PATH] [--analysis-roi X,Y,W,H] [--no-png]
```

The three positional paths must be distinct; `--emit-analysis PATH` must also
be distinct. Output parents must exist. The PNG is RGB, 16 bits/channel, in
active-area pixel order. There is no OETF, ICC, sRGB, or gamma chunk. Interpret
each integer as **code / 16384** in scene-linear ACEScg/AP1/D60. Quantization
rounds nearest, ties upward; negative values clip to zero and values above
65535/16384 clip to 65535. The preceding scene-linear FP32 values retain
negatives and values above one. No BaselineExposure gain, tone curve, exposure
matching, orientation, default-crop render, or preview render is applied. SDK
decode itself has the upstream limitations below.

`--exposure-ev EV` multiplies every FP32 scene-linear channel by `2^EV` after
decode/color and before quantization. `EV` must be finite and its gain must be
finite; the default is `0`. The JSON records `exposure_ev` and `gain`, and
`observed_scene_min`/`observed_scene_max` plus the quantization counts describe
the gained delivery data. `--no-png` suppresses PNG encoding while still
writing JSON (and the analysis sidecar when requested); the PNG positional path
is still validated. `--analysis-roi X,Y,W,H` selects a unit-ROI sub-rectangle
for analysis (default `0,0,1,1`).

`--emit-analysis PATH` writes the deterministic analysis sidecar consumed by
the deflicker node (schema version 1). The analysis downsamples the long edge to
at most 1024 by exact integer-weighted BOX area averaging, then measures a
12x8 grid clipped to the ROI (at least 8 tiles of 16x16 analysis pixels, else
exit 4). Luminance is `0.2126 R + 0.7152 G + 0.0722 B` on the **gained** ACEScg
samples. Each tile records `center` (unit ROI coordinates, row-major
`tile_layout`), `log_luminance` (5%/5% trimmed linear mean, then log2; `null`
when invalid), `valid` (>=256 downsampled samples, all finite, camera-domain
saturation occupancy <= 0.005, mean > floor), `saturation_fraction`,
`output_clip_fraction`, `structure_samples` (log2 domain, every fourth
downsampled sample) and `channel_log_means` (log2 of each channel's trimmed
mean, floored at 1e-9). `floor` is `max(1e-5, 8/16384)`; `quantum_linear` is
`1/16384`.

The two occupancy series are deliberately different and are not
interchangeable:

- `saturation_fraction` is evaluated in the normalized **camera domain**
  (post-Linearize, pre white-balance/color; `1.0 = WhiteLevel`) with the
  threshold `any channel >= 0.98`, then downsampled. It is invariant to the
  applied EV. The quality label is `camera_domain`.
- `output_clip_fraction` is evaluated in the **delivered domain**: the exact
  quantization predicate `code >= 16057` (`code/16384 >= 0.98`) on any gained
  channel, then downsampled. It changes with `--exposure-ev`.

The same input and options produce byte-identical sidecar JSON (fixed
10-significant-digit formatting, no run provenance). The sidecar contains
`source_sha256`, analysis geometry, `exposure_ev`, the fixed analysis constants,
and the row-major tile list.

Pipeline consumption: the `rawtl-linear-video` sequence driver invokes this tool
with `--exposure-ev`, `--emit-analysis` and `--no-png` during its deflicker
analysis pass and gain probes, and with `--exposure-ev` plus `--emit-analysis`
during each application pass. See `pipeline/README.md` for the orchestration,
report and audit flow. The two occupancy series feed distinct decisions:
camera-domain occupancy gates tile validity/estimation, while delivered-domain
occupancy drives the report's output-clipping delta.

Routing reads classic TIFF directory metadata **before SDK entry**, requires
DNGVersion and exactly one full-resolution (`NewSubfileType=0`) main RAW IFD,
and ignores reduced-resolution previews. BigTIFF and ambiguous full-resolution
IFDs are explicitly rejected. Only three-channel LinearRaw decode is supported.

- Main RAW `34892`: direct Adobe SDK decode.
- Main RAW `32803`: Adobe Converter `-c -l -d <run-temp-directory> <input>`;
  require exactly one output and independently verify main RAW `34892`, then SDK.
- Other/missing values: reject. There is no fallback provider.

Default Converter: `/Applications/Adobe DNG Converter.app/Contents/MacOS/Adobe DNG Converter`.
The timeout defaults to 120 seconds; allowed override 1–3600 seconds. Execution
uses an argv vector and a process group, with no shell. Failures and timeout are
explicit errors; the run-owned temporary directory is removed on success and
exception, with cleanup logged to stderr. Successful sidecars include the
Converter command, version (bundle metadata, otherwise `UNKNOWN`), exit code,
transcript, output hash/name, verified routing value, and cleanup location.

WB values are **camera neutral values in AsShotNeutral's domain**, rather than
reciprocal RGB gains. Defaults preserve the original file tag's rational
precision (the SDK's six-decimal stored neutral is not substituted). Values
must be finite and positive. The color calculation normalizes their maximum
to one. S02 must read the first numerically ordered frame's
`applied_white_balance` array, then pass those same three values as
`--white-balance` to subsequent frames. `as_shot_neutral` always describes the
original source; `decoded_as_shot_neutral` describes the SDK input after any
Converter step; `white_balance_source` is `as-shot` or `override`.

JSON schema version 1 also provides `source_sha256`, `route`, dimensions,
`color` (actual SDK-selected profile, illuminants, matrices, white, applied
transform), `decode_semantics`, `observed_scene_min`/`observed_scene_max` after
the requested gain and before quantization, `exposure_ev`, `gain`,
`capture_time` (EXIF `DateTimeOriginal` from the source; legal directories of
any fitting size are scanned, and `null` means absent or structurally
unreadable metadata — optional metadata never fails the frame),
`analysis_sidecar` (requested path or `null`), and the explicit `quantization`
policy/counts. It is a numeric sidecar; no claim that 0.18 equals 18% physical
reflectance is made. PNG bytes are deterministic on the tested toolchain; JSON
contains run-specific temporary paths. The caller must check exit status before
consuming either output. Output publication is per-file; an I/O failure between
renames can leave a PNG without its new sidecar and returns failure.

| Exit | Meaning |
| --- | --- |
| 0 | Requested outputs written successfully |
| 2 | Invalid CLI/WB/options or overlapping paths |
| 3 | Invalid analysis option: non-finite `--exposure-ev`/gain, malformed or out-of-range `--analysis-roi` |
| 4 | `ROI_TOO_SMALL`: fewer than 8 tiles of at least 16x16 analysis pixels |
| 20 | Invalid/unsupported TIFF/DNG directory structure or input read |
| 21 | Missing main RAW PhotometricInterpretation |
| 22 | Unsupported main RAW PhotometricInterpretation |
| 23 | Converter output absent/ambiguous/not LinearRaw; CFA SDK guard |
| 24 | Converter spawn/process failure |
| 25 | Converter timeout (process group killed/reaped) |
| 30 | SDK parse/decode/unsupported channel/residual-black failure |
| 40 | Missing/invalid color/WB metadata, singular matrix, nonfinite sample |
| 50 | Other filesystem/output/runtime failure |

## Decode and color attribution

SDK pipeline: ReadStage1Image → BuildStage2Image with its own `Linearize` into
an FP32 destination → BuildStage3Image. CFA never enters this path. Converter
performs demosaic upstream. There is no `dng_render` call.

Controlled fixtures verify linearization table application before black
subtraction, nonzero per-channel 2×2 BlackLevel subtraction (with SDK's
`WhiteLevel-MaxBlack` scale), WhiteLevel scaling, MapPolynomial dispatch at
OpcodeList1/2/3, and unchanged samples/PNG for BaselineExposure 0 versus 1.9.
Absent opcode lists are recorded as absent. Nonempty lists are recorded as SDK
dispatches with IDs/flags; optional unsupported opcodes may be skipped by SDK.
Arbitrary opcode implementations are not all independently verified.

**Upstream range limitation:** SDK integer linearization clamps below-black
and above-white values to [0,1], even with an FP32 destination. This is measured,
not silently relabeled as uncropped RAW range. Converter clipping/other baking
is `UNKNOWN`; float-source range behavior is not tested. Each frame records
camera min/max and residual Stage3 black (a nonzero residual is rejected).

Color uses the DNG Chapter 6 no-ForwardMatrix construction, even if a file also
contains ForwardMatrix. SDK `dng_color_spec` selects/interpolates ColorMatrix
and calibration per illuminants/signatures and computes selected white from
the applied neutral. We invert its normalized
`AB*CC*CM*MapWhiteMatrix(D50, selectedWhite)` (`PCStoCamera`), whose inverse
already includes selected-white→D50 adaptation. No second CA is added.
Then an explicit linear Bradford CAT D50→D60 and XYZ→AP1 transform are applied.
SDK profile-ingestion normalization/rounding and white saturation normalization
are declared, and the actual composed matrices are recorded.

## Verification

Default CTest generates small DNGs in the build tree, runs the real SDK, checks
routing (including big-endian/preview traps, missing tags, failed/invalid/timeout
Converter doubles), independent Decimal Chapter 6 reference values including
the DJI non-symmetric matrix and non-neutral vectors, override WB, FP32 range,
quantization known values and actual PNG sample encoding. Converter doubles
only test routing; they are not decode fallbacks.

`analysis-measurement` covers the deflicker photometry on synthetic buffers:
trimmed means (including unclamped negative noise), grid partitions/centers,
identity and fractional BOX downsampling, saturation and floor gates, structure
stride sampling, channel-mean flooring, the exact delivered-clip predicate, EV
equivariance, camera-mask invariance and `+EV` clip growth. The integration
script additionally checks the analysis sidecar contract on generated DNGs
(golden values from the independent Decimal matrix, `--no-png`, capture-time
presence/absence, deterministic sidecar bytes, non-finite EV/ROI rejections,
nonzero-black camera-domain masking and delivered-domain clipping).

Opt-in real test (large outputs stay in the work directory):

```sh
python3 native/dng-linear-frame/tests/real_material.py native/build/dng-linear-frame \
  /Users/ibobby/Temp/tl .agent-work/tmp/dng-linear-frame/real
```

This runs first/middle/last frames twice and compares PNG SHA-256, then checks
last-frame frozen WB while retaining its own differing AsShotNeutral. Per-run
stdout/stderr and real exit codes are saved under the supplied work directory's
`logs/`. All-39 Converter routing evidence and exact environment/known gaps are
in `docs/migration-log.md`.
