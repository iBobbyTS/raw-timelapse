mod common;

use common::{
    fake_native_tool, ffprobe, make_input, require_ffmpeg, run_cli, standard_frames, test_dir,
    FFMPEG,
};
use rawtl_linear_video::color_profile::{PreviewRange, YcbcrCoding};
use rawtl_linear_video::export::{
    hardware_profiles, ExportProfile, InputFormat, SOFTWARE_PROFILES,
};
use rawtl_linear_video::json;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[test]
fn rawvideo_layouts_follow_ffmpeg_plane_order_depth_range_and_chroma_footprint() {
    let frame = [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.], [1., 1., 1.]];
    let variants = [
        ("yuv420p", 8, 1),
        ("yuv420p10le", 10, 1),
        ("yuv420p12le", 12, 1),
        ("yuv422p", 8, 2),
        ("yuv422p10le", 10, 2),
        ("yuv422p12le", 12, 2),
        ("yuv444p", 8, 4),
        ("yuv444p10le", 10, 4),
        ("yuv444p12le", 12, 4),
        ("gbrp", 8, 4),
        ("gbrp10le", 10, 4),
        ("gbrp12le", 12, 4),
        ("gbrp16le", 16, 4),
    ];
    assert_eq!(
        variants.map(|(name, _, _)| name),
        InputFormat::ALL.map(InputFormat::name)
    );
    for (name, bits, chroma) in variants {
        let format = InputFormat::parse(name).unwrap();
        let bytes = format
            .pack(&frame, 2, 2, PreviewRange::Tv, YcbcrCoding::Bt709)
            .unwrap();
        let codes: Vec<u16> = if bits == 8 {
            bytes.iter().copied().map(u16::from).collect()
        } else {
            assert_eq!(bytes.len() % 2, 0);
            bytes
                .chunks_exact(2)
                .map(|sample| u16::from_le_bytes(sample.try_into().unwrap()))
                .collect()
        };
        assert_eq!(codes.len(), 4 + 2 * chroma, "{name}");
        if format.is_rgb() {
            let peak = ((1u32 << bits) - 1) as u16;
            assert_eq!(
                codes,
                [0, peak, 0, peak, 0, 0, peak, peak, peak, 0, 0, peak],
                "{name}"
            );
        } else {
            // The luma codes for the primaries follow the BT.709 coefficients.
            let shift = 1 << (bits - 8);
            let luma = [0.2126, 0.7152, 0.0722, 1.0]
                .map(|y| ((16.0_f64 + 219.0 * y) * f64::from(shift)).round() as u16);
            assert_eq!(&codes[..4], &luma, "{name}");
            let neutral = 128 * shift as u16;
            match chroma {
                1 => assert_eq!(&codes[4..], &[neutral, neutral], "{name}"),
                2 => {
                    assert!(codes[4] < neutral && codes[5] > neutral, "{name}");
                    assert!(codes[6] > neutral && codes[7] < neutral, "{name}");
                }
                4 => {
                    assert!(codes[4] < neutral && codes[6] > neutral, "{name}");
                    assert!(codes[8] > neutral && codes[10] < neutral, "{name}");
                    assert_eq!([codes[7], codes[11]], [neutral, neutral], "{name}");
                }
                _ => unreachable!(),
            }
        }
    }
    let depth = InputFormat::Gbrp16
        .pack(
            &[[0.5, 0.50003, 0.50006]],
            1,
            1,
            PreviewRange::Pc,
            YcbcrCoding::Bt709,
        )
        .unwrap();
    assert_eq!(depth, [1, 128, 3, 128, 0, 128]);
    assert!(InputFormat::Yuv420p.validate_geometry(3, 2).is_err());
    assert!(InputFormat::Yuv422p.validate_geometry(2, 3).is_ok());
}

