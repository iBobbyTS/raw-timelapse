"""DNG rendering semantics: metadata, backend capability probes, continuity.

Three layers must stay separable: raw sensor values, DNG default-development
metadata (BaselineExposure, opcode lists) and the project's own corrections.
This module reads the second layer with ExifTool (optional), verifies with
byte-preserving controlled probes what the installed backend actually
executes, and derives capture-time continuity. Originals are never modified;
probes run on temporary copies.
"""
from __future__ import annotations
from dataclasses import dataclass, field
import hashlib
import json
import math
import re
import shutil
import struct
import subprocess
from datetime import datetime
from pathlib import Path
from typing import Sequence

TAG_BASELINE_EXPOSURE = 50730
TAG_OPCODE_LIST1 = 51008
TAG_OPCODE_LIST2 = 51009
TAG_OPCODE_LIST3 = 51022
_SUB_IFD_TAGS = (330, 34665)
_TYPE_SIZES = {1: 1, 2: 1, 6: 1, 7: 1, 3: 2, 8: 2, 4: 4, 9: 4,
               5: 8, 10: 8, 11: 8, 12: 8}
OPCODE_NAMES = {1: "WarpRectilinear", 2: "WarpFisheye", 3: "FixVignetteRadial",
                4: "FixBadPixelsConstant", 5: "FixBadPixelsList",
                6: "TrimBounds", 7: "MapTable", 8: "MapPolynomial",
                9: "GainMap", 10: "DeltaPerRow", 11: "DeltaPerColumn",
                12: "ScalePerRow", 13: "ScalePerColumn"}


def split_opcode_digests(payload: bytes) -> dict[str, str]:
    """Per-opcode digests inside one opcode list payload (big-endian DNG)."""
    import struct as _struct
    out: dict[str, str] = {}
    if len(payload) < 4:
        return out
    count = _struct.unpack_from(">I", payload, 0)[0]
    off = 4
    for _ in range(count):
        if off + 16 > len(payload):
            break
        oid, _version, _flags, size = _struct.unpack_from(">4I", payload, off)
        off += 16
        body = payload[off:off + size]
        off += size
        name = OPCODE_NAMES.get(oid, f"Opcode{oid}")
        out[f"{oid}_{name}"] = hashlib.sha256(body).hexdigest()
    return out


@dataclass(frozen=True)
class FrameSourceInfo:
    filename: str
    capture_time: str | None = None
    exposure_time_s: float | None = None
    iso: float | None = None
    fnumber: float | None = None
    baseline_exposure_ev: float | None = None
    baseline_exposure_offset_ev: float | None = None
    black_levels: tuple[float, ...] = ()
    white_level: float | None = None
    opcode_digests: dict[str, str] = field(default_factory=dict)
    opcode_lengths: dict[str, int] = field(default_factory=dict)
    opcode_entry_digests: dict[str, dict[str, str]] = field(default_factory=dict)

    @property
    def camera_config_id(self) -> str:
        """Stable digest of the development-affecting metadata subset."""
        payload = json.dumps({
            "baseline": self.baseline_exposure_ev,
            "opcodes": dict(sorted(self.opcode_digests.items())),
            "black": list(self.black_levels), "white": self.white_level,
        }, sort_keys=True)
        return hashlib.sha256(payload.encode()).hexdigest()[:16]


