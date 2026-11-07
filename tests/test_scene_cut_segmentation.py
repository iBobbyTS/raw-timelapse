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

def test_scene_cut_reported_but_never_splits():
    x,flicker=spliced_sequence()
    e=estimate_exposure(frames(x))
    assert [c["index"] for c in e.scene_cuts]==[60]
    assert e.scene_cuts[0]["common_tiles"]==K
    assert all(f.applied_ev is not None for f in e.frames)
    assert {f.segment_id for f in e.frames}=={0}
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

def test_island_frames_get_real_estimates():
    # No segmentation concept: a 4-frame content island is never passed
    # through uncorrected -- it gets a real (interpolated/absolute) estimate,
    # and the discontinuities stay reported metadata.
    x,flicker=spliced_sequence(n=64,cut=64)
    x[30:]+=1.0+1.5*(np.arange(K)/K)
    x[34:]+=.6+.9*(np.arange(K)/K)[::-1]
    e=estimate_exposure(frames(x))
    assert [c["index"] for c in e.scene_cuts]==[30,34]
    assert all(f.applied_ev is not None for f in e.frames)
    assert all(f.status!="SHORT_SEQUENCE_NO_DEFLICKER" for f in e.frames)
    assert segment_rmse(e,flicker,0,30)<.03
    assert segment_rmse(e,flicker,34,64)<.03


def test_whole_mode_never_splits_estimation():
    # Owner directive: deflickering treats the input as ONE sequence in
    # filename order. A confirmed capture-time gap and a detected scene cut
    # stay reported metadata; estimation keeps a single segment.
    x,_=spliced_sequence()
    e=estimate_exposure(frames(x),brightness="uniform",
        time_gaps=[60])
    assert [c["index"] for c in e.scene_cuts]==[60]
    assert e.time_gaps==[60]
    assert {f.segment_id for f in e.frames}=={0}
    assert all(f.applied_ev is not None for f in e.frames)
    assert all(f.status!="SHORT_SEQUENCE_NO_DEFLICKER" for f in e.frames)

def test_whole_mode_uniform_levels_across_a_splice():
    # The non-uniform splice is rejected by pair gates, splitting the graph
    # into components; uniform still yields ONE constant output level,
    # stitched with a recorded cross-component assumption.
    x,_=spliced_sequence()
    e=estimate_exposure(frames(x),brightness="uniform")
    assert [c["index"] for c in e.scene_cuts]==[60]
    assert {f.segment_id for f in e.frames}=={0}
    assert all(f.status=="UNIFORM" and f.applied_ev is not None for f in e.frames)
    applied=np.array([f.applied_ev for f in e.frames])
    level=np.array([np.median(row) for row in x])+applied
    assert level.max()-level.min()<.05



def test_uniform_levels_a_lone_frame_absolutely():
    # A single-frame splice (no pair evidence) must still be held to the one
    # uniform level: it is leveled by its own
    # measured row level against the global level-constant, with an explicit
    # ABSOLUTE_LEVEL status, and the output level stays flat.
    x,_=spliced_sequence(cut=120)
    x[60]+=1.0+1.5*(np.arange(K)/K)
    e=estimate_exposure(frames(x),brightness="uniform")
    assert e.frames[60].status=="ABSOLUTE_LEVEL"
    assert "LONE_FRAME_ABSOLUTE_LEVEL" in e.frames[60].warnings
    assert all(f.applied_ev is not None for f in e.frames)
    applied=np.array([f.applied_ev for f in e.frames])
    level=np.array([np.median(row) for row in x])+applied
    assert level.max()-level.min()<.05



def test_whole_mode_stitch_is_boundary_local():
    # Component-median alignment is NOT enough when the tile-mix offset
    # drifts within a component: the stitch must align the boundary-adjacent
    # frames' whole-frame level, or the video keeps a visible step at the
    # discontinuity while every component is internally flat.
    n,k=40,96
    rng=np.random.default_rng(7)
    off=rng.normal(0,.05,k)
    rank=np.argsort(np.argsort(off))/k-.5   # per-tile mix weight
    x=np.empty((n,k))
    flicker=.05*(-1.)**np.arange(n)
    for t in range(n):
        x[t]=off+flicker[t]+.02*t*rank      # slow mix drift within components
    x[20:]+=np.where(np.arange(k)%100<45,2.0,0.)  # selective jump: no usable
    # cross pairs (consensus <60%), whole-frame median unchanged (55% intact)
    e=estimate_exposure(frames(x),brightness="uniform")
    assert {f.segment_id for f in e.frames}=={0}
    # the selective jump is invisible to the median-based cut detector, but the
    # pair gates still reject every cross pair -> two components get stitched
    assert len(e.pairwise_summary["cross_boundary_anchors"]["entries"])>=1
    applied=np.array([f.applied_ev if f.applied_ev is not None else np.nan
        for f in e.frames])
    row=np.array([np.median(r) for r in x])
    step=(row+applied)[20]-(row+applied)[19]
    assert abs(step)<.05


def test_confirmed_step_never_triggers_a_cut():
    x,_=spliced_sequence(cut=120)
    x[40:]+=1.0
    e=estimate_exposure(frames(x),steps=[ExposureStep(40,1.)])
    assert e.scene_cuts==[]
    assert all(f.applied_ev is not None for f in e.frames)
    assert e.frames[40].cumulative_confirmed_step_ev==1.
