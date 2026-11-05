"""Two-pass, single-frame-at-a-time ProcessingService; no CLI/UI coupling."""
from __future__ import annotations

from dataclasses import asdict
from fractions import Fraction
from pathlib import Path
import math
import os
import shutil
import tempfile
import traceback
from typing import Callable
import numpy as np
from PIL import Image

from .types import (
    CancellationToken, EncoderError, ExposureError, InputError, ProcessingCancelled,
    ProcessingRequest, ProcessingResult, ProgressUpdate, RawDecodeError, RawRenderOptions,
    RawRenderer, VerificationError)
from .inventory import inspect_directory, prevent_output_overlap
from .raw_backend import RawpyRenderer
from .color import SRGBTransfer,pad_even_rgb8
from .frame_processing import IdentityProcessor
from .exposure import measure_frame,estimate_exposure
from .exposure.validation import (
    probe_gain,evaluate_render,feedback_update,feedback_diverged,ValidationSummary)
from .video import (
    VideoWriter,check_disk_space,probe_encoders,resolve_ffprobe)
from .quality.video_audit import DETECTOR_MODEL,VideoAuditConfig,audit_video
from .reporting import write_report,write_curve,strict_jsonable

EXECUTION_STATUS_MAP={"OK":"COMPLETED","PARTIAL":"COMPLETED_WITH_REVIEW",
    "COMPLETED_WITH_REVIEW":"COMPLETED_WITH_REVIEW"}

def _encoded_quality_fields(audit: dict) -> dict:
    """Compact view of the independent audit for the run report."""
    return {"status":audit.get("status"),
        "detector_model":audit.get("detector_model",DETECTOR_MODEL),
        "frame_count":audit.get("frame_count"),
        "candidate_count":audit.get("candidate_count"),
        "chroma_variation":audit.get("chroma_variation","NOT_IMPLEMENTED"),
        "metadata_warnings":audit.get("metadata_warnings",[]),
        "events":audit.get("events",[]),
        "segments":[{k:s.get(k) for k in ("start","end_exclusive","frames","status",
            "usable_pairs","pairs","median_abs_step_ev","p95_abs_step_ev",
            "max_abs_step_ev","curvature_rms_ev","steps_over_0_05_ev")}
            for s in audit.get("segments",[])]}

def _gain_response_fields(probe: dict) -> dict:
    if probe.get("passed") is True:
        return {"status":"PASS","median_error_ev":probe.get("median_error_ev"),
            "p95_abs_error_ev":probe.get("p95_abs_error_ev"),
            "observations":probe.get("observations")}
    if probe.get("passed") is False:
        return {"status":"FAIL","reasons":probe.get("reasons",[])[:5]}
    return {"status":"SKIPPED","reason":probe.get("reason")}

