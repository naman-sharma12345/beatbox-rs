#!/usr/bin/env python3
"""Render this composition with an existing Beatbox-rs binary. No builds/install/network."""
import hashlib,json,os,pathlib,shutil,subprocess,sys
root=pathlib.Path(__file__).resolve().parent
binary=pathlib.Path(sys.argv[1]).resolve() if len(sys.argv)>1 else None
if binary is None or not binary.is_file() or not os.access(binary,os.X_OK):
 raise SystemExit('Requires an existing executable Beatbox-rs Linux CLI as its sole argument. No audio has been rendered.')
for c in ['ffmpeg','ffprobe']:
 if not shutil.which(c):raise SystemExit(f'Existing {c} is required; this script does not install software.')
scratch=root.parent.parent/'.openclaw/tmp/midnight-glass-render';scratch.mkdir(parents=True,exist_ok=True)
source=root/'Midnight_Glass.beatbox.json';project=scratch/'render-project.json';shutil.copyfile(source,project)
raw=scratch/'beatbox-raw.wav';wav=root/'Midnight_Glass.wav';mp3=root/'Midnight_Glass.mp3'
def run(args,log=None):
 p=subprocess.run([str(x) for x in args],stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,check=True)
 if log:log.write_text(p.stdout+'\n'+p.stderr)
 return p
# Only Beatbox-rs creates the notes, instruments, arrangement, routing and effects.
r=run([binary,'call','render',json.dumps(dict(path=str(raw),tail=4)), '--project',project],scratch/'engine-render.log')
if not raw.is_file() or raw.stat().st_size<1000:raise SystemExit('Beatbox did not produce the expected WAV.')
filt='loudnorm=I=-12:TP=-1.5:LRA=9:print_format=json'
r=run(['ffmpeg','-nostdin','-hide_banner','-i',raw,'-af',filt,'-f','null','-'],scratch/'normalization-pass1.log')
stats=json.loads(r.stderr[r.stderr.rfind('{'):r.stderr.rfind('}')+1])
filt=('loudnorm=I=-12:TP=-1.5:LRA=9:linear=true:print_format=json:'+':'.join(f'{a}={stats[b]}' for a,b in [('measured_I','input_i'),('measured_TP','input_tp'),('measured_LRA','input_lra'),('measured_thresh','input_thresh'),('offset','target_offset')]))
run(['ffmpeg','-nostdin','-hide_banner','-y','-i',raw,'-af',filt,'-ar','44100','-c:a','pcm_s16le',wav],scratch/'normalization-pass2.log')
run(['ffmpeg','-nostdin','-hide_banner','-y','-i',wav,'-c:a','libmp3lame','-b:a','320k','-metadata','title=Midnight Glass','-metadata','genre=Melodic Trap',mp3],scratch/'mp3-encode.log')
qc={}
for path in [wav,mp3]:
 probe=json.loads(run(['ffprobe','-v','error','-show_streams','-show_format','-of','json',path]).stdout)
 run(['ffmpeg','-nostdin','-v','error','-i',path,'-f','null','-'])
 r=run(['ffmpeg','-nostdin','-hide_banner','-i',path,'-af','loudnorm=I=-12:TP=-1:LRA=9:print_format=json','-f','null','-'])
 loud=json.loads(r.stderr[r.stderr.rfind('{'):r.stderr.rfind('}')+1])
 qc[path.name]=dict(probe=probe,measurement=loud,full_decode='passed',sha256=hashlib.sha256(path.read_bytes()).hexdigest())
qc['provenance']=dict(engine='Beatbox-rs, supplied prebuilt executable',engine_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),project_sha256=hashlib.sha256(source.read_bytes()).hexdigest(),note='All sound generation and project FX by Beatbox-rs; FFmpeg used only for loudness normalization, WAV/MP3 delivery and technical measurements. No subjective audition performed.')
(root/'audio-qc.json').write_text(json.dumps(qc,indent=2)+'\n')
peak=float(qc[mp3.name]['measurement']['input_tp'])
if peak>-1.0:raise SystemExit(f'Files rendered, but delivery QC failed: MP3 true peak {peak} dBTP exceeds -1 dBTP. Revise normalization ceiling before delivery.')
print(json.dumps({k:{'seconds':v['probe']['format']['duration'],'LUFS':v['measurement']['input_i'],'dBTP':v['measurement']['input_tp']} for k,v in qc.items() if k!='provenance'},indent=2))
