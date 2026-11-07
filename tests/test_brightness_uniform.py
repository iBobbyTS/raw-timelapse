import numpy as np
import pytest
from rawtl.exposure.measurement import FrameMeasurement
from rawtl.exposure.estimation import estimate_exposure

CENTERS=np.array([((i+.5)/12,(j+.5)/8) for j in range(8) for i in range(12)])
K=96

def frames(rows,valid=None):
    out=[]
    for i,row in enumerate(rows):
        v=np.ones(K,dtype=bool) if valid is None else valid[i]
        out.append(FrameMeasurement(np.asarray(row,dtype=float),v,CENTERS,
            np.zeros(K),320,240,"approximate"))
    return out

def evolving_sequence(n=90,cut=45,ramp=.02,jump=1.5,seed=20261031):
    """Day->dusk ramp, common flicker and a spatially non-uniform splice."""
    rng=np.random.default_rng(seed)
    t=np.arange(n,dtype=float)
    level=-1.2-ramp*t
    flicker=.11*(-1.)**t
    offsets=rng.normal(0,.05,size=K)
    x=level[:,None]+offsets[None,:]+flicker[:,None]
    step=jump*(.8+.4*(np.arange(K)/K))
    x[cut:]+=step[None,:]
    return x

def output_level(rows,est):
    med=np.array([np.median(np.asarray(row)) for row in rows])
    applied=np.array([f.applied_ev for f in est.frames])
    return med+applied

def test_uniform_holds_constant_brightness():
    x=evolving_sequence()
    e=estimate_exposure(frames(x),brightness="uniform")
    assert all(f.status=="UNIFORM" for f in e.frames)
    assert all(f.applied_ev is not None for f in e.frames)
    out=output_level(x,e)
    assert np.max(out)-np.min(out)<1e-9
    # Scene cuts stay report metadata even though smoothing ignores them.
    assert [c["index"] for c in e.scene_cuts]==[45]

def test_uniform_clips_to_ev_limits():
    x=evolving_sequence(n=90,cut=45,ramp=.07,jump=1.5)  # span > 4 EV
    e=estimate_exposure(frames(x),brightness="uniform",ev_limits=(-2.,2.))
    statuses={f.status for f in e.frames}
    assert "LIMITED" in statuses
    clipped=[f for f in e.frames if f.status=="LIMITED"]
    assert all(f.applied_ev in (-2.,2.) for f in clipped)
    assert all("EXPOSURE_LIMITED" in f.warnings for f in clipped)
    assert all(f.applied_ev is not None for f in e.frames)

def test_uniform_unbounded_applies_any_ev():
    x=evolving_sequence(n=90,cut=45,ramp=.07,jump=1.5)  # span > 4 EV
    e=estimate_exposure(frames(x),brightness="uniform")
    assert all(f.status=="UNIFORM" for f in e.frames)
    assert all(f.requested_ev==f.applied_ev for f in e.frames)
    assert all("EXPOSURE_LIMITED" not in f.warnings for f in e.frames)
    span=[f.applied_ev for f in e.frames]
    assert min(span)<-2. or max(span)>2.


def test_uniform_works_short_and_disabled():
    x=evolving_sequence(n=5,cut=5)
    e=estimate_exposure(frames(x),brightness="uniform")
    assert all(f.status=="UNIFORM" and f.applied_ev is not None for f in e.frames)
    z=estimate_exposure(frames(x),brightness="uniform",strength=0)
    assert all(f.status=="DISABLED" and f.applied_ev==0. for f in z.frames)

def test_uniform_unmeasurable_frame_still_review():
    x=evolving_sequence()
    ms=frames(x)
    ms[40].valid[:]=False
    ms[40].log_luminance[:]=np.nan
    e=estimate_exposure(ms,brightness="uniform")
    assert e.frames[40].status=="REVIEW_REQUIRED"
    assert e.frames[40].applied_ev is None
    assert e.frames[41].status=="UNIFORM"

def test_uniform_invalid_mode_rejected():
    x=evolving_sequence(n=12,cut=12)
    with pytest.raises(Exception,match="brightness"):
        estimate_exposure(frames(x),brightness="auto")
