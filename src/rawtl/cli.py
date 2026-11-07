"""Public CLI; only argument parsing, presentation and stable exit codes."""
from __future__ import annotations

import argparse
from fractions import Fraction
import json
import math
from pathlib import Path
import sys
import traceback
from .types import ExposureStep, InputError, ProcessingRequest, RawTLError

def positive_fps(value: str) -> Fraction:
    try:
        f = Fraction(value)
        if f <= 0 or f > 1000:
            raise ValueError(value)
        return f
    except (ValueError, ZeroDivisionError) as exc:
        raise argparse.ArgumentTypeError("fps must be a positive rational <=1000") from exc

def parse_roi(value: str) -> tuple[float,float,float,float]:
    try:
        vals = tuple(float(s) for s in value.split(","))
        if len(vals) != 4 or not all(math.isfinite(x) for x in vals):
            raise ValueError(value)
        x,y,w,h = vals
        if min(x,y) < 0 or min(w,h) <= 0 or x+w > 1 or y+h > 1:
            raise ValueError(value)
        return (x,y,w,h)
    except ValueError as exc:
        raise argparse.ArgumentTypeError("ROI must be x,y,w,h within normalized image bounds") from exc

def parse_ev_limits(value: str) -> tuple[float,float] | None:
    text=value.strip().lower()
    if text in ("none","unbounded",""):
        return None
    try:
        parts=text.split(",",1)
        lo,hi=float(parts[0]),float(parts[1])
        if not (math.isfinite(lo) and math.isfinite(hi)) or not lo<hi:
            raise ValueError(value)
        return (lo,hi)
    except (ValueError, IndexError) as exc:
        raise argparse.ArgumentTypeError(
            'EV limits must be "none" or two finite floats "LOW,HIGH" with LOW<HIGH') from exc

def positive_period(value: str) -> int:
    try:
        v=int(value)
        if v < 6:
            raise ValueError(value)
        return v
    except ValueError as exc:
        raise argparse.ArgumentTypeError("period must be an integer >=6") from exc

def strength_value(value: str) -> float:
    try:
        f=float(value)
        if not math.isfinite(f) or not 0 <= f <= 1:
            raise ValueError(value)
        return f
    except ValueError as exc:
        raise argparse.ArgumentTypeError("strength must be finite and within [0,1]") from exc

def load_steps(path: Path | None) -> tuple[ExposureStep, ...]:
    if path is None:
        return ()
    try:
        payload=json.loads(path.read_text(encoding="utf-8"))
        if not isinstance(payload,list):
            raise ValueError("steps file must be an array")
        steps=[]
        for item in payload:
            if not isinstance(item,dict) or type(item.get("start_frame")) is not int:
                raise ValueError("each step requires integer start_frame")
            v=float(item["delta_ev"])
            if item["start_frame"] < 0 or not math.isfinite(v):
                raise ValueError("invalid step index or delta_ev")
            steps.append(ExposureStep(item["start_frame"],v))
        return tuple(steps)
    except (OSError, ValueError, KeyError, TypeError) as exc:
        raise InputError(f"invalid steps file {path}: {exc}") from exc

