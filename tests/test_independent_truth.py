"""F2: independent ground-truth fixtures; acceptance decoupled from the
correction's own self-consistency.

These fixtures encode DESIRED semantics with independent truth (backgrounds
that provably did not change, gains that were provably injected), never the
algebra of the current correction formula.
"""
from __future__ import annotations
import shutil
from fractions import Fraction
from pathlib import Path
import numpy as np
import pytest

from rawtl.color import srgb_oetf
from rawtl.exposure.estimation import estimate_exposure
from rawtl.exposure.measurement import FrameMeasurement
from rawtl.quality.photometry import (tile_statistics, pair_metric,
                                      temporal_summary, TileStats)
from rawtl.quality.video_audit import audit_video, VideoAuditConfig

ffmpeg = shutil.which("ffmpeg")
ffprobe = shutil.which("ffprobe")
needs_ffmpeg = pytest.mark.skipif(not (ffmpeg and ffprobe),
                                  reason="local FFmpeg/ffprobe unavailable")

NX, NY = 12, 8
K = NX * NY


def measurement(tiles: np.ndarray) -> FrameMeasurement:
    centers = np.array([[(i + .5) / NX, (j + .5) / NY]
                        for j in range(NY) for i in range(NX)], dtype=np.float64)
    return FrameMeasurement(tiles.astype(np.float64), np.ones(K, bool), centers,
                            np.zeros(K), 480, 360, "exact")


def background_tiles(levels: np.ndarray, noise: float = 0.) -> np.ndarray:
    """Static reflectance per tile + a common per-frame level."""
    reflectance = np.linspace(-.4, .4, K)
    rng = np.random.default_rng(7)
    return (levels[:, None] + reflectance[None, :] +
            (rng.normal(0, noise, (len(levels), K)) if noise else 0.))


