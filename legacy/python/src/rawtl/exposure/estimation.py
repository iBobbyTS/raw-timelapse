"""Robust common-flicker inference with explicit steps, quality states and optional EV limits.

Filename-sorted frames form one timeline. Content jumps (scene cuts) split it
into independently smoothed segments so a splice between distant moments never
poisons neighbouring frames, while camera-level exposure jumps stay common
flicker the estimator removes.
"""
from __future__ import annotations
from dataclasses import dataclass, field
from typing import Sequence
import math
import numpy as np
from ..types import ExposureError, ExposureStep
from .measurement import FrameMeasurement
from .trends import huber_whittaker
from .pairwise import build_edges, persistent_background
from .graph import solve_exposure_graph

# A scene cut needs BOTH a large common shift and large cross-tile disagreement:
# a spatially uniform jump is common flicker to remove, not a content change.
SCENE_CUT_MEDIAN_EV = .20
SCENE_CUT_TILE_MAD_EV = .08

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
    measured_level_ev: float | None = None
    desired_level_ev: float | None = None
    segment_id: int = 0
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
    scene_cuts: list[dict] = field(default_factory=list)
    time_gaps: list[int] = field(default_factory=list)
    pairwise_summary: dict = field(default_factory=dict)

def _step_vector(n: int,steps: Sequence[ExposureStep]) -> np.ndarray:
    vector=np.zeros(n,dtype=np.float64)
    for s in steps:
        if type(s.start_frame) is not int or not 0<=s.start_frame<n or not math.isfinite(s.delta_ev):
            raise ExposureError(f"invalid confirmed step {s}")
        vector[s.start_frame]+=s.delta_ev
    return np.cumsum(vector)

def detect_scene_cuts(step_adjusted: np.ndarray,valid: np.ndarray,
    *,median_ev: float | None=None,tile_mad_ev: float | None=None
    ) -> tuple[list[int],list[dict]]:
    """Find content jumps between consecutive frames of step-adjusted tiles."""
    a=np.asarray(step_adjusted,dtype=np.float64)
    q=np.asarray(valid,dtype=bool)
    if median_ev is None: median_ev=SCENE_CUT_MEDIAN_EV
    if tile_mad_ev is None: tile_mad_ev=SCENE_CUT_TILE_MAD_EV
    n,k=a.shape
    need=max(8,math.ceil(k/4))
    cuts: list[int]=[]
    records: list[dict]=[]
    for t in range(n-1):
        ok=q[t]&q[t+1]
        common=int(np.count_nonzero(ok))
        if common<need:
            continue
        d=a[t+1,ok]-a[t,ok]
        med=float(np.median(d))
        mad=float(1.4826*np.median(np.abs(d-med)))
        if abs(med)>=median_ev and mad>=tile_mad_ev:
            cuts.append(t+1)
            records.append({"index":t+1,"median_ev":med,"tile_mad_ev":mad,
                "common_tiles":common})
    return cuts,records



