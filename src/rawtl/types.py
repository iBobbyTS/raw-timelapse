"""Stable, UI-agnostic request, progress and frame contracts."""
from __future__ import annotations

from dataclasses import dataclass, field
from fractions import Fraction
from pathlib import Path
from threading import Event
from typing import Callable, Protocol, Any
import numpy as np

@dataclass(frozen=True)
class FrameRef:
    index: int
    path: Path
    filename: str
    segment_id: int = 0

@dataclass(frozen=True)
class RawMetadata:
    camera: str
    width: int
    height: int
    orientation: int
    camera_wb: tuple[float, float, float, float] | None
    daylight_wb: tuple[float, float, float, float] | None
    raw_type: str = ""
    sensor_signature: str = ""

@dataclass(frozen=True)
class RawRenderOptions:
    fixed_white_balance: tuple[float, float, float, float]
    color_transform_id: str = "libraw_builtin_srgb"
    transfer_id: str = "linear"

@dataclass
class LinearFrame:
    rgb: np.ndarray
    saturation_mask: np.ndarray | None = None
    saturation_mask_quality: str = "unavailable"
    quantum_linear: float = 1 / 65535
    clipped_before_output: bool = True
    working_primaries: str = "linear-srgb"

@dataclass(frozen=True)
class ExposureStep:
    start_frame: int
    delta_ev: float

@dataclass(frozen=True)
class ProcessingRequest:
    input_dir: Path
    output_dir: Path
    fps: Fraction = Fraction(30, 1)
    period: int = 60
    strength: float = 1.0
    brightness: str = "smooth"
    roi: tuple[float, float, float, float] = (0., 0., 1., 1.)
    steps: tuple[ExposureStep, ...] = ()
    on_review: str = "error"
    temporal_quality: str = "strict"
    save_frames: bool = False
    overwrite: bool = False
    encoder: str = "auto"
    ffmpeg: str = "ffmpeg"

@dataclass(frozen=True)
class ProgressUpdate:
    phase: str
    processed: int
    total: int
    filename: str | None = None
    encoder: str | None = None
    warnings: int = 0

@dataclass
class ProcessingResult:
    run_status: str
    output_dir: Path
    video_path: Path | None = None
    report_path: Path | None = None
    frame_count: int = 0
    encoder: str | None = None

class CancellationToken:
    def __init__(self) -> None:
        self._event = Event()
    def cancel(self) -> None:
        self._event.set()
    @property
    def cancelled(self) -> bool:
        return self._event.is_set()
    def check(self) -> None:
        if self.cancelled:
            raise ProcessingCancelled("Processing cancelled")

class RawRenderer(Protocol):
    def inspect(self, path: Path) -> RawMetadata: ...
    def render_linear(self, path: Path, exposure_ev: float, options: RawRenderOptions) -> LinearFrame: ...

class LinearFrameProcessor(Protocol):
    def process(self, frame: LinearFrame) -> LinearFrame: ...

class OutputTransfer(Protocol):
    def to_rgb8(self, frame: LinearFrame) -> np.ndarray: ...

ProgressCallback = Callable[[ProgressUpdate], None]

class RawTLError(Exception):
    exit_code: int = 2

class InputError(RawTLError):
    exit_code = 2

class RawDecodeError(RawTLError):
    exit_code = 4

class ExposureError(RawTLError):
    exit_code = 5

class EncoderError(RawTLError):
    exit_code = 6

class VerificationError(RawTLError):
    exit_code = 7

class ProcessingCancelled(RawTLError):
    exit_code = 130
