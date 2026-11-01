#!/usr/bin/env python3
"""Fail-closed technical gates. Passing is not musical approval."""
import hashlib,json,math,pathlib
root=pathlib.Path(__file__).resolve().parent
q=json.loads((root/'audio-qc.json').read_text())
expected=8*4*60/92+3
for name in ['Low_Light_Preview.raw.wav','Low_Light_Preview.wav','Low_Light_Preview.mp3']:
 v=q[name];p=v['probe'];a=[s for s in p['streams'] if s['codec_type']=='audio']
 assert len(a)==1 and a[0]['channels']==2 and a[0]['sample_rate']=='44100',name
 assert abs(float(p['format']['duration'])-expected)<.12,(name,'duration')
 loud=float(v['measurement']['input_i']);peak=float(v['measurement']['input_tp'])
 assert math.isfinite(loud) and math.isfinite(peak),(name,'non-finite/silent')
 assert v['full_decode']=='passed' and (root/name).stat().st_size>100000,name
 assert hashlib.sha256((root/name).read_bytes()).hexdigest()==v['sha256'],name
 if '.raw.' in name:
  assert peak<-.1,(name,'raw headroom',peak)
 else:
  assert -16<=loud<=-12,(name,'LUFS',loud)
  assert peak<=-1,(name,'post-export true peak',peak)
  assert a[0]['codec_name']==('mp3' if name.endswith('.mp3') else 'pcm_s16le'),name
for name in ['Low_Light_Preview.raw.wav','Low_Light_Preview.wav']:
 pcm=q[name]['pcm']
 assert pcm['clipped_samples']==0,(name,'clipped PCM')
 assert pcm['final_100ms_rms']<.001 and pcm['final_sample_peak']<.001,(name,'tail not quiet')
c=q['comparisons'];mono=float(c['mono']['input_i']);stereo=float(q['Low_Light_Preview.wav']['measurement']['input_i'])
assert math.isfinite(mono) and mono>=stereo-6,('gross mono loss',mono,stereo)
for half in ['hook','response']:assert math.isfinite(float(c[half]['input_i'])),half
assert q['provenance']['project_sha256']==hashlib.sha256((root/'Low_Light_Preview.beatbox.json').read_bytes()).hexdigest()
print('PASS: hashes, format, duration, finite non-silence, raw headroom, WAV/MP3 loudness and true peak, full decode, gross mono-loss screen, zero clipped PCM samples and quiet-tail screen. No subjective audition; no claim about groove/timbre/tail quality.')
