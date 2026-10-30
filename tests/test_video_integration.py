"""Real software H.264 encoding, not a mock of ffprobe."""
from fractions import Fraction
from pathlib import Path
import shutil
import numpy as np
import pytest
from rawtl.video import VideoWriter,verify_video,EncoderCandidate,probe_encoders

ffmpeg=shutil.which("ffmpeg")
ffprobe=shutil.which("ffprobe")

@pytest.mark.skipif(not (ffmpeg and ffprobe),reason="local FFmpeg/ffprobe unavailable")
def test_actual_h264_12_frames(tmp_path):
    c=EncoderCandidate("libx264","libx264")
    with VideoWriter(ffmpeg,c,tmp_path,320,240,Fraction(30)) as writer:
        for i in range(12):
            img=np.full((240,320,3),i*10,dtype=np.uint8)
            writer.write(img)
        info=writer.finish(ffprobe,12)
        assert info["frames"]==12
        assert info["codec"]=="h264"
        assert info["width"]==320 and info["height"]==240
        assert info["pix_fmt"]=="yuv420p"
        writer.path.unlink()

@pytest.mark.skipif(not (ffmpeg and ffprobe),reason="local FFmpeg/ffprobe unavailable")
def test_auto_encoder_finally_has_cpu(tmp_path):
    available,attempts=probe_encoders(ffmpeg,ffprobe,320,240,Fraction(30),tmp_path)
    assert available
    assert any(a.encoder=="libx264" and a.available for a in attempts)

def test_wrong_raw_byte_shape(tmp_path):
    c=EncoderCandidate("libx264","libx264")
    if not ffmpeg:pytest.skip("FFmpeg unavailable")
    with pytest.raises(Exception):
        with VideoWriter(ffmpeg,c,tmp_path,320,240,Fraction(30)) as w:
            w.write(np.zeros((120,320,3),dtype=np.uint8))
    assert not list(tmp_path.glob(".rawtl-encode-*.mp4"))
