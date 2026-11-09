#!/usr/bin/env python3
"""Regenerate the frozen Python V4 golden fixtures from real RAW material.

This script is *fixture scaffolding*, not part of the legacy algorithm. It only
calls the frozen legacy implementation (src/rawtl) and serialises its output.
Running it twice on the same machine, pinned environment and unchanged material
must produce byte-identical fixture files.

Two tiers are produced:

1. Algorithm replay records (``measurement-input-basis.json``,
   ``frame-measurements.json``, ``pixel-samples/*.f32le.bin``): every field of
   the legacy ``FrameMeasurement`` plus the exact input basis needed to
   re-derive the rendered linear frames deterministically (renderer settings,
   fixed white balance, quantum, mask specification, source digests). Rendered
   frames are NOT stored; they are re-derived by the frozen legacy renderer.

2. Legacy end-to-end baseline (``pair-constraints.json``,
   ``exposure-plan.json``, ``end-to-end-baseline.json``,
   ``temporal-audit.json``, ``reference-video.mp4``): the frozen V4 estimate and
   encoded-video audit. These are a behavioural baseline and quality reference
   produced in the rawpy / linear-sRGB / BT.709-luminance domain. They are NOT a
   numeric oracle for the future Adobe / ACEScg chain.

Usage (from legacy/python):

    uv run python tools/generate_golden_fixtures.py \
        --input-dir /Users/ibobby/Temp/tl \
        --output-dir tests/fixtures/real-material

The material directory is only read; it is never written.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import math
import platform
import shutil
import subprocess
import sys
from dataclasses import asdict
from fractions import Fraction
from pathlib import Path
from typing import Any

import numpy as np

from rawtl.dng_semantics import (
    FrameSourceInfo,
    continuity,
    exiftool_version,
    extract_source_metadata,
)
from rawtl.exposure import estimate_exposure, measure_frame
from rawtl.exposure.pairwise import build_edges
from rawtl.inventory import inspect_directory
from rawtl.raw_backend import RawpyRenderer
from rawtl.service import ProcessingService
from rawtl.types import ProcessingRequest, RawRenderOptions

GENERATOR_NAME = "legacy-v4-golden-fixtures"
GENERATOR_VERSION = 1
SCHEMA_PREFIX = "rawtl-v4-golden"

# Frozen decisions for the baseline (mirror the legacy CLI defaults).
PERIOD = 60
STRENGTH = 1.0
BRIGHTNESS = "smooth"
ESTIMATOR = "pairwise"
TEMPORAL_QUALITY = "strict"
ENCODER = "libx264"
# libx264 with automatic frame threading is not bit-reproducible on this
# material (identical rgb24 input produced different MP4 bytes while every
# measurement and exposure field stayed identical). The fixture generator
# therefore pins the encoder thread count at run time; legacy source is not
# modified. A single thread is fully deterministic and is documented in the
# manifest.
ENCODER_THREADS = 1
ON_REVIEW = "passthrough"
MAX_EDGE = 1024
GRID = (12, 8)

PRODUCTION_DOMAIN = {
    "decoder": "rawpy / LibRaw (libraw_builtin_srgb postprocess, gamma=(1,1))",
    "working_primaries": "linear-srgb",
    "luminance": "BT.709 coefficients (0.2126, 0.7152, 0.0722)",
    "encoded_video": "sRGB OETF pixels, BT.709 primaries/matrix, limited range",
    "scope": (
        "Behavioural baseline and quality reference only. Do not use as a "
        "numeric oracle for the Adobe DNG Converter / DNG SDK / ACEScg chain: "
        "the two chains do not promise pixel equality."
    ),
}

NAN_POLICY = (
    "JSON null represents a non-finite float (NaN or +-Inf). The legacy "
    "reporting layer uses the same convention. Tile validity is carried "
    "explicitly by the 'valid' mask; pixel-sample binary files contain raw "
    "IEEE-754 values, including NaN padding."
)


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def jsonable(value: Any) -> Any:
    """Convert legacy/numpy values to strict-JSON types (non-finite -> None)."""
    if isinstance(value, np.ndarray):
        return jsonable(value.tolist())
    if isinstance(value, np.generic):
        return jsonable(value.item())
    if isinstance(value, float):
        return value if math.isfinite(value) else None
    if isinstance(value, dict):
        return {str(key): jsonable(item) for key, item in value.items()}
    if isinstance(value, (list, tuple)):
        return [jsonable(item) for item in value]
    return value


def write_json(path: Path, payload: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    text = json.dumps(
        jsonable(payload), ensure_ascii=False, allow_nan=False, indent=2
    ) + "\n"
    path.write_text(text, encoding="utf8")


def ffmpeg_version(ffmpeg: str) -> str | None:
    try:
        proc = subprocess.run(
            [ffmpeg, "-version"], capture_output=True, text=True, timeout=60, check=True
        )
    except (OSError, subprocess.SubprocessError):
        return None
    return proc.stdout.splitlines()[0] if proc.stdout else None


def source_identity(paths: list[Path]) -> list[dict]:
    return [
        {"filename": path.name, "bytes": path.stat().st_size, "sha256": sha256_file(path)}
        for path in paths
    ]


def legacy_source_digest(root: Path) -> str:
    hasher = hashlib.sha256()
    for path in sorted((root / "src").rglob("*.py")):
        rel = path.relative_to(root).as_posix()
        hasher.update(rel.encode("utf8"))
        hasher.update(b"\0")
        hasher.update(sha256_file(path).encode("ascii"))
        hasher.update(b"\n")
    return hasher.hexdigest()


def write_pixel_samples(
    out_dir: Path, filename: str, samples: np.ndarray
) -> dict:
    rel = Path("pixel-samples") / f"{Path(filename).stem}.log-pixel-samples.f32le.bin"
    path = out_dir / rel
    path.parent.mkdir(parents=True, exist_ok=True)
    array = np.ascontiguousarray(samples, dtype="<f4")
    path.write_bytes(array.tobytes(order="C"))
    return {
        "file": rel.as_posix(),
        "dtype": "<f4",
        "shape": list(array.shape),
        "sha256": sha256_file(path),
    }


def measure(renderer: RawpyRenderer, input_dir: Path) -> dict:
    inventory = inspect_directory(input_dir, renderer)
    refs = inventory.frames
    meta = inventory.first
    wb = RawpyRenderer.fixed_wb(meta)
    options = RawRenderOptions(wb)

    frames: list[dict] = []
    measurements = []
    quanta: list[float] = []
    for ref in refs:
        frame = renderer.render_linear(ref.path, 0.0, options)
        quantum = float(frame.quantum_linear)
        quanta.append(quantum)
        measurement = measure_frame(
            frame, (0.0, 0.0, 1.0, 1.0), max_edge=MAX_EDGE, grid=GRID,
            structure_samples=True,
        )
        measurements.append(measurement)
        frames.append(
            {
                "index": ref.index,
                "filename": ref.filename,
                "width": int(measurement.width),
                "height": int(measurement.height),
                "saturation_mask_quality": measurement.saturation_mask_quality,
                "quantum_linear": quantum,
                "log_luminance": measurement.log_luminance,
                "valid": measurement.valid,
                "centers": measurement.centers,
                "saturation_fraction": measurement.saturation_fraction,
                "log_channel_means": measurement.log_channel_means,
                "log_pixel_samples": measurement.log_pixel_samples,
            }
        )
        del frame

    basis = {
        "schema": f"{SCHEMA_PREFIX}/measurement-input-basis/1",
        "production_domain": PRODUCTION_DOMAIN,
        "material": {
            "source_dir_name": input_dir.name,
            "frame_count": len(refs),
            "frames": source_identity([ref.path for ref in refs]),
            "note": (
                "RAW files are NOT stored in the repository. Re-derive the "
                "rendered frames from these identical bytes with the frozen "
                "renderer below."
            ),
        },
        "renderer": {
            "implementation": "src/rawtl/raw_backend.py:RawpyRenderer.render_linear",
            "color_transform_id": options.color_transform_id,
            "transfer_id": options.transfer_id,
            "exposure_ev": 0.0,
            "fixed_white_balance": list(wb),
            "fixed_white_balance_source": (
                "camera_whitebalance of the first frame"
                if meta.camera_wb is not None else "daylight_whitebalance of the first frame"
            ),
            "raw_metadata": {
                "camera": meta.camera,
                "raw_geometry": [meta.width, meta.height],
                "orientation": meta.orientation,
                "raw_type": meta.raw_type,
                "sensor_signature": meta.sensor_signature,
            },
            "postprocess": {
                "output_color": "sRGB",
                "output_bps": 16,
                "gamma": [1, 1],
                "no_auto_bright": True,
                "adjust_maximum_thr": 0.0,
                "use_camera_wb": False,
                "use_auto_wb": False,
                "user_wb": list(wb),
                "bright": 1.0,
                "highlight_mode": "Clip",
                "exp_shift": 1.0,
                "exp_preserve_highlights": 0.0,
            },
            "versions": {
                "rawpy": str(getattr(renderer.rawpy, "__version__", "unknown")),
                "libraw": str(getattr(renderer.rawpy, "libraw_version", "unknown")),
            },
        },
        "measurement": {
            "function": "src/rawtl/exposure/measurement.py:measure_frame",
            "roi": [0.0, 0.0, 1.0, 1.0],
            "max_edge": MAX_EDGE,
            "grid": list(GRID),
            "structure_samples": True,
            "quantum_linear": quanta[0] if quanta else None,
            "quantum_linear_consistent": len(set(quanta)) <= 1,
            "notes": [
                "Analysis image is area-downsampled so max(width,height)<=max_edge.",
                "Tile grid is nx=min(grid_x,w//16), ny=min(grid_y,h//16).",
                "log_pixel_samples are a deterministic b[::4,::4] strided log2 "
                "grid per tile, padded to a common width with NaN.",
                "saturation_fraction is the mean of the renderer saturation mask "
                "per tile.",
            ],
        },
        "saturation_mask": {
            "source": "rawpy renderer surrogate",
            "quality": measurements[0].saturation_mask_quality if measurements else None,
            "spec": (
                "any channel >= 64224/65535 (~0.98) of the 16-bit sRGB-linear "
                "postprocess output; this is a clipped-linear proxy, not a "
                "sensor saturation mask"
            ),
        },
        "nan_policy": NAN_POLICY,
    }
    return {"basis": basis, "frames": frames, "measurements": measurements, "refs": refs,
            "meta": meta, "wb": wb, "options": options}


def estimate(measurements, refs) -> tuple[object, dict, dict, list]:
    source_info = extract_source_metadata([ref.path for ref in refs])
    if source_info is None:
        continuity_info = None
        time_gaps: list[int] = []
    else:
        # Preserve the legacy per-frame ordering by filename.
        ordered = [
            source_info.get(ref.filename, FrameSourceInfo(ref.filename)) for ref in refs
        ]
        continuity_info = continuity(ordered)
        time_gaps = [entry["index"] for entry in continuity_info["time_gaps"]]

    exposure = estimate_exposure(
        measurements,
        period=PERIOD,
        strength=STRENGTH,
        steps=(),
        brightness=BRIGHTNESS,
        time_gaps=time_gaps,
        estimator=ESTIMATOR,
        exclude_rois=(),
        ev_limits=None,
    )
    centers = measurements[0].centers
    grid = (len(np.unique(centers[:, 0])), len(np.unique(centers[:, 1])))
    edges, edge_stats = build_edges(
        measurements, boundaries=(), train=np.ones(len(centers), dtype=bool), grid=grid
    )
    return exposure, continuity_info, edge_stats, edges


def pin_encoder_threads(threads: int) -> None:
    """Make the legacy FFmpeg command reproducible without editing legacy code.

    ``ffmpeg_command`` is read from ``rawtl.video`` at call time, so patching the
    module attribute affects the unchanged ``VideoWriter`` and ``probe_encoders``.
    """
    if threads <= 0:
        return
    import rawtl.video as video_module

    original = video_module.ffmpeg_command

    def deterministic_command(ffmpeg, candidate, width, height, fps, output):
        command = original(ffmpeg, candidate, width, height, fps, output)
        index = command.index("-c:v")
        return command[:index] + ["-threads", str(threads)] + command[index:]

    video_module.ffmpeg_command = deterministic_command


def run_service(input_dir: Path, work_dir: Path, ffmpeg: str):
    out = work_dir / "e2e"
    if out.exists():
        shutil.rmtree(out)
    request = ProcessingRequest(
        input_dir=input_dir,
        output_dir=out,
        fps=Fraction(30, 1),
        period=PERIOD,
        strength=STRENGTH,
        brightness=BRIGHTNESS,
        estimator=ESTIMATOR,
        ev_limits=None,
        roi=(0.0, 0.0, 1.0, 1.0),
        exclude_rois=(),
        steps=(),
        on_review=ON_REVIEW,
        temporal_quality=TEMPORAL_QUALITY,
        save_frames=False,
        overwrite=True,
        encoder=ENCODER,
        ffmpeg=ffmpeg,
    )
    result = ProcessingService().run(request)
    return out, result


def build_e2e_record(report: dict) -> dict:
    keys = [
        "schema_version", "run_status", "execution_status", "frame_count",
        "analysis_shape", "input_sort", "video_target_bitrate_bps",
        "period_effective", "scene_cuts", "estimation_time_gaps", "ev_limits",
        "unconverged_trend_tiles", "feedback_updates", "feedback_history",
        "decoder", "render_temporal_quality", "encoded_temporal_quality",
        "render_semantics", "source_continuity", "source_discontinuities",
        "gain_probe", "gain_response", "pairwise_estimation", "video_metadata",
        "padded_dimensions", "video_expected_budget_bytes", "warnings",
        "status_counts", "frames",
    ]
    record = {key: report[key] for key in keys if key in report}
    record["run_config"] = {
        "fps": "30",
        "period": PERIOD,
        "strength": STRENGTH,
        "brightness": BRIGHTNESS,
        "estimator": ESTIMATOR,
        "ev_limits": None,
        "roi": [0.0, 0.0, 1.0, 1.0],
        "exclude_rois": [],
        "on_review": ON_REVIEW,
        "temporal_quality": TEMPORAL_QUALITY,
        "save_frames": False,
        "encoder": ENCODER,
    }
    return record


def generate(input_dir: Path, output_dir: Path, work_dir: Path, ffmpeg: str,
             encoder_threads: int = ENCODER_THREADS) -> None:
    repo_root = Path(__file__).resolve().parents[1]
    renderer = RawpyRenderer()

    print("[1/5] rendering and measuring frames", flush=True)
    measured = measure(renderer, input_dir)
    refs = measured["refs"]
    measurements = measured["measurements"]

    print("[2/5] estimating exposure and pair constraints", flush=True)
    (
        exposure,
        continuity_info,
        edge_stats,
        edges,
    ) = estimate(measurements, refs)

    output_dir.mkdir(parents=True, exist_ok=True)
    write_json(output_dir / "measurement-input-basis.json", measured["basis"])

    frame_records = []
    for record in measured["frames"]:
        payload = {key: record[key] for key in (
            "index", "filename", "width", "height", "saturation_mask_quality",
            "quantum_linear", "log_luminance", "valid", "centers",
            "saturation_fraction", "log_channel_means",
        )}
        payload["log_pixel_samples"] = write_pixel_samples(
            output_dir, record["filename"], record["log_pixel_samples"]
        )
        frame_records.append(payload)
    write_json(
        output_dir / "frame-measurements.json",
        {
            "schema": f"{SCHEMA_PREFIX}/frame-measurements/1",
            "production_domain": PRODUCTION_DOMAIN,
            "nan_policy": NAN_POLICY,
            "grid": {"nx": int(GRID[0]), "ny": int(GRID[1]), "tiles": len(measurements[0].log_luminance)},
            "frames": frame_records,
        },
    )

    write_json(
        output_dir / "pair-constraints.json",
        {
            "schema": f"{SCHEMA_PREFIX}/pair-constraints/1",
            "production_domain": PRODUCTION_DOMAIN,
            "input": {
                "estimator": ESTIMATOR,
                "boundaries": [],
                "train": "all tiles",
                "grid": list(GRID),
                "lags": sorted(int(lag) for lag in {edge.lag for edge in edges}),
                "candidate_lags": [1, 2, 4, 8, 16],
                "source": "src/rawtl/exposure/pairwise.py:build_edges",
                "note": (
                    "Only graph-eligible edges are returned by legacy build_edges; "
                    "rejected candidates are summarized in stats."
                ),
            },
            "stats": edge_stats,
            "edge_count": len(edges),
            "edges": [dict(asdict(edge), reliable=bool(edge.reliable)) for edge in edges],
        },
    )

    result = exposure
    write_json(
        output_dir / "exposure-plan.json",
        {
            "schema": f"{SCHEMA_PREFIX}/exposure-plan/1",
            "production_domain": PRODUCTION_DOMAIN,
            "input": {
                "period": PERIOD,
                "strength": STRENGTH,
                "brightness": BRIGHTNESS,
                "estimator": ESTIMATOR,
                "steps": [],
                "exclude_rois": [],
                "ev_limits": None,
                "time_gaps": list(result.time_gaps),
                "continuity": continuity_info,
            },
            "result": {
                "period_effective": int(result.period_effective),
                "trend_unconverged_tiles": int(result.trend_unconverged_tiles),
                "scene_cuts": result.scene_cuts,
                "time_gaps": list(result.time_gaps),
                "pairwise_summary": result.pairwise_summary,
                "tile_log_luminance": result.tile_log_luminance,
                "tile_valid": result.tile_valid,
                "consensus_mask": result.consensus_mask,
                "frozen_target": result.frozen_target,
                "trusted_indices": result.trusted_indices,
                "frames": [asdict(frame) for frame in result.frames],
            },
        },
    )

    print("[3/5] running the frozen end-to-end pipeline", flush=True)
    pin_encoder_threads(encoder_threads)
    e2e_dir, service_result = run_service(input_dir, work_dir, ffmpeg)
    video_src = e2e_dir / "video.mp4"
    audit_src = e2e_dir / "temporal_audit.json"
    if not video_src.is_file() or not audit_src.is_file():
        raise RuntimeError("legacy pipeline did not produce video.mp4 / temporal_audit.json")

    reference_video = output_dir / "reference-video.mp4"
    shutil.copyfile(video_src, reference_video)
    audit = json.loads(audit_src.read_text(encoding="utf8"))
    # Annotate the frozen legacy audit with its production domain; every other
    # key/value is the legacy audit_video output verbatim.
    audit["production_domain"] = PRODUCTION_DOMAIN
    audit["annotation_note"] = (
        "production_domain added by the T00 fixture generator; all other fields "
        "are the frozen legacy audit_video output verbatim"
    )
    write_json(output_dir / "temporal-audit.json", audit)

    report = json.loads((e2e_dir / "exposure_report.json").read_text(encoding="utf8"))
    write_json(
        output_dir / "end-to-end-baseline.json",
        {
            "schema": f"{SCHEMA_PREFIX}/end-to-end-baseline/1",
            "production_domain": PRODUCTION_DOMAIN,
            "service_result": {
                "run_status": service_result.run_status,
                "frame_count": service_result.frame_count,
                "encoder": service_result.encoder,
            },
            "report": build_e2e_record(report),
        },
    )

    print("[4/5] writing manifest", flush=True)
    generated: list[Path] = []
    for path in output_dir.rglob("*"):
        if not path.is_file():
            continue
        rel = path.relative_to(output_dir).as_posix()
        if rel in ("manifest.json", "README.md"):
            continue
        generated.append(path)
    manifest = {
        "schema": f"{SCHEMA_PREFIX}/manifest/1",
        "generator": {"name": GENERATOR_NAME, "version": GENERATOR_VERSION},
        "parameters": {
            "material_frames": len(refs),
            "max_edge": MAX_EDGE,
            "grid": list(GRID),
            "structure_samples": True,
            "estimator": ESTIMATOR,
            "period": PERIOD,
            "strength": STRENGTH,
            "brightness": BRIGHTNESS,
            "on_review": ON_REVIEW,
            "temporal_quality": TEMPORAL_QUALITY,
            "encoder": ENCODER,
            "encoder_threads": encoder_threads,
        },
        "legacy_source": {
            "commit": "f6d3ae7",
            "src_tree_sha256": legacy_source_digest(repo_root),
        },
        "environment": {
            "python": platform.python_version(),
            "platform": platform.platform(),
            "numpy": np.__version__,
            "rawpy": str(getattr(renderer.rawpy, "__version__", "unknown")),
            "libraw": str(getattr(renderer.rawpy, "libraw_version", "unknown")),
            "exiftool": exiftool_version(),
            "ffmpeg": ffmpeg_version(ffmpeg),
        },
        "determinism": {
            "policy": "byte-identical across runs on the same machine, pinned environment and material",
            "video_container": "MP4 written from rawvideo pipe; no creation_time metadata",
            "encoder_threading": (
                f"libx264 -threads {encoder_threads} pinned at generation time "
                "because automatic frame threading is not bit-reproducible on "
                "this material; legacy source is unchanged"
            ),
        },
        "files": [
            {
                "path": path.relative_to(output_dir).as_posix(),
                "bytes": path.stat().st_size,
                "sha256": sha256_file(path),
            }
            for path in sorted(generated)
        ],
    }
    write_json(output_dir / "manifest.json", manifest)
    print(f"[5/5] fixtures written to {output_dir}", flush=True)


def validate_directories(
    input_dir: Path, output_dir: Path, work_dir: Path | None = None,
) -> tuple[Path, Path, Path]:
    """Resolve and validate paths without creating or removing anything."""
    input_dir = input_dir.expanduser().resolve()
    output_dir = output_dir.expanduser().resolve()
    requested_work = (work_dir.expanduser() if work_dir is not None
                      else output_dir.parent / (output_dir.name + ".work"))
    work_dir = requested_work.resolve()

    if not input_dir.is_dir():
        raise ValueError(f"input directory not found: {input_dir}")
    if output_dir.is_relative_to(input_dir):
        raise ValueError("output directory must not be inside the read-only material")
    if work_dir.is_relative_to(input_dir) or input_dir.is_relative_to(work_dir):
        raise ValueError("work directory must not overlap the read-only material")
    if work_dir.is_relative_to(output_dir) or output_dir.is_relative_to(work_dir):
        raise ValueError("work directory must not overlap the output directory")
    if work_dir.exists() or requested_work.is_symlink():
        raise ValueError(f"work directory must not already exist: {work_dir}")
    return input_dir, output_dir, work_dir


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input-dir", type=Path, required=True,
                        help="read-only directory with the real RAW frames")
    parser.add_argument("--output-dir", type=Path, required=True,
                        help="fixture directory to write")
    parser.add_argument("--work-dir", type=Path, default=None,
                        help="new scratch directory; must not already exist or "
                             "overlap input/output in either direction "
                             "(default: <output-dir>.work; created and removed "
                             "by this invocation, including on failure)")
    parser.add_argument("--ffmpeg", default="ffmpeg")
    parser.add_argument("--encoder-threads", type=int, default=ENCODER_THREADS,
                        help="pin the FFmpeg encoder thread count for reproducible "
                             "MP4 bytes (0 keeps the legacy automatic value)")
    args = parser.parse_args(argv)

    try:
        input_dir, output_dir, work_dir = validate_directories(
            args.input_dir, args.output_dir, args.work_dir,
        )
    except ValueError as error:
        parser.error(str(error))
    try:
        work_dir.mkdir(parents=True, exist_ok=False)
    except FileExistsError:
        parser.error(f"work directory must not already exist: {work_dir}")
    # Cleanup is entered only after exclusive creation succeeds: this root is ours.
    try:
        generate(input_dir, output_dir, work_dir, args.ffmpeg, args.encoder_threads)
    finally:
        shutil.rmtree(work_dir, ignore_errors=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
