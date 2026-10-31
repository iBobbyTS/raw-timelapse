import csv
import json
import shutil
from pathlib import Path
import numpy as np
import pytest
from rawtl.types import (
    LinearFrame,RawMetadata,ProcessingRequest,CancellationToken,ProcessingCancelled,
    RawDecodeError,ExposureError,InputError)
from rawtl.service import ProcessingService

FF=shutil.which("ffmpeg");FP=shutil.which("ffprobe")

class FakeRaw:
    def inspect(self,path):
        return RawMetadata("Test Camera",320,240,0,(2.,1.,1.5,1.),(2.,1.,1.5,1.))
    def render_linear(self,path,ev,opts):
        i=int(path.stem.split("_")[-1])
        v=.18*2**(ev+.04*(-1)**i)
        return LinearFrame(np.full((240,320,3),v,dtype=np.float32),
            np.zeros((240,320),dtype=bool),"approximate",1/65535,False)
    def versions(self):
        return {"rawpy":"synthetic","libraw":"synthetic"}

class BlackRaw(FakeRaw):
    def render_linear(self,path,ev,opts):
        f=super().render_linear(path,ev,opts)
        f.rgb.fill(0.)
        return f

class BrokenRaw(FakeRaw):
    def render_linear(self,path,ev,opts):
        if path.stem=="frame_003":
            raise RawDecodeError("injected decoder error")
        return super().render_linear(path,ev,opts)

def folder(tmp_path,n=12):
    src=tmp_path/"raws"
    src.mkdir()
    for i in range(n):
        (src/f"frame_{i:03d}.DNG").write_bytes(b"synthetic")
    return src

@pytest.mark.skipif(not (FF and FP),reason="FFmpeg unavailable")
def test_service_e2e_synthetic_no_deflicker(tmp_path):
    src=folder(tmp_path)
    out=tmp_path/"out"
    req=ProcessingRequest(src,out,strength=0.,save_frames=True,encoder="libx264")
    result=ProcessingService(FakeRaw()).run(req)
    assert result.run_status=="OK"
    assert (out/"video.mp4").is_file()
    assert len(list((out/"frames").glob("*.png")))==12
    report=json.loads((out/"exposure_report.json").read_text())
    assert len(report["frames"])==12
    assert report["encoder"]["selected"]=="libx264"
    assert [x["render_ev"] for x in report["frames"]]==[0.]*12
    with (out/"exposure_curve.csv").open(newline="") as f:
        assert len(list(csv.DictReader(f)))==12
    with pytest.raises(InputError):
        ProcessingService(FakeRaw()).run(req)
    assert (out/"video.mp4").is_file()

@pytest.mark.skipif(not (FF and FP),reason="FFmpeg unavailable")
def test_review_required_and_passthrough(tmp_path):
    src=folder(tmp_path)
    out=tmp_path/"review"
    with pytest.raises(ExposureError):
        ProcessingService(BlackRaw()).run(ProcessingRequest(src,out,encoder="libx264"))
    assert not (out/"video.mp4").exists()
    report=json.loads((out/"exposure_report.json").read_text())
    assert report["run_status"]=="REVIEW_REQUIRED"
    result=ProcessingService(BlackRaw()).run(
        ProcessingRequest(src,out,on_review="passthrough",encoder="libx264"))
    assert result.run_status=="PARTIAL"
    rows=json.loads((out/"exposure_report.json").read_text())["frames"]
    assert all(x["applied_ev"] is None and x["render_ev"]==0 for x in rows)

class SplicedRaw(FakeRaw):
    """Two content-continuous runs joined at frame 15 by a spatially
    non-uniform brightness jump (scene cut); the +-0.04 EV common flicker
    continues across the splice."""
    def render_linear(self,path,ev,opts):
        i=int(path.stem.split("_")[-1])
        v=.18*2**(ev+.04*(-1)**i)
        top=v*(2.**2.0 if i>=15 else 1.)
        bottom=v*(2.**.5 if i>=15 else 1.)
        rgb=np.empty((240,320,3),dtype=np.float32)
        rgb[:120]=top
        rgb[120:]=bottom
        return LinearFrame(rgb,np.zeros((240,320),dtype=bool),"approximate",1/65535,False)

