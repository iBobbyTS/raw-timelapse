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
