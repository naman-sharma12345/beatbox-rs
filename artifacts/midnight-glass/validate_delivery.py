#!/usr/bin/env python3
"""Technical gates only, never a claim of subjective audition."""
import json, math, pathlib
root=pathlib.Path(__file__).resolve().parent
q=json.loads((root/'audio-qc.json').read_text())
for name in ['Midnight_Glass.wav','Midnight_Glass.mp3']:
    v=q[name]; p=v['probe']; a=[s for s in p['streams'] if s['codec_type']=='audio']
    assert len(a)==1 and a[0]['channels']==2 and a[0]['sample_rate']=='44100', name
    assert 123<=float(p['format']['duration'])<=125, name
    loud=float(v['measurement']['input_i']); peak=float(v['measurement']['input_tp'])
    assert math.isfinite(loud) and -14<=loud<=-10, f'{name}: LUFS {loud}'
    assert math.isfinite(peak) and peak<=-1, f'{name}: true peak {peak}'
    assert v['full_decode']=='passed' and (root/name).stat().st_size>100000, name
print('WAV/MP3 format, duration, non-silence via finite loudness, loudness, true-peak and decode gates passed. No subjective audition performed.')
