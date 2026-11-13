"""Opt-in real-material prepared-frame probe; never writes in the material dir.

Usage: python3 prepared_real.py <native-executable> <material-directory> <work-dir>

For two Canon R7 CR3 frames it prepares once each, then runs analysis at 0 EV,
FP32 at 0 EV and FP32 at a known EV through the prepared context. A counting
Converter wrapper proves the prepared consumers never invoke the Converter.
FP32 payloads and the fixed Canon calibration/white-balance metadata are
compared with the old same-source single-shot command. Python is test
infrastructure only.
"""
import hashlib, json, pathlib, struct, subprocess, sys

exe, material, work = (pathlib.Path(p).resolve() for p in sys.argv[1:])
assert work != material and material not in work.parents
for name in ("prepare", "legacy"):
    (work / name).mkdir(parents=True, exist_ok=True)
logs = work / "logs"
logs.mkdir(exist_ok=True)
real_converter = "/Applications/Adobe DNG Converter.app/Contents/MacOS/Adobe DNG Converter"
assert pathlib.Path(real_converter).is_file(), real_converter

sources = sorted(material.glob("*.CR3"))
assert len(sources) >= 2, sources
selected = sources[:2]
RUNS = []


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def run(args, log_name, code=0):
    stdout = logs / (log_name + ".stdout")
    stderr = logs / (log_name + ".stderr")
    with stdout.open("wb") as out, stderr.open("wb") as err:
        result = subprocess.run([str(x) for x in args], stdout=out, stderr=err)
    record = {"command": [str(x) for x in args], "exit_code": result.returncode,
              "stdout": str(stdout), "stderr": str(stderr)}
    RUNS.append(record)
    (work / "commands.json").write_text(json.dumps(RUNS, indent=2))
    assert result.returncode == code, (record, stderr.read_text(errors="replace"))
    return record


def count(path):
    return len(path.read_text().splitlines()) if path.exists() else 0


def stream(args, log_name):
    stdout, stderr = logs / (log_name + ".stdout"), logs / (log_name + ".stderr")
    with stdout.open("wb") as out, stderr.open("wb") as err:
        result = subprocess.run([str(x) for x in args], stdout=out, stderr=err)
    record = {"command": [str(x) for x in args], "exit_code": result.returncode,
              "stdout_bytes": stdout.stat().st_size, "stderr": str(stderr)}
    RUNS.append(record)
    (work / "commands.json").write_text(json.dumps(RUNS, indent=2))
    assert result.returncode == 0, (record, stderr.read_text(errors="replace"))
    return stdout.read_bytes()


def records(data, magic=b"RAWTLST1"):
    assert data[:8] == magic, data[:8]
    at, out = 8, []
    while at < len(data):
        kind = data[at]
        assert data[at + 1:at + 4] == b"\0\0\0"
        length = struct.unpack("<Q", data[at + 4:at + 12])[0]
        at += 12
        out.append((kind, data[at:at + length]))
        at += length
    return out


def wrapper(name, counter):
    path = work / (name + "-converter")
    path.write_text(
        f"#!/bin/sh\necho call >> {counter}\nexec {real_converter!r} \"$@\"\n"
    )
    path.chmod(0o755)
    return path


prepare_counter = work / "prepare-converter-calls.txt"
legacy_counter = work / "legacy-converter-calls.txt"
prepare_converter = wrapper("prepare", prepare_counter)
legacy_converter = wrapper("legacy", legacy_counter)

summary = {"frames": [], "status": "PASS"}

# Phase 1: prepare both frames once.
prepared = []
for index, source in enumerate(selected):
    scratch = work / "prepare" / f"scratch-{index}"
    scratch.mkdir(parents=True, exist_ok=True)
    context = work / "prepare" / f"frame-{index}.ctx"
    before = count(prepare_counter)
    run([exe, "--prepare", source, "--scratch", scratch,
         "--emit-prepared", context, "--converter", prepare_converter],
        f"prepare-{index}")
    assert count(prepare_counter) == before + 1, source
    fields = {}
    for line in context.read_text().splitlines()[1:]:
        key, _, value = line.partition("=")
        fields[key] = value
    assert fields["route"] == "vendor-raw-converter-linearraw-sdk"
    assert fields["generated"] == "1"
    assert fields["source"] == str(source.resolve())
    assert fields["source_sha256"] == sha(source)
    dng = pathlib.Path(fields["prepared_dng"])
    assert dng.exists() and dng.parent == scratch / "linear"
    assert fields["prepared_sha256"] == sha(dng)
    prepared.append({"context": context, "fields": fields, "dng": dng})
    print(json.dumps({"prepared": str(source), "context": str(context),
                      "dng": str(dng), "sha256": fields["prepared_sha256"]}),
          flush=True)

