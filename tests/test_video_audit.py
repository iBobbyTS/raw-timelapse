"""F1: independent decoded-video temporal audit, photometry and status semantics."""
from __future__ import annotations
import io
import json
import math
from fractions import Fraction
from pathlib import Path
import shutil
import numpy as np
import pytest

from rawtl.color import srgb_oetf
from rawtl.quality.photometry import (
    classify_transfer_tag, inverse_transfer, linear_luminance, pair_metric,
    roi_level, roi_series_metrics, temporal_summary, tile_statistics,
    TileStats, SHORT_CONTEXT_FRAMES)
from rawtl.quality.video_audit import (
    analysis_geometry, audit_video, probe_stream, read_frame, resolve_transfer,
    VideoAuditConfig, GATE_P95_STEP_EV)
from rawtl.types import VerificationError

ffmpeg = shutil.which("ffmpeg")
ffprobe = shutil.which("ffprobe")
needs_ffmpeg = pytest.mark.skipif(not (ffmpeg and ffprobe),
                                  reason="local FFmpeg/ffprobe unavailable")


def tile_stats(image: np.ndarray) -> TileStats:
    return tile_statistics(image.astype(np.float32), np.zeros_like(image))


def compare(a: np.ndarray, b: np.ndarray):
    return pair_metric(tile_stats(b), tile_stats(a))[0]


def test_global_gain_is_recovered():
    a = np.full((180, 240), .18, np.float32)
    r = compare(a, a * 2 ** .08)
    assert r.quality == "RELIABLE_COMMON_CHANGE"
    assert r.common_ev == pytest.approx(.08, abs=1e-4)


def test_small_moving_bright_patch_does_not_make_global_gain():
    a = np.full((180, 240), .18, np.float32)
    b = a.copy()
    b[100:150, 80:120] = .65
    r = compare(a, b)
    assert r.common_ev == pytest.approx(0, abs=1e-6)


def test_dark_is_not_zero_flicker_pass():
    a = np.zeros((180, 240), np.float32)
    r = compare(a, a)
    assert r.quality == "INSUFFICIENT_SUPPORT" and r.common_ev is None


def test_ramp_has_zero_curvature_but_not_zero_step():
    s = temporal_summary([.02] * 40)
    assert s["curvature_rms_ev"] == 0 and s["median_abs_step_ev"] == .02


def test_alternation_has_large_curvature():
    s = temporal_summary([.1, -.1] * 20)
    assert s["curvature_rms_ev"] == pytest.approx(.1)
    assert s["lag2_residual_rms_ev"] == pytest.approx(.1)


def test_no_temporal_evidence_for_one_frame():
    s = temporal_summary([])
    assert s["curvature_rms_ev"] is None and s["p95_abs_step_ev"] is None


def test_missing_pair_is_not_interpolated_into_evidence():
    s = temporal_summary([.1, None, -.1])
    assert s["curvature_rms_ev"] is None and s["usable_pairs"] == 2


def test_transfer_decode_and_truncated_frame():
    assert inverse_transfer(np.array([0., 1.]), "srgb").tolist() == [0., 1.]
    assert inverse_transfer(np.array([.5]), "bt709") is not None
    with pytest.raises(ValueError):
        inverse_transfer(np.array([.5]), "unknown")
    with pytest.raises(VerificationError):
        read_frame(io.BytesIO(b"123"), 4)


def test_transfer_tag_classification():
    assert classify_transfer_tag("bt709") == "bt709"
    assert classify_transfer_tag("srgb") == "srgb"
    for bad in (None, "unknown", "unspecified"):
        with pytest.raises(ValueError):
            classify_transfer_tag(bad)
    with pytest.raises(ValueError):
        classify_transfer_tag("smpte2084")
    with pytest.raises(ValueError):
        classify_transfer_tag("arib-std-b67")


def test_missing_transfer_without_assumption_is_an_error():
    with pytest.raises(VerificationError):
        resolve_transfer({"color_transfer": None}, None)
    transfer, warnings = resolve_transfer({"color_transfer": None}, "srgb")
    assert transfer == "srgb"  # explicit assumption; unknown-tag warning lives in audit_video
    transfer, warnings = resolve_transfer({"color_transfer": "bt709"}, "srgb")
    assert transfer == "srgb" and any("MISMATCH" in w for w in warnings)


def test_luminance_known_values():
    rgb = np.zeros((1, 2, 3), dtype=np.uint16)
    rgb[0, 0] = 65535
    y = linear_luminance(rgb, "srgb")
    assert y[0, 0] == pytest.approx(1.0)
    assert y[0, 1] == pytest.approx(0.0, abs=1e-9)


