#!/usr/bin/env python3
"""Render only using a supplied Beatbox CLI; no install, build or network."""
import array,hashlib,json,math,os,pathlib,shutil,subprocess,sys,wave
root=pathlib.Path(__file__).resolve().parent
binary=pathlib.Path(sys.argv[1]).resolve() if len(sys.argv)==2 else None
if binary is None or not binary.is_file() or not os.access(binary,os.X_OK):
 raise SystemExit('Supply an existing executable Beatbox CLI. Nothing was rendered.')
for c in ['ffmpeg','ffprobe']:
 if not shutil.which(c):raise SystemExit(f'Existing {c} required; no software installed.')
scratch=root.parent.parent/'.openclaw/tmp/low-light-preview-render';scratch.mkdir(parents=True,exist_ok=True)
source=root/'Low_Light_Preview.beatbox.json';project=scratch/'render-project.json';shutil.copyfile(source,project)
raw=root/'Low_Light_Preview.raw.wav';wav=root/'Low_Light_Preview.wav';mp3=root/'Low_Light_Preview.mp3'
def run(args,log=None):
 p=subprocess.run([str(x) for x in args],stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,timeout=180)
 if log:log.write_text(p.stdout+'\n'+p.stderr)
 p.check_returncode();return p
def stats(p):return json.loads(p.stderr[p.stderr.rfind('{'):p.stderr.rfind('}')+1])
def measure(path,extra=None,log=None):
 filt=((extra+',') if extra else '')+'loudnorm=I=-14:TP=-2.5:LRA=9:print_format=json'
 return stats(run(['ffmpeg','-nostdin','-hide_banner','-i',path,'-af',filt,'-f','null','-'],log))
run([binary,'call','render',json.dumps(dict(path=str(raw),tail=3)), '--project',project],scratch/'engine-render.log')
if not raw.is_file() or raw.stat().st_size<1000:raise SystemExit('No expected engine WAV produced.')
s=measure(raw,log=scratch/'normalization-pass1.log')
for k in ['input_i','input_tp','input_lra','input_thresh','target_offset']:
 if not math.isfinite(float(s[k])):raise SystemExit('Non-finite raw measurement; stop before export.')
filt=('loudnorm=I=-14:TP=-2.5:LRA=9:linear=true:print_format=json:'+':'.join(f'{a}={s[b]}' for a,b in [('measured_I','input_i'),('measured_TP','input_tp'),('measured_LRA','input_lra'),('measured_thresh','input_thresh'),('offset','target_offset')]))
run(['ffmpeg','-nostdin','-hide_banner','-y','-i',raw,'-af',filt,'-ar','44100','-c:a','pcm_s16le',wav],scratch/'normalization-pass2.log')
run(['ffmpeg','-nostdin','-hide_banner','-y','-i',wav,'-c:a','libmp3lame','-b:a','320k','-metadata','title=Low Light Preview','-metadata','genre=Hip-Hop',mp3],scratch/'mp3-encode.log')
qc={}
for path in [raw,wav,mp3]:
 probe=json.loads(run(['ffprobe','-v','error','-show_streams','-show_format','-of','json',path]).stdout)
 run(['ffmpeg','-nostdin','-v','error','-i',path,'-f','null','-'])
 qc[path.name]=dict(probe=probe,measurement=measure(path),full_decode='passed',sha256=hashlib.sha256(path.read_bytes()).hexdigest())
# Source write_wav_stream emits stereo signed 16-bit PCM. Inspect samples with stdlib.
for path in [raw,wav]:
 with wave.open(str(path),'rb') as w:
  if w.getsampwidth()!=2:raise SystemExit('Expected source-contract 16-bit PCM.')
  x=array.array('h',w.readframes(w.getnframes()))
 if sys.byteorder!='little':x.byteswap()
 tail=x[-8820:]
 qc[path.name]['pcm']=dict(clipped_samples=sum(abs(v)>=32767 for v in x),final_100ms_rms=math.sqrt(sum((v/32768)**2 for v in tail)/len(tail)),final_sample_peak=max(abs(v)/32768 for v in x[-2:]))
qc['comparisons']={}
for label,f in [('hook','atrim=start=0:end=10.434782609,asetpts=PTS-STARTPTS'),('response','atrim=start=10.434782609:end=20.869565217,asetpts=PTS-STARTPTS'),('mono','pan=mono|c0=0.5*c0+0.5*c1')]:
 qc['comparisons'][label]=measure(wav,f)
qc['provenance']=dict(engine='Beatbox-rs supplied CLI',engine_source='f554d15f8ed978188bda0de4fcdf84244518e75c',engine_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),project_sha256=hashlib.sha256(source.read_bytes()).hexdigest(),body_seconds=8*4*60/92,tail_seconds=3,note='All synthesis and project FX by Beatbox. FFmpeg only exports and measures. No subjective audition; halves are reported separately, not perceptually judged. No automatic rerender/retry.')
(root/'audio-qc.json').write_text(json.dumps(qc,indent=2)+'\n')
print(json.dumps({k:{'seconds':v['probe']['format']['duration'],'LUFS':v['measurement']['input_i'],'dBTP':v['measurement']['input_tp']} for k,v in qc.items() if 'probe' in v},indent=2))
# Final decision belongs to validate_delivery.py; a QC failure never hides generated files.
