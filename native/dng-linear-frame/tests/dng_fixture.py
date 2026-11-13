"""Generated DNG fixtures shared by the native tool's integration tests.

Python is test infrastructure only; the installed native tool does not use it.
"""
import pathlib,struct
from fractions import Fraction


def blob(typ,values):
    if typ in (1,7):return bytes(values),len(values)
    if typ==2:return values.encode()+b'\0',len(values)+1
    if typ in (3,4):return struct.pack('<'+('H' if typ==3 else 'I')*len(values),*values),len(values)
    if typ==11:return struct.pack('<'+'f'*len(values),*values),len(values)
    if typ in (5,10):
        vals=[Fraction(str(x)).limit_denominator(1000000000) for x in values]
        return b''.join(struct.pack('<'+('ii' if typ==10 else 'II'),v.numerator,v.denominator) for v in vals),len(vals)
    raise AssertionError(typ)


# 16x16 is small enough for checked-in generator-only regression coverage.
def make_dng(work,name,*,pi=34892,black=None,white=1000,pixels=None,table=None,baseline=0,opcodes=None,ab=None,cc=None,neutral=None,w=16,h=16,datetime_original=None,exif_case=None,forward=None,model=None,hue_sat=None):
    work=pathlib.Path(work)
    if pixels is None:pixels=[250,500,750]*(w*h)
    payload=struct.pack('<'+'H'*len(pixels),*pixels)
    tags={254:(4,[0]),256:(4,[w]),257:(4,[h]),258:(3,[16]*3),259:(3,[1]),271:(2,'Test'),272:(2,'LinearProbe'),273:(4,[0]),274:(3,[1]),277:(3,[3]),278:(4,[h]),279:(4,[len(payload)]),284:(3,[1]),339:(3,[1]*3),50706:(1,[1,4,0,0]),50707:(1,[1,3,0,0]),50708:(2,'Controlled LinearRaw Probe'),50713:(3,[2,2] if black else [1,1]),50714:(5,black if black else [0,0,0]),50717:(4,[white]*3),50718:(5,[1,1]),50719:(4,[0,0]),50720:(4,[w,h]),50721:(10,[1.843,-1.2288,.1003,-.5888,1.5271,.3387,-.0001,.0219,1.0795]),50728:(5,neutral or ['.6050808314',1,'.4825046041']),50730:(10,[baseline]),50778:(3,[17]),50829:(4,[0,0,h,w])}
    if model is not None:tags[50708]=(2,model)
    if forward is not None:tags[50964]=(10,forward)
    if hue_sat is not None:
        dims,data=hue_sat
        tags[50937]=(4,dims);tags[50938]=(11,data)
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