def _iter_ifds(data: bytes, bo: str) -> list[int]:
    seen: set[int] = set()
    queue = [struct.unpack_from(bo + "I", data, 4)[0]]
    order: list[int] = []
    while queue:
        off = queue.pop(0)
        if not off or off in seen or off + 2 > len(data):
            continue
        seen.add(off)
        count = struct.unpack_from(bo + "H", data, off)[0]
        order.append(off)
        for i in range(count):
            entry = off + 2 + i * 12
            if entry + 12 > len(data):
                continue
            tag, typ, num = struct.unpack_from(bo + "HHI", data, entry)
            size = _TYPE_SIZES.get(typ, 4)
            if tag == 330 and typ == 4:
                # SubIFD offsets: inline when count==1, else at an offset
                if num == 1:
                    queue.append(struct.unpack_from(bo + "I", data, entry + 8)[0])
                elif num * size > 4:
                    value_off = struct.unpack_from(bo + "I", data, entry + 8)[0]
                    if value_off + 4 * num <= len(data):
                        for j in range(num):
                            queue.append(struct.unpack_from(bo + "I", data, value_off + 4 * j)[0])
            elif tag == 34665 and typ == 4:
                off = (struct.unpack_from(bo + "I", data, entry + 8)[0]
                       if num * size > 4 else entry + 8)
                queue.append(struct.unpack_from(bo + "I", data, off)[0])
        if off + 2 + count * 12 + 4 <= len(data):
            nxt = struct.unpack_from(bo + "I", data, off + 2 + count * 12)[0]
            if nxt:
                queue.append(nxt)
    return order


def _tiff_bo(data: bytes) -> str | None:
    if data[:2] == b"MM":
        return ">"
    if data[:2] == b"II":
        return "<"
    return None


def _find_entry(data: bytes, bo: str, tag: int) -> tuple[int, int, int] | None:
    for ifd in _iter_ifds(data, bo):
        count = struct.unpack_from(bo + "H", data, ifd)[0]
        for i in range(count):
            entry = ifd + 2 + i * 12
            if entry + 12 > len(data):
                continue
            t, typ, num = struct.unpack_from(bo + "HHI", data, entry)
            if t == tag:
                return entry, typ, num
    return None


def read_baseline_exposure(data: bytes) -> float | None:
    """Read BaselineExposure (SRATIONAL or RATIONAL) from raw DNG bytes."""
    bo = _tiff_bo(data)
    if bo is None:
        return None
    found = _find_entry(data, bo, TAG_BASELINE_EXPOSURE)
    if found is None:
        return None
    entry, typ, num = found
    if num != 1:
        return None
    if typ not in (5, 10):
        return None
    off = struct.unpack_from(bo + "I", data, entry + 8)[0]
    if off + 8 > len(data):
        return None
    if typ == 10:
        n, d = struct.unpack_from(bo + "ii", data, off)
    else:
        n, d = struct.unpack_from(bo + "II", data, off)
    return n / d if d else None


def set_baseline_exposure(data: bytes, value: float) -> bytes | None:
    """Return patched bytes with BaselineExposure set to value (same length)."""
    bo = _tiff_bo(data)
    if bo is None:
        return None
    found = _find_entry(data, bo, TAG_BASELINE_EXPOSURE)
    if found is None:
        return None
    entry, typ, num = found
    if typ != 10 or num != 1 or not math.isfinite(value):
        return None
    out = bytearray(data)
    off = struct.unpack_from(bo + "I", out, entry + 8)[0]
    _, d = struct.unpack_from(bo + "ii", out, off)
    if not d:
        return None
    struct.pack_into(bo + "i", out, off, int(round(value * d)))
    return bytes(out)


def neutralize_opcode_list3(data: bytes) -> bytes | None:
    """Rename OpcodeList3 to a private tag id; payload stays at its offset."""
    bo = _tiff_bo(data)
    if bo is None:
        return None
    out = bytearray(data)
    patched = 0
    for ifd in _iter_ifds(out, bo):
        count = struct.unpack_from(bo + "H", out, ifd)[0]
        for i in range(count):
            entry = ifd + 2 + i * 12
            if entry + 12 > len(out):
                continue
            tag = struct.unpack_from(bo + "H", out, entry)[0]
            if tag == TAG_OPCODE_LIST3:
                struct.pack_into(bo + "H", out, entry, 60000)
                patched += 1
    return bytes(out) if patched else None


