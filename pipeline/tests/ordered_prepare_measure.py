"""Gate actual CLI tasks with native-shaped subprocesses, using no timing sleeps."""
import json, os, pathlib, socket, subprocess, sys
cli, tool, root = map(pathlib.Path, sys.argv[1:])
work = pathlib.Path(os.environ.get('RAWTL_TEST_WORK', str(root.parent))).parent
sock = work / f'wb{os.getpid()}.sock'
server = socket.socket(socket.AF_UNIX); server.bind(str(sock)); server.listen(8); server.settimeout(20)
wrapper = root/'gated-native'
wrapper.write_text(f'''#!/usr/bin/python3
import json,os,pathlib,socket,subprocess,sys
args=sys.argv[1:]
result=subprocess.run([{str(tool)!r},*args])
if result.returncode: sys.exit(result.returncode)
if args[0] in ['--prepare','--stdout-analysis','--stdout-frame-f32']:
    source=pathlib.Path(args[1]).stem if args[0]=='--prepare' else pathlib.Path(dict(line.split('=',1) for line in pathlib.Path(args[2]).read_text().splitlines()[1:])['source']).stem
    s=socket.socket(socket.AF_UNIX);s.connect({str(sock)!r});s.sendall((json.dumps({{'stage':args[0],'source':source,'args':args}})+'\\n').encode());s.recv(1);s.close()
sys.exit(0)
''')
wrapper.chmod(0o755)
graph=json.loads((pathlib.Path(__file__).parent/'../src/graph/default.json').resolve().read_text())
for n in graph['nodes']:
    n['concurrency']=4 if n['kind'] in ['prepare','solve','develop'] else 1
graph_path=root/'graph.json';graph_path.write_text(json.dumps(graph))
args=[str(cli),'--input',str(root/'input'),'--output',str(root/'output/timelapse.mov'),'--dng-linear-frame',str(wrapper),'--graph',str(graph_path)]
proc=subprocess.Popen(args,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
trace=[]
try:
    for stage in ['--prepare','--stdout-analysis','--stdout-frame-f32']:
        peers={}
        for _ in range(4):
            conn,_=server.accept(); conn.settimeout(20)
            data=b''
            while not data.endswith(b'\n'): data+=conn.recv(4096)
            event=json.loads(data); assert event['stage']==stage,event
            peers[event['source']]=(conn,event)
        # Frame zero remains gated until all later frames have returned.
        for frame in [3,2,4,1]:
            conn,event=peers[f'IMG_{frame}'];trace.append(event)
            conn.sendall(b'!');assert conn.recv(1)==b'';conn.close()
    stdout,stderr=proc.communicate(timeout=30)
    (root/'cli.stdout').write_bytes(stdout);(root/'cli.stderr').write_bytes(stderr)
    (root/'trace.json').write_text(json.dumps({'command':args,'exit':proc.returncode,'releases':trace},indent=2))
    assert proc.returncode==0,(stdout,stderr)
finally:
    if proc.poll() is None: proc.kill();proc.wait()
    server.close();sock.unlink()
