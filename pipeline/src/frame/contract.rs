//! Native frame contract validation: sidecar JSON contracts, metadata fields
//! and prepared-context cross-checks consumed by the orchestration layer.
use super::doc::PreparedFrame;
use super::sidecar::{
    canonical_source_pair, frame_metadata_envelope, missing_field, parse_wb, wb_close,
};
use super::{failure, JSON_MEDIA_TYPE, WB_SOURCE_AS_SHOT, WB_SOURCE_OVERRIDE};
use crate::error::{Error, Result, EXIT_FRAME};
use crate::json::Json;
use crate::transport::MemoryFile;
use std::fs;
use std::path::Path;
use std::sync::Arc;

// Matched native/Rust contract: no silent fallback to sensor-relative exposure.
fn valid_fixed_calibration(color: &Json) -> bool {
    let Some(c) = color.get("exposure_calibration") else { return false };
    let number = |k| c.get(k).and_then(Json::as_f64).filter(|v| v.is_finite());
    let (Some(max), Some(black), Some(white), Some(dw), Some(db), Some(gain), Some(scale), Some(normal)) =
        (number("adc_maximum"), number("black"), number("normal_white"), number("dng_white"),
         number("dng_black"), number("native_scene_gain"), number("storage_scale"), number("normal_white_gain"))
        else { return false };
    let close = |a: f64,b: f64| (a-b).abs() <= 1e-9 * b.abs().max(1.0);
    c.get("applied") == Some(&Json::Bool(true))
        && c.get("method").and_then(Json::as_str) == Some("canon-normal-white-fixed-v1")
        && c.get("auto_tone_normalization") == Some(&Json::Bool(false))
        && c.get("baseline_exposure_applied") == Some(&Json::Bool(false))
        && number("fixed_scene_gain") == Some(1.8)
        && max >= 255.0 && max <= 65535.0 && (max + 1.0).fract() == 0.0
        && ((max as u64)+1).is_power_of_two()
        && black >= 0.0 && black < white && white <= max && white > max/2.0
        && dw == 65535.0 && db >= 0.0 && (db-black*scale).abs() <= scale+1e-6
        && close(scale, (dw+1.0)/(max+1.0))
        && close(normal, (dw-db)/(scale*(white-black))) && close(gain, 1.8*normal)
}


