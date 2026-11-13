"""Prepared-frame prepare/consume conformance for the native tool.

Covers the two-operation contract: --prepare resolves one source to a verified
LinearRaw scratch DNG plus a versioned context, and every later
analysis/develop call consumes that context without invoking the Converter or
deleting the prepared artifact. Python is test infrastructure only.
"""
import hashlib, json, pathlib, shutil, struct, subprocess, sys

from dng_fixture import make_dng

exe, work = map(pathlib.Path, sys.argv[1:])
exe, work = exe.resolve(), work.resolve()
# The artifacts directory is owned by this test and regenerated each run.
if work.exists():
    for entry in work.iterdir():
        if entry.is_dir():
            shutil.rmtree(entry)
        else:
            entry.unlink()
work.mkdir(parents=True, exist_ok=True)
RUNS = []


def log(item):
    RUNS.append(item)
    (work / "commands.json").write_text(json.dumps(RUNS, indent=2))
    print(json.dumps(item), flush=True)


def run(args, code=0):
    r = subprocess.run([str(x) for x in args], capture_output=True)
    log({"command": [str(x) for x in args], "exit_code": r.returncode,
         "stdout": r.stdout.decode(errors="replace"),
         "stderr": r.stderr.decode(errors="replace")})
    assert r.returncode == code, (r.returncode, code, r.stderr)
    return r


def stream(args, code=0):
    r = subprocess.run([str(x) for x in args], capture_output=True)
    log({"command": [str(x) for x in args], "exit_code": r.returncode,
         "stdout_bytes": len(r.stdout),
         "stdout_sha256": hashlib.sha256(r.stdout).hexdigest(),
         "stderr": r.stderr.decode(errors="replace")})
    assert r.returncode == code, (r.returncode, code, r.stderr)
    return r


def records(data, magic=b"RAWTLST1"):
    assert data[:8] == magic, data[:8]
    at, out = 8, []
    while at < len(data):
        assert at + 12 <= len(data), at
        kind = data[at]
        assert data[at + 1:at + 4] == b"\0\0\0", (at, data[at + 1:at + 4])
        length = struct.unpack("<Q", data[at + 4:at + 12])[0]
        at += 12
        assert at + length <= len(data), (at, length, len(data))
        out.append((kind, data[at:at + length]))
        at += length
    return out


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def context_fields(path):
    lines = path.read_bytes().decode().split("\n")
    assert lines[0] == "rawtl-prepared-frame/1", lines[0]
    fields = {}
    for line in lines[1:]:
        if not line:
            continue
        key, _, value = line.partition("=")
        assert key not in fields, key
        fields[key] = value
    return fields


def positional_analysis(fixture, name, options=()):
    side = work / (name + ".analysis.json")
    run([exe, fixture, work / (name + ".png"), work / (name + ".json"),
         "--emit-analysis", side, "--no-png", *options])
    return side


def positional_meta(fixture, name, options=()):
    meta = work / (name + ".json")
    run([exe, fixture, work / (name + ".png"), meta, "--no-png", *options])
    return json.loads(meta.read_text())


def prepare(source, scratch_name, context_name, options=()):
    scratch = work / scratch_name
    scratch.mkdir()
    context = work / context_name
    run([exe, "--prepare", source, "--scratch", scratch,
         "--emit-prepared", context, *options])
    return scratch, context


def converted_count(counter):
    return len(counter.read_text().splitlines()) if counter.exists() else 0


def write_context(path, fields, order):
    path.write_text("\n".join(["rawtl-prepared-frame/1"] +
                              [f"{key}={fields[key]}" for key in order]) + "\n")


base = make_dng(work, "base", w=64, h=64, pixels=[500] * 3 * 64 * 64)
half = make_dng(work, "half", w=128, h=128, pixels=[400] * 3 * 128 * 128)
base_meta = positional_meta(base, "base-reference")

