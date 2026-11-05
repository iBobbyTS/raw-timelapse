"""Fixed-camera paired photometry on decoded frames (pure NumPy/SciPy).

This module measures WHAT changed between frames of a decoded video: the
common (spatially consensus) brightness change and the local residual spread.
It never decides whether a physical lighting change was intentional; that is a
policy question for callers. All functions accept plain arrays so the
photometry is unit-testable without FFmpeg.
"""
from __future__ import annotations
from dataclasses import dataclass
import math
import numpy as np
from scipy.ndimage import gaussian_filter

REC709_LUMA = np.array([.2126, .7152, .0722], dtype=np.float32)

# Candidate screening thresholds; engineering values calibrated on the
# 119-frame indoor benchmark, not universal perceptual constants.
PAIR_SHAPE_MAD_EV = .04
PAIR_TILE_MIN_EV = math.log2(.002)
PAIR_SAT_MAX = .01
PAIR_CONSENSUS_EV = .06
PAIR_MIN_INLIER_FRACTION = .6
PAIR_MIN_QUADRANTS = 3
CANDIDATE_STEP_EV = .05
SHORT_CONTEXT_FRAMES = 24

SUPPORTED_TRANSFERS = ("srgb", "bt709")
HDR_LOG_TRANSFERS = ("smpte2084", "arib-std-b67", "log", "log10", "log16",
                     "hlg", "pq", "slog", "slog2", "slog3", "vlog", "clog")


def inverse_transfer(x: np.ndarray, transfer: str) -> np.ndarray:
    """Decode gamma-encoded code values to linear light for supported SDR transfers."""
    if transfer == "srgb":
        x = np.asarray(x, dtype=np.float32)
        return np.where(x <= .04045, x / np.float32(12.92),
                        ((x + np.float32(.055)) / np.float32(1.055)) ** np.float32(2.4))
    if transfer == "bt709":
        x = np.asarray(x, dtype=np.float32)
        return np.where(x < np.float32(.081), x / np.float32(4.5),
                        ((x + np.float32(.099)) / np.float32(1.099)) ** np.float32(1 / .45))
    raise ValueError(
        f"unsupported or unspecified transfer {transfer!r}; specify one of "
        f"{SUPPORTED_TRANSFERS} explicitly -- never infer it from the pixel format")


def classify_transfer_tag(tag: str | None) -> str:
    """Map an ffprobe color_transfer tag to a supported id or raise for HDR/Log."""
    if tag is None or tag in ("unknown", "unspecified"):
        raise ValueError("UNSPECIFIED_TRANSFER: stream has no transfer tag; "
                         "pass an explicit transfer assumption")
    t = tag.lower()
    if any(h in t for h in HDR_LOG_TRANSFERS):
        raise ValueError(f"UNSUPPORTED_TRANSFER: {tag} is HDR/Log; this SDR audit "
                         "cannot make absolute EV statements about it")
    if t in ("srgb", "iec61966-2-1"):
        return "srgb"
    if t in ("bt709", "bt.709", "1"):
        return "bt709"
    raise ValueError(f"UNSUPPORTED_TRANSFER: {tag}")


def linear_luminance(rgb16: np.ndarray, transfer: str,
                     lut: np.ndarray | None = None) -> np.ndarray:
    """uint16 HxWx3 gamma-coded RGB -> linear luminance (float32)."""
    if rgb16.dtype != np.uint16 or rgb16.ndim != 3 or rgb16.shape[2] != 3:
        raise ValueError("expected HxWx3 uint16 RGB")
    if lut is None:
        lut = inverse_transfer(np.arange(65536, dtype=np.float32) / np.float32(65535.), transfer)
    y = np.zeros(rgb16.shape[:2], dtype=np.float32)
    for channel, coefficient in enumerate(REC709_LUMA):
        y += coefficient * lut[rgb16[..., channel]]
    return y


def structure_blur(image: np.ndarray) -> np.ndarray:
    """Spatial pre-blur used only for structure-difference testing.

    A fixed 5x5 Gaussian (sigma 1.2), matching the calibrated reference
    detector kernel; truncate is chosen so the filter radius is exactly 2.
    """
    return gaussian_filter(np.asarray(image, dtype=np.float32), sigma=1.2,
                           mode="mirror", truncate=2.08)


def tile_grid(array: np.ndarray, nx: int, ny: int) -> np.ndarray:
    """Reshape HxW into (ny*nx, tile_h*tile_w); row-major like the reference grid."""
    h, w = array.shape
    if h % ny or w % nx:
        raise ValueError(f"grid {nx}x{ny} must divide {w}x{h}")
    th, tw = h // ny, w // nx
    return (array.reshape(ny, th, nx, tw)
            .transpose(0, 2, 1, 3).reshape(nx * ny, th * tw))


@dataclass
class TileStats:
    log_mean: np.ndarray          # float64[K] log2 trimmed tile means
    log_pixels: np.ndarray        # float64[K, P] log2 blurred pixel samples
    saturation_fraction: np.ndarray  # float64[K]


def tile_statistics(y: np.ndarray, saturation: np.ndarray, nx: int = 24,
                    ny: int = 18) -> TileStats:
    samples = tile_grid(y, nx, ny)
    order = np.sort(samples, axis=1)
    k = max(1, int(samples.shape[1] * .05))
    means = order[:, k:-k].mean(axis=1, dtype=np.float64)
    blurred = structure_blur(y)
    log_pixels = tile_grid(np.log2(np.maximum(blurred, 1e-6)), nx, ny)
    return TileStats(np.log2(np.maximum(means, 1e-12)), log_pixels,
                     tile_grid(saturation, nx, ny).mean(axis=1, dtype=np.float64))