def probe_backend_semantics(renderer, path: Path, options,
                            *, linear_tolerance_ev: float = .05,
                            opcode_tolerance_ev: float = 1e-4) -> dict:
    """Controlled probes on temporary copies: is DNG default-development
    metadata (BaselineExposure, OpcodeList3) actually executed by the backend?

    A +1 EV BaselineExposure patch and an OpcodeList3 tag neutralisation are
    applied to copies; renders are compared in linear light. Bit-preserving
    patches only -- the original file is never touched.
    """
    import numpy as np
    import tempfile
    result: dict = {"probe": "patch-and-render", "file": Path(path).name}
    data = Path(path).read_bytes()
    with tempfile.TemporaryDirectory(prefix="rawtl-dngprobe-") as tmp:
        tmp_path = Path(tmp)
        baseline = read_baseline_exposure(data)
        if baseline is None:
            result["baseline_exposure"] = {"tag": "absent"}
        else:
            patched = set_baseline_exposure(data, baseline + 1.0)
            if patched is None:
                result["baseline_exposure"] = {"tag": "present", "probe": "unsupported-layout"}
            else:
                a = tmp_path / "a.dng"; a.write_bytes(data)
                b = tmp_path / "b.dng"; b.write_bytes(patched)
                try:
                    ra = renderer.render_linear(a, 0., options)
                    rb = renderer.render_linear(b, 0., options)
                    med_a = float(np.median(ra.rgb))
                    med_b = float(np.median(rb.rgb))
                    delta = float(np.log2(med_b / med_a)) if med_a > 0 else None
                    result["baseline_exposure"] = {
                        "tag": "present", "tag_value_ev": baseline,
                        "injected_ev": 1.0, "measured_delta_ev": delta,
                        "applied": bool(delta is not None and abs(delta - 1.0) <= linear_tolerance_ev)}
                except Exception as exc:  # renderer failure is a probe result, not a crash
                    result["baseline_exposure"] = {"tag": "present", "probe": "render_failed",
                                                   "error": str(exc)[:200]}
        neutral = neutralize_opcode_list3(data)
        if neutral is None:
            result["opcode_list3"] = {"tag": "absent"}
        else:
            a = tmp_path / "c.dng"; a.write_bytes(data)
            b = tmp_path / "d.dng"; b.write_bytes(neutral)
            try:
                ra = renderer.render_linear(a, 0., options)
                rb = renderer.render_linear(b, 0., options)
                ratio = np.log2(np.maximum(rb.rgb, 1e-9) / np.maximum(ra.rgb, 1e-9))
                finite = ratio[np.isfinite(ratio)]
                worst = float(np.max(np.abs(finite))) if finite.size else None
                result["opcode_list3"] = {
                    "tag": "present", "max_pixel_delta_ev": worst,
                    "applied": bool(worst is not None and worst > opcode_tolerance_ev)}
            except Exception as exc:
                result["opcode_list3"] = {"tag": "present", "probe": "render_failed",
                                          "error": str(exc)[:200]}
    return result


def _float(value) -> float | None:
    try:
        parsed = float(value)
        return parsed if math.isfinite(parsed) else None
    except (TypeError, ValueError):
        return None