# --- Direct LinearRaw source: prepare performs no Conversion and no decode ----
scratch, context = prepare(base, "direct-scratch", "direct.ctx")
fields = context_fields(context)
assert fields["source"] == str(base.resolve())
assert fields["prepared_dng"] == str(base.resolve())
assert fields["generated"] == "0"
assert fields["route"] == "linearraw-sdk"
assert fields["source_sha256"] == sha(base)
assert fields["prepared_sha256"] == sha(base)
assert fields["converter_used"] == "0"
assert list(scratch.iterdir()) == [], list(scratch.iterdir())
assert float(fields["as_shot_neutral"].split(",")[0]) == base_meta["as_shot_neutral"][0]
print("PASS: direct prepare leaves no cache and carries original identity", flush=True)

# The consumer reproduces the source-path analysis sidecar byte for byte and
# marks that it consumed a prepared context.
for options, label in (([], "plain"), (["--exposure-ev", "0.5"], "ev"),
                       (["--white-balance", "0.7,1,0.6", "--exposure-ev", "-0.25"], "wb-ev")):
    reference = positional_analysis(base, "direct-reference-" + label, options)
    response = stream([exe, "--stdout-analysis", "--prepared", context, *options])
    parsed = records(response.stdout)
    assert [kind for kind, _ in parsed] == [0x01, 0x02]
    assert parsed[1][1] == reference.read_bytes(), label
    metadata = json.loads(parsed[0][1])
    assert metadata["prepared"] == {
        "schema_version": 1,
        "dng": str(base.resolve()),
        "sha256": sha(base),
        "generated": False,
        "consumed_context": True,
    }
    assert metadata["source"] == str(base.resolve())
    assert metadata["source_sha256"] == sha(base)
    assert metadata["analysis_sidecar"] is None
    assert metadata["exposure_ev"] == (0.5 if label == "ev" else (-0.25 if label == "wb-ev" else 0))
print("PASS: direct analysis consumes the context byte-identically", flush=True)

for options, label in (([], "plain"), (["--exposure-ev", "0.25"], "ev")):
    source = records(stream([exe, "--stdout-frame-f32", base, *options]).stdout, b"RAWTLST2")
    consumed = records(stream([exe, "--stdout-frame-f32", "--prepared", context, *options]).stdout, b"RAWTLST2")
    assert source[2][1] == consumed[2][1], label
    assert json.loads(source[1][1])["pixels_sha256"] == json.loads(consumed[1][1])["pixels_sha256"]
    a, b = json.loads(source[0][1]), json.loads(consumed[0][1])
    for key in ("source", "source_sha256", "applied_white_balance", "as_shot_neutral",
                "exposure_ev", "color", "quantization", "observed_scene_min",
                "observed_scene_max"):
        assert a[key] == b[key], (label, key)
print("PASS: direct FP32 develop is bitwise identical to the source path", flush=True)

# ROI composes with prepared analysis.
roi_reference = positional_analysis(half, "roi-reference", ["--analysis-roi", "0.5,0.5,0.5,0.5"])
prepare(half, "roi-scratch", "roi.ctx")
roi = stream([exe, "--stdout-analysis", "--prepared", work / "roi.ctx",
              "--analysis-roi", "0.5,0.5,0.5,0.5"])
assert records(roi.stdout)[1][1] == roi_reference.read_bytes()
print("PASS: prepared analysis accepts an explicit ROI", flush=True)

# Re-preparing the same source is deterministic in every identity field.
_, second = prepare(base, "direct-scratch-2", "direct-2.ctx")
a, b = context_fields(context), context_fields(second)
assert a == b
print("PASS: repeated direct prepare is deterministic", flush=True)

# --- CFA DNG route: exactly one Conversion, cache retained until caller ------
counter = work / "converter-count"
converter = work / "converter-copy"
converter.write_text(
    f"#!/bin/sh\necho call >> {counter}\n"
    f"cp {base.resolve()} \"$4/converted.dng\"\n"
)
converter.chmod(0o755)
cfa = make_dng(work, "cfa", pi=32803, w=64, h=64, pixels=[250, 500, 750] * 4096)
cfa_scratch, cfa_context = prepare(cfa, "cfa-scratch", "cfa.ctx",
                                  ["--converter", converter])
