"""Real native prepare, controlled independently grouped Converter, caller stop.
Readiness and error injection use Unix sockets/FIFOs, never timed sleeps.
"""
import json, os, pathlib, signal, socket, subprocess, sys, textwrap
cli,native,work=map(pathlib.Path,sys.argv[1:]);work.mkdir(parents=True,exist_ok=True)
repo=pathlib.Path(__file__).resolve().parents[2]
sys.dont_write_bytecode=True
sys.path.insert(0,str(repo/'native/dng-linear-frame/tests'))
from dng_fixture import make_dng

def executable(path,body):
    path.write_text('#!/usr/bin/python3\n'+textwrap.dedent(body));path.chmod(0o755);return path

def absent(pid):
    try: os.kill(pid,0)
    except ProcessLookupError: return True
    return False

for mode in ['interrupt','terminate','operation-error','operation-error-with-encoder']:
    error_mode=mode.startswith('operation-error')
    late=mode.endswith('with-encoder')
    root=work/mode;root.mkdir(exist_ok=True)
    # Short socket paths avoid macOS's sockaddr_un limit.
    sock_path=str(pathlib.Path(os.environ.get('RAWTL_TEST_WORK',str(work))).parent / f'stop{os.getpid()}.sock')
    server=socket.socket(socket.AF_UNIX);server.bind(sock_path);server.listen(4);server.settimeout(15)
    incoming=root/'input';incoming.mkdir(exist_ok=True)
    mosaic=make_dng(root,'mosaic_1',pi=32803)
    if error_mode:
        for n in [1,2]: make_dng(incoming,f'linear_{n}')
    else: make_dng(incoming,'mosaic_1',pi=32803)
    converter=executable(root/'converter',f'''
        import json,os,signal,socket,subprocess,sys
        worker=subprocess.Popen([sys.executable,'-c','import signal; signal.pause()'])
        def stop(sig,frame):
            worker.wait(timeout=2)
            sys.exit(143)
        signal.signal(signal.SIGTERM,stop)
        s=socket.socket(socket.AF_UNIX);s.connect({sock_path!r})
        s.sendall((json.dumps({{'kind':'converter','native':os.getppid(),'converter':os.getpid(),'group':os.getpgrp(),'worker':worker.pid}})+'\\n').encode());s.close()
        signal.pause()
    ''')
    control=root/'inject';os.mkfifo(control)
    wrapper=executable(root/'native-adapter',f'''
        import os,pathlib,sys
        args=sys.argv[1:]
        if args[0]=='--stdout-frame-f32' and {('linear_2.dng' if late else 'linear_1.dng')!r} in pathlib.Path(args[2]).read_text():
            if os.fork()==0:
                with open({str(control)!r},'rb',buffering=0) as f: f.read(1)
                os.write(1,b'BADMAGIC');os._exit(0)
            args=['--prepare',{str(mosaic)!r},'--scratch',{str(root/'in-flight')!r},'--emit-prepared',{str(root/'in-flight.context')!r},'--converter',{str(converter)!r}]
        os.execv({str(native)!r},[{str(native)!r},*args])
    ''')
    (root/'in-flight').mkdir(exist_ok=True)
    encoder=executable(root/'encoder',f'''
        import json,os,signal,socket,sys
        if 'lavfi' in sys.argv or '-h' in sys.argv: sys.exit(0)
        s=socket.socket(socket.AF_UNIX);s.connect({sock_path!r});s.sendall((json.dumps({{'kind':'encoder','encoder':os.getpid()}})+'\\n').encode());s.close()
        sys.stdin.buffer.read()
        signal.pause()
    ''')
    output=root/'run';video=output/'timelapse.mov'
    args=[str(cli),'--input',str(incoming),'--output',str(video),'--strength','0','--dng-linear-frame',str(wrapper if error_mode else native),'--converter',str(converter),'--ffmpeg',str(encoder)]
    proc=subprocess.Popen(args,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
    owned={};ready=[]
    try:
        for _ in range(2 if late else 1):
            conn,_=server.accept()
            with conn:
                packet=b''
                while not packet.endswith(b'\n'): packet+=conn.recv(4096)
            event=json.loads(packet);ready.append(event);owned.update({(len(ready),k):v for k,v in event.items() if k in ['native','converter','worker','encoder']})
        c=next(e for e in ready if e['kind']=='converter')
        assert c['group']==c['converter'] and c['group']!=os.getpgid(proc.pid),ready
        if error_mode:
            with control.open('wb',buffering=0) as f: f.write(b'!')
        else: proc.send_signal(signal.SIGINT if mode=='interrupt' else signal.SIGTERM)
        stdout,stderr=proc.communicate(timeout=15)
        (root/'stdout.txt').write_bytes(stdout);(root/'stderr.txt').write_bytes(stderr)
        assert proc.returncode!=0,(args,stdout,stderr)
        assert all(absent(pid) for pid in owned.values()),owned
        assert not video.exists()
        trace=(output/'timelapse.log').read_text()
        # Frame IDs are 1-based in the product log; a late frame must not reach
        # the develop node.
        if late:
            assert 'node=apply-ev frame=2 ' not in trace,trace
            assert 'node=apply-ev frame=1 ' in trace,trace
        else: assert 'kind=node node=apply-ev' not in trace,trace
        record={'mode':mode,'command':args,'exit':proc.returncode,'ready':ready,'owned_absent_at_return':{f'{task}:{role}':pid for (task,role),pid in owned.items()}}
        (root/'result.json').write_text(json.dumps(record,indent=2));print(json.dumps(record),flush=True)
    finally:
        if proc.poll() is None: proc.kill();proc.wait()
        for key,pid in owned.items():
            if not absent(pid):
                try: os.kill(pid,signal.SIGKILL)
                except ProcessLookupError: pass
        server.close();os.unlink(sock_path)

# Four real native/Converter process groups, all ready before one error/stop.
# Keep every role keyed by task identity; no PID can overwrite another task.
for stage in ['prepare','measure']:
  for trigger in ['interrupt','terminate','error']:
    label=f'concurrent-{stage}-{trigger}'
    root=work/label;root.mkdir(exist_ok=True)
    incoming=root/'input';incoming.mkdir(exist_ok=True)
    for frame in range(4): make_dng(incoming,f'frame_{frame}',pi=32803 if stage=='prepare' else 34892)
    mosaic=make_dng(root,'mosaic_0',pi=32803)
    sock_path=str(pathlib.Path(os.environ.get('RAWTL_TEST_WORK',str(work))).parent / f'multi{os.getpid()}.sock')
    server=socket.socket(socket.AF_UNIX);server.bind(sock_path);server.listen(8);server.settimeout(20)
    converter=executable(root/'converter',f'''
        import json,os,signal,socket,subprocess,sys
        worker=subprocess.Popen([sys.executable,'-c','import signal; signal.pause()'])
        def stop(sig,frame):
            worker.wait(timeout=2)
            sys.exit(143)
        signal.signal(signal.SIGTERM,stop)
        s=socket.socket(socket.AF_UNIX);s.connect({sock_path!r})
        s.sendall((json.dumps({{'native':os.getppid(),'converter':os.getpid(),'group':os.getpgrp(),'worker':worker.pid,'args':sys.argv}})+'\\n').encode())
        if s.recv(1)==b'!':
            worker.terminate();worker.wait();sys.exit(9)
    ''')
    wrapper=executable(root/'native-adapter',f'''
        import os,pathlib,sys
        args=sys.argv[1:]
        if args[0]=='--stdout-analysis':
            task=pathlib.Path(args[2]).parent.name
            scratch=pathlib.Path({str(root)!r})/task;scratch.mkdir(exist_ok=True)
            args=['--prepare',{str(mosaic)!r},'--scratch',str(scratch),'--emit-prepared',str(scratch/'context'),'--converter',{str(converter)!r}]
        os.execv({str(native)!r},[{str(native)!r},*args])
    ''')
    graph=json.loads((repo/'pipeline/src/graph/default.json').read_text())
    for node in graph['nodes']: node['concurrency']=4 if node['kind'] in ['prepare','solve'] else 1
    config=root/'graph.json';config.write_text(json.dumps(graph))
    video=root/'output/timelapse.mov'
    args=[str(cli),'--input',str(incoming),'--output',str(video),'--graph',str(config),'--dng-linear-frame',str(wrapper if stage=='measure' else native),'--converter',str(converter),'--strength','1' if stage=='measure' else '0']
    proc=subprocess.Popen(args,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
    owned={};ready=[];peers=[]
    try:
        for task in range(4):
            conn,_=server.accept();peers.append(conn)
            packet=b''
            while not packet.endswith(b'\n'): packet+=conn.recv(4096)
            event=json.loads(packet);ready.append(event)
            assert event['group']==event['converter'] and event['group']!=os.getpgid(proc.pid),event
            for role in ['native','converter','worker']: owned[(task,role)]=event[role]
        if trigger=='error': peers[-1].sendall(b'!')
        else: proc.send_signal(signal.SIGINT if trigger=='interrupt' else signal.SIGTERM)
        stdout,stderr=proc.communicate(timeout=20)
        (root/'stdout.txt').write_bytes(stdout);(root/'stderr.txt').write_bytes(stderr)
        assert proc.returncode==(30 if trigger=='error' else 130),(args,proc.returncode,stdout,stderr)
        assert all(absent(pid) for pid in owned.values()),owned
        assert all(absent(-event['group']) for event in ready),ready
        assert not video.exists()
        trace=(video.with_suffix('.log')).read_text()
        assert 'kind=node node=apply-ev' not in trace,trace
        if stage=='prepare': assert 'kind=node node=convert-cache frame=' not in trace,trace
        else: assert 'kind=measure frame=' not in trace and 'node=measure-solve frame=sequence' not in trace,trace
        record={'mode':label,'command':args,'exit':proc.returncode,'ready':ready,'owned_absent_at_return':{f'{task}:{role}':pid for (task,role),pid in owned.items()}}
        (root/'result.json').write_text(json.dumps(record,indent=2));print(json.dumps(record),flush=True)
    finally:
        if proc.poll() is None: proc.kill();proc.wait()
        for conn in peers: conn.close()
        for pid in owned.values():
            if not absent(pid):
                try: os.kill(pid,signal.SIGKILL)
                except ProcessLookupError: pass
        server.close();os.unlink(sock_path)
