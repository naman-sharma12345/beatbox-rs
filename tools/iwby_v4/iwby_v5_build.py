#!/usr/bin/env python3
"""IWBY v5 end to end from the sung take (B53, 2026-10-14): cover_song -> the v3/cc4/v4b project edits ->
export_stems with v5's stem gains PINNED -> iwby_post.py.

    python3 iwby_v5_build.py BEATBOX_BIN TAKE.wav WORKDIR OUT.wav [--sibilance-band [THRESHOLD_DB]]

TAKE.wav is take_dn2 (not in the repo). Without options this rebuilds the canonical IWBY v5 render
(sha256 4c42f368...90441 on tab/frontier). --sibilance-band asks cover_song for its opt-in 7 kHz band
(default threshold -27 dB) and keeps it LAST on the vocal chain after this recipe's vocal edits; nothing
else changes.

Why the stem gains are pinned (params_v5_stems.json): export_stems' default trims each stem until its own
decoded true peak meets 0 dBTP, so a stem's level follows its own peak. v5's vocal (+12 dB fader) was
turned down 7.05 dB that way, in 4 trim steps. A change that lowers the vocal's peaks (de-essing) then
made the trimmed vocal 2.35 dB louder and broke iwby_post's phrase split (12 phrases, not 20). The pins
are the exact steps the trim took for v5 (export_stems reports them as stem_gain_db_pin), so v5 rebuilds
bit for bit and a variant keeps v5's stem levels, whatever its peaks do.
Determinism: the engine and iwby_post.py are bit-exact for the same binary and numpy/scipy build.
"""
import json, os, shutil, subprocess, sys

HERE = os.path.dirname(os.path.abspath(__file__))
if len(sys.argv) < 5:
    sys.exit(__doc__)
BIN, TAKE, WD, OUT = (os.path.abspath(x) for x in sys.argv[1:5])
SIB = None
if '--sibilance-band' in sys.argv:
    i = sys.argv.index('--sibilance-band')
    SIB = float(sys.argv[i + 1]) if len(sys.argv) > i + 1 else -27.0
P = json.load(open(os.path.join(HERE, 'params_v5_build.json')))
PINS = json.load(open(os.path.join(HERE, 'params_v5_stems.json')))['stem_gain_db']
PROJ = os.path.join(WD, 'proj.json'); STEMS = os.path.join(WD, 'stems'); EW = os.path.join(WD, 'engine')
shutil.rmtree(WD, ignore_errors=True); os.makedirs(EW)
LOG = open(os.path.join(WD, 'build.log'), 'w')

def call(tool, x, timeout=290):
    r = subprocess.run([BIN, '--workdir', EW, 'call', tool, json.dumps(x), '--project', PROJ], capture_output=True, text=True, timeout=timeout)
    if r.returncode != 0 or r.stdout.startswith(' Error'):
        sys.exit(f'{tool} failed: {r.stdout[:400]} {r.stderr[-400:]}')
    print(tool, r.stdout[:200].replace('\n', ' '), file=LOG, flush=True)
    return r.stdout

def load(): return json.load(open(PROJ))
def save(p): json.dump(p, open(PROJ, 'w'))

# 1. cover_song (the v1 args with v5's take, key and name)
cs = dict(P['cover_song'], vocal=TAKE, out=os.path.join(WD, 'base.wav'))
if SIB is not None:
    cs.update(sibilance_band=True, sibilance_band_threshold_db=SIB)
call('produce', {'action': 'cover_song', 'params': cs})
# 2. build_v3 edits (organ_db -24, drums +6 dB, vox shelves)
call('design_sound', {'track': 'organ', 'description': 'warm vintage organ pad with slow chorus, soft'})
for pat in load()['patterns']:
    cn = pat['clips'].get('comp', [])
    if not cn: continue
    notes = []
    for w0 in range(0, pat['bars'] * 16, 8):
        for pc in sorted({n['pitch'] % 12 for n in cn if w0 <= n['start'] < w0 + 8}):
            notes.append({'start': w0, 'len': 7.9, 'pitch': int(55 + ((pc - 55) % 12)), 'vel': 0.45})
    call('add_notes', {'pattern': pat['name'], 'track': 'organ', 'notes': notes, 'replace': True})
call('set_send', {'track': 'organ', 'bus': 'hall', 'db': -6})
call('add_effect', {'track': 'vox', 'type': 'saturator', 'params': {'mode': 'tape', 'drive_db': 4.0, 'mix': 0.35}})
call('add_effect', {'track': 'vox', 'type': 'parametric_eq', 'params': {'bands': [{'kind': 'high_shelf', 'freq': 11000, 'gain_db': 3.5, 'q': 0.7}, {'kind': 'high_shelf', 'freq': 7000, 'gain_db': 2.0, 'q': 0.7}]}})
call('set_send', {'track': 'vox', 'bus': 'vdelay', 'db': -19})
for t in ['comp', 'comp2', 'drums']:
    call('add_effect', {'track': t, 'type': 'parametric_eq', 'params': {'bands': [{'kind': 'high_shelf', 'freq': 7000, 'gain_db': 4.0, 'q': 0.7}]}})
