"""Real SDK contract: delivery recovery precedes HSM/EV; measurement is unchanged."""
import hashlib,json,pathlib,struct,subprocess,sys
from dng_fixture import make_dng
exe,work=map(pathlib.Path,sys.argv[1:]);exe=exe.resolve();work.mkdir(parents=True,exist_ok=True)
args=['--color-contract-version','2','--camera-recipe','generic','--camera-color-method','dng-camera-to-pcs','--profile-correction','none','--camera-calibration','none']
logs=[]
def run(source,enabled='on',mode='--stdout-frame-f32',ev=0,extra=(),expected=0):
    cmd=[str(exe),mode,str(source),*args,'--highlight-recovery',enabled,'--exposure-ev',str(ev),*extra]
    r=subprocess.run(cmd,capture_output=True);logs.append(dict(argv=cmd,exit_code=r.returncode,stderr=r.stderr.decode(),stdout_sha256=hashlib.sha256(r.stdout).hexdigest()))
    (work/'commands.json').write_text(json.dumps(logs,indent=2));assert r.returncode==expected,(r.returncode,r.stderr)
    if expected:return
    records={};at=8
    while at<len(r.stdout):
        t,zero,n=struct.unpack_from('<B3sQ',r.stdout,at);assert zero==b'\0'*3;at+=12;records[t]=r.stdout[at:at+n];at+=n
    return json.loads(records[1]),records

def values(records):return struct.unpack('<'+'f'*(len(records[4])//4),records[4])
base=[];truth=[]
for y in range(64):
    for x in range(64):
        v=[6500,10000,9100] if 24<=x<40 and 24<=y<40 else [2500,5000,3500]
        base+=v;truth += [3250,6500,4550] if 24<=x<40 and 24<=y<40 else [1250,2500,1750]
hsm=([2,2,1],[0,1.2,1.1]*4)
for correction in (False,True):
    label='hsm' if correction else 'matrix'
    f=make_dng(work,label,white=10000,w=64,h=64,pixels=base,neutral=[.5,1,.7],hue_sat=hsm if correction else None)
    t=make_dng(work,label+'-truth',white=10000,w=64,h=64,pixels=truth,neutral=[.5,1,.7],hue_sat=hsm if correction else None)
    extra=['--camera-recipe','legacy-test','--profile-correction','hue-sat-map'] if correction else []
    on,pr=run(f,extra=extra);off,po=run(f,'off',extra=extra);_,pt=run(t,'off',ev=1,extra=extra)
    assert on['highlight_recovery']['status']=='processed' and on['highlight_recovery']['affected_pixels']==256
    a,b,c=values(pr),values(po),values(pt);assert a!=b
    center=(32*64+32)*3
    for x,z in zip(a[center:center+3],c[center:center+3]):assert abs(x-z)<2e-5,(label,x,z)
    for ev in (-2,2):
        _,pe=run(f,ev=ev,extra=extra)
        assert all(abs(x-y*2**ev)<1e-5*max(1,abs(x)) for x,y in zip(values(pe),a))
    for request in ('on','off'):
        meta,records=run(f,request,mode='--stdout-analysis',extra=extra)
        assert meta['highlight_recovery']['status']=='off'
        if request=='on':saved=records[2]
        else:assert records[2]==saved,'measurement changed with delivery policy'
    # Original source and prepared-context consumers must share the same result.
    cache=work/(label+'-prepared');cache.mkdir(exist_ok=True);context=cache/'frame.context'
    r=subprocess.run([str(exe),'--prepare',str(f),'--scratch',str(cache),'--emit-prepared',str(context)],capture_output=True);assert r.returncode==0,r.stderr
    prepared_cmd=[str(exe),'--stdout-frame-f32','--prepared',str(context),*args,'--highlight-recovery','on',*extra]
    out=subprocess.run(prepared_cmd,capture_output=True);assert out.returncode==0,out.stderr
    # Extract pixels from prepared stream independently.
    at=8;prepared_pixels=None
    while at<len(out.stdout):
        k,_,n=struct.unpack_from('<B3sQ',out.stdout,at);at+=12
        if k==4:prepared_pixels=out.stdout[at:at+n]
        at+=n
    assert prepared_pixels==pr[4]
# A camera-domain opcode changes the endpoint interpretation: report unsupported,
# preserve all values, never pretend a valid recovery took place.
data=struct.pack('>8II2d',0,0,64,64,0,3,1,1,1,0.,.5)
op=struct.pack('>5I',1,8,0x01030000,0,len(data))+data
unsupported=make_dng(work,'opcode-endpoints',white=10000,w=64,h=64,pixels=base,neutral=[.5,1,.7],opcodes={51009:op})
m,a=run(unsupported);_,b=run(unsupported,'off')
assert m['highlight_recovery']['status']=='unsupported' and a[4]==b[4]
run(f,'invalid',expected=2)
print('PASS highlight real SDK matrix/HSM ordering, source/prepared equality, EV linearity, analysis isolation, invalid policy')