def test_median_of_medians_is_not_matched_background_truth():
    """Mathematical core of the flicker bug: when ~half the frame is a bright
    object, swapping which side of the median the object sits on moves a
    per-frame-median estimator while every background surface is constant."""
    reflectance = np.linspace(0., 1., K)
    frame_a = reflectance.copy()
    frame_b = reflectance.copy()
    frame_b[:K // 2 + 1] += 2.0            # object owns the median rank
    frame_c = reflectance.copy()
    frame_c[K // 2:] += 2.0                # object sits above the median rank
    # Per-frame median jumps ~1.5 EV with ZERO background change...
    assert np.median(frame_b) - np.median(frame_a) > 1.0
    assert abs(np.median(frame_c) - np.median(frame_a)) > .9
    # ...while matched-surface differences on untouched tiles are exactly 0.
    assert np.median(frame_b[K // 2 + 1:] - frame_a[K // 2 + 1:]) == pytest.approx(0, abs=1e-12)
    assert np.median(frame_c[:K // 2] - frame_a[:K // 2]) == pytest.approx(0, abs=1e-12)


def _moving_object_frames(n: int = 30) -> list[FrameMeasurement]:
    """Constant background exposure; a bright object covering ~70% of the
    tiles shrinks steadily to ~11%. The per-frame median's representative
    flips object<->background once mid-sequence although the background
    never changes."""
    tiles = background_tiles(np.zeros(n))
    for t in range(n):
        width = max(8, int(K * (.70 - .02 * t)))
        tiles[t, :width] += 2.0
    return [measurement(x) for x in tiles]


def test_uniform_holds_constant_background_despite_moving_object():
    """DESIRED truth (pairwise default): with a provably constant background
    the correction stays ~0 no matter which object owns the median."""
    result = estimate_exposure(_moving_object_frames(), period=60,
                               brightness="uniform")
    evs = np.array([f.applied_ev if f.applied_ev is not None else 0.
                    for f in result.frames], dtype=float)
    assert np.max(np.abs(evs)) <= .05


def test_legacy_uniform_pumps_on_moving_object_documented():
    """Evidence fixture for the LEGACY estimator: the per-frame median chases
    the object. Kept as the documented failure the pairwise estimator fixes;
    the companion desired-truth test above now runs on the pairwise default."""
    result = estimate_exposure(_moving_object_frames(), period=60,
                               brightness="uniform", estimator="legacy")
    evs = np.array([f.applied_ev for f in result.frames], dtype=float)
    assert np.max(np.abs(evs)) > .25


def test_smooth_keeps_ramp_and_removes_high_frequency():
    """Slow linear EV ramp must survive; +-0.05 EV alternation must not."""
    n = 60
    ramp = np.linspace(0., .8, n)
    flicker = .05 * (-1) ** np.arange(n)
    measurements = [measurement(x) for x in background_tiles(ramp + flicker)]
    result = estimate_exposure(measurements, period=60, brightness="smooth")
    evs = np.array([f.applied_ev for f in result.frames], dtype=float)
    even = evs[0::2].mean()
    odd = evs[1::2].mean()
    assert odd - even > .04          # counter-flicker applied frame by frame
    hf = flicker - np.convolve(flicker, np.ones(9) / 9, mode="same")
    assert abs(np.corrcoef(evs, -hf)[0, 1]) > .5
    assert abs(np.corrcoef(evs, -ramp)[0, 1]) < .5


def test_injected_alternating_gain_is_countered():
    """Known +0.1/-0.1 EV camera gain: the correction must alternate against it."""
    n = 40
    gain = .1 * (-1) ** np.arange(n)
    measurements = [measurement(x) for x in background_tiles(gain)]
    result = estimate_exposure(measurements, period=60, brightness="smooth")
    evs = np.array([f.applied_ev for f in result.frames], dtype=float)
    # even frames gained +0.1 EV, so their correction is negative
    assert evs[0::2].mean() < -.05 and evs[1::2].mean() > .05


def test_validity_set_change_alone_must_not_move_common_level():
    """Static reflectance, changing VALID tile set: the pairwise common level
    on the shared support is exactly unchanged, while the per-frame median of
    available tiles moves purely because its representative changed."""
    values = np.linspace(-.5, .5, K)
    median_all = float(np.median(values))
    median_upper_half = float(np.median(values[K // 2:]))
    common = float(np.median(values[K // 2:] - values[K // 2:]))
    assert abs(median_upper_half - median_all) > .2
    assert common == pytest.approx(0., abs=1e-12)


def test_global_flash_is_candidate_not_diagnosis():
    """A spatially uniform jump is observable as a common change; the detector
    reports a measurable candidate without claiming a physical cause."""
    base = np.full((180, 240), .2, np.float32)
    other = base * 2 ** .3
    sat = np.zeros_like(base)
    ta = tile_statistics(base, sat)
    tb = tile_statistics(other, sat)
    metric, _, _ = pair_metric(tb, ta)
    assert metric.quality == "RELIABLE_COMMON_CHANGE"
    assert metric.common_ev == pytest.approx(.3, abs=1e-3)


def _encode_video(path: Path, frames: list[np.ndarray]) -> None:
    from rawtl.video import VideoWriter, EncoderCandidate
    h, w = frames[0].shape[:2]
    with VideoWriter(ffmpeg, EncoderCandidate("libx264", "libx264"), path.parent,
                     w + w % 2, h + h % 2, Fraction(30)) as writer:
        for frame in frames:
            writer.write(frame)
        writer.finish(ffprobe, len(frames))
    for candidate in path.parent.glob(".rawtl-encode-*.mp4"):
        candidate.replace(path)


def _gray_frame(level_uint8: int) -> np.ndarray:
    frame = np.empty((360, 480, 3), dtype=np.uint8)
    frame[..., :] = level_uint8
    return frame


@needs_ffmpeg
def test_single_bad_frame_is_not_masked_by_percentiles(tmp_path):
    """One clearly wrong frame (<5% of the timeline) must fail the audit even
    though every median/P95 stays clean."""
    frames = [_gray_frame(120) for _ in range(30)]
    frames[15] = _gray_frame(170)
    video = tmp_path / "onebad.mp4"
    _encode_video(video, frames)
    result = audit_video(video, ffmpeg=ffmpeg, ffprobe=ffprobe,
                         config=VideoAuditConfig(transfer="srgb"))
    assert result["status"] == "FAIL"
    assert {e["index"] for e in result["events"]} == {15, 16}


@needs_ffmpeg
def test_fully_unmeasurable_video_is_unobservable_not_zero(tmp_path):
    frames = [np.zeros((360, 480, 3), dtype=np.uint8) for _ in range(30)]
    video = tmp_path / "black.mp4"
    _encode_video(video, frames)
    result = audit_video(video, ffmpeg=ffmpeg, ffprobe=ffprobe,
                         config=VideoAuditConfig(transfer="srgb"))
    assert result["status"] == "UNOBSERVABLE"
    assert result["candidate_count"] == 0
    seg = result["segments"][0]
    assert seg["median_abs_step_ev"] is None


@needs_ffmpeg
def test_stats_never_cross_declared_boundary(tmp_path):
    """A huge legal jump exactly at a declared boundary is excluded from every
    aggregate; the same jump without the boundary declaration fails."""
    frames = [_gray_frame(120 if i < 20 else 220) for i in range(40)]
    video = tmp_path / "boundary.mp4"
    _encode_video(video, frames)
    bounded = audit_video(video, ffmpeg=ffmpeg, ffprobe=ffprobe,
                          config=VideoAuditConfig(transfer="srgb", boundaries=(20,)))
    assert bounded["candidate_count"] == 0
    assert all(s["max_abs_step_ev"] is None or s["max_abs_step_ev"] < .2
               for s in bounded["segments"] if s["status"] != "SHORT_CONTEXT")
    unbounded = audit_video(video, ffmpeg=ffmpeg, ffprobe=ffprobe,
                            config=VideoAuditConfig(transfer="srgb"))
    assert unbounded["status"] == "FAIL" and unbounded["candidate_count"] >= 1


def test_temporal_summary_missing_pair_is_never_interpolated():
    summary = temporal_summary([.1, None, -.1])
    assert summary["usable_pairs"] == 2
    assert summary["curvature_rms_ev"] is None

class MovingObjectRaw:
    """Faithful renderer (gain probe passes): a bright object slides across
    the frame a little each frame. The background exposure never changes, but
    when the object's coverage crosses half the frame the per-frame median's
    representative flips object<->background and the uniform correction makes
    a spurious whole-image EV step -- the same mechanism as the real-output
    events at frames 37/40 in the flicker baseline."""
    def inspect(self, path):
        from rawtl.types import RawMetadata
        return RawMetadata("Test Camera", 320, 240, 0, (2., 1., 1.5, 1.), (2., 1., 1.5, 1.))

    def render_linear(self, path, ev, opts):
        from rawtl.types import LinearFrame
        i = int(path.stem.split("_")[-1])
        v = .18 * 2 ** ev
        rgb = np.full((240, 320, 3), v, dtype=np.float32)
        width = max(16, int(320 * (.70 - .03 * i)))   # 70% -> ~11% coverage
        rgb[:, :width] *= 2.
        return LinearFrame(rgb, np.zeros((240, 320), dtype=bool), "approximate",
                           1 / 65535, False)

    def versions(self):
        return {"rawpy": "synthetic", "libraw": "synthetic"}


@pytest.mark.skipif(not (ffmpeg and ffprobe), reason="FFmpeg unavailable")
def test_wrong_correction_passes_gain_but_fails_temporal_gate(tmp_path):
    """The decoupling proof: gain_response PASS, encoded_temporal_quality FAIL."""
    import json
    from rawtl.service import ProcessingService
    from rawtl.types import ProcessingRequest, VerificationError
    src = tmp_path / "raws"
    src.mkdir()
    for i in range(30):
        (src / f"frame_{i:03d}.DNG").write_bytes(b"synthetic")
    out = tmp_path / "out"
    with pytest.raises(VerificationError):
        # Legacy estimator: documents the median-chasing failure the pairwise
        # default fixes; the pairwise path passes this same scenario's audit.
        ProcessingService(MovingObjectRaw()).run(ProcessingRequest(
            src, out, encoder="libx264", strength=1., period=60,
            brightness="uniform", estimator="legacy"))
    report = json.loads((out / "exposure_report.json").read_text())
    assert report["gain_probe"]["passed"] is True
    assert report["encoded_temporal_quality"]["status"] == "FAIL"
    assert report["encoded_temporal_quality"]["events"]
    assert report["run_status"] == "TEMPORAL_QUALITY_FAILED"
    assert not (out / "video.mp4").exists()

