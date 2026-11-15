"""Prepare/Develop lens contract through real SDK pixels and bounded XML adapters."""
import hashlib, json, math, pathlib, struct, subprocess, sys, time, urllib.parse
from dng_fixture import make_dng
exe, work = map(pathlib.Path, sys.argv[1:]); exe = exe.resolve(); work = work / str(time.time_ns()); work.mkdir(parents=True, exist_ok=True)
receipts = []
def command(argv, expected=0):
    r = subprocess.run([str(exe), *map(str, argv)], capture_output=True)
    receipts.append(dict(argv=list(map(str, argv)), exit_code=r.returncode, stderr=r.stderr.decode(), stdout_sha256=hashlib.sha256(r.stdout).hexdigest()))
    (work/'commands.json').write_text(json.dumps(receipts, indent=2))
    assert r.returncode == expected, (r.returncode, r.stderr)
    return r.stdout

def stream(context, mode='auto', analysis=False, extra=()):
    data=command(['--stdout-analysis' if analysis else '--stdout-frame-f32', '--prepared', context, '--lens-correction', mode, *extra])
    assert data[:8]==(b'RAWTLST1' if analysis else b'RAWTLST2'); at=8; records={}
    while at<len(data):
        kind, zero, n=struct.unpack_from('<B3sQ', data, at); at+=12; assert zero==bytes(3)
        records[kind]=data[at:at+n]; at+=n
    return json.loads(records[1]), records

rgb=[]
for y in range(48):
    for x in range(64): rgb += [300+x+2*y, 500+2*x+y, 700+x+y]
source=make_dng(work,'lens-ramp',w=64,h=48,white=2000,pixels=rgb)
original_hash=hashlib.sha256(source.read_bytes()).hexdigest()
profiles=work/'profiles'; profiles.mkdir(exist_ok=True)
profile=profiles/'arbitrary-name.lcp'
def lcp(radial=-.12):
    rows=[]
    for focal, factor in [(10,1/3),(30,1)]:
        rows.append(f'''<rdf:li><rdf:Description stCamera:Make="Test" stCamera:Model="LinearProbe" stCamera:Lens="Known Lens" stCamera:CameraRawProfile="True" stCamera:SensorFormatFactor="1" stCamera:ImageWidth="64" stCamera:ImageLength="48" stCamera:FocalLength="{focal}" stCamera:ApertureValue="4" stCamera:FocusDistance="10">
<stCamera:PerspectiveModel stCamera:Version="2" stCamera:FocalLengthX="1" stCamera:FocalLengthY="1" stCamera:RadialDistortParam1="{radial*factor}">
<stCamera:VignetteModel stCamera:FocalLengthX="1" stCamera:FocalLengthY="1" stCamera:VignetteModelParam1="{-.6*factor}"/>
<stCamera:ChromaticRedGreenModel stCamera:FocalLengthX="1" stCamera:FocalLengthY="1" stCamera:ScaleFactor=".998"/>
<stCamera:ChromaticGreenModel stCamera:FocalLengthX="1" stCamera:FocalLengthY="1"/>
<stCamera:ChromaticBlueGreenModel stCamera:FocalLengthX="1" stCamera:FocalLengthY="1" stCamera:ScaleFactor="1.002"/>
</stCamera:PerspectiveModel></rdf:Description></rdf:li>''')
    return '<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:stCamera="http://ns.adobe.com/photoshop/1.0/camera-profile"><rdf:Seq>'+''.join(rows)+'</rdf:Seq></rdf:RDF>'
