"""Fixed-grid linear-light robust photometry; no global mean shortcut."""
from __future__ import annotations
from dataclasses import dataclass
from typing import Sequence
import math
import numpy as np
from PIL import Image
from ..types import LinearFrame, ExposureError

@dataclass
class FrameMeasurement:
    log_luminance: np.ndarray        # float64[K], invalid = nan
    valid: np.ndarray                # bool[K]
    centers: np.ndarray              # float64[K,2] in unit ROI coordinates
    saturation_fraction: np.ndarray # float64[K]
    width: int
    height: int
    saturation_mask_quality: str

def _area_downsample(a: np.ndarray, width: int, height: int) -> np.ndarray:
    if (a.shape[1],a.shape[0])==(width,height):
        return np.asarray(a,dtype=np.float32)
    img=Image.fromarray(np.ascontiguousarray(a,dtype=np.float32))
    return np.asarray(img.resize((width,height),resample=Image.Resampling.BOX),dtype=np.float32)

def trimmed_linear_mean(a: np.ndarray, fraction: float = 0.05) -> float:
    """Do not clamp negative linear noise before summing; trim then average."""
    vals=np.asarray(a,dtype=np.float64).ravel()
    if vals.size==0 or not np.isfinite(vals).all():
        return float("nan")
    n=int(vals.size*fraction)
    if n:
        mid=np.partition(vals,(n,vals.size-n-1))[n:vals.size-n]
    else:
        mid=vals
    return float(np.mean(mid,dtype=np.float64))

def measure_frame(frame: LinearFrame,roi: tuple[float,float,float,float]=(0,0,1,1),
    *, max_edge: int=1024,grid: tuple[int,int]=(12,8)) -> FrameMeasurement:
    if frame.working_primaries!="linear-srgb":
        raise ExposureError("measurement requires linear-srgb working primaries")
    rgb=frame.rgb
    if rgb.ndim!=3 or rgb.shape[-1]!=3 or rgb.shape[0]<1 or rgb.shape[1]<1:
        raise ExposureError("expected HxWx3 linear RGB")
    h0,w0,_=rgb.shape
    scale=min(1.,max_edge/max(h0,w0))
    width=max(1,round(w0*scale)); height=max(1,round(h0*scale))
    # Float32 luminance is only an analysis intermediate; block statistics use float64.
    lum=(np.float32(.2126)*rgb[...,0]+np.float32(.7152)*rgb[...,1]+
        np.float32(.0722)*rgb[...,2])
    lum=_area_downsample(lum,width,height)
    mask=frame.saturation_mask
    if mask is None:
        # No sensor mask; clipped linear renderer provides only a conservative surrogate.
        mask=np.any(rgb>=.98,axis=2)
        quality="approximate"
    else:
        if mask.shape!=rgb.shape[:2]:
            raise ExposureError("saturation mask geometry mismatch")
        quality=frame.saturation_mask_quality
    occupancy=_area_downsample(np.asarray(mask,dtype=np.float32),width,height)
    x,y,rw,rh=roi
    if any(not math.isfinite(v) for v in roi) or min(x,y)<0 or min(rw,rh)<=0 or x+rw>1 or y+rh>1:
        raise ExposureError("invalid ROI")
    x0=int(round(x*width)); y0=int(round(y*height))
    x1=int(round((x+rw)*width)); y1=int(round((y+rh)*height))
    w=x1-x0; h=y1-y0
    nx=min(grid[0],w//16);ny=min(grid[1],h//16)
    if nx<2 or ny<2 or nx*ny<8:
        raise ExposureError("ROI_TOO_SMALL: need >=8 tiles of at least 16x16 analysis pixels")
    xs=np.rint(np.linspace(x0,x1,nx+1)).astype(int)
    ys=np.rint(np.linspace(y0,y1,ny+1)).astype(int)
    centers=[]; vals=[]; valid=[]; saturated=[]
    floor=max(1e-5,8.*float(frame.quantum_linear))
    for j in range(ny):
        for i in range(nx):
            b=lum[ys[j]:ys[j+1],xs[i]:xs[i+1]]
            sat=occupancy[ys[j]:ys[j+1],xs[i]:xs[i+1]]
            coverage=float(np.mean(sat,dtype=np.float64))
            mean=trimmed_linear_mean(b)
            ok=(b.size>=256 and np.isfinite(b).all() and np.isfinite(coverage) and
                coverage<=.005 and math.isfinite(mean) and mean>floor)
            valid.append(ok)
            vals.append(math.log2(mean) if ok else np.nan)
            saturated.append(coverage)
            centers.append(((i+.5)/nx,(j+.5)/ny))
    return FrameMeasurement(np.asarray(vals,dtype=np.float64),
        np.asarray(valid,dtype=bool),np.asarray(centers,dtype=np.float64),
        np.asarray(saturated,dtype=np.float64),width,height,quality)