#[test]
fn x265_profiles_emit_requested_quality_and_pool_size() {
    let cores = std::thread::available_parallelism().unwrap().get();
    let pools = format!("pools={}", (cores as f64 * 0.7).round() as usize);
    for (name, crf, preset) in [
        ("x265-12bit-444", "4", Some("slow")),
        ("x265-10bit-422", "8", Some("slow")),
        ("x265-8bit-420", "20", None),
    ] {
        let profile = ExportProfile::software(name).unwrap();
        assert_eq!(profile.encoder(), "libx265");
        let args = profile.ffmpeg_args();
        let option = |key| {
            args.windows(2)
                .find(|pair| pair[0] == key)
                .map(|pair| pair[1].as_str())
        };
        assert_eq!(option("-x265-params"), Some(pools.as_str()));
        assert_eq!(option("-crf"), Some(crf));
        assert_eq!(option("-preset"), preset);
    }
}

#[test]
fn hardware_catalog_probes_every_candidate_and_tries_eight_bit_when_main10_fails() {
    let mut calls = Vec::new();
    let list = concat!(
        " V....D h264_videotoolbox Encoder (codec h264)\n",
        " V....D hevc_videotoolbox Encoder (codec hevc)\n",
        " V....D hevc_nvenc Encoder (codec hevc)\n",
        " V....D h264_vulkan Encoder (codec h264)\n",
        " V....D libx264 Libx264 (codec h264)\n",
    );
    let profiles = hardware_profiles(|args| {
        if args.iter().any(|arg| arg == "-encoders") {
            return Ok((true, list.into()));
        }
        if let Some(encoder) = args[2].strip_prefix("encoder=") {
            if encoder == "libx264" {
                return Ok((
                    true,
                    "General capabilities: delay\nSupported pixel formats: yuv420p".into(),
                ));
            }
            let formats = if encoder.ends_with("vulkan") {
                "vulkan"
            } else {
                "yuv420p nv12 p010le"
            };
            return Ok((
                true,
                format!("General capabilities: delay hardware\nSupported pixel formats: {formats}"),
            ));
        }
        let encoder = &args[args.iter().position(|arg| arg == "-c:v").unwrap() + 1];
        let depth = args.iter().any(|arg| arg.ends_with("format=yuv420p10le"));
        calls.push((encoder.clone(), depth));
        let works = match encoder.as_str() {
            "hevc_videotoolbox" => !depth,
            "hevc_nvenc" => depth,
            "h264_vulkan" => false,
            _ => true,
        };
        Ok((works, "".into()))
    })
    .unwrap();
    let choices: Vec<_> = profiles
        .iter()
        .map(|p| (p.name.as_str(), p.input))
        .collect();
    assert_eq!(
        choices,
        [
            ("h264_videotoolbox", InputFormat::Yuv420p),
            ("hevc_nvenc", InputFormat::Yuv420p10),
            ("hevc_videotoolbox", InputFormat::Yuv420p),
        ]
    );
    assert!(calls.contains(&("hevc_videotoolbox".into(), true)));
    assert!(calls.contains(&("h264_vulkan".into(), false)));
    assert!(!calls.iter().any(|(encoder, _)| encoder == "libx264"));
}

