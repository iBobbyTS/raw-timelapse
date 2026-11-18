"""Batch atomicity, writer termination, mapping, shared provenance and cancellation.

All timing assertions use an explicit socket handshake; deadlines only bound faults.
"""
import hashlib, json, os, pathlib, select, shutil, signal, socket, subprocess, sys, time
from urllib.parse import unquote
from dng_fixture import make_dng

exe, work = [pathlib.Path(x).resolve() for x in sys.argv[1:]]
work = work / str(time.time_ns())
work.mkdir(parents=True, exist_ok=True)
receipts = []
def record(argv, p, out, err):
    receipts.append(dict(argv=list(map(str, argv)), exit=p.returncode,
                         stdout=out.decode(errors='replace'), stderr=err.decode(errors='replace')))
    (work/'commands.json').write_text(json.dumps(receipts, indent=2))
def run(argv, code=0):
    p = subprocess.run(list(map(str, argv)), capture_output=True, timeout=30)
    record(argv,p,p.stdout,p.stderr)
    assert p.returncode == code, (argv,p.returncode,code,p.stderr)
    return p

def fields(path):
    return {k:unquote(v) for k,v in (line.split('=',1) for line in path.read_text().splitlines()[1:])}
def esc(x):
    return ''.join('%%%02x'%b if b<32 or b==127 or b==37 else chr(b) for b in str(x).encode()).encode('latin1').decode()
def manifest(root, sources, ids=None):
    root.mkdir()
    ids = list(range(len(sources))) if ids is None else ids
    text='rawtl-prepare-request/1\ncount=%d\n'%len(sources)
    for i,(source,id) in enumerate(zip(sources,ids)):
        for key,value in [('id',id),('source',source),('context',root/('%d.ctx'%i))]:
            text+='item%d_%s=%s\n'%(i,key,esc(value))
    (root/'request').write_text(text)
    return root/'request'
def args(root, converter, timeout=5):
    command = [exe,'--prepare-sequence',root/'request','--scratch',root,'--emit-result',root/'result',
               '--converter',converter]
    return command if timeout is None else command + ['--converter-timeout',str(timeout)]

base=make_dng(work,'base',w=64,h=64,pixels=[500]*3*4096)
cfa=make_dng(work,'cfa',pi=32803,w=64,h=64,pixels=[500]*3*4096)
sources=[]
for name in ['one.CR3','two.CR3']:
    p=work/name; p.write_bytes(b'fake vendor raw '+name.encode()); sources.append(p)
# The fake forks a writer to exercise process-group ownership after leader exit.
fake=work/'adc'
fake.write_text('''#!/usr/bin/env python3
import os,sys,pathlib,socket,shutil,signal,time
args=sys.argv[1:]; out=pathlib.Path(args[args.index('-d')+1]); inputs=args[args.index('-mp')+1:]
mode=os.environ.get('BATCH_MODE','ok')
if mode=='noisy': print('é'*100000)
if mode=='nul': os.write(2,b'warning before\\x00after\\n')
if mode=='root-error':
 r,w=os.pipe()
 if os.fork():
  os.close(w); os.read(r,1); sys.exit(7)
 else:
  os.close(r); signal.signal(signal.SIGTERM,signal.SIG_IGN)
  pathlib.Path(os.environ['BATCH_PID']).write_text(str(os.getpid()))
  os.write(w,b'x');os.close(w);time.sleep(60);sys.exit(0)
with open(os.environ['BATCH_CALLS'],'a') as f: f.write(' '.join(inputs)+'\\n')
s=None
if os.environ.get('BATCH_SOCKET'):
 s=socket.socket(); host,port=os.environ['BATCH_SOCKET'].split(':'); s.connect((host,int(port)))
 if mode in ('descendant','descendant-complete'):
  if os.fork(): sys.exit(0)
 if mode in ('blocked','descendant'):
  def term(sig,frame): pathlib.Path(os.environ['BATCH_TERM']).write_text('TERM')
  signal.signal(signal.SIGTERM,term)
  s.sendall(('PID %d\\n'%os.getpid()).encode()); s.recv(1)
for i,source in enumerate(inputs):
 if mode=='missing' and i==len(inputs)-1: continue
 target=out/(pathlib.Path(source).stem+'.dng')
 data=pathlib.Path(os.environ['BATCH_CFA'] if mode=='wrong' else os.environ['BATCH_BASE']).read_bytes()
 if mode=='partial' and i==0:
  target.write_bytes(data[:32]); s.sendall(b'PARTIAL\\n'); s.recv(1)
  with target.open('ab') as f: f.write(data[32:])
 else: target.write_bytes(data)
if mode=='extra': (out/'unexpected.dng').write_bytes(data)
if s and mode in ('partial','complete','descendant-complete'):
 s.sendall(b'COMPLETE\\n'); s.recv(1)
if mode=='fail': sys.exit(7)
'''); fake.chmod(0o755)
os.environ.update(BATCH_BASE=str(base),BATCH_CFA=str(cfa),BATCH_CALLS=str(work/'calls'))

