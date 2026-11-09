"""Regression checks for the frozen V4 golden fixtures (T00).

These tests validate the committed fixture artifacts and generator path safety;
they do not need the read-only RAW material and never regenerate anything.
Field-by-field replay against the material happens in later Rust migration tasks.
"""
from __future__ import annotations

import hashlib
import importlib.util
import json
from pathlib import Path

import pytest

FIXTURE_DIR = Path(__file__).resolve().parent / "fixtures" / "real-material"


def _load(name: str) -> dict:
    return json.loads((FIXTURE_DIR / name).read_text(encoding="utf8"))


def _sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


@pytest.fixture(scope="module")
def manifest() -> dict:
    assert FIXTURE_DIR.is_dir(), f"missing golden fixture directory: {FIXTURE_DIR}"
    return _load("manifest.json")


def test_manifest_files_are_complete_and_unmodified(manifest):
    listed = {entry["path"]: entry for entry in manifest["files"]}
    assert len(listed) == len(manifest["files"])
    assert manifest["parameters"]["material_frames"] == 39
    for rel, entry in listed.items():
        path = FIXTURE_DIR / rel
        assert path.is_file(), f"manifest lists missing file: {rel}"
        assert path.stat().st_size == entry["bytes"], f"size drift: {rel}"
        assert _sha256(path) == entry["sha256"], f"content drift: {rel}"
    actual = {
        path.relative_to(FIXTURE_DIR).as_posix()
        for path in FIXTURE_DIR.rglob("*")
        if path.is_file() and path.name not in ("manifest.json", "README.md")
    }
    assert actual == set(listed), "fixture directory contains unlisted or missing files"


def test_no_raw_material_is_stored():
    assert not list(FIXTURE_DIR.rglob("*.DNG"))
    assert not list(FIXTURE_DIR.rglob("*.dng"))


def test_measurement_fixtures_match_input_basis():
    basis = _load("measurement-input-basis.json")
    measurements = _load("frame-measurements.json")
    assert basis["material"]["frame_count"] == 39
    assert len(basis["material"]["frames"]) == 39
    assert len(basis["renderer"]["fixed_white_balance"]) == 4
    assert basis["measurement"]["grid"] == [12, 8]
    assert measurements["grid"]["tiles"] == 96
    assert len(measurements["frames"]) == 39

    previous_centers = None
    for frame in measurements["frames"]:
        assert len(frame["log_luminance"]) == 96
        assert len(frame["valid"]) == 96
        assert len(frame["centers"]) == 96
        assert len(frame["saturation_fraction"]) == 96
        assert len(frame["log_channel_means"]) == 96
        assert all(len(row) == 3 for row in frame["log_channel_means"])
        if previous_centers is not None:
            assert frame["centers"] == previous_centers
        previous_centers = frame["centers"]
        samples = frame["log_pixel_samples"]
        assert samples["dtype"] == "<f4"
        assert samples["shape"][0] == 96
        path = FIXTURE_DIR / samples["file"]
        assert path.stat().st_size == samples["shape"][0] * samples["shape"][1] * 4
        assert _sha256(path) == samples["sha256"]


def test_pair_constraints_and_exposure_plan_agree():
    pairs = _load("pair-constraints.json")
    plan = _load("exposure-plan.json")
    result = plan["result"]
    assert pairs["edge_count"] == len(pairs["edges"])
    assert result["pairwise_summary"]["edges"] == pairs["edge_count"]
    assert len(result["frames"]) == 39
    assert len(result["tile_log_luminance"]) == 39
    assert all(len(row) == 96 for row in result["tile_log_luminance"])
    assert len(result["tile_valid"]) == 39
    assert len(result["consensus_mask"]) == 39
    assert len(result["frozen_target"]) == 39
    # The frozen behaviour on this material: no pair survives the legacy gates.
    assert pairs["edge_count"] == 0
    assert {frame["status"] for frame in result["frames"]} == {"REVIEW_REQUIRED"}
    # The standalone estimation record must equal what the end-to-end run used.
    report = _load("end-to-end-baseline.json")["report"]
    assert result["pairwise_summary"] == report["pairwise_estimation"]
    assert result["scene_cuts"] == report["scene_cuts"]
    assert result["time_gaps"] == report["estimation_time_gaps"]
    assert [
        (frame["status"], frame["applied_ev"], frame["residual_mad_ev"],
         frame["valid_tile_count"])
        for frame in result["frames"]
    ] == [
        (frame["status"], frame["applied_ev"], frame["residual_mad_ev"],
         frame["valid_tile_count"])
        for frame in report["frames"]
    ]


