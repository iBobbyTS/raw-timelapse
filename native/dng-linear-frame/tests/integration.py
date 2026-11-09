"""Small generated fixtures: routing, controlled SDK semantics, independent color.
Python is test infrastructure only; the installed native tool does not use it.
"""
import hashlib,json,math,pathlib,struct,subprocess,sys,zlib,shlex
from decimal import Decimal as D
from fractions import Fraction
from reference_color import reference,matrix,mv,NEUTRAL,IDENTITY

exe,probe,work=map(pathlib.Path,sys.argv[1:])
exe,probe,work=exe.resolve(),probe.resolve(),work.resolve()
work.mkdir(parents=True,exist_ok=True)

def blob(typ,values):
    if typ in (1,7):return bytes(values),len(values)
    if typ==2:return values.encode()+b'\0',len(values)+1
    if typ in (3,4):return struct.pack('<'+('H' if typ==3 else 'I')*len(values),*values),len(values)
    if typ in (5,10):
        vals=[Fraction(str(x)).limit_denominator(1000000000) for x in values]
        return b''.join(struct.pack('<'+('ii' if typ==10 else 'II'),v.numerator,v.denominator) for v in vals),len(vals)
    raise AssertionError(typ)

# 16x16 is small enough for checked-in generator-only regression coverage.
def dng(name,*,pi=34892,black=None,white=1000,pixels=None,table=None,baseline=0,opcodes=None,ab=None,cc=None,neutral=None,w=16,h=16,datetime_original=None,exif_case=None):
    if pixels is None:pixels=[250,500,750]*(w*h)
    payload=struct.pack('<'+'H'*len(pixels),*pixels)
    tags={254:(4,[0]),256:(4,[w]),257:(4,[h]),258:(3,[16]*3),259:(3,[1]),271:(2,'Test'),272:(2,'LinearProbe'),273:(4,[0]),274:(3,[1]),277:(3,[3]),278:(4,[h]),279:(4,[len(payload)]),284:(3,[1]),339:(3,[1]*3),50706:(1,[1,4,0,0]),50707:(1,[1,3,0,0]),50708:(2,'Controlled LinearRaw Probe'),50713:(3,[2,2] if black else [1,1]),50714:(5,black if black else [0,0,0]),50717:(4,[white]*3),50718:(5,[1,1]),50719:(4,[0,0]),50720:(4,[w,h]),50721:(10,[1.843,-1.2288,.1003,-.5888,1.5271,.3387,-.0001,.0219,1.0795]),50728:(5,neutral or ['.6050808314',1,'.4825046041']),50730:(10,[baseline]),50778:(3,[17]),50829:(4,[0,0,h,w])}
    if pi is not None:tags[262]=(3,[pi])
    if table is not None:tags[50712]=(3,table)
    if ab is not None:tags[50727]=(5,ab)
    if cc is not None:tags[50723]=(10,cc)
    if datetime_original is not None and exif_case is None:tags[36867]=(2,datetime_original)
    if exif_case is not None:tags[34665]=(4,[0])  # patched after layout
    for tag,data in (opcodes or {}).items():tags[tag]=(7,data)
    extra=bytearray();entries=[];base=8+2+12*len(tags)+4
    for tag,(typ,v) in sorted(tags.items()):
        if tag==273:v=[base+sum((len(blob(t,x)[0])+1)//2*2 for k,(t,x) in sorted(tags.items()) if len(blob(t,x)[0])>4)]
        data,count=blob(typ,v)
        if len(data)<=4:field=data.ljust(4,b'\0')
        else:
            field=struct.pack('<I',base+len(extra));extra+=data
            if len(extra)%2:extra+=b'\0'
        entries.append(bytearray(struct.pack('<HHI',tag,typ,count)+field))
    tail=b''
    if exif_case is not None:
        value=(datetime_original or '1970:01:01 00:00:00').encode()+b'\0'
        after_payload=base+len(extra)+len(payload)
        if exif_case=='subifd':
            target=after_payload
            tail=struct.pack('<H',1)+struct.pack('<HHI',36867,2,len(value))+struct.pack('<I',target+18)+struct.pack('<I',0)+value
        elif exif_case=='oversized':
            # Legal 300-entry Exif IFD: must be scanned, not rejected.
            target=after_payload
            data_at=target+2+12*300+4
            entries_sub=[struct.pack('<HHI',65000+i,1,1)+b'\0\0\0\0' for i in range(299)]
            entries_sub.append(struct.pack('<HHI',36867,2,len(value))+struct.pack('<I',data_at))
            tail=struct.pack('<H',300)+b''.join(entries_sub)+struct.pack('<I',0)+value
        elif exif_case=='value-offset':
            # Valid directory whose ASCII value offset lies past EOF. The Adobe
            # SDK rejects a directory that is itself truncated or past EOF
            # (dng_error_bad_format), so this decodable out-of-range value is
            # the closest readable-metadata failure for the regression.
            target=after_payload
            tail=struct.pack('<H',1)+struct.pack('<HHI',36867,2,len(value))+struct.pack('<I',after_payload+1000)+struct.pack('<I',0)
        else:raise AssertionError(exif_case)
        index=sorted(tags).index(34665)
        entries[index][8:12]=struct.pack('<I',target)
    path=work/(name+'.dng');path.write_bytes(b'II*\0'+struct.pack('<I',8)+struct.pack('<H',len(tags))+b''.join(entries)+b'\0'*4+extra+payload+tail)
    return path

runs=[]
def run(args,code=0):
    r=subprocess.run([str(x) for x in args],capture_output=True)
    item={'command':[str(x) for x in args],'exit_code':r.returncode,'stdout':r.stdout.decode(errors='replace'),'stderr':r.stderr.decode(errors='replace')}
    runs.append(item);print(json.dumps(item),flush=True)
    (work/'commands.json').write_text(json.dumps(runs,indent=2))
    assert r.returncode==code,item
    return r

def decode(path):return json.loads(run([probe,path]).stdout)
def close(a,b,tol=2e-6):assert abs(float(a)-float(b))<=tol,(a,b,tol)
def camera_equal(result,expected,tol=2e-6):
    got=result['camera'];assert len(got)==len(expected)
    for a,b in zip(got,expected):close(a,b,tol)

def polynomial(factor):
    data=struct.pack('>8II2d',0,0,16,16,0,3,1,1,1,0.,factor)
    return struct.pack('>5I',1,8,0x01030000,0,len(data))+data

def tool(path,name,options=(),code=0):
    png=work/(name+'.png');sidecar=work/(name+'.json')
    run([exe,path,png,sidecar,*options],code)
    if code:return None
    meta=json.loads(sidecar.read_text())
    if meta['converter']['used']:assert not pathlib.Path(meta['converter']['temporary_directory']).exists()
    return meta

base=dng('base');base_probe=decode(base)
camera_equal(base_probe,[.25,.5,.75]*256)
# BlackLevel 2x2 with distinct nonzero values in all four repeat positions.
bl=[10,20,30,40,50,60,70,80,90,100,110,120]
black=decode(dng('black',black=bl))
expected=[]
# DNG SDK uses each plane's WhiteLevel-MaxBlack for scale, then local black.
for y in range(16):
    for x in range(16):
        for c,v in enumerate([250,500,750]):expected.append((v-bl[((y%2)*2+x%2)*3+c])/(1000-bl[9+c]))
camera_equal(black,expected)
white=decode(dng('white',white=2000));camera_equal(white,[.125,.25,.375]*256)
# Linearization table transforms stored codes BEFORE black subtraction/scaling.
table=decode(dng('lut',table=[min(2*i,65535) for i in range(1001)],white=2000))
camera_equal(table,[.25,.5,.75]*256)
combined=decode(dng('lut-black',table=[min(2*i,65535) for i in range(1001)],black=[100]*12,white=2000))
camera_equal(combined,[(500-100)/1900,(1000-100)/1900,(1500-100)/1900]*256)
plain_no_lut=decode(dng('lut-control',white=2000));assert table['camera']!=plain_no_lut['camera']
for stage,tag in enumerate((51008,51009,51022),1):
    r=decode(dng('opcode'+str(stage),opcodes={tag:polynomial(.5)}))
    # Stage1 converts back to stored uint16; exact inputs are all even here.
    camera_equal(r,[.125,.25,.375]*256,tol=2e-5)
    assert r['semantics']['opcodes'][stage-1]['count']==1
both=decode(dng('opcodes123',opcodes={tag:polynomial(.5) for tag in (51008,51009,51022)}))
camera_equal(both,[.03125,.0625,.09375]*256,tol=3e-5)
ev=decode(dng('ev19',baseline=1.9));camera_equal(ev,base_probe['camera'],tol=0)
assert not ev['semantics']['baseline_exposure']['applied']
clipped=decode(dng('decode-clipping',black=[100]*12,white=1000,pixels=[0,1000,2000]*256))
camera_equal(clipped,[0,1,1]*256)

# Compare actual SDK-composed no-FM matrix + native CAT/AP1 against independent
# Decimal Ch.6 reference, including the real non-symmetric DJI ColorMatrix1.
for name,fixture,ab,cc in [
    ('real-matrix',base,IDENTITY,IDENTITY),
    ('calibrated',dng('abcc',ab=[1.1,1,1.2],cc=[1,.01,0,0,1,.02,.01,0,1]),matrix([[1.1,0,0],[0,1,0],[0,0,1.2]]),matrix([[1,.01,0],[0,1,.02],[.01,0,1]]))]:
    result=decode(fixture)
    d50,ap1=reference(ab=ab,cc=cc)
    for key,ref in [('camera_to_xyz_d50',d50),('camera_to_acescg',ap1)]:
        actual=result['color'][key]
        for v in [NEUTRAL,matrix([[.2,.4,.8]])[0],matrix([[1,0,0]])[0],matrix([[-.2,1.5,4.5]])[0]]:
            want=mv(ref,v)
            got=[sum(float(x)*float(y) for x,y in zip(row,v)) for row in actual]
            for a,b in zip(got,want):close(a,b,3e-6)
    scene=mv(ap1,matrix([[-.2,1.5,4.5]])[0]);assert min(scene)<0 and max(scene)>1
    print('independent color reference passed',name,flush=True)

meta=tool(base,'direct',options=['--converter','/does/not/exist']);assert meta['route']=='linearraw-sdk'
assert meta['white_balance_source']=='as-shot'
# Verify the actual RGB16 PNG payload, independently of libpng (no OETF,
# correct byte order, code scale and channel order).
def png_samples(path):
    data=path.read_bytes();assert data[:8]==b'\x89PNG\r\n\x1a\n';at=8;compressed=b'';chunks=[]
    while at<len(data):
        size=struct.unpack('>I',data[at:at+4])[0];kind=data[at+4:at+8];body=data[at+8:at+8+size];chunks.append(kind)
        if kind==b'IHDR':assert struct.unpack('>IIBBBBB',body)==(16,16,16,2,0,0,0)
        if kind==b'IDAT':compressed+=body
        at+=12+size
    assert b'gAMA' not in chunks and b'sRGB' not in chunks
    raw=zlib.decompress(compressed);stride=1+16*6
    assert len(raw)==16*stride
    for y in range(16):assert raw[y*stride]==0
    return struct.unpack('>3H',raw[1:7])
expected=[]
for row in meta['color']['camera_to_acescg']:
    value=struct.unpack('<f',struct.pack('<f',sum(x*y for x,y in zip(row,[.25,.5,.75]))))[0]
    expected.append(max(0,min(65535,int(value*16384+.5))))
assert png_samples(work/'direct.png')==tuple(expected)

blue=tool(dng('blue-range',pixels=[0,0,1000]*256),'blue-range')
assert blue['observed_scene_min']<0 and blue['observed_scene_max']>1
assert blue['quantization']['negative_samples']==512
assert png_samples(work/'blue-range.png')[:2]==(0,0)
a=tool(base,'repeat');assert (work/'direct.png').read_bytes()==(work/'repeat.png').read_bytes()
ev_meta=tool(work/'ev19.dng','ev19');assert (work/'direct.png').read_bytes()==(work/'ev19.png').read_bytes()
override=tool(base,'override',options=['--white-balance','0.7,1,0.6'])
assert override['white_balance_source']=='override' and override['applied_white_balance']==[.7,1,.6]
assert override['as_shot_neutral']==meta['as_shot_neutral']
d50,ap1=reference(neutral=matrix([[.7,1,.6]])[0])
for row,refrow in zip(override['color']['camera_to_acescg'],ap1):
    for x,y in zip(row,refrow):close(x,y,3e-6)

# S01: analysis sidecar, exposure gain, capture_time and CLI contract.
def analysis_run(fixture,name,options=(),code=0):
    png=work/(name+'.png');meta=work/(name+'.json');side=work/(name+'.analysis.json')
    run([exe,fixture,png,meta,'--emit-analysis',side,*options],code)
    if code:return None,None,None
    return json.loads(meta.read_text()),json.loads(side.read_text()),side

def codes_for(meta,camera,gain):
    out=[]
    for row in meta['color']['camera_to_acescg']:
        value=sum(float(x)*float(y) for x,y in zip(row,camera))
        x=struct.unpack('<f',struct.pack('<f',value*gain))[0]
        out.append(0 if x<=0 else 65535 if x>=65535/16384 else int(x*16384+.5))
    return out

uniform=dng('analysis-uniform',w=64,h=64,pixels=[500]*3*64*64,datetime_original='2026:11:08 20:09:53')
meta0,side0,path0=analysis_run(uniform,'analysis-uniform',('--no-png',))
assert not (work/'analysis-uniform.png').exists()
assert meta0['exposure_ev']==0 and meta0['gain']==1
assert meta0['capture_time']=='2026:11:08 20:09:53'
assert meta0['analysis_sidecar']==str(path0)
assert side0['schema_version']==1 and side0['kind']=='rawtl-analysis-sidecar'
assert side0['source_sha256']==meta0['source_sha256']
geometry=side0['analysis']
assert (geometry['downsampled_width'],geometry['downsampled_height'])==(64,64)
assert (geometry['grid_nx'],geometry['grid_ny'])==(4,4)
assert geometry['roi']==[0,0,1,1] and geometry['roi_pixel_bounds']==[0,0,64,64]
assert geometry['saturation_mask_quality']=='camera_domain'
assert geometry['output_clip_code_threshold']==16057
assert geometry['structure_stride']==4
assert geometry['luma_weights']==[.2126,.7152,.0722]
assert len(side0['tiles'])==16
_,ap1=reference()
scene=[float(v) for v in mv(ap1,[D('0.5')]*3)]
luma=sum(w*v for w,v in zip((.2126,.7152,.0722),scene))
expected_clip=1.0 if any(c>=16057 for c in codes_for(meta0,[.5,.5,.5],1.0)) else 0.0
for tile in side0['tiles']:
    assert tile['valid'] is True
    close(tile['log_luminance'],math.log2(luma),1e-6)
    assert tile['saturation_fraction']==0
    assert tile['output_clip_fraction']==expected_clip
    for got,want in zip(tile['channel_log_means'],scene):close(got,math.log2(want),1e-6)
    assert len(tile['structure_samples'])==16
    for value in tile['structure_samples']:close(value,math.log2(luma),1e-6)
assert [t['center'] for t in side0['tiles']][:2]==[[.125,.125],[.375,.125]]
assert meta0['observed_scene_min']<0.4 and meta0['observed_scene_max']>1
assert meta['capture_time'] is None  # legacy fixtures carry no DateTimeOriginal

# DateTimeOriginal through the Exif sub-IFD pointer (tag 34665), not IFD0.
exif_uniform=dng('analysis-exif-subifd',w=64,h=64,pixels=[500]*3*64*64,datetime_original='2001:02:03 04:05:06',exif_case='subifd')
exif_meta,_,_=analysis_run(exif_uniform,'analysis-exif-subifd',('--no-png',))
assert exif_meta['capture_time']=='2001:02:03 04:05:06'
# A legal, readable 300-entry Exif directory must yield its DateTimeOriginal.
large=dng('analysis-exif-300',w=64,h=64,pixels=[500]*3*64*64,datetime_original='2003:04:05 06:07:08',exif_case='oversized')
large_meta,_,_=analysis_run(large,'analysis-exif-300',('--no-png',))
assert large_meta['capture_time']=='2003:04:05 06:07:08'
# Out-of-range metadata: the Exif directory is valid but the ASCII value offset
# is past EOF, so the timestamp is unreadable while the frame still decodes.
out_of_range=dng('analysis-exif-out-of-range',w=64,h=64,pixels=[500]*3*64*64,datetime_original='2004:05:06 07:08:09',exif_case='value-offset')
out_of_range_meta,_,_=analysis_run(out_of_range,'analysis-exif-out-of-range',('--no-png',))
assert out_of_range_meta['capture_time'] is None
assert out_of_range_meta['width']==64 and out_of_range_meta['height']==64

# Rerun determinism: identical sidecar bytes; JSON semantics excluding the
# requested sidecar path are equal.
meta1,side1,path1=analysis_run(uniform,'analysis-uniform-repeat',('--no-png',))
assert path1.read_bytes()==path0.read_bytes()
strip=lambda m:{k:v for k,v in m.items() if k!='analysis_sidecar'}
assert strip(meta1)==strip(meta0)

# EV equivariance and the two-series separation: camera mask is EV invariant,
# delivered-domain clipping grows with +EV on near-white content.
near=dng('analysis-ev',w=64,h=64,pixels=[400]*3*64*64)
m0,s0,_=analysis_run(near,'analysis-ev0',('--no-png',))
m1,s1,_=analysis_run(near,'analysis-ev-plus',('--no-png','--exposure-ev','0.5'))
m2,s2,_=analysis_run(near,'analysis-ev-minus',('--no-png','--exposure-ev','-0.5'))
assert m1['exposure_ev']==0.5 and abs(m1['gain']-2**.5)<1e-12
for a,b,c in zip(s0['tiles'],s1['tiles'],s2['tiles']):
    assert a['valid'] and b['valid'] and c['valid']
    close(b['log_luminance']-a['log_luminance'],0.5,1e-4)
    close(c['log_luminance']-a['log_luminance'],-0.5,1e-4)
    assert a['saturation_fraction']==b['saturation_fraction']==c['saturation_fraction']==0
    assert a['output_clip_fraction']==0 and b['output_clip_fraction']==1
    assert c['output_clip_fraction']==0
for key in ('observed_scene_min','observed_scene_max'):
    close(m1[key]/m0[key],2**.5,1e-6)

# Camera-domain mask on the SDK-normalized domain, including a nonzero black
# level, and a color transform that produces negative scene channels.
def pixel_fixture(name,value):
    return dng(name,w=64,h=64,black=[100]*12,white=2000,pixels=[value]*3*64*64)
black_meta,black_side,_=analysis_run(pixel_fixture('analysis-black',1981),'analysis-black',('--no-png',))
black_control,_,_=analysis_run(pixel_fixture('analysis-black-control',1050),'analysis-black-control',('--no-png',))
for tile in black_side['tiles']:
    assert tile['valid'] is False and tile['log_luminance'] is None
    assert tile['saturation_fraction']==1
close(black_control['observed_scene_min'],min(scene),1e-6)  # 0.5 camera after black
blue_meta,blue_side,_=analysis_run(dng('analysis-blue',w=64,h=64,white=2000,pixels=[0,0,2000]*64*64),'analysis-blue',('--no-png',))
assert blue_meta['observed_scene_min']<0  # color transform leaves the camera domain
for tile in blue_side['tiles']:
    assert tile['saturation_fraction']==1 and tile['valid'] is False

# CLI contract: non-finite EV, malformed/oversized ROI, ROI_TOO_SMALL.
bad_json=work/'contract.json'
run([exe,base,work/'contract.png',bad_json,'--exposure-ev','nan'],3)
run([exe,base,work/'contract.png',bad_json,'--exposure-ev','inf'],3)
run([exe,base,work/'contract.png',bad_json,'--exposure-ev','1e999'],3)
run([exe,uniform,work/'roi.png',work/'roi.json','--emit-analysis',work/'roi.analysis.json','--no-png','--analysis-roi','0,0,2,1'],3)
run([exe,uniform,work/'roi.png',work/'roi.json','--emit-analysis',work/'roi.analysis.json','--no-png','--analysis-roi','0,0,0.5'],3)
run([exe,uniform,work/'roi.png',work/'roi.json','--analysis-roi','0,0,1,1'],2)  # requires --emit-analysis
run([exe,base,work/'small.png',work/'small.json','--emit-analysis',work/'small.analysis.json','--no-png'],4)

# Generated subprocess doubles are scoped routing tests, never decode providers.
marker=work/'converter-called'
def fake(name,body):
    p=work/name;p.write_text('#!/bin/sh\n'+body+'\n');p.chmod(0o755);return p
conv=fake('converter-copy',f'touch {shlex.quote(str(marker))}\ncp {shlex.quote(str(base))} "$4/converted.dng"')
cfa=dng('cfa',pi=32803)
run([probe,cfa],23)
meta=tool(cfa,'converted',options=['--converter',str(conv)]);assert marker.exists() and meta['route']=='cfa-converter-linearraw-sdk'
marker.unlink()
for name,fixture,code in [('missing',dng('missing-pi',pi=None),21),('unsupported',dng('unsupported-pi',pi=2),22)]:
    tool(fixture,name,options=['--converter',str(conv)],code=code);assert not marker.exists()
bad=fake('converter-bad',f'cp {shlex.quote(str(cfa))} "$4/not-linear.dng"')
tool(cfa,'bad-conversion',options=['--converter',str(bad)],code=23)
failure=fake('converter-failure','echo deliberate-failure >&2\nexit 7')
tool(cfa,'failed-conversion',options=['--converter',str(failure)],code=24)
timeout=fake('converter-timeout','exec /bin/sleep 10')
tool(cfa,'timeout',options=['--converter',str(timeout),'--converter-timeout','1'],code=25)
tool(base,'bad-wb',options=['--white-balance','1,-1,1'],code=2)
# Big-endian metadata route + preview trap: build minimal TIFF directory graph.
def directory_fixture(name,main_pi,preview_pi=34892):
    def directory(tags,next_at=0):
        return struct.pack('>H',len(tags))+b''.join(struct.pack('>HHI',tag,typ,1)+ (struct.pack('>H',v)+b'\0\0' if typ==3 else struct.pack('>I',v)) for tag,typ,v in tags)+struct.pack('>I',next_at)
    preview=[(254,4,1),(256,4,8),(257,4,8),(262,3,preview_pi),(330,4,8+2+6*12+4),(50706,4,0x01040000)]
    # Change DNGVersion entry to type BYTE,count=4, keeping its inline bytes.
    first=bytearray(directory(preview));first[2+5*12+2:2+5*12+8]=struct.pack('>HI',1,4)
    main=[(254,4,0),(256,4,16),(257,4,16)]+([] if main_pi is None else [(262,3,main_pi)])
    p=work/(name+'.dng');p.write_bytes(b'MM\0*'+struct.pack('>I',8)+first+directory(main));return p
# Main unsupported/missing must reject before AsShotNeutral/SDK, despite LinearRaw preview.
tool(directory_fixture('preview-trap',2),'preview-trap',code=22)
tool(directory_fixture('preview-missing',None),'preview-missing',code=21)
print('PASS: controlled routing, SDK attribution, deterministic PNG, WB and independent color reference')
