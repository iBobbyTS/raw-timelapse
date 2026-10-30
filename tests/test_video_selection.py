from pathlib import Path
from rawtl.video import candidates,amd_vaapi_devices,VIDEO_BITRATE_BPS,ffmpeg_command,EncoderCandidate
from fractions import Fraction
import os

def test_priority_by_platform():
    assert [x.name for x in candidates("windows")]==["nvenc","amf","libx264"]
    assert [x.name for x in candidates("darwin")]==["videotoolbox","libx264"]
    assert [x.name for x in candidates("linux",["/dev/dri/renderD128"])]==["nvenc","vaapi","libx264"]
    assert [x.name for x in candidates("linux",[])]==["nvenc","libx264"]

def test_vendor_filter(tmp_path,monkeypatch):
    sys=tmp_path/"sys";dev=tmp_path/"dev"
    for name,vendor in [("renderD128","0x1002"),("renderD129","0x8086")]:
        (sys/name/"device").mkdir(parents=True)
        (sys/name/"device"/"vendor").write_text(vendor)
        dev.mkdir(exist_ok=True)
        (dev/name).write_text("")
    assert amd_vaapi_devices(sys,dev)==[str(dev/"renderD128")]

def test_hardcoded_target_and_safe_args(tmp_path):
    assert VIDEO_BITRATE_BPS==160_000_000
    args=ffmpeg_command("ffmpeg",EncoderCandidate("libx264","libx264"),
        320,240,Fraction(30000,1001),tmp_path/"clip with space.mp4")
    assert "160000000" in args
    assert "30000/1001" in args
    assert "pipe:0" in args
    assert args[-1]==str(tmp_path/"clip with space.mp4")
    assert "-nostdin" in args