def test_end_to_end_baseline_matches_reference_artifact():
    record = _load("end-to-end-baseline.json")
    report = record["report"]
    audit = _load("temporal-audit.json")
    video = FIXTURE_DIR / "reference-video.mp4"
    assert video.is_file()
    assert report["frame_count"] == 39
    assert len(report["frames"]) == 39
    assert report["video_metadata"]["frames"] == 39
    assert report["video_metadata"]["file_size"] == video.stat().st_size
    assert audit["video_sha256"] == _sha256(video)
    assert audit["frame_count"] == 39
    assert record["service_result"]["frame_count"] == 39
    assert record["service_result"]["encoder"] == "libx264"
    assert all(frame["applied_ev"] is None for frame in report["frames"])
    assert report["run_status"] == "PARTIAL"


@pytest.fixture(scope="module")
def golden_generator():
    source = Path(__file__).resolve().parents[1] / "tools" / "generate_golden_fixtures.py"
    spec = importlib.util.spec_from_file_location("golden_generator_tests", source)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _directory_snapshot(root):
    return {
        path.relative_to(root).as_posix(): path.read_bytes() if path.is_file() else None
        for path in root.rglob("*")
    }


@pytest.mark.parametrize("case", [
    "input", "input-parent", "input-child", "output", "output-parent",
    "output-child", "input-symlink", "output-symlink",
])
def test_generator_rejects_overlapping_work_paths(golden_generator, tmp_path, case):
    input_dir = tmp_path / "material" / "input"
    input_dir.mkdir(parents=True)
    (input_dir / "frame.DNG").write_bytes(b"read-only material")
    output_dir = tmp_path / "fixtures"
    output_dir.mkdir()
    (output_dir / "keep.json").write_bytes(b"existing fixture")
    work_dir = {
        "input": input_dir,
        "input-parent": input_dir.parent,
        "input-child": input_dir / "scratch",
        "output": output_dir,
        "output-parent": tmp_path,
        "output-child": output_dir / "scratch",
    }.get(case)
    if case.endswith("symlink"):
        work_dir = tmp_path / "alias"
        work_dir.symlink_to(input_dir if case == "input-symlink" else output_dir,
                            target_is_directory=True)
    before = _directory_snapshot(tmp_path)
    with pytest.raises(ValueError, match="work directory must not overlap"):
        golden_generator.validate_directories(input_dir, output_dir, work_dir)
    assert _directory_snapshot(tmp_path) == before


def test_generator_cli_rejects_overlap_before_mutation(
    golden_generator, tmp_path, monkeypatch, capsys,
):
    input_dir = tmp_path / "material"
    input_dir.mkdir()

    def forbidden(*args, **kwargs):
        pytest.fail("invalid paths must be rejected before mutation or generation")

    monkeypatch.setattr(Path, "mkdir", forbidden)
    monkeypatch.setattr(golden_generator.shutil, "rmtree", forbidden)
    monkeypatch.setattr(golden_generator, "generate", forbidden)
    with pytest.raises(SystemExit) as error:
        golden_generator.main([
            "--input-dir", str(input_dir), "--output-dir", str(tmp_path / "fixtures"),
            "--work-dir", str(input_dir),
        ])
    assert error.value.code == 2
    assert "work directory must not overlap" in capsys.readouterr().err


