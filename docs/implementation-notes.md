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
