"""F4: matched-background pair photometry and the sparse exposure graph."""
from __future__ import annotations
import numpy as np
import pytest

from rawtl.exposure.measurement import FrameMeasurement
from rawtl.exposure.pairwise import build_edges, pair_difference, persistent_background
from rawtl.exposure.graph import GraphEdge, solve_exposure_graph

NX, NY = 12, 8
K = NX * NY
CENTERS = np.array([[(i + .5) / NX, (j + .5) / NY]
                    for j in range(NY) for i in range(NX)])


def make_measurement(tiles: np.ndarray, samples: bool = True,
                     valid: np.ndarray | None = None) -> FrameMeasurement:
    tiles = np.asarray(tiles, dtype=np.float64)
    pixel = (np.repeat(tiles[:, None], 25, axis=1)
             .astype(np.float32) if samples else None)
    channels = (np.repeat(tiles[:, None], 3, axis=1) if samples else None)
    return FrameMeasurement(tiles, np.ones(K, bool) if valid is None else valid,
                            CENTERS, np.zeros(K), 480, 360, "exact",
                            pixel, channels)


def static_background(levels, reflectance=None):
    reflectance = np.linspace(-.3, .3, K) if reflectance is None else reflectance
    return np.asarray(levels)[:, None] + reflectance[None, :]


def test_pair_difference_recovers_common_gain():
    a = make_measurement(static_background([0.]))
    b = make_measurement(static_background([.12]))
    edge = pair_difference(a, b)
    assert edge.reliable
    assert edge.delta_ev == pytest.approx(.12, abs=1e-6)
    assert edge.support == K


def test_pair_difference_ignores_moving_object():
    base = static_background([0.] * 2)
    tiles_a = base[0].copy()
    tiles_b = base[1].copy()
    tiles_b[:20] += 1.5     # object covers 20 of 96 tiles in frame b only
    edge = pair_difference(make_measurement(tiles_a), make_measurement(tiles_b))
    assert edge.reliable
    # Synthetic uniform-tile samples cannot show structure, so the object only
    # loses to the robust consensus; the residual bias must stay tiny.
    assert edge.delta_ev == pytest.approx(0., abs=.02)


def test_pair_difference_rejects_half_scene_change():
    a = make_measurement(static_background([0.])[0])
    b_tiles = static_background([0.])[0].copy()
    b_tiles[:K // 2] += 1.0
    edge = pair_difference(a, make_measurement(b_tiles))
    assert not edge.reliable


def test_pair_difference_excluded_rois_reduce_support():
    a = make_measurement(static_background([0.]))
    b = make_measurement(static_background([.1]))
    train = np.ones(K, bool)
    train[::2] = False
    edge = pair_difference(a, b, train=train)
    assert edge.support == K // 2
    assert edge.delta_ev == pytest.approx(.1, abs=1e-6)


def test_build_edges_respects_boundaries_and_lags():
    levels = .05 * (-1) ** np.arange(12)
    measurements = [make_measurement(x) for x in static_background(levels)]
    edges, stats = build_edges(measurements, boundaries=(6,))
    assert all(not (e.i < 6 <= e.j) for e in edges)
    assert stats["by_lag"]["1"]["kept"] == 10  # 5 + 5 within two runs


def test_graph_solve_recovers_known_brightness():
    truth = np.array([0., .1, .18, .05, -.12, -.2, .02, .3])
    rng = np.random.default_rng(3)
    edges = []
    for i in range(len(truth)):
        for j in range(i + 1, len(truth)):
            noise = rng.normal(0, .004)
            edges.append(GraphEdge(i, j, truth[j] - truth[i] + noise,
                                   1., K, 1., .004, 4, j - i))
    result = solve_exposure_graph(len(truth), edges)
    assert result.converged
    fitted = result.brightness - result.brightness[0]
    assert np.allclose(fitted, truth, atol=.01)
    assert result.edge_p95_abs_residual_ev is not None
    assert result.edge_p95_abs_residual_ev < .02


def test_graph_disconnected_components_keep_separate_gauges():
    edges = [GraphEdge(0, 1, .5, 1., K, 1., .0, 4, 1),
             GraphEdge(2, 3, -.2, 1., K, 1., .0, 4, 1)]
    result = solve_exposure_graph(4, edges)
    assert len(result.components) == 2
    assert result.brightness[0] == 0. and result.brightness[2] == 0.
    assert result.brightness[1] == pytest.approx(.5)
    assert result.brightness[3] == pytest.approx(-.2)


def test_graph_lone_frame_stays_unconstrained():
    edges = [GraphEdge(0, 2, .1, 1., K, 1., .0, 4, 2)]
    result = solve_exposure_graph(4, edges)
    assert np.isnan(result.brightness[1])   # no evidence: NaN, never invented
    assert result.component[1] == -1 or len(result.components) > 1


def test_graph_no_edges_all_unconstrained():
    result = solve_exposure_graph(5, [])
    assert np.all(np.isnan(result.brightness))


def test_persistent_background_rejects_visiting_object():
    levels = np.zeros(20)
    tiles = static_background(levels)
    for t in range(10):        # object sits on tiles 0..9 for half the run
        tiles[t, :10] += 1.0
    brightness = np.zeros(20)
    measurements = [make_measurement(x) for x in tiles]
    persistent = persistent_background(measurements, brightness)
    assert not persistent[:10].any()
    assert persistent[10:].all()


def test_persistent_background_keeps_stable_surfaces():
    levels = np.linspace(0, .8, 20)   # real brightness ramp
    tiles = static_background(levels)
    brightness = levels.copy()
    measurements = [make_measurement(x) for x in tiles]
    persistent = persistent_background(measurements, brightness)
    assert persistent.all()