def parse_source_metadata(rows: Sequence[dict]) -> dict[str, FrameSourceInfo]:
    """Parse one ExifTool -j batch result into per-source-file records."""
    out: dict[str, FrameSourceInfo] = {}
    for row in rows:
        name = Path(row.get("SourceFile", "")).name
        capture = None
        stamp = row.get("DateTimeOriginal")
        if stamp:
            try:
                capture = datetime.strptime(stamp, "%Y:%m:%d %H:%M:%S").isoformat()
            except ValueError:
                capture = None
            if capture and row.get("SubSecTimeOriginal"):
                capture += f".{row['SubSecTimeOriginal']}"
        black: tuple[float, ...] = ()
        raw_black = str(row.get("BlackLevel", "") or "").split()
        if raw_black:
            parsed = [_float(x) for x in raw_black]
            black = tuple(x for x in parsed if x is not None)
        op_digests: dict[str, str] = {}
        op_lengths: dict[str, int] = {}
        for key, tag in (("OpcodeList1", TAG_OPCODE_LIST1),
                         ("OpcodeList2", TAG_OPCODE_LIST2),
                         ("OpcodeList3", TAG_OPCODE_LIST3)):
            text = row.get(key)
            if text is not None:
                encoded = str(text).encode("utf-8", "surrogateescape")
                op_digests[f"{key}(0x{tag:x})"] = hashlib.sha256(encoded).hexdigest()
                op_lengths[f"{key}(0x{tag:x})"] = len(encoded)
        out[name] = FrameSourceInfo(
            filename=name, capture_time=capture,
            exposure_time_s=_float(row.get("ExposureTime")),
            iso=_float(row.get("ISO")), fnumber=_float(row.get("FNumber")),
            baseline_exposure_ev=_float(row.get("BaselineExposure")),
            baseline_exposure_offset_ev=_float(row.get("BaselineExposureOffset")),
            black_levels=black, white_level=_float(row.get("WhiteLevel")),
            opcode_digests=op_digests, opcode_lengths=op_lengths)
    return out


EXIFTOOL_FIELDS = ("-DateTimeOriginal", "-SubSecTimeOriginal", "-ExposureTime",
                   "-ISO", "-FNumber", "-BaselineExposure", "-BaselineExposureOffset",
                   "-BlackLevel", "-WhiteLevel", "-OpcodeList1", "-OpcodeList2",
                   "-OpcodeList3")


def _opcode_binary_digests(exe: str, paths: Sequence[Path],
                           info: dict[str, FrameSourceInfo]) -> None:
    """Replace name-only opcode digests with raw-payload digests.

    ExifTool's text rendering of opcode lists shows names, not payload data;
    distinctness requires hashing the binary tag. Payloads land in a temp dir
    and are hashed there; nothing is written next to the sources.
    """
    import tempfile
    lists = [name for name in ("OpcodeList1", "OpcodeList2", "OpcodeList3")
             if any(key.startswith(name + "(")
                    for f in info.values() for key in f.opcode_digests)]
    if not lists:
        return
    with tempfile.TemporaryDirectory(prefix="rawtl-opcodes-") as tmp:
        tmp_path = Path(tmp)
        for start in range(0, len(paths), 48):
            chunk = paths[start:start + 48]
            for name in lists:
                target = tmp_path / name
                target.mkdir(exist_ok=True)
                try:
                    subprocess.run(
                        [exe, f"-{name}", "-b", "-w", str(target / "%f.op"),
                         *[str(p) for p in chunk]],
                        capture_output=True, timeout=180, check=True)
                except (OSError, subprocess.SubprocessError):
                    continue
            for record in info.values():
                source = tmp_path / name / f"{Path(record.filename).stem}.op"
                try:
                    payload = source.read_bytes()
                except OSError:
                    continue
                key = next((k for k in record.opcode_digests
                            if k.startswith(name + "(")), None)
                if key is None:
                    continue
                record.opcode_digests[key] = hashlib.sha256(payload).hexdigest()
                record.opcode_lengths[key] = len(payload)
                record.opcode_entry_digests[key] = split_opcode_digests(payload)


def extract_source_metadata(paths: Sequence[Path],
                            executable: str | None = None) -> dict[str, FrameSourceInfo] | None:
    """Batch ExifTool extraction; returns None when ExifTool is unavailable.

    Only development-affecting fields are read; GPS and serial numbers are
    never requested.
    """
    exe = executable or shutil.which("exiftool")
    if not exe:
        return None
    info: dict[str, FrameSourceInfo] = {}
    for start in range(0, len(paths), 48):
        chunk = paths[start:start + 48]
        try:
            proc = subprocess.run(
                [exe, "-j", "-n", *EXIFTOOL_FIELDS, *[str(p) for p in chunk]],
                capture_output=True, text=True, timeout=180, check=True)
            info.update(parse_source_metadata(json.loads(proc.stdout)))
        except (OSError, ValueError, subprocess.SubprocessError):
            continue
    if info:
        _opcode_binary_digests(exe, list(paths), info)
    return info or None


