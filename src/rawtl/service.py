"""ProcessingService will orchestrate RAW/analysis/video in task T7."""
from .types import ProcessingRequest, InputError

class ProcessingService:
    def probe(self, input_dir, *, ffmpeg="ffmpeg"):
        raise InputError("RAW probe integration pending T2")
    def run(self, request: ProcessingRequest, on_progress=None, cancel=None):
        raise InputError("Processing integration pending T7")
