"""F7: output colour contract and encoder-difference verification."""
from __future__ import annotations
import json
import shutil
from fractions import Fraction
from pathlib import Path
import numpy as np
import pytest

from rawtl.color import srgb_oetf
from rawtl.video import EncoderCandidate, VideoWriter, ffmpeg_command, verify_video
from rawtl.quality.video_audit import audit_video, VideoAuditConfig
from rawtl.quality.photometry import classify_transfer_tag

ffmpeg = shutil.which("ffmpeg")
ffprobe = shutil.which("ffprobe")
needs_ffmpeg = pytest.mark.skipif(not (ffmpeg and ffprobe),
                                  reason="local FFmpeg/ffprobe unavailable")


def test_ffmpeg_command_declares_srgb_contract():
    cmd = ffmpeg_command("ffmpeg", EncoderCandidate("libx264", "libx264"),
                         320, 240, Fraction(30), Path("out.mp4"))
    text = " ".join(cmd)
    assert "-color_trc iec61966-2-1" in text
    assert "-color_primaries bt709" in text and "-colorspace bt709" in text
    assert "-color_range tv" in text
    assert "out_color_matrix=bt709" in text      # explicit matrix, no defaults


def test_srgb_transfer_tag_maps_in_audit():
    assert classify_transfer_tag("iec61966-2-1") == "srgb"


def _gray_frames(levels):
    frames = []
    for linear in levels:
        code = int(np.clip(np.floor(srgb_oetf(np.float32(linear)) * 255 + .5), 0, 255))
        frame = np.empty((240, 320, 3), dtype=np.uint8)
        frame[..., :] = code
        frames.append(frame)
    return frames


def _encode(path: Path, frames, codec: str):
    candidate = (EncoderCandidate("libx264", "libx264") if codec == "libx264"
                 else EncoderCandidate("videotoolbox", "h264_videotoolbox"))
    with VideoWriter(ffmpeg, candidate, path.parent, 320, 240,
                     Fraction(30)) as writer:
        for frame in frames:
            writer.write(frame)
        info = writer.finish(ffprobe, len(frames))
    writer.path.replace(path)
    return info


@needs_ffmpeg
def test_written_stream_declares_the_contract(tmp_path):
    frames = _gray_frames([.25] * 30)
    info = _encode(tmp_path / "c.mp4", frames, "libx264")
    tags = info["color_tags"]
    assert tags["transfer"] == "iec61966-2-1"
    assert tags["matrix"] == "bt709" and tags["range"] in ("tv", "limited")
    assert info["avg_bitrate_bps"] is not None and info["avg_bitrate_bps"] > 0


@needs_ffmpeg
def test_audit_without_explicit_transfer_uses_stream_tag(tmp_path):
    frames = _gray_frames([.25] * 30)
    video = tmp_path / "tagged.mp4"
    _encode(video, frames, "libx264")
    result = audit_video(video, ffmpeg=ffmpeg, ffprobe=ffprobe,
                         config=VideoAuditConfig())   # no explicit transfer
    assert result["transfer_assumed"] == "srgb"
    assert not any("MISMATCH" in w for w in result["metadata_warnings"])


@needs_ffmpeg
def test_same_frames_two_encoders_temporal_difference(tmp_path):
    """Identical pre-encode frames through x264 and VideoToolbox: the temporal
    error each encoder ADDS must be tiny (constant content)."""
    frames = _gray_frames([.25] * 30)
    a = tmp_path / "sw.mp4"
    b = tmp_path / "hw.mp4"
    _encode(a, frames, "libx264")
    try:
        _encode(b, frames, "videotoolbox")
    except Exception:
        pytest.skip("VideoToolbox unavailable on this machine")
    # VideoToolbox drops the VUI transfer tag (recorded as a contract
    # warning); the A/B audit states the known pixel encoding explicitly.
    audit_a = audit_video(a, ffmpeg=ffmpeg, ffprobe=ffprobe,
                          config=VideoAuditConfig(transfer="srgb"))
    audit_b = audit_video(b, ffmpeg=ffmpeg, ffprobe=ffprobe,
                          config=VideoAuditConfig(transfer="srgb"))
    seg_a, seg_b = audit_a["segments"][0], audit_b["segments"][0]
    assert seg_a["max_abs_step_ev"] is not None and seg_a["max_abs_step_ev"] < .05
    assert seg_b["max_abs_step_ev"] is not None and seg_b["max_abs_step_ev"] < .05