def _pairwise_path(measurements: Sequence[FrameMeasurement],x: np.ndarray,
    q: np.ndarray,s: np.ndarray,centers: np.ndarray,period: int,strength: float,
    brightness: str,bounds: list[int],ev_limits: tuple[float,float] | None,
    consensus: np.ndarray,residual: np.ndarray,mad: np.ndarray,
    fractions: np.ndarray,counts: np.ndarray,reliable: np.ndarray,
    interpolated: np.ndarray,labels: list[str],segment_ids: np.ndarray,
    exclude_rois: tuple[tuple[float,float,float,float],...],
    measured_level: np.ndarray|None=None,
    desired_level: np.ndarray|None=None
    ) -> tuple[np.ndarray,dict]:
    """Matched-background estimation: pair graph -> relative brightness b,
    then a brightness-intent target on b (slow trend or constant level on the
    persistent background). Fills the shared diagnostic arrays in place."""
    n,k=x.shape
    xs=np.unique(centers[:,0]); ys=np.unique(centers[:,1])
    grid=(len(xs),len(ys))
    train=np.ones(k,dtype=bool)
    for (bx,by,bx1,by1) in exclude_rois:
        inside=((centers[:,0]>=bx)&(centers[:,0]<bx1)&
                (centers[:,1]>=by)&(centers[:,1]<by1))
        train&=~inside
    boundaries=[lo for lo,hi in zip(bounds[:-1],bounds[1:])]
    edges,edge_stats=build_edges(measurements,boundaries=boundaries,train=train,
        grid=grid)
    graph=solve_exposure_graph(n,edges)
    b=graph.brightness
    persistent=persistent_background(measurements,b)
    need=max(8,math.ceil(k/4))
    def _row_level_all(t:int) -> float:
        # Whole-frame typical tile: what the eye integrates. Used ONLY for
        # cross-boundary assumptions (component stitching, lone-frame
        # absolute leveling); flicker removal itself always rests on matched
        # persistent pairs.
        ok=q[t]&np.isfinite(x[t])
        return float(np.median(x[t,ok])) if np.count_nonzero(ok) else math.nan
    # Uniform wants ONE constant level for the whole sequence. Components
    # separated by cuts/gaps have no cross evidence, so their relative level
    # is stitched with an explicit assumption: the WHOLE-FRAME typical tile
    # mix (all-valid-tile median) continues across the boundary. This is the
    # owner's whole-sequence intent -- perceived overall brightness stays
    # continuous even when tracked surfaces and the rest of the scene
    # diverge (e.g. selective lighting change across a capture gap).
    b_hat=b.copy()
    cross_anchors=[]
    if len(graph.components)>1:
        # Boundary-local whole-frame continuity: shift each later component
        # so the WHOLE-FRAME typical level of its first frames continues from
        # the previous component's last frames (median of up to 3 frames per
        # side, robust to one odd frame). Aligning component MEDIANS is not
        # enough: the tile-mix offset (row_level_all - b) drifts within real
        # components, so a median-aligned stitch can still leave a large
        # perceived step at the boundary itself.
        def _edge_const(comp,at_start:bool) -> float:
            member=sorted(t for t in comp if np.isfinite(b[t]))
            if not member:
                return math.nan
            sel=member[:3] if at_start else member[-3:]
            vals=[_row_level_all(t)-b[t] for t in sel]
            vals=[v for v in vals if math.isfinite(v)]
            return float(np.median(vals)) if vals else math.nan
        prev=None
        for comp in graph.components:
            start_c=_edge_const(comp,True)
            if prev is not None and math.isfinite(prev) and math.isfinite(start_c):
                shift=start_c-prev
                b_hat[comp]+=shift
                cross_anchors.append({
                    "boundary":min(comp),"local_const_before":prev,
                    "local_const_after":start_c,"applied_shift_ev":-shift})
            elif prev is not None and math.isfinite(prev):
                # An unverifiable island (NaN-brightness component) INTERRUPTS
                # the chain: both of its boundaries carry real content change,
                # so forcing continuity across both ends is over-constrained.
                # The next component keeps its graph-gauge placement instead
                # of a fabricated anchor.
                cross_anchors.append({"boundary":min(comp),
                    "note":"unverifiable island interrupts level chaining; "
                    "the following component keeps its graph-gauge placement"})
            prev=_edge_const(comp,False)
    need=max(8,math.ceil(k/4))
    for seg_id,(lo,hi) in enumerate(zip(bounds[:-1],bounds[1:])):
        segment_ids[lo:hi]=seg_id
    for t in range(n):
        counts[t]=int(np.count_nonzero(q[t]&persistent))
    requested=np.full(n,np.nan,dtype=np.float64)
    if brightness=="uniform":
        # Constant brightness on the matched background: the level is the
        # (globally stitched) graph brightness. The target is a property of
        # matched surfaces, never of whatever object happens to sit at the
        # current frame's median.
        need=max(8,math.ceil(k/4))
        finite_all=np.flatnonzero(np.isfinite(b_hat))
        target=(float(np.median(b_hat[finite_all]))
                if finite_all.size else math.nan)
        for t in finite_all:
            reliable[t]=True
            consensus[t,q[t]&persistent]=True
            residual[t]=b_hat[t]-target
            requested[t]=strength*(target-b_hat[t])
            labels[t]="UNIFORM"
            if measured_level is not None: measured_level[t]=b_hat[t]
            if desired_level is not None: desired_level[t]=target
        # Lone nodes carry no pair evidence, but the uniform target is a
        # property of matched surfaces, not of pairs: a measurable lone frame
        # is still leveled ABSOLUTELY against the whole-frame reference
        # (row_level_all - global level-const, the same basis as component
        # stitching). Lower confidence, explicit status; the render-domain
        # check still verifies it against the same target.
        if math.isfinite(target):
            row_consts=np.array([_row_level_all(t)-b_hat[t] for t in finite_all])
            row_consts=row_consts[np.isfinite(row_consts)]
            lone=[t for comp in graph.components if len(comp)==1
                for t in comp if not np.isfinite(b_hat[t]) and np.any(q[t])]
            if row_consts.size and lone:
                global_const=float(np.median(row_consts))
                for t in lone:
                    level=_row_level_all(t)-global_const
                    if not math.isfinite(level):
                        continue
                    requested[t]=strength*(target-level)
                    interpolated[t]=True  # unverified estimate flag machinery
                    labels[t]="ABSOLUTE_LEVEL"
                    consensus[t,q[t]]=True
                    if measured_level is not None: measured_level[t]=level
                    if desired_level is not None: desired_level[t]=target
        good=np.flatnonzero(np.isfinite(requested))
        for a_idx,b_idx in zip(good[:-1],good[1:]):
            missing=b_idx-a_idx-1
            if (1<=missing<=3 and counts[a_idx+1:b_idx].min()>=0 and
                    all(int(np.sum(q[t]))>=need for t in range(a_idx+1,b_idx)) and
                    not any(g in boundaries for g in range(a_idx+1,b_idx))):
                # only measurable-but-unconstrained frames get interpolated;
                # a frame with no usable photometry stays REVIEW_REQUIRED
                for t in range(a_idx+1,b_idx):
                    if not np.any(q[t]):
                        continue
                    requested[t]=float(np.interp(t,[a_idx,b_idx],
                        [requested[a_idx],requested[b_idx]]))
                    interpolated[t]=True
                    labels[t]="INTERPOLATED_ESTIMATE"
    else:
        # smooth: keep the slow trend of b, remove the fast part. Confirmed
        # user steps are subtracted from b before fitting so the trend follows
        # them without trying to "correct" an intentional exposure change.
        b_adj=b-s
        trend=np.full(n,np.nan,dtype=np.float64)
        for comp in graph.components:
            if len(comp)<8:
                continue
            member=np.array(comp)
            fit=huber_whittaker(b_adj[member],np.isfinite(b_adj[member]),period)
            if fit.converged:
                trend[member]=fit.trend
        for t in range(n):
            if np.isfinite(b_adj[t]) and np.isfinite(trend[t]):
                reliable[t]=True
                consensus[t,q[t]&persistent]=True
                residual[t]=b_adj[t]-trend[t]
                requested[t]=strength*(trend[t]-b_adj[t])
                labels[t]="OK"
                if measured_level is not None: measured_level[t]=b_adj[t]
                if desired_level is not None: desired_level[t]=trend[t]
        # anchor each connected component so corrections are level-neutral
        for comp in graph.components:
            member=np.array([t for t in comp if np.isfinite(requested[t])])
            if member.size:
                anchor=-float(np.median(requested[member]))
                requested[member]+=anchor
        good=np.flatnonzero(np.isfinite(requested))
        for a_idx,b_idx in zip(good[:-1],good[1:]):
            missing=b_idx-a_idx-1
            # A run shorter than the 8-frame trend threshold has no
            # self-evidence (a content island): continuity interpolation is
            # the documented assumption, flagged INTERPOLATED_NOT_VERIFIED.
            if (1<=missing<=7 and
                    all(int(np.sum(q[t]))>=need for t in range(a_idx+1,b_idx)) and
                    not any(g in boundaries for g in range(a_idx+1,b_idx))):
                # only measurable-but-unconstrained frames get interpolated;
                # a frame with no usable photometry stays REVIEW_REQUIRED
                for t in range(a_idx+1,b_idx):
                    if not np.any(q[t]):
                        continue
                    requested[t]=float(np.interp(t,[a_idx,b_idx],
                        [requested[a_idx],requested[b_idx]]))
                    interpolated[t]=True
                    labels[t]="INTERPOLATED_ESTIMATE"
    summary={"target_provenance":{
        "intent":brightness,
        "reference_scope":"global" if brightness=="uniform" else "trend",
        "defined_before_iteration":True,
        "measured":"graph brightness b on persistent matched background"
        if brightness=="uniform" else "b minus confirmed steps",
        "desired":"constant level (global median of the stitched graph brightness)"
        if brightness=="uniform" else
        f"Huber-Whittaker trend, period={period} "
        f"(lambda=1/(16 sin(pi/P)^4))"},
        "cross_boundary_anchors":{"method":
        "whole-frame all-valid-tile median level const across the boundary "
        "(owner whole-sequence intent: perceived overall brightness stays "
        "continuous); explicit assumption, not evidence",
        "entries":cross_anchors},
        "edges":len(edges),
        "edges_by_lag":edge_stats["by_lag"],
        "edge_reject_reasons":edge_stats["reject_reasons"],
        "edge_p95_abs_residual_ev":graph.edge_p95_abs_residual_ev,
        "components":[{"frames":len(c),"gauge":c[0]} for c in graph.components],
        "persistent_tiles":int(np.count_nonzero(persistent)),
        "excluded_tiles":int(np.count_nonzero(~train)),
        "converged":graph.converged,"iterations":graph.iterations}
    if brightness=="uniform":
        summary["uniform_target_level_ev"]=(None if not math.isfinite(target) else target)
    return requested,summary