for t in ['comp', 'comp2']:
    call('add_effect', {'track': t, 'type': 'chorus', 'params': {'rate_hz': 0.6, 'depth_ms': 6.0, 'mix': 0.3}})
p = load()
for t in p['tracks']:
    if t['name'] == 'organ': t['volume_db'] = -24
    for f in t['effects']:
        if f['type'] == 'dynamic_eq' and f.get('source') == 'vox': f['release_ms'] = 180.0
for l in p['automation']:
    if l['target'] == 'drums':
        for pt in l['points']: pt['value'] = round(pt['value'] + 6, 2)
save(p)
# 3. cc4 edits (exciters, drum top, drums -2 / comps +2 dB)
call('add_effect', {'track': 'vox', 'type': 'saturator', 'params': {'mode': 'exciter', 'tone_hz': 5000, 'drive_db': 8.0, 'mix': 0.35}})
for t in ['comp', 'comp2']:
    call('add_effect', {'track': t, 'type': 'saturator', 'params': {'mode': 'exciter', 'tone_hz': 4000, 'drive_db': 8.0, 'mix': 0.4}})
call('add_effect', {'track': 'drums', 'type': 'parametric_eq', 'params': {'bands': [{'kind': 'high_shelf', 'freq': 6000, 'gain_db': 4.0, 'q': 0.7}]}})
p = load()
for l in p['automation']:
    if l['target'] == 'drums':
        for pt in l['points']: pt['value'] = round(pt['value'] - 2.0, 2)
    if l['target'] in ('comp', 'comp2'):
        for pt in l['points']: pt['value'] = round(pt['value'] + 2.0, 2)
save(p)
# 4. v4b's later values, applied by role (not by effect index)
p = load(); TR = {t['name']: t for t in p['tracks']}
def last(t, pred): return [f for f in TR[t]['effects'] if pred(f)][-1]
last('drums', lambda f: f['type'] == 'parametric_eq' and f['bands'][0].get('freq') == 6000.0)['bands'][0]['gain_db'] = 6.0
for t in ['comp', 'comp2']:
    f = last(t, lambda f: f['type'] == 'saturator' and f.get('mode') == 'exciter'); f['drive_db'] = 10.0; f['mix'] = 0.75
last('vox', lambda f: f['type'] == 'parametric_eq' and f['bands'][0].get('freq') == 11000.0)['bands'][0]['gain_db'] = 4.0
f = last('vox', lambda f: f['type'] == 'saturator' and f.get('mode') == 'exciter'); f['drive_db'] = 10.0; f['mix'] = 0.65
# 5. the opt-in sibilance band stays LAST on the vocal chain (after this recipe's shelves and exciter)
if SIB is not None:
    vx = TR['vox']['effects']
    band = [k for k, f in enumerate(vx) if f['type'] == 'dynamic_eq' and not f.get('source') and f.get('freq') == 7000.0 and f.get('q') == 2.0]
    if len(band) != 1: sys.exit(f'expected one sibilance band on vox, found {len(band)}')
    vx.append(vx.pop(band[0]))
save(p)
# 6. stems at v5's pinned gains, then the IWBY post-process
r = json.loads(call('export_stems', {'dir': STEMS, 'format': 'wav', 'mode': 'pre_master', 'stem_gain_db': PINS}))
json.dump(r, open(os.path.join(WD, 'stems.json'), 'w'), indent=1)
over = [s['name'] for s in r['stems'] if s.get('over_ceiling')]
if over: print('note: over 0 dBTP at the pinned gain:', over, file=LOG, flush=True)
post = subprocess.run([sys.executable, os.path.join(HERE, 'iwby_post.py'), STEMS, OUT, PROJ], capture_output=True, text=True, timeout=290)
print(post.stdout[-1500:], post.stderr[-1500:], file=LOG, flush=True)
if post.returncode != 0: sys.exit(f'iwby_post failed: {post.stderr[-600:]}')
shutil.rmtree(os.path.join(EW, '.beatbox_cache'), ignore_errors=True)
if not os.environ.get('IWBY_KEEP_STEMS'):  # IWBY_KEEP_STEMS=1 keeps WORKDIR/stems for measuring
    shutil.rmtree(STEMS, ignore_errors=True)
print('wrote', OUT)