def test_generator_refuses_preexisting_work_directory(
    golden_generator, tmp_path, monkeypatch, capsys,
):
    input_dir = tmp_path / "material"
    output_dir = tmp_path / "fixtures"
    work_dir = tmp_path / "unrelated"
    for directory in (input_dir, output_dir, work_dir / "e2e"):
        directory.mkdir(parents=True)
        (directory / "keep").write_bytes(b"must survive")
    before = _directory_snapshot(tmp_path)

    def forbidden(*args, **kwargs):
        pytest.fail("a pre-existing work directory must not reach generation or cleanup")

    monkeypatch.setattr(golden_generator, "generate", forbidden)
    monkeypatch.setattr(golden_generator.shutil, "rmtree", forbidden)
    with pytest.raises(SystemExit) as error:
        golden_generator.main([
            "--input-dir", str(input_dir), "--output-dir", str(output_dir),
            "--work-dir", str(work_dir),
        ])
    assert error.value.code == 2
    assert "work directory must not already exist" in capsys.readouterr().err
    assert _directory_snapshot(tmp_path) == before


def test_generator_does_not_clean_work_created_after_validation(
    golden_generator, tmp_path, monkeypatch, capsys,
):
    input_dir = tmp_path / "material"
    input_dir.mkdir()
    output_dir = tmp_path / "fixtures"
    work_dir = tmp_path / "scratch"
    validate = golden_generator.validate_directories

    def validate_then_create(*args):
        resolved = validate(*args)
        work_dir.mkdir()
        (work_dir / "keep").write_bytes(b"created by another invocation")
        return resolved

    def forbidden(*args, **kwargs):
        pytest.fail("failed exclusive creation must not reach generation or cleanup")

    monkeypatch.setattr(golden_generator, "validate_directories", validate_then_create)
    monkeypatch.setattr(golden_generator, "generate", forbidden)
    monkeypatch.setattr(golden_generator.shutil, "rmtree", forbidden)
    with pytest.raises(SystemExit) as error:
        golden_generator.main([
            "--input-dir", str(input_dir), "--output-dir", str(output_dir),
            "--work-dir", str(work_dir),
        ])
    assert error.value.code == 2
    assert "work directory must not already exist" in capsys.readouterr().err
    assert (work_dir / "keep").read_bytes() == b"created by another invocation"
    assert not output_dir.exists()


@pytest.mark.parametrize("explicit_work", [False, True], ids=["default", "explicit"])
@pytest.mark.parametrize("fail_generation", [False, True], ids=["success", "failure"])
def test_generator_cleans_only_owned_scratch(
    golden_generator, tmp_path, monkeypatch, explicit_work, fail_generation,
):
    input_dir = tmp_path / "material"
    output_dir = tmp_path / "fixtures"
    unrelated = tmp_path / "unrelated"
    for directory in (input_dir, output_dir, unrelated):
        directory.mkdir()
        (directory / "keep").write_bytes(b"must survive")
    before = _directory_snapshot(tmp_path)
    work_dir = tmp_path / ("scratch" if explicit_work else "fixtures.work")
    calls = []

    def fake_generate(actual_input, actual_output, actual_work, ffmpeg, threads):
        calls.append((actual_input, actual_output, actual_work))
        assert actual_work.is_dir()
        assert list(actual_work.iterdir()) == []
        (actual_work / "e2e").mkdir()
        (actual_work / "e2e" / "partial").write_bytes(b"incomplete generation")
        if fail_generation:
            raise RuntimeError("injected generation failure")

    monkeypatch.setattr(golden_generator, "generate", fake_generate)
    argv = ["--input-dir", str(input_dir), "--output-dir", str(output_dir)]
    if explicit_work:
        argv.extend(["--work-dir", str(work_dir)])
    if fail_generation:
        with pytest.raises(RuntimeError, match="injected generation failure"):
            golden_generator.main(argv)
    else:
        assert golden_generator.main(argv) == 0
    assert calls == [(input_dir.resolve(), output_dir.resolve(), work_dir.resolve())]
    assert not work_dir.exists()
    assert _directory_snapshot(tmp_path) == before
