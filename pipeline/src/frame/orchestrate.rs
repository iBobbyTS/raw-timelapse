//! One-frame and batch orchestration: prepare, analyze, develop, and the
//! per-frame native run with full metadata contract checks.
use super::contract::{frame_metadata, validate_prepared_metadata, validate_sidecar_contracts};
use super::doc::{
    field_value, hex64, parse_document_fields, parse_u32, PreparedFrame, PreparedRoute,
    read_document, require_fields,
};
use super::sidecar::{analysis_sidecar_envelope, format_axis, format_ev, format_wb, parse_json};
#[cfg(test)]
use super::sidecar::wb_close;
use super::stream::{NativeRun, NativeStream};
#[cfg(test)]
use super::ProduceConfig;
#[cfg(test)]
use super::FrameSequence;
use super::{failure, FrameInfo, percent_encode};
use crate::error::{Error, Result, EXIT_FRAME};
use crate::json::Json;
use crate::order::SourceFrame;
use crate::proc::{self, ActiveNative};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Either an original source file or a validated prepared context. Both reach
/// the same native consumer protocol; only the second may not carry Converter
/// options because the conversion already happened during --prepare. The
/// source variant exists only for the retired-producer tests.
#[derive(Clone, Copy)]
enum FrameInput<'a> {
    #[cfg(test)]
    Source(&'a SourceFrame),
    Prepared(&'a PreparedFrame),
}

impl FrameInput<'_> {
    fn name(&self) -> &str {
        match self {
            #[cfg(test)]
            Self::Source(source) => &source.name,
            Self::Prepared(prepared) => &prepared.source_name,
        }
    }
    fn path(&self) -> &Path {
        match self {
            #[cfg(test)]
            Self::Source(source) => &source.path,
            Self::Prepared(prepared) => &prepared.source,
        }
    }
    fn prepared(&self) -> Option<&PreparedFrame> {
        match self {
            #[cfg(test)]
            Self::Source(_) => None,
            Self::Prepared(prepared) => Some(prepared),
        }
    }
}

