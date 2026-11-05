"""Matched-background pairwise photometry between measured frames.

The per-frame median answers "what is the median object right now"; pairs of
the SAME tiles answer "how much did the light on this background change".
Only the second question estimates exposure. Every edge records its own
support, consensus and rejection reasons -- nothing is inferred silently.
"""
from __future__ import annotations
from dataclasses import dataclass, field
import math
from typing import Sequence
import numpy as np

from .measurement import FrameMeasurement
from .graph import GraphEdge

STRUCTURE_MAD_EV = .04
TILE_MIN_EV = math.log2(.002)
SAT_MAX = .01
CONSENSUS_EV = .06
MIN_INLIER_FRACTION = .6
MIN_QUADRANTS = 3
MIN_SUPPORT_TILES = 24
MIN_SUPPORT_FRACTION = .25
CHANNEL_CHANGE_EV = .08     # per-channel deviation from the common change

DEFAULT_LAGS = (1, 2, 4, 8)
LONG_LAG = 16
LONG_LAG_MIN_FRAMES = 32


def _quadrant_count(ids: np.ndarray, nx: int, ny: int) -> int:
    return len({(int(i % nx >= nx // 2), int(i // nx >= ny // 2)) for i in ids})


def pair_difference(m_i: FrameMeasurement, m_j: FrameMeasurement, *, lag: int = 1,
                    train: np.ndarray | None = None,
                    grid: tuple[int, int] = (12, 8)) -> GraphEdge:
    """Robust common log2-luminance difference from matched tiles.

    Tiles are matched by position and must be valid, unsaturated, above the
    noise floor in both frames, structurally stable (pixel-level log
    differences tight inside the tile) and colour-stable (each channel's tile
    mean tracks the common change). The common change is a median followed by
    Huber-style reweighting -- never two independent medians subtracted.
    """
    nx, ny = grid
    k = len(m_i.log_luminance)
    d = m_j.log_luminance - m_i.log_luminance
    usable = (m_i.valid & m_j.valid & np.isfinite(d) &
              (m_i.log_luminance > TILE_MIN_EV) & (m_j.log_luminance > TILE_MIN_EV) &
              (np.maximum(m_i.saturation_fraction, m_j.saturation_fraction) < SAT_MAX))
    rejects: dict[str, int] = {}
    if not usable.any():
        rejects["no_shared_valid_tiles"] = int(np.count_nonzero(m_i.valid & m_j.valid))
        return GraphEdge(0, 0, math.nan, 0., 0, 0., math.inf, 0, lag, rejects)
    structure_ok = np.zeros(k, dtype=bool)
    if m_i.log_pixel_samples is not None and m_j.log_pixel_samples is not None:
        diff = m_j.log_pixel_samples - m_i.log_pixel_samples
        with np.errstate(invalid="ignore"):
            loc = np.nanmedian(diff, axis=1)
            shape = 1.4826 * np.nanmedian(np.abs(diff - loc[:, None]), axis=1)
        structure_ok = np.nan_to_num(shape, nan=math.inf) < STRUCTURE_MAD_EV + 1e-4
        _rejected = int(np.count_nonzero(usable & ~structure_ok))
        if _rejected:
            rejects["structure_mad"] = _rejected
    else:
        structure_ok = usable
    colour_ok = np.ones(k, dtype=bool)
    if m_i.log_channel_means is not None and m_j.log_channel_means is not None:
        channel_delta = m_j.log_channel_means - m_i.log_channel_means
        # Colour stability = the three channel changes agree with each other.
        # A common luminance change moves all channels together; a colour-cast
        # or reflectance change moves them apart.
        with np.errstate(invalid="ignore"):
            spread = (np.nanmax(channel_delta, axis=1)
                      - np.nanmin(channel_delta, axis=1))
            colour_ok = np.nan_to_num(spread, nan=math.inf) <= CHANNEL_CHANGE_EV + 1e-4
        _rejected = int(np.count_nonzero(usable & ~colour_ok))
        if _rejected:
            rejects["colour_change"] = _rejected
    good = usable & structure_ok & colour_ok
    if train is not None:
        _rejected = int(np.count_nonzero(good & ~train))
        if _rejected:
            rejects["outside_reference_support"] = _rejected
        good = good & train
    needed = max(MIN_SUPPORT_TILES, math.ceil(MIN_SUPPORT_FRACTION * k))
    support = int(np.count_nonzero(good))
    if support < needed:
        rejects["insufficient_support"] = support
        return GraphEdge(0, 0, math.nan, 0., support, 0., math.inf, 0, lag, rejects)
    values = d[good]
    med = float(np.median(values))
    scale = float(1.4826 * np.median(np.abs(values - med)))
    for _ in range(4):
        w = np.minimum(1., .03 / np.maximum(np.abs(values - med), 1e-8))
        med = float(np.sum(w * values) / w.sum())
    inliers = np.flatnonzero(good & (np.abs(d - med) <= CONSENSUS_EV))
    fraction = len(inliers) / support if support else 0.
    quadrants = _quadrant_count(inliers, nx, ny)
    weight = min(support / 200., 1.) / (.01 ** 2 + scale ** 2)
    edge = GraphEdge(0, 0, med, weight, support, fraction, scale, quadrants, lag, rejects)
    return edge


def build_edges(measurements: Sequence[FrameMeasurement], *,
                boundaries: Sequence[int] = (),
                train: np.ndarray | None = None,
                grid: tuple[int, int] = (12, 8),
                lags: Sequence[int] = DEFAULT_LAGS) -> tuple[list[GraphEdge], dict]:
    """Edges between frames of the same continuous run at several lags.

    boundaries are zero-based frame indices starting a new run (confirmed
    capture-time gaps or scene cuts): no edge ever crosses one.
    """
    n = len(measurements)
    stop = set(int(b) for b in boundaries if 0 < int(b) < n)
    active_lags = list(lags)
    if n >= LONG_LAG_MIN_FRAMES and LONG_LAG not in active_lags:
        active_lags = sorted(active_lags + [LONG_LAG])
    edges: list[GraphEdge] = []
    stats = {"candidate_edges": 0, "rejected_edges": 0,
             "reject_reasons": {}, "by_lag": {str(lag): {"kept": 0, "rejected": 0}
                                              for lag in active_lags}}
    for lag in active_lags:
        for j in range(lag, n):
            i = j - lag
            if any(start in stop for start in range(i + 1, j + 1)):
                continue
            stats["candidate_edges"] += 1
            edge = pair_difference(measurements[i], measurements[j], lag=lag,
                                   train=train, grid=grid)
            if not math.isfinite(edge.delta_ev) or not edge.reliable:
                stats["rejected_edges"] += 1
                stats["by_lag"][str(lag)]["rejected"] += 1
                for reason in edge.rejects:
                    stats["reject_reasons"][reason] = stats["reject_reasons"].get(reason, 0) + 1
                continue
            edges.append(GraphEdge(i, j, edge.delta_ev, edge.weight, edge.support,
                                   edge.consensus_fraction, edge.spatial_mad_ev,
                                   edge.quadrants, lag, edge.rejects))
            stats["by_lag"][str(lag)]["kept"] += 1
    return edges, stats


def persistent_background(measurements: Sequence[FrameMeasurement],
                          brightness: np.ndarray, *,
                          min_valid_fraction: float = .8,
                          max_spread_p90_ev: float = .1) -> np.ndarray:
    """Tiles whose offset from the sequence brightness is stable over time.

    A background surface under changing exposure keeps x[t,k] - b[t]
    approximately constant; an object that arrives, leaves or crosses the tile
    breaks that relation even when its own brightness is self-consistent.
    """
    n = len(measurements)
    k = len(measurements[0].log_luminance)
    valid = np.zeros((n, k), dtype=bool)
    x = np.zeros((n, k), dtype=np.float64)
    for t, m in enumerate(measurements):
        valid[t] = m.valid
        x[t] = m.log_luminance
    valid_fraction = valid.mean(axis=0)
    persistent = np.zeros(k, dtype=bool)
    with np.errstate(invalid="ignore"):
        for tile in range(k):
            if not np.isfinite(brightness).any() or valid_fraction[tile] < min_valid_fraction:
                continue
            offset = x[:, tile] - brightness
            offset = offset[valid[:, tile] & np.isfinite(offset) & np.isfinite(brightness)]
            if offset.size < max(8, int(.5 * n)):
                continue
            centre = float(np.median(offset))
            spread = float(np.percentile(np.abs(offset - centre), 90))
            persistent[tile] = spread <= max_spread_p90_ev
    return persistent