def build_parser() -> argparse.ArgumentParser:
    p=argparse.ArgumentParser(prog="rawtl",description="Offline RAW time-lapse exposure smoothing")
    sub=p.add_subparsers(dest="action",required=True)
    r=sub.add_parser("run",help="deflicker RAW frames and produce H.264 MP4")
    r.add_argument("input",type=Path,help="flat directory of RAW files")
    r.add_argument("-o","--output",type=Path,required=True)
    r.add_argument("--fps",type=positive_fps,default=Fraction(30))
    r.add_argument("--period",type=positive_period,default=60,
        help="trend period in frames for smooth mode ONLY; uniform holds a "
             "constant reference level and ignores this parameter entirely")
    r.add_argument("--strength",type=strength_value,default=1.)
    r.add_argument("--brightness",choices=["smooth","uniform"],default="smooth",
        help="smooth: remove flicker, preserve real brightness evolution; "
             "uniform: hold one constant brightness across the whole sequence")
    r.add_argument("--estimator",choices=["pairwise","legacy"],default="pairwise",
        help="pairwise: matched-background pair graph (default); "
             "legacy: per-frame-median tiles + Whittaker residual (baseline)")
    r.add_argument("--ev-limits",dest="ev_limits",type=parse_ev_limits,default=None,
        help='clamp applied EV to "LOW,HIGH" (e.g. "-2,2"); "none" (default) '
             'applies any finite correction without clamping')
    r.add_argument("--exclude-rois",type=Path,dest="exclude_rois",
        help="JSON file of ROI boxes [x0,y0,x1,y1] keyed by name; tiles under "
             "them are excluded from estimation (independent acceptance areas)")
    r.add_argument("--roi",type=parse_roi,default=(0.,0.,1.,1.))
    r.add_argument("--steps",type=Path)
    r.add_argument("--on-review",choices=["error","passthrough"],default="error")
    r.add_argument("--temporal-quality",choices=["strict","report","off"],default="strict",
        dest="temporal_quality",
        help="strict: fail the run when the encoded video shows residual flicker; "
             "report: publish the video as COMPLETED_WITH_REVIEW with events; "
             "off: skip the independent audit (status SKIPPED, never a pass)")
    r.add_argument("--save-frames",action="store_true")
    r.add_argument("--overwrite",action="store_true")
    r.add_argument("--encoder",choices=["auto","nvenc","amf","vaapi","videotoolbox","libx264"],default="auto")
    r.add_argument("--ffmpeg",default="ffmpeg")
    pr=sub.add_parser("probe",help="inspect RAW folder and actual available encoders")
    pr.add_argument("input",type=Path)
    pr.add_argument("--ffmpeg",default="ffmpeg")
    av=sub.add_parser("audit-video",
        help="independent read-only temporal-flicker audit of an encoded video")
    av.add_argument("video",type=Path)
    av.add_argument("-o","--output",type=Path,required=True,
        help="directory for temporal_audit.json / pairs.csv / frames.csv")
    av.add_argument("--transfer",choices=["srgb","bt709"],default=None,
        help="explicit inverse-transfer assumption; required when the stream "
             "has no usable SDR transfer tag (never inferred from pixel format)")
    av.add_argument("--rois",type=Path,
        help="JSON of independent ROI boxes [x0,y0,x1,y1] keyed by name")
    av.add_argument("--segments",type=Path,
        help="JSON continuity segments [[start,end),...] or {\"segments\": ...}; "
             "statistics never cross segment boundaries")
    av.add_argument("--report",type=Path,
        help="exposure_report.json providing per-frame source filenames")
    av.add_argument("--width",type=int,default=912)
    av.add_argument("--ffmpeg",default="ffmpeg")
    pd=sub.add_parser("probe-dng",
        help="controlled probes: does this backend apply BaselineExposure / OpcodeList3?")
    pd.add_argument("file",type=Path)
    return p