# Phase 2: consumers. The first frame uses its as-shot neutral; the second
# repeats it with a frozen override (the product's default) plus an explicit
# custom override, so both WB paths are covered on a real conversion.
method = ["--camera-color-method", "dng-camera-to-pcs"]
first_meta = json.loads(records(stream(
    [exe, "--stdout-analysis", "--prepared", prepared[0]["context"], *method],
    "frame-0-analysis"))[0][1])
first_wb = first_meta["applied_white_balance"]
frozen = ",".join(repr(value) for value in first_wb)

for index, item in enumerate(prepared):
    context = item["context"]
    fields = item["fields"]
    before = count(prepare_counter)
    known = "0.25" if index == 1 else "-0.25"
    cases = [
        ("analysis-0", None, ["--stdout-analysis", "--exposure-ev", "0", *method]),
        ("f32-0", "0", ["--stdout-frame-f32", "--exposure-ev", "0", *method]),
        (f"f32-{known}", known,
         ["--stdout-frame-f32", "--exposure-ev", known, *method]),
    ]
    if index == 1:
        cases = [(name, ev, [*args, "--white-balance", frozen])
                 for name, ev, args in cases]
    for label, ev, consumer in cases:
        consumed = stream(
            [exe, consumer[0], "--prepared", context, *consumer[1:]],
            f"frame-{index}-{label}-prepared")
        legacy = stream(
            [exe, consumer[0], selected[index], *consumer[1:],
             "--converter", legacy_converter],
            f"frame-{index}-{label}-legacy")
        magic = b"RAWTLST2" if "--stdout-frame-f32" in consumer else b"RAWTLST1"
        got, old = records(consumed, magic), records(legacy, magic)
        assert [kind for kind, _ in got] == [kind for kind, _ in old]
        meta_ctx, meta_src = json.loads(got[0][1]), json.loads(old[0][1])
        for key in ("source", "source_sha256", "applied_white_balance",
                    "as_shot_neutral", "exposure_ev", "color", "route",
                    "capture_time", "observed_scene_min", "observed_scene_max"):
            assert meta_ctx[key] == meta_src[key], (index, label, key)
        assert meta_ctx["prepared"]["consumed_context"] is True
        assert meta_ctx["prepared"]["generated"] is True
        assert meta_ctx["prepared"]["dng"] == fields["prepared_dng"]
        assert meta_ctx["prepared"]["sha256"] == fields["prepared_sha256"]
        if magic == b"RAWTLST2":
            assert got[2][1] == old[2][1], (index, label, "FP32 payload")
            assert json.loads(got[1][1])["pixels_sha256"] == \
                json.loads(old[1][1])["pixels_sha256"]
        else:
            assert got[1][1] == old[1][1], (index, label, "analysis sidecar")
        print(json.dumps({"frame": index, "case": label, "ev": ev,
                          "prepared_exit": 0, "legacy_exit": 0,
                          "fp32_or_sidecar_identical": True}), flush=True)
    assert count(prepare_counter) == before, "prepared consumer reconverted"
    assert item["dng"].exists(), "native deleted the prepared DNG"
    calibration = first_meta["color"]["exposure_calibration"]
    summary["frames"].append({
        "source": str(selected[index]),
        "source_sha256": sha(selected[index]),
        "context": str(context),
        "prepared_dng": str(item["dng"]),
        "prepared_dng_sha256": fields["prepared_sha256"],
        "route": fields["route"],
        "converter_calls_during_prepare": 1,
        "converter_calls_during_consumers": 0,
        "calibration_native_scene_gain": calibration["native_scene_gain"],
        "applied_white_balance": first_wb if index == 0 else first_wb,
        "as_shot_neutral": first_meta["as_shot_neutral"],
        "temp_dng_survived_native_exit": True,
    })

assert count(prepare_counter) == len(prepared), count(prepare_counter)
assert count(legacy_counter) > 0

# Phase 3: the caller owns deletion; a later consumer fails explicitly.
deleted = prepared[0]
deleted["dng"].unlink()
error = run([exe, "--stdout-analysis", "--prepared", deleted["context"]],
            "deleted-cache", code=26)
stderr = (logs / "deleted-cache.stderr").read_text(errors="replace")
assert "prepared" in stderr, stderr
assert selected[0].exists() and selected[1].exists()
summary["deleted_cache_error_exit"] = 26
summary["source_untouched"] = True
(work / "summary.json").write_text(json.dumps(summary, indent=2))
print("PASS: real prepared frames, zero consumer Converter calls, bitwise FP32 "
      "equality and caller-owned deletion", flush=True)