pub(crate) fn validate_sidecar_contracts(
    sidecar: &Json,
    method: &crate::color_profile::CameraRecipe,
    pixels: bool,
    lens_mode: crate::lens::Mode,
    lens_policy: crate::lens::Policy,
) -> Result<()> {
    if let Some(report) = sidecar.get("lens_correction") {
        crate::lens::validate_report(report, lens_mode, lens_policy)?;
    } else if lens_mode == crate::lens::Mode::Auto {
        return Err(failure("missing lens correction contract"));
    }
    if pixels && *method != crate::color_profile::CameraRecipe::LegacyInverseCm {
        let recovery = sidecar.get("highlight_recovery")
            .ok_or_else(|| failure("missing highlight recovery contract"))?;
        if recovery.get("revision").and_then(Json::as_u64) != Some(1)
            || recovery.get("requested").and_then(Json::as_bool) != Some(true)
            || !matches!(recovery.get("status").and_then(Json::as_str), Some("processed" | "unchanged" | "unsupported"))
        {
            return Err(failure("invalid highlight recovery contract"));
        }
    }
    {
        let color = sidecar
            .get("color")
            .ok_or_else(|| failure("missing camera color contract"))?;
        let effective = color.get("method_effective").and_then(Json::as_str);
        let valid_matrix = |key| {
            color.get(key).and_then(Json::as_array).is_some_and(|rows| {
                rows.len() == 3
                    && rows.iter().all(|r| {
                        r.as_array().is_some_and(|r| {
                            r.len() == 3 && r.iter().all(|x| x.as_f64().is_some_and(f64::is_finite))
                        })
                    })
            })
        };
        if !valid_matrix("camera_to_xyz_d50")
            || !valid_matrix("camera_to_acescg")
            || color
                .get("profile_fingerprint")
                .and_then(Json::as_str)
                .is_none_or(str::is_empty)
        {
            return Err(failure("missing/invalid camera matrix/profile fingerprint"));
        }

        if method.requires_canon_cr3()
            && (color.get("algorithm_revision").and_then(Json::as_u64) != Some(4)
                || !valid_fixed_calibration(color)
                || !matches!(
                    (
                        color.get("hue_sat_map_present"),
                        color.get("hue_sat_map_applied")
                    ),
                    (Some(Json::Bool(true)), Some(Json::Bool(true)))
                        | (Some(Json::Bool(false)), Some(Json::Bool(false)))
                ))
        {
            return Err(failure(
                "incomplete camera profile calibration; rebuild matched native/Rust tools",
            ));
        }
        let eq = |key, value: &str| color.get(key).and_then(Json::as_str) == Some(value);
        let flag = |key| color.get(key).and_then(Json::as_bool);
        let valid_presence = |present, applied, status, enabled| {
            match (flag(present), flag(applied)) {
                (Some(p), Some(a)) => a == (p && enabled) && eq(status, if !p { "absent" } else if a { "applied" } else { "skipped" }),
                _ => false,
            }
        };
        let calibration = color.get("exposure_calibration");
        if !eq("recipe_requested", method.token()) || !eq("recipe_effective", method.token())
            || !eq("profile_correction_requested", method.correction()) || !eq("profile_correction_effective", method.correction())
            || !eq("camera_calibration_requested", method.calibration()) || !eq("camera_calibration_effective", method.calibration())
            || color.get("algorithm_revision").and_then(Json::as_u64) != Some(4)
            || !valid_matrix("xyz_d50_to_acescg") || !valid_matrix("sdk_normalized_pcs_to_camera")
            || !eq("cat", "linear Bradford D50->D60; AP1 inverse primaries matrix")
            || !valid_presence("hue_sat_map_present", "hue_sat_map_applied", "hue_sat_map_status", method.correction() == "hue-sat-map")
            || !valid_presence("look_table_present", "look_table_applied", "look_table_status", false)
            || !valid_presence("tone_curve_present", "tone_curve_applied", "tone_curve_status", false)
            || flag("baseline_exposure_applied") != Some(false) || !eq("baseline_exposure_status", "skipped")
            || !color.get("baseline_exposure_ev").and_then(Json::as_f64).is_some_and(f64::is_finite)
            || flag("forward_matrix_present").is_none()
            || flag("forward_matrix_used") != Some(effective == Some("dng-camera-to-pcs-forward-matrix"))
            || (effective == Some("dng-camera-to-pcs-forward-matrix") && flag("forward_matrix_present") != Some(true))
            || (effective == Some("dng-camera-to-pcs-inverse-fallback") && flag("forward_matrix_present") != Some(false))
            || color.get("camera_model").and_then(Json::as_str).is_none_or(str::is_empty)
            || (method.calibration() == "none" && (calibration.and_then(|c| c.get("applied")) != Some(&Json::Bool(false))
                || calibration.and_then(|c| c.get("method")).and_then(Json::as_str) != Some("none")
                || calibration.and_then(|c| c.get("status")).and_then(Json::as_str) != Some("uncalibrated")))
        { return Err(failure("inconsistent camera recipe/profile policy contract")); }
        if sidecar.get("color_contract_version").and_then(Json::as_u64) != Some(2)
            || color.get("method_requested").and_then(Json::as_str)
                != Some(method.native_token())
            || !(if *method != crate::color_profile::CameraRecipe::LegacyInverseCm {
                matches!(
                    effective,
                    Some("dng-camera-to-pcs-forward-matrix" | "dng-camera-to-pcs-inverse-fallback")
                )
            } else {
                effective == Some("legacy-inverse-cm")
            })
            || (method.requires_canon_cr3()
                && !color
                    .get("camera_model")
                    .and_then(Json::as_str)
                    .is_some_and(|s| s.starts_with("Canon ")))
        {
            return Err(failure("invalid camera color contract"));
        }
    }

    Ok(())
}

/// Metadata fields every frame must report, extracted and validated in one pass.
pub(crate) struct FrameMetadata {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) source_sha256: Option<String>,
    pub(crate) applied_wb: [f64; 3],
    pub(crate) as_shot_neutral: [f64; 3],
    pub(crate) wb_source: String,
    pub(crate) reported_ev: f64,
    pub(crate) capture_time: Option<String>,
}

