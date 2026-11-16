"""Controlled optical truth; actual SDK, production CLI and FFmpeg, no fake pixels.
This synthetic time sequence proves fixed crop behavior, not real-camera calibration.
"""
import array,hashlib,json,pathlib,re,subprocess,sys
sys.dont_write_bytecode=True
cli,native,work=map(lambda p:pathlib.Path(p).resolve(),sys.argv[1:])
repo=pathlib.Path(__file__).resolve().parents[2]
sys.path.insert(0,str(repo/'native/dng-linear-frame/tests'))
from dng_fixture import make_dng
incoming=work/'input';incoming.mkdir()
for i in range(3):make_dng(incoming,f'frame_{i}',w=96,h=64,white=4000,pixels=[700,900,500]*(96*64),datetime_original=f'2026:11:15 12:00:0{i}')
originals={p.name:hashlib.sha256(p.read_bytes()).hexdigest() for p in incoming.iterdir()}
metadata=work/'metadata-tool'
metadata.write_text('''#!/usr/bin/python3
import html,pathlib,sys
print('<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">')
for arg in sys.argv[1:]:
 p=pathlib.Path(arg)
 if p.suffix.lower()!='.dng':continue
 focal=10+10*int(p.stem.rsplit('_',1)[1])
 print('<rdf:Description rdf:about="'+html.escape(str(p),quote=True)+'" xmlns:IFD0="http://ns.exiftool.org/EXIF/IFD0/1.0/" xmlns:ExifIFD="http://ns.exiftool.org/EXIF/ExifIFD/1.0/" xmlns:XMP-aux="http://ns.exiftool.org/XMP/XMP-aux/1.0/">')
 print('<IFD0:Make>Test</IFD0:Make><IFD0:Model>LinearProbe</IFD0:Model><ExifIFD:LensModel>Known Lens</ExifIFD:LensModel><ExifIFD:FNumber>4</ExifIFD:FNumber>')
 print('<ExifIFD:FocalLength>'+str(focal)+'</ExifIFD:FocalLength><ExifIFD:FocalLengthIn35mmFormat>'+str(focal)+'</ExifIFD:FocalLengthIn35mmFormat>')
 for key in ['VignetteCorrectionAlreadyApplied','DistortionCorrectionAlreadyApplied','LateralChromaticAberrationCorrectionAlreadyApplied']:print('<XMP-aux:'+key+'>False</XMP-aux:'+key+'>')
 print('</rdf:Description>')
print('</rdf:RDF>')
''');metadata.chmod(0o755)
profiles=work/'profiles';profiles.mkdir();rows=[]
for focal,scale in [(10,1.03),(30,1.08)]:
 rows.append(f'''<rdf:li><rdf:Description c:Make="Test" c:Model="LinearProbe" c:Lens="Known Lens" c:CameraRawProfile="True" c:SensorFormatFactor="1" c:ImageWidth="96" c:ImageLength="64" c:FocalLength="{focal}" c:ApertureValue="4" c:FocusDistance="10"><c:PerspectiveModel c:Version="2" c:FocalLengthX="1" c:FocalLengthY="1" c:ScaleFactor="{scale}"><c:VignetteModel c:FocalLengthX="1" c:FocalLengthY="1" c:VignetteModelParam1="-.3"/><c:ChromaticRedGreenModel c:FocalLengthX="1" c:FocalLengthY="1" c:ScaleFactor="1.01"/><c:ChromaticGreenModel c:FocalLengthX="1" c:FocalLengthY="1"/><c:ChromaticBlueGreenModel c:FocalLengthX="1" c:FocalLengthY="1" c:ScaleFactor="1.02"/></c:PerspectiveModel></rdf:Description></rdf:li>''')
(profiles/'known.lcp').write_text('<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:c="http://ns.adobe.com/photoshop/1.0/camera-profile"><rdf:Seq>'+''.join(rows)+'</rdf:Seq></rdf:RDF>')
ffmpeg='/opt/homebrew/opt/ffmpeg-full/bin/ffmpeg';ffprobe='/opt/homebrew/opt/ffmpeg-full/bin/ffprobe'
receipts=[]
def command(argv,label,expected=0):
 p=subprocess.run(list(map(str,argv)),stdout=subprocess.PIPE,stderr=subprocess.PIPE)
 (work/(label+'.stdout')).write_bytes(p.stdout);(work/(label+'.stderr')).write_bytes(p.stderr)
 receipts.append({'argv':list(map(str,argv)),'exit_code':p.returncode,'label':label})
 (work/'commands.json').write_text(json.dumps(receipts,indent=2)+'\n')
 assert p.returncode==expected,(label,p.returncode,p.stderr.decode(errors='replace'))
 return p.stdout
