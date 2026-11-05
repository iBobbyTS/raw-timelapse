"""FFmpeg raw-RGB pipe, actual encoder probes, device priority and strict MP4 validation."""
from __future__ import annotations
from dataclasses import dataclass
from fractions import Fraction
from pathlib import Path
import json
import math
import os
import platform
import shutil
import subprocess
import tempfile
import numpy as np
from .types import CancellationToken, EncoderError, VerificationError

VIDEO_BITRATE_BPS = 160_000_000
BITRATE_STR = str(VIDEO_BITRATE_BPS)
BUFFER_STR = str(VIDEO_BITRATE_BPS * 2)

@dataclass(frozen=True)
class EncoderCandidate:
    name: str
    ffmpeg_codec: str
    device: str | None = None

@dataclass
class EncoderAttempt:
    encoder: str
    available: bool
    reason: str
    device: str | None = None

def resolve_ffprobe(ffmpeg: str) -> str:
    path=Path(ffmpeg)
    if path.parent!=Path("."):
        exe=path.with_name("ffprobe.exe" if path.suffix.lower()==".exe" else "ffprobe")
        if exe.is_file():
            return str(exe)
    binary=shutil.which("ffprobe")
    if binary is None:
        raise EncoderError("ffprobe not found alongside FFmpeg or on PATH")
    return binary

def amd_vaapi_devices(sys_root: Path=Path("/sys/class/drm"),
    dev_root: Path=Path("/dev/dri")) -> list[str]:
    found=[]
    for parent in sorted(sys_root.glob("renderD*")):
        try:
            vendor=(parent/"device"/"vendor").read_text().strip().lower()
        except OSError:
            continue
        name=dev_root/parent.name
        if vendor=="0x1002" and name.exists() and os.access(name,os.R_OK|os.W_OK):
            found.append(str(name))
    return found

def candidates(system: str | None=None,amd_devices: list[str] | None=None) -> list[EncoderCandidate]:
    system=(system or platform.system()).lower()
    result=[]
    if system in ("linux","windows"):
        result.append(EncoderCandidate("nvenc","h264_nvenc"))
    if system=="windows":
        result.append(EncoderCandidate("amf","h264_amf"))
    elif system=="linux":
        for device in (amd_devices if amd_devices is not None else amd_vaapi_devices()):
            result.append(EncoderCandidate("vaapi","h264_vaapi",device))
    elif system=="darwin":
        result.append(EncoderCandidate("videotoolbox","h264_videotoolbox"))
    result.append(EncoderCandidate("libx264","libx264"))
    return result

def check_disk_space(out_dir: Path,n: int,fps: Fraction) -> int:
    estimate=int(n/float(fps)*(VIDEO_BITRATE_BPS/8))
    if shutil.disk_usage(out_dir).free < 2*estimate+128*1024*1024:
        raise EncoderError(f"insufficient disk space: need ~{2*estimate+128*1024*1024} bytes free")
    return estimate

def ffmpeg_command(ffmpeg: str,c: EncoderCandidate,width: int,height: int,
    fps: Fraction, output: Path) -> list[str]:
    cmd=[ffmpeg,"-hide_banner","-nostdin","-loglevel","warning"]
    if c.name=="vaapi":
        if not c.device:
            raise EncoderError("VAAPI render node unspecified")
        cmd+=["-vaapi_device",c.device]
    cmd+=["-f","rawvideo","-pixel_format","rgb24","-video_size",
        f"{width}x{height}","-framerate",str(fps),"-i","pipe:0",
        "-an"]
    if c.name=="vaapi":
        cmd+=["-vf","format=nv12,hwupload"]
    else:
        # Explicit RGB->YUV matrix and range: never rely on swscale defaults.
        cmd+=["-vf","scale=out_color_matrix=bt709:in_range=full:out_range=tv"]
    cmd+=["-c:v",c.ffmpeg_codec,"-b:v",BITRATE_STR,"-maxrate",BITRATE_STR,
        "-bufsize",BUFFER_STR]
    if c.name=="libx264":
        # libx264 ignores the generic -color_* options for VUI writing on
        # FFmpeg 8; the codec-private parameters do write them.
        cmd+=["-x264-params",
              "colorprim=bt709:transfer=iec61966-2-1:colormatrix=bt709"]
    if c.name!="vaapi":
        cmd+=["-pix_fmt","yuv420p"]
    # Colour contract, strategy (a): pixels carry the sRGB OETF (see
    # color.py); declare exactly that, with BT.709 primaries/matrix and
    # limited range. sRGB and BT.709 transfer functions are NOT identical --
    # declaring bt709 here would misdescribe the pixels.
    cmd+=["-color_primaries","bt709","-color_trc","iec61966-2-1",
        "-colorspace","bt709","-color_range","tv",
        "-movflags","+faststart","-y",str(output)]
    return cmd