#[cfg(test)]
pub fn produce_with_evs(
    tool: &Path,
    sources: &[SourceFrame],
    evs: &[f64],
    config: &ProduceConfig<'_>,
) -> Result<FrameSequence> {
    produce_inner(
        tool,
        sources,
        Some(evs),
        config,
        false,
        Default::default(),
        &mut |_, _| Ok(()),
    )
}
#[cfg(test)]
pub fn stream_frames(
    tool: &Path,
    sources: &[SourceFrame],
    evs: &[f64],
    config: &ProduceConfig<'_>,
    active: ActiveNative,
    consume: &mut impl FnMut(&FrameInfo, &mut NativeStream) -> Result<()>,
) -> Result<FrameSequence> {
    produce_inner(tool, sources, Some(evs), config, true, active, consume)
}
#[cfg(test)]
fn produce_inner(
    tool: &Path,
    sources: &[SourceFrame],
    evs: Option<&[f64]>,
    config: &ProduceConfig<'_>,
    pixels: bool,
    active: ActiveNative,
    consume: &mut impl FnMut(&FrameInfo, &mut NativeStream) -> Result<()>,
) -> Result<FrameSequence> {
    if let Some(evs) = evs {
        if evs.len() != sources.len() || evs.iter().any(|v| !v.is_finite()) {
            return Err(failure("invalid per-frame EV vector"));
        }
    }
    let started = Instant::now();
    let run = NativeRun::from_produce(config);
    let mut frames: Vec<FrameInfo> = Vec::with_capacity(sources.len());
    for (offset, source) in sources.iter().enumerate() {
        let reference = frames
            .first()
            .map(|f| f.applied_wb)
            .filter(|_| config.white_balance.freezes_reference());
        let ev = evs.map(|e| e[offset]).or(config.exposure_ev);
        let info = run_frame(
            tool,
            FrameInput::Source(source),
            offset + 1,
            sources.len(),
            reference,
            ev,
            &run,
            pixels,
            active.clone(),
            &mut |info, stream| {
                if let Some(first) = frames.first() {
                    if config.camera_recipe.requires_canon_cr3()
                        && info
                            .camera_color
                            .as_ref()
                            .and_then(|c| c.get("camera_model"))
                            != first
                                .camera_color
                                .as_ref()
                                .and_then(|c| c.get("camera_model"))
                    {
                        return Err(failure("rev-eng Canon Log rejects mixed camera models"));
                    }
                    if info.width != first.width || info.height != first.height {
                        return Err(failure(format!(
                            "frame {} ({}) has dimensions {}x{} but the sequence is {}x{}",
                            info.index,
                            info.source_name,
                            info.width,
                            info.height,
                            first.width,
                            first.height
                        )));
                    }
                    if config.white_balance.freezes_reference()
                        && !wb_close(info.applied_wb, first.applied_wb)
                    {
                        return Err(failure(
                            "applied white balance differs from the frozen reference",
                        ));
                    }
                }
                consume(info, stream)
            },
        )
        .map_err(|e| {
            let log = config.logs_dir.join(format!("frame_{:06}.log", offset + 1));
            let detail = fs::read_to_string(&log).unwrap_or_default();
            Error::new(
                e.code,
                format!(
                    "frame {}/{} ({}) failed: {}; transcript {}: {}; no video was produced",
                    offset + 1,
                    sources.len(),
                    source.name,
                    e.message,
                    log.display(),
                    proc::tail(&detail, 1200)
                ),
            )
        })?;
        if config.echo {
            println!(
                "{}{}/{} {} (WB {}, EV {:+.3})",
                config.echo_tag,
                info.index,
                sources.len(),
                info.source_name,
                info.wb_source,
                info.exposure_ev
            );
        }
        frames.push(info);
    }
    let first = frames
        .first()
        .ok_or_else(|| failure("empty frame sequence"))?;
    Ok(FrameSequence {
        width: first.width,
        height: first.height,
        reference_wb: first.applied_wb,
        reference_frame: first.source_name.clone(),
        frames,
        total_duration: started.elapsed(),
    })
}
/// Settings for `dng-linear-frame --prepare`. The caller owns the scratch
/// directory (it must already exist), the context path and its transcript.
pub struct PrepareSettings<'a> {
    pub scratch_dir: &'a Path,
    pub context_path: &'a Path,
    pub log: &'a Path,
    pub converter: Option<&'a str>,
    pub converter_timeout: Option<u64>,
    pub lens: crate::lens::Settings,
}

/// Prepared-frame consumer settings. No Converter options exist here on
/// purpose: a prepared context already records its conversion provenance.
pub struct PreparedSettings {
    pub camera_recipe: crate::color_profile::CameraRecipe,
    pub lens: crate::lens::Settings,
}

/// Run the native prepare operation once for one source and load the resulting
/// context. The returned frame carries the original identity; the prepared DNG
/// stays in the caller's scratch until the caller deletes it.
#[cfg(test)]
pub fn prepare(
    tool: &Path,
    source: &SourceFrame,
    settings: &PrepareSettings<'_>,
) -> Result<PreparedFrame> {
    prepare_active(tool, source, settings, Default::default())
}
pub fn prepare_active(
    tool: &Path,
    source: &SourceFrame,
    settings: &PrepareSettings<'_>,
    active: ActiveNative,
) -> Result<PreparedFrame> {
    let mut args = vec![
        "--prepare".to_string(),
        source.path.display().to_string(),
        "--scratch".into(),
        settings.scratch_dir.display().to_string(),
        "--emit-prepared".into(),
        settings.context_path.display().to_string(),
    ];
    if let Some(path) = settings.converter {
        args.extend(["--converter".into(), path.into()]);
    }
    if let Some(seconds) = settings.converter_timeout {
        args.extend(["--converter-timeout".into(), seconds.to_string()]);
    }
    settings.lens.prepare_args(&mut args);
    let captured = proc::run_capture_active(tool, &args, EXIT_FRAME, settings.log, active.clone())?;
    write_prepare_log(settings.log, tool, &args, &captured)?;
    proc::check_stop()?;
    if !captured.status.success() {
        return Err(Error::new(
            EXIT_FRAME,
            format!(
                "prepared-frame conversion failed for {}: {}; transcript {}: {}",
                source.name,
                proc::exit_description(&captured.status),
                settings.log.display(),
                proc::tail(&captured.stderr, 1200)
            ),
        ));
    }
    let mut prepared = PreparedFrame::load(settings.context_path)?;
    let expected = fs::canonicalize(&source.path).map_err(|e| {
        failure(format!(
            "requested source {} cannot be resolved: {e}",
            source.path.display()
        ))
    })?;
    if prepared.source != expected {
        return Err(failure(format!(
            "prepared context {} describes {} instead of the requested source {}",
            settings.context_path.display(),
            prepared.source.display(),
            expected.display()
        )));
    }
    query_lens_area(tool, &mut prepared, &settings.lens, active)?;
    Ok(prepared)
}

