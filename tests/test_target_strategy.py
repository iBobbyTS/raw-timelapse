"""F5: target strategy separation -- measured b / desired s / reference scope."""
from __future__ import annotations
import numpy as np
import pytest

from rawtl.exposure.estimation import estimate_exposure
from rawtl.exposure.measurement import FrameMeasurement

CENTERS = np.array([[(i + .5) / 12, (j + .5) / 8] for j in range(8) for i in range(12)])
K = 96


def frames(rows, valid=None):
    out = []
    for i, row in enumerate(rows):
        v = np.ones(K, dtype=bool) if valid is None else valid[i]
        out.append(FrameMeasurement(np.asarray(row, dtype=float), v, CENTERS,
                                    np.zeros(K), 320, 240, "approximate"))
    return out


def spliced_levels(n=60, cut=30, jump=1.0):
    rng = np.random.default_rng(5)
    offsets = rng.normal(0, .05, K)
    x = np.zeros((n, K))
    x[:cut] = offsets[None, :]
    x[cut:] = offsets[None, :] + jump * (.8 + .4 * np.arange(K) / K)
    x += (.05 * (-1.) ** np.arange(n))[:, None]
    return x


def test_measured_and_desired_levels_are_reported():
    x = spliced_levels()
    e = estimate_exposure(frames(x), brightness="smooth")
    measured = [f.measured_level_ev for f in e.frames]
    desired = [f.desired_level_ev for f in e.frames]
    assert all(v is not None for v in measured)
    assert all(v is not None for v in desired)
    # requested = strength * (desired - measured) with strength 1
    for f in e.frames:
        assert f.requested_ev - (f.desired_level_ev - f.measured_level_ev) < 1e-9


def test_uniform_global_flattens_across_boundary_segment_does_not():
    x = spliced_levels()
    glob = estimate_exposure(frames(x), brightness="uniform", uniform_reference="global")
    seg = estimate_exposure(frames(x), brightness="uniform", uniform_reference="segment")
    g = np.array([f.applied_ev for f in glob.frames])
    s = np.array([f.applied_ev for f in seg.frames])
    level = np.median(x, axis=1)          # both blocks have near-zero-median offsets
    out_g = level + g
    out_s = level + s
    # global: one constant level for the whole sequence
    assert out_g.max() - out_g.min() < .1
    # segment: each continuous run constant, the ~1 EV splice jump INTENTIONALLY kept
    assert abs((out_s[30:].mean() - out_s[:30].mean()) -
               (level[30:].mean() - level[:30].mean())) < .05
    assert (out_s[:30].max() - out_s[:30].min()) < .1
    assert (out_s[31:].max() - out_s[31:].min()) < .1
    # and the difference between the two modes is the intent itself
    assert abs(np.median(g[:30]) - np.median(g[30:])) > .5
    assert abs(np.median(s[:30]) - np.median(s[30:])) < .05


def test_target_provenance_is_auditable():
    x = spliced_levels()
    e = estimate_exposure(frames(x), brightness="uniform", uniform_reference="segment")
    prov = e.pairwise_summary["target_provenance"]
    assert prov["intent"] == "uniform"
    assert prov["reference_scope"] == "segment"
    assert prov["defined_before_iteration"] is True
    g = estimate_exposure(frames(x), brightness="smooth")
    assert "period=60" in g.pairwise_summary["target_provenance"]["desired"]


def test_invalid_reference_scope_rejected():
    x = spliced_levels(n=12)
    with pytest.raises(Exception, match="uniform_reference"):
        estimate_exposure(frames(x), brightness="uniform", uniform_reference="auto")


def test_confirmed_step_semantics_differ_by_intent():
    """smooth preserves an intentional step; uniform cancels it by definition."""
    x = spliced_levels(n=60, cut=30, jump=1.0)
    from rawtl.types import ExposureStep
    smooth = estimate_exposure(frames(x), brightness="smooth",
                               steps=[ExposureStep(30, 1.0)])
    uniform = estimate_exposure(frames(x), brightness="uniform",
                                steps=[ExposureStep(30, 1.0)],
                                uniform_reference="global")
    sm = np.array([f.applied_ev for f in smooth.frames])
    un = np.array([f.applied_ev for f in uniform.frames])
    # smooth: the step stays in the output (correction does not cancel it)
    assert abs(np.median(sm[:10]) - np.median(sm[-10:])) < .1
    # uniform: constant brightness means the step is flattened too (means
    # average out the +-0.05 alternating flicker)
    assert abs((un[:10] + np.median(x[:10], axis=1)).mean() -
               (un[-10:] + np.median(x[-10:], axis=1)).mean()) < .1


def test_no_lowpass_on_final_correction():
    """Anti-flicker correction itself alternates; no smoothing may be applied
    to the correction curve after the fact."""
    n = 40
    rng = np.random.default_rng(2)
    offsets = rng.normal(0, .05, K)
    flicker = .1 * (-1.) ** np.arange(n)
    x = offsets[None, :] + flicker[:, None]
    e = estimate_exposure(frames(x), brightness="smooth")
    applied = np.array([f.applied_ev for f in e.frames])
    even, odd = applied[0::2].mean(), applied[1::2].mean()
    assert odd - even > .15          # frame-by-frame alternation preserved
