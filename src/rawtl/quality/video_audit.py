"""Independent decoded-video temporal audit (fixed-camera, read-only).

Decodes the actual output file through FFmpeg at full precision (RGB48),
applies the declared inverse transfer, reduces in LINEAR light, and measures
temporal brightness consistency on matched tile support. This runs completely
outside the render pipeline: it audits whatever file exists and never needs
the RAWs. A PASS here is evidence about the encoded output only; it is not a
statement that physical lighting events were intentional.
"""
from __future__ import annotations
from dataclasses import dataclass, asdict
import hashlib
import json
import math
import subprocess
import tempfile
from pathlib import Path
import numpy as np
from PIL import Image

from ..types import VerificationError
from .photometry import (
    CANDIDATE_STEP_EV, SHORT_CONTEXT_FRAMES, TileStats,
    classify_transfer_tag, inverse_transfer, linear_luminance, pair_metric,
    roi_level, roi_series_metrics, temporal_summary, tile_statistics)

DETECTOR_MODEL = "fixed-camera paired photometric common-mode v1"

# Absolute engineering gates for one continuous, observable segment. A FAIL is
# "residual flicker above the screening thresholds", independent of intent.
GATE_P95_STEP_EV = .025
GATE_CURVATURE_RMS_EV = .012
GATE_SEVERE_EVENT_EV = CANDIDATE_STEP_EV
UNOBSERVABLE_PAIR_FRACTION = .75