/// One explicit source identity in an ordered batch (1..64 entries).
pub struct PrepareBatchItem<'a> {
    pub frame_id: usize,
    pub source: &'a SourceFrame,
}
/// A private, empty batch scratch directory. This adapter owns prepare.request,
/// prepare.result, batch-native.log and frame_{id}.context in that directory.
/// The transcript is the batch's unique ActiveNative registration key.
pub struct PrepareBatchSettings<'a> {
    pub scratch_dir: &'a Path,
    pub converter: Option<&'a str>,
    pub converter_timeout: Option<u64>,
    pub lens: crate::lens::Settings,
}
fn batch_check_stop(active: &ActiveNative) -> Result<()> {
    proc::check_stop()?;
    if active.lock().unwrap().cancelled {
        return Err(failure("run cancelled before batch publication"));
    }
    Ok(())
}
/// Returns the complete validated batch in request order, or no frames. No
/// caller may load individual contexts as a substitute for this success path.
pub fn prepare_batch_active(
    tool: &Path,
    items: &[PrepareBatchItem<'_>],
    settings: &PrepareBatchSettings<'_>,
    active: ActiveNative,
) -> Result<Vec<(usize, PreparedFrame)>> {
    batch_check_stop(&active)?;
    if items.is_empty() || items.len() > 64 {
        return Err(failure("batch count must be 1..64"));
    }
    let scratch = fs::canonicalize(settings.scratch_dir)
        .map_err(|e| failure(format!("batch scratch: {e}")))?;
    if fs::read_dir(&scratch)
        .map_err(|e| failure(e.to_string()))?
        .next()
        .is_some()
    {
        return Err(failure(
            "batch adapter requires an empty private scratch directory",
        ));
    }
    let request = scratch.join("prepare.request");
    let result = scratch.join("prepare.result");
    let log = scratch.join("batch-native.log");
    let mut expected = Vec::new();
    let mut ids = std::collections::BTreeSet::new();
    let mut text = format!("rawtl-prepare-request/1\ncount={}\n", items.len());
    for (i, item) in items.iter().enumerate() {
        if item.frame_id > u32::MAX as usize || !ids.insert(item.frame_id) {
            return Err(failure("batch duplicate or out-of-range frame ID"));
        }
        let source = fs::canonicalize(&item.source.path)
            .map_err(|e| failure(format!("batch source: {e}")))?;
        let context = scratch.join(format!("frame_{}.context", item.frame_id));
        for (key, value) in [
            ("id", item.frame_id.to_string()),
            ("source", source.display().to_string()),
            ("context", context.display().to_string()),
        ] {
            text.push_str(&format!("item{i}_{key}={}\n", percent_encode(&value)));
        }
        expected.push((item.frame_id, source, context));
    }
    if text.len() > 1024 * 1024 {
        return Err(failure("batch request exceeds size limit"));
    }
    fs::write(&request, text).map_err(|e| failure(format!("batch request write: {e}")))?;
    let mut args = vec![
        "--prepare-batch".into(),
        request.display().to_string(),
        "--scratch".into(),
        scratch.display().to_string(),
        "--emit-result".into(),
        result.display().to_string(),
    ];
    if let Some(path) = settings.converter {
        args.extend(["--converter".into(), path.into()]);
    }
    if let Some(seconds) = settings.converter_timeout {
        args.extend(["--converter-timeout".into(), seconds.to_string()]);
    }
    settings.lens.prepare_args(&mut args);
    let captured = proc::run_capture_active(tool, &args, EXIT_FRAME, &log, active.clone())?;
    write_prepare_log(&log, tool, &args, &captured)?;
    batch_check_stop(&active)?;
    if !captured.status.success() {
        return Err(failure(format!(
            "prepared batch failed IDs {ids:?}: {}; transcript {}: {}",
            proc::exit_description(&captured.status),
            log.display(),
            proc::tail(&captured.stderr, 1200)
        )));
    }
    let mut prepared = load_batch_result(&result, &expected)?;
    for (_, frame) in &mut prepared { query_lens_area(tool, frame, &settings.lens, active.clone())?; }
    batch_check_stop(&active)?; // Late success cannot revive cancelled work.
    Ok(prepared)
}
fn load_batch_result(
    result: &Path,
    expected: &[(usize, PathBuf, PathBuf)],
) -> Result<Vec<(usize, PreparedFrame)>> {
    let fields = parse_document_fields(
        &read_document(result, 1024 * 1024)?,
        "rawtl-prepare-result/1",
        1024 * 1024,
    )?;
    let get = |key: &str| field_value(&fields, key);
    let count = parse_u32(get("count")?, "count")? as usize;
    if count != expected.len() {
        return Err(failure("batch result count differs from request"));
    }
    let mut keys = vec!["count".into()];
    let mut found = std::collections::BTreeMap::new();
    for i in 0..count {
        for k in ["id", "source", "source_sha256", "context"] {
            keys.push(format!("item{i}_{k}"));
        }
        let id = parse_u32(get(&format!("item{i}_id"))?, "id")? as usize;
        let (_, source, context) = expected
            .iter()
            .find(|(n, _, _)| *n == id)
            .ok_or_else(|| failure("batch result has extra frame ID"))?;
        if Path::new(get(&format!("item{i}_source"))?) != source
            || Path::new(get(&format!("item{i}_context"))?) != context
        {
            return Err(failure("batch result source/context differs from request"));
        }
        let frame = PreparedFrame::load(context)?;
        if frame.source != *source
            || frame.source_sha256
                != hex64(get(&format!("item{i}_source_sha256"))?, "source_sha256")?
        {
            return Err(failure("batch result context identity mismatch"));
        }
        if found.insert(id, frame).is_some() {
            return Err(failure("batch result repeats frame ID"));
        }
    }
    require_fields(&fields, &keys)?;
    expected
        .iter()
        .map(|(id, _, _)| {
            found
                .remove(id)
                .map(|f| (*id, f))
                .ok_or_else(|| failure("batch result missing frame ID"))
        })
        .collect()
}

fn query_lens_area(tool:&Path,prepared:&mut PreparedFrame,settings:&crate::lens::Settings,active:ActiveNative)->Result<()> {
    if !settings.crop.is_safe() { return Ok(()); }
    let log=prepared.context.with_extension("lens-area.log");
    let mut args=vec!["--lens-valid-area".into(),"--prepared".into(),prepared.context.display().to_string(),
        "--camera-recipe".into(),"generic".into(),"--camera-color-method".into(),"dng-camera-to-pcs".into(),
        "--profile-correction".into(),"none".into(),"--camera-calibration".into(),"none".into(),"--highlight-recovery".into(),"off".into()];
    settings.policy.args(settings.mode,&mut args);
    let captured=proc::run_capture_active(tool,&args,EXIT_FRAME,&log,active)?;
    write_prepare_log(&log,tool,&args,&captured)?;
    proc::check_stop()?;
    if !captured.status.success() {return Err(failure(format!("lens valid-area query failed: {}; transcript {}: {}",proc::exit_description(&captured.status),log.display(),proc::tail(&captured.stderr,1200))));}
    if captured.stdout.len()>16384 {return Err(failure("lens valid-area response exceeds bound"));}
    let value=crate::json::parse(&captured.stdout).map_err(|e|failure(format!("lens valid-area JSON: {e}")))?;
    prepared.lens_valid_area=Some(crate::lens::ValidArea::parse(&value,&prepared.source_sha256,&prepared.dng_sha256,settings)?);
    Ok(())
}
fn write_prepare_log(
    log: &Path,
    tool: &Path,
    args: &[String],
    captured: &proc::Captured,
) -> Result<()> {
    if let Some(parent) = log.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| Error::new(EXIT_FRAME, format!("cannot create {}: {e}", parent.display())))?;
    }
    let text = format!(
        "command: {}\nexit: {}\nstdout:\n{}\nstderr:\n{}\n",
        proc::display_command(tool, args),
        proc::exit_description(&captured.status),
        captured.stdout,
        captured.stderr
    );
    fs::write(log, text)
        .map_err(|e| Error::new(EXIT_FRAME, format!("cannot write {}: {e}", log.display())))
}

