"""Gain probes and output-domain render verification (maximum two feedback updates).

Two different questions, deliberately separated:
- probe_gain: EXECUTION consistency -- does the RAW backend realise a
  requested EV shift? It says nothing about whether the correction was right.
- evaluate_render: TARGET consistency in the render domain -- do the actually
  rendered frames follow the frozen brightness intent (per-frame level vs
  desired level, anchored by one median offset), with a single-frame gate so
  one severe mismatch cannot hide inside clean P95 statistics?
"""
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
    rendered: Sequence[FrameMeasurement],*, iteration: int=0,
    single_frame_gate_ev: float=.10) -> ValidationSummary:
    """Independent render-domain target check.

    For every frame with consensus support, the actually rendered level is the
    median log-luminance over the SAME consensus tiles; the error is
    desired_level + offset - rendered_level with one shared median offset, so
    the check is about the brightness INTENT (and its execution), never about
    the algebra of the correction that was proposed. One severe frame (>gate)
    fails the whole summary regardless of clean percentiles.
    """
    if len(rendered)!=len(exposure.frames):
        raise ValueError("rendered validation frame count mismatch")
    errors=[]; unobservable=[];outliers=[]
    levels=[]
    for t,(est,m) in enumerate(zip(exposure.frames,rendered)):
        original=exposure.consensus_mask[t]
        if (est.applied_ev is None or est.status in ("LIMITED","REVIEW_REQUIRED")
                or est.desired_level_ev is None or not np.any(original)):
            errors.append(None)
            est.render_target_error_ev=None
            levels.append(None)
            continue
        if len(m.valid)!=len(original):
            errors.append(None);unobservable.append(t)
            est.render_target_error_ev=None
            levels.append(None)
            continue
        usable=original & m.valid & np.isfinite(m.log_luminance)
        initial_count=np.count_nonzero(original)
        if (np.count_nonzero(usable)<max(8,math.ceil(len(original)/4)) or
                np.count_nonzero(usable)<math.ceil(.75*initial_count)):
            errors.append(None);unobservable.append(t)
            est.render_target_error_ev=None
            levels.append(None)
            continue
        level=float(np.median(m.log_luminance[usable]))
        levels.append(level)
        errors.append(None)  # filled after the shared offset is known
    finite_levels=[(t,l) for t,l in enumerate(levels) if l is not None]
    if not finite_levels:
        return ValidationSummary(errors,None,None,outliers,unobservable,False,iteration)
    desired=np.array([exposure.frames[t].desired_level_ev for t,_ in finite_levels])
    measured=np.array([l for _,l in finite_levels])
    offset=float(np.median(measured-desired))
    for (t,_),error in zip(finite_levels,desired+offset-measured):
        errors[t]=float(error)
        exposure.frames[t].render_target_error_ev=float(error)
        if abs(error)>single_frame_gate_ev:
            outliers.append(t)
    finite=np.array([abs(x) for x in errors if x is not None],dtype=np.float64)
    med=float(np.median(finite))
    p95=float(np.percentile(finite,95))
    passed=bool(med<=.01 and p95<=.03 and not outliers and not unobservable)
    return ValidationSummary(errors,med,p95,outliers,unobservable,passed,iteration)

def feedback_update(exposure: ExposureResult, summary: ValidationSummary,
    *, limits:tuple[float,float]|None=None, damping: float=.75,
    max_step_ev: float=.25) -> int:
    """Move applied EV toward the frozen target; the target itself never moves.

    limits=None (default) is unbounded; explicit limits keep feedback inside the
    same clamp the estimator used."""
    lo,hi=limits if limits is not None else (-math.inf,math.inf)
    adjusted=0
    for frame,error in zip(exposure.frames,summary.errors):
        if (error is None or frame.applied_ev is None or frame.status in
            {"LIMITED","REVIEW_REQUIRED","INTERPOLATED_ESTIMATE","DISABLED",
             "SHORT_SEQUENCE_NO_DEFLICKER"}):
            continue
        val=float(np.clip(frame.applied_ev+np.clip(damping*error,-max_step_ev,max_step_ev),lo,hi))
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