pub(crate) fn frame_metadata(
    bytes: Arc<[u8]>,
    sidecar: &Json,
    index: usize,
    total: usize,
    name: &str,
    path: &Path,
    override_wb: Option<[f64; 3]>,
    exposure_ev: Option<f64>,
) -> Result<FrameMetadata> {
    let file =
        MemoryFile::new(bytes.into(), JSON_MEDIA_TYPE).map_err(|e| failure(e.to_string()))?;
    let frame_width = sidecar
        .get("width")
        .and_then(Json::as_u32)
        .ok_or_else(|| missing_field(index, total, name, "width"))?;
    let frame_height = sidecar
        .get("height")
        .and_then(Json::as_u32)
        .ok_or_else(|| missing_field(index, total, name, "height"))?;
    if frame_width == 0 || frame_height == 0 {
        return Err(Error::new(
            EXIT_FRAME,
            format!(
                "frame {index}/{total} ({}) reports an empty image {}x{}",
                name, frame_width, frame_height
            ),
        ));
    }

    let schema_version = sidecar
        .get("schema_version")
        .and_then(Json::as_u64)
        .ok_or_else(|| missing_field(index, total, name, "schema_version"))?;
    if schema_version != 1 {
        return Err(Error::new(
            EXIT_FRAME,
            format!(
                "frame {index}/{total} ({}) has unsupported metadata schema_version {schema_version}",
                name
            ),
        ));
    }

    if sidecar
        .get("source_sha256")
        .and_then(Json::as_str)
        .is_some_and(|s| !s.is_empty())
        && !sidecar
            .get("source_sha256")
            .and_then(Json::as_str)
            .is_some_and(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err(failure("invalid source_sha256 for FP32 frame"));
    }
    let source_sha256 = Some(
        sidecar
            .get("source_sha256")
            .and_then(Json::as_str)
            .filter(|hash| !hash.is_empty())
            .ok_or_else(|| missing_field(index, total, name, "source_sha256"))?
            .to_string(),
    );
    let _metadata_envelope = frame_metadata_envelope(
        file,
        schema_version,
        frame_width,
        frame_height,
        source_sha256.as_deref(),
        index,
        total,
        name,
    )?;
    let applied_wb = parse_wb(
        &sidecar,
        "applied_white_balance",
        index,
        total,
        name,
    )?;
    let as_shot_neutral = parse_wb(&sidecar, "as_shot_neutral", index, total, name)?;
    let wb_source = sidecar
        .get("white_balance_source")
        .and_then(Json::as_str)
        .ok_or_else(|| missing_field(index, total, name, "white_balance_source"))?
        .to_string();

    if override_wb.is_none() {
        if wb_source != WB_SOURCE_AS_SHOT {
            return Err(Error::new(
                EXIT_FRAME,
                format!(
                    "frame {index}/{total} ({}) should use its as-shot white balance, metadata reports {wb_source:?}",
                    name
                ),
            ));
        }
    } else if wb_source != WB_SOURCE_OVERRIDE {
        return Err(Error::new(
            EXIT_FRAME,
            format!(
                "frame {index}/{total} ({}) should use the frozen override white balance, metadata reports {wb_source:?}",
                name
            ),
        ));
    }

    // A producer that reports `as-shot` must have applied exactly that frame's
    // as_shot_neutral. This covers the first frame of `first-frame` mode, every
    // frame of `per-frame` mode and every probe: a lying sidecar can never
    // smuggle a different neutral into the sequence or into the frozen
    // reference.
    if wb_source == WB_SOURCE_AS_SHOT && !wb_close(applied_wb, as_shot_neutral) {
        return Err(Error::new(
            EXIT_FRAME,
            format!(
                "frame {index}/{total} ({}) reports as-shot white balance but applied {:?} differs from its own as_shot_neutral {:?}",
                name, applied_wb, as_shot_neutral
            ),
        ));
    }

    let source_field = sidecar
        .get("source")
        .and_then(Json::as_str)
        .ok_or_else(|| missing_field(index, total, name, "source"))?;
    // The native producer resolves the input path (symlinks included), so
    // compare canonical identities rather than the literal argument text.
    let (expected_source, reported_source) =
        canonical_source_pair(source_field, path).map_err(|err| {
            Error::new(
                EXIT_FRAME,
                format!(
                    "frame {index}/{total} ({}) metadata source {source_field:?} cannot be resolved against the requested input: {err}",
                    name
                ),
            )
        })?;
    if reported_source != expected_source {
        return Err(Error::new(
            EXIT_FRAME,
            format!(
                "frame {index}/{total} ({}) metadata source {source_field:?} does not resolve to the requested input {}",
                name,
                expected_source.display()
            ),
        ));
    }


    let reported_ev = sidecar
        .get("exposure_ev")
        .and_then(Json::as_f64)
        .ok_or_else(|| missing_field(index, total, name, "exposure_ev"))?;
    if !reported_ev.is_finite() {
        return Err(Error::new(
            EXIT_FRAME,
            format!(
                "frame {index}/{total} ({}) reports a non-finite exposure_ev",
                name
            ),
        ));
    }
    let requested_ev = exposure_ev.unwrap_or(0.0);
    if reported_ev != requested_ev {
        return Err(Error::new(
            EXIT_FRAME,
            format!(
                "frame {index}/{total} ({}) reports exposure_ev {reported_ev} but {requested_ev} was requested",
                name
            ),
        ));
    }

    let capture_time = match sidecar.get("capture_time") {
        None | Some(Json::Null) => None,
        Some(Json::Str(text)) if !text.is_empty() => Some(text.clone()),
        Some(_) => {
            return Err(Error::new(
                EXIT_FRAME,
                format!(
                    "frame {index}/{total} ({}) capture_time must be a non-empty string or null",
                    name
                ),
            ))
        }
    };

    if sidecar.get("analysis_sidecar") != Some(&Json::Null) {
        return Err(failure("stdout metadata analysis_sidecar must be null"));
    }

    Ok(FrameMetadata {
        width: frame_width,
        height: frame_height,
        source_sha256,
        applied_wb,
        as_shot_neutral,
        wb_source,
        reported_ev,
        capture_time,
    })
}

