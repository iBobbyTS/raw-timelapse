"""Strict atomic JSON/CSV reporting; stable for CLI and future GUI users."""
from __future__ import annotations
import csv
import json
import math
import os
from pathlib import Path
import tempfile
import numpy as np

def strict_jsonable(value):
    if isinstance(value,np.generic):
        return strict_jsonable(value.item())
    if isinstance(value,(float,)):
        return float(value) if math.isfinite(value) else None
    if isinstance(value,dict):
        return {str(k):strict_jsonable(v) for k,v in value.items()}
    if isinstance(value,(list,tuple)):
        return [strict_jsonable(v) for v in value]
    return value

def _atomic_text(path: Path,text: str):
    path.parent.mkdir(parents=True,exist_ok=True)
    fd,name=tempfile.mkstemp(prefix=".rawtl-report-",dir=path.parent)
    try:
        with os.fdopen(fd,"w",encoding="utf-8",newline="") as file:
            file.write(text)
        os.replace(name,path)
    finally:
        Path(name).unlink(missing_ok=True)

def write_report(path: Path,data: dict):
    _atomic_text(path,json.dumps(strict_jsonable(data),ensure_ascii=False,
        allow_nan=False,indent=2)+"\n")

FIELDS=("index","filename","segment_id","status","valid_tile_count",
        "consensus_fraction","residual_mad_ev","common_residual_ev",
        "cumulative_confirmed_step_ev","requested_ev","applied_ev",
        "render_ev","gain","render_target_error_ev",
        "new_output_clipping_fraction","warnings")

def write_curve(path: Path,frames: list[dict]):
    import io
    stream=io.StringIO()
    w=csv.DictWriter(stream,fieldnames=FIELDS,extrasaction="ignore")
    w.writeheader()
    for frame in frames:
        row={key:frame.get(key) for key in FIELDS}
        row["warnings"]=";".join(frame.get("warnings",[]))
        w.writerow({k:"" if v is None else v for k,v in row.items()})
    _atomic_text(path,stream.getvalue())