profile.write_text(lcp())
metadata=work/'metadata-tool'; metadata.write_text('''#!/usr/bin/env python3
import pathlib,sys,html
print('<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">')
for p in sys.argv[1:]:
 if not p.endswith('.dng'): continue
 print('<rdf:Description rdf:about="'+html.escape(p,quote=True)+'" xmlns:IFD0="http://ns.exiftool.org/EXIF/IFD0/1.0/" xmlns:ExifIFD="http://ns.exiftool.org/EXIF/ExifIFD/1.0/" xmlns:XMP-aux="http://ns.exiftool.org/XMP/XMP-aux/1.0/" xmlns:Track4="http://ns.exiftool.org/MakerNotes/Track4/1.0/">')
 print('<IFD0:Make>Test</IFD0:Make><IFD0:Model>LinearProbe</IFD0:Model><ExifIFD:LensModel>Known Lens</ExifIFD:LensModel><ExifIFD:FocalLength>20</ExifIFD:FocalLength><ExifIFD:FocalLengthIn35mmFormat>20</ExifIFD:FocalLengthIn35mmFormat><ExifIFD:FNumber>4</ExifIFD:FNumber>')
 for key in ['VignetteCorrectionAlreadyApplied','DistortionCorrectionAlreadyApplied','LateralChromaticAberrationCorrectionAlreadyApplied']: print('<XMP-aux:'+key+'>False</XMP-aux:'+key+'>')
 print('<Track4:FNumber>4</Track4:FNumber><Track4:FNumber>4.1</Track4:FNumber>')
 print('</rdf:Description>')
print('</rdf:RDF>')
'''); metadata.chmod(0o755)
prepare_options=['--lens-correction','auto','--exiftool',metadata.resolve(),'--lens-profile-dir',profiles.resolve()]
scratch=work/'single'; scratch.mkdir(exist_ok=True); context=scratch/'frame.context'
command(['--prepare',source.resolve(),'--scratch',scratch,'--emit-prepared',context,*prepare_options])
batch=work/'batch';batch.mkdir(exist_ok=True);batch_context=batch/'frame.context'
request=batch/'request'; result=batch/'result'
def encode(s):return urllib.parse.quote(str(s),safe='/ :,._-')
request.write_text('rawtl-prepare-request/1'+chr(10)+'count=1'+chr(10)+''.join(k+'='+encode(v)+chr(10) for k,v in [('item0_id',0),('item0_source',source.resolve()),('item0_context',batch_context.resolve())]))
command(['--prepare-batch',request,'--scratch',batch,'--emit-result',result,*prepare_options])
def fields(p):return dict((k,urllib.parse.unquote(v)) for k,v in (line.split('=',1) for line in p.read_text().splitlines()[1:]))
assert fields(context)['lens_calibration']==fields(batch_context)['lens_calibration'],'single/batch calibration mismatch'
# A conflicting authoritative capture value must still fail rather than pick one.
conflict=work/"metadata-conflict"
conflict.write_text(metadata.read_text().replace("<ExifIFD:FNumber>4</ExifIFD:FNumber>","<ExifIFD:FNumber>4</ExifIFD:FNumber><ExifIFD:FNumber>8</ExifIFD:FNumber>")); conflict.chmod(0o755)
conflict_scratch=work/"conflict"; conflict_scratch.mkdir()
command(["--prepare",source.resolve(),"--scratch",conflict_scratch,"--emit-prepared",conflict_scratch/"frame.context","--lens-correction","auto","--exiftool",conflict.resolve(),"--lens-profile-dir",profiles.resolve()],26)
assert not (conflict_scratch/"frame.context").exists()
# A homogeneous HSM must consume lens delivery pixels, not raw observations.
hsm_source=make_dng(work,"lens-hsm",w=64,h=48,white=2000,pixels=rgb,hue_sat=([2,2,1],[0,1,2]*4))
hsm_scratch=work/"hsm";hsm_scratch.mkdir();hsm_context=hsm_scratch/"frame.context"
command(["--prepare",hsm_source.resolve(),"--scratch",hsm_scratch,"--emit-prepared",hsm_context,*prepare_options])
# Develop consumes frozen parameters, not metadata tools or installed profile files.
metadata.write_text('#!/bin/sh'+chr(10)+'exit 99'+chr(10)); profile.write_text('INVALID AFTER PREPARE')
on,a=stream(context); off,b=stream(context,'off'); assert a[4]!=b[4]
report=on['lens_correction']; assert [x['status'] for x in report['components']]==['applied']*3,report
assert all(x['fingerprint'] for x in report['components'])
assert report['outside_samples']==0 and on['width']==64 and on['height']==48
assert stream(batch_context)[1][4]==a[4]
# Analysis always uses observations, regardless of request.
x,px=stream(context,analysis=True);y,py=stream(context,'off',analysis=True)
assert px[2]==py[2] and not x['lens_correction']['requested']
# EV remains a single downstream scalar.
_,ev=stream(context,extra=['--exposure-ev','1'])
va=struct.unpack('<'+'f'*(len(a[4])//4),a[4]); ve=struct.unpack('<'+'f'*(len(ev[4])//4),ev[4])
assert all(abs(y-2*x)<1e-5*max(1,abs(y)) for x,y in zip(va,ve))
# DNG fixes the zero-saturation row to unity. With V=2 at S=1 the
# interpolated value gain is 1+saturation, not a constant two. Derive
# ProPhoto <- AP1 independently from primaries to predict that gain.
from reference_color import D, xy, mm, mv, inv, cat, D50, D60
def primary_matrix(primaries, white):
 columns=list(map(list,zip(*(xy(*p) for p in primaries))))
 factors=mv(inv(columns),white)
 return [[columns[i][j]*factors[j] for j in range(3)] for i in range(3)]
ap1=primary_matrix([(.713,.293),(.165,.830),(.128,.044)],D60)
prophoto=primary_matrix([(.7347,.2653),(.1596,.8404),(.0366,.0001)],D50)
ap1_to_prophoto=[[float(v) for v in row] for row in mm(inv(prophoto),mm(cat(D60,D50),ap1))]
for mode in ["auto","off"]:
 matrix_args=["--camera-color-method","dng-camera-to-pcs","--camera-recipe","legacy-test","--profile-correction","none"]
 _,matrix=stream(hsm_context,mode,extra=matrix_args)
 _,hsm=stream(hsm_context,mode,extra=[*matrix_args,"--profile-correction","hue-sat-map"])
 m=struct.unpack("<"+"f"*(len(matrix[4])//4),matrix[4]); h=struct.unpack("<"+"f"*(len(hsm[4])//4),hsm[4])
 for i in range(0,len(m),3):
  rgb_p=[sum(a*b for a,b in zip(row,m[i:i+3])) for row in ap1_to_prophoto]
  peak=max(rgb_p); gain=1 if min(rgb_p)<0 or peak<=0 else 1+(peak-min(rgb_p))/peak
  assert all(abs(y-gain*x)<5e-5*max(1,abs(y)) for x,y in zip(m[i:i+3],h[i:i+3])),("HSM did not consume corrected delivery pixels",mode,i,gain,m[i:i+3],h[i:i+3])
# Corrupt/missing versions fail closed, before emitting pixel success.
bad=work/'bad.context';bad.write_text(context.read_text().replace('rawtl-lens-calibration/1','rawtl-lens-calibration/9'))
command(['--stdout-frame-f32','--prepared',bad,'--lens-correction','auto'],26)
command(['--stdout-frame-f32','--prepared',context,'--exiftool',metadata],2)
assert hashlib.sha256(source.read_bytes()).hexdigest()==original_hash
print('PASS lens single/batch metadata, frozen LCP, SDK pixel effects, analysis isolation, EV and strict consumer failures')