pub(crate) fn validate_prepared_metadata(
    sidecar: &Json,
    prepared: &PreparedFrame,
    source_sha256: Option<&str>,
    as_shot_neutral: [f64; 3],
    capture_time: Option<&str>,
) -> Result<()> {
    let block = sidecar
        .get("prepared")
        .ok_or_else(|| failure("prepared metadata block is missing"))?;

    if block.get("schema_version").and_then(Json::as_u64) != Some(1)
            || block.get("consumed_context") != Some(&Json::Bool(true))
        {
            return Err(failure("prepared metadata block is inconsistent"));
        }
        if block.get("generated") != Some(&Json::Bool(prepared.generated)) {
            return Err(failure(
                "prepared metadata generated flag does not match the context",
            ));
        }
        let dng = block
            .get("dng")
            .and_then(Json::as_str)
            .ok_or_else(|| failure("prepared metadata block has no DNG path"))?;
        let dng_path = fs::canonicalize(dng).map_err(|e| {
            failure(format!(
                "prepared metadata DNG {dng:?} cannot be resolved: {e}"
            ))
        })?;
        if dng_path != prepared.dng
            || block.get("sha256").and_then(Json::as_str)
                != Some(prepared.dng_sha256.as_str())
        {
            return Err(failure(
                "prepared metadata DNG identity does not match the context",
            ));
        }
        if sidecar.get("route").and_then(Json::as_str) != Some(prepared.route.token()) {
            return Err(failure(
                "prepared metadata route does not match the context",
            ));
        }
        if source_sha256 != Some(prepared.source_sha256.as_str()) {
            return Err(failure(
                "prepared metadata source_sha256 does not match the context",
            ));
        }
        if !wb_close(as_shot_neutral, prepared.as_shot_neutral) {
            return Err(failure(
                "prepared metadata as_shot_neutral does not match the context",
            ));
        }
        if capture_time != prepared.capture_time.as_deref() {
            return Err(failure(
                "prepared metadata capture_time does not match the context",
            ));
        }
    Ok(())
    }
