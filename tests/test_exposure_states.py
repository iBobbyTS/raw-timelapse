import numpy as np
from rawtl.exposure.measurement import FrameMeasurement
from rawtl.exposure.estimation import estimate_exposure
from rawtl.types import ExposureStep

CENTERS=np.array([((i+.5)/12,(j+.5)/8) for j in range(8) for i in range(12)])
def sequence(n=90):
    return [FrameMeasurement(np.full(96,-2.+.001*i),np.ones(96,dtype=bool),
        CENTERS,np.zeros(96),320,240,"approximate") for i in range(n)]

def test_long_gap_and_edge_are_review():
    s=sequence()
    for i in list(range(0,2))+list(range(20,27)):
        s[i].valid[:]=False
    e=estimate_exposure(s)
    assert e.frames[0].status=="REVIEW_REQUIRED"
    assert e.frames[22].applied_ev is None
    assert e.frames[28].applied_ev is not None

def test_short_gap_across_confirmed_step():
    s=sequence()
    for i in [40,41]:
        s[i].valid[:]=False
    e=estimate_exposure(s,steps=[ExposureStep(41,.4)])
    assert e.frames[40].status=="INTERPOLATED_ESTIMATE"
    assert e.frames[41].cumulative_confirmed_step_ev==.4
    assert e.frames[41].requested_ev is not None
    assert e.frames[41].requested_ev-e.frames[40].requested_ev == -0.4 + (e.frames[40].common_residual_ev-e.frames[41].common_residual_ev)

def test_limit_keeps_request_distinct():
    s=sequence(5)
    e=estimate_exposure(s,steps=[ExposureStep(3,5.)])
    assert any(f.status=="LIMITED" for f in e.frames)
    assert all(f.applied_ev is not None for f in e.frames)
