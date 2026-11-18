    use super::*;
use crate::json::Json;
use crate::order::SourceFrame;
use std::fs;
use std::path::{Path, PathBuf};

    use crate::cli::WhiteBalance;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;
    use std::sync::OnceLock;

    fn fake_binary() -> PathBuf {
        static BINARY: OnceLock<PathBuf> = OnceLock::new();
        BINARY
            .get_or_init(|| {
                let directory = std::env::var_os("RAWTL_TEST_WORK").map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp"))
                    .join("fake-native-bin");
                fs::create_dir_all(&directory).expect("create fake native dir");
                let binary = directory.join("fake-native-prepared");
                let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("tests")
                    .join("common")
                    .join("fake_native_main.rs");
                let output = Command::new("rustc")
                    .args(["--edition", "2021", "-O"])
                    .arg(&source)
                    .arg("-o")
                    .arg(&binary)
                    .output()
                    .expect("run rustc for the fake native producer");
                assert!(
                    output.status.success(),
                    "fake native compilation failed:\n{}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                binary
            })
            .clone()
    }

    fn shell_quote(text: &str) -> String {
        format!("'{}'", text.replace('\'', "'\\''"))
    }

    fn work_dir(name: &str) -> PathBuf {
        let dir = std::env::var_os("RAWTL_TEST_WORK").map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp"))
            .join(format!("prepared-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create test dir");
        dir
    }

    fn source_frame(dir: &std::path::Path, name: &str) -> SourceFrame {
        let input = dir.join("input");
        fs::create_dir_all(&input).expect("create input dir");
        let path = input.join(name);
        fs::write(&path, b"synthetic raw bytes").expect("write source");
        SourceFrame {
            path,
            name: name.to_string(),
            key: Vec::new(),
        }
    }

    /// Write a fake-producer config and a wrapper that records every argv.
    fn fake_tool(dir: &std::path::Path, frames: &[&str], extra: &str) -> PathBuf {
        let config = dir.join("fake.config");
        let text = format!(
            "width=8\nheight=6\ngrid_nx=4\ngrid_ny=2\n{extra}\n{}\n",
            frames.join("\n")
        );
        fs::write(&config, text).expect("write fake config");
        let argv = dir.join("argv.log");
        let wrapper = dir.join("fake-dng-linear-frame");
        let script = format!(
            "#!/bin/sh\nexport FAKE_NATIVE_CONFIG={}\nprintf '%s\\n' \"$@\" >> {}\nexec {} \"$@\"\n",
            shell_quote(&config.display().to_string()),
            shell_quote(&argv.display().to_string()),
            shell_quote(&fake_binary().display().to_string())
        );
        fs::write(&wrapper, script).expect("write wrapper");
        let mut permissions = fs::metadata(&wrapper).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&wrapper, permissions).expect("chmod wrapper");
        wrapper
    }

    const FRAME_1: &str = "frame=IMG_1.DNG|0.5,1,0.8|0.0|0.0|-|-|flat";
    const FRAME_2: &str = "frame=IMG_2.CR3|0.6,1,0.7|0.2|0.3|2026:11:13 04:05:06|-|flat";

    fn prepared_settings() -> PreparedSettings {
        PreparedSettings {
            lens: Default::default(),
            camera_recipe: crate::camera::CameraRecipe::Generic,
            preview_edge: None,
        }
    }

    fn prepare_one(
        dir: &std::path::Path,
        tool: &Path,
        source: &SourceFrame,
        name: &str,
    ) -> PreparedFrame {
        let scratch = dir.join(format!("scratch-{name}"));
        fs::create_dir_all(&scratch).expect("create scratch");
        prepare(
            tool,
            source,
            &PrepareSettings {
                lens: Default::default(),
                scratch_dir: &scratch,
                context_path: &dir.join(format!("{name}.ctx")),
                log: &dir.join(format!("{name}-prepare.log")),
                converter: None,
                converter_timeout: None,
            },
        )
        .expect("prepare")
    }

    #[test]
    fn prepared_round_trip_uses_the_context_and_validates_metadata() {
        let dir = work_dir("round-trip");
        let source = source_frame(&dir, "IMG_1.DNG");
        let tool = fake_tool(&dir, &[FRAME_1], "");
        let prepared = prepare_one(&dir, &tool, &source, "direct");
        assert_eq!(prepared.route, PreparedRoute::Direct);
        assert!(!prepared.generated);
        assert_eq!(prepared.source, fs::canonicalize(&source.path).unwrap());
        assert_eq!(prepared.dng, prepared.source);
        assert_eq!(prepared.source_sha256, prepared.dng_sha256);
        assert_eq!(
            (prepared.main_ifd_width, prepared.main_ifd_height),
            (8, 6)
        );
        assert_eq!(prepared.capture_time, None);
        assert_eq!(prepared.as_shot_neutral, [0.5, 1.0, 0.8]);

        let logs = dir.join("logs");
        let settings = prepared_settings();
        let analysis =
            analyze_prepared(&tool, &prepared, None, Some(0.0), None, &settings, &logs.join("frame_000001.log")).expect("analyze");
        assert_eq!(analysis.index, 1);
        assert_eq!(analysis.source_name, "IMG_1.DNG");
        assert_eq!(analysis.applied_wb, prepared.as_shot_neutral);
        assert_eq!(analysis.wb_source, WB_SOURCE_AS_SHOT);
        assert_eq!(
            analysis.source_sha256.as_deref(),
            Some(prepared.source_sha256.as_str())
        );
        assert!(analysis_bytes(&analysis)
            .expect("sidecar")
            .starts_with(b"{\"schema_version\":2"));

        let mut pixel_bytes = 0usize;
        let developed = develop_prepared(&tool, &prepared, None, Some(0.25), &settings, &logs.join("frame_000001.log"), &mut |info, native| {
            let pixels = native.pixels(info.width, info.height)?;
            pixel_bytes = pixels.len();
            assert!(pixels
                .chunks_exact(4)
                .all(|b| f32::from_le_bytes(b.try_into().unwrap()).is_finite()));
            Ok(())
        })
        .expect("develop");
        assert_eq!(pixel_bytes, 8 * 6 * 12);
        assert_eq!(developed.exposure_ev, 0.25);

        let overridden = analyze_prepared(
            &tool,
            &prepared,
            Some([0.7, 1.0, 0.6]),
            None,
            Some([0.0, 0.0, 0.5, 0.5]),
            &settings,
            &logs.join("frame_000002.log"),
        )
        .expect("override analyze");
        assert_eq!(overridden.wb_source, WB_SOURCE_OVERRIDE);
        assert_eq!(overridden.applied_wb, [0.7, 1.0, 0.6]);

        // Every consumer was invoked with --prepared and never with Converter
        // options.
        let argv = fs::read_to_string(dir.join("argv.log")).expect("argv log");
        assert_eq!(argv.lines().filter(|line| *line == "--prepared").count(), 3);
        assert!(!argv
            .lines()
            .any(|line| line == "--converter" || line == "--converter-timeout"));
        assert_eq!(prepared.dng, fs::canonicalize(&source.path).unwrap());
    }

    #[test]
    #[ignore = "controlled synthetic SDK boundary; requires RAWTL_NATIVE_TOOL and RAWTL_CONTROLLED_DNG"]
    fn generic_controlled_native_round_trip() {
        let dir = work_dir("generic-real-sdk");
        let source_path = fs::canonicalize(std::env::var_os("RAWTL_CONTROLLED_DNG").expect("fixture")).unwrap();
        let tool = PathBuf::from(std::env::var_os("RAWTL_NATIVE_TOOL").expect("matched native"));
        let source = SourceFrame { name: source_path.file_name().unwrap().to_string_lossy().into(), path: source_path, key: Vec::new() };
        let prepared = prepare_one(&dir, &tool, &source, "generic");
        let settings = PreparedSettings { lens: Default::default(), camera_recipe: crate::camera::CameraRecipe::Generic, preview_edge: None };
        let wb = Some([0.6050808314, 1.0, 0.4825046041]);
        let analysis = analyze_prepared(&tool, &prepared, wb, Some(0.0), None, &settings, &dir.join("analysis.log")).unwrap();
        let mut reference: Vec<f32> = Vec::new();
        for ev in [0.0, -2.0, 2.0] {
            let info = develop_prepared(&tool, &prepared, wb, Some(ev), &settings, &dir.join(format!("develop-{ev}.log")), &mut |info, stream| {
                let p = stream.pixels(info.width, info.height)?;
                let values: Vec<f32> = p.chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).collect();
                if ev == 0.0 { reference = values; } else {
                    for (a,b) in values.iter().zip(&reference) { assert!((*a - *b * 2f32.powf(ev as f32)).abs() <= 2e-6 * a.abs().max(1.0)); }
                }
                Ok(())
            }).unwrap();
            assert_eq!(info.camera_color, analysis.camera_color);
            assert_eq!(info.source_sha256, analysis.source_sha256);
            assert_eq!(info.applied_wb, analysis.applied_wb);
        }
        for name in ["analysis.log", "develop-0.log", "develop--2.log", "develop-2.log"] {
            let text=fs::read_to_string(dir.join(name)).unwrap();
            assert!(!text.contains("--profile-correction"), "{text}");
            for expected in ["--camera-recipe generic", "--camera-color-method dng-camera-to-pcs", "--camera-calibration none", "--color-contract-version 2"] { assert!(text.contains(expected), "{text}"); }
            assert!(!text.contains("reference-gain"));
        }
    }

    #[test]
    fn prepared_converter_route_and_canon_method_guard() {
        let dir = work_dir("converter-route");
        let source = source_frame(&dir, "IMG_2.CR3");
        let tool = fake_tool(&dir, &[FRAME_2], "");
        let prepared = prepare_one(&dir, &tool, &source, "vendor");
        assert_eq!(prepared.route, PreparedRoute::VendorRaw);
        assert!(prepared.generated);
        assert_ne!(prepared.dng, prepared.source);
        assert!(prepared.dng.exists());
        assert_eq!(
            prepared.dng,
            fs::canonicalize(dir.join("scratch-vendor/linear/IMG_2.dng")).unwrap()
        );
        assert_eq!(
            prepared.capture_time.as_deref(),
            Some("2026:11:13 04:05:06")
        );

        let logs = dir.join("canon-logs");
        let settings = PreparedSettings {
            lens: Default::default(),
            camera_recipe: crate::camera::CameraRecipe::CanonCr3,
            preview_edge: None,
        };
        let info = analyze_prepared(&tool, &prepared, None, Some(0.0), None, &settings, &logs.join("frame_000001.log"))
            .expect("canon analyze");
        assert_eq!(
            info.camera_color
                .as_ref()
                .and_then(|color| color.get("camera_model"))
                .and_then(Json::as_str),
            Some("Canon EOS R7")
        );
        develop_prepared(&tool, &prepared, Some([0.6, 1.0, 0.7]), Some(0.0), &settings, &logs.join("frame_000001.log"), &mut |info, native| {
            native.pixels(info.width, info.height)?;
            Ok(())
        })
        .expect("canon develop");
        assert!(prepared.dng.exists(), "consumers must not delete the cache");

        // The release Canon method refuses a prepared LinearRaw DNG before any
        // native process starts.
        let dng_source = source_frame(&dir, "IMG_1.DNG");
        let dng_tool = fake_tool(&dir, &[FRAME_1], "");
        let dng_prepared = prepare_one(&dir, &dng_tool, &dng_source, "dng");
        let error = analyze_prepared(&tool, &dng_prepared, None, None, None, &settings, &logs.join("frame_000001.log"))
            .expect_err("Canon method requires CR3");
        assert!(
            error.message.contains("requires original Canon CR3"),
            "{}",
            error.message
        );
    }

    #[test]
    fn delivery_requires_recovery_contract_and_analysis_disables_it() {
        let dir = work_dir("highlight-contract");
        let source = source_frame(&dir, "IMG_1.DNG");
        let base = fake_tool(&dir, &[FRAME_1], "");
        let prepared = prepare_one(&dir, &base, &source, "base");
        let settings = PreparedSettings { lens: Default::default(), camera_recipe: crate::camera::CameraRecipe::Generic, preview_edge: None };
        let analysis = analyze_prepared(&base, &prepared, None, Some(0.), None, &settings, &dir.join("analysis.log")).unwrap();
        assert_eq!(analysis.highlight_recovery.as_ref().and_then(|r| r.get("requested")).and_then(Json::as_bool), Some(false));
        for defect in ["highlight-missing", "highlight-disabled", "highlight-version", "highlight-status"] {
            let tool = fake_tool(&dir, &[FRAME_1], &format!("rev_defect={defect}"));
            let error = develop_prepared(&tool, &prepared, None, Some(0.), &settings, &dir.join(format!("{defect}.log")), &mut |info, native| {
                native.pixels(info.width, info.height)?;
                Ok(())
            }).expect_err(defect);
            assert!(error.message.contains("highlight recovery contract"), "{defect}: {}", error.message);
        }
    }

    #[test]
    fn prepared_metadata_identity_defects_are_rejected() {
        let dir = work_dir("metadata-defects");
        let source = source_frame(&dir, "IMG_2.CR3");
        let base = fake_tool(&dir, &[FRAME_2], "");
        let prepared = prepare_one(&dir, &base, &source, "base");
        let settings = prepared_settings();
        for (defect, needle) in [
            ("route", "route"),
            ("dng", "DNG identity"),
            ("sha", "DNG identity"),
            ("generated", "generated"),
            ("consumed", "inconsistent"),
            ("missing", "missing"),
        ] {
            let tool = fake_tool(
                &dir,
                &[FRAME_2],
                &format!("prepared_metadata_defect={defect}"),
            );
            let error = analyze_prepared(&tool, &prepared, None, None, None, &settings, &dir.join("defect.log"))
                .expect_err(defect);
            assert!(
                error.message.contains(needle),
                "{defect}: {}",
                error.message
            );
        }
    }

    #[test]
    fn prepared_context_parser_and_prepare_failures_are_explicit() {
        let dir = work_dir("parser");
        let source = source_frame(&dir, "IMG_1.DNG");
        let tool = fake_tool(&dir, &[FRAME_1], "");
        let scratch = dir.join("scratch");
        fs::create_dir_all(&scratch).unwrap();
        let context = dir.join("IMG_1.ctx");
        let settings = PrepareSettings {
            lens: Default::default(),
            scratch_dir: &scratch,
            context_path: &context,
            log: &dir.join("prepare.log"),
            converter: None,
            converter_timeout: None,
        };
        prepare(&tool, &source, &settings).expect("prepare");
        let valid = fs::read_to_string(&context).expect("context");

        // A missing scratch directory is rejected before any conversion.
        let missing = PrepareSettings {
            lens: Default::default(),
            scratch_dir: &dir.join("absent"),
            context_path: &dir.join("absent.ctx"),
            log: &dir.join("absent.log"),
            converter: None,
            converter_timeout: None,
        };
        assert!(prepare(&tool, &source, &missing).is_err());

        // The fake fails the conversion and the Rust side surfaces its log.
        let failing_tool = fake_tool(&dir, &[FRAME_1], "prepare_fail=1");
        let error = prepare(&failing_tool, &source, &settings).expect_err("prepare failure");
        assert!(error.message.contains("prepared-frame conversion failed"), "{}", error.message);
        assert!(dir.join("prepare.log").exists());

        // A context that names another source is rejected by prepare().
        let other = source_frame(&dir, "OTHER.DNG");
        let swapped_tool = fake_tool(
            &dir,
            &[FRAME_1],
            &format!(
                "prepared_source_override={}\nprepared_dng_override={}",
                other.path.display(),
                other.path.display()
            ),
        );
        let error = prepare(&swapped_tool, &source, &settings).expect_err("source swap");
        assert!(error.message.contains("instead of the requested source"), "{}", error.message);

        // Structural grammar failures fail closed.
        for (name, text) in [
            ("header", valid.replacen("rawtl-prepared-frame/2", "rawtl-prepared-frame/1", 1)),
            ("missing", valid.replace("route=linearraw-sdk\n", "")),
            ("duplicate", format!("{valid}route=linearraw-sdk\n")),
            ("escape", valid.replace("source=", "source=%zz")),
        ] {
            let path = dir.join(format!("{name}.ctx"));
            fs::write(&path, text).unwrap();
            assert!(PreparedFrame::load(&path).is_err(), "{name}");
        }

        // Relative identities are never resolved from the current directory.
        let relative = valid
            .replace(
                &format!("source={}", source.path.display()),
                "source=input/IMG_1.DNG",
            )
            .replace(
                &format!("prepared_dng={}", source.path.display()),
                "prepared_dng=input/IMG_1.DNG",
            );
        let path = dir.join("relative.ctx");
        fs::write(&path, relative).unwrap();
        assert!(PreparedFrame::load(&path).is_err());

        // Route/generated consistency and the direct-cache rule.
        let mismatched = valid.replace("generated=0", "generated=1");
        let path = dir.join("generated.ctx");
        fs::write(&path, mismatched).unwrap();
        assert!(PreparedFrame::load(&path).is_err());

        // A percent-escaped path round-trips through the parser.
        let odd_dir = dir.join("odd % name");
        fs::create_dir_all(&odd_dir).unwrap();
        let odd = odd_dir.join("odd % frame.dng");
        fs::write(&odd, b"odd").unwrap();
        let canonical = fs::canonicalize(&odd).unwrap();
        let sha = crate::digest::sha256(b"odd");
        let escaped = canonical.display().to_string().replace('%', "%25").replace(' ', "%20");
        let context_text = format!(
            "rawtl-prepared-frame/2\nsource={escaped}\nsource_sha256={sha}\nroute=linearraw-sdk\nprepared_dng={escaped}\nprepared_sha256={sha}\ngenerated=0\nmain_ifd_width=8\nmain_ifd_height=6\nas_shot_neutral=0.5,1,0.8\nhas_capture_time=0\nconverter_used=0\n"
        );
        let path = dir.join("escaped.ctx");
        fs::write(&path, context_text).unwrap();
        let loaded = PreparedFrame::load(&path).expect("escaped context");
        assert_eq!(loaded.dng, canonical);
        assert_eq!(loaded.source, canonical);
    }

    fn converter_wrapper(dir: &std::path::Path, name: &str, counter: &std::path::Path) -> PathBuf {
        let real = std::env::var("RAWTL_REAL_CONVERTER").unwrap_or_else(|_| {
            "/Applications/Adobe DNG Converter.app/Contents/MacOS/Adobe DNG Converter".to_string()
        });
        let path = dir.join(name);
        let script = format!(
            "#!/bin/sh\necho call >> {}\nexec {} \"$@\"\n",
            shell_quote(&counter.display().to_string()),
            shell_quote(&real)
        );
        fs::write(&path, script).unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).unwrap();
        path
    }

    fn counter_lines(path: &std::path::Path) -> usize {
        fs::read_to_string(path)
            .map(|text| text.lines().count())
            .unwrap_or(0)
    }

    fn pixel_digest(config: &ProduceConfig<'_>, tool: &Path, sources: &[SourceFrame], evs: &[f64], digests: &mut Vec<String>, infos: &mut Vec<FrameInfo>) {
        let sequence = stream_frames(tool, sources, evs, config, Default::default(), &mut |info, native| {
            let pixels = native.pixels(info.width, info.height)?;
            digests.push(crate::digest::sha256(&pixels));
            Ok(())
        })
        .expect("legacy stream");
        infos.extend(sequence.frames);
    }

    /// Real-material oracle. Opt-in because it needs the built native tool and
    /// staged Canon CR3s:
    /// `RAWTL_NATIVE_TOOL=... RAWTL_REAL_CR3=a.CR3,b.CR3 RAWTL_PREPARED_WORK=dir \
    ///  cargo test --bin rawtl-linear-video -- --ignored --test-threads=1`
    #[test]
    #[ignore = "requires RAWTL_NATIVE_TOOL, RAWTL_REAL_CR3 and RAWTL_PREPARED_WORK"]
    fn prepared_context_on_real_native_frames() {
        let tool = PathBuf::from(
            std::env::var_os("RAWTL_NATIVE_TOOL").expect("RAWTL_NATIVE_TOOL"),
        );
        let materials: Vec<PathBuf> = std::env::var("RAWTL_REAL_CR3")
            .expect("RAWTL_REAL_CR3")
            .split(',')
            .map(PathBuf::from)
            .collect();
        assert!(!materials.is_empty() && materials.len() <= 2);
        let work = PathBuf::from(std::env::var_os("RAWTL_PREPARED_WORK").expect("RAWTL_PREPARED_WORK"));
        fs::create_dir_all(&work).expect("create evidence dir");
        // Only this test's own artifacts are removed; unknown entries stay.
        for entry in fs::read_dir(&work).expect("read evidence dir") {
            let entry = entry.expect("evidence entry");
            let name = entry.file_name().to_string_lossy().into_owned();
            let owned = name.starts_with("scratch-")
                || name.ends_with(".ctx")
                || name.ends_with(".log")
                || name.starts_with("converter-")
                || name.ends_with("-calls.txt")
                || name.ends_with("-logs");
            if !owned {
                continue;
            }
            let path = entry.path();
            if entry.file_type().expect("entry type").is_dir() {
                fs::remove_dir_all(&path).expect("remove old evidence dir");
            } else {
                fs::remove_file(&path).expect("remove old evidence file");
            }
        }
        let prepared_counter = work.join("prepared-converter-calls.txt");
        let legacy_counter = work.join("legacy-converter-calls.txt");
        let prepared_converter = converter_wrapper(&work, "converter-prepared", &prepared_counter);
        let legacy_converter = converter_wrapper(&work, "converter-legacy", &legacy_counter);
        let prepared_logs = work.join("prepared-logs");
        let legacy_logs = work.join("legacy-logs");
        let settings = PreparedSettings {
            lens: Default::default(),
            camera_recipe: crate::camera::CameraRecipe::CanonCr3,
            preview_edge: None,
        };
        let sources: Vec<SourceFrame> = materials
            .iter()
            .map(|path| SourceFrame {
                path: path.clone(),
                name: path.file_name().unwrap().to_str().unwrap().to_string(),
                key: Vec::new(),
            })
            .collect();

        // Prepare both frames once; this is the only Converter invocation on
        // the prepared path.
        let mut prepared_frames = Vec::new();
        for (index, source) in sources.iter().enumerate() {
            let scratch = work.join(format!("scratch-{index}"));
            fs::create_dir_all(&scratch).unwrap();
            let frame = prepare(
                &tool,
                source,
                &PrepareSettings {
                    lens: Default::default(),
                    scratch_dir: &scratch,
                    context_path: &work.join(format!("frame-{index}.ctx")),
                    log: &work.join(format!("prepare-{index}.log")),
                    converter: Some(prepared_converter.to_str().unwrap()),
                    converter_timeout: Some(120),
                },
            )
            .expect("real prepare");
            assert_eq!(
                frame.route,
                PreparedRoute::VendorRaw,
                "a CR3 must route through the Converter"
            );
            assert!(frame.generated);
            assert!(frame.dng.exists());
            prepared_frames.push(frame);
        }
        assert_eq!(counter_lines(&prepared_counter), sources.len());

        // Analysis: the first frame uses its as-shot neutral, later frames the
        // frozen first-frame reference (the product's default).
        let first = analyze_prepared(&tool, &prepared_frames[0], None, Some(0.0), None, &settings, &prepared_logs.join("frame_000001.log"))
            .expect("prepare analysis");
        let first_wb = first.applied_wb;
        let legacy_config = ProduceConfig {
            camera_recipe: crate::camera::CameraRecipe::CanonCr3,
            logs_dir: &legacy_logs,
            exposure_ev: None,
            analysis_roi: None,
            echo: false,
            echo_tag: "",
            white_balance: WhiteBalance::FirstFrame,
            converter: Some(legacy_converter.to_str().unwrap()),
            converter_timeout: Some(120),
        };
        let baseline = produce_with_evs(&tool, &sources, &[0.0, 0.0][..sources.len()], &legacy_config)
            .expect("legacy analysis");
        for (index, (prepared, legacy)) in prepared_frames.iter().zip(&baseline.frames).enumerate() {
            let wb = if index == 0 { None } else { Some(first_wb) };
            let info = analyze_prepared(&tool, prepared, wb, Some(0.0), None, &settings, &prepared_logs.join(format!("frame_{index:06}.log")))
                .expect("prepared analysis");
            assert_eq!(info.applied_wb, legacy.applied_wb, "frame {index} WB");
            assert_eq!(info.as_shot_neutral, legacy.as_shot_neutral);
            assert_eq!(info.camera_color, legacy.camera_color, "frame {index} color");
            assert_eq!(info.source_sha256, legacy.source_sha256);
            assert_eq!(
                analysis_bytes(&info),
                analysis_bytes(legacy),
                "frame {index} analysis sidecar bytes"
            );
        }

        // FP32 develop at 0 and a known EV: bitwise digest and metadata equality
        // against the old same-source single-shot path.
        for ev in [0.0f64, 0.25] {
            let mut legacy_digests = Vec::new();
            let mut legacy_infos = Vec::new();
            pixel_digest(
                &legacy_config,
                &tool,
                &sources,
                &[ev; 2][..sources.len()],
                &mut legacy_digests,
                &mut legacy_infos,
            );
            for (index, prepared) in prepared_frames.iter().enumerate() {
                let wb = if index == 0 { None } else { Some(first_wb) };
                let mut digest = String::new();
                let info = develop_prepared(&tool, prepared, wb, Some(ev), &settings, &prepared_logs.join(format!("frame_{index:06}.log")), &mut |info, native| {
                    let pixels = native.pixels(info.width, info.height)?;
                    digest = crate::digest::sha256(&pixels);
                    Ok(())
                })
                .expect("prepared develop");
                assert_eq!(digest, legacy_digests[index], "frame {index} EV {ev} FP32 hash");
                assert_eq!(info.applied_wb, legacy_infos[index].applied_wb);
                assert_eq!(info.as_shot_neutral, legacy_infos[index].as_shot_neutral);
                assert_eq!(info.camera_color, legacy_infos[index].camera_color);
                assert_eq!(info.width, legacy_infos[index].width);
                assert_eq!(info.height, legacy_infos[index].height);
            }
        }
        assert_eq!(counter_lines(&prepared_counter), sources.len());
        for prepared in &prepared_frames {
            assert!(prepared.dng.exists(), "prepared DNG must survive consumers");
        }

        // The caller's deletion is the only owner of the prepared artifact.
        let scratch = work.join("scratch-delete");
        fs::create_dir_all(&scratch).unwrap();
        let deletion = prepare(
            &tool,
            &sources[0],
            &PrepareSettings {
                lens: Default::default(),
                scratch_dir: &scratch,
                context_path: &work.join("delete.ctx"),
                log: &work.join("prepare-delete.log"),
                converter: Some(prepared_converter.to_str().unwrap()),
                converter_timeout: Some(120),
            },
        )
        .expect("prepare deletion frame");
        fs::remove_file(&deletion.dng).expect("caller deletes prepared DNG");
        let error = analyze_prepared(&tool, &deletion, None, None, None, &settings, &prepared_logs.join("frame_000001.log"))
            .expect_err("deleted cache must fail");
        assert!(error.message.contains("prepared"), "{}", error.message);
        assert_eq!(counter_lines(&prepared_counter), sources.len() + 1);
    }