def assert_paused(p, root):
    # The writer remains behind an explicit socket barrier. Observe the native
    # process's terminal state for a bounded negative interval; this is not a
    # file-stability delay or a guess that conversion has finished.
    try:
        p.wait(timeout=0.2)
        raise AssertionError('native exited while its writer was held at the barrier')
    except subprocess.TimeoutExpired:
        pass
    assert not (root/'result').exists() and not list(root.glob('*.ctx'))

def gated(name, mode, sig=None):
    root=work/name
    selected=sources
    if sig:
        duplicate=work/(name+'-source'); duplicate.mkdir(); alias=duplicate/sources[0].name; alias.write_bytes(b'other raw'); selected=[sources[0],alias]
    manifest(root,selected)
    previous_calls=len((work/'calls').read_text().splitlines()) if (work/'calls').exists() else 0
    sock=socket.socket(); sock.bind(('127.0.0.1',0)); sock.listen(1); address='127.0.0.1:%d'%sock.getsockname()[1]
    os.environ.update(BATCH_MODE=mode,BATCH_SOCKET=address,BATCH_TERM=str(root/'term'))
    argv=args(root,fake,1 if mode=='blocked' and sig is None else 5)
    p=subprocess.Popen(list(map(str,argv)),stdout=subprocess.PIPE,stderr=subprocess.PIPE)
    try:
        assert select.select([sock],[],[],10)[0], 'no fake handshake'
        conn,_=sock.accept(); conn.settimeout(10)
        msg=conn.recv(128); assert msg
        assert_paused(p,root)
        if sig: p.send_signal(sig)
        elif mode!='blocked':
            conn.sendall(b'x')
            if mode=='partial':
                assert conn.recv(128)==b'COMPLETE\n'
                assert_paused(p,root)
                conn.sendall(b'x')
        out,err=p.communicate(timeout=12); record(argv,p,out,err)
        expected=130 if sig else (25 if mode=='blocked' else 0)
        assert p.returncode==expected,(p.returncode,err)
        assert (root/'result').exists()==(expected==0)
        if expected in (25,130): assert (root/'term').read_text()=='TERM', 'missing TERM before KILL escalation'
        if sig: assert len((work/'calls').read_text().splitlines())==previous_calls+1, 'Converter restarted after cancellation'
        if msg.startswith(b'PID '):
            pid=int(msg.split()[1])
            try:
                os.kill(pid,0)
                # An orphan zombie cannot write; macOS init may reap asynchronously.
                status=subprocess.run(['ps','-o','stat=','-p',str(pid)],capture_output=True,text=True).stdout.strip()
                assert not status or status.startswith('Z'),(pid,status)
            except ProcessLookupError: pass
    finally:
        if p.poll() is None: p.kill(); p.wait()
        sock.close(); os.environ.pop('BATCH_SOCKET',None)

for mode in ['partial','complete','descendant-complete']:
    gated('gate-'+mode,mode)
for mode in ['fail','missing','wrong','extra']:
    os.environ['BATCH_MODE']=mode
    root=work/mode; manifest(root,sources)
    p=run(args(root,fake),24 if mode=='fail' else 23)
    assert not (root/'result').exists() and not list(root.glob('*.ctx'))
    assert b'one.CR3' in p.stderr