def _jsonable(value):
    if isinstance(value, np.generic):
        return _jsonable(value.item())
    if isinstance(value, float) and not math.isfinite(value):
        return None
    if isinstance(value, dict):
        return {str(k): _jsonable(v) for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return [_jsonable(v) for v in value]
    return value


def probe_stream(ffprobe: str, video: Path) -> dict:
    try:
        result = subprocess.run(
            [ffprobe, "-v", "error", "-show_streams", "-show_format", "-of", "json", str(video)],
            capture_output=True, text=True, check=True, timeout=120)
        streams = json.loads(result.stdout)["streams"]
    except (OSError, subprocess.SubprocessError, ValueError, KeyError) as exc:
        raise VerificationError(f"ffprobe stream inspection failed for {video}: {exc}") from exc
    video_streams = [s for s in streams if s.get("codec_type") == "video"]
    if len(video_streams) != 1:
        raise VerificationError(f"expected exactly one video stream in {video}")
    return video_streams[0]


def resolve_transfer(stream: dict, requested: str | None) -> tuple[str, list[str]]:
    """Decide the decode transfer: explicit request wins; tags must classify."""
    warnings: list[str] = []
    tag = stream.get("color_transfer")
    if requested is not None:
        tagged = None
        try:
            tagged = classify_transfer_tag(tag)
        except ValueError:
            pass
        if tagged is not None and tagged != requested:
            warnings.append(f"TRANSFER_TAG_MISMATCH: stream tagged {tag} but auditing as {requested}")
        return requested, warnings
    try:
        return classify_transfer_tag(tag), warnings
    except ValueError as exc:
        raise VerificationError(
            f"{exc}; rerun with an explicit --transfer choice") from exc


def analysis_geometry(width0: int, height0: int, max_width: int = 912,
                      nx: int = 24, ny: int = 18) -> tuple[int, int]:
    w = max(nx, round(min(max_width, width0) / nx) * nx)
    h = max(ny, round(height0 / width0 * w / ny) * ny)
    if min(w // nx, h // ny) < 8:
        raise VerificationError("analysis resolution too small for the tile grid")
    return w, h


def _area_reduce(array: np.ndarray, width: int, height: int) -> np.ndarray:
    if (array.shape[1], array.shape[0]) == (width, height):
        return array
    image = Image.fromarray(np.ascontiguousarray(array, dtype=np.float32))
    return np.asarray(image.resize((width, height), resample=Image.Resampling.BOX),
                      dtype=np.float32)


def read_frame(pipe, count: int) -> bytes | None:
    chunks = []
    received = 0
    while received < count:
        block = pipe.read(count - received)
        if not block:
            if received:
                raise VerificationError("truncated decoded frame from FFmpeg")
            return None
        chunks.append(block)
        received += len(block)
    return b"".join(chunks)


@dataclass
class VideoAuditConfig:
    transfer: str | None = None
    rois: dict[str, tuple[float, float, float, float]] | None = None
    boundaries: tuple[int, ...] = ()          # zero-based frame indices starting a new source segment
    frame_labels: list[str] | None = None     # source filenames per decoded frame, optional
    max_width: int = 912
    nx: int = 24
    ny: int = 18


def decode_video_features(video: Path, ffmpeg: str, width0: int, height0: int,
                          config: VideoAuditConfig) -> tuple[list[TileStats], list[np.ndarray], list[dict]]:
    """Decode every frame once; return per-frame tile stats, linear images and records."""
    w, h = analysis_geometry(width0, height0, config.max_width, config.nx, config.ny)
    lut = inverse_transfer(np.arange(65536, dtype=np.float32) / np.float32(65535.), config.transfer)
    cmd = [ffmpeg, "-v", "error", "-threads", "2", "-filter_threads", "1",
           "-i", str(video), "-map", "0:v:0", "-fps_mode", "passthrough",
           "-pix_fmt", "rgb48le", "-f", "rawvideo", "-"]
    stats: list[TileStats] = []
    levels: list[np.ndarray] = []
    records: list[dict] = []
    with tempfile.TemporaryFile() as err:
        process = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=err)
        try:
            index = 0
            while True:
                data = read_frame(process.stdout, width0 * height0 * 6)
                if data is None:
                    break
                rgb = np.frombuffer(data, dtype="<u2").reshape(height0, width0, 3)
                y = linear_luminance(rgb, config.transfer, lut)
                sat = _area_reduce(
                    (rgb.max(axis=2) >= int(.965 * 65535)).astype(np.float32), w, h)
                y = _area_reduce(y, w, h)
                tile = tile_statistics(y, sat, config.nx, config.ny)
                stats.append(tile)
                levels.append(y)
                records.append({"index": index})
                index += 1
            code = process.wait(timeout=60)
            if code:
                err.seek(0)
                raise VerificationError(
                    "FFmpeg decode failed: " + err.read().decode(errors="replace")[-2000:])
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
    return stats, levels, records


def audit_video(video: Path, *, ffmpeg: str, ffprobe: str,
                config: VideoAuditConfig | None = None,
                output_dir: Path | None = None) -> dict:
    """Full independent temporal audit of one encoded video; read-only."""
    config = config or VideoAuditConfig()
    video = Path(video)
    stream = probe_stream(ffprobe, video)
    width0, height0 = int(stream["width"]), int(stream["height"])
    transfer, warnings = resolve_transfer(stream, config.transfer)
    config.transfer = transfer
    if stream.get("color_transfer") in (None, "unknown", "unspecified"):
        warnings.append("UNKNOWN_TRANSFER_TAG; explicit analysis assumption used")
    if stream.get("color_primaries") in (None, "unknown", "unspecified"):
        warnings.append("UNKNOWN_PRIMARIES_TAG; BT.709/sRGB primaries assumed for luminance")
    for name, box in (config.rois or {}).items():
        if len(box) != 4 or not (0 <= box[0] < box[2] <= 1 and 0 <= box[1] < box[3] <= 1):
            raise VerificationError(f"invalid ROI {name}: expected normalized [x0,y0,x1,y1]")
    pts_info = subprocess.run(
        [ffprobe, "-v", "error", "-select_streams", "v:0", "-show_frames",
         "-show_entries", "frame=best_effort_timestamp_time", "-of", "json", str(video)],
        capture_output=True, text=True, check=True, timeout=600)
    pts = [float(f["best_effort_timestamp_time"])
           for f in json.loads(pts_info.stdout)["frames"]
           if "best_effort_timestamp_time" in f]
    stats, levels, records = decode_video_features(video, ffmpeg, width0, height0, config)
    if pts and len(pts) != len(records):
        raise VerificationError(
            f"decoded frame count {len(records)} != PTS count {len(pts)}")
    if config.frame_labels and len(config.frame_labels) != len(records):
        raise VerificationError(
            f"decoded frame count {len(records)} != label count {len(config.frame_labels)}")
    roi_names = list(config.rois or {})
    roi_series: dict[str, list[float | None]] = {name: [] for name in roi_names}
    for record, y in zip(records, levels):
        if pts:
            record["pts_s"] = pts[record["index"]]
        if config.frame_labels:
            record["filename"] = config.frame_labels[record["index"]]
        for name in roi_names:
            value = roi_level(y, config.rois[name])
            roi_series[name].append(value)
            record[name + "_ev"] = value
    boundaries = {0: "start"}
    for index in config.boundaries:
        if index != 0:
            boundaries[index] = "source_discontinuity"
    pairs: list[dict] = []
    previous: TileStats | None = None
    for record, tile in zip(records, stats):
        if previous is not None:
            metric, _, _ = pair_metric(tile, previous, config.nx, config.ny)
            entry = {"index": record["index"], "pts_s": record.get("pts_s"),
                     "filename": record.get("filename"),
                     "boundary": boundaries.get(record["index"]),
                     **asdict(metric)}
            if entry["boundary"]:
                entry["quality"] = "SOURCE_DISCONTINUITY"
            entry["candidate_global_jump"] = bool(
                not entry["boundary"] and entry["quality"] == "RELIABLE_COMMON_CHANGE"
                and entry["common_ev"] is not None
                and abs(entry["common_ev"]) > CANDIDATE_STEP_EV)
            entry["chroma_variation"] = "NOT_IMPLEMENTED"
            pairs.append(entry)
        previous = tile
    starts = sorted({0, len(records), *(i for i in boundaries if i < len(records))})
    segments = []
    for a, b in zip(starts[:-1], starts[1:]):
        within = [p for p in pairs if a <= p["index"] < b]
        reliable = [p["common_ev"] if p["quality"] == "RELIABLE_COMMON_CHANGE" else None
                    for p in within]
        summary = temporal_summary(reliable)
        summary.update(start=a, end_exclusive=b, frames=b - a)
        summary["roi_metrics"] = {name: roi_series_metrics(series[a:b])
                                  for name, series in roi_series.items()}
        usable_fraction = (summary["usable_pairs"] / summary["pairs"]
                           if summary["pairs"] else 0.)
        if summary["frames"] < SHORT_CONTEXT_FRAMES:
            summary["status"] = "SHORT_CONTEXT"
        elif summary["usable_pairs"] < 8 or usable_fraction < UNOBSERVABLE_PAIR_FRACTION:
            summary["status"] = "UNOBSERVABLE"
        else:
            failed = (summary["max_abs_step_ev"] is not None and
                      (summary["max_abs_step_ev"] > GATE_SEVERE_EVENT_EV or
                       summary["p95_abs_step_ev"] > GATE_P95_STEP_EV or
                       summary["curvature_rms_ev"] > GATE_CURVATURE_RMS_EV))
            summary["status"] = "FAIL" if failed else "PASS"
        segments.append(summary)
    gated = [s for s in segments if s["status"] not in ("SHORT_CONTEXT", "UNOBSERVABLE")]
    if gated:
        overall = "FAIL" if any(s["status"] == "FAIL" for s in gated) else "PASS"
    elif segments:
        overall = "UNOBSERVABLE" if all(
            s["status"] == "UNOBSERVABLE" for s in segments) else "SHORT_CONTEXT"
    else:
        overall = "UNOBSERVABLE"
    result = {
        "detector_model": DETECTOR_MODEL,
        "video_sha256": hashlib.file_digest(video.open("rb"), "sha256").hexdigest(),
        "frame_count": len(records),
        "decoded_shape": [width0, height0],
        "transfer_assumed": config.transfer,
        "decode_policy": "RGB48 -> inverse transfer -> linear area reduce; no temporal resampling",
        "tags": {k: stream.get(k) for k in
                 ("color_space", "color_range", "color_transfer", "color_primaries")},
        "metadata_warnings": warnings,
        "chroma_variation": "NOT_IMPLEMENTED",
        "status": overall,
        "boundaries": {str(k): v for k, v in boundaries.items()},
        "segments": segments,
        "events": [{"index": p["index"], "filename": p.get("filename"),
                    "pts_s": p.get("pts_s"), "common_ev": p["common_ev"],
                    "local_p90_ev": p["local_p90_ev"],
                    "consensus_fraction": p["consensus_fraction"]}
                   for p in pairs if p["candidate_global_jump"]],
        "candidate_count": sum(p["candidate_global_jump"] for p in pairs),
        "constraints": {
            "pair_shape_mad_ev": .04, "pair_consensus_ev": .06,
            "minimum_inlier_fraction": .6, "minimum_quadrants": 3,
            "candidate_step_ev": CANDIDATE_STEP_EV,
            "gate_p95_step_ev": GATE_P95_STEP_EV,
            "gate_curvature_rms_ev": GATE_CURVATURE_RMS_EV,
            "gate_severe_event_ev": GATE_SEVERE_EVENT_EV},
    }
    if output_dir is not None:
        import csv
        output_dir.mkdir(parents=True, exist_ok=True)
        (output_dir / "temporal_audit.json").write_text(
            json.dumps(_jsonable(result), indent=2, ensure_ascii=False, allow_nan=False) + "\n",
            encoding="utf8")
        for name, rows in (("pairs.csv", pairs), ("frames.csv", records)):
            if rows:
                with (output_dir / name).open("w", newline="", encoding="utf8") as file:
                    writer = csv.DictWriter(file, fieldnames=list(rows[0]))
                    writer.writeheader()
                    writer.writerows(_jsonable(rows))
    return result
