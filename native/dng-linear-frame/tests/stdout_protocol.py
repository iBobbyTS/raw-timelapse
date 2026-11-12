"""Streaming stdout protocol v1 conformance for the native tool.

Checks record framing, the two stdout modes, byte-level equivalence with the
positional-mode metadata/analysis files, mode/option rejection and closed-pipe
failure. Python is test infrastructure only.
"""
import hashlib,json,os,pathlib,shlex,struct,subprocess,sys
from dng_fixture import make_dng

exe,work=map(pathlib.Path,sys.argv[1:])
exe,work=exe.resolve(),work.resolve()
work.mkdir(parents=True,exist_ok=True)
MAGIC=b'RAWTLST1'
RUNS=[]

def log(item):
    RUNS.append(item)
    (work/'commands.json').write_text(json.dumps(RUNS,indent=2))
    print(json.dumps(item),flush=True)

def run(args,code=0):
    r=subprocess.run([str(x) for x in args],capture_output=True)
    log({'command':[str(x) for x in args],'exit_code':r.returncode,
         'stdout':r.stdout.decode(errors='replace'),'stderr':r.stderr.decode(errors='replace')})
    assert r.returncode==code,(r.returncode,code,r.stderr)
    return r

def stream(args,code=0):
    """Stdout-mode run: log a payload digest instead of binary stdout."""
    r=subprocess.run([str(x) for x in args],capture_output=True)
    log({'command':[str(x) for x in args],'exit_code':r.returncode,
         'stdout_bytes':len(r.stdout),'stdout_sha256':hashlib.sha256(r.stdout).hexdigest(),
         'stderr':r.stderr.decode(errors='replace')})
    assert r.returncode==code,(r.returncode,code,r.stderr)
    return r

def records(data):
    """Parse protocol v1 framing; asserts magic, zero reserved bytes, lengths."""
    assert data[:8]==MAGIC,data[:8]
    at,out=8,[]
    while at<len(data):
        assert at+12<=len(data),at
        typ=data[at]
        assert data[at+1:at+4]==b'\0\0\0',(at,data[at+1:at+4])
        length=struct.unpack('<Q',data[at+4:at+12])[0]
        at+=12
        assert at+length<=len(data),(at,length,len(data))
        out.append((typ,data[at:at+length]));at+=length
    return out

def positional_meta(fixture,name,options=()):
    path=work/(name+'.json')
    run([exe,fixture,work/(name+'.png'),path,'--no-png',*options])
    return path

def positional_sidecar(fixture,name,options=()):
    side=work/(name+'.analysis.json')
    run([exe,fixture,work/(name+'.png'),work/(name+'.json'),'--emit-analysis',side,'--no-png',*options])
    return side

def codes_for(meta,camera,gain):
    """Independent reimplementation of the documented quantization policy."""
    out=[]
    for row in meta['color']['camera_to_acescg']:
        value=sum(float(x)*float(y) for x,y in zip(row,camera))
        x=struct.unpack('<f',struct.pack('<f',value*gain))[0]
        out.append(0 if x<=0 else 65535 if x>=65535/16384 else int(x*16384+.5))
    return out

base=make_dng(work,'base')
uniform=make_dng(work,'uniform',w=64,h=64,pixels=[500]*3*64*64)
half=make_dng(work,'half',w=128,h=128,pixels=[500]*3*128*128)

# --- AC1/AC2: analysis mode framing and byte equivalence ---------------------
reference_meta=positional_meta(uniform,'analysis-reference-metadata')
reference_side=positional_sidecar(uniform,'analysis-reference-sidecar')
r=stream([exe,'--stdout-analysis',uniform])
parsed=records(r.stdout)
assert [t for t,_ in parsed]==[0x01,0x02],parsed
assert parsed[0][1]==reference_meta.read_bytes()
assert parsed[0][1].endswith(b'\n')
assert parsed[1][1]==reference_side.read_bytes()
assert parsed[1][1].endswith(b'\n')
metadata=json.loads(parsed[0][1])
analysis=json.loads(parsed[1][1])
assert metadata['analysis_sidecar'] is None
assert metadata['source']==str(uniform)
assert analysis['schema_version']==1 and analysis['kind']=='rawtl-analysis-sidecar'
assert analysis['source_sha256']==metadata['source_sha256']
assert b'decoded ' not in r.stdout and b'decoded ' in r.stderr

# Applied EV is recorded in the 0x02 analysis payload and matches positional
# sidecar bytes for the same parameters.
reference_meta_ev=positional_meta(uniform,'analysis-reference-metadata-ev',('--exposure-ev','0.5'))
reference_side_ev=positional_sidecar(uniform,'analysis-reference-sidecar-ev',('--exposure-ev','0.5'))
r=stream([exe,'--stdout-analysis',uniform,'--exposure-ev','0.5'])
parsed=records(r.stdout)
assert [t for t,_ in parsed]==[0x01,0x02]
assert parsed[0][1]==reference_meta_ev.read_bytes()
assert parsed[1][1]==reference_side_ev.read_bytes()
assert json.loads(parsed[1][1])['analysis']['exposure_ev']==0.5

