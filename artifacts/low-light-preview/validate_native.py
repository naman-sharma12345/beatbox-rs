#!/usr/bin/env python3
"""Fail closed on native load errors or silently ignored authored fields."""
import json, math, pathlib, subprocess, sys
root = pathlib.Path(__file__).resolve().parent
source = root / 'Low_Light_Preview.beatbox.json'
p = subprocess.run([str(pathlib.Path(sys.argv[1]).resolve()), 'call', 'get_project', '{"detail":"full"}', '--project', str(source)], text=True, capture_output=True, timeout=45)
(root/'ci-logs').mkdir(exist_ok=True)
(root/'ci-logs/native-load.stderr.txt').write_text(p.stderr)
p.check_returncode()
loaded = json.loads(p.stdout)
(root/'ci-logs/native-loaded-project.json').write_text(json.dumps(loaded, indent=2)+'\n')
def compare(a, b, path='$'):
    if isinstance(a, dict):
        assert isinstance(b, dict), path
        for k,v in a.items():
            assert k in b, f'{path}.{k}: authored field dropped'
            compare(v, b[k], f'{path}.{k}')
    elif isinstance(a, list):
        assert isinstance(b, list) and len(a)==len(b), f'{path}: list changed'
        for i,(v,w) in enumerate(zip(a,b)): compare(v,w,f'{path}[{i}]')
    elif isinstance(a,(int,float)) and not isinstance(a,bool):
        assert type(b) in (int,float) and math.isclose(a,b,rel_tol=2e-6,abs_tol=2e-6), f'{path}: {a!r} became {b!r}'
    else:
        assert a==b, f'{path}: {a!r} became {b!r}'
expected=json.loads(source.read_text())
# Note serde explicitly omits these defaults; their absence is not field loss.
for pattern in expected['patterns']:
    for notes in pattern['clips'].values():
        for note in notes:
            for key, default in [('prob',1),('offset',0),('slide_to',None),('id','')]:
                if key in note and note[key]==default: del note[key]
# SynthParams intentionally omits these explicit disabled fields on serialization.
for track in expected['tracks']:
    if track['instrument']['type']=='synth':
        for key in ['drift_cents','vel_to_cutoff']:
            if track['instrument'].get(key)==0:
                if key not in next(t for t in loaded['tracks'] if t['name']==track['name'])['instrument']:
                    del track['instrument'][key]
compare(expected, loaded)
print('Native load and all authored-field round-trip checks passed. This is not an audition.')
