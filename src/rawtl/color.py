"""Explicit, deterministic transfer encoding from linear sRGB."""
from __future__ import annotations
import numpy as np
from .types import LinearFrame, InputError

def srgb_oetf(linear: np.ndarray) -> np.ndarray:
    x=np.asarray(linear,dtype=np.float32)
    if not np.isfinite(x).all():
        raise InputError("nonfinite linear RGB cannot be encoded")
    x=np.clip(x,0.0,1.0)
    return np.where(x<=0.0031308,12.92*x,1.055*np.power(x,1/2.4)-0.055)

def pad_even_rgb8(arr: np.ndarray) -> np.ndarray:
    if arr.ndim!=3 or arr.shape[-1]!=3 or arr.dtype!=np.uint8:
        raise InputError("expected uint8 HxWx3 RGB image")
    h,w,_=arr.shape
    if h%2 or w%2:
        arr=np.pad(arr,((0,h%2),(0,w%2),(0,0)),mode="edge")
    return np.ascontiguousarray(arr)

class SRGBTransfer:
    def to_rgb8(self,frame: LinearFrame) -> np.ndarray:
        if frame.working_primaries!="linear-srgb":
            raise InputError(f"unsupported primaries {frame.working_primaries}")
        rgb=srgb_oetf(frame.rgb)
        return np.clip(np.floor(rgb*255+0.5),0,255).astype(np.uint8)

def output_new_clipping_fraction(before: LinearFrame,after: LinearFrame) -> float:
    """Approximation: compare already-saturated output masks; cannot recover clipped RAW."""
    if before.rgb.shape!=after.rgb.shape:
        raise InputError("render frame dimensions changed")
    old=(before.rgb>=0.98).any(axis=2)
    new=(after.rgb>=0.98).any(axis=2)
    return float(np.mean(new & ~old))