# --analysis-roi composes with the analysis mode and stays byte-identical.
# A half-frame ROI needs at least 8 tiles: use the 128x128 fixture.
roi_side=positional_sidecar(half,'analysis-reference-sidecar-roi',('--analysis-roi','0.5,0.5,0.5,0.5'))
r=stream([exe,'--stdout-analysis',half,'--analysis-roi','0.5,0.5,0.5,0.5'])
parsed=records(r.stdout)
assert parsed[1][1]==roi_side.read_bytes()
assert json.loads(parsed[1][1])['analysis']['roi']==[0.5,0.5,0.5,0.5]

# --- AC1/AC2: frame mode framing, descriptor and pixel payload --------------
reference_frame_meta=positional_meta(base,'frame-reference-metadata')
r=stream([exe,'--stdout-frame',base])
parsed=records(r.stdout)
assert [t for t,_ in parsed]==[0x01,0x03,0x04],parsed
assert parsed[0][1]==reference_frame_meta.read_bytes()
metadata=json.loads(parsed[0][1])
assert metadata['analysis_sidecar'] is None
descriptor=json.loads(parsed[1][1])
assert list(descriptor)==['width','height','pixel_format','pixels_sha256']
assert (descriptor['width'],descriptor['height'],descriptor['pixel_format'])==(16,16,'rgb48le')
assert len(parsed[2][1])==16*16*6
assert descriptor['pixels_sha256']==hashlib.sha256(parsed[2][1]).hexdigest()
expected=codes_for(metadata,[.25,.5,.75],1.0)
assert parsed[2][1]==struct.pack('<'+'H'*(16*16*3),*(expected*256))
assert b'decoded ' not in r.stdout and b'decoded ' in r.stderr

# White balance and exposure compose with the frame mode; the pixel payload is
# still the documented quantized interleaved RGB48LE buffer.
options=('--white-balance','0.7,1,0.6','--exposure-ev','0.25')
reference_frame_wb=positional_meta(base,'frame-reference-wb',options)
r=stream([exe,'--stdout-frame',base,*options])
parsed=records(r.stdout)
assert [t for t,_ in parsed]==[0x01,0x03,0x04]
assert parsed[0][1]==reference_frame_wb.read_bytes()
metadata=json.loads(parsed[0][1])
assert metadata['white_balance_source']=='override' and metadata['applied_white_balance']==[.7,1,.6]
descriptor=json.loads(parsed[1][1])
assert descriptor['pixels_sha256']==hashlib.sha256(parsed[2][1]).hexdigest()
expected=codes_for(metadata,[.25,.5,.75],2**.25)
assert parsed[2][1]==struct.pack('<'+'H'*(16*16*3),*(expected*256))

# --- Converter-backed inputs keep their temporary-directory handling --------
cfa=make_dng(work,'cfa',pi=32803)
converter=work/'converter-copy'
converter.write_text('#!/bin/sh\ncp '+shlex.quote(str(base))+' "$4/converted.dng"\n')
converter.chmod(0o755)
r=stream([exe,'--stdout-frame',cfa,'--converter',converter])
parsed=records(r.stdout)
assert [t for t,_ in parsed]==[0x01,0x03,0x04]
metadata=json.loads(parsed[0][1])
assert metadata['route']=='cfa-converter-linearraw-sdk'
assert metadata['converter']['used'] is True
assert metadata['analysis_sidecar'] is None
assert not pathlib.Path(metadata['converter']['temporary_directory']).exists()
descriptor=json.loads(parsed[1][1])
assert descriptor['pixels_sha256']==hashlib.sha256(parsed[2][1]).hexdigest()
expected=codes_for(metadata,[.25,.5,.75],1.0)
assert parsed[2][1]==struct.pack('<'+'H'*(16*16*3),*(expected*256))
# Converter timeout propagates unchanged and publishes no partial stream.
slow=work/'converter-slow'
slow.write_text('#!/bin/sh\nexec /bin/sleep 5\n')
slow.chmod(0o755)
r=stream([exe,'--stdout-frame',cfa,'--converter',slow,'--converter-timeout','1'],25)
assert r.stdout==b'',r.stdout

# --- Zero on-disk artifacts and mode/option rejection -----------------------
clean=work/'no-artifacts';clean.mkdir(exist_ok=True)
for mode,fixture in [(('--stdout-frame',),base),(('--stdout-analysis',),uniform)]:
    probe=subprocess.run([str(exe),*mode,str(fixture)],cwd=clean,capture_output=True)
    assert probe.returncode==0,probe.stderr
    assert list(clean.iterdir())==[],list(clean.iterdir())

