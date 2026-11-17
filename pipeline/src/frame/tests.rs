    use super::*;
use std::fs;
use std::path::PathBuf;


    #[test]
    fn formats_white_balance_without_precision_loss() {
        let wb = [0.605080831408776, 1.0, 0.48250460405156537];
        let text = format_wb(wb);
        assert_eq!(text.matches(',').count(), 2);
        let parsed: Vec<f64> = text.split(',').map(|v| v.parse().unwrap()).collect();
        assert_eq!(parsed, wb.to_vec());
    }

    #[test]
    fn white_balance_comparison_is_strict() {
        let reference = [0.605080831408776, 1.0, 0.48250460405156537];
        assert!(wb_close(reference, reference));
        assert!(!wb_close(
            reference,
            [0.605080831408777, 1.0, 0.48250460405156537]
        ));
    }

    #[test]
    fn sidecar_ev_accepts_only_producer_precision() {
        let requested = 0.0920620700999999f64;
        let producer: f64 = "0.0920620701".parse().expect("producer decimal");
        assert_ne!(producer, requested);
        assert!(sidecar_ev_matches(producer, requested));
        assert!(sidecar_ev_matches(requested, requested));
        assert!(sidecar_ev_matches(-producer, -requested));
        assert!(sidecar_ev_matches(0.0, 0.0));
        assert!(!sidecar_ev_matches(0.0, requested));
        // Materially wrong gains are still rejected (well beyond half ulp).
        assert!(!sidecar_ev_matches(requested + 1e-6, requested));
        assert!(!sidecar_ev_matches(requested + 1e-9, requested));
        assert!(!sidecar_ev_matches(requested + 1e-10, requested));
        // 10 significant digits at this magnitude -> decimal step 1e-11,
        // so the half-ulp bound is 5e-12.
        assert!(sidecar_ev_matches(requested + 4e-12, requested));
    }

    #[test]
    fn formats_ev_and_axes_round_trip() {
        for ev in [0.0, 0.25, -0.25, 0.1, 1.0 / 3.0] {
            assert_eq!(format_ev(ev).parse::<f64>().unwrap(), ev);
            assert_eq!(format_axis(ev).parse::<f64>().unwrap(), ev);
        }
    }

    #[cfg(unix)]
    #[test]
    fn canonical_source_comparison_resolves_symlinks() {
        use std::os::unix::fs::symlink;
        let dir = std::env::var_os("RAWTL_TEST_WORK").map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp"))
            .join(format!("frame-source-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create test dir");
        let real = dir.join("real.DNG");
        fs::write(&real, b"data").expect("write real file");
        let link = dir.join("link.DNG");
        symlink(&real, &link).expect("create symlink");
        let (expected, reported) =
            canonical_source_pair(link.to_str().expect("utf8"), &real).expect("canonicalize");
        assert_eq!(expected, reported);
        assert_eq!(expected, fs::canonicalize(&real).expect("canonical"));
    }
