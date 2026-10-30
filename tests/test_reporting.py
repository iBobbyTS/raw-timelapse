import csv
import json
from pathlib import Path
import numpy as np
from rawtl.reporting import strict_jsonable,write_report,write_curve

def test_strict_json_roundtrip(tmp_path):
    data={"x":float("nan"),"y":np.float64("inf"),"z":[np.int64(1),-0.2]}
    path=tmp_path/"a.json"
    write_report(path,data)
    text=path.read_text()
    assert "NaN" not in text and "Infinity" not in text
    assert json.loads(text)=={"x":None,"y":None,"z":[1,-0.2]}

def test_csv_missing_ev_is_empty(tmp_path):
    path=tmp_path/"a.csv"
    write_curve(path,[{"index":0,"filename":"a.CR3","status":"REVIEW_REQUIRED",
        "applied_ev":None,"warnings":["REVIEW_REQUIRED"]}])
    with path.open(newline="") as f:
        row=next(csv.DictReader(f))
    assert row["applied_ev"]==""
    assert row["warnings"]=="REVIEW_REQUIRED"
