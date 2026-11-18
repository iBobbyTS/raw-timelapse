"""Whole-run observations; handshakes establish negative moments and cancellation.
Actual native/ADC ownership and prepared readers, real FFmpeg for successful runs.
"""
import hashlib, json, os, pathlib, select, signal, socket, subprocess, sys, textwrap, time
sys.dont_write_bytecode = True
cli,native,work = [pathlib.Path(p).resolve() for p in sys.argv[1:]]
repo = pathlib.Path(__file__).resolve().parents[2]
sys.path.insert(0,str(repo/'native/dng-linear-frame/tests'))
from dng_fixture import make_dng

def executable(path, body):
    path.write_text('#!/usr/bin/python3\n'+textwrap.dedent(body));path.chmod(0o755);return path

def absent(pid):
    try: os.kill(pid,0);return False
    except ProcessLookupError:return True

def run_case(mode):
    root=work/mode;root.mkdir()
    incoming=root/'input';incoming.mkdir()
    count=65 if mode=='long-sequence' else (6 if mode=='mapping' else (5 if mode in ['out-of-order','measure-error','shared-lifetime'] else 4))
    for i in range(count):
        make_dng(incoming,f'frame_{i}',pi=32803,w=64,h=64,neutral=[.5+i*.01,1,.75+i*.01],datetime_original=f'2026:11:10 12:{i//60:02}:{i%60:02}')
    originals={p.name:hashlib.sha256(p.read_bytes()).hexdigest() for p in incoming.iterdir()}
    if mode in ['interrupt','terminate','timeout','mapping']:
        physical=root/'physical';physical.mkdir()
        names=['same_0','same_0','SAME_0','space 非 % 3','café_4','cafe\u0301_4']
        for i in range(count):
            directory=physical/str(i);directory.mkdir();alias=incoming/f'frame_{i}.dng'
            source=directory/(names[i] + '.dng');alias.rename(source);alias.symlink_to(source)
    base=make_dng(root,'base' ,w=64,h=64)
    cfa=make_dng(root,'wrong',pi=32803,w=64,h=64)
    sock=socket.socket();sock.bind(('127.0.0.1',0));sock.listen(8);sock.settimeout(15)
    address=sock.getsockname()
    calls=root/'calls.jsonl';measure=root/'measure-starts';early=root/'early-finished';owned=[];connections=[]
    converter=executable(root/'converter',f'''
        import json,os,pathlib,signal,socket,subprocess,sys,time
        args=sys.argv[1:];out=pathlib.Path(args[args.index('-d')+1]);inputs=args[args.index('-mp')+1:]
        with open({str(calls)!r},'a') as f:f.write(json.dumps(inputs)+'\\n')
        mode={mode!r};s=None
        first='frame_0' in pathlib.Path(inputs[0]).stem
        blocked=mode in ['interrupt','terminate','timeout']
        if blocked:
            child=subprocess.Popen([sys.executable,'-c','import signal; signal.pause()'])
            def term(sig,frame):
                child.wait(timeout=3);sys.exit(143)
            signal.signal(signal.SIGTERM,term)
            s=socket.socket();s.connect({address!r});s.sendall((json.dumps({{'kind':'blocked','native':os.getppid(),'converter':os.getpid(),'worker':child.pid,'group':os.getpgrp()}})+'\\n').encode());s.recv(1);sys.exit(7)
        if mode in ['partial','complete','descendant-complete','out-of-order'] and first:
            s=socket.socket();s.connect({address!r})
            if mode=='descendant-complete':
                if os.fork():sys.exit(0)
        order=[3,1,0,2,4] if mode=='out-of-order' and first else range(len(inputs))
        for i in order:
            if mode=='missing' and i==len(inputs)-1:continue
            target=out/(pathlib.Path(inputs[i]).stem+'.dng');data=pathlib.Path({str(cfa if mode=='wrong' else base)!r}).read_bytes()
            if mode=='partial' and i==0:
                target.write_bytes(data[:32]);s.sendall(b'{{"kind":"partial"}}\\n');s.recv(1)
                with target.open('ab') as f:f.write(data[32:])
            elif mode=='early-mutation' and i<2:
                with target.open('wb') as f:
                    f.write(data);f.flush();time.sleep(.25)
            else:target.write_bytes(data)
            if mode=='out-of-order' and first:
                s.sendall((json.dumps({{'kind':'written','id':i}})+'\\n').encode());s.recv(1)
        if mode=='extra':(out/'unexpected.dng').write_bytes(data)
        if mode=='early-mutation':
            marker=pathlib.Path({str(early)!r})
            deadline=time.monotonic()+9
            while not marker.exists() or len(marker.read_text().splitlines())<2:
                if time.monotonic()>deadline:sys.exit(11)
                time.sleep(.02)
            # Identical bytes, different file identity. The second result must
            # be rejected even though a content hash comparison would match.
            target=out/'frame_1.dng'
            replacement=out/'replacement'
            replacement.write_bytes(target.read_bytes())
            os.replace(replacement,target)
        if s:
            s.sendall(b'{{"kind":"complete"}}\\n');s.recv(1)
        if mode=='nonzero':sys.exit(7)
    ''')
    wrapper=executable(root/'native-adapter',f'''
        import json,os,pathlib,socket,subprocess,sys
        from urllib.parse import unquote
        args=sys.argv[1:];mode={mode!r}
        if args[0] in ['--stdout-analysis','--stdout-frame-f32']:
            payload=sys.stdin.buffer.read()
            context={{k:unquote(v) for k,v in (line.split('=',1) for line in payload.decode().splitlines()[1:])}}
            if args[0]=='--stdout-frame-f32' and mode=='shared-lifetime' and context['frame_id']=='4':
                dng=pathlib.Path(context['prepared_dng'])
                assert dng.is_file() and not dng.with_name('frame_0.dng').exists()
                assert list(dng.parent.parent.iterdir())==[dng.parent]
                pathlib.Path({str(root/'shared-survived-delete')!r}).write_text(str(dng))
            if args[0]=='--stdout-analysis' and mode=='early-mutation' and args[1]=='--export-early':
                child=subprocess.run([{str(native)!r},*args],input=payload)
                if child.returncode==0:
                    with open({str(early)!r},'a') as f:f.write(context['frame_id']+'\\n')
                sys.exit(child.returncode)
            if args[0]=='--stdout-analysis':
                with open({str(measure)!r},'a') as f:f.write(context['frame_id']+'\\n')
                if mode=='measure-error':
                    s=socket.socket();s.connect({address!r});s.sendall(b'{{"kind":"measurement"}}\\n');s.recv(1);sys.stderr.write('chosen measurement failure');sys.exit(9)
            reader,writer=os.pipe()
            assert os.write(writer,payload)==len(payload)
            os.close(writer);os.dup2(reader,0);os.close(reader)
        if args[0]=='--prepare-export-early' and mode in ['missing-id','extra-id','duplicate-id','source-id','truncated-frame']:
            p=subprocess.run([{str(native)!r},*args],stdout=subprocess.PIPE)
            if p.returncode:sys.exit(p.returncode)
            records=p.stdout.split(b'\\0')[:-1]
            if mode=='missing-id':records.pop()
            if mode=='extra-id':records[-1]=records[-1].replace(b'frame_id=3',b'frame_id=999')
            if mode=='duplicate-id':records[-1]=records[-1].replace(b'frame_id=3',b'frame_id=0')
            if mode=='source-id':records[-1]=records[-1].replace(b'source=',b'source=/wrong/',1)
            data=b'\\0'.join(records)+b'\\0'
            if mode=='truncated-frame':data=data[:-8]
            sys.stdout.buffer.write(data);sys.exit(0)
        os.execv({str(native)!r},[{str(native)!r},*args])
    ''')
    graph=json.loads((repo/'pipeline/src/graph/default.json').read_text())
    if mode=='mapping':graph['nodes'].pop()
    if mode=='shared-lifetime':graph['nodes'][3]['concurrency']=1
    config=root/'graph.json';config.write_text(json.dumps(graph))
    video=root/'output'/'timelapse.mov';log=video.with_suffix('.log')
    args=[str(cli),'--input',str(incoming),'--output',str(video),'--graph',str(config),'--dng-linear-frame',str(wrapper),'--converter',str(converter),'--converter-timeout','1' if mode=='timeout' else '10','--resize','64x64']
    if mode=='out-of-order':args+=['--verify-output']
    p=subprocess.Popen(args,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
    def packet():
        conn,_=sock.accept();connections.append(conn);conn.settimeout(15)
        stream=conn.makefile('rb',buffering=0);return conn,stream,json.loads(stream.readline())
    def paused():
        try:p.wait(timeout=.1);raise AssertionError('graph exited behind live writer barrier')
        except subprocess.TimeoutExpired:pass
        trace=log.read_text()
        assert 'kind=prepare frame=' not in trace and 'kind=node node=convert-cache' not in trace,trace
        assert not measure.exists(), 'measurement started before complete-set publication'
        (root/f'negative-{len(list(root.glob("negative-*")))}.json').write_text(json.dumps({'zero_prepare_commits':True,'zero_measure_starts':True,'cli_running':True}))
    try:
        if mode in ['partial','complete','descendant-complete','out-of-order']:
            conn,stream,msg=packet()
            if mode=='partial':
                assert msg['kind']=='partial';paused();conn.sendall(b'x');msg=json.loads(stream.readline())
            if mode=='out-of-order':
                written=[]
                for expected in [3,1,0,2,4]:
                    assert msg=={'kind':'written','id':expected},msg;written.append(expected);paused();conn.sendall(b'x');msg=json.loads(stream.readline())
                (root/'adc-order.json').write_text(json.dumps(written))
            assert msg['kind']=='complete';paused();conn.sendall(b'x')
        elif mode in ['interrupt','terminate','timeout']:
            conn,stream,msg=packet();owned=[msg[k] for k in ['native','converter','worker']]
            assert msg['group']==msg['converter'] and msg['group']!=os.getpgid(p.pid),msg
            paused()
            if mode!='timeout':p.send_signal(signal.SIGINT if mode=='interrupt' else signal.SIGTERM)
        elif mode=='measure-error':
            conn,stream,msg=packet()
            assert msg['kind']=='measurement',msg
            conn.sendall(b'!')
        stdout,stderr=p.communicate(timeout=25)
        trace=log.read_text()
        success=mode in ['partial','complete','descendant-complete','out-of-order','mapping','shared-lifetime','long-sequence','early-mutation']
        assert (p.returncode==0)==success,(mode,p.returncode,stderr.decode())
        assert video.exists()==success
        if success:
            ids=[int(line.split('frame=')[1].split()[0]) for line in trace.splitlines() if 'kind=prepare frame=' in line]
            assert ids==list(range(1,count+1)),ids
            prepared=[line for line in trace.splitlines() if 'kind=prepare.sequence ' in line]
            assert len(prepared)==1 and f'count={count} adc_calls=1 ' in prepared[0],prepared
            assert len(calls.read_text().splitlines())==1
            assert trace.count('kind=solve frames=')==1
            assert trace.count('kind=node node=convert-cache frame=sequence ')==1
            if mode=='early-mutation':
                assert set(early.read_text().splitlines())=={'0','1'}
                completed=[line for line in trace.splitlines() if 'kind=measure frame=' in line]
                assert 'frame=1 ' in completed[0] and 'reused_early=true' in completed[0], completed
                assert 'frame=2 ' in completed[1] and 'reused_early=false' in completed[1], completed
            if mode=='shared-lifetime':assert (root/'shared-survived-delete').is_file()
            if mode=='mapping':
                import shlex
                start=next(line for line in trace.splitlines() if 'kind=run.start' in line)
                scratch=pathlib.Path(next(t.split('=',1)[1] for t in shlex.split(start) if t.startswith('scratch=')))
                cache=scratch/'cache'
                assert list(cache.iterdir())==[cache/'linear']
                assert len(list((cache/'linear').glob('*.dng')))==6
            if mode=='out-of-order':
                develops=[line for line in trace.splitlines() if 'kind=develop frame=' in line]
                assert len(develops)==5
                assert all(' wb=0.5,1,0.75' in line for line in develops),develops
        else:
            if mode!='measure-error':
                assert 'kind=prepare frame=' not in trace and not measure.exists(),trace
            assert 'kind=run.success' not in trace
            assert all(absent(pid) for pid in owned),owned
            if mode=='measure-error':assert b'chosen measurement failure' in stderr,stderr
            if mode in ['interrupt','terminate']:assert p.returncode==130,p.returncode
            if mode=='timeout':assert b'timed out' in stderr or b'timeout' in stderr,stderr
        if mode in ['interrupt','terminate','timeout']:assert len(calls.read_text().splitlines())==1,'Converter restarted after cancellation'
        for name,digest in originals.items():assert hashlib.sha256((incoming/name).read_bytes()).hexdigest()==digest
        receipt={'command':args,'exit':p.returncode,'success':success,'owned_absent':owned,'adc_calls':len(calls.read_text().splitlines()) if calls.exists() else 0}
        (root/'result.json').write_text(json.dumps(receipt,indent=2));print(json.dumps(receipt),flush=True)
    finally:
        killed=p.poll() is None
        if killed:p.kill()
        stdout,stderr=p.communicate(timeout=10)
        (root/'stdout').write_bytes(stdout);(root/'stderr').write_bytes(stderr)
        (root/'exit.json').write_text(json.dumps({'exit':p.returncode,'harness_killed':killed,'command':args}))
        for conn in connections:conn.close()
        sock.close()
        for pid in owned:
            if not absent(pid):
                try:os.kill(pid,signal.SIGKILL)
                except ProcessLookupError:pass

for mode in ['partial','complete','descendant-complete','nonzero','missing','wrong','extra','missing-id','extra-id','duplicate-id','source-id','truncated-frame','interrupt','terminate','timeout','out-of-order','measure-error','mapping','shared-lifetime','long-sequence','early-mutation']:
    run_case(mode)
