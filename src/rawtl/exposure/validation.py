"""Gain probes and frozen-target re-render verification (maximum two feedback updates)."""
from __future__ import annotations
from dataclasses import dataclass
from typing import Callable, Sequence
import math
import numpy as np
from .measurement import FrameMeasurement, measure_frame
from .estimation import ExposureResult
from ..types import RawRenderer, RawRenderOptions, FrameRef

@dataclass
class GainProbeResult:
    passed: bool
    tested_frames: list[int]
    median_error_ev: float | None
    p95_abs_error_ev: float | None
    observations: int
    reasons: list[str]

@dataclass
class ValidationSummary:
    errors: list[float | None]
    median_abs_error_ev: float | None
    p95_abs_error_ev: float | None
    outlier_frames: list[int]
    unobservable_frames: list[int]
    passed: bool
    iteration: int = 0

def representative_indices(measurements: Sequence[FrameMeasurement]) -> list[int]:
    n=len(measurements)
    if n==0:
        return []
    base=[0,n//2,n-1]
    levels=[float(np.nanmedian(m.log_luminance[m.valid])) if np.any(m.valid) else np.nan
        for m in measurements]
    good=np.flatnonzero(np.isfinite(levels))
    if good.size:
        base.extend([int(good[np.argmin(np.asarray(levels)[good])]),
            int(good[np.argmax(np.asarray(levels)[good])])])
    return list(dict.fromkeys(base))[:5]

def probe_gain(renderer: RawRenderer, frames: Sequence[FrameRef],
    options: RawRenderOptions, baseline: Sequence[FrameMeasurement],
    *, roi: tuple[float,float,float,float]=(0.,0.,1.,1.)) -> GainProbeResult:
    indices=representative_indices(baseline)
    errors=[];reasons=[]
    tested=[]
    for idx in indices:
        valid=baseline[idx].valid
        if not np.any(valid):
            reasons.append(f"frame {idx}: no valid baseline tiles")
            continue
        tested.append(idx)
        for ev in (-.25,.25):
            rendered=measure_frame(renderer.render_linear(frames[idx].path,ev,options),roi)
            good=valid & rendered.valid & np.isfinite(rendered.log_luminance)
            # A useful probe must measure enough same original positions.
            if np.count_nonzero(good)<max(8,math.ceil(len(good)*.25)):
                reasons.append(f"frame {idx} {ev:+g}: insufficient matching unsaturated tiles")
                continue
            delta=rendered.log_luminance[good]-baseline[idx].log_luminance[good]
            errors.extend((delta-ev).tolist())
    if not errors:
        return GainProbeResult(False,tested,None,None,0,reasons+["no usable gain probes"])
    a=np.asarray(errors,dtype=np.float64)
    median_err=float(np.median(a))
    p95=float(np.percentile(np.abs(a),95))
    passed=bool(abs(median_err)<=.02 and p95<=.05 and not reasons)
    if not passed:
        reasons.append(f"gain response out of tolerance median={median_err:+.5f} P95={p95:.5f}")
    return GainProbeResult(passed,tested,median_err,p95,len(errors),reasons)

def evaluate_render(exposure: ExposureResult,
    rendered: Sequence[FrameMeasurement],*, iteration: int=0) -> ValidationSummary:
    if len(rendered)!=len(exposure.frames):
        raise ValueError("rendered validation frame count mismatch")
    errors=[]; unobservable=[];outliers=[]
    for t,(est,m) in enumerate(zip(exposure.frames,rendered)):
        if est.applied_ev is None or est.status=="LIMITED" or est.status=="REVIEW_REQUIRED":
            errors.append(None)
            est.render_target_error_ev=None
            continue
        original=exposure.consensus_mask[t]
        # Interpolated/short sequences do not have consensus masks; they are NOT observed validations.
        if not np.any(original):
            errors.append(None)
            est.render_target_error_ev=None
            continue
        if len(m.valid)!=len(original):
            errors.append(None);unobservable.append(t)
            est.render_target_error_ev=None
            continue
        usable=original & m.valid & np.isfinite(m.log_luminance)
        initial_count=np.count_nonzero(original)
        if (np.count_nonzero(usable)<max(8,math.ceil(len(original)/4)) or
            np.count_nonzero(usable)<math.ceil(.75*initial_count)):
            errors.append(None);unobservable.append(t)
            est.render_target_error_ev=None
            continue
        diff=exposure.frozen_target[t,usable]-m.log_luminance[usable]
        error=float(np.median(diff))
        errors.append(error)
        est.render_target_error_ev=error
        if abs(error)>.10:
            outliers.append(t)
    finite=np.array([abs(x) for x in errors if x is not None],dtype=np.float64)
    if len(finite)==0:
        return ValidationSummary(errors,None,None,outliers,unobservable,False,iteration)
    med=float(np.median(finite))
    p95=float(np.percentile(finite,95))
    return ValidationSummary(errors,med,p95,outliers,unobservable,
        med<=.01 and p95<=.03 and not unobservable,iteration)

def feedback_update(exposure: ExposureResult, summary: ValidationSummary,
    *, limits:tuple[float,float]=(-2.,2.)) -> int:
    """Mutate applied EV only; frozen target and requested EV remain immutable."""
    adjusted=0
    for frame,error in zip(exposure.frames,summary.errors):
        if (error is None or frame.applied_ev is None or frame.status in
            {"LIMITED","REVIEW_REQUIRED","INTERPOLATED_ESTIMATE","DISABLED"}):
            continue
        val=float(np.clip(frame.applied_ev+np.clip(.75*error,-.25,.25),*limits))
        if val!=frame.applied_ev:
            frame.applied_ev=val
            adjusted+=1
    return adjusted

def feedback_diverged(old: ValidationSummary,new: ValidationSummary) -> bool:
    if old.median_abs_error_ev is None or new.median_abs_error_ev is None:
        return False
    for a,b in ((old.median_abs_error_ev,new.median_abs_error_ev),
                (old.p95_abs_error_ev,new.p95_abs_error_ev)):
        if a is not None and b is not None and b-a>max(a*.10,.002):
            return True
    return False
