import numpy as np
import pytest
from rawtl.types import FrameRef,LinearFrame,RawRenderOptions
from rawtl.exposure.measurement import measure_frame,FrameMeasurement
from rawtl.exposure.validation import (
    probe_gain,evaluate_render,feedback_update,feedback_diverged,ValidationSummary)
from rawtl.exposure.estimation import estimate_exposure

class MockRenderer:
    def __init__(self,factor=1):
        self.factor=factor
    def render_linear(self,path,exposure_ev,options):
        gain=2**(self.factor*exposure_ev)
        return LinearFrame(np.full((320,480,3),.18*gain,dtype=np.float32),
            None,"approximate",0.0,False)

def test_probe_good_and_catches_double_gain(tmp_path):
    options=RawRenderOptions((2,1,1.5,1))
    frames=[FrameRef(i,tmp_path/f"{i}.dng",f"{i}.dng") for i in range(9)]
    m=[measure_frame(MockRenderer().render_linear(f.path,0,options)) for f in frames]
    assert probe_gain(MockRenderer(),frames,options,m).passed
    bad=probe_gain(MockRenderer(factor=2),frames,options,m)
    assert not bad.passed
    assert bad.p95_abs_error_ev>.05

def test_freeze_and_feedback():
    n=40
    centers=np.array([((i+.5)/12,(j+.5)/8) for j in range(8) for i in range(12)])
    flicker=np.array([.1*(-1)**t for t in range(n)])
    original=[FrameMeasurement(np.full(96,-2.+flicker[t]),np.ones(96,bool),centers,np.zeros(96),
        320,240,"approximate") for t in range(n)]
    exposure=estimate_exposure(original)
    frozen=exposure.frozen_target.copy()
    # Simulate a renderer that applies only 80% of requested gain.
    rendered=[FrameMeasurement(original[t].log_luminance+.8*exposure.frames[t].applied_ev,
        np.ones(96,bool),centers,np.zeros(96),320,240,"approximate") for t in range(n)]
    before=evaluate_render(exposure,rendered)
    assert before.median_abs_error_ev>.01
    assert feedback_update(exposure,before)>0
    np.testing.assert_array_equal(exposure.frozen_target,frozen)
    assert not feedback_diverged(
        ValidationSummary([],0.02,0.04,[],[],False),
        ValidationSummary([],0.021,0.041,[],[],False))
    assert feedback_diverged(
        ValidationSummary([],0.01,0.03,[],[],False),
        ValidationSummary([],0.02,0.05,[],[],False))