#[test]
fn live_hardware_catalog_only_lists_working_ffmpeg_encoders() {
    require_ffmpeg();
    let profiles = hardware_profiles(|args| {
        let output = Command::new(FFMPEG)
            .args(args)
            .output()
            .map_err(|e| e.to_string())?;
        Ok((
            output.status.success(),
            format!(
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ),
        ))
    })
    .unwrap();
    eprintln!(
        "FFmpeg hardware export profiles: {:?}",
        profiles
            .iter()
            .map(|profile| (&profile.name, profile.input.name()))
            .collect::<Vec<_>>()
    );
    for profile in profiles {
        let root = test_dir(&format!("export-{}", profile.name));
        let patch = format!(r#"{{"export_profile":"{}"}}"#, profile.name);
        let (output, path) = encode(&root, &patch, "mp4", "640x480");
        assert!(
            output.status.success(),
            "{}: {}",
            profile.name,
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(ffprobe(&path).contains("nb_read_frames=3"));
    }
}

fn encode(root: &Path, patch: &str, extension: &str, resize: &str) -> (Output, PathBuf) {
    let input = root.join("input");
    make_input(&input, &["IMG_1.DNG", "IMG_2.DNG", "IMG_10.DNG"]);
    let fixtures = root.join("fixtures");
    fs::create_dir_all(&fixtures).unwrap();
    let tool = fake_native_tool(&fixtures, &standard_frames(&fixtures), None, false);
    let graph = root.join("internal.json");
    let source = include_str!("../src/graph/default.json");
    let patched = source.replace(
        r#""kind":"encode","#,
        &format!(r#""kind":"encode","encoder":{patch},"#),
    );
    assert_ne!(source, patched);
    json::parse(&patched).unwrap();
    fs::write(&graph, patched).unwrap();
    let video = root.join(format!("export.{extension}"));
    let container = match extension {
        "mkv" => "matroska",
        "mp4" => "mp4",
        _ => "mov",
    };
    let result = run_cli(&[
        "--input",
        input.to_str().unwrap(),
        "--output",
        video.to_str().unwrap(),
        "--graph",
        graph.to_str().unwrap(),
        "--container",
        container,
        "--resize",
        resize,
        "--strength",
        "0",
        "--dng-linear-frame",
        tool.to_str().unwrap(),
        "--verify-output",
    ]);
    (result, video)
}

#[test]
fn real_ffmpeg_delivers_every_software_export_profile_through_the_graph() {
    require_ffmpeg();
    for (name, codec, pixel_format) in [
        ("x265-12bit-444", "hevc", "yuv444p12le"),
        ("x265-10bit-422", "hevc", "yuv422p10le"),
        ("x265-8bit-420", "hevc", "yuv420p"),
        ("dnxhr-sq", "dnxhd", "yuv422p"),
        ("dnxhr-444", "dnxhd", "yuv444p10le"),
        ("prores-422", "prores", "yuv422p10le"),
        ("prores-4444", "prores", "yuv444p12le"),
    ] {
        assert!(SOFTWARE_PROFILES.contains(&name));
        let root = test_dir(name);
        let (result, path) = encode(
            &root,
            &format!(r#"{{"export_profile":"{name}"}}"#),
            "mov",
            "640x480",
        );
        assert!(
            result.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let probe = ffprobe(&path);
        assert!(
            probe.contains(&format!("codec_name={codec}")),
            "{name}: {probe}"
        );
        assert!(
            probe.contains(&format!("pix_fmt={pixel_format}")),
            "{name}: {probe}"
        );
        assert!(probe.contains("nb_read_frames=3"), "{name}: {probe}");
        let log = fs::read_to_string(path.with_extension("log")).unwrap();
        let profile = ExportProfile::software(name).unwrap();
        assert!(
            log.contains(&format!("input_format={}", profile.input.name())),
            "{name}: {log}"
        );
    }
}

#[test]
fn manual_export_accepts_rgb16_and_requires_an_explicit_pipe_format() {
    require_ffmpeg();
    let root = test_dir("export-rgb16");
    let (result, path) = encode(
        &root,
        r#"{"input_format":"gbrp16le","ffmpeg_args":["-c:v","ffv1","-pix_fmt","gbrp16le","-level","3"]}"#,
        "mkv",
        "8x6",
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let probe = ffprobe(&path);
    assert!(
        probe.contains("codec_name=ffv1") && probe.contains("pix_fmt=gbrp16le"),
        "{probe}"
    );

    let root = test_dir("export-rgb-to-yuv");
    let (result, path) = encode(
        &root,
        r#"{"input_format":"gbrp12le","ffmpeg_args":["-c:v","libx264","-pix_fmt","yuv420p","-crf","18"]}"#,
        "mp4",
        "8x6",
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(ffprobe(&path).contains("pix_fmt=yuv420p"));

    let root = test_dir("export-missing-format");
    let (result, path) = encode(&root, r#"{"ffmpeg_args":["-c:v","ffv1"]}"#, "mkv", "8x6");
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr)
        .contains("manual export requires both input_format and ffmpeg_args"));
    assert!(!path.exists() && !path.with_extension("log").exists());
}