for sig in [signal.SIGINT,signal.SIGTERM]: gated('cancel-'+str(sig), 'blocked',sig)
gated('timeout','blocked')
gated('descendant','descendant',signal.SIGTERM)
# Same-stem, case and Unicode canonical equivalents use distinct input aliases.
os.environ['BATCH_MODE']='ok'
mapped=[]
for i,name in enumerate(['same.CR3','same.CR3','SAME.CR3','caf\u00e9.CR3','cafe\u0301.CR3','space 非 % name.CR3']):
    d=work/('source-'+str(i)); d.mkdir(); p=d/name; p.write_bytes(b'raw'+bytes([i])); mapped.append(p)
root=work/'mapping'; manifest(root,mapped)
previous_calls=len((work/'calls').read_text().splitlines()); run(args(root,fake))
assert len((work/'calls').read_text().splitlines())==previous_calls+1
contexts=[fields(root/('%d.ctx'%i)) for i in range(len(mapped))]
assert len({c['prepared_dng'] for c in contexts})==len(mapped)
assert len({str(pathlib.Path(c['prepared_dng']).parent) for c in contexts})==1
run([exe,'--stdout-analysis','--prepared',root/'0.ctx'])
# Delete one output; the surviving conversion record and the other consumers survive.
root=work/'mixed'; manifest(root,[base,cfa,*sources])
assert b'timeout_s=45 ' in run(args(root,fake,timeout=None)).stderr
f=[fields(root/('%d.ctx'%i)) for i in range(4)]
assert [c['route'] for c in f]==['linearraw-sdk','cfa-converter-linearraw-sdk',*(['vendor-raw-converter-linearraw-sdk']*2)]
pathlib.Path(f[1]['prepared_dng']).unlink()
run([exe,'--stdout-analysis','--prepared',root/'2.ctx'])
assert pathlib.Path(f[2]['converter_record']).exists()
# More than the old 16 argv bound, request identity and last context write failure.
many=[]
for i in range(20):
    p=work/('many%d.CR3'%i); p.write_bytes(b'raw'); many.append(p)
root=work/'many'; manifest(root,many); run(args(root,fake))
assert fields(pathlib.Path(fields(root/'0.ctx')['converter_record']))['argument']==str(many[0])
for name,ids in [('duplicate',[1,1])]:
    root=work/name; manifest(root,sources,ids); run(args(root,fake),26); assert not (root/'result').exists()
root=work/'context-failure'; manifest(root,sources); (root/'request').write_text((root/'request').read_text().replace(str(root/'1.ctx'),str(root/'missing'/'1.ctx')))
run(args(root,fake),26); assert not (root/'result').exists()
assert (root/'0.ctx').exists(), 'failure must occur on last context write'
print('PASS: sequence gates, failures, cancellation, mapping, isolated records, mixed routes and identity')

# Exercise one invocation beyond ARG_MAX with short input aliases.
longdir=work/'long-paths'
while len(str(longdir).encode()) < 1006:
    remaining=1006-len(str(longdir).encode())-1
    if remaining <= 0: break
    longdir=longdir/('x'*min(190,remaining))
longdir.mkdir(parents=True)
long_sources=[]
for i in range(1100):
    p=longdir/('f%03d.CR3'%i);p.write_bytes(b'raw');long_sources.append(p)
root=work/'argv-budget';manifest(root,long_sources)
previous_calls=len((work/'calls').read_text().splitlines())
assert b'timeout_s=16500 ' in run(args(root,fake,timeout=None)).stderr
assert len((work/'calls').read_text().splitlines())==previous_calls+1
assert (root/'converter-input'/'0.CR3').is_symlink()
assert len(list(root.glob('linear/*.dng')))==1100

# A reference with a matching byte hash still must describe this source/output.
root=work/'many';ctx=root/'0.ctx';original=ctx.read_text();f=fields(ctx)
r=pathlib.Path(f['converter_record']);bad=work/'wrong.record'
bad.write_text(r.read_text().replace('source=', 'source=/wrong-source',1))
tampered=original.replace(f['converter_record'],str(bad)).replace(f['converter_record_sha256'],hashlib.sha256(bad.read_bytes()).hexdigest())
wrong=work/'wrong-record.ctx';wrong.write_text(tampered)
run([exe,'--stdout-analysis','--prepared',wrong],26)