assert converted_count(counter) == 1
cfa_fields = context_fields(cfa_context)
assert cfa_fields["generated"] == "1"
assert cfa_fields["route"] == "cfa-converter-linearraw-sdk"
assert cfa_fields["source"] == str(cfa.resolve())
prepared_dng = pathlib.Path(cfa_fields["prepared_dng"])
assert prepared_dng != cfa
assert prepared_dng.exists() and prepared_dng.parent == cfa_scratch / "linear"
assert cfa_fields["prepared_sha256"] == sha(prepared_dng)
assert cfa_fields["converter_used"] == "1"
assert cfa_fields["converter_argc"] == "6"
assert cfa_fields["converter_output_name"] == prepared_dng.name
assert "caller-owned" in cfa_fields["converter_cleanup"]

cfa_source_f32 = records(stream([exe, "--stdout-frame-f32", base]).stdout, b"RAWTLST2")
cfa_consumed = records(stream([exe, "--stdout-frame-f32", "--prepared", cfa_context]).stdout, b"RAWTLST2")
assert cfa_consumed[2][1] == cfa_source_f32[2][1]
cfa_meta = json.loads(cfa_consumed[0][1])
assert cfa_meta["route"] == "cfa-converter-linearraw-sdk"
assert cfa_meta["converter"]["used"] is True
assert cfa_meta["prepared"]["generated"] is True
assert cfa_meta["as_shot_neutral"] == base_meta["as_shot_neutral"]
assert converted_count(counter) == 1
assert prepared_dng.exists(), "prepared DNG must survive native exit"
run([exe, "--stdout-analysis", "--prepared", cfa_context,
     "--converter", converter], 2)
run([exe, "--stdout-analysis", "--prepared", cfa_context,
     "--converter-timeout", "5"], 2)
assert converted_count(counter) == 1
print("PASS: CFA prepare converts once and consumers never reconvert", flush=True)

# --- Vendor RAW route: metadata comes from the converted DNG ----------------
vendor_output = make_dng(work, "vendor-output", datetime_original="2010:01:02 03:04:05")
vendor_counter = work / "vendor-count"
vendor_converter = work / "converter-vendor"
vendor_converter.write_text(
    f"#!/bin/sh\necho call >> {vendor_counter}\n"
    f"cp {vendor_output.resolve()} \"$4/converted.dng\"\n"
)
vendor_converter.chmod(0o755)
vendor = work / "vendor-cr3-like.CR3"
vendor.write_bytes(b"\0\0\0\x18ftypcrx " * 40 + b"\x01\x02" + bytes(range(256)))
vendor_scratch, vendor_context = prepare(vendor, "vendor-scratch", "vendor.ctx",
                                        ["--converter", vendor_converter])
assert converted_count(vendor_counter) == 1
vendor_fields = context_fields(vendor_context)
assert vendor_fields["route"] == "vendor-raw-converter-linearraw-sdk"
assert vendor_fields["generated"] == "1"
assert vendor_fields["has_capture_time"] == "1"
assert vendor_fields["capture_time"] == "2010:01:02 03:04:05"
assert vendor_fields["source_sha256"] == sha(vendor)
vendor_consumed = records(stream([exe, "--stdout-frame-f32", "--prepared", vendor_context]).stdout, b"RAWTLST2")
vendor_meta = json.loads(vendor_consumed[0][1])
assert vendor_meta["route"] == "vendor-raw-converter-linearraw-sdk"
assert vendor_meta["source"] == str(vendor.resolve())
assert vendor_meta["source_sha256"] == sha(vendor)
assert vendor_meta["capture_time"] == "2010:01:02 03:04:05"
assert vendor_meta["as_shot_neutral"] == positional_meta(
    vendor_output, "vendor-output-reference")["as_shot_neutral"]
assert converted_count(vendor_counter) == 1
print("PASS: vendor RAW identity/metadata survive the prepared boundary", flush=True)

