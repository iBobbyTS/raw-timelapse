import math
import numpy as np
import pytest
from rawtl.exposure.trends import huber_whittaker,lambda_for_period
from rawtl.exposure.measurement import FrameMeasurement
from rawtl.exposure.estimation import estimate_exposure
from rawtl.types import ExposureStep

def synthetic(n=240,k=96,noise=0.,polluted=0.,seed=20261030,flash=True):
    rng=np.random.default_rng(seed)
    t=np.arange(n,dtype=float)
    base=-1.2-.002*t+.11*np.sin(2*np.pi*t/280)
    flicker=.13*(-1.)**t+.065*np.sin(2*np.pi*t/7)+.045*np.sin(2*np.pi*t/17)
    if flash and n>120:
        flicker[42]+=.4
        flicker[118]-=.27
    offsets=rng.normal(0,.2,size=k)
    x=base[:,None]+offsets[None,:]+flicker[:,None]
    if noise: x=x+rng.normal(0,noise,size=(n,k))
    if polluted:
        for i in range(n):
            cols=(np.arange(polluted)+i*7)%k
            x[i,cols]+=.35*np.sin(i/9)+.05
    centers=np.array([((i+.5)/12,(j+.5)/8) for j in range(8) for i in range(12)])
    m=[FrameMeasurement(x[i].copy(),np.ones(k,dtype=bool),centers,np.zeros(k),320,240,"approximate") for i in range(n)]
    return m,flicker

def metric(est,truth):
    c=np.array([f.applied_ev for f in est.frames],dtype=float)
    e=c+truth
    e-=np.median(e)
    return np.sqrt(np.mean(e*e)),np.percentile(abs(e),95)

def test_period_and_constant_linear():
    assert lambda_for_period(60)==pytest.approx(8330.6591,rel=1e-6)
    for x in [np.full(300,-1.5), -1.5+.001*np.arange(300)]:
        z=huber_whittaker(x,np.ones(len(x),bool),60)
        assert z.converged
        assert max(abs(z.trend-x))<1e-7

def test_clean_flicker():
    m,f=synthetic()
    e=estimate_exposure(m)
    assert all(x.applied_ev is not None for x in e.frames)
    assert metric(e,f)[0] <.015

def test_polluted_tiles():
    m,f=synthetic(300,96,noise=.015,polluted=24)
    e=estimate_exposure(m)
    assert all(x.applied_ev is not None for x in e.frames), [
        (i,z.status,z.valid_tile_count,z.consensus_fraction,z.residual_mad_ev)
        for i,z in enumerate(e.frames) if z.applied_ev is None][:20]
    rmse,p95=metric(e,f)
    assert rmse<.025 and p95<.04

def test_user_step_only_and_short_sequence():
    m,_=synthetic(100,flash=False)
    for i in range(40,100):
        m[i].log_luminance+=1.
    e=estimate_exposure(m,steps=[ExposureStep(40,1.)])
    assert np.median([x.applied_ev for x in e.frames[45:]])-np.median([x.applied_ev for x in e.frames[:35]])==pytest.approx(-1,abs=.06)
    small,_=synthetic(5,flash=False)
    zero=estimate_exposure(small)
    assert all(f.applied_ev==0 and f.status=="SHORT_SEQUENCE_NO_DEFLICKER" for f in zero.frames)

def test_disabled():
    m,_=synthetic()
    e=estimate_exposure(m,strength=0)
    assert all(f.applied_ev==0 and f.status=="DISABLED" for f in e.frames)
