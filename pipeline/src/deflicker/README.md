# Pure V4 deflicker engine

This library ports the pure algorithms from `main-legacy @f6d3ae7`. It uses
`f64`, the standard library only, and no I/O or child processes. Public numerical
operations return `Result` for malformed shapes/configuration. Missing legacy
observations remain NaN internally; report serializers write them as JSON null.

`measurement::AnalysisSidecar::parse` consumes S01 schema 1 (top-level tiles,
actual grid dimensions, ragged structure samples). Match source identity using
`measurement.source_sha256`. Camera saturation gates and delivered output clip
occupancy are independent arrays. Exclusion boxes are `[x0,y0,x1,y1]`; sidecar
analysis ROI is `[x,y,width,height]`.

`estimate_exposure(&measurements, &ExposureConfig)` returns frames, original
tiles, consensus, trusted indices, frozen targets, scene-cut metadata and graph
summary. Defaults are smooth / pairwise / period 60 / strength 1 / unbounded EV.
Scene cuts and time gaps do not split estimation. Confirmed steps preserve
smooth intent; uniform intent flattens all measured evolution. Legacy estimator
selection retains the older tile-trend and per-frame uniform behavior.

The graph solver eliminates the first node of each component as its gauge and
uses deterministic Jacobi-preconditioned conjugate gradients on sparse weighted
incidence normal equations. Weights are normalized by their global median. Each
CG solve targets relative L2 residual 1e-12 with `4*free_nodes+64` iterations;
the recomputed true residual must be <=1e-10 and all values finite. Every inner
solve records component, IRLS round, iterations, residual and convergence. A
failure sets graph `converged=false`; it is never silently marked converged.
Huber IRLS uses delta 0.02, at most eight rounds, and successive-solution change
<1e-10. Lone nodes retain NaN. This is a solver substitution, not a claim of
SciPy bit identity.

`trends::solve_weighted` uses bandwidth-two Cholesky for `W + lambda D2' D2`.
Missing values are zeroed before forming the RHS. Huber-Whittaker retains delta
0.1, at most 100 iterations, change <=1e-5, at least eight valid observations,
and `lambda = 1/(16*sin(pi/P)^4)`. Solves use O(N) storage.

`validation::evaluate_render` compares consensus tile levels to the frozen desired
levels through one shared median offset. `feedback_update` changes only applied
EVs. S03 owns the render loop, best-state preservation and the limit of
`MAX_FEEDBACK_UPDATES=2`; the pure update function itself follows legacy and has
no orchestration counter. All-unobservable validation has no statistics and
`passed=false`. The service must distinguish that case from partial evidence.

`photometry` accepts decoded arrays. `linear16` means raw integer code divided by
16384; sRGB/bt709 inverse-transfer inputs are normalized code/65535. It includes
Gaussian mirror blur, trimmed tiles, matched-pair gates, temporal metrics,
audit geometry, ROI metrics and pure feature aggregation with boundaries.
`audit_features` flags common-jump candidates without assigning a physical cause.
S03 supplies source boundaries and owns decoding/encoding/publication.

`report::ExposureReport` stores the required JSON fields with caller-supplied
metadata and process results. Build it from the final exposure state (after
feedback) or refresh its cloned frames before serialization. Its initial run
status is PENDING, not success. It performs no writing. S03 owns CSV and file
publication.

Run `cargo clean` then `cargo test --lib` from `pipeline/`. Engine tests embed all
input fixtures; they do not depend on Python, `.agent-work`, ffmpeg or network.
`fixtures/generate.py` documents one-time NumPy fixture generation from the
frozen legacy synthetic inputs and S01 sample copies. Runtime tests never run it.