stream([exe,'--stdout-analysis',uniform,'--stdout-frame',base],2)
stream([exe,'--stdout-frame',base,'--stdout-analysis',uniform],2)
stream([exe,'--stdout-frame',base,'--stdout-frame',base],2)
stream([exe,'--stdout-analysis'],2)
stream([exe,'--stdout-analysis',uniform,'--analysis-roi','0,0,1,1','--stdout-frame',base],2)
stream([exe,'--stdout-frame',base,'--analysis-roi','0,0,1,1'],2)
stream([exe,'--stdout-frame',base,'--emit-analysis',work/'x.json'],2)
stream([exe,'--stdout-frame',base,'--no-png'],2)
stream([exe,'--stdout-frame',base,'--bogus','value'],2)
# The positional hard rule is unchanged: ROI without an analysis sink.
run([exe,base,work/'roi-only.png',work/'roi-only.json','--analysis-roi','0,0,1,1'],2)

# --- Positional grammar: path slots are paths, even when spelled like flags --
# The stdout grammar is flag-first (argv[1]), so a bare relative output filename
# that happens to equal --stdout-frame/--stdout-analysis must still be accepted
# by the three-positional invocation.
for flag in ('--stdout-frame','--stdout-analysis'):
    out_json='positional-'+flag[2:]+'.json'
    r=subprocess.run([str(exe),str(base),flag,out_json],cwd=clean,capture_output=True)
    log({'command':['positional-flag-named-path',str(base),flag,out_json],
         'exit_code':r.returncode,'stdout':r.stdout.decode(errors='replace'),
         'stderr':r.stderr.decode(errors='replace')})
    assert r.returncode==0,(flag,r.returncode,r.stderr)
    png=clean/flag
    assert png.read_bytes()[:8]==b'\x89PNG\r\n\x1a\n',flag
    meta=json.loads((clean/out_json).read_bytes())
    assert (meta['width'],meta['height'])==(16,16)
    png.unlink();(clean/out_json).unlink()

# --- Closed consumer pipe: nonzero exit, no success claim -------------------
read_end,write_end=os.pipe()
os.close(read_end)
pipe=subprocess.Popen([str(exe),'--stdout-frame',str(base)],stdout=write_end,stderr=subprocess.PIPE)
os.close(write_end)
stderr=pipe.stderr.read()
status=pipe.wait()
log({'command':['closed-pipe','--stdout-frame','base'],'exit_code':status,
     'stderr':stderr.decode(errors='replace')})
assert status!=0,status
assert status>0,('died by signal',status)
assert b'stdout write failed' in stderr,stderr

print('PASS: stdout protocol v1 framing, equivalence, options and failure modes',flush=True)

# v2 signed FP32 is captured before legacy quantization; both profile methods
# are checked against an independent matrix product, including EV exactly once.
for fm in (None,[.7,.1,.1643,.2,.7,.1,.01,.1,.7151]):
    label='no-fm' if fm is None else 'with-fm'
    f=make_dng(work,'float-'+label,forward=fm,model='Canon EOS Test',w=64,h=64,pixels=[0,1000,0]*4096)
    r=stream([exe,'--stdout-frame-f32',f,'--camera-color-method','dng-camera-to-pcs','--exposure-ev','5'])
    assert r.stdout[:8]==b'RAWTLST2'
    parsed=records(b'RAWTLST1'+r.stdout[8:])
    assert [t for t,_ in parsed]==[1,3,4]
    meta,desc=json.loads(parsed[0][1]),json.loads(parsed[1][1])
    assert meta['color']['method_requested']=='dng-camera-to-pcs'
    assert meta['color']['forward_matrix_used']==(fm is not None)
    assert meta['color']['method_effective']==('dng-camera-to-pcs-forward-matrix' if fm else 'dng-camera-to-pcs-inverse-fallback')
    assert meta['color']['camera_model']=='Canon EOS Test'
    assert meta['color']['profile_fingerprint']
    assert meta['quantization']['negative_policy']=='preserve'
    assert meta['quantization']['upper_policy']=='preserve'
    assert (desc['pixel_format'],desc['byte_length'],desc['space'],desc['transfer'])==('rgbf32le',64*64*12,'AP1/D60','linear')
    assert desc['pixels_sha256']==hashlib.sha256(parsed[2][1]).hexdigest()
    expected=[row[1]*32 for row in meta['color']['camera_to_acescg']]
    expected_bytes=struct.pack('<3f',*expected)*4096
    assert parsed[2][1]==expected_bytes
    assert max(expected)>4
    if fm is None: assert min(expected)<0  # a value RGB48 would irreversibly clip
    # Analysis uses the same selected matrix, keeping its lightweight v1 records.
    a=stream([exe,'--stdout-analysis',f,'--camera-color-method','dng-camera-to-pcs'])
    am=json.loads(records(a.stdout)[0][1])
    assert am['color']==meta['color']

stream([exe,'--stdout-frame-f32',base,'--camera-color-method','bogus'],2)
print('Signed FP32 / CameraToPCS conformance passed',flush=True)

# A present but singular FM is an error, never a silent inverse-CM fallback.
f=make_dng(work,'singular-fm',forward=[1,1,1]*3)
r=subprocess.run([str(exe),'--stdout-frame-f32',str(f),'--camera-color-method','dng-camera-to-pcs'],capture_output=True)
assert r.returncode!=0 and r.stdout==b'',(r.returncode,r.stderr)