fn prepared_native_run(
    settings: &PreparedSettings,
    analysis_roi: Option<[f64; 4]>,
    log: &Path,
) -> NativeRun {
    NativeRun {
        method: settings.camera_recipe,
        lens: settings.lens.clone(),
        log: log.to_path_buf(),
        analysis_roi,
        #[cfg(test)]
        converter: None,
        #[cfg(test)]
        converter_timeout: None,
    }
}

/// Analysis pass over a prepared context. Invokes no Converter and never
/// deletes the prepared DNG.
#[cfg(test)]
pub fn analyze_prepared(
    tool: &Path,
    prepared: &PreparedFrame,
    override_wb: Option<[f64; 3]>,
    exposure_ev: Option<f64>,
    analysis_roi: Option<[f64; 4]>,
    settings: &PreparedSettings,
    log: &Path,
) -> Result<FrameInfo> {
    let run = prepared_native_run(settings, analysis_roi, log);
    run_frame(
        tool,
        FrameInput::Prepared(prepared),
        1,
        1,
        override_wb,
        exposure_ev,
        &run,
        false,
        Default::default(),
        &mut |_, _| Ok(()),
    )
    .map_err(|error| prepared_consumer_error(error, &log))
}

fn prepared_consumer_error(error: Error, log: &Path) -> Error {
    let detail = fs::read_to_string(log).unwrap_or_default();
    Error::new(
        error.code,
        format!(
            "prepared-frame consumer failed: {}; transcript {}: {}",
            error.message,
            log.display(),
            proc::tail(&detail, 1200)
        ),
    )
}

