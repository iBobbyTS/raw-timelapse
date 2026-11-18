"""Controlled SDK CameraToPCS profile development and independent PCS oracle."""
import hashlib,json,pathlib,struct,subprocess,sys
from dng_fixture import make_dng
from reference_color import reference, matrix, inv, mm, mv, cat, D50, D60, xy, D
exe,work=map(pathlib.Path,sys.argv[1:]);exe=exe.resolve();work.mkdir(parents=True,exist_ok=True)
args=['--color-contract-version','2','--camera-recipe','generic','--camera-color-method','dng-camera-to-pcs','--camera-calibration','none']
def run(f,ev=0,analysis=False,extra=(),code=0):
    cmd=[str(exe),'--stdout-analysis' if analysis else '--stdout-frame-f32',str(f),*args,'--exposure-ev',str(ev),*extra]
    r=subprocess.run(cmd,capture_output=True)
    print(json.dumps(dict(command=cmd,exit=r.returncode,stderr=r.stderr.decode(),stdout_bytes=len(r.stdout),stdout_sha256=hashlib.sha256(r.stdout).hexdigest())),flush=True)
    assert r.returncode==code,(r.returncode,r.stderr)
    if code:return
    assert r.stdout[:8]==(b'RAWTLST1' if analysis else b'RAWTLST2')
    at=8;records=[]
    while at<len(r.stdout):
        kind=r.stdout[at];assert r.stdout[at+1:at+4]==b'\0'*3
        n=struct.unpack_from('<Q',r.stdout,at+4)[0];at+=12
        records.append((kind,r.stdout[at:at+n]));at+=n
    assert at==len(r.stdout)
    meta=json.loads(records[0][1]);assert meta['color_contract_version']==2
    if analysis:return meta,json.loads(records[1][1])
    desc=json.loads(records[1][1]);assert desc['color_contract_version']==2
    return meta,struct.unpack('<'+'f'*(len(records[2][1])//4),records[2][1])
fwd=[.7,.1,.1643,.2,.7,.1,.01,.1,.7151]
hsm=([2,2,1],[0,1.2,1.3]*4)
for fm in (None,fwd):
    label='fallback' if fm is None else 'fm'
    base=make_dng(work,label,forward=fm,w=64,h=64,pixels=[0,1000,0]*4096,model='NonCanon Test')
    decorated=make_dng(work,label+'-profile',forward=fm,w=64,h=64,pixels=[0,1000,0]*4096,model='NonCanon Test',hue_sat=hsm,tone=[0,0,.5,.8,1,1],look=hsm,baseline=1.88)
    m,p=run(base);m2,p2=run(decorated)
    assert p==p2  # This signed ProPhoto fixture bypasses HSV; positive pixels tested below.
    c=m2['color'];assert c['recipe_requested']==c['recipe_effective']=='generic'
    assert c['profile_correction_requested']==c['profile_correction_effective']=='hue-sat-map'
    assert c['camera_calibration_requested']==c['camera_calibration_effective']=='none'
    assert not c['exposure_calibration']['applied'] and c['exposure_calibration']['status']=='uncalibrated'
    assert c['hue_sat_map_applied'] and c['hue_sat_map_status']=='applied'
    for field in ('look_table','tone_curve'):
        assert c[field+'_present'] and not c[field+'_applied'] and c[field+'_status']=='skipped'
    assert c['baseline_exposure_ev']==1.88 and not c['baseline_exposure_applied']
    # Independent Decimal no-FM reference; FM branch uses D50-normalized FM,
    # SDK reference neutral at the D50 PCS white, then the existing input CAT.
    if fm is None: _,expected=reference()
    else:
        fm_matrix=matrix([fm[:3],fm[3:6],fm[6:]])
        fm_matrix=[[v*D50[i]/sum(row) for v in row] for i,row in enumerate(fm_matrix)]
        neutral=[D('.6050808314'),D(1),D('.4825046041')]
        camera_to_d50=mm(fm_matrix,[[1/neutral[i] if i==j else D(0) for j in range(3)] for i in range(3)])
        columns=[list(v) for v in zip(xy('.713','.293'),xy('.165','.830'),xy('.128','.044'))]
        factors=mv(inv(columns),D60)
        ap1=[[columns[i][j]*factors[j] for j in range(3)] for i in range(3)]
        expected=mm(inv(ap1),mm(cat(D50,D60),camera_to_d50))
    expected_pixel=[float(row[1]) for row in expected]
    for a,b in zip(p[:3],expected_pixel):assert abs(a-b)<2e-6,(label,p[:3],expected_pixel)
    for ev in (-2,2,5):
        me,pe=run(decorated,ev)
        assert me['color']==m2['color']
        for a,b in zip(pe[:3],p2[:3]):assert abs(a-b*2**ev)<2e-6*max(1,abs(a))
    assert max(run(decorated,5)[1][:3])>1
    if fm is None:assert min(p[:3])<0
    ma,an=run(decorated,analysis=True);assert ma['color']==m2['color']
    assert ma['exposure_ev']==0
    run(decorated,extra=['--color-contract-version','1'],code=2)
    run(decorated,extra=['--camera-calibration','canon-normal-white-fixed-v1'],code=2)
    # A specialized request on direct non-Canon DNG cannot silently become generic.
    run(decorated,extra=['--camera-recipe','canon-cr3','--camera-calibration','canon-normal-white-fixed-v1'],code=40)
# HSM is intrinsic to every development recipe, independent of output encoding.
# A positive fixture must change pixels; absence must leave the matrix result intact.
for fm in (None,fwd):
    label='positive-fallback' if fm is None else 'positive-fm'
    base=make_dng(work,label,forward=fm,w=64,h=64,pixels=[500,400,200]*4096)
    decorated=make_dng(work,label+'-hsm',forward=fm,w=64,h=64,pixels=[500,400,200]*4096,
        hue_sat=hsm,tone=[0,0,.5,.8,1,1],look=hsm,baseline=1.88)
    for source in (base,decorated):
        md,pd=run(source)
        c=md['color'];assert c['recipe_effective']=='generic'
        assert c['profile_correction_effective']=='hue-sat-map'
        assert c['hue_sat_map_applied']==(source==decorated)
        assert c['hue_sat_map_status']==('applied' if source==decorated else 'absent')
        assert not c['tone_curve_applied'] and not c['look_table_applied'] and not c['baseline_exposure_applied']
        ma,_=run(source,analysis=True);assert ma['color']==c
        me,pe=run(source,2)
        assert me['color']==c
        assert all(abs(a-b*4)<2e-6*max(1,abs(a)) for a,b in zip(pe,pd))
    assert run(decorated)[1]!=run(base)[1]
    # No opt-out or obsolete output-specific recipe is accepted.
    for policy in ('none','hue-sat-map'):
        run(decorated,extra=['--profile-correction',policy],code=2)
    run(decorated,extra=['--camera-recipe','dji-dlog'],code=2)
print('PASS universal HSM present/absent, FM/fallback, analysis parity, EV and policy rejection')