def _bitrate_stats(ffprobe: str, path: Path, fps: Fraction) -> dict:
    """Actual average bitrate and the worst one-second-window peak."""
    try:
        result=subprocess.run(
            [ffprobe,"-v","error","-select_streams","v:0","-show_packets",
             "-show_entries","packet=pts_time,size","-of","json",str(path)],
            capture_output=True,text=True,check=True,timeout=180)
        packets=[(float(p["pts_time"]),int(p["size"])) for p in
                 json.loads(result.stdout)["packets"] if "pts_time" in p]
    except (OSError,subprocess.SubprocessError,ValueError,KeyError):
        return {"avg_bitrate_bps":None,"peak_1s_bitrate_bps":None}
    if not packets:
        return {"avg_bitrate_bps":None,"peak_1s_bitrate_bps":None}
    total_bits=sum(size for _,size in packets)*8
    duration=max(pts for pts,_ in packets)-min(pts for pts,_ in packets)+1/float(fps)
    peak=0; window=0; start=packets[0][0]
    for pts,size in packets:
        window+=size
        if pts-start>=1.0:
            peak=max(peak,window); window=size; start=pts
    peak=max(peak,window)
    return {"avg_bitrate_bps":int(total_bits/max(duration,1e-9)),
            "peak_1s_bitrate_bps":int(peak*8)}

def verify_video(path: Path, ffprobe: str, count: int,width: int,
    height: int,fps: Fraction) -> dict:
    cmd=[ffprobe,"-v","error","-count_frames","-show_entries",
        "stream=index,codec_type,codec_name,width,height,r_frame_rate,nb_read_frames,pix_fmt,"+
        "color_primaries,color_transfer,color_space,color_range",
        "-of","json",str(path)]
    try:
        result=subprocess.run(cmd,capture_output=True,text=True,check=True,timeout=120)
        streams=json.loads(result.stdout)["streams"]
    except (OSError,subprocess.SubprocessError,ValueError,KeyError) as exc:
        raise VerificationError(f"ffprobe validation failed for {path}: {exc}") from exc
    vid=[x for x in streams if x.get("codec_type")=="video"]
    if len(vid)!=1 or any(x.get("codec_type")=="audio" for x in streams):
        raise VerificationError("expected one video stream and no audio")
    v=vid[0]
    actual= v.get("nb_read_frames")
    if (v.get("codec_name")!="h264" or int(v.get("width",0))!=width or
        int(v.get("height",0))!=height or actual in (None,"N/A") or
        int(actual)!=count or Fraction(v["r_frame_rate"])!=fps or
        v.get("pix_fmt") not in ("yuv420p","nv12")):
        raise VerificationError(f"H.264 video dimensions/framerate/pixfmt/frame count mismatch: {v}")
    tags={"primaries":v.get("color_primaries"),"transfer":v.get("color_transfer"),
        "matrix":v.get("color_space"),"range":v.get("color_range")}
    contract_warnings=[]
    if tags["primaries"] not in ("bt709",):
        contract_warnings.append(f"COLOR_PRIMARIES_TAG_MISSING_OR_UNEXPECTED: {tags['primaries']!r}")
    if tags["transfer"]!="iec61966-2-1":
        contract_warnings.append(f"COLOR_TRANSFER_TAG_MISSING_OR_UNEXPECTED: {tags['transfer']!r} "
            "(pixels are sRGB OETF; VideoToolbox builds may refuse VUI transfer tags)")
    if tags["matrix"]!="bt709":
        contract_warnings.append(f"COLOR_MATRIX_TAG_MISSING_OR_UNEXPECTED: {tags['matrix']!r}")
    if tags["range"] not in ("tv","limited"):
        contract_warnings.append(f"COLOR_RANGE_TAG_MISSING_OR_UNEXPECTED: {tags['range']!r}")
    info={"codec":v["codec_name"],"width":width,"height":height,
        "fps":str(fps),"frames":int(actual),"pix_fmt":v.get("pix_fmt"),
        "color_tags":tags,"colour_contract_warnings":contract_warnings,
        "file_size":path.stat().st_size}
    info.update(_bitrate_stats(ffprobe,path,fps))
    return info

