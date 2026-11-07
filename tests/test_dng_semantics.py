"""F3: DNG development semantics, controlled probes and source continuity."""
from __future__ import annotations
import struct
from datetime import datetime, timedelta
from pathlib import Path
import numpy as np
import pytest

from rawtl.dng_semantics import (
    FrameSourceInfo, baseline_exposure_summary, continuity, neutralize_opcode_list3,
    opcode_variation, parse_source_metadata, probe_backend_semantics,
    read_baseline_exposure, set_baseline_exposure)


def build_tiff(bo: str, baseline: tuple[int, int] | None = (27, 10),
               with_opcode: bool = True) -> bytes:
    """Minimal TIFF: IFD0 with BaselineExposure (SRATIONAL at offset) and a
    SubIFD holding OpcodeList3 (UNDEFINED at offset). Little/big endian."""
    e = "<" if bo == "II" else ">"
    entries_ifd0 = (1 if baseline is not None else 0) + (1 if with_opcode else 0)
    ifd0_off = 8
    ifd0_size = 2 + entries_ifd0 * 12 + 4
    value_off = ifd0_off + ifd0_size
    subifd_off = value_off + (8 if baseline is not None else 0)
    payload = b"\x01\x02\x03\x04gainmap"
    subifd_size = 2 + 12 + 4
    payload_off = subifd_off + subifd_size
    out = bytearray()
    out += (b"II" if bo == "II" else b"MM") + (b"\x2a\x00" if bo == "II" else b"\x00\x2a")
    out += struct.pack(e + "I", ifd0_off)
    assert len(out) == 8
    out += struct.pack(e + "H", entries_ifd0)
    if baseline is not None:
        out += struct.pack(e + "HHI", 50730, 10, 1) + struct.pack(e + "I", value_off)
    if with_opcode:
        out += struct.pack(e + "HHI", 330, 4, 1) + struct.pack(e + "I", subifd_off)
    out += struct.pack(e + "I", 0)
    assert len(out) == value_off, (len(out), value_off)
    if baseline is not None:
        out += struct.pack(e + "ii", *baseline)
    assert len(out) == subifd_off
    out += struct.pack(e + "H", 1)
    out += struct.pack(e + "HHI", 51022, 7, len(payload)) + struct.pack(e + "I", payload_off)
    out += struct.pack(e + "I", 0)
    assert len(out) == payload_off
    out += payload
    return bytes(out)


def test_read_baseline_exposure_both_endians():
    assert read_baseline_exposure(build_tiff("II", (27, 10))) == pytest.approx(2.7)
    assert read_baseline_exposure(build_tiff("MM", (-13, 10))) == pytest.approx(-1.3)
    assert read_baseline_exposure(build_tiff("II", baseline=None)) is None
    assert read_baseline_exposure(b"not a tiff") is None


def test_set_baseline_exposure_preserves_length_and_value():
    data = build_tiff("II", (27, 10))
    patched = set_baseline_exposure(data, 3.7)
    assert patched is not None and len(patched) == len(data)
    assert read_baseline_exposure(patched) == pytest.approx(3.7)
    # only the rational changed; the rest of the file is untouched
    diff = [i for i in range(len(data)) if data[i] != patched[i]]
    assert diff and max(diff) - min(diff) <= 7
    assert set_baseline_exposure(data, 2.0) != patched


def test_neutralize_opcode_list3_in_subifd():
    data = build_tiff("II")
    patched = neutralize_opcode_list3(data)
    assert patched is not None and len(patched) == len(data)
    from rawtl.dng_semantics import _find_entry
    bo = "<"
    assert _find_entry(patched, bo, 51022) is None
    assert _find_entry(patched, bo, 60000) is not None
    assert neutralize_opcode_list3(build_tiff("II", with_opcode=False)) is None


def _fake_renderer(path, ev, opts, *, applies_baseline):
    data = Path(path).read_bytes()
    level = .2
    if applies_baseline:
        base = read_baseline_exposure(data)
        if base is not None:
            level *= 2 ** base
    level *= 2 ** ev
    from rawtl.types import LinearFrame
    return LinearFrame(np.full((32, 32, 3), level, dtype=np.float32),
                       None, "unavailable", 1 / 65535, False, "linear-srgb")


class IgnoringBackend:
    def render_linear(self, path, ev, opts):
        return _fake_renderer(path, ev, opts, applies_baseline=False)


class ApplyingBackend:
    def render_linear(self, path, ev, opts):
        return _fake_renderer(path, ev, opts, applies_baseline=True)


def test_probe_detects_ignored_baseline(tmp_path):
    src = tmp_path / "x.dng"
    src.write_bytes(build_tiff("II", (20, 10)))
    result = probe_backend_semantics(IgnoringBackend(), src, None)
    assert result["baseline_exposure"]["applied"] is False
    assert result["baseline_exposure"]["measured_delta_ev"] == pytest.approx(0., abs=1e-9)
    assert result["opcode_list3"]["applied"] is False


def test_probe_detects_applied_baseline(tmp_path):
    src = tmp_path / "x.dng"
    src.write_bytes(build_tiff("II", (20, 10)))
    result = probe_backend_semantics(ApplyingBackend(), src, None)
    assert result["baseline_exposure"]["applied"] is True
    assert result["baseline_exposure"]["measured_delta_ev"] == pytest.approx(1.0, abs=1e-6)


def test_probe_absent_tags_short_circuits(tmp_path):
    src = tmp_path / "x.dng"
    src.write_bytes(build_tiff("II", baseline=None, with_opcode=False))
    result = probe_backend_semantics(IgnoringBackend(), src, None)
    assert result["baseline_exposure"] == {"tag": "absent"}
    assert result["opcode_list3"] == {"tag": "absent"}


