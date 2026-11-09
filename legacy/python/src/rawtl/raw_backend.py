"""Fixed-settings LibRaw/rawpy renderer with linear sRGB output."""
from __future__ import annotations

import math
import json
import shutil
import subprocess
from pathlib import Path
from typing import Any
import numpy as np
from .types import LinearFrame, RawDecodeError, RawMetadata, RawRenderOptions

class RawpyRenderer:
    """RAII-style single-frame rendering. The original RAW is never modified."""

    def __init__(self, rawpy_module: Any | None = None):
        if rawpy_module is None:
            try:
                import rawpy as rawpy_module
            except ImportError as exc:
                raise RawDecodeError("rawpy is required; install project dependencies") from exc
        self.rawpy = rawpy_module

    @staticmethod
    def _wb(raw_wb: Any, three_color: bool=False) -> tuple[float, float, float, float] | None:
        try:
            vals=tuple(float(x) for x in raw_wb)
            if len(vals)!=4 or not all(math.isfinite(x) and x>0 for x in vals[:3]):
                return None
            if not math.isfinite(vals[3]) or vals[3]<=0:
                # 3-colour Bayer files sometimes leave the unused G2 coefficient at 0.
                # Infer G2=G only when the decoder reports three actual colours.
                if not three_color:
                    return None
                vals=(vals[0],vals[1],vals[2],vals[1])
            return vals
        except (ValueError,TypeError):
            return None

    def inspect(self,path: Path) -> RawMetadata:
        try:
            with self.rawpy.imread(str(path)) as raw:
                # rawpy 0.27 does not guarantee a .metadata make/model property.
                optional_metadata=getattr(raw,"metadata",None)
                camera=" ".join(s for s in [
                    str(getattr(optional_metadata,"make","") or "").strip(),
                    str(getattr(optional_metadata,"model","") or "").strip()
                ] if s).strip() or "UNKNOWN_CAMERA"
                try:
                    pattern=raw.raw_pattern
                    matrix=raw.color_matrix
                    fingerprint={
                        "raw_type":str(raw.raw_type),
                        "num_colors":int(raw.num_colors),
                        "color_desc":str(raw.color_desc),
                        "raw_pattern":None if pattern is None else np.asarray(pattern).tolist(),
                        "matrix":np.asarray(matrix,dtype=np.float64).round(5).tolist()
                    }
                    signature=json.dumps(fingerprint,sort_keys=True)
                except (AttributeError,ValueError,TypeError):
                    signature=""
                return RawMetadata(camera,
                    int(raw.sizes.width),int(raw.sizes.height),int(raw.sizes.flip),
                    self._wb(raw.camera_whitebalance,getattr(raw,"num_colors",4)==3),
                    self._wb(raw.daylight_whitebalance,getattr(raw,"num_colors",4)==3),
                    Path(path).suffix.lower(),signature)
        except Exception as exc:
            raise RawDecodeError(f"cannot inspect {path}: {exc}") from exc

    def camera_models(self, paths: list[Path]) -> dict[str,str]:
        """Optional ExifTool batch for accurate camera make/model without requiring it."""
        executable=shutil.which("exiftool")
        if not executable:
            return {}
        result={}
        for start in range(0,len(paths),48):
            chunk=paths[start:start+48]
            try:
                p=subprocess.run(
                    [executable,"-fast2","-j","-Make","-Model",
                     *[str(x) for x in chunk]],
                    capture_output=True,text=True,timeout=120,check=True)
                for item in json.loads(p.stdout):
                    camera=" ".join(str(item.get(key,"") or "").strip()
                        for key in ("Make","Model")).strip()
                    if camera:
                        result[str(Path(item["SourceFile"]).resolve())]=camera
            except (OSError,ValueError,KeyError,subprocess.SubprocessError):
                # Model identity stays explicitly unknown rather than inventing it.
                continue
        return result

    @staticmethod
    def fixed_wb(metadata: RawMetadata) -> tuple[float,float,float,float]:
        if metadata.camera_wb is not None:
            return metadata.camera_wb
        if metadata.daylight_wb is not None:
            return metadata.daylight_wb
        raise RawDecodeError("first RAW has no valid four-channel camera/daylight WB")

    # LibRaw realises exp_shift only inside its documented [0.25, 8.0] range
    # (-2..+3 EV) and silently clamps outside it (probe 2026-11-06 on a real
    # DNG: requested -4 EV renders as -1.99, +4 as +2.98). Any finite EV is
    # realised by composing the in-range exp_shift with an exact post-decode
    # linear gain; on the dimming side the composition adds 0.0000 EV of
    # measured error beyond the in-range path's own bias.
    EXP_SHIFT_RANGE=(0.25,8.0)

    def render_linear(self,path: Path,exposure_ev: float,
        options: RawRenderOptions) -> LinearFrame:
        if options.color_transform_id!="libraw_builtin_srgb" or options.transfer_id!="linear":
            raise RawDecodeError("V1 supports only libraw_builtin_srgb / linear rendering")
        if not math.isfinite(exposure_ev):
            raise RawDecodeError(f"exposure EV must be finite: {exposure_ev}")
        gain=2.0**exposure_ev
        lo,hi=self.EXP_SHIFT_RANGE
        shift=float(min(max(gain,lo),hi))
        extra=gain/shift  # exactly 1.0 whenever the request is inside the honoured range
        wb=options.fixed_white_balance
        if len(wb)!=4 or any(not math.isfinite(x) or x <= 0 for x in wb):
            raise RawDecodeError("invalid fixed four-channel WB")
        try:
            with self.rawpy.imread(str(path)) as raw:
                a=raw.postprocess(
                    output_color=self.rawpy.ColorSpace.sRGB,
                    output_bps=16, gamma=(1,1),
                    no_auto_bright=True, adjust_maximum_thr=0.0,
                    use_auto_wb=False, use_camera_wb=False, user_wb=list(wb),
                    bright=1.0, highlight_mode=self.rawpy.HighlightMode.Clip,
                    exp_shift=shift, exp_preserve_highlights=0.0)
                if a.dtype!=np.uint16 or a.ndim!=3 or a.shape[2]!=3:
                    raise ValueError(f"unexpected processed format {a.shape} {a.dtype}")
                rgb=a.astype(np.float32)
                rgb *= np.float32(1/65535)
                if extra!=1.:
                    rgb *= np.float32(extra)
                    # Saturation must describe the delivered linear data, so it
                    # is recomputed after the post-gain (may grow AND shrink).
                    saturated=(rgb>=np.float32(64224/65535)).any(axis=2)
                else:
                    saturated=(a >= 64224).any(axis=2)  # ~0.98*65535; proxy, not sensor saturation
                return LinearFrame(rgb,saturated,"approximate",1/65535,True,"linear-srgb")
        except RawDecodeError:
            raise
        except Exception as exc:
            raise RawDecodeError(f"linear RAW rendering failed for {path}: {exc}") from exc

    def versions(self) -> dict[str,str]:
        return {"rawpy":str(getattr(self.rawpy,"__version__","unknown")),
            "libraw":str(getattr(self.rawpy,"libraw_version","unknown"))}
