"""Deterministic flat RAW inventory, compatibility validation, and source manifest."""
from __future__ import annotations
from dataclasses import dataclass
from pathlib import Path
from typing import Sequence
from .types import FrameRef, RawMetadata, RawRenderer, InputError, RawDecodeError

RAW_EXTENSIONS = frozenset({".cr2",".cr3",".nef",".arw",".dng",".raf",".rw2",".orf"})

@dataclass(frozen=True)
class Inventory:
    frames: tuple[FrameRef, ...]
    metadata: tuple[RawMetadata, ...]

    @property
    def first(self) -> RawMetadata:
        return self.metadata[0]

    def source_manifest(self) -> list[dict]:
        out=[]
        for ref in self.frames:
            st=ref.path.stat()
            out.append({"index":ref.index,"filename":ref.filename,
                "path":str(ref.path),"size":st.st_size,"mtime_ns":st.st_mtime_ns})
        return out

def discover(folder: Path) -> tuple[FrameRef, ...]:
    folder=folder.expanduser().resolve()
    if not folder.is_dir():
        raise InputError(f"input is not a directory: {folder}")
    paths=sorted((p for p in folder.iterdir()
        if p.is_file() and p.suffix.lower() in RAW_EXTENSIONS),key=lambda p:p.name)
    if not paths:
        raise InputError(f"no candidate RAW files in {folder} (nonrecursive)")
    return tuple(FrameRef(i,path,path.name) for i,path in enumerate(paths))

def prevent_output_overlap(input_dir: Path, output_dir: Path) -> None:
    source=input_dir.expanduser().resolve()
    target=output_dir.expanduser().resolve()
    if source == target or source in target.parents:
        raise InputError(f"output directory must not be input directory or a descendant: {target}")

def check_metadata(ordered: Sequence[FrameRef], renderer: RawRenderer) -> Inventory:
    if not ordered:
        raise InputError("empty frame list")
    meta=[]
    for frame in ordered:
        try:
            data=renderer.inspect(frame.path)
        except RawDecodeError:
            raise
        except Exception as exc:
            raise RawDecodeError(f"RAW inspection failed at frame {frame.index} ({frame.filename}): {exc}") from exc
        if data.width <= 0 or data.height <= 0:
            raise RawDecodeError(f"invalid RAW dimensions for {frame.filename}: {data.width}x{data.height}")
        if meta:
            first=meta[0]
            if (data.camera,data.width,data.height,data.orientation) != (
                first.camera,first.width,first.height,first.orientation):
                raise RawDecodeError(
                    f"incompatible RAW at index {frame.index} {frame.filename}: "
                    f"{data.camera} {data.width}x{data.height} orientation={data.orientation}; "
                    f"expected {first.camera} {first.width}x{first.height} orientation={first.orientation}")
        meta.append(data)
    return Inventory(tuple(ordered),tuple(meta))

def inspect_directory(input_dir: Path, renderer: RawRenderer) -> Inventory:
    return check_metadata(discover(input_dir),renderer)