# --- Caller-owned deletion: consumers fail explicitly after the caller deletes -
deleted_context = work / "deleted.ctx"
deleted_scratch = work / "deleted-scratch"
deleted_scratch.mkdir()
run([exe, "--prepare", cfa, "--scratch", deleted_scratch,
     "--emit-prepared", deleted_context, "--converter", converter])
deleted_fields = context_fields(deleted_context)
deleted_dng = pathlib.Path(deleted_fields["prepared_dng"])
assert deleted_dng.exists()
deleted_dng.unlink()
r = run([exe, "--stdout-analysis", "--prepared", deleted_context], 26)
assert b"prepared" in r.stderr, r.stderr
assert cfa.exists() and base.exists()
print("PASS: missing prepared DNG is an explicit error and sources survive", flush=True)

# --- Contract failures: every malformed context fails closed -----------------
identity_order = ["source", "source_sha256", "route", "prepared_dng",
                  "prepared_sha256", "generated", "main_ifd_width",
                  "main_ifd_height", "as_shot_neutral", "has_capture_time",
                  "converter_used"]
cfa_base = dict(cfa_fields)
cfa_other = make_dng(work, "cfa-other", pi=32803, w=64, h=64, pixels=[100] * 3 * 4096)


def mutate(name, **changes):
    fields = dict(cfa_base)
    fields.update(changes)
    path = work / (name + ".ctx")
    write_context(path, fields, identity_order +
                  [key for key in fields if key not in identity_order])
    return path


# Prepared artifact replaced by another file: hash mismatch.
mutate("bad-hash", prepared_sha256="0" * 64)
run([exe, "--stdout-analysis", "--prepared", work / "bad-hash.ctx"], 26)
# Prepared path is a CFA DNG rather than a LinearRaw artifact.
mutate("not-linear", prepared_dng=str(cfa_other.resolve()),
       prepared_sha256=sha(cfa_other),
       converter_output_sha256=sha(cfa_other),
       converter_output_name=cfa_other.name)
r = run([exe, "--stdout-analysis", "--prepared", work / "not-linear.ctx"], 26)
assert b"LinearRaw" in r.stderr, r.stderr
# Cache path equals the original source for a converter route.
mutate("cache-is-source", prepared_dng=str(cfa.resolve()),
       prepared_sha256=sha(cfa), converter_output_name=cfa.name)
run([exe, "--stdout-analysis", "--prepared", work / "cache-is-source.ctx"], 26)
# Original source replaced: the identity hash no longer matches.
mutate("source-hash", source_sha256="1" * 64)
run([exe, "--stdout-analysis", "--prepared", work / "source-hash.ctx"], 26)
# Route disagrees with the recomputed source route.
mutate("wrong-route", route="linearraw-sdk", generated="0")
run([exe, "--stdout-analysis", "--prepared", work / "wrong-route.ctx"], 26)
# Geometry disagrees with the prepared artifact.
mutate("wrong-geometry", main_ifd_width="32")
run([exe, "--stdout-analysis", "--prepared", work / "wrong-geometry.ctx"], 26)
# Converter provenance does not describe the prepared DNG.
mutate("wrong-output", converter_output_name="other.dng")
run([exe, "--stdout-analysis", "--prepared", work / "wrong-output.ctx"], 26)
# Relative paths are never reinterpreted from the current directory.
mutate("relative", prepared_dng="linear/converted.dng")
run([exe, "--stdout-analysis", "--prepared", work / "relative.ctx"], 26)
# Unknown, duplicate, truncated-escape and wrong-header documents.
(work / "unknown.ctx").write_bytes(
    context.read_bytes() + b"surprise=1\n")
run([exe, "--stdout-analysis", "--prepared", work / "unknown.ctx"], 26)
(work / "duplicate.ctx").write_bytes(
    context.read_bytes() + b"source=" + str(base).encode() + b"\n")
run([exe, "--stdout-analysis", "--prepared", work / "duplicate.ctx"], 26)
(work / "escape.ctx").write_bytes(
    context.read_bytes().replace(b"rawtl-prepared-frame/1",
                                 b"rawtl-prepared-frame/1\nsource=%zz"))
