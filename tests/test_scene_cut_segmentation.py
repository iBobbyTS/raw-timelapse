import numpy as np
import pytest
from rawtl.exposure import estimation
from rawtl.exposure.measurement import FrameMeasurement
from rawtl.exposure.estimation import estimate_exposure
from rawtl.types import ExposureStep

CENTERS=np.array([((i+.5)/12,(j+.5)/8) for j in range(8) for i in range(12)])
K=96

def frames(tile_logs):
    return [FrameMeasurement(row.copy(),np.ones(K,dtype=bool),CENTERS,np.zeros(K),
        320,240,"approximate") for row in tile_logs]

def spliced_sequence(n=120,cut=60,seed=20261030):
    """Two content-continuous runs joined at `cut` by a spatially non-uniform
    brightness jump; common flicker continues across the splice."""
    rng=np.random.default_rng(seed)
    t=np.arange(n,dtype=float)
    base=-1.2-.002*t
    flicker=.13*(-1.)**t+.065*np.sin(2*np.pi*t/7)
    offsets=rng.normal(0,.05,size=K)
    x=base[:,None]+offsets[None,:]+flicker[:,None]
    if cut<n:
        jump=1.0+1.5*(np.arange(K)/K)
        x[cut:]+=jump[None,:]
    return x,flicker

def segment_rmse(est,truth,lo,hi):
    e=np.array([est.frames[t].applied_ev+truth[t] for t in range(lo,hi)],dtype=float)
    e-=np.median(e)
    return float(np.sqrt(np.mean(e*e)))

def test_scene_cut_splits_and_both_segments_deflicker():
    x,flicker=spliced_sequence()
    e=estimate_exposure(frames(x))
    assert [c["index"] for c in e.scene_cuts]==[60]
    assert e.scene_cuts[0]["common_tiles"]==K
    assert all(f.applied_ev is not None for f in e.frames)
    assert [f.segment_id for f in e.frames[:60]]==[0]*60
    assert [f.segment_id for f in e.frames[60:]]==[1]*60
    assert segment_rmse(e,flicker,0,60)<.03
    assert segment_rmse(e,flicker,60,120)<.03

def test_without_cuts_the_splice_stays_unreliable():
    # Oracle sensitivity: with scene-cut detection disabled, the same fixture
    # must reproduce the pre-change failure (splice poisons the timeline).
    x,_=spliced_sequence()
    with pytest.MonkeyPatch.context() as mp:
        mp.setattr(estimation,"SCENE_CUT_MEDIAN_EV",1e9)
        e=estimate_exposure(frames(x),estimator="legacy")
    assert any(f.applied_ev is None for f in e.frames)
    assert e.scene_cuts==[]

def test_common_jump_is_flicker_not_a_cut():
    x,flicker=spliced_sequence(cut=120)  # no splice; add a pure common jump
    x[60:]+=1.0
    e=estimate_exposure(frames(x))
    assert e.scene_cuts==[]
    assert all(f.applied_ev is not None for f in e.frames)
    # A sustained common shift is a slow ramp for the period-60 trend, not a
    # content change: no cut, near-jump frames pulled toward continuity, and
    # the new level eventually accepted.
    applied=np.array([f.applied_ev for f in e.frames])
    assert np.median(applied[61:75])<=-.15
    assert abs(np.median(applied[105:120]))<.15

def test_short_middle_segment_passes_through():
    x,flicker=spliced_sequence(n=64,cut=64)
    x[30:]+=1.0+1.5*(np.arange(K)/K)
    x[34:]+=.6+.9*(np.arange(K)/K)[::-1]
    e=estimate_exposure(frames(x))
    assert [c["index"] for c in e.scene_cuts]==[30,34]
    for t in range(30,34):
        assert e.frames[t].status=="SHORT_SEQUENCE_NO_DEFLICKER"
        assert e.frames[t].applied_ev==0.
        assert e.frames[t].segment_id==1
        assert "SHORT_CONTEXT" not in e.frames[t].warnings
    assert all(f.applied_ev is not None for f in e.frames)
    assert segment_rmse(e,flicker,0,30)<.03
    assert segment_rmse(e,flicker,34,64)<.03

def test_confirmed_step_never_triggers_a_cut():
    x,_=spliced_sequence(cut=120)
    x[40:]+=1.0
    e=estimate_exposure(frames(x),steps=[ExposureStep(40,1.)])
    assert e.scene_cuts==[]
    assert all(f.applied_ev is not None for f in e.frames)
    assert e.frames[40].cumulative_confirmed_step_ev==1.