def test_roi_level_and_series():
    img = np.full((100, 100), .25, np.float32)
    assert roi_level(img, (0., 0., 1., 1.)) == pytest.approx(-2.0, abs=1e-6)
    m = roi_series_metrics([0., .01, .02, .03])
    assert m["range_ev"] == pytest.approx(.03)
    assert m["observed_frames"] == 4
    assert roi_level(np.zeros((10, 10), np.float32), (0., 0., 1., 1.)) is None


def test_geometry_matches_reference():
    assert analysis_geometry(3648, 2736) == (912, 684)
    assert analysis_geometry(1920, 1080) == (912, 504)  # same banker's rounding as reference


def _encode_video(path: Path, frames: list[np.ndarray]) -> None:
    from rawtl.video import VideoWriter, EncoderCandidate
    h, w = frames[0].shape[:2]
    with VideoWriter(ffmpeg, EncoderCandidate("libx264", "libx264"), path.parent,
                     w + w % 2, h + h % 2, Fraction(30)) as writer:
        for frame in frames:
            writer.write(frame)
        writer.finish(ffprobe, len(frames))
    # VideoWriter encodes to a temp name; move it where the test wants it.
    for candidate in path.parent.glob(".rawtl-encode-*.mp4"):
        candidate.replace(path)


def _srgb_levels(base_linear: float, gain: float) -> np.ndarray:
    target = np.clip(base_linear * gain, 0., 1.)
    return np.clip(np.floor(srgb_oetf(target) * 255 + .5), 0, 255).astype(np.uint8)


@needs_ffmpeg
def test_constant_video_passes_and_short_is_flagged(tmp_path):
    frames = [np.full((360, 480, 3), 0, dtype=np.uint8) for _ in range(30)]
    level = _srgb_levels(.25, 1.)
    for f in frames:
        f[..., :] = level
    video = tmp_path / "constant.mp4"
    _encode_video(video, frames)
    result = audit_video(video, ffmpeg=ffmpeg, ffprobe=ffprobe,
                         config=VideoAuditConfig(transfer="srgb"), output_dir=tmp_path)
    assert result["frame_count"] == 30
    assert result["status"] == "PASS"
    assert result["candidate_count"] == 0
    assert (tmp_path / "temporal_audit.json").exists()
    assert json.loads((tmp_path / "temporal_audit.json").read_text())["status"] == "PASS"


@needs_ffmpeg
def test_alternating_gain_fails_with_events(tmp_path):
    frames = []
    for i in range(30):
        gain = 2 ** .15 if i % 2 else 1.
        level = _srgb_levels(.25, gain)
        frame = np.full((360, 480, 3), 0, dtype=np.uint8)
        frame[..., :] = level
        frames.append(frame)
    video = tmp_path / "flicker.mp4"
    _encode_video(video, frames)
    result = audit_video(video, ffmpeg=ffmpeg, ffprobe=ffprobe,
                         config=VideoAuditConfig(transfer="srgb"))
    assert result["status"] == "FAIL"
    assert result["candidate_count"] >= 20
    # 8-bit quantization + yuv420 encoding shift the injected 0.15 EV by a
    # code step or two; the detector must recover a clear alternating signal.
    event = result["events"][0]
    assert .1 < abs(event["common_ev"]) < .2
    signs = {math.copysign(1, e["common_ev"]) for e in result["events"]}
    assert signs == {1., -1.}
    assert result["chroma_variation"] == "NOT_IMPLEMENTED"


@needs_ffmpeg
def test_boundary_pairs_never_count_as_flicker(tmp_path):
    frames = []
    for i in range(30):
        level = _srgb_levels(.25, 2 ** .3 if i >= 20 else 1.)
        frame = np.full((360, 480, 3), 0, dtype=np.uint8)
        frame[..., :] = level
        frames.append(frame)
    video = tmp_path / "cut.mp4"
    _encode_video(video, frames)
    result = audit_video(video, ffmpeg=ffmpeg, ffprobe=ffprobe,
                         config=VideoAuditConfig(transfer="srgb", boundaries=(20,)))
    pairs = result["segments"]
    assert all(s["max_abs_step_ev"] is None or s["max_abs_step_ev"] < .05
               for s in pairs if s["status"] != "SHORT_CONTEXT")
    assert result["candidate_count"] == 0 or all(
        e["index"] != 20 for e in result["events"])


@needs_ffmpeg
def test_short_segment_is_flagged_not_passed(tmp_path):
    frames = []
    for i in range(8):
        level = _srgb_levels(.25, 2 ** .08 if i % 2 else 1.)
        frame = np.full((360, 480, 3), 0, dtype=np.uint8)
        frame[..., :] = level
        frames.append(frame)
    video = tmp_path / "short.mp4"
    _encode_video(video, frames)
    result = audit_video(video, ffmpeg=ffmpeg, ffprobe=ffprobe,
                         config=VideoAuditConfig(transfer="srgb"))
    assert result["segments"][0]["status"] == "SHORT_CONTEXT"
    assert result["status"] in ("SHORT_CONTEXT", "UNOBSERVABLE")
