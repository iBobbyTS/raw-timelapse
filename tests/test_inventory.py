from pathlib import Path
import pytest
from rawtl.inventory import discover, inspect_directory, prevent_output_overlap
from rawtl.types import InputError, RawDecodeError, RawMetadata

class MockRenderer:
    def inspect(self,path):
        if path.name=="broken.CR3":
            raise OSError("corrupt")
        return RawMetadata("Camera X", 6000, 4000, 0, (2.,1.,1.5,1.),(2.,1.,1.5,1.))
class BadRenderer(MockRenderer):
    def inspect(self,path):
        a=super().inspect(path)
        if path.name=="C_2.cr3":
            return RawMetadata("Camera Y",a.width,a.height,a.orientation,a.camera_wb,a.daylight_wb)
        return a

def test_unicode_filename_order_non_recursive(tmp_path):
    for name in ["C_2.cr3","C_1.CR3","C_10.Cr3","ignored.jpg"]:
        (tmp_path/name).write_bytes(b"x")
    (tmp_path/"folder").mkdir()
    (tmp_path/"folder"/"nested.CR3").write_bytes(b"x")
    ordered=discover(tmp_path)
    assert [r.filename for r in ordered]==["C_1.CR3","C_10.Cr3","C_2.cr3"]
    assert [r.index for r in ordered]==[0,1,2]
    assert len(inspect_directory(tmp_path,MockRenderer()).metadata)==3

def test_empty_and_bad_raw(tmp_path):
    with pytest.raises(InputError):
        discover(tmp_path)
    (tmp_path/"broken.CR3").write_bytes(b"broken")
    with pytest.raises(RawDecodeError,match="frame 0"):
        inspect_directory(tmp_path,MockRenderer())

def test_incompatible_metadata(tmp_path):
    (tmp_path/"C_1.CR3").touch()
    (tmp_path/"C_2.cr3").touch()
    with pytest.raises(RawDecodeError,match="index 1"):
        inspect_directory(tmp_path,BadRenderer())

def test_output_overlap(tmp_path):
    raw=tmp_path/"raws"
    raw.mkdir()
    prevent_output_overlap(raw,tmp_path/"out")
    with pytest.raises(InputError):
        prevent_output_overlap(raw,raw/"out")
    with pytest.raises(InputError):
        prevent_output_overlap(raw,raw)