/// FP32 develop pass over a prepared context. The caller reads pixels through
/// the callback while the native process is still alive.
#[cfg(test)]
pub fn develop_prepared(
    tool: &Path,
    prepared: &PreparedFrame,
    override_wb: Option<[f64; 3]>,
    exposure_ev: Option<f64>,
    settings: &PreparedSettings,
    log: &Path,
    consume: &mut impl FnMut(&FrameInfo, &mut NativeStream) -> Result<()>,
) -> Result<FrameInfo> {
    let run = prepared_native_run(settings, None, log);
    run_frame(
        tool,
        FrameInput::Prepared(prepared),
        1,
        1,
        override_wb,
        exposure_ev,
        &run,
        true,
        Default::default(),
        consume,
    )
    .map_err(|error| prepared_consumer_error(error, &log))
}

/// Scheduler entry points own the frame ID and transcript path.
pub fn analyze_prepared_at(
    tool: &Path,
    prepared: &PreparedFrame,
    wb: Option<[f64; 3]>,
    roi: [f64; 4],
    settings: &PreparedSettings,
    id: usize,
    total: usize,
    log: &Path,
    active: ActiveNative,
) -> Result<FrameInfo> {
    let run = prepared_native_run(settings, Some(roi), log);
    run_frame(
        tool,
        FrameInput::Prepared(prepared),
        id + 1,
        total,
        wb,
        Some(0.),
        &run,
        false,
        active,
        &mut |_, _| Ok(()),
    )
    .map_err(|e| prepared_consumer_error(e, log))
}
pub fn develop_prepared_at(
    tool: &Path,
    prepared: &PreparedFrame,
    wb: Option<[f64; 3]>,
    ev: f64,
    settings: &PreparedSettings,
    id: usize,
    total: usize,
    log: &Path,
    active: ActiveNative,
    consume: &mut impl FnMut(&FrameInfo, &mut NativeStream) -> Result<()>,
) -> Result<FrameInfo> {
    let run = prepared_native_run(settings, None, log);
    run_frame(
        tool,
        FrameInput::Prepared(prepared),
        id + 1,
        total,
        wb,
        Some(ev),
        &run,
        true,
        active,
        consume,
    )
    .map_err(|e| prepared_consumer_error(e, log))
}


