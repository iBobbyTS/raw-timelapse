"""Public CLI: argument parsing only; ProcessingService owns all operations."""
from __future__ import annotations

import argparse
from fractions import Fraction
import json
import math
from pathlib import Path
import sys
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
    r.add_argument("--period",type=positive_period,default=60)
    r.add_argument("--strength",type=strength_value,default=1.)
    r.add_argument("--roi",type=parse_roi,default=(0.,0.,1.,1.))
    r.add_argument("--steps",type=Path)
    r.add_argument("--on-review",choices=["error","passthrough"],default="error")
    r.add_argument("--save-frames",action="store_true")
    r.add_argument("--overwrite",action="store_true")
    r.add_argument("--encoder",choices=["auto","nvenc","amf","vaapi","videotoolbox","libx264"],default="auto")
    r.add_argument("--ffmpeg",default="ffmpeg")
    pr=sub.add_parser("probe",help="inspect RAW folder and available encoders without exporting")
    pr.add_argument("input",type=Path)
    pr.add_argument("--ffmpeg",default="ffmpeg")
    return p

def main(argv: list[str] | None = None) -> int:
    args=build_parser().parse_args(argv)
    try:
        from .service import ProcessingService
        service=ProcessingService()
        if args.action == "probe":
            service.probe(args.input, ffmpeg=args.ffmpeg)
        else:
            req=ProcessingRequest(args.input,args.output,args.fps,args.period,args.strength,args.roi,
                load_steps(args.steps),args.on_review,args.save_frames,args.overwrite,args.encoder,args.ffmpeg)
            service.run(req, on_progress=lambda ev: print(
                f"[{ev.phase}] {ev.processed}/{ev.total} {ev.filename or ''}",file=sys.stderr))
        return 0
    except KeyboardInterrupt:
        print("Cancelled", file=sys.stderr)
        return 130
    except RawTLError as exc:
        print(f"rawtl: {exc}",file=sys.stderr)
        return exc.exit_code

if __name__ == "__main__":
    raise SystemExit(main())