class ProcessingService:
    def __init__(self,renderer: RawRenderer | None=None):
        self._renderer=renderer
        self.processor=IdentityProcessor()
        self.transfer=SRGBTransfer()

    def _raw(self) -> RawRenderer:
        if self._renderer is None:
            self._renderer=RawpyRenderer()
        return self._renderer

    @staticmethod
    def _emit(on_progress, phase,processed,total,filename=None,encoder=None,warnings=0):
        if on_progress is not None:
            on_progress(ProgressUpdate(phase,processed,total,filename,encoder,warnings))

    @staticmethod
    def _check(cancel: CancellationToken | None):
        if cancel:cancel.check()

    def probe(self,input_dir: Path,*, ffmpeg: str="ffmpeg") -> dict:
        r=self._raw()
        inventory=inspect_directory(input_dir,r)
        meta=inventory.first
        wb=RawpyRenderer.fixed_wb(meta)
        opts=RawRenderOptions(wb)
        names=inventory.frames
        samples=sorted(set([0,len(names)//2,len(names)-1]))
        images=[]
        for idx in samples:
            frame=r.render_linear(names[idx].path,0.,opts)
            images.append({"index":idx,"filename":names[idx].filename,
                "width":frame.rgb.shape[1],"height":frame.rgb.shape[0]})
        w=images[0]["width"];h=images[0]["height"]
        if any(x["width"]!=w or x["height"]!=h for x in images):
            raise RawDecodeError("rendered orientation/dimensions differ in representative RAWs")
        fp=resolve_ffprobe(ffmpeg)
        with tempfile.TemporaryDirectory(prefix="rawtl-probe-") as probe_dir:
            available,attempts=probe_encoders(ffmpeg,fp,w+w%2,h+h%2,
                Fraction(30,1),Path(probe_dir))
        return {"count":len(names),"sorted_files":[x.filename for x in names],
            "camera":meta.camera,"raw_geometry":[meta.width,meta.height],
            "orientation":meta.orientation,"rendered_geometry":[w,h],
            "fixed_white_balance":list(wb),"representative_frames":images,
            "encoder_available":[x.name for x in available],
            "encoder_attempts":[asdict(x) for x in attempts],
            "warning":"CAMERA_IDENTITY_UNVERIFIED: ExifTool not available"
                if meta.camera=="UNKNOWN_CAMERA" else (
                "RAW_PROFILE_UNVERIFIED: verify Canon EOS R6 Mark III CR3 colour manually"
                if "R6 Mark III" in meta.camera or "R6 Mark 3" in meta.camera else None)}

    def run(self,request: ProcessingRequest,on_progress: Callable | None=None,
            cancel: CancellationToken | None=None) -> ProcessingResult:
        prevent_output_overlap(request.input_dir,request.output_dir)
        out=request.output_dir.expanduser().resolve()
        # Ensure an existing video is protected before potentially costly analysis.
        if (out/"video.mp4").exists() and not request.overwrite:
            raise InputError(f"output already exists: {out/'video.mp4'} (pass --overwrite)")
        if request.on_review not in ("error","passthrough"):
            raise InputError("on_review must be error or passthrough")
        if request.temporal_quality not in ("strict","report","off"):
            raise InputError("temporal_quality must be strict, report or off")
        if request.brightness not in ("smooth","uniform"):
            raise InputError("brightness must be smooth or uniform")
        if request.save_frames and (out/"frames").exists() and not request.overwrite:
            raise InputError(f"PNG frame output already exists: {out/'frames'} (pass --overwrite)")
        out.mkdir(parents=True,exist_ok=True)
        cancel=cancel or CancellationToken()
        report={"schema_version":1,"run_status":"RUNNING","frames":[],
            "encoder":{"selected":None,"attempts":[]},"warnings":[],
            "input_sort":"filename_unicode_case_sensitive",
            "video_target_bitrate_bps":160_000_000,
            "config":{"input":str(request.input_dir),"output":str(out),
                "fps":str(request.fps),"period":request.period,
                "strength":request.strength,"brightness":request.brightness,
                "roi":list(request.roi),
                "steps":[asdict(x) for x in request.steps],
                "on_review":request.on_review,"temporal_quality":request.temporal_quality,
                "save_frames":request.save_frames,
                "encoder":request.encoder,"ffmpeg":request.ffmpeg}}
        report["render_temporal_quality"]={"status":"NOT_INDEPENDENT",
            "note":"legacy frozen-target check is self-consistent with whatever "
                    "correction was proposed; independent render-domain targets are "
                    "tracked separately"}
        report_path=out/"exposure_report.json"
        temp_videos: list[Path]=[]
        temp_frame_dirs: list[Path]=[]
        try:
            self._check(cancel)
            r=self._raw()
            inventory=inspect_directory(request.input_dir,r)
            refs=inventory.frames
            n=len(refs)
            report["sources"]=inventory.source_manifest()
            meta=inventory.first
            wb=RawpyRenderer.fixed_wb(meta)
            render_opts=RawRenderOptions(wb)
            report["decoder"]={"camera":meta.camera,"fixed_wb":list(wb),
                "orientation":meta.orientation,
                "versions":r.versions() if hasattr(r,"versions") else {},
                "saturation_mask_quality":"approximate"}
            if meta.camera=="UNKNOWN_CAMERA":
                report["warnings"].append("CAMERA_IDENTITY_UNVERIFIED: install ExifTool for make/model preflight")
            if "R6 Mark III" in meta.camera or (
                meta.camera=="UNKNOWN_CAMERA" and meta.raw_type==".cr3"):
                report["warnings"].append("RAW_PROFILE_UNVERIFIED: Canon CR3 colour must be checked")
            self._emit(on_progress,"inventory",n,n)
            measurements=[]
            baseline_coverage=[]
            width=height=None
            for ref in refs:
                self._check(cancel)
                frame=r.render_linear(ref.path,0.,render_opts)
                h,w,_=frame.rgb.shape
                if width is None:
                    width=w;height=h
                if (w,h)!=(width,height):
                    raise RawDecodeError(f"rendered dimensions mismatch at {ref.index} {ref.filename}")
                if frame.saturation_mask is not None:
                    baseline_coverage.append(float(np.mean(frame.saturation_mask)))
                else:
                    baseline_coverage.append(float(np.mean(np.any(frame.rgb>=.98,axis=2))))
                measurements.append(measure_frame(frame,request.roi))
                self._emit(on_progress,"analysis",ref.index+1,n,ref.filename)
                del frame
            report["analysis_shape"]=[width,height]
            report["frame_count"]=n
            self._check(cancel)
            exposure=estimate_exposure(measurements,period=request.period,
                strength=request.strength,steps=request.steps,
                brightness=request.brightness)
            report["unconverged_trend_tiles"]=exposure.trend_unconverged_tiles
            report["period_effective"]=exposure.period_effective
            report["scene_cuts"]=exposure.scene_cuts
            report["frames"]=[self._frame_record(ref,est)
                for ref,est in zip(refs,exposure.frames)]
            need_review=any(x.applied_ev is None for x in exposure.frames)
            if need_review:
                report["warnings"].append("REVIEW_REQUIRED: one or more frames have no reliable EV estimate")
                if request.on_review=="passthrough":
                    for frame in exposure.frames:
                        if frame.applied_ev is None:
                            frame.warnings.append("PASSTHROUGH_UNCORRECTED_FRAME: output rendered at 0 EV")
                if request.on_review=="error":
                    report["run_status"]="REVIEW_REQUIRED"
                    raise ExposureError("exposure estimate incomplete: use --on-review passthrough to output zeros for unreliable frames")
            if request.strength!=0 and n>=8 and any(x.applied_ev is not None for x in exposure.frames):
                self._emit(on_progress,"probe",0,n)
                probe=probe_gain(r,refs,render_opts,measurements,roi=request.roi)
                report["gain_probe"]=asdict(probe)
                if not probe.passed:
                    report["run_status"]="GAIN_PROBE_FAILED"
                    raise ExposureError("RAW exposure gain probe failed: "+str(probe.reasons[:3]))
            else:
                report["gain_probe"]={"status":"SKIPPED","reason":"disabled, short sequence or no reliable exposures"}
            ffprobe=resolve_ffprobe(request.ffmpeg)
            width_even=width+width%2
            height_even=height+height%2
            expected=check_disk_space(out,n,request.fps)
            report["video_expected_budget_bytes"]=expected
            available,attempts=probe_encoders(request.ffmpeg,ffprobe,
                width_even,height_even,request.fps,out,selection=request.encoder)
            report["encoder"]["attempts"]=[asdict(x) for x in attempts]
            report["padded_dimensions"]=[width_even,height_even]
            previous_summary=None
            best_video=None
            best_png=None
            best_encoder=None
            best_video_info=None
            best_score=(float("inf"),float("inf"))
            accepted_summary=None
            best_frame_state=None
            feedback_count=0
            # First pass plus at most two feedback passes.
            for iteration in range(3):
                self._check(cancel)
                round_success=False
                for enc in available:
                    self._check(cancel)
                    frame_dir=None
                    if request.save_frames:
                        frame_dir=Path(tempfile.mkdtemp(prefix=".rawtl-frames-",dir=out))
                        temp_frame_dirs.append(frame_dir)
                    rendered=[]
                    try:
                        with VideoWriter(request.ffmpeg,enc,out,width_even,height_even,
                                         request.fps) as writer:
                            temp_videos.append(writer.path)
                            for ref,est,base_coverage in zip(refs,exposure.frames,baseline_coverage):
                                self._check(cancel)
                                ev=est.applied_ev if est.applied_ev is not None else 0.
                                est.render_ev=float(ev)
                                current=r.render_linear(ref.path,ev,render_opts)
                                if current.rgb.shape[:2]!=(height,width):
                                    raise RawDecodeError(f"RAW dimensions changed while rendering {ref.filename}")
                                current_measure=measure_frame(current,request.roi)
                                rendered.append(current_measure)
                                current_coverage=float(np.mean(current.saturation_mask)) if current.saturation_mask is not None else float(np.mean(np.any(current.rgb>=.98,axis=2)))
                                est.new_output_clipping_fraction=max(0.,current_coverage-base_coverage)
                                processed=self.processor.process(current)
                                rgb8=pad_even_rgb8(self.transfer.to_rgb8(processed))
                                if frame_dir is not None:
                                    Image.fromarray(rgb8,"RGB").save(frame_dir/f"frame_{ref.index+1:06d}.png")
                                writer.write(rgb8,cancel)
                                del current,processed,rgb8
                                self._emit(on_progress,"render",ref.index+1,n,ref.filename,enc.name)
                            info=writer.finish(ffprobe,n)
                            video_path=writer.path
                        round_success=True
                        report["encoder"]["selected"]=enc.name
                        report["video_metadata"]=info
                        break
                    except (EncoderError,VerificationError) as exc:
                        report["encoder"]["attempts"].append({
                            "encoder":enc.name,"available":False,
                            "reason":f"runtime failure: {exc}","device":enc.device})
                        if frame_dir and frame_dir.exists():
                            shutil.rmtree(frame_dir)
                        continue
                    except Exception:
                        if frame_dir and frame_dir.exists():
                            shutil.rmtree(frame_dir)
                        raise
                if not round_success:
                    raise EncoderError("all probed encoders failed during actual RAW video encode")
                # Validate the applied gain against the *original* frozen targets.
                must_validate=request.strength!=0 and n>=8 and np.any(exposure.trusted_indices)
                summary=(evaluate_render(exposure,rendered,iteration=iteration)
                    if must_validate else None)
                if summary is None:
                    best_video=video_path;best_png=frame_dir;accepted_summary=None
                    best_encoder=enc.name;best_video_info=info
                    break
                current_score=(
                    summary.median_abs_error_ev if summary.median_abs_error_ev is not None else float("inf"),
                    summary.p95_abs_error_ev if summary.p95_abs_error_ev is not None else float("inf"))
                report.setdefault("feedback_history",[]).append({
                    "iteration":iteration,"median_abs_error_ev":summary.median_abs_error_ev,
                    "p95_abs_error_ev":summary.p95_abs_error_ev,
                    "outlier_frames":summary.outlier_frames,
                    "unobservable_frames":summary.unobservable_frames,"passed":summary.passed})
                if (current_score<best_score or
                    (summary.passed and (accepted_summary is None or not accepted_summary.passed))):
                    if best_video is not None and best_video!=video_path:
                        best_video.unlink(missing_ok=True)
                    if best_png is not None and best_png!=frame_dir and best_png.exists():
                        shutil.rmtree(best_png)
                    best_score=current_score
                    best_video=video_path;best_png=frame_dir
                    best_encoder=enc.name;best_video_info=info
                    accepted_summary=summary
                    best_frame_state=[(e.applied_ev,e.render_ev,e.render_target_error_ev,
                        e.new_output_clipping_fraction) for e in exposure.frames]
                else:
                    video_path.unlink(missing_ok=True)
                    if frame_dir and frame_dir.exists():shutil.rmtree(frame_dir)
                if summary.passed:
                    break
                if previous_summary is not None and feedback_diverged(previous_summary,summary):
                    report["warnings"].append("FEEDBACK_DIVERGED: aborted and retained best candidate")
                    break
                if iteration>=2:
                    break
                previous_summary=summary
                updated=feedback_update(exposure,summary)
                if updated==0:
                    break
                feedback_count+=1
            report["feedback_updates"]=feedback_count
            if best_frame_state is not None:
                for est,state in zip(exposure.frames,best_frame_state):
                    (est.applied_ev,est.render_ev,est.render_target_error_ev,
                        est.new_output_clipping_fraction)=state
            if accepted_summary is not None and not accepted_summary.passed:
                report["run_status"]="TARGET_NOT_REACHED"
                raise ExposureError("renderer failed to achieve frozen exposure targets within two feedback updates")
            if best_video is None or not best_video.exists():
                raise VerificationError("missing verified temporary MP4")
            report["encoder"]["selected"]=best_encoder
            report["video_metadata"]=best_video_info
            report["gain_response"]=_gain_response_fields(report["gain_probe"])
            # Independent temporal audit of the encoded artifact: separate from
            # the container-level verify_video check and from any input-side
            # photometry. Pixels are encoded with the sRGB OETF today; the
            # stream-tag mismatch is recorded and fixed with the colour task.
            audit_result=None
            if request.temporal_quality!="off":
                self._emit(on_progress,"audit",0,n,encoder=report["encoder"]["selected"])
                audit_config=VideoAuditConfig(transfer="srgb",
                    frame_labels=[ref.filename for ref in refs])
                try:
                    audit_result=audit_video(best_video,ffmpeg=request.ffmpeg,
                        ffprobe=ffprobe,config=audit_config,output_dir=out)
                except VerificationError as exc:
                    audit_result={"status":"UNOBSERVABLE","error":str(exc),
                        "detector_model":DETECTOR_MODEL,"events":[],"segments":[]}
                    report["warnings"].append(f"TEMPORAL_AUDIT_UNOBSERVABLE: {exc}")
                report["encoded_temporal_quality"]=_encoded_quality_fields(audit_result)
                if audit_result.get("status")=="FAIL":
                    events=audit_result.get("events",[])
                    summary=(f"encoded temporal quality FAIL: {len(events)} events >0.05 EV; "
                        f"worst={max((e['common_ev'] for e in events),key=abs):+.4f} EV"
                        if events else "encoded temporal quality FAIL: thresholds exceeded")
                    if request.temporal_quality=="strict":
                        report["run_status"]="TEMPORAL_QUALITY_FAILED"
                        raise VerificationError(summary+"; rerun with --temporal-quality report "
                            "to keep the video flagged for review")
                    report["warnings"].append(summary)
            else:
                report["encoded_temporal_quality"]={"status":"SKIPPED",
                    "note":"temporal audit disabled by request"}
            self._emit(on_progress,"verify",n,n,encoder=report["encoder"]["selected"])
            # The final PNGs are from the same accepted correction round, not failed retries.
            if best_png is not None:
                self._publish_frames(best_png,out/"frames",request.overwrite)
            # Only committed after correct frame count and all quality gates.
            os.replace(best_video,out/"video.mp4")
            report["run_status"]="PARTIAL" if need_review else "OK"
            if audit_result is not None and audit_result.get("status")=="FAIL":
                report["run_status"]="COMPLETED_WITH_REVIEW"
            report["execution_status"]=EXECUTION_STATUS_MAP.get(
                report["run_status"],report["run_status"])
            report["frames"]=[self._frame_record(ref,est)
                for ref,est in zip(refs,exposure.frames)]
            report["status_counts"]={
                key:sum(z.status==key for z in exposure.frames)
                for key in sorted({z.status for z in exposure.frames})}
            self._emit(on_progress,"done",n,n,encoder=report["encoder"]["selected"])
            return ProcessingResult(report["run_status"],out,out/"video.mp4",
                report_path,n,report["encoder"]["selected"])
        except ProcessingCancelled as exc:
            report["run_status"]="CANCELLED";report["error"]=str(exc)
            raise
        except Exception as exc:
            if report["run_status"]=="RUNNING":
                report["run_status"]="FAILED"
            report["error"]=str(exc)
            report["traceback"]=traceback.format_exc()
            raise
        finally:
            # Every failure retains diagnostics. NaN is not legal JSON output.
            try:
                if "exposure" in locals():
                    report["frames"]=[self._frame_record(ref,est)
                        for ref,est in zip(refs,exposure.frames)]
                write_report(report_path,strict_jsonable(report))
                if "exposure" in locals():
                    write_curve(out/"exposure_curve.csv",report["frames"])
            finally:
                for p in temp_videos:
                    p.unlink(missing_ok=True)
                for p in temp_frame_dirs:
                    if p.exists():shutil.rmtree(p,ignore_errors=True)

    @staticmethod
    def _frame_record(ref,est) -> dict:
        data={"index":ref.index,"filename":ref.filename,"segment_id":est.segment_id,
            "status":est.status,"valid_tile_count":est.valid_tile_count,
            "consensus_fraction":est.consensus_fraction,
            "residual_mad_ev":est.residual_mad_ev,
            "common_residual_ev":est.common_residual_ev,
            "cumulative_confirmed_step_ev":est.cumulative_confirmed_step_ev,
            "requested_ev":est.requested_ev,
            "applied_ev":est.applied_ev,"render_ev":est.render_ev,
            "gain":est.gain,
            "render_target_error_ev":est.render_target_error_ev,
            "new_output_clipping_fraction":est.new_output_clipping_fraction,
            "warnings":est.warnings}
        return data

    @staticmethod
    def _publish_frames(src: Path,dst: Path,overwrite: bool):
        backup=None
        if dst.exists():
            if not overwrite:
                raise InputError(f"PNG frame output exists: {dst}; pass --overwrite")
            backup=dst.with_name(dst.name+".rawtl-backup")
            if backup.exists():
                raise InputError(f"stale frame backup exists: {backup}")
            dst.rename(backup)
        try:
            src.rename(dst)
        except Exception:
            if backup is not None:backup.rename(dst)
            raise
        if backup is not None:
            shutil.rmtree(backup)