def _robust_segment(a: np.ndarray,q: np.ndarray,centers: np.ndarray,period: int,
    lo: int,hi: int,consensus: np.ndarray,residual: np.ndarray,mad: np.ndarray,
    fractions: np.ndarray,counts: np.ndarray,reliable: np.ndarray,
    interpolated: np.ndarray,labels: list[str]) -> int:
    """Fill consensus/reliability state for frames [lo,hi) of one segment."""
    k=a.shape[1]
    failed_tiles=0
    seg=a[lo:hi]
    qseg=q[lo:hi]
    trend=np.full_like(seg,np.nan)
    for col in range(k):
        fit=huber_whittaker(seg[:,col],qseg[:,col],period)
        if fit.converged:
            trend[:,col]=fit.trend
        elif np.count_nonzero(qseg[:,col])>=8:
            failed_tiles+=1
    r=seg-trend
    for j in range(hi-lo):
        t=lo+j
        ok=q[t]&np.isfinite(r[j])
        counts[t]=int(np.count_nonzero(ok))
        if counts[t]<max(8,math.ceil(k/4)):
            continue
        med=float(np.median(r[j,ok]))
        dev=np.abs(r[j,ok]-med)
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
            consensus[t,np.flatnonzero(ok)[agree]]=True
            labels[t]="OK"
        else:
            residual[t]=np.nan
    # Only small internal gaps inside this segment; never span a scene cut,
    # never extrapolate an edge gap.
    good=np.flatnonzero(reliable[lo:hi])+lo
    for a_idx,b_idx in zip(good[:-1],good[1:]):
        missing=b_idx-a_idx-1
        if 1<=missing<=3:
            for t in range(a_idx+1,b_idx):
                residual[t]=float(np.interp(t,[a_idx,b_idx],
                    [residual[a_idx],residual[b_idx]]))
                interpolated[t]=True
                labels[t]="INTERPOLATED_ESTIMATE"
    return failed_tiles