os.environ['BATCH_MODE']='noisy'
root=work/'noisy';manifest(root,sources);run(args(root,fake))
f=fields(root/'0.ctx');record_path=pathlib.Path(f['converter_record'])
assert (root/'0.ctx').stat().st_size < 65536 and record_path.stat().st_size < 1024*1024
assert (root/'linear-converter.log').stat().st_size > 200000
assert 'transcript' not in fields(record_path)
run([exe,'--stdout-analysis','--prepared',root/'0.ctx'])
os.environ['BATCH_MODE']='root-error';os.environ['BATCH_PID']=str(work/'root-error.pid')
root=work/'root-error';manifest(root,sources)
p=run(args(root,fake),24);assert b'exit=7' in p.stderr
assert not (root/'result').exists()
pid=int((work/'root-error.pid').read_text())
status=subprocess.run(['ps','-o','stat=','-p',str(pid)],capture_output=True,text=True).stdout.strip()
assert not status or status.startswith('Z'),(pid,status)

# Direct-only sequences never spawn the configured (deliberately missing) executable.
root=work/'all-direct';manifest(root,[base]);run(args(root,work/'does-not-exist'))
assert fields(root/'0.ctx')['generated']=='0'
assert not list(root.glob('linear*')) and not list(root.glob('conversion-*.record'))
# Malformed request identity/output boundaries fail before any ADC call.
for mode in ['same-context','context-is-source','truncated','old-version']:
    root=work/('request-'+mode);manifest(root,sources)
    text=(root/'request').read_text()
    if mode=='same-context': text=text.replace(str(root/'1.ctx'),str(root/'0.ctx'))
    if mode=='context-is-source': text=text.replace(str(root/'1.ctx'),str(sources[1]))
    if mode=='truncated': text=text[:-1]
    if mode=='old-version': text=text.replace('rawtl-prepare-request/1','rawtl-prepare-request/0')
    (root/'request').write_text(text)
    run(args(root,fake),26);assert not (root/'result').exists()
# Record digest/version/truncation errors are rejected by real native readers.
f=fields(work/'many'/'0.ctx');original=(work/'many'/'0.ctx').read_text();r=pathlib.Path(f['converter_record'])
for name,text in [('record-version',r.read_text().replace('rawtl-sequence-conversion/1','rawtl-sequence-conversion/0')),('record-truncated',r.read_text()[:-1])]:
    record_path=work/(name+'.record');record_path.write_text(text)
    ctx=work/(name+'.ctx');ctx.write_text(original.replace(str(r),str(record_path)).replace(f['converter_record_sha256'],hashlib.sha256(record_path.read_bytes()).hexdigest()))
    run([exe,'--stdout-analysis','--prepared',ctx],26)

# A converter's raw log may contain NUL; published documents must not carry it.
os.environ['BATCH_MODE']='nul'
root=work/'nul';manifest(root,sources);run(args(root,fake))
assert (root/'result').is_file()
f=fields(root/'0.ctx');record_path=pathlib.Path(f['converter_record'])
assert (root/'linear-converter.log').read_bytes()==b'warning before\x00after\n'
# Keep the reader contract strict, including a hash-consistent forged record.
context_nul=work/'context-nul.ctx'
context_nul.write_text((root/'0.ctx').read_text().replace('converter_cleanup=','converter_cleanup=%00',1))
record_nul=work/'record-nul.record'
record_nul.write_text(record_path.read_text().replace('version=','version=%00',1))
record_nul_context=work/'record-nul.ctx'
record_nul_context.write_text((root/'0.ctx').read_text().replace(str(record_path),str(record_nul)).replace(f['converter_record_sha256'],hashlib.sha256(record_nul.read_bytes()).hexdigest()))
for forged in [context_nul,record_nul_context]:
    rejected=run([exe,'--stdout-analysis','--prepared',forged],26)
    assert b'prepared document contains NUL' in rejected.stderr
# Red-first: the unfixed writer publishes success, but this real consumer fails.
run([exe,'--stdout-analysis','--prepared',root/'0.ctx'])
assert 'transcript' not in fields(record_path)
assert '%00' not in record_path.read_text()
