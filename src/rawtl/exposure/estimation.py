"""Robust common-flicker inference with explicit steps, quality states and EV limits."""
from __future__ import annotations
from dataclasses import dataclass, field
from typing import Sequence
import math
import numpy as np
from ..types import ExposureError, ExposureStep
from .measurement import FrameMeasurement
from .trends import huber_whittaker

@dataclass
class FrameEstimate:
    index: int
    status: str
    common_residual_ev: float | None
    requested_ev: float | None
    applied_ev: float | None
    cumulative_confirmed_step_ev: float
    valid_tile_count: int
    consensus_fraction: float | None
    residual_mad_ev: float | None
    warnings: list[str] = field(default_factory=list)
    render_target_error_ev: float | None = None
    new_output_clipping_fraction: float | None = None
    render_ev: float | None = None
    @property
    def gain(self) -> float | None:
        return None if self.applied_ev is None else float(2**self.applied_ev)

@dataclass
class ExposureResult:
    frames: list[FrameEstimate]
    tile_log_luminance: np.ndarray
    tile_valid: np.ndarray
    consensus_mask: np.ndarray
    frozen_target: np.ndarray
    trusted_indices: np.ndarray
    trend_unconverged_tiles: int
    period_effective: int

def _step_vector(n: int,steps: Sequence[ExposureStep]) -> np.ndarray:
    vector=np.zeros(n,dtype=np.float64)
    for s in steps:
        if type(s.start_frame) is not int or not 0<=s.start_frame<n or not math.isfinite(s.delta_ev):
            raise ExposureError(f"invalid confirmed step {s}")
        vector[s.start_frame]+=s.delta_ev
    return np.cumsum(vector)

def estimate_exposure(measurements: Sequence[FrameMeasurement],*,period: int=60,
    strength: float=1.,steps: Sequence[ExposureStep]=(),ev_limits: tuple[float,float]=(-2.,2.)) -> ExposureResult:
    n=len(measurements)
    if n<1:
        raise ExposureError("empty exposure sequence")
    if period<6 or not math.isfinite(strength) or not 0<=strength<=1:
        raise ExposureError("invalid period or strength")
    low,high=ev_limits
    if not math.isfinite(low+high) or not low<high:
        raise ExposureError("invalid EV limits")
    k=len(measurements[0].log_luminance)
    if k<8:
        raise ExposureError("insufficient tile grid")
    centers=measurements[0].centers
    for i,m in enumerate(measurements):
        if len(m.log_luminance)!=k or not np.array_equal(m.centers,centers):
            raise ExposureError(f"processing conditions changed at frame {i}")
    x=np.asarray([m.log_luminance for m in measurements],dtype=np.float64)
    q=np.asarray([m.valid for m in measurements],dtype=bool)
    if np.isfinite(x[q]).all() is False:
        raise ExposureError("nonfinite valid photometry")
    s=_step_vector(n,steps)
    consensus=np.zeros((n,k),dtype=bool)
    residual=np.full(n,np.nan,dtype=np.float64)
    mad=np.full(n,np.nan,dtype=np.float64)
    fractions=np.full(n,np.nan,dtype=np.float64)
    counts=np.zeros(n,dtype=int)
    reliable=np.zeros(n,dtype=bool)
    interpolated=np.zeros(n,dtype=bool)
    failed_tiles=0
    if strength==0 or n<8:
        residual[:]=0
        reliable[:]=True
        if n<8 and strength!=0:
            labels=["SHORT_SEQUENCE_NO_DEFLICKER"]*n
        else:
            labels=["DISABLED"]*n
        # For short sequence, only confirmed steps are applied.
    else:
        a=x-s[:,None]
        trend=np.full_like(a,np.nan)
        for col in range(k):
            fit=huber_whittaker(a[:,col],q[:,col],period)
            if fit.converged:
                trend[:,col]=fit.trend
            elif np.count_nonzero(q[:,col])>=8:
                failed_tiles+=1
        r=a-trend
        for t in range(n):
            ok=q[t] & np.isfinite(r[t])
            counts[t]=int(np.count_nonzero(ok))
            if counts[t]<max(8,math.ceil(k/4)):
                continue
            med=float(np.median(r[t,ok]))
            dev=np.abs(r[t,ok]-med)
            scaled_mad=float(1.4826*np.median(dev))
            agree=dev<=.06
            fraction=float(np.mean(agree))
            coord=centers[ok][agree]
            quadrants={(int(px>=.5),int(py>=.5)) for px,py in coord}
            residual[t]=med
            mad[t]=scaled_mad
            fractions[t]=fraction
            if scaled_mad<=.06 and fraction>=.60 and len(quadrants)>=3:
                reliable[t]=True
                indices=np.flatnonzero(ok)[agree]
                consensus[t,indices]=True
            else:
                residual[t]=np.nan
        labels=["OK" if b else "REVIEW_REQUIRED" for b in reliable]
        # Only small internal gaps, never extrapolate an edge gap.
        good=np.flatnonzero(reliable)
        for a_idx,b_idx in zip(good[:-1],good[1:]):
            missing=b_idx-a_idx-1
            if 1<=missing<=3:
                for t in range(a_idx+1,b_idx):
                    residual[t]=float(np.interp(t,[a_idx,b_idx],[residual[a_idx],residual[b_idx]]))
                    interpolated[t]=True
                    labels[t]="INTERPOLATED_ESTIMATE"
    raw=np.full(n,np.nan,dtype=np.float64)
    accepted=reliable|interpolated
    if strength==0:
        raw[:]=0.
    else:
        raw[accepted]=strength*(-s[accepted]-residual[accepted])
    if not np.any(reliable):
        anchor=0.
    else:
        anchor=-float(np.median(raw[reliable]))
    requested=raw+anchor
    frozen=np.full_like(x,np.nan)
    frames=[]
    for t in range(n):
        val=float(requested[t]) if np.isfinite(requested[t]) else None
        applied=float(np.clip(val,low,high)) if val is not None else None
        status=labels[t]
        warnings=[]
        if n<2*period and strength!=0:
            warnings.append("SHORT_CONTEXT")
        if failed_tiles:
            warnings.append(f"TREND_NONCONVERGED_TILES={failed_tiles}")
        if applied is not None and val!=applied:
            status="LIMITED"
            warnings.append("EXPOSURE_LIMITED")
        elif not accepted[t]:
            status="REVIEW_REQUIRED"
        if interpolated[t]:
            warnings.append("INTERPOLATED_NOT_VERIFIED")
        if applied is not None:
            frozen[t,q[t]]=x[t,q[t]]+val
        frames.append(FrameEstimate(t,status,
            float(residual[t]) if np.isfinite(residual[t]) else None,val,applied,
            float(s[t]),int(counts[t]) if n>=8 and strength!=0 else int(np.sum(q[t])),
            float(fractions[t]) if np.isfinite(fractions[t]) else None,
            float(mad[t]) if np.isfinite(mad[t]) else None,warnings))
    return ExposureResult(frames,x,q,consensus,frozen,reliable,failed_tiles,min(period,n))