run([exe, "--stdout-analysis", "--prepared", work / "escape.ctx"], 26)
(work / "header.ctx").write_bytes(
    context.read_bytes().replace(b"rawtl-prepared-frame/1",
                                 b"rawtl-prepared-frame/2", 1))
run([exe, "--stdout-analysis", "--prepared", work / "header.ctx"], 26)
print("PASS: tampered prepared contexts fail closed with explicit errors", flush=True)

# --- Prepare-time Converter failures are explicit ---------------------------
def failing(name, body):
    path = work / name
    path.write_text("#!/bin/sh\n" + body + "\n")
    path.chmod(0o755)
    return path


failure = failing("converter-failure", "echo deliberate-failure >&2\nexit 7")
failure_scratch = work / "failure-scratch"
failure_scratch.mkdir()
r = run([exe, "--prepare", cfa, "--scratch", failure_scratch,
         "--emit-prepared", work / "failure.ctx", "--converter", failure], 24)
assert b"deliberate-failure" in r.stderr, r.stderr
slow = failing("converter-timeout", "exec /bin/sleep 10")
slow_scratch = work / "timeout-scratch"
slow_scratch.mkdir()
run([exe, "--prepare", cfa, "--scratch", slow_scratch,
     "--emit-prepared", work / "timeout.ctx", "--converter", slow,
     "--converter-timeout", "1"], 25)
run([exe, "--prepare", work / "no-such-source.CR3", "--scratch", failure_scratch,
     "--emit-prepared", work / "missing.ctx"], 26)
print("PASS: prepare Converter failure/timeout and missing source are explicit", flush=True)

# --- CLI grammar ------------------------------------------------------------
scratch_missing = work / "missing-scratch"
run([exe, "--prepare", base, "--emit-prepared", work / "x.ctx"], 2)
run([exe, "--prepare", base, "--scratch", scratch_missing], 2)
run([exe, "--prepare", base, "--scratch", scratch_missing,
     "--emit-prepared", work / "x.ctx"], 26)
existing = work / "existing-scratch"
existing.mkdir()
(existing / "linear").mkdir()
run([exe, "--prepare", cfa, "--scratch", existing,
     "--emit-prepared", work / "x.ctx", "--converter", converter], 26)
third_scratch = work / "direct-scratch-3"
third_scratch.mkdir()
run([exe, "--prepare", base, "--scratch", third_scratch,
     "--emit-prepared", base], 2)
run([exe, "--prepare", base, "--scratch", scratch, "--emit-prepared",
     work / "x.ctx", "--white-balance", "1,1,1"], 2)
run([exe, "--stdout-analysis", "--prepared", context, "--prepared", context], 2)
run([exe, "--stdout-analysis", "--prepared", context, "--no-png"], 2)
run([exe, "--stdout-frame-f32", "--prepared", context,
     "--analysis-roi", "0,0,1,1"], 2)
run([exe, "--stdout-frame-f32", "--prepared", context, "--bogus", "value"], 2)
run([exe, "--stdout-analysis", "--prepared"], 2)
run([exe, "--stdout-analysis", "--prepared", work / "nope.ctx"], 26)
print("PASS: prepare/consume CLI grammar rejects unsupported combinations", flush=True)

# --- Compatibility wrapper keeps the original single-shot contract ----------
# Positional output still exists, the stdout source path still emits no
# context, and the two implementations agree on the analysis bytes.
single = records(stream([exe, "--stdout-analysis", base]).stdout)
assert json.loads(single[0][1])["prepared"]["consumed_context"] is False
assert single[1][1] == positional_analysis(base, "compat-reference").read_bytes()
assert not (work / "compat-reference.png").exists()

(work / "summary.json").write_text(json.dumps({
    "status": "PASS",
    "direct_analysis_equivariance": True,
    "direct_fp32_bitwise": True,
    "converter_calls": converted_count(counter),
    "vendor_converter_calls": converted_count(vendor_counter),
}, indent=2))
print("PASS: prepared-frame prepare/consume contract", flush=True)
