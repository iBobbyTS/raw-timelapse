"""F6: output-domain target verification, single-frame gate, frozen feedback."""
from __future__ import annotations
import numpy as np
import pytest

from rawtl.exposure.estimation import estimate_exposure
from rawtl.exposure.measurement import FrameMeasurement
from rawtl.exposure.validation import (
    ValidationSummary, evaluate_render, feedback_update, probe_gain,
)

NX, NY = 12, 8
K = NX * NY
CENTERS = np.array([[(i + .5) / NX, (j + .5) / NY] for j in range(NY) for i in range(NX)])
RNG = np.random.default_rng(11)


def frames(rows):
    return [FrameMeasurement(np.asarray(row, dtype=float), np.ones(K, bool), CENTERS,
                             np.zeros(K), 320, 240, "approximate") for row in rows]


def base_sequence(n=30):
    offsets = RNG.normal(0, .05, K)
    flicker = .08 * (-1.) ** np.arange(n)
    return offsets[None, :] + flicker[:, None]


def _rendered_from(exposure, x, gain=lambda ev: ev, spoil_frame=None, spoil_ev=.5):
    """Simulate a renderer: applies the correction (times a response gain),
    optionally ruins ONE frame."""
    out = []
    for t, est in enumerate(exposure.frames):
        ev = est.applied_ev if est.applied_ev is not None else 0.
        applied = gain(ev)
        if spoil_frame is not None and t == spoil_frame:
            applied += spoil_ev
        out.append(x[t] + applied)
    return frames(out)


def test_correct_execution_passes_with_small_error():
    x = base_sequence()
    exposure = estimate_exposure(frames(x), period=60)
    rendered = _rendered_from(exposure, x)
    summary = evaluate_render(exposure, rendered)
    assert summary.passed
    assert summary.median_abs_error_ev < .01


def test_single_severe_frame_fails_despite_clean_percentiles():
    x = base_sequence()
    exposure = estimate_exposure(frames(x), period=60)
    rendered = _rendered_from(exposure, x, spoil_frame=15, spoil_ev=.5)
    summary = evaluate_render(exposure, rendered)
    assert not summary.passed
    assert 15 in summary.outlier_frames
    # percentiles alone stay clean -- the gate is what catches it
    assert summary.p95_abs_error_ev < .05


def test_under_execution_is_detected_and_feedback_converges():
    x = base_sequence()
    exposure = estimate_exposure(frames(x), period=60)
    rendered = _rendered_from(exposure, x, gain=lambda ev: .8 * ev)  # 20% under
    summary = evaluate_render(exposure, rendered)
    assert not summary.passed
    # frozen desired levels: feedback moves applied EV, never the target
    desired_before = [f.desired_level_ev for f in exposure.frames]
    adjusted = feedback_update(exposure, summary)
    assert adjusted > 0
    assert [f.desired_level_ev for f in exposure.frames] == desired_before
    # a second honest render at the updated EVs shrinks the error further
    updated = evaluate_render(exposure, _rendered_from(exposure, x, gain=lambda ev: .8 * ev))
    assert updated.median_abs_error_ev < summary.median_abs_error_ev


def test_frozen_target_cannot_be_resmoothed_into_passing():
    """A wrong execution error does not disappear by 'iterating' the target:
    desired levels are immutable through feedback."""
    x = base_sequence()
    exposure = estimate_exposure(frames(x), period=60)
    before = [f.desired_level_ev for f in exposure.frames]
    for _ in range(3):
        summary = evaluate_render(exposure, _rendered_from(
            exposure, x, gain=lambda ev: .5 * ev))
        if summary.passed:
            break
        feedback_update(exposure, summary)
    assert [f.desired_level_ev for f in exposure.frames] == before


def test_unobservable_frames_do_not_silently_pass():
    x = base_sequence()
    exposure = estimate_exposure(frames(x), period=60)
    rendered = _rendered_from(exposure, x)
    for m in rendered[5:9]:
        m.valid[:] = False
        m.log_luminance[:] = np.nan
    summary = evaluate_render(exposure, rendered)
    assert 5 in summary.unobservable_frames
    assert not summary.passed


def test_interpolated_and_limited_frames_are_not_validated():
    x = base_sequence()
    big = x.copy()
    big[10:] += 4.0        # forces LIMITED clipping at explicit limits
    exposure = estimate_exposure(frames(big), period=60, ev_limits=(-2., 2.))
    rendered = _rendered_from(exposure, big)
    summary = evaluate_render(exposure, rendered)
    # LIMITED frames carry None errors, not fake zeros
    assert any(e is None for e in summary.errors)

def test_feedback_moves_beyond_two_by_default():
    x = base_sequence()
    exposure = estimate_exposure(frames(x), period=60)
    exposure.frames[0].applied_ev = -1.9
    summary = ValidationSummary([-.5] + [None] * (len(x) - 1), None, None, [], [], False)
    adjusted = feedback_update(exposure, summary)
    assert adjusted == 1
    # unbounded: -1.9 plus the clipped max step (-0.25) lands below -2 EV
    assert exposure.frames[0].applied_ev == pytest.approx(-2.15)
    feedback_update(exposure, summary, limits=(-2., 2.))
    assert exposure.frames[0].applied_ev == -2.