fn frame_args(
    input: FrameInput<'_>,
    run: &NativeRun,
    override_wb: Option<[f64; 3]>,
    exposure_ev: Option<f64>,
    pixels: bool,
    lens_mode: crate::lens::Mode,
    lens_policy: crate::lens::Policy,
) -> Result<Vec<String>> {
    let mut args = vec![if pixels {
        "--stdout-frame-f32"
    } else {
        "--stdout-analysis"
    }
    .to_string()];
    if run.method.requires_canon_cr3()
        && input
            .path()
            .extension()
            .and_then(|e| e.to_str())
            .is_none_or(|e| !e.eq_ignore_ascii_case("cr3"))
    {
        return Err(failure("rev-eng Canon Log requires Canon CR3 photos"));
    }
    match input {
        #[cfg(test)]
        FrameInput::Source(source) => args.push(source.path.display().to_string()),
        FrameInput::Prepared(prepared) => {
            // The context already records the conversion; never pass a
            // Converter option on this path.
            args.push("--prepared".into());
            args.push(prepared.context.display().to_string());
        }
    }
    if let Some(prepared) = input.prepared() {
        #[cfg(test)]
        if run.converter.is_some() || run.converter_timeout.is_some() {
            return Err(failure(
                "a prepared context already carries its Converter provenance",
            ));
        }
        if prepared.route == PreparedRoute::Direct && prepared.generated {
            return Err(failure("prepared context route/generated mismatch"));
        }
    }
    args.extend([
        "--camera-color-method".into(),
        run.method.native_token().into(),
        "--color-contract-version".into(), "2".into(),
        "--camera-recipe".into(), run.method.token().into(),
        "--profile-correction".into(), run.method.correction().into(),
        "--camera-calibration".into(), run.method.calibration().into(),
        "--highlight-recovery".into(),
        if pixels && run.method != crate::color_profile::CameraRecipe::LegacyInverseCm { "on" } else { "off" }.into(),
    ]);
    lens_policy.args(lens_mode, &mut args);
    if let Some(wb) = override_wb {
        args.extend(["--white-balance".into(), format_wb(wb)]);
    }
    if let Some(ev) = exposure_ev {
        args.extend(["--exposure-ev".into(), format_ev(ev)]);
    }
    #[cfg(test)]
    if let FrameInput::Source(_) = input {
        if let Some(path) = &run.converter {
            args.extend(["--converter".into(), path.clone()]);
        }
        if let Some(seconds) = run.converter_timeout {
            args.extend(["--converter-timeout".into(), seconds.to_string()]);
        }
    }
    if !pixels {
        if let Some(roi) = run.analysis_roi {
            args.extend([
                "--analysis-roi".into(),
                roi.iter()
                    .map(|v| format_axis(*v))
                    .collect::<Vec<_>>()
                    .join(","),
            ]);
        }
    }
    Ok(args)
}


