"""Real native prepare, controlled independently grouped Converter, caller stop.
Readiness and error injection use Unix sockets/FIFOs, never timed sleeps.
"""
import json, os, pathlib, signal, socket, subprocess, sys, textwrap
cli,native,work=map(pathlib.Path,sys.argv[1:]);work.mkdir(parents=True,exist_ok=True)
repo=pathlib.Path(__file__).resolve().parents[2]
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
    sock_path=f'/tmp/rawtl-stop-{os.getpid()}-{mode}.sock'
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
        if 'lavfi' in sys.argv: sys.exit(0)
        s=socket.socket(socket.AF_UNIX);s.connect({sock_path!r});s.sendall((json.dumps({{'kind':'encoder','encoder':os.getpid()}})+'\\n').encode());s.close()
        sys.stdin.buffer.read()
        signal.pause()
    ''')
    output=root/'run'
    args=[str(cli),'--input',str(incoming),'--output',str(output),'--strength','0','--dng-linear-frame',str(wrapper if error_mode else native),'--converter',str(converter),'--ffmpeg',str(encoder)]
    proc=subprocess.Popen(args,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
    owned={};ready=[]
    try:
        for _ in range(2 if late else 1):
            conn,_=server.accept()
            with conn:
                packet=b''
                while not packet.endswith(b'\n'): packet+=conn.recv(4096)
            event=json.loads(packet);ready.append(event);owned.update({k:v for k,v in event.items() if k in ['native','converter','worker','encoder']})
        c=next(e for e in ready if e['kind']=='converter')
        assert c['group']==c['converter'] and c['group']!=os.getpgid(proc.pid),ready
        if error_mode:
            with control.open('wb',buffering=0) as f: f.write(b'!')
        else: proc.send_signal(signal.SIGINT if mode=='interrupt' else signal.SIGTERM)
        stdout,stderr=proc.communicate(timeout=15)
        (root/'stdout.txt').write_bytes(stdout);(root/'stderr.txt').write_bytes(stderr)
        assert proc.returncode!=0,(args,stdout,stderr)
        assert all(absent(pid) for pid in owned.values()),owned
        assert not (output/'output.mov').exists()
        trace=(output/'logs/graph.log').read_text()
        if late:
            assert 'node=apply-ev frame=Some(1)' not in trace,trace
            assert 'node=apply-ev frame=Some(0)' in trace,trace
        else: assert 'node=apply-ev' not in trace,trace
        record={'mode':mode,'command':args,'exit':proc.returncode,'ready':ready,'owned_absent_at_return':owned}
        (root/'result.json').write_text(json.dumps(record,indent=2));print(json.dumps(record),flush=True)
    finally:
        if proc.poll() is None: proc.kill();proc.wait()
        for key,pid in owned.items():
            if not absent(pid):
                try: os.kill(pid,signal.SIGKILL)
                except ProcessLookupError: pass
        server.close();os.unlink(sock_path)
