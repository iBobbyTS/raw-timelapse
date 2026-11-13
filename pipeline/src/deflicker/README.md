# Pure exposure engine

This standard-library-only library retains the V4 smooth/uniform, strength,
period, ROI, confirmed-step and EV-limit semantics. Numerical operations use
`f64`, validate shapes/configuration and return errors for malformed inputs.
Missing observations remain NaN internally; serializers use JSON null.

`measurement::AnalysisSidecar::parse` consumes schema 2 measurements in signed
scene-linear AP1/D60. Source hashes are retained. Camera saturation and gained
analysis highlight occupancy have separate meanings. Exclusion boxes are
`[x0,y0,x1,y1]`; the native analysis ROI is `[x,y,width,height]`.

`estimate_exposure(&measurements, &ExposureConfig)` returns an ordered, complete
finite estimate for every input frame. Defaults are smooth / pairwise /
period 60 / strength 1 / unbounded EV. Smooth retains low-frequency evolution
and confirmed steps; uniform flattens measured evolution. Legacy selection
retains the tile-trend estimator; missing legacy gains now use conservative
neighbour interpolation/hold, or zero when nothing was observed. Missing
support never authorizes a large absolute-level correction.

The primary pair graph retains its structure, chroma, saturation, support and
spatial gates. Its sparse solver eliminates one gauge per component and uses
Jacobi-preconditioned conjugate gradients and bounded Huber IRLS. A graph with
no edges still contains each frame as a singleton, with unknown brightness.

Component placement is a second, small offset-only solve. Every actual adjacent
frame transition between primary components contributes a constraint. Coherent
shared-tile luminance differences constrain offsets without requiring a tile
that persists throughout the sequence. This preserves primary within-component
differences and handles interleaved components, arbitrary gauge constants,
component numbering and singleton frames. Persistent background is computed
only after placement. A zero-persistent sequence remains untrusted even when
local observations support a useful correction.

`pairwise::local_common_change` measures shared valid, adequately lit,
unsaturated in-ROI tile differences. It requires at least max(24, grid/4)
tiles, 60% agreement within 0.06 EV, scaled MAD <=0.06 EV and three quadrants.
It measures common luminance change, not its physical cause. It deliberately
does not assert fine-structure/chroma constancy or persistent background.
Without this evidence, low-weight fallback constraints preserve correction
continuity for smooth, or use the declared whole-frame level intent for
uniform when both frames have enough measurements. Unsupported frames remain
unverified. A sequence without measurements gets zero correction.

Smooth trends are fitted within groups joined by primary edges or observable
local transitions, retaining real frame spacing rather than compressing
non-contiguous members into consecutive samples. Unrelated spatial scene
baselines do not get smoothed into an exposure ramp. Confirmed steps are
removed before trend fitting and retained in the desired signal. There is
one correction gauge for the sequence, not an independent re-anchor per
component. Huber-Whittaker keeps period-derived lambda and the fixed 100-iterate
budget. If that tolerance is not reached, one weighted Whittaker solve supplies
a finite conservative trend, with `TREND_REFERENCE_NONCONVERGED` and no trust;
there is no extra RAW pass or unbounded retry.

`estimation::ExposurePlan::from_result` freezes the estimates into a read-only
ordered slice of `{frame_id, source_sha256, ev}`. It rejects count, identity,
order, missing-gain and nonfinite/unrepresentable-gain errors. Missing or
misordered entries are protocol errors, never silently rendered at zero.
The current run entry uses this handoff before final rendering.

`validation::evaluate_plan` runs before estimation returns and again before
the current final render handoff. It projects local input changes through the
actual gains and checks remaining common discontinuities against the intended
uniform signal or smooth trend/confirmed steps, including intentional partial
strength and EV limits. Its internal `PlanQuality` records potential pairs,
evaluated/skipped pairs and residuals above 0.05 EV. It neither raises a user
quality gate nor triggers another RAW traversal. It does not use the frozen
per-frame target obtained by adding our own gains; correct gain execution
alone cannot establish continuity. Render targets include the requested strength,
so bounded legacy feedback does not turn a partial correction into full strength.

`validation::evaluate_render` retains the old render-target feedback helper.
Its internal coverage counts every frame, including empty consensus masks,
missing targets, limited and unobservable frames. All-unobservable validation
has no error statistics and `passed=false`. This execution check is distinct
from local plan continuity. The current service owns its bounded feedback
loop, report compatibility and immutable final snapshot; the pure numerical
library performs no rendering, encoding or process I/O.

`photometry` supplies decoded-pixel measurement, transfer handling, matched
pairs and temporal features. The independent encoded-output oracle remains a
composition check; saved-measurement replay is not encoded-output acceptance.

Tests embed numerical inputs, including the compact
[exposure-seam fixture](fixtures/exposure-seam.md). They require no photos,
external disks, Python, ffmpeg or network for the library checks. Run from the
repository root with `cargo test --manifest-path pipeline/Cargo.toml` after the
repository-required initial clean.