for label,crop,sampler,resize in [('preserve','preserve','bilinear',True),('safe','safe','bilinear',True),('sequence-quality','sequence-safe','quality',True),('source-quality','sequence-safe','quality',False)]:
 video=work/(label+'.mkv')
 args=[cli,'--input',incoming,'--output',video,'--dng-linear-frame',native,'--ffmpeg',ffmpeg,'--ffprobe',ffprobe,'--strength','0','--camera-color','generic','--encoder','ffv1','--container','matroska','--pix-fmt','yuv444p10le','--lens-correction','auto','--exiftool',metadata,'--lens-profile-dir',profiles,'--lens-vignette','off','--lens-resampling',sampler,'--lens-crop',crop,'--verify-output']
 if resize:args+=['--resize','128x72']
 command(args,label)
 log=video.with_suffix('.log').read_text()
 crops=[line for line in log.splitlines() if 'kind=lens.crop ' in line]
 reports=[json.loads(line.split(' report=',1)[1]) for line in log.splitlines() if 'kind=lens frame=' in line]
 assert len(reports)==3 and all(r['resampling']==sampler and r['components'][0]['status']=='off' for r in reports)
 assert all([c['status'] for c in r['components'][1:]]==['applied','applied'] for r in reports)
 width,height=(128,72) if resize else (96,64)
 probe=json.loads(command([ffprobe,'-v','error','-count_frames','-show_entries','stream=width,height,nb_read_frames','-of','json',video],label+'-probe'))['streams']
 assert len(probe)==1 and (probe[0]['width'],probe[0]['height'],probe[0]['nb_read_frames'])==(width,height,'3')
 data=command([ffmpeg,'-v','error','-i',video,'-pix_fmt','rgb48le','-f','rawvideo','-'],label+'-decode')
 assert len(data)==3*width*height*6
 values=array.array('H');values.frombytes(data)
 zeros=sum(sum(values[i:i+3])<768 for i in range(0,len(values),3))
 if crop=='preserve':
  assert not crops and zeros>0,'preserve unexpectedly hid invalid borders'
 else:
  assert len(crops)==3 and len({re.search(r'window=([^ ]+)',line).group(1) for line in crops})==1,'per-frame crop breathing'
  assert zeros==0,'safe crop leaked lens-introduced invalid pixels into video'
  frame_size=width*height*3
  assert max(abs(int(values[i])-int(values[i%frame_size])) for i in range(frame_size,len(values)))<=256,'constant scene changed across geometry sequence'
 assert all(hashlib.sha256(p.read_bytes()).hexdigest()==originals[p.name] for p in incoming.iterdir())
folded=work/'folded-profiles';folded.mkdir();(folded/'known.lcp').write_text((profiles/'known.lcp').read_text().replace('<c:PerspectiveModel ','<c:PerspectiveModel c:RadialDistortParam1="-8" '))
controlled=work/'vignette-only.mkv'
args=[cli,'--input',incoming,'--output',controlled,'--dng-linear-frame',native,'--ffmpeg',ffmpeg,'--ffprobe',ffprobe,'--strength','0','--camera-color','generic','--encoder','ffv1','--container','matroska','--pix-fmt','yuv444p10le','--lens-correction','auto','--exiftool',metadata,'--lens-profile-dir',folded,'--lens-vignette','auto','--lens-vignette-strength','.5','--lens-distortion','off','--lens-ca','off','--resize','128x72','--verify-output']
command(args,'disabled-folded-geometry')
reports=[json.loads(line.split(' report=',1)[1]) for line in controlled.with_suffix('.log').read_text().splitlines() if 'kind=lens frame=' in line]
assert len(reports)==3 and all([c['status'] for c in r['components']]==['applied','off','off'] for r in reports)
active=list(args);active[active.index(str(controlled)) if str(controlled) in active else active.index(controlled)]=work/'invalid-active.mkv'
active[active.index('--lens-distortion')+1]='auto'
command(active,'active-folded-geometry',30)
assert not (work/'invalid-active.mkv').exists()
print('PASS actual SDK/CLI/FFmpeg controls, RGB-safe fixed sequence crop, resize/source geometry, 12 decoded frames and source protection')