_NUMBER_TAIL = re.compile(r"(\d+)\D*$")


def _filename_number(name: str) -> int | None:
    match = _NUMBER_TAIL.search(Path(name).stem)
    return int(match.group(1)) if match else None


def continuity(frames: Sequence[FrameSourceInfo], *,
               gap_factor: float = 1.5,
               min_nominal_samples: int = 3) -> dict:
    """Capture-time continuity plus filename-gap suspicion.

    Times are naive camera strings (no timezone inference). The nominal
    interval is a robust median; only intervals clearly beyond gap_factor
    times nominal are discontinuities. Variable-interval shooting needs an
    explicit timeline policy -- this detector reports, it never reorders.
    """
    times = [datetime.fromisoformat(f.capture_time) if f.capture_time else None
             for f in frames]
    deltas = [(times[i] - times[i - 1]).total_seconds()
              if times[i] and times[i - 1] else None
              for i in range(1, len(times))]
    positive = [d for d in deltas if d is not None and d > 0]
    nominal = float(sorted(positive)[len(positive) // 2]) if positive else None
    events: list[dict] = []
    for i, delta in enumerate(deltas, start=1):
        if nominal is None or len(positive) < min_nominal_samples:
            break
        if delta is None or delta <= 0 or delta > gap_factor * nominal:
            events.append({"index": i, "kind": "time_gap",
                           "gap_s": delta, "nominal_interval_s": nominal})
    for i in range(1, len(frames)):
        a, b = _filename_number(frames[i - 1].filename), _filename_number(frames[i].filename)
        if a is not None and b is not None and b - a != 1:
            events.append({"index": i, "kind": "filename_gap_suspicion",
                           "filename_step": b - a})
    segments: list[list[int]] = []
    start = 0
    cut = {e["index"] for e in events if e["kind"] == "time_gap"}
    for i in range(1, len(frames)):
        if i in cut:
            segments.append([start, i])
            start = i
    segments.append([start, len(frames)])
    return {"nominal_interval_s": nominal, "time_gaps": [e for e in events if e["kind"] == "time_gap"],
            "filename_suspicions": [e for e in events if e["kind"] == "filename_gap_suspicion"],
            "segments": segments,
            "coverage": "capture_time" if positive else "filename_only"}


def opcode_variation(info: dict[str, FrameSourceInfo]) -> dict:
    """Distinct opcode payloads per list -- and per opcode inside each list --
    across the sequence (equality only, never payload content)."""
    out: dict[str, dict] = {}
    ordered = list(info.values())
    keys = set().union(*(f.opcode_digests.keys() for f in ordered)) if ordered else set()
    for key in keys:
        digests = [f.opcode_digests.get(key) for f in ordered]
        present = [d for d in digests if d is not None]
        entry: dict = {"frames_with_tag": len(present),
                       "unique_payloads": len(set(present)),
                       "constant": len(set(present)) == 1 if present else None}
        per_opcode: dict[str, dict] = {}
        for frame in ordered:
            for oid, digest in frame.opcode_entry_digests.get(key, {}).items():
                slot = per_opcode.setdefault(oid, {"frames": 0, "unique_payloads": set()})
                slot["frames"] += 1
                slot["unique_payloads"].add(digest)
        entry["opcodes"] = {oid: {"frames": s["frames"],
                                  "unique_payloads": len(s["unique_payloads"])}
                             for oid, s in sorted(per_opcode.items())}
        out[key] = entry
    return out


def baseline_exposure_summary(info: dict[str, FrameSourceInfo]) -> dict:
    values = [f.baseline_exposure_ev for f in info.values()
              if f.baseline_exposure_ev is not None]
    if not values:
        return {"present": False}
    return {"present": True, "frames_with_tag": len(values),
            "unique_values": len(set(values)),
            "range_ev": [min(values), max(values)]}