EXIFTOOL_ROW = {
    "SourceFile": "/tmp/TIMELAPSE_0040.DNG",
    "DateTimeOriginal": "2026:10:30 21:59:43",
    "SubSecTimeOriginal": "12",
    "ExposureTime": 0.02,
    "ISO": 100,
    "FNumber": 2.8,
    "BaselineExposure": 2.66,
    "BlackLevel": "256 256 256 256",
    "WhiteLevel": 4095,
    "OpcodeList3": "WarpRectilinear(...), GainMap(...)",
}


def test_parse_source_metadata_fields_and_digest():
    info = parse_source_metadata([EXIFTOOL_ROW])
    rec = info["TIMELAPSE_0040.DNG"]
    assert rec.capture_time.endswith(".12") and rec.capture_time.startswith("2026-10-30T21:59:43")
    assert rec.baseline_exposure_ev == 2.66
    assert rec.black_levels == (256., 256., 256., 256.)
    assert rec.white_level == 4095.
    key = "OpcodeList3(0xc74e)"  # 51022
    assert rec.opcode_digests[key] and rec.opcode_lengths[key] > 0
    again = parse_source_metadata([dict(EXIFTOOL_ROW)])
    assert again["TIMELAPSE_0040.DNG"].opcode_digests[key] == rec.opcode_digests[key]
    assert len(rec.camera_config_id) == 16


def _infos(times=None, names=None):
    names = names or [f"T_{i:04}.DNG" for i in range(len(times or []))]
    if times is None:
        return [FrameSourceInfo(n) for n in names]
    base = datetime(2026, 10, 30, 8, 0, 0)
    stamps = [base + timedelta(seconds=s) for s in times]
    return [FrameSourceInfo(n, capture_time=s.isoformat())
            for n, s in zip(names, stamps)]


def test_continuity_finds_nine_gap_pattern():
    # reproduce the benchmark cadence: 60 s nominal, one 38400 s overnight gap
    base = datetime(2026, 10, 30, 8, 0, 0)
    stamps = [base + timedelta(seconds=60 * i) for i in range(11)]
    stamps += [stamps[-1] + timedelta(seconds=38400)]          # index 11
    stamps += [stamps[-1] + timedelta(seconds=60 * i if i else 60)
               for i in range(1, 6)]
    infos = [FrameSourceInfo(f"T_{i:04}.DNG", capture_time=s.isoformat())
             for i, s in enumerate(stamps)]
    cont = continuity(infos)
    assert cont["nominal_interval_s"] == 60.
    gaps = cont["time_gaps"]
    assert [g["index"] for g in gaps] == [11]
    assert gaps[0]["gap_s"] == 38400.
    assert cont["segments"] == [[0, 11], [11, 17]]


def test_continuity_filename_suspicion_is_separate_kind():
    base = datetime(2026, 10, 30, 8, 0, 0)
    names = [f"T_{i:04}.DNG" for i in range(5)]
    names[3] = "T_0999.DNG"
    infos = [FrameSourceInfo(n, capture_time=(base + timedelta(seconds=60 * i)).isoformat())
             for i, n in enumerate(names)]
    cont = continuity(infos)
    assert cont["time_gaps"] == []
    # both boundaries of the odd-numbered file are flagged as suspicion
    assert [s["index"] for s in cont["filename_suspicions"]] == [3, 4]
    assert cont["segments"] == [[0, 5]]


def test_continuity_without_times_degrades_to_filename_only():
    cont = continuity(_infos(times=None,
                             names=[f"T_{i:04}.DNG" for i in range(3)]))
    assert cont["coverage"] == "filename_only"
    assert cont["time_gaps"] == []


def test_opcode_variation_counts_unique_payloads():
    a = FrameSourceInfo("a.DNG", opcode_digests={"OpcodeList3(0xc6fe)": "h1"})
    b = FrameSourceInfo("b.DNG", opcode_digests={"OpcodeList3(0xc6fe)": "h1"})
    c = FrameSourceInfo("c.DNG", opcode_digests={"OpcodeList3(0xc6fe)": "h2"})
    var = opcode_variation({"a.DNG": a, "b.DNG": b, "c.DNG": c})
    entry = var["OpcodeList3(0xc6fe)"]
    assert entry["frames_with_tag"] == 3 and entry["unique_payloads"] == 2
    assert entry["constant"] is False


def test_baseline_summary_ranges():
    infos = {f"{v}.DNG": FrameSourceInfo(f"{v}.DNG", baseline_exposure_ev=v)
             for v in (1.08, 2.0, 3.04)}
    s = baseline_exposure_summary(infos)
    assert s["present"] and s["unique_values"] == 3
    assert s["range_ev"] == [1.08, 3.04]


def test_time_gaps_are_reported_not_boundaries():
    from rawtl.exposure.estimation import estimate_exposure
    from rawtl.exposure.measurement import FrameMeasurement
    k = 96
    centers = np.array([[(i % 12) / 12, (i // 12) / 8] for i in range(k)])
    levels = np.zeros(30)
    levels[15:] += .5          # a big common step exactly at a capture-time gap
    reflectance = np.linspace(-.3, .3, k)
    measurements = [FrameMeasurement(levels[t] + reflectance, np.ones(k, bool),
                                     centers, np.zeros(k), 480, 360, "exact")
                    for t in range(30)]
    result = estimate_exposure(measurements, period=60, time_gaps=[15])
    # gaps are reported metadata only: estimation stays one whole sequence
    assert {f.segment_id for f in result.frames} == {0}
    assert result.time_gaps == [15]
    assert all(f.applied_ev is not None for f in result.frames)