class VideoWriter:
    """Owns a single FFmpeg subprocess and one temporary output file."""
    def __init__(self,ffmpeg: str,candidate: EncoderCandidate,
        output_dir: Path,width: int,height: int,fps: Fraction):
        if width%2 or height%2 or width<=0 or height<=0:
            raise EncoderError("VideoWriter requires positive even dimensions")
        self.ffmpeg=ffmpeg
        self.candidate=candidate
        self.width=width
        self.height=height
        self.fps=fps
        self.count=0
        self.proc=None
        output_dir.mkdir(parents=True,exist_ok=True)
        fd,name=tempfile.mkstemp(prefix=".rawtl-encode-",suffix=".mp4",dir=output_dir)
        os.close(fd)
        self.path=Path(name)
        self.stderr=tempfile.TemporaryFile(mode="w+t",encoding="utf-8")
    def _errors(self) -> str:
        self.stderr.flush()
        self.stderr.seek(0)
        return self.stderr.read()[-4000:]
    def __enter__(self):
        try:
            self.proc=subprocess.Popen(ffmpeg_command(self.ffmpeg,self.candidate,
                self.width,self.height,self.fps,self.path),
                stdin=subprocess.PIPE,stdout=subprocess.DEVNULL,stderr=self.stderr)
        except OSError as exc:
            self.cleanup()
            raise EncoderError(f"could not launch {self.candidate.name}: {exc}") from exc
        return self
    def write(self,rgb: np.ndarray, cancel: CancellationToken | None=None) -> None:
        if cancel:
            cancel.check()
        if (rgb.dtype!=np.uint8 or rgb.shape!=(self.height,self.width,3)):
            raise EncoderError(f"RGB frame shape/dtype mismatch: {rgb.shape}/{rgb.dtype}")
        if self.proc is None or self.proc.stdin is None:
            raise EncoderError("video process not running")
        try:
            buf=np.ascontiguousarray(rgb).tobytes()
            if len(buf)!=self.width*self.height*3:
                raise EncoderError("invalid RGB24 payload length")
            self.proc.stdin.write(buf)
            self.count+=1
        except (BrokenPipeError,OSError) as exc:
            self.proc.wait(timeout=30)
            raise EncoderError(
                f"{self.candidate.name} failed while writing frame {self.count}: {self._errors()}") from exc
    def finish(self,ffprobe: str,expected_count: int) -> dict:
        if self.proc is None or self.proc.stdin is None:
            raise EncoderError("video process not started")
        self.proc.stdin.close()
        try:
            code=self.proc.wait(timeout=300)
        except subprocess.TimeoutExpired as exc:
            self.proc.kill();self.proc.wait()
            raise EncoderError("FFmpeg timed out during finalization") from exc
        if code!=0 or self.count!=expected_count:
            raise EncoderError(
                f"{self.candidate.name} exited {code}; wrote {self.count}/{expected_count} frames: {self._errors()}")
        return verify_video(self.path,ffprobe,expected_count,self.width,self.height,self.fps)
    def cleanup(self):
        if self.proc is not None and self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=3)
            except subprocess.TimeoutExpired:
                self.proc.kill();self.proc.wait()
        if self.proc is not None and self.proc.stdin is not None:
            try:self.proc.stdin.close()
            except OSError:pass
        self.stderr.close()
        self.path.unlink(missing_ok=True)
    def __exit__(self,exc_type,exc,tb):
        if exc_type is not None:
            self.cleanup()
        else:
            # Callers must move a verified output out of temp path before leaving.
            if self.proc is not None and self.proc.poll() is None:
                self.cleanup()
            else:
                self.stderr.close()

def listed_encoders(ffmpeg: str) -> set[str]:
    try:
        p=subprocess.run([ffmpeg,"-hide_banner","-encoders"],capture_output=True,
            text=True,timeout=20,check=True)
    except (OSError,subprocess.SubprocessError) as exc:
        raise EncoderError(f"FFmpeg encoder listing failed: {exc}") from exc
    return {line.split()[1] for line in p.stdout.splitlines()
        if len(line.split())>1 and line.lstrip().startswith("V")}

def probe_encoders(ffmpeg: str,ffprobe: str, width: int,height: int,fps: Fraction,
    out_dir: Path, *, selection: str="auto",
    systems: list[EncoderCandidate] | None=None) -> tuple[list[EncoderCandidate],list[EncoderAttempt]]:
    available=[];attempts=[]
    try:
        listed=listed_encoders(ffmpeg)
    except EncoderError as exc:
        raise EncoderError(f"cannot inspect installed FFmpeg: {exc}") from exc
    all_candidates=systems if systems is not None else candidates()
    if selection!="auto":
        all_candidates=[x for x in all_candidates if x.name==selection]
        if not all_candidates:
            raise EncoderError(f"forced encoder {selection} is unavailable on this platform")
    for c in all_candidates:
        if c.ffmpeg_codec not in listed:
            attempts.append(EncoderAttempt(c.name,False,"encoder not included in FFmpeg",c.device))
            continue
        try:
            with VideoWriter(ffmpeg,c,out_dir,width,height,fps) as writer:
                # Two actual target-resolution frames, not a tiny fake hardware smoke test.
                blank=np.zeros((height,width,3),dtype=np.uint8)
                for _ in range(2):
                    writer.write(blank)
                writer.finish(ffprobe,2)
                writer.path.unlink(missing_ok=True)
            available.append(c)
            attempts.append(EncoderAttempt(c.name,True,"two-frame H.264 probe passed",c.device))
        except (EncoderError,VerificationError,OSError) as exc:
            attempts.append(EncoderAttempt(c.name,False,str(exc)[-400:],c.device))
    if not available:
        raise EncoderError("no tested H.264 encoders available: "+str(attempts))
    return available,attempts
