"""Same-camera real graph equality at identities/measurement/EV/pre-encode pixels.
The native wrapper records analysis and FP32 payload hashes for test comparison.
The fixture records the frame identities and checks cleanup on the default graph.
"""
import hashlib,json,os,pathlib,shlex,struct,subprocess,sys,textwrap,time
cli,native,work,incoming=[pathlib.Path(p).resolve() for p in sys.argv[1:]]
repo=pathlib.Path(__file__).resolve().parents[2]

def tokens(line):return dict(word.split('=',1) for word in shlex.split(line) if '=' in word)
def selected(trace,kind):return [tokens(line) for line in trace.splitlines() if f' kind={kind} ' in line]

def run(profile,mode):
    root=work/(profile+'-'+mode);root.mkdir()
    graph=json.loads((repo/'pipeline/src/graph/default.json').read_text())
    graph['nodes'][2]['concurrency']=1 if mode=='serial' else 8
    (root/'graph.json').write_text(json.dumps(graph))
    analysis=root/'analysis';analysis.mkdir()
    wrapper=root/'native-adapter'
    wrapper.write_text('#!/usr/bin/python3\n'+textwrap.dedent(f'''
        import hashlib,pathlib,struct,subprocess,sys,os
        from urllib.parse import unquote
        args=sys.argv[1:]
        if args[0] not in ('--stdout-analysis','--stdout-frame-f32'):os.execv({str(native)!r},[{str(native)!r},*args])
        wire=sys.stdin.buffer.read()
        p=subprocess.run([{str(native)!r},*args],input=wire,capture_output=True)
        context={{k:unquote(v) for k,v in (line.split('=',1) for line in wire.decode().splitlines()[1:])}}
        name=pathlib.Path(context['source']).name
        data=p.stdout;offset=8;records=[]
        while offset+12<=len(data):
            kind,length=struct.unpack('<IQ',data[offset:offset+12]);offset+=12
            records.append((kind,data[offset:offset+length]));offset+=length
        for kind,payload in records:
            if args[0]=='--stdout-analysis' and kind in [1,2]:
                pathlib.Path({str(analysis)!r},name+'.'+str(kind)+'.json').write_bytes(payload)
            if args[0]=='--stdout-frame-f32' and kind==1:
                keys=['frame_id','source','route','generated','main_ifd_width','main_ifd_height','as_shot_neutral','has_capture_time','capture_time']
                pathlib.Path({str(analysis)!r},name+'.context.json').write_text(__import__('json').dumps({{k:context.get(k) for k in keys}}))
            if kind==4:
                pathlib.Path({str(analysis)!r},name+'.pixels.sha256').write_text(hashlib.sha256(payload).hexdigest())
        sys.stdout.buffer.write(p.stdout);sys.stdout.buffer.flush();sys.stderr.buffer.write(p.stderr);sys.exit(p.returncode)
    '''));wrapper.chmod(0o755)
    video=root/'output/timelapse.mov'
    args=[str(cli),'--input',str(incoming),'--output',str(video),'--graph',str(root/'graph.json'),'--dng-linear-frame',str(wrapper),'--preview-profile',profile,'--resize','696x464','--verify-output']
    started=time.monotonic()
    with (root/'stdout').open('wb') as out,(root/'stderr').open('wb') as err:p=subprocess.run(args,stdout=out,stderr=err,timeout=600)
    (root/'exit.json').write_text(json.dumps({'command':args,'exit':p.returncode,'wall':time.monotonic()-started},indent=2))
    assert p.returncode==0,(args,p.returncode,(root/'stderr').read_text())
    trace=video.with_suffix('.log').read_text();scratch=pathlib.Path(selected(trace,'run.start')[0]['scratch'])
    assert not scratch.exists()
    originals=[json.loads(p.read_text()) for p in sorted(analysis.glob('*.context.json'))]
    assert len(originals)==len(list(incoming.iterdir()))
    develops=[{k:d[k] for k in ['frame','source','ev','wb','wb_source','as_shot','pixels']} for d in selected(trace,'develop')]
    measurements={p.name:json.loads(p.read_text()) for p in analysis.glob('*.2.json')}
    assert len(measurements)==len(originals),list(analysis.iterdir())
    # Compare actual analysis and capture-time/WB metadata, not MOV byte hashes.
    metadata={p.name:{k:v for k,v in json.loads(p.read_text()).items() if k in ['frame_id','source','capture_time','as_shot_neutral','applied_white_balance']} for p in analysis.glob('*.1.json')}
    if incoming.suffix!='.dng':
        models={json.loads(p.read_text()).get('color',{}).get('camera_model') for p in analysis.glob('*.1.json')}
        assert len(models)==1,models
    pixels={p.name:p.read_text() for p in analysis.glob('*.pixels.sha256')}
    assert len(pixels)==len(originals)
    result={'originals':originals,'develops':develops,'measurements':measurements,'metadata':metadata,'pixels':pixels}
    (root/'oracle.json').write_text(json.dumps(result,indent=2,sort_keys=True))
    return result

receipts=[]
profiles=['srgb-preview','clog2'] if list(incoming.glob('*.CR3')) else ['srgb-preview']
for profile in profiles:
    serial=run(profile,'serial');parallel=run(profile,'parallel')
    assert serial==parallel,profile
    receipt={'profile':profile,'frames':len(serial['develops']),'identity_wb_capture_time_measurement_ev_pixels_equal':True}
    receipts.append(receipt);print(json.dumps(receipt),flush=True)
(work/'equivalence.json').write_text(json.dumps(receipts,indent=2))