@pytest.mark.skipif(not (FF and FP),reason="FFmpeg unavailable")
def test_service_e2e_uniform_brightness_flattens_splice(tmp_path):
    src=folder(tmp_path,n=30)
    out=tmp_path/"uniform"
    result=ProcessingService(SplicedRaw()).run(
        ProcessingRequest(src,out,encoder="libx264",strength=1.,period=60,
            brightness="uniform"))
    assert result.run_status=="OK"
    report=json.loads((out/"exposure_report.json").read_text())
    assert report["config"]["brightness"]=="uniform"
    rows=report["frames"]
    assert all(r["applied_ev"] is not None for r in rows)
    # The +1.25 EV median splice jump is fully removed, not preserved. The
    # +-0.04 EV alternating flicker is asymmetric inside each 15-frame block
    # (8 vs 7 parities), adding +0.08 EV to the block-median difference.
    before=np.median([r["applied_ev"] for r in rows[:15]])
    after=np.median([r["applied_ev"] for r in rows[15:]])
    assert after-before==pytest.approx(-1.25+.08,abs=.02)
    assert report["video_metadata"]["frames"]==30
    assert (out/"video.mp4").is_file()

def test_service_e2e_spliced_sequence_deflickers_per_segment(tmp_path):
    src=folder(tmp_path,n=30)
    out=tmp_path/"spliced"
    result=ProcessingService(SplicedRaw()).run(
        ProcessingRequest(src,out,encoder="libx264",strength=1.,period=60))
    assert result.run_status=="OK"
    report=json.loads((out/"exposure_report.json").read_text())
    assert [c["index"] for c in report["scene_cuts"]]==[15]
    rows=report["frames"]
    assert [x["segment_id"] for x in rows[:15]]==[0]*15
    assert [x["segment_id"] for x in rows[15:]]==[1]*15
    assert all(x["applied_ev"] is not None for x in rows)
    assert report["video_metadata"]["frames"]==30
    assert (out/"video.mp4").is_file()

def test_decode_failure_and_cancel(tmp_path):
    src=folder(tmp_path)
    out=tmp_path/"broken"
    with pytest.raises(RawDecodeError):
        ProcessingService(BrokenRaw()).run(ProcessingRequest(src,out,strength=0.))
    assert not (out/"video.mp4").exists()
    token=CancellationToken()
    def cancel_first(ev):
        if ev.phase=="analysis":token.cancel()
    with pytest.raises(ProcessingCancelled):
        ProcessingService(FakeRaw()).run(
            ProcessingRequest(src,tmp_path/"cancel",strength=0.),
            on_progress=cancel_first,cancel=token)
    assert not (tmp_path/"cancel"/"video.mp4").exists()
    assert json.loads((tmp_path/"cancel"/"exposure_report.json").read_text())["run_status"]=="CANCELLED"

@pytest.mark.skipif(not (FF and FP),reason="FFmpeg unavailable")
def test_service_e2e_with_deflicker_gain_probe_and_frozen_validation(tmp_path):
    src=folder(tmp_path,n=30)
    out=tmp_path/"deflicker"
    result=ProcessingService(FakeRaw()).run(
        ProcessingRequest(src,out,encoder="libx264",strength=1.,period=60))
    assert result.run_status=="OK"
    report=json.loads((out/"exposure_report.json").read_text())
    assert report["gain_probe"]["passed"] is True
    assert report["run_status"]=="OK"
    assert report["feedback_history"][-1]["passed"] is True
    assert len(report["frames"])==30
    assert report["status_counts"].get("OK")==30
    requested=[row["requested_ev"] for row in report["frames"]]
    assert all(x is not None for x in requested)
    # The known alternating exposure error is reversed rather than flattened
    # after sRGB conversion; fixed target remains in linear light.
    even=np.mean(requested[::2])
    odd=np.mean(requested[1::2])
    assert odd-even > .04
    assert report["encoder"]["selected"]=="libx264"
    assert report["video_metadata"]["frames"]==30
