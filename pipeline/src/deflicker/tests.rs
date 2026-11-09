//! Frozen f6d3ae7 scenario ports. Names below match the Python source files.
//! Fixtures preserve NumPy random draws; no Python/process/I/O runs in tests.
use super::*;
use super::{
    estimation::*,
    graph::*,
    pairwise::*,
    photometry::*,
    stats::{median, percentile},
    trends::*,
    validation::*,
};
use crate::json::Json;
#[path = "fixtures/random.rs"]
mod random;
const K: usize = 96;
fn near(a: f64, b: f64, tol: f64) {
    assert!((a - b).abs() <= tol, "{a} != {b} within {tol}");
}
fn centers() -> Vec<[f64; 2]> {
    (0..K)
        .map(|i| [((i % 12) as f64 + 0.5) / 12., ((i / 12) as f64 + 0.5) / 8.])
        .collect()
}
fn measurement(row: Vec<f64>, samples: bool) -> FrameMeasurement {
    FrameMeasurement {
        source_sha256: String::new(),
        log_pixel_samples: if samples {
            Some(row.iter().map(|v| vec![*v; 25]).collect())
        } else {
            None
        },
        log_channel_means: if samples {
            Some(row.iter().map(|v| [*v; 3]).collect())
        } else {
            None
        },
        log_luminance: row,
        valid: vec![true; K],
        centers: centers(),
        saturation_fraction: vec![0.; K],
        output_clip_fraction: vec![0.; K],
        width: 320,
        height: 240,
        grid_nx: 12,
        grid_ny: 8,
        saturation_mask_quality: "approximate".into(),
    }
}
fn frames(rows: &[Vec<f64>]) -> Vec<FrameMeasurement> {
    rows.iter().map(|r| measurement(r.clone(), false)).collect()
}
fn background(levels: &[f64], lo: f64, hi: f64) -> Vec<Vec<f64>> {
    levels
        .iter()
        .map(|v| {
            (0..K)
                .map(|i| v + lo + (hi - lo) * i as f64 / (K - 1) as f64)
                .collect()
        })
        .collect()
}
fn alt(t: usize, amplitude: f64) -> f64 {
    if t % 2 == 0 {
        amplitude
    } else {
        -amplitude
    }
}
fn cfg(brightness: &str, estimator: &str) -> ExposureConfig {
    ExposureConfig {
        brightness: brightness.into(),
        estimator: estimator.into(),
        ..ExposureConfig::default()
    }
}
fn estimate(ms: &[FrameMeasurement]) -> ExposureResult {
    estimate_exposure(ms, &ExposureConfig::default()).unwrap()
}
fn evs(e: &ExposureResult) -> Vec<f64> {
    e.frames
        .iter()
        .map(|f| f.applied_ev.unwrap_or(f64::NAN))
        .collect()
}
fn mean(a: &[f64]) -> f64 {
    a.iter().sum::<f64>() / a.len() as f64
}
fn range(a: &[f64]) -> f64 {
    a.iter().copied().fold(f64::NEG_INFINITY, f64::max)
        - a.iter().copied().fold(f64::INFINITY, f64::min)
}
fn metric(e: &ExposureResult, truth: &[f64], lo: usize, hi: usize) -> (f64, f64) {
    let a: Vec<_> = (lo..hi)
        .filter_map(|t| Some(e.frames[t].applied_ev? + truth[t]))
        .collect();
    let med = median(&a);
    let errs: Vec<_> = a.iter().map(|v| (v - med).abs()).collect();
    (
        (mean(&errs.iter().map(|v| v * v).collect::<Vec<_>>())).sqrt(),
        percentile(&errs, 95.),
    )
}
fn core(n: usize) -> (Vec<FrameMeasurement>, Vec<f64>) {
    let bytes: &[u8] = match n {
        240 => include_bytes!("fixtures/core-240.bin"),
        300 => include_bytes!("fixtures/core-300.bin"),
        900 => include_bytes!("fixtures/core-900.bin"),
        100 => include_bytes!("fixtures/core-100.bin"),
        _ => panic!("test fixture unavailable"),
    };
    let a: Vec<_> = bytes
        .chunks_exact(8)
        .map(|b| f64::from_le_bytes(b.try_into().unwrap()))
        .collect();
    (
        a[..n * K]
            .chunks_exact(K)
            .map(|r| measurement(r.to_vec(), false))
            .collect(),
        a[n * K..].to_vec(),
    )
}
fn sequence(n: usize) -> Vec<FrameMeasurement> {
    (0..n)
        .map(|t| measurement(vec![-2. + 0.001 * t as f64; K], false))
        .collect()
}
fn spliced(n: usize, cut: usize) -> (Vec<Vec<f64>>, Vec<f64>) {
    let flicker: Vec<_> = (0..n)
        .map(|t| alt(t, 0.13) + 0.065 * (2. * std::f64::consts::PI * t as f64 / 7.).sin())
        .collect();
    let rows = (0..n)
        .map(|t| {
            (0..K)
                .map(|i| {
                    -1.2 - 0.002 * t as f64
                        + random::OFF_20261030[i]
                        + flicker[t]
                        + if t >= cut {
                            1. + 1.5 * i as f64 / K as f64
                        } else {
                            0.
                        }
                })
                .collect()
        })
        .collect();
    (rows, flicker)
}
fn target_splice(n: usize, cut: usize, jump: f64) -> Vec<Vec<f64>> {
    (0..n)
        .map(|t| {
            (0..K)
                .map(|i| {
                    random::OFF_5[i]
                        + alt(t, 0.05)
                        + if t >= cut {
                            jump * (0.8 + 0.4 * i as f64 / K as f64)
                        } else {
                            0.
                        }
                })
                .collect()
        })
        .collect()
}
fn evolving(n: usize, cut: usize, ramp: f64) -> Vec<Vec<f64>> {
    (0..n)
        .map(|t| {
            (0..K)
                .map(|i| {
                    -1.2 - ramp * t as f64
                        + alt(t, 0.11)
                        + random::OFF_20261031[i]
                        + if t >= cut {
                            1.5 * (0.8 + 0.4 * i as f64 / K as f64)
                        } else {
                            0.
                        }
                })
                .collect()
        })
        .collect()
}
fn output_level(x: &[Vec<f64>], e: &ExposureResult) -> Vec<f64> {
    x.iter().zip(evs(e)).map(|(r, ev)| median(r) + ev).collect()
}
fn graph_edge(i: usize, j: usize, d: f64) -> GraphEdge {
    GraphEdge {
        i,
        j,
        delta_ev: d,
        weight: 1.,
        support: K,
        consensus_fraction: 1.,
        spatial_mad_ev: 0.,
        quadrants: 4,
        lag: j - i,
        rejects: Default::default(),
    }
}
mod test_exposure_core {
    use super::*;
    #[test]
    fn test_period_and_constant_linear() {
        near(lambda_for_period(60).unwrap(), 8330.6591, 8330.6591 * 1e-6);
        for linear in [false, true] {
            let x: Vec<_> = (0..300)
                .map(|t| -1.5 + if linear { 0.001 * t as f64 } else { 0. })
                .collect();
            let z = huber_whittaker(&x, &vec![true; x.len()], 60).unwrap();
            assert!(z.converged);
            for (a, b) in x.iter().zip(z.trend) {
                assert!((a - b).abs() < 1e-7);
            }
        }
    }
    #[test]
    fn test_clean_flicker() {
        let (m, f) = core(240);
        let e = estimate(&m);
        assert!(e.frames.iter().all(|f| f.applied_ev.is_some()));
        assert!(metric(&e, &f, 0, m.len()).0 < 0.015);
    }
    #[test]
    fn test_polluted_tiles_short_sequence_boundary() {
        let (m, f) = core(300);
        let e = estimate(&m);
        let missing: Vec<_> = e
            .frames
            .iter()
            .enumerate()
            .filter(|(_, f)| f.applied_ev.is_none())
            .map(|(i, _)| i)
            .collect();
        assert!(
            missing.len() <= 3 && missing.iter().all(|&i| i >= 297),
            "{missing:?}"
        );
        let (r, p) = metric(&e, &f, 0, m.len());
        assert!(r < 0.035 && p < 0.065, "{r} {p}");
    }
    #[test]
    fn test_polluted_tiles_900_frame_reference() {
        let (m, f) = core(900);
        let e = estimate(&m);
        assert!(e.frames.iter().filter(|f| f.applied_ev.is_some()).count() >= 897);
        let (r, p) = metric(&e, &f, 0, m.len());
        assert!(r < 0.025 && p < 0.04, "{r} {p}");
    }
    #[test]
    fn test_user_step_only_and_short_sequence() {
        let (mut m, _) = core(100);
        for row in &mut m[40..] {
            for v in &mut row.log_luminance {
                *v += 1.;
            }
        }
        let mut c = cfg("smooth", "legacy");
        c.steps.push(ExposureStep {
            start_frame: 40,
            delta_ev: 1.,
        });
        let e = estimate_exposure(&m, &c).unwrap();
        let ev = evs(&e);
        near(median(&ev[45..]) - median(&ev[..35]), -1., 0.06);
        let z = estimate(&m[..5]);
        assert!(z
            .frames
            .iter()
            .all(|f| f.applied_ev == Some(0.) && f.status == "SHORT_SEQUENCE_NO_DEFLICKER"));
    }
    #[test]
    fn test_disabled() {
        let (m, _) = core(240);
        let e = estimate_exposure(
            &m,
            &ExposureConfig {
                strength: 0.,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(e
            .frames
            .iter()
            .all(|f| f.applied_ev == Some(0.) && f.status == "DISABLED"));
    }
}
mod test_pairwise_graph {
    use super::*;
    fn pair(a: &[f64], b: &[f64], train: Option<&[bool]>) -> GraphEdge {
        pair_difference(
            &measurement(a.to_vec(), true),
            &measurement(b.to_vec(), true),
            1,
            train,
        )
        .unwrap()
    }
    #[test]
    fn test_pair_difference_recovers_common_gain() {
        let a = background(&[0., 0.12], -0.3, 0.3);
        let e = pair(&a[0], &a[1], None);
        assert!(e.reliable());
        near(e.delta_ev, 0.12, 1e-6);
        assert_eq!(e.support, K);
    }
    #[test]
    fn test_pair_difference_ignores_moving_object() {
        let mut a = background(&[0., 0.], -0.3, 0.3);
        for v in &mut a[1][..20] {
            *v += 1.5;
        }
        let e = pair(&a[0], &a[1], None);
        assert!(e.reliable());
        near(e.delta_ev, 0., 0.02);
    }
    #[test]
    fn test_pair_difference_rejects_half_scene_change() {
        let mut a = background(&[0., 0.], -0.3, 0.3);
        for v in &mut a[1][..K / 2] {
            *v += 1.;
        }
        assert!(!pair(&a[0], &a[1], None).reliable());
    }
    #[test]
    fn test_pair_difference_excluded_rois_reduce_support() {
        let a = background(&[0., 0.1], -0.3, 0.3);
        let train: Vec<_> = (0..K).map(|i| i % 2 == 1).collect();
        let e = pair(&a[0], &a[1], Some(&train));
        assert_eq!(e.support, K / 2);
        near(e.delta_ev, 0.1, 1e-6);
    }
    #[test]
    fn test_build_edges_respects_boundaries_and_lags() {
        let levels: Vec<_> = (0..12).map(|t| alt(t, 0.05)).collect();
        let m = frames(&background(&levels, -0.3, 0.3));
        let (e, s) = build_edges(&m, &[6], None, &DEFAULT_LAGS).unwrap();
        assert!(e.iter().all(|e| !(e.i < 6 && e.j >= 6)));
        assert_eq!(s.by_lag[&1].0, 10);
    }
    #[test]
    fn test_graph_solve_recovers_known_brightness() {
        let truth = [0., 0.1, 0.18, 0.05, -0.12, -0.2, 0.02, 0.3];
        let mut edges = vec![];
        let mut k = 0;
        for i in 0..8 {
            for j in i + 1..8 {
                let mut e = graph_edge(i, j, truth[j] - truth[i] + random::GRAPH_NOISE[k]);
                e.spatial_mad_ev = 0.004;
                edges.push(e);
                k += 1;
            }
        }
        let r = solve_exposure_graph(8, &edges).unwrap();
        assert!(r.converged);
        for (a, b) in r.brightness.iter().zip(truth) {
            near(a - r.brightness[0], b, 0.01);
        }
        assert!(r.edge_p95_abs_residual_ev.unwrap() < 0.02);
        assert!(r
            .linear_solves
            .iter()
            .all(|r| r.converged && r.relative_residual <= 1e-10));
    }
    #[test]
    fn test_graph_disconnected_components_keep_separate_gauges() {
        let r = solve_exposure_graph(4, &[graph_edge(0, 1, 0.5), graph_edge(2, 3, -0.2)]).unwrap();
        assert_eq!(r.components.len(), 2);
        assert_eq!(r.brightness[0], 0.);
        assert_eq!(r.brightness[2], 0.);
        near(r.brightness[1], 0.5, 5e-7);
        near(r.brightness[3], -0.2, 2e-7);
    }
    #[test]
    fn test_graph_lone_frame_stays_unconstrained() {
        let r = solve_exposure_graph(4, &[graph_edge(0, 2, 0.1)]).unwrap();
        assert!(r.brightness[1].is_nan());
        assert!(r.component[1] == -1 || r.components.len() > 1);
    }
    #[test]
    fn test_graph_no_edges_all_unconstrained() {
        let r = solve_exposure_graph(5, &[]).unwrap();
        assert!(r.brightness.iter().all(|v| v.is_nan()));
    }
    #[test]
    fn test_persistent_background_rejects_visiting_object() {
        let mut a = background(&[0.; 20], -0.3, 0.3);
        for r in &mut a[..10] {
            for v in &mut r[..10] {
                *v += 1.;
            }
        }
        let p = persistent_background(&frames(&a), &[0.; 20]).unwrap();
        assert!(p[..10].iter().all(|v| !*v));
        assert!(p[10..].iter().all(|v| *v));
    }
    #[test]
    fn test_persistent_background_keeps_stable_surfaces() {
        let levels: Vec<_> = (0..20).map(|t| 0.8 * t as f64 / 19.).collect();
        assert!(
            persistent_background(&frames(&background(&levels, -0.3, 0.3)), &levels)
                .unwrap()
                .iter()
                .all(|v| *v)
        );
    }
}
mod test_target_strategy {
    use super::*;
    #[test]
    fn test_measured_and_desired_levels_are_reported() {
        let x = target_splice(60, 30, 1.);
        let e = estimate(&frames(&x));
        assert!(e
            .frames
            .iter()
            .all(|f| f.measured_level_ev.is_some() && f.desired_level_ev.is_some()));
        for f in e.frames {
            assert!(
                f.requested_ev.unwrap()
                    - (f.desired_level_ev.unwrap() - f.measured_level_ev.unwrap())
                    < 1e-9
            );
        }
    }
    #[test]
    fn test_uniform_flattens_across_boundary() {
        let x = target_splice(60, 30, 1.);
        let e = estimate_exposure(&frames(&x), &cfg("uniform", "pairwise")).unwrap();
        assert!(range(&output_level(&x, &e)) < 0.1);
    }
    #[test]
    fn test_target_provenance_is_auditable() {
        let x = frames(&target_splice(60, 30, 1.));
        let e = estimate_exposure(&x, &cfg("uniform", "pairwise")).unwrap();
        let p = e.pairwise_summary.get("target_provenance").unwrap();
        assert_eq!(p.get("intent").unwrap().as_str(), Some("uniform"));
        assert_eq!(p.get("reference_scope").unwrap().as_str(), Some("global"));
        assert_eq!(p.get("defined_before_iteration"), Some(&Json::Bool(true)));
        let g = estimate(&x);
        assert!(g
            .pairwise_summary
            .get("target_provenance")
            .unwrap()
            .get("desired")
            .unwrap()
            .as_str()
            .unwrap()
            .contains("period=60"));
    }
    #[test]
    fn test_confirmed_step_semantics_differ_by_intent() {
        let x = target_splice(60, 30, 1.);
        let mut c = cfg("smooth", "pairwise");
        c.steps.push(ExposureStep {
            start_frame: 30,
            delta_ev: 1.,
        });
        let s = evs(&estimate_exposure(&frames(&x), &c).unwrap());
        c.brightness = "uniform".into();
        let u = estimate_exposure(&frames(&x), &c).unwrap();
        assert!((median(&s[..10]) - median(&s[50..])).abs() < 0.1);
        let out = output_level(&x, &u);
        assert!((mean(&out[..10]) - mean(&out[50..])).abs() < 0.1);
    }
    #[test]
    fn test_no_lowpass_on_final_correction() {
        let x: Vec<Vec<_>> = (0..40)
            .map(|t| random::OFF_2.iter().map(|v| v + alt(t, 0.1)).collect())
            .collect();
        let e = evs(&estimate(&frames(&x)));
        assert!(
            mean(&e.iter().skip(1).step_by(2).copied().collect::<Vec<_>>())
                - mean(&e.iter().step_by(2).copied().collect::<Vec<_>>())
                > 0.15
        );
    }
}
mod test_scene_cut_segmentation {
    use super::*;
    #[test]
    fn test_scene_cut_reported_but_never_splits() {
        let (x, f) = spliced(120, 60);
        let e = estimate(&frames(&x));
        assert_eq!(
            e.scene_cuts.iter().map(|c| c.index).collect::<Vec<_>>(),
            vec![60]
        );
        assert_eq!(e.scene_cuts[0].common_tiles, K);
        assert!(e
            .frames
            .iter()
            .all(|f| f.applied_ev.is_some() && f.segment_id == 0));
        assert!(metric(&e, &f, 0, 60).0 < 0.03);
        assert!(metric(&e, &f, 60, 120).0 < 0.03);
    }
    #[test]
    fn test_without_cuts_the_splice_stays_unreliable() {
        let (x, _) = spliced(120, 60);
        let mut c = cfg("smooth", "legacy");
        c.scene_cut_median_ev = 1e9;
        let e = estimate_exposure(&frames(&x), &c).unwrap();
        assert!(e.frames.iter().any(|f| f.applied_ev.is_none()));
        assert!(e.scene_cuts.is_empty());
    }
    #[test]
    fn test_common_jump_is_flicker_not_a_cut() {
        let (mut x, _) = spliced(120, 120);
        for r in &mut x[60..] {
            for v in r {
                *v += 1.;
            }
        }
        let e = estimate(&frames(&x));
        assert!(e.scene_cuts.is_empty());
        assert!(e.frames.iter().all(|f| f.applied_ev.is_some()));
        let a = evs(&e);
        assert!(median(&a[61..75]) <= -0.15);
        assert!(median(&a[105..120]).abs() < 0.15);
    }
    #[test]
    fn test_island_frames_get_real_estimates() {
        let (mut x, f) = spliced(64, 64);
        for (t, row) in x.iter_mut().enumerate() {
            for (i, v) in row.iter_mut().enumerate() {
                if t >= 30 {
                    *v += 1. + 1.5 * i as f64 / K as f64;
                }
                if t >= 34 {
                    *v += 0.6 + 0.9 * (K - 1 - i) as f64 / K as f64;
                }
            }
        }
        let e = estimate(&frames(&x));
        assert_eq!(
            e.scene_cuts.iter().map(|c| c.index).collect::<Vec<_>>(),
            vec![30, 34]
        );
        assert!(e
            .frames
            .iter()
            .all(|f| f.applied_ev.is_some() && f.status != "SHORT_SEQUENCE_NO_DEFLICKER"));
        assert!(metric(&e, &f, 0, 30).0 < 0.03);
        assert!(metric(&e, &f, 34, 64).0 < 0.03);
    }
    #[test]
    fn test_whole_mode_never_splits_estimation() {
        let (x, _) = spliced(120, 60);
        let mut c = cfg("uniform", "pairwise");
        c.time_gaps = vec![60];
        let e = estimate_exposure(&frames(&x), &c).unwrap();
        assert_eq!(
            e.scene_cuts.iter().map(|c| c.index).collect::<Vec<_>>(),
            vec![60]
        );
        assert_eq!(e.time_gaps, vec![60]);
        assert!(e.frames.iter().all(|f| f.segment_id == 0
            && f.applied_ev.is_some()
            && f.status != "SHORT_SEQUENCE_NO_DEFLICKER"));
    }
    #[test]
    fn test_whole_mode_uniform_levels_across_a_splice() {
        let (x, _) = spliced(120, 60);
        let e = estimate_exposure(&frames(&x), &cfg("uniform", "pairwise")).unwrap();
        assert_eq!(
            e.scene_cuts.iter().map(|c| c.index).collect::<Vec<_>>(),
            vec![60]
        );
        assert!(e
            .frames
            .iter()
            .all(|f| f.status == "UNIFORM" && f.applied_ev.is_some() && f.segment_id == 0));
        assert!(range(&output_level(&x, &e)) < 0.05);
    }
    #[test]
    fn test_uniform_levels_a_lone_frame_absolutely() {
        let (mut x, _) = spliced(120, 120);
        for (i, v) in x[60].iter_mut().enumerate() {
            *v += 1. + 1.5 * i as f64 / K as f64;
        }
        let e = estimate_exposure(&frames(&x), &cfg("uniform", "pairwise")).unwrap();
        assert_eq!(e.frames[60].status, "ABSOLUTE_LEVEL");
        assert!(e.frames[60]
            .warnings
            .iter()
            .any(|w| w == "LONE_FRAME_ABSOLUTE_LEVEL"));
        assert!(e.frames.iter().all(|f| f.applied_ev.is_some()));
        assert!(range(&output_level(&x, &e)) < 0.05);
    }
    #[test]
    fn test_whole_mode_stitch_is_boundary_local() {
        let off = random::OFF_7;
        let mut order: Vec<_> = (0..K).collect();
        order.sort_by(|&a, &b| off[a].total_cmp(&off[b]));
        let mut rank = vec![0.; K];
        for (r, &i) in order.iter().enumerate() {
            rank[i] = r as f64 / K as f64 - 0.5;
        }
        let x: Vec<Vec<_>> = (0..40)
            .map(|t| {
                (0..K)
                    .map(|i| {
                        off[i]
                            + alt(t, 0.05)
                            + 0.02 * t as f64 * rank[i]
                            + if t >= 20 && i % 100 < 45 { 2. } else { 0. }
                    })
                    .collect()
            })
            .collect();
        let e = estimate_exposure(&frames(&x), &cfg("uniform", "pairwise")).unwrap();
        assert!(e.frames.iter().all(|f| f.segment_id == 0));
        assert!(!e
            .pairwise_summary
            .get("cross_boundary_anchors")
            .unwrap()
            .get("entries")
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty());
        let out = output_level(&x, &e);
        assert!((out[20] - out[19]).abs() < 0.05);
    }
    #[test]
    fn test_confirmed_step_never_triggers_a_cut() {
        let (mut x, _) = spliced(120, 120);
        for r in &mut x[40..] {
            for v in r {
                *v += 1.;
            }
        }
        let c = ExposureConfig {
            steps: vec![ExposureStep {
                start_frame: 40,
                delta_ev: 1.,
            }],
            ..Default::default()
        };
        let e = estimate_exposure(&frames(&x), &c).unwrap();
        assert!(e.scene_cuts.is_empty());
        assert!(e.frames.iter().all(|f| f.applied_ev.is_some()));
        assert_eq!(e.frames[40].cumulative_confirmed_step_ev, 1.);
    }
}
mod test_exposure_states {
    use super::*;
    #[test]
    fn test_long_gap_and_edge_are_review() {
        let mut s = sequence(90);
        for i in [0, 1, 20, 21, 22, 23, 24, 25, 26] {
            s[i].valid.fill(false);
        }
        let e = estimate(&s);
        assert_eq!(e.frames[0].status, "REVIEW_REQUIRED");
        assert!(e.frames[22].applied_ev.is_none());
        assert!(e.frames[28].applied_ev.is_some());
    }
    #[test]
    fn test_short_gap_across_confirmed_step() {
        let mut s = sequence(90);
        for i in [40, 41] {
            s[i].valid.fill(false);
        }
        let mut c = cfg("smooth", "legacy");
        c.steps = vec![ExposureStep {
            start_frame: 41,
            delta_ev: 0.4,
        }];
        let e = estimate_exposure(&s, &c).unwrap();
        assert_eq!(e.frames[40].status, "INTERPOLATED_ESTIMATE");
        assert_eq!(e.frames[41].cumulative_confirmed_step_ev, 0.4);
        assert!(e.frames[41].requested_ev.is_some());
        assert_eq!(
            e.frames[41].requested_ev.unwrap() - e.frames[40].requested_ev.unwrap(),
            -0.4 + (e.frames[40].common_residual_ev.unwrap()
                - e.frames[41].common_residual_ev.unwrap())
        );
    }
    #[test]
    fn test_limit_keeps_request_distinct() {
        let c = ExposureConfig {
            ev_limits: Some((-2., 2.)),
            steps: vec![ExposureStep {
                start_frame: 3,
                delta_ev: 5.,
            }],
            ..Default::default()
        };
        let e = estimate_exposure(&sequence(5), &c).unwrap();
        assert!(e.frames.iter().any(|f| f.status == "LIMITED"));
        assert!(e.frames.iter().all(|f| f.applied_ev.is_some()));
    }
}
mod test_exposure_regressions {
    use super::*;
    #[test]
    fn test_nan_missing_not_rhs_nan() {
        let mut x: Vec<_> = (0..60).map(|t| -2. + t as f64 / 59.).collect();
        x[18..21].fill(f64::NAN);
        let w: Vec<_> = x
            .iter()
            .map(|v| if v.is_finite() { 1. } else { 0. })
            .collect();
        assert!(solve_weighted(&x, &w, lambda_for_period(60).unwrap())
            .unwrap()
            .iter()
            .all(|v| v.is_finite()));
    }
    #[test]
    fn test_natural_boundary_and_10000_frames() {
        let x: Vec<_> = (0..10000).map(|t| -2. + t as f64 / 9999.).collect();
        let z = solve_weighted(&x, &vec![1.; 10000], lambda_for_period(60).unwrap()).unwrap();
        assert!((z[0] - x[0]).abs() < 1e-7 && (z[9999] - x[9999]).abs() < 1e-7);
    }
    #[test]
    fn test_nonfinite_explicitly_valid_rejected() {
        let mut ms = sequence(20);
        ms[2].log_luminance[7] = f64::NAN;
        assert!(estimate_exposure(&ms, &Default::default())
            .unwrap_err()
            .contains("nonfinite"));
    }
}
mod test_brightness_uniform {
    use super::*;
    #[test]
    fn test_uniform_holds_constant_brightness() {
        let x = evolving(90, 45, 0.02);
        let e = estimate_exposure(&frames(&x), &cfg("uniform", "pairwise")).unwrap();
        assert!(e
            .frames
            .iter()
            .all(|f| f.status == "UNIFORM" && f.applied_ev.is_some()));
        assert!(range(&output_level(&x, &e)) < 1e-9);
        assert_eq!(
            e.scene_cuts.iter().map(|c| c.index).collect::<Vec<_>>(),
            vec![45]
        );
    }
    #[test]
    fn test_uniform_clips_to_ev_limits() {
        let mut c = cfg("uniform", "pairwise");
        c.ev_limits = Some((-2., 2.));
        let e = estimate_exposure(&frames(&evolving(90, 45, 0.07)), &c).unwrap();
        let clipped: Vec<_> = e.frames.iter().filter(|f| f.status == "LIMITED").collect();
        assert!(!clipped.is_empty());
        assert!(clipped
            .iter()
            .all(|f| (f.applied_ev == Some(-2.) || f.applied_ev == Some(2.))
                && f.warnings.iter().any(|w| w == "EXPOSURE_LIMITED")));
        assert!(e.frames.iter().all(|f| f.applied_ev.is_some()));
    }
    #[test]
    fn test_uniform_unbounded_applies_any_ev() {
        let e = estimate_exposure(
            &frames(&evolving(90, 45, 0.07)),
            &cfg("uniform", "pairwise"),
        )
        .unwrap();
        assert!(e.frames.iter().all(|f| f.status == "UNIFORM"
            && f.requested_ev == f.applied_ev
            && !f.warnings.iter().any(|w| w == "EXPOSURE_LIMITED")));
        let a = evs(&e);
        assert!(a.iter().any(|v| v.abs() > 2.));
    }
    #[test]
    fn test_uniform_works_short_and_disabled() {
        let x = frames(&evolving(5, 5, 0.02));
        let mut c = cfg("uniform", "pairwise");
        let e = estimate_exposure(&x, &c).unwrap();
        assert!(e
            .frames
            .iter()
            .all(|f| f.status == "UNIFORM" && f.applied_ev.is_some()));
        c.strength = 0.;
        let z = estimate_exposure(&x, &c).unwrap();
        assert!(z
            .frames
            .iter()
            .all(|f| f.status == "DISABLED" && f.applied_ev == Some(0.)));
    }
    #[test]
    fn test_uniform_unmeasurable_frame_still_review() {
        let mut ms = frames(&evolving(90, 45, 0.02));
        ms[40].valid.fill(false);
        ms[40].log_luminance.fill(f64::NAN);
        let e = estimate_exposure(&ms, &cfg("uniform", "pairwise")).unwrap();
        assert_eq!(e.frames[40].status, "REVIEW_REQUIRED");
        assert!(e.frames[40].applied_ev.is_none());
        assert_eq!(e.frames[41].status, "UNIFORM");
    }
    #[test]
    fn test_uniform_invalid_mode_rejected() {
        assert!(
            estimate_exposure(&frames(&evolving(12, 12, 0.02)), &cfg("auto", "pairwise"))
                .unwrap_err()
                .contains("brightness")
        );
    }
}
fn base_sequence(n: usize) -> Vec<Vec<f64>> {
    (0..n)
        .map(|t| random::OFF_11.iter().map(|v| v + alt(t, 0.08)).collect())
        .collect()
}
fn rendered(
    e: &ExposureResult,
    x: &[Vec<f64>],
    factor: f64,
    spoil: Option<(usize, f64)>,
) -> Vec<FrameMeasurement> {
    frames(
        &x.iter()
            .enumerate()
            .map(|(t, r)| {
                r.iter()
                    .map(|v| {
                        v + factor * e.frames[t].applied_ev.unwrap_or(0.)
                            + spoil.filter(|(i, _)| *i == t).map_or(0., |(_, s)| s)
                    })
                    .collect()
            })
            .collect::<Vec<_>>(),
    )
}
fn validation_summary(
    errors: Vec<Option<f64>>,
    med: Option<f64>,
    p95: Option<f64>,
) -> ValidationSummary {
    ValidationSummary {
        errors,
        median_abs_error_ev: med,
        p95_abs_error_ev: p95,
        outlier_frames: vec![],
        unobservable_frames: vec![],
        passed: false,
        iteration: 0,
    }
}
mod test_output_feedback {
    use super::*;
    #[test]
    fn test_correct_execution_passes_with_small_error() {
        let x = base_sequence(30);
        let mut e = estimate(&frames(&x));
        let r = rendered(&e, &x, 1., None);
        let s = evaluate_render(&mut e, &r, 0).unwrap();
        assert!(s.passed);
        assert!(s.median_abs_error_ev.unwrap() < 0.01);
    }
    #[test]
    fn test_single_severe_frame_fails_despite_clean_percentiles() {
        let x = base_sequence(30);
        let mut e = estimate(&frames(&x));
        let r = rendered(&e, &x, 1., Some((15, 0.5)));
        let s = evaluate_render(&mut e, &r, 0).unwrap();
        assert!(!s.passed);
        assert!(s.outlier_frames.contains(&15));
        assert!(s.p95_abs_error_ev.unwrap() < 0.05);
    }
    #[test]
    fn test_under_execution_is_detected_and_feedback_converges() {
        let x = base_sequence(30);
        let mut e = estimate(&frames(&x));
        let r = rendered(&e, &x, 0.8, None);
        let s = evaluate_render(&mut e, &r, 0).unwrap();
        assert!(!s.passed);
        let before: Vec<_> = e.frames.iter().map(|f| f.desired_level_ev).collect();
        assert!(feedback_update(&mut e, &s, None).unwrap() > 0);
        assert_eq!(
            e.frames
                .iter()
                .map(|f| f.desired_level_ev)
                .collect::<Vec<_>>(),
            before
        );
        let r = rendered(&e, &x, 0.8, None);
        let updated = evaluate_render(&mut e, &r, 1).unwrap();
        assert!(updated.median_abs_error_ev < s.median_abs_error_ev);
    }
    #[test]
    fn test_frozen_target_cannot_be_resmoothed_into_passing() {
        let x = base_sequence(30);
        let mut e = estimate(&frames(&x));
        let before: Vec<_> = e.frames.iter().map(|f| f.desired_level_ev).collect();
        for iteration in 0..3 {
            let r = rendered(&e, &x, 0.5, None);
            let s = evaluate_render(&mut e, &r, iteration).unwrap();
            if s.passed {
                break;
            }
            feedback_update(&mut e, &s, None).unwrap();
        }
        assert_eq!(
            e.frames
                .iter()
                .map(|f| f.desired_level_ev)
                .collect::<Vec<_>>(),
            before
        );
    }
    #[test]
    fn test_unobservable_frames_do_not_silently_pass() {
        let x = base_sequence(30);
        let mut e = estimate(&frames(&x));
        let mut r = rendered(&e, &x, 1., None);
        for m in &mut r[5..9] {
            m.valid.fill(false);
            m.log_luminance.fill(f64::NAN);
        }
        let s = evaluate_render(&mut e, &r, 0).unwrap();
        assert!(s.unobservable_frames.contains(&5));
        assert!(!s.passed);
    }
    #[test]
    fn test_interpolated_and_limited_frames_are_not_validated() {
        let mut x = base_sequence(30);
        for r in &mut x[10..] {
            for v in r {
                *v += 4.;
            }
        }
        let mut e = estimate_exposure(
            &frames(&x),
            &ExposureConfig {
                ev_limits: Some((-2., 2.)),
                ..Default::default()
            },
        )
        .unwrap();
        let r = rendered(&e, &x, 1., None);
        let s = evaluate_render(&mut e, &r, 0).unwrap();
        assert!(s.errors.iter().any(|v| v.is_none()));
    }
    #[test]
    fn test_feedback_moves_beyond_two_by_default() {
        let x = base_sequence(30);
        let mut e = estimate(&frames(&x));
        e.frames[0].applied_ev = Some(-1.9);
        let mut errors = vec![None; x.len()];
        errors[0] = Some(-0.5);
        let s = validation_summary(errors, None, None);
        assert_eq!(feedback_update(&mut e, &s, None).unwrap(), 1);
        near(e.frames[0].applied_ev.unwrap(), -2.15, 2.15e-6);
        feedback_update(&mut e, &s, Some((-2., 2.))).unwrap();
        assert_eq!(e.frames[0].applied_ev, Some(-2.));
    }
}
mod test_gain_validation {
    use super::*;
    // test_probe_good_and_catches_double_gain is S03: invokes the renderer/measure_frame.
    #[test]
    fn test_freeze_and_feedback() {
        let x: Vec<_> = (0..40).map(|t| vec![-2. + alt(t, 0.1); K]).collect();
        let mut e = estimate(&frames(&x));
        let frozen = e.frozen_target.clone();
        let r = rendered(&e, &x, 0.8, None);
        let before = evaluate_render(&mut e, &r, 0).unwrap();
        assert!(before.median_abs_error_ev.unwrap() > 0.01);
        assert!(feedback_update(&mut e, &before, None).unwrap() > 0);
        assert_eq!(e.frozen_target, frozen);
        assert!(!feedback_diverged(
            &validation_summary(vec![], Some(0.02), Some(0.04)),
            &validation_summary(vec![], Some(0.021), Some(0.041))
        ));
        assert!(feedback_diverged(
            &validation_summary(vec![], Some(0.01), Some(0.03)),
            &validation_summary(vec![], Some(0.02), Some(0.05))
        ));
    }
}
mod test_independent_truth {
    use super::*;
    fn moving() -> Vec<FrameMeasurement> {
        let mut tiles = background(&[0.; 30], -0.4, 0.4);
        for (t, r) in tiles.iter_mut().enumerate() {
            let width = 8.max((K as f64 * (0.70 - 0.02 * t as f64)) as usize);
            for v in &mut r[..width] {
                *v += 2.;
            }
        }
        frames(&tiles)
    }
    #[test]
    fn test_median_of_medians_is_not_matched_background_truth() {
        let a: Vec<_> = (0..K).map(|i| i as f64 / (K - 1) as f64).collect();
        let mut b = a.clone();
        let mut c = a.clone();
        for v in &mut b[..K / 2 + 1] {
            *v += 2.;
        }
        for v in &mut c[K / 2..] {
            *v += 2.;
        }
        assert!(median(&b) - median(&a) > 1.);
        assert!((median(&c) - median(&a)).abs() > 0.9);
        near(
            median(&(K / 2 + 1..K).map(|i| b[i] - a[i]).collect::<Vec<_>>()),
            0.,
            1e-12,
        );
        near(
            median(&(0..K / 2).map(|i| c[i] - a[i]).collect::<Vec<_>>()),
            0.,
            1e-12,
        );
    }
    #[test]
    fn test_uniform_holds_constant_background_despite_moving_object() {
        let e = estimate_exposure(&moving(), &cfg("uniform", "pairwise")).unwrap();
        assert!(e
            .frames
            .iter()
            .all(|f| f.applied_ev.unwrap_or(0.).abs() <= 0.05));
    }
    #[test]
    fn test_legacy_uniform_pumps_on_moving_object_documented() {
        let e = estimate_exposure(&moving(), &cfg("uniform", "legacy")).unwrap();
        assert!(evs(&e).iter().any(|v| v.abs() > 0.25));
    }
    fn correlation(a: &[f64], b: &[f64]) -> f64 {
        let x = mean(a);
        let y = mean(b);
        let cov: f64 = a.iter().zip(b).map(|(a, b)| (a - x) * (b - y)).sum();
        cov / (a.iter().map(|a| (a - x).powi(2)).sum::<f64>()
            * b.iter().map(|b| (b - y).powi(2)).sum::<f64>())
        .sqrt()
    }
    #[test]
    fn test_smooth_keeps_ramp_and_removes_high_frequency() {
        let n = 60;
        let ramp: Vec<_> = (0..n).map(|t| 0.8 * t as f64 / 59.).collect();
        let flicker: Vec<_> = (0..n).map(|t| alt(t, 0.05)).collect();
        let levels: Vec<_> = ramp.iter().zip(&flicker).map(|(a, b)| a + b).collect();
        let e = evs(&estimate(&frames(&background(&levels, -0.4, 0.4))));
        let even: Vec<_> = e.iter().step_by(2).copied().collect();
        let odd: Vec<_> = e.iter().skip(1).step_by(2).copied().collect();
        assert!(mean(&odd) - mean(&even) > 0.04);
        let hf: Vec<_> = (0..n)
            .map(|t| {
                -(flicker[t]
                    - (t.saturating_sub(4)..(t + 5).min(n))
                        .map(|i| flicker[i] / 9.)
                        .sum::<f64>())
            })
            .collect();
        assert!(correlation(&e, &hf).abs() > 0.5);
        assert!(correlation(&e, &ramp.iter().map(|v| -v).collect::<Vec<_>>()).abs() < 0.5);
    }
    #[test]
    fn test_injected_alternating_gain_is_countered() {
        let levels: Vec<_> = (0..40).map(|t| alt(t, 0.1)).collect();
        let e = evs(&estimate(&frames(&background(&levels, -0.4, 0.4))));
        assert!(mean(&e.iter().step_by(2).copied().collect::<Vec<_>>()) < -0.05);
        assert!(mean(&e.iter().skip(1).step_by(2).copied().collect::<Vec<_>>()) > 0.05);
    }
    #[test]
    fn test_validity_set_change_alone_must_not_move_common_level() {
        let v: Vec<_> = (0..K).map(|i| -0.5 + i as f64 / (K - 1) as f64).collect();
        assert!((median(&v[K / 2..]) - median(&v)).abs() > 0.2);
        near(
            median(
                &v[K / 2..]
                    .iter()
                    .zip(&v[K / 2..])
                    .map(|(a, b)| a - b)
                    .collect::<Vec<_>>(),
            ),
            0.,
            1e-12,
        );
    }
    #[test]
    fn test_global_flash_is_candidate_not_diagnosis() {
        let a = vec![0.2; 180 * 240];
        let b = vec![0.2 * 0.3_f64.exp2(); a.len()];
        let sat = vec![0.; a.len()];
        let ta = tile_statistics(&a, &sat, 240, 180, 24, 18).unwrap();
        let tb = tile_statistics(&b, &sat, 240, 180, 24, 18).unwrap();
        let (m, _, _) = pair_metric(&tb, &ta, 24, 18, None).unwrap();
        assert_eq!(m.quality, "RELIABLE_COMMON_CHANGE");
        near(m.common_ev.unwrap(), 0.3, 1e-3);
    }
    #[test]
    fn test_temporal_summary_missing_pair_is_never_interpolated() {
        let s = temporal_summary(&[Some(0.1), None, Some(-0.1)]);
        assert_eq!(s.usable_pairs, 2);
        assert!(s.curvature_rms_ev.is_none());
    }
}
mod test_video_audit {
    use super::*;
    fn compare(a: &[f64], b: &[f64]) -> PairMetric {
        let sat = vec![0.; a.len()];
        let ta = tile_statistics(a, &sat, 240, 180, 24, 18).unwrap();
        let tb = tile_statistics(b, &sat, 240, 180, 24, 18).unwrap();
        pair_metric(&tb, &ta, 24, 18, None).unwrap().0
    }
    #[test]
    fn test_global_gain_is_recovered() {
        let a = vec![0.18; 180 * 240];
        let b = vec![0.18 * 0.08_f64.exp2(); a.len()];
        let r = compare(&a, &b);
        assert_eq!(r.quality, "RELIABLE_COMMON_CHANGE");
        near(r.common_ev.unwrap(), 0.08, 1e-4);
    }
    #[test]
    fn test_small_moving_bright_patch_does_not_make_global_gain() {
        let a = vec![0.18; 180 * 240];
        let mut b = a.clone();
        for y in 100..150 {
            for x in 80..120 {
                b[y * 240 + x] = 0.65;
            }
        }
        near(compare(&a, &b).common_ev.unwrap(), 0., 1e-6);
    }
    #[test]
    fn test_dark_is_not_zero_flicker_pass() {
        let a = vec![0.; 180 * 240];
        let r = compare(&a, &a);
        assert_eq!(r.quality, "INSUFFICIENT_SUPPORT");
        assert!(r.common_ev.is_none());
    }
    #[test]
    fn test_ramp_has_zero_curvature_but_not_zero_step() {
        let s = temporal_summary(&[Some(0.02); 40]);
        assert_eq!(s.curvature_rms_ev, Some(0.));
        assert_eq!(s.median_abs_step_ev, Some(0.02));
    }
    #[test]
    fn test_alternation_has_large_curvature() {
        let s = temporal_summary(&(0..40).map(|t| Some(alt(t, 0.1))).collect::<Vec<_>>());
        near(s.curvature_rms_ev.unwrap(), 0.1, 1e-7);
        near(s.lag2_residual_rms_ev.unwrap(), 0.1, 1e-7);
    }
    #[test]
    fn test_no_temporal_evidence_for_one_frame() {
        let s = temporal_summary(&[]);
        assert!(s.curvature_rms_ev.is_none() && s.p95_abs_step_ev.is_none());
    }
    #[test]
    fn test_missing_pair_is_not_interpolated_into_evidence() {
        let s = temporal_summary(&[Some(0.1), None, Some(-0.1)]);
        assert!(s.curvature_rms_ev.is_none());
        assert_eq!(s.usable_pairs, 2);
    }
    #[test]
    fn test_transfer_decode_and_truncated_frame() {
        // Pure assertions only; read_frame pipe/truncation is S03.
        assert_eq!(inverse_transfer(&[0., 1.], "srgb").unwrap(), vec![0., 1.]);
        assert!(inverse_transfer(&[0.5], "bt709").is_ok());
        assert!(inverse_transfer(&[0.5], "unknown").is_err());
    }
    #[test]
    fn test_transfer_tag_classification() {
        assert_eq!(classify_transfer_tag(Some("bt709")).unwrap(), "bt709");
        assert_eq!(classify_transfer_tag(Some("srgb")).unwrap(), "srgb");
        for tag in [
            None,
            Some("unknown"),
            Some("unspecified"),
            Some("smpte2084"),
            Some("arib-std-b67"),
        ] {
            assert!(classify_transfer_tag(tag).is_err());
        }
    }
    #[test]
    fn test_missing_transfer_without_assumption_is_an_error() {
        assert!(resolve_transfer(None, None).is_err());
        assert_eq!(resolve_transfer(None, Some("srgb")).unwrap().0, "srgb");
        let (t, w) = resolve_transfer(Some("bt709"), Some("srgb")).unwrap();
        assert_eq!(t, "srgb");
        assert!(w.iter().any(|w| w.contains("MISMATCH")));
    }
    #[test]
    fn test_luminance_known_values() {
        let y = linear_luminance(&[[65535; 3], [0; 3]], 2, 1, "srgb").unwrap();
        near(y[0], 1., 1e-6);
        near(y[1], 0., 1e-9);
    }
    #[test]
    fn test_roi_level_and_series() {
        near(
            roi_level(&vec![0.25; 10000], 100, 100, [0., 0., 1., 1.])
                .unwrap()
                .unwrap(),
            -2.,
            1e-6,
        );
        let m = roi_series_metrics(&[Some(0.), Some(0.01), Some(0.02), Some(0.03)]);
        near(m.range_ev.unwrap(), 0.03, 3e-8);
        assert_eq!(m.observed_frames, 4);
        assert!(roi_level(&[0.; 100], 10, 10, [0., 0., 1., 1.])
            .unwrap()
            .is_none());
    }
    #[test]
    fn test_geometry_matches_reference() {
        assert_eq!(analysis_geometry(3648, 2736).unwrap(), (912, 684));
        assert_eq!(analysis_geometry(1920, 1080).unwrap(), (912, 504));
    }
}
mod contract_and_boundaries {
    use super::super::measurement::AnalysisSidecar;
    use super::*;
    const FIRST: &str = include_str!("fixtures/frame-first.analysis.json");
    const MID: &str = include_str!("fixtures/frame-mid.analysis.json");
    const PLUS: &str = include_str!("fixtures/frame-first-ev-plus-0.5.analysis.json");
    #[test]
    fn s01_three_frozen_sidecars_field_null_ragged_identity() {
        let mut parsed = vec![];
        for (text, count, ev) in [(FIRST, 73, 0.), (MID, 70, 0.), (PLUS, 84, 0.5)] {
            let s = AnalysisSidecar::parse(text).unwrap();
            let m = &s.measurement;
            assert_eq!(
                (m.width, m.height, m.grid_nx, m.grid_ny),
                (1024, 578, 12, 8)
            );
            assert_eq!(m.valid.len(), 96);
            assert_eq!(m.valid.iter().filter(|v| **v).count(), count);
            assert_eq!(s.exposure_ev, ev);
            assert_eq!(s.roi, [0., 0., 1., 1.]);
            assert_eq!(s.roi_pixel_bounds, [0, 0, 1024, 578]);
            assert_eq!(s.luma_weights, REC709_LUMA);
            near(s.quantum_linear, 1. / 16384., 1e-14);
            assert_eq!(s.floor, 8. / 16384.);
            assert_eq!(m.saturation_mask_quality, "camera_domain");
            assert_eq!(s.output_clip_quality, "delivered_domain");
            assert!(s.analysis_domain.contains("ACEScg"));
            assert_eq!(m.source_sha256.len(), 64);
            let mut lengths = std::collections::BTreeSet::new();
            for (i, row) in m.log_pixel_samples.as_ref().unwrap().iter().enumerate() {
                lengths.insert(row.len());
                assert!(row.iter().all(|v| v.is_finite()));
                assert_eq!(m.log_luminance[i].is_nan(), !m.valid[i]);
                near(m.centers[i][0], ((i % 12) as f64 + 0.5) / 12., 1e-10);
                near(m.centers[i][1], ((i / 12) as f64 + 0.5) / 8., 1e-10);
                assert!(m.log_channel_means.as_ref().unwrap()[i]
                    .iter()
                    .all(|v| v.is_finite()));
            }
            assert_eq!(lengths.into_iter().collect::<Vec<_>>(), vec![396, 418]);
            parsed.push(s);
        }
        let a = &parsed[0].measurement;
        let mid = &parsed[1].measurement;
        let b = &parsed[2].measurement;
        assert_eq!(a.source_sha256, b.source_sha256);
        assert_ne!(a.source_sha256, mid.source_sha256);
        assert_eq!(a.saturation_fraction, b.saturation_fraction);
        assert!(a
            .output_clip_fraction
            .iter()
            .zip(&b.output_clip_fraction)
            .any(|(a, b)| b > a));
        let mut shared = 0;
        for i in 0..K {
            if a.valid[i] && b.valid[i] && a.saturation_fraction[i] < 0.01 {
                near(b.log_luminance[i] - a.log_luminance[i], 0.5, 2e-8);
                shared += 1;
            }
        }
        assert_eq!(shared, 73);
        // Invalid tiles retain finite channels; they must not become pair support.
        let edge = pair_difference(a, b, 1, None).unwrap();
        assert_eq!(edge.support, 18);
        assert!(!edge.reliable());
        assert_eq!(edge.rejects["insufficient_support"], 18);
    }
    #[test]
    fn sidecar_dynamic_grid_and_null_channel_entries() {
        let mut j = crate::json::parse(FIRST).unwrap();
        if let Json::Object(ref mut es) = j {
            for (key, v) in es.iter_mut() {
                if key == "analysis" {
                    if let Json::Object(a) = v {
                        for (k, v) in a {
                            if k == "grid_nx" {
                                *v = Json::Int(4);
                            }
                            if k == "grid_ny" {
                                *v = Json::Int(2);
                            }
                        }
                    }
                }
                if key == "tiles" {
                    if let Json::Array(tiles) = v {
                        tiles.truncate(8);
                        if let Json::Object(tile) = &mut tiles[0] {
                            for (k, v) in tile {
                                if k == "channel_log_means" {
                                    *v = Json::Array(vec![Json::Null; 3]);
                                }
                            }
                        }
                    }
                }
            }
        }
        let s = AnalysisSidecar::from_json(&j).unwrap();
        assert_eq!((s.measurement.grid_nx, s.measurement.grid_ny), (4, 2));
        assert_eq!(s.measurement.log_luminance.len(), 8);
        assert!(s.measurement.log_channel_means.unwrap()[0]
            .iter()
            .all(|v| v.is_nan()));
    }
    #[test]
    fn report_contract_is_finite_and_complete() {
        let mut e = estimate(&sequence(5));
        e.frames[0].common_residual_ev = Some(f64::NAN);
        e.frames[0].applied_ev = Some(2000.);
        let names = vec!["frame.dng".into(); 5];
        let times = vec![None; 5];
        let mut r =
            report::ExposureReport::from_exposure(&e, &Default::default(), &names, &times).unwrap();
        r.gain_probe = Json::Float(f64::INFINITY);
        let text = r.serialize();
        assert!(!text.contains("NaN") && !text.contains("inf"));
        let j = crate::json::parse(&text).unwrap();
        for key in [
            "run_status",
            "config",
            "frames",
            "pairwise_estimation",
            "scene_cuts",
            "source_discontinuities",
            "gain_probe",
            "feedback_history",
            "encoded_temporal_quality",
            "status_counts",
        ] {
            assert!(j.get(key).is_some(), "{key}");
        }
        let f = &j.get("frames").unwrap().as_array().unwrap()[0];
        for key in [
            "index",
            "filename",
            "status",
            "valid_tile_count",
            "consensus_fraction",
            "residual_mad_ev",
            "common_residual_ev",
            "cumulative_confirmed_step_ev",
            "requested_ev",
            "applied_ev",
            "render_ev",
            "gain",
            "render_target_error_ev",
            "new_output_clipping_fraction",
            "measured_level_ev",
            "desired_level_ev",
            "segment_id",
            "warnings",
            "capture_time",
        ] {
            assert!(f.get(key).is_some(), "{key}");
        }
        assert_eq!(f.get("common_residual_ev"), Some(&Json::Null));
        assert_eq!(f.get("gain"), Some(&Json::Null));
    }
    #[test]
    fn malformed_inputs_return_errors_without_panics() {
        assert!(estimate_exposure(&[], &Default::default()).is_err());
        for c in [
            ExposureConfig {
                period: 5,
                ..Default::default()
            },
            ExposureConfig {
                strength: f64::NAN,
                ..Default::default()
            },
            ExposureConfig {
                strength: 1.1,
                ..Default::default()
            },
            ExposureConfig {
                estimator: "unknown".into(),
                ..Default::default()
            },
            ExposureConfig {
                ev_limits: Some((2., -2.)),
                ..Default::default()
            },
            ExposureConfig {
                exclude_rois: vec![[0., 0., f64::NAN, 1.]],
                ..Default::default()
            },
            ExposureConfig {
                steps: vec![ExposureStep {
                    start_frame: 99,
                    delta_ev: 1.,
                }],
                ..Default::default()
            },
        ] {
            assert!(estimate_exposure(&sequence(9), &c).is_err());
        }
        let mut ms = sequence(10);
        ms[2].valid.pop();
        assert!(estimate_exposure(&ms, &Default::default()).is_err());
        assert!(pair_difference(&ms[0], &ms[2], 1, None).is_err());
        assert!(build_edges(&sequence(8), &[], None, &[0]).is_err());
        assert!(solve_exposure_graph(2, &[graph_edge(0, 3, 1.)]).is_err());
        assert!(huber_whittaker(&[0.; 8], &[true; 7], 60).is_err());
        assert!(solve_weighted(&[0.; 8], &[0.; 8], 1.).is_err());
        assert!(lambda_for_period(5).is_err());
        assert!(AnalysisSidecar::parse("{}").is_err());
        assert!(AnalysisSidecar::parse(
            &FIRST.replace("\"log_luminance\":null", "\"log_luminance\":1")
        )
        .is_err());
        assert!(crate::json::parse("1e999").is_err());
        assert!(crate::json::parse(&format!("{}0{}", "[".repeat(1000), "]".repeat(1000))).is_err());
        assert!(analysis_geometry(0, 1).is_err());
        assert!(analysis_geometry(1, usize::MAX).is_err());
        assert!(tile_grid(&[1.; 16], 4, 4, 3, 2).is_err());
        assert!(inverse_transfer(&[f64::MAX], "srgb").is_err());
        assert!(linear_luminance(&[], 1, 1, "linear16").is_err());
    }
    #[test]
    fn confirmed_steps_strength_and_frozen_requested_not_clamped() {
        let x = background(&(0..30).map(|t| alt(t, 4.)).collect::<Vec<_>>(), 0., 0.);
        let mut c = cfg("uniform", "legacy");
        c.ev_limits = Some((-2., 2.));
        c.strength = 0.8;
        let e = estimate_exposure(&frames(&x), &c).unwrap();
        assert!(e.frames.iter().any(|f| f.status == "LIMITED"));
        for t in 0..30 {
            near(
                e.frozen_target[t][0],
                x[t][0] + e.frames[t].requested_ev.unwrap(),
                1e-12,
            );
        }
    }
    #[test]
    fn short_gap_interpolation_limits_and_no_edge_extrapolation() {
        // Measurable, pair-incompatible islands; samples omitted, gates split them.
        for (intent, max_gap) in [("smooth", 7), ("uniform", 3)] {
            for gap in [max_gap, max_gap + 1] {
                let n = 60;
                let mut rows =
                    background(&(0..n).map(|t| alt(t, 0.1)).collect::<Vec<_>>(), -0.1, 0.1);
                for t in 25..25 + gap {
                    for i in 0..K {
                        rows[t][i] +=
                            0.4 * (t - 24) as f64 + 2. * ((i * 7 + t * 13) % K) as f64 / K as f64;
                    }
                }
                let mut ms = frames(&rows);
                for (t, m) in ms.iter_mut().enumerate() {
                    m.log_pixel_samples = Some(vec![
                        (0..25)
                            .map(|p| if (25..25 + gap).contains(&t) {
                                p as f64 * 0.1 * (t - 24) as f64
                            } else {
                                0.
                            })
                            .collect();
                        K
                    ]);
                }
                let e = estimate_exposure(&ms, &cfg(intent, "pairwise")).unwrap();
                if gap <= max_gap {
                    assert!(e.frames[25..25 + gap]
                        .iter()
                        .all(|f| f.status == "INTERPOLATED_ESTIMATE"
                            || intent == "uniform" && f.status == "ABSOLUTE_LEVEL"));
                } else if intent == "smooth" {
                    assert!(e.frames[25..25 + gap]
                        .iter()
                        .any(|f| f.applied_ev.is_none()));
                }
            }
        }
        // Legacy interpolates <=3 even when no tile is valid, never edge gaps.
        for gap in [3, 4] {
            let mut m = sequence(60);
            m[0].valid.fill(false);
            for f in &mut m[25..25 + gap] {
                f.valid.fill(false);
            }
            let e = estimate_exposure(&m, &cfg("smooth", "legacy")).unwrap();
            assert_eq!(e.frames[0].status, "REVIEW_REQUIRED");
            assert!(e.frames[25..25 + gap].iter().all(|f| if gap == 3 {
                f.status == "INTERPOLATED_ESTIMATE"
            } else {
                f.status == "REVIEW_REQUIRED"
            }));
        }
    }
    #[test]
    fn unverifiable_island_interrupts_uniform_stitch_chain() {
        let mut rows = spliced(80, 35).0;
        for (i, v) in rows[35].iter_mut().enumerate() {
            *v += 4. * i as f64 / K as f64;
        }
        let mut ms = frames(&rows);
        ms[35].valid.fill(false);
        ms[35].log_luminance.fill(f64::NAN);
        let e = estimate_exposure(&ms, &cfg("uniform", "pairwise")).unwrap();
        let entries = e
            .pairwise_summary
            .get("cross_boundary_anchors")
            .unwrap()
            .get("entries")
            .unwrap()
            .as_array()
            .unwrap();
        assert!(entries.iter().any(|v| v
            .get("note")
            .is_some_and(|v| v.as_str().unwrap().contains("interrupts"))));
        assert!(e.frames[35].applied_ev.is_none());
    }
    #[test]
    fn pair_gates_structure_color_saturation_and_quadrants() {
        let a = measurement(vec![-2.; K], true);
        let mut b = a.clone();
        for i in 0..K {
            b.log_luminance[i] += 0.2;
            for v in &mut b.log_pixel_samples.as_mut().unwrap()[i] {
                *v += 0.2;
            }
            for v in &mut b.log_channel_means.as_mut().unwrap()[i] {
                *v += 0.2;
            }
        }
        assert!(pair_difference(&a, &b, 1, None).unwrap().reliable());
        let mut color = b.clone();
        for c in color.log_channel_means.as_mut().unwrap() {
            c[0] += 0.081;
        }
        let e = pair_difference(&a, &color, 1, None).unwrap();
        assert!(!e.reliable());
        assert_eq!(e.rejects["colour_change"], K);
        let mut shape = b.clone();
        for row in shape.log_pixel_samples.as_mut().unwrap() {
            for (i, v) in row.iter_mut().enumerate() {
                *v += 0.1 * (i as f64 - 12.);
            }
        }
        assert!(!pair_difference(&a, &shape, 1, None).unwrap().reliable());
        let mut sat = b.clone();
        sat.saturation_fraction.fill(0.01);
        assert!(!pair_difference(&a, &sat, 1, None).unwrap().reliable());
        let train: Vec<_> = (0..K).map(|i| i / 12 < 4).collect();
        assert!(!pair_difference(&a, &b, 1, Some(&train)).unwrap().reliable());
    }
    #[test]
    fn long_lag_and_edge_rejection_stats() {
        let ms = sequence(40);
        let (edges, stats) = build_edges(&ms, &[20], None, &DEFAULT_LAGS).unwrap();
        assert!(edges.iter().any(|e| e.lag == 16));
        assert!(edges.iter().all(|e| !(e.i < 20 && e.j >= 20)));
        assert_eq!(stats.rejected_edges, 0);
        assert_eq!(stats.candidate_edges, edges.len());
        let mut bad = ms;
        for m in &mut bad {
            m.valid.fill(false);
        }
        let (edges, stats) = build_edges(&bad, &[], None, &DEFAULT_LAGS).unwrap();
        assert!(edges.is_empty());
        assert_eq!(stats.candidate_edges, stats.rejected_edges);
        assert!(stats.reject_reasons["no_shared_valid_tiles"] > 0);
    }
    #[test]
    fn trend_short_nonfinite_and_nonconvergence_records() {
        let z = huber_whittaker(&[1.; 7], &[true; 7], 60).unwrap();
        assert!(!z.converged && z.iterations == 0 && z.trend.iter().all(|v| v.is_nan()));
        let mut y = vec![0.; 30];
        y[0] = 1.;
        let z = trends::huber_whittaker_with(&y, &[true; 30], 60, 0.1, 1, 0.).unwrap();
        assert!(!z.converged && z.iterations == 1);
        let mut ms = sequence(30);
        for (t, m) in ms.iter_mut().enumerate() {
            m.log_luminance.fill(if t % 2 == 0 { -1. } else { 0. });
        }
        let z = estimate(&ms);
        assert!(z
            .pairwise_summary
            .get("linear_solves")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .all(|j| j.get("converged") == Some(&Json::Bool(true))));
    }
    #[test]
    fn feedback_two_updates_frozen_and_unobservable_semantics() {
        let x = base_sequence(30);
        let mut e = estimate(&frames(&x));
        let frozen = e.frozen_target.clone();
        let desired: Vec<_> = e.frames.iter().map(|f| f.desired_level_ev).collect();
        let mut updates = 0;
        for i in 0..=MAX_FEEDBACK_UPDATES {
            let r = rendered(&e, &x, 0.8, None);
            let s = evaluate_render(&mut e, &r, i).unwrap();
            if s.passed {
                break;
            }
            if i < MAX_FEEDBACK_UPDATES {
                feedback_update(&mut e, &s, None).unwrap();
                updates += 1;
            }
        }
        assert!(updates <= 2);
        assert_eq!(e.frozen_target, frozen);
        assert_eq!(
            e.frames
                .iter()
                .map(|f| f.desired_level_ev)
                .collect::<Vec<_>>(),
            desired
        );
        let mut r = rendered(&e, &x, 1., None);
        for m in &mut r {
            m.valid.fill(false);
        }
        let s = evaluate_render(&mut e, &r, 0).unwrap();
        assert!(!s.passed && s.median_abs_error_ev.is_none());
        assert_eq!(s.unobservable_frames.len(), 30);
        let d = estimate(&sequence(5));
        let mut d = d;
        let s = evaluate_render(&mut d, &sequence(5), 0).unwrap();
        assert!(
            !s.passed && s.errors.iter().all(Option::is_none) && s.unobservable_frames.is_empty()
        );
    }
    #[test]
    fn declared_linear16_blur_mirror_and_audit_gates() {
        assert_eq!(
            inverse_transfer(&[0., 16384., 32768., 65535.], "linear16").unwrap(),
            vec![0., 1., 2., 65535. / 16384.]
        );
        let y = linear_luminance(&[[16384; 3]], 1, 1, "linear16").unwrap();
        near(y[0], 1., 1e-12);
        let y = structure_blur(&[1., 0., 0., 0., 0.], 5, 1).unwrap();
        let mut kernel: [f64; 5] = [0.; 5];
        for (i, v) in kernel.iter_mut().enumerate() {
            *v = (-0.5 * ((i as f64 - 2.) / 1.2).powi(2)).exp();
        }
        let total: f64 = kernel.iter().sum();
        near(y[0], kernel[2] / total, 1e-12);
        near(y[1], kernel[1] / total, 1e-12);
        near(y[2], kernel[0] / total, 1e-12);
        assert_eq!(
            segment_status(24, &temporal_summary(&[Some(0.); 23])),
            "PASS"
        );
        assert_eq!(
            segment_status(23, &temporal_summary(&[Some(0.); 22])),
            "SHORT_CONTEXT"
        );
        assert_eq!(
            segment_status(30, &temporal_summary(&[None; 29])),
            "UNOBSERVABLE"
        );
        assert_eq!(
            segment_status(30, &temporal_summary(&[Some(0.026); 29])),
            "FAIL"
        );
        let mut pairs = vec![Some(0.); 29];
        pairs[15] = Some(0.051);
        assert_eq!(segment_status(30, &temporal_summary(&pairs)), "FAIL");
    }
}
mod audit_evidence_boundaries {
    use super::*;
    fn stats(level: f64) -> TileStats {
        TileStats {
            log_mean: vec![level; 432],
            log_pixels: vec![vec![level; 25]; 432],
            saturation_fraction: vec![0.; 432],
        }
    }
    #[test]
    fn source_boundary_pair_never_enters_temporal_evidence() {
        let features: Vec<_> = (0..60)
            .map(|i| stats(if i >= 30 { -1.5 } else { -2. }))
            .collect();
        let r = audit_features(&features, 24, 18, &[30]).unwrap();
        assert_eq!(r.status, "PASS");
        assert_eq!(r.candidate_count, 0);
        assert_eq!(r.pairs[29].metric.quality, "SOURCE_DISCONTINUITY");
        assert!(!r.pairs[29].candidate_global_jump);
        assert_eq!(r.segments[1].summary.pairs, 30);
        assert_eq!(r.segments[1].summary.usable_pairs, 29);
        assert_eq!(r.segments[1].summary.curvature_rms_ev, Some(0.));
        let full = audit_features(&features, 24, 18, &[]).unwrap();
        assert_eq!(full.status, "FAIL");
        assert_eq!(full.candidate_count, 1);
    }
    #[test]
    fn audit_gates_keep_short_unobservable_and_fail_distinct() {
        let dark = vec![stats(-40.); 30];
        let r = audit_features(&dark, 24, 18, &[]).unwrap();
        assert_eq!(r.status, "UNOBSERVABLE");
        let short = vec![stats(-2.); 8];
        assert_eq!(
            audit_features(&short, 24, 18, &[]).unwrap().status,
            "SHORT_CONTEXT"
        );
        let alternate: Vec<_> = (0..30).map(|t| stats(-2. + alt(t, 0.1))).collect();
        let r = audit_features(&alternate, 24, 18, &[]).unwrap();
        assert_eq!(r.status, "FAIL");
        assert_eq!(r.candidate_count, 29);
        assert_eq!(
            audit_features(&[], 24, 18, &[]).unwrap().status,
            "UNOBSERVABLE"
        );
    }
}