def main(argv: list[str] | None = None) -> int:
    args=build_parser().parse_args(argv)
    log=None
    try:
        from .service import ProcessingService
        service=ProcessingService()
        if args.action=="probe-dng":
            from .raw_backend import RawpyRenderer
            from .dng_semantics import probe_backend_semantics, read_baseline_exposure
            renderer=RawpyRenderer()
            meta=renderer.inspect(args.file)
            from .types import RawRenderOptions
            options=RawRenderOptions(RawpyRenderer.fixed_wb(meta))
            result=probe_backend_semantics(renderer,args.file,options)
            result["baseline_exposure_tag_value_ev"]=read_baseline_exposure(
                args.file.read_bytes())
            print(json.dumps(result,indent=2,ensure_ascii=False,allow_nan=False))
        elif args.action=="probe":
            result=service.probe(args.input,ffmpeg=args.ffmpeg)
            print(json.dumps(result,indent=2,ensure_ascii=False,allow_nan=False))
        elif args.action=="audit-video":
            from .video import resolve_ffprobe
            from .quality.video_audit import VideoAuditConfig,audit_video
            ffprobe=resolve_ffprobe(args.ffmpeg)
            rois=json.loads(args.rois.read_text(encoding="utf-8")) if args.rois else None
            segments_payload=(json.loads(args.segments.read_text(encoding="utf-8"))
                if args.segments else None)
            segment_list=(segments_payload.get("segments")
                if isinstance(segments_payload,dict) else segments_payload) or []
            boundaries=tuple(int(a) for a,_ in segment_list[1:])
            labels=None
            if args.report:
                frames=json.loads(args.report.read_text(encoding="utf-8"))["frames"]
                labels=[f["filename"] for f in frames]
            config=VideoAuditConfig(transfer=args.transfer,rois=rois,
                boundaries=boundaries,frame_labels=labels,max_width=args.width)
            result=audit_video(args.video,ffmpeg=args.ffmpeg,ffprobe=ffprobe,
                config=config,output_dir=args.output)
            print(json.dumps({"status":result["status"],
                "frame_count":result["frame_count"],
                "candidate_count":result["candidate_count"],
                "events":result["events"],
                "segments":[{k:s.get(k) for k in ("start","end_exclusive","status",
                    "usable_pairs","p95_abs_step_ev","max_abs_step_ev",
                    "curvature_rms_ev","steps_over_0_05_ev")}
                    for s in result["segments"]]},indent=2,ensure_ascii=False,allow_nan=False))
        else:
            exclude=()
            if args.exclude_rois:
                payload=json.loads(args.exclude_rois.read_text(encoding="utf-8"))
                exclude=tuple(tuple(float(v) for v in box) for box in payload.values())
            req=ProcessingRequest(
                input_dir=args.input,output_dir=args.output,fps=args.fps,
                period=args.period,strength=args.strength,brightness=args.brightness,
                estimator=args.estimator,
                ev_limits=args.ev_limits,
                roi=args.roi,exclude_rois=exclude,
                steps=load_steps(args.steps),on_review=args.on_review,
                temporal_quality=args.temporal_quality,save_frames=args.save_frames,
                overwrite=args.overwrite,encoder=args.encoder,ffmpeg=args.ffmpeg)
            def on_progress(ev):
                nonlocal log
                line=f"[{ev.phase}] {ev.processed}/{ev.total} {ev.filename or ''} {ev.encoder or ''}".rstrip()
                print(line,file=sys.stderr)
                if log is None:
                    path=req.output_dir/"run.log"
                    path.parent.mkdir(parents=True,exist_ok=True)
                    log=path.open("a",encoding="utf-8")
                print(line,file=log,flush=True)
            result=service.run(req,on_progress=on_progress)
            print(json.dumps({"run_status":result.run_status,
                "video":str(result.video_path),"frames":result.frame_count,
                "encoder":result.encoder,"report":str(result.report_path)},ensure_ascii=False))
            if result.run_status!="OK":
                print("Warning: output is partial; review exposure_report.json",file=sys.stderr)
        return 0
    except KeyboardInterrupt:
        print("Cancelled",file=sys.stderr)
        return 130
    except RawTLError as exc:
        print(f"rawtl: {exc}",file=sys.stderr)
        if log is not None: print(f"ERROR: {exc}",file=log,flush=True)
        return exc.exit_code
    except Exception as exc:
        print(f"rawtl: unexpected error: {exc}",file=sys.stderr)
        traceback.print_exc(file=sys.stderr)
        if log is not None: traceback.print_exc(file=log)
        return 7
    finally:
        if log is not None:log.close()

if __name__ == "__main__":
    raise SystemExit(main())