fn run_frame(
    tool: &Path,
    input: FrameInput<'_>,
    index: usize,
    total: usize,
    override_wb: Option<[f64; 3]>,
    exposure_ev: Option<f64>,
    run: &NativeRun,
    pixels: bool,
    active: ActiveNative,
    consume: &mut impl FnMut(&FrameInfo, &mut NativeStream) -> Result<()>,
) -> Result<FrameInfo> {
    let started = Instant::now();
    let lens_mode = if pixels { run.lens.mode } else { crate::lens::Mode::Off };
    let lens_policy = if pixels { run.lens.policy } else { crate::lens::Policy::default() };
    let args = frame_args(input, run, override_wb, exposure_ev, pixels, lens_mode, lens_policy)?;
    let mut stream = NativeStream::open(tool, &args, &run.log, active)?;
    let bytes = stream.json(1)?;
    let sidecar = parse_json(&bytes)?;
    validate_sidecar_contracts(&sidecar, &run.method, pixels, lens_mode, lens_policy)?;
    let meta = frame_metadata(
        bytes,
        &sidecar,
        index,
        total,
        input.name(),
        input.path(),
        override_wb,
        exposure_ev,
    )?;
    if let Some(prepared) = input.prepared() {
        validate_prepared_metadata(
            &sidecar,
            prepared,
            meta.source_sha256.as_deref(),
            meta.as_shot_neutral,
            meta.capture_time.as_deref(),
        )?;
    }
    let mut info = FrameInfo {
        index,
        source_name: input.name().to_string(),
        camera_color: sidecar.get("color").cloned(),
        highlight_recovery: sidecar.get("highlight_recovery").cloned(),
        lens_correction: sidecar.get("lens_correction").cloned(),
        source_sha256: meta.source_sha256,
        pixels_sha256: None,
        width: meta.width,
        height: meta.height,
        applied_wb: meta.applied_wb,
        wb_source: meta.wb_source,
        as_shot_neutral: meta.as_shot_neutral,
        exposure_ev: meta.reported_ev,
        duration: Duration::ZERO,
        analysis_envelope: None,
    };
    attach_payload(&mut stream, &sidecar, &mut info, pixels)?;
    consume(&info, &mut stream)?;
    stream.finish()?;
    info.duration = started.elapsed();
    Ok(info)
}

fn attach_payload(
    stream: &mut NativeStream,
    sidecar: &Json,
    info: &mut FrameInfo,
    pixels: bool,
) -> Result<()> {
    if pixels {
        let descriptor = parse_json(&stream.json(3)?)?;
        if sidecar.get("linear_pixel_format").and_then(Json::as_str) != Some("rgbf32le") {
            return Err(failure("metadata linear_pixel_format must be rgbf32le"));
        }
        if descriptor.get("width").and_then(Json::as_u32) != Some(info.width)
            || descriptor.get("height").and_then(Json::as_u32) != Some(info.height)
            || descriptor.get("pixel_format").and_then(Json::as_str) != Some("rgbf32le")
        {
            return Err(failure("frame descriptor geometry/pixel_format mismatch"));
        }
        let digest = descriptor
            .get("pixels_sha256")
            .and_then(Json::as_str)
            .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or_else(|| failure("invalid pixels_sha256"))?;
        info.pixels_sha256 = Some(digest.into());
        {
            let expected_bytes = u64::from(info.width)
                .checked_mul(u64::from(info.height))
                .and_then(|n| n.checked_mul(12))
                .ok_or_else(|| failure("FP32 geometry overflow"))?;
            if descriptor.get("space").and_then(Json::as_str) != Some("AP1/D60")
                || descriptor.get("transfer").and_then(Json::as_str) != Some("linear")
                || descriptor
                    .get("color_contract_version")
                    .and_then(Json::as_u64)
                    != Some(2)
                || descriptor.get("byte_length").and_then(Json::as_u64) != Some(expected_bytes)
                || expected_bytes > 2_400_000_000
            {
                return Err(failure("invalid FP32 descriptor contract/allocation bound"));
            }
            stream.expected_digest = Some(digest.into());
        }
    } else {
        let bytes = stream.json(2)?;
        let envelope =
            analysis_sidecar_envelope(bytes, info.source_sha256.as_deref(), info.exposure_ev)?;
        info.analysis_envelope = Some(envelope);
    }
    Ok(())
}
