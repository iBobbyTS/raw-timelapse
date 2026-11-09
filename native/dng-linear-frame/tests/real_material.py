"""Opt-in real-frame determinism/WB check; never writes in the material directory.
Usage: python3 real_material.py <native-executable> <material-directory> <work-dir>
"""
import hashlib,json,pathlib,subprocess,sys
exe,material,work=(pathlib.Path(p).resolve() for p in sys.argv[1:])
assert work!=material and material not in work.parents
logs=work/'logs';logs.mkdir(parents=True,exist_ok=True)
source=sorted(material.glob('*.DNG'));assert len(source)==39
selected=[source[0],source[len(source)//2],source[-1]]
results=[]
def digest(p):return hashlib.sha256(p.read_bytes()).hexdigest()
def run(path,name,options=()):
    png=work/(name+'.png');sidecar=work/(name+'.json')
    command=[str(exe),str(path),str(png),str(sidecar),*options]
    before=digest(path)
    with (logs/(name+'.stdout')).open('wb') as out,(logs/(name+'.stderr')).open('wb') as err:
        r=subprocess.run(command,stdout=out,stderr=err)
    record={'command':command,'exit_code':r.returncode}
    (logs/(name+'.process.json')).write_text(json.dumps(record,indent=2))
    assert r.returncode==0,record
    meta=json.loads(sidecar.read_text());assert meta['source_sha256']==before==digest(path)
    assert meta['source_photometric']==32803 and meta['route']=='cfa-converter-linearraw-sdk'
    assert meta['converter']['verified_main_ifd_photometric']==34892
    assert not pathlib.Path(meta['converter']['temporary_directory']).exists()
    assert meta['width']==4000 and meta['height']==2256
    assert isinstance(meta['decode_semantics']['baseline_exposure']['ev'],(int,float))
    assert not meta['decode_semantics']['baseline_exposure']['applied']
    record.update(png_sha256=digest(png),metadata=str(sidecar),min=meta['observed_scene_min'],max=meta['observed_scene_max'],baseline_ev=meta['decode_semantics']['baseline_exposure']['ev'])
    results.append(record);print(json.dumps(record),flush=True)
    return meta,record
frozen=None
for path in selected:
    first,one=run(path,path.stem+'-run1')
    second,two=run(path,path.stem+'-run2')
    assert one['png_sha256']==two['png_sha256']
    if frozen is None:frozen=first['applied_white_balance']
    assert first['applied_white_balance']==first['as_shot_neutral'] and first['white_balance_source']=='as-shot'
last,record=run(selected[-1],'last-frozen',options=['--white-balance',','.join(map(str,frozen))])
assert last['applied_white_balance']==frozen and last['as_shot_neutral']!=frozen and last['white_balance_source']=='override'
(work/'summary.json').write_text(json.dumps({'results':results,'frozen_white_balance':frozen,'status':'PASS'},indent=2))
print('PASS: first/middle/last byte-identical PNG reruns; last frame frozen-WB propagation')