@dataclass
class PairMetric:
    common_ev: float | None
    spatial_mad_ev: float | None
    valid_tiles: int
    consensus_tiles: int
    consensus_fraction: float | None
    quadrants: int
    quality: str
    local_p90_ev: float | None


def pair_metric(now: TileStats, previous: TileStats, nx: int = 24, ny: int = 18,
                train: np.ndarray | None = None) -> tuple[PairMetric, np.ndarray, np.ndarray]:
    """Compare consecutive frames; matched-support differences only.

    A tile is usable when its pixel-level structure change is small, it is
    neither near-black nor near-saturated in either frame, and (optionally)
    it belongs to the fitting mask. The common change is the consensus median
    of per-tile differences; widespread disagreement demotes the pair to
    LOCAL_OR_MIXED_CHANGE instead of a confident number.
    """
    diff = now.log_pixels - previous.log_pixels
    loc = np.median(diff, axis=1)
    shape = 1.4826 * np.median(np.abs(diff - loc[:, None]), axis=1)
    good = ((shape < PAIR_SHAPE_MAD_EV) & (now.log_mean > PAIR_TILE_MIN_EV) &
            (previous.log_mean > PAIR_TILE_MIN_EV) &
            (np.maximum(now.saturation_fraction, previous.saturation_fraction) < PAIR_SAT_MAX))
    if train is not None:
        good &= train
    d = now.log_mean - previous.log_mean
    needed = max(24, math.ceil(.25 * len(d)))
    if int(good.sum()) < needed:
        return (PairMetric(None, None, int(good.sum()), 0, None, 0,
                           "INSUFFICIENT_SUPPORT", None), d, good)
    med = float(np.median(d[good]))
    mad = float(1.4826 * np.median(np.abs(d[good] - med)))
    inliers = good & (np.abs(d - med) <= PAIR_CONSENSUS_EV)
    ids = np.flatnonzero(inliers)
    quadrants = len({(int(i % nx >= nx // 2), int(i // nx >= ny // 2)) for i in ids})
    fraction = float(inliers.sum() / good.sum())
    quality = ("RELIABLE_COMMON_CHANGE"
               if fraction >= PAIR_MIN_INLIER_FRACTION and mad <= PAIR_CONSENSUS_EV
               and quadrants >= PAIR_MIN_QUADRANTS else "LOCAL_OR_MIXED_CHANGE")
    p90 = float(np.percentile(np.abs(d[good] - med), 90))
    return (PairMetric(med, mad, int(good.sum()), int(inliers.sum()), fraction,
                       quadrants, quality, p90), d, good)


def temporal_summary(pair_values: list[float | None]) -> dict:
    """Aggregate reliable common changes; missing pairs stay missing."""
    a = np.array([np.nan if v is None else v for v in pair_values], float)
    v = a[np.isfinite(a)]
    curvature = .5 * (a[:-1] - a[1:])
    curvature = curvature[np.isfinite(curvature)]
    lag2 = .25 * (a[:-2] - 2 * a[1:-1] + a[2:])
    lag2 = lag2[np.isfinite(lag2)]
    return {
        "pairs": len(a),
        "usable_pairs": len(v),
        "median_abs_step_ev": float(np.median(np.abs(v))) if len(v) else None,
        "p95_abs_step_ev": float(np.percentile(np.abs(v), 95)) if len(v) else None,
        "max_abs_step_ev": float(np.max(np.abs(v))) if len(v) else None,
        "curvature_rms_ev": float(np.sqrt(np.mean(curvature ** 2))) if len(curvature) else None,
        "lag2_residual_rms_ev": float(np.sqrt(np.mean(lag2 ** 2))) if len(lag2) else None,
        "steps_over_0_02_ev": int(np.sum(np.abs(v) > .02)),
        "steps_over_0_05_ev": int(np.sum(np.abs(v) > .05)),
    }


def roi_level(y: np.ndarray, box: tuple[float, float, float, float]) -> float | None:
    """Trimmed linear mean of one normalized ROI [x0,y0,x1,y1] in log2 EV."""
    x0, y0, x1, y1 = box
    h, w = y.shape
    a = np.sort(y[int(y0 * h):int(y1 * h), int(x0 * w):int(x1 * w)].ravel())
    s = max(1, int(.05 * len(a)))
    mean = a[s:-s].mean(dtype=np.float64)
    return float(np.log2(mean)) if mean > .0001 else None


def roi_series_metrics(levels: list[float | None]) -> dict:
    r = np.array([np.nan if x is None else x for x in levels], float)
    steps = np.diff(r)
    hp3 = r[1:-1] - .5 * (r[:-2] + r[2:])
    return {
        "p95_step_ev": float(np.nanpercentile(np.abs(steps), 95)) if len(steps) else None,
        "hp3_rms_ev": float(np.sqrt(np.nanmean(hp3 * hp3))) if len(hp3) else None,
        "range_ev": float(np.nanmax(r) - np.nanmin(r)) if len(r) else None,
        "observed_frames": int(np.isfinite(r).sum()),
    }