def estimate_exposure(measurements: Sequence[FrameMeasurement],*,period: int=60,
    strength: float=1.,steps: Sequence[ExposureStep]=(),
    brightness: str="smooth",ev_limits: tuple[float,float] | None=None,
    time_gaps: Sequence[int]=(),estimator: str="pairwise",
    exclude_rois: tuple[tuple[float,float,float,float],...]=(),
) -> ExposureResult:
    n=len(measurements)
    if n<1:
        raise ExposureError("empty exposure sequence")
    if period<6 or not math.isfinite(strength) or not 0<=strength<=1:
        raise ExposureError("invalid period or strength")
    if brightness not in ("smooth","uniform"):
        raise ExposureError("brightness must be smooth or uniform")
    if estimator not in ("pairwise","legacy"):
        raise ExposureError("estimator must be pairwise or legacy")
    gaps=sorted({int(g) for g in time_gaps if 0<int(g)<n})
    if ev_limits is None:
        # Unbounded: any finite request is applied as-is; the renderer realises
        # any finite EV, so there is no hidden second clamp downstream.
        low,high=-math.inf,math.inf
    else:
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
    if not bool(np.isfinite(x[q]).all()):
        raise ExposureError("nonfinite valid photometry")
    s=_step_vector(n,steps)
    consensus=np.zeros((n,k),dtype=bool)
    residual=np.full(n,np.nan,dtype=np.float64)
    mad=np.full(n,np.nan,dtype=np.float64)
    fractions=np.full(n,np.nan,dtype=np.float64)
    counts=np.zeros(n,dtype=int)
    reliable=np.zeros(n,dtype=bool)
    interpolated=np.zeros(n,dtype=bool)
    labels: list[str]=["REVIEW_REQUIRED"]*n
    segment_ids=np.zeros(n,dtype=int)
    scene_cuts: list[dict]=[]
    pairwise_summary: dict={}
    failed_tiles=0
    pairwise_requested=None
    measured_level=np.full(n,np.nan,dtype=np.float64)
    desired_level=np.full(n,np.nan,dtype=np.float64)
    if strength==0 or (brightness=="smooth" and n<8):
        # Whole-sequence short/disabled path: zero residual, confirmed steps only.
        residual[:]=0
        reliable[:]=True
        if n<8 and strength!=0:
            labels=["SHORT_SEQUENCE_NO_DEFLICKER"]*n
        else:
            labels=["DISABLED"]*n
        segments=[(0,n,False)]
    else:
        a=x-s[:,None]
        # One sequence, always: the input is deflickered as a WHOLE in filename
        # order. Discontinuities (scene cuts, capture-time gaps, filename
        # jumps) are reported metadata, never estimation boundaries -- there
        # is no segmentation concept. Pair-quality gates reject non-matching
        # pairs; graph components are stitched so perceived whole-frame
        # brightness continues, with the assumption recorded per boundary.
        _cuts,scene_cuts=detect_scene_cuts(a,q)
        bounds=[0,n]
        if estimator=="pairwise":
            pairwise_requested,pairwise_summary=_pairwise_path(measurements,x,q,s,
                centers,period,strength,brightness,bounds,ev_limits,consensus,
                residual,mad,fractions,counts,reliable,interpolated,labels,
                segment_ids,exclude_rois,measured_level,
                desired_level)
            segments=[(lo,hi,hi-lo>=8) for lo,hi in zip(bounds[:-1],bounds[1:])]
        elif brightness=="uniform":
            # Hold one constant brightness: correct every frame's common level
            # to the global median. Real evolution (day->dusk), flicker and
            # splice jumps are all flattened; scene cuts and time gaps stay
            # report metadata (the global anchor policy is explicit).
            need=max(8,math.ceil(k/4))
            for seg_id,(lo,hi) in enumerate(zip(bounds[:-1],bounds[1:])):
                segment_ids[lo:hi]=seg_id
            for t in range(n):
                ok=q[t]&np.isfinite(a[t])
                counts[t]=int(np.count_nonzero(ok))
                if counts[t]>=need:
                    residual[t]=float(np.median(a[t,ok]))
                    reliable[t]=True
                    consensus[t,ok]=True
                    labels[t]="UNIFORM"
            segments=[(0,n,False)]
        else:
            segments=[]
            for seg_id,(lo,hi) in enumerate(zip(bounds[:-1],bounds[1:])):
                segment_ids[lo:hi]=seg_id
                if hi-lo>=8:
                    failed_tiles+=_robust_segment(a,q,centers,period,lo,hi,consensus,
                        residual,mad,fractions,counts,reliable,interpolated,labels)
                    segments.append((lo,hi,True))
                else:
                    labels[lo:hi]=["SHORT_SEQUENCE_NO_DEFLICKER"]*(hi-lo)
                    reliable[lo:hi]=True
                    residual[lo:hi]=0
                    segments.append((lo,hi,False))
    accepted=reliable|interpolated
    if pairwise_requested is not None:
        requested=pairwise_requested
    else:
        raw=np.full(n,np.nan,dtype=np.float64)
        if strength==0:
            raw[:]=0.
        else:
            raw[accepted]=strength*(-s[accepted]-residual[accepted])
        requested=np.full(n,np.nan,dtype=np.float64)
        # Each segment anchors to its own reliable frames: exposure continuity is
        # only claimed inside a content-continuous run, never across a scene cut.
        for lo,hi,deflickered in segments:
            if np.any(reliable[lo:hi]):
                anchor=-float(np.median(raw[lo:hi][reliable[lo:hi]]))
            else:
                anchor=0.
            requested[lo:hi]=raw[lo:hi]+anchor
    frozen=np.full_like(x,np.nan)
    frames=[]
    for t in range(n):
        val=float(requested[t]) if np.isfinite(requested[t]) else None
        applied=float(np.clip(val,low,high)) if val is not None else None
        status=labels[t]
        warnings=[]
        seg_len=next(hi-lo for lo,hi,_ in segments if lo<=t<hi)
        whole_short=n<8
        if (brightness=="smooth" and strength!=0 and seg_len<2*period and
                (whole_short or labels[t]!="SHORT_SEQUENCE_NO_DEFLICKER")):
            warnings.append("SHORT_CONTEXT")
        if failed_tiles:
            warnings.append(f"TREND_NONCONVERGED_TILES={failed_tiles}")
        if applied is not None and val!=applied:
            status="LIMITED"
            warnings.append("EXPOSURE_LIMITED")
        elif not accepted[t]:
            status="REVIEW_REQUIRED"
        if interpolated[t]:
            warnings.append("LONE_FRAME_ABSOLUTE_LEVEL" if labels[t]=="ABSOLUTE_LEVEL"
                else "INTERPOLATED_NOT_VERIFIED")
        if applied is not None:
            frozen[t,q[t]]=x[t,q[t]]+val
        frames.append(FrameEstimate(t,status,
            float(residual[t]) if np.isfinite(residual[t]) else None,val,applied,
            float(s[t]),int(counts[t]) if n>=8 and strength!=0 else int(np.sum(q[t])),
            float(fractions[t]) if np.isfinite(fractions[t]) else None,
            float(mad[t]) if np.isfinite(mad[t]) else None,warnings,
            measured_level_ev=(float(measured_level[t])
                if np.isfinite(measured_level[t]) else None),
            desired_level_ev=(float(desired_level[t])
                if np.isfinite(desired_level[t]) else None),
            segment_id=int(segment_ids[t])))
    return ExposureResult(frames,x,q,consensus,frozen,reliable,failed_tiles,
        min(period,n),scene_cuts,time_gaps=gaps,pairwise_summary=pairwise_summary)
