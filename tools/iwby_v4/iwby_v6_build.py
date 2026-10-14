#!/usr/bin/env python3
"""IWBY v6 end to end from the sung take (B54, 2026-10-14): the v5 recipe (iwby_v5_build.py, frozen) with
the opt-in 7 kHz sibilance band ON (Naman approved it by ear, 2026-10-14 1:43 PM) and CLEAN STEMS: export_stems
writes 32-bit float stems (stem_format "f32") at v5's pinned gains. Nothing else changes.

    python3 iwby_v6_build.py BEATBOX_BIN TAKE.wav WORKDIR OUT.wav [--sibilance-band THRESHOLD_DB | --no-sibilance-band] [--int-exact | --f32-tp]

The band is cover_song's sibilance_band (7 kHz bell, Q2, 4:1, at most -4 dB, threshold -27 dB), kept LAST on the
vocal chain, exactly as iwby_v5_build.py --sibilance-band builds it (B53's d4c45e57). --no-sibilance-band builds
v5 + clean stems only (to measure the stems on their own).

Why: v5's stems are 24-bit, and its vocal stem at the pinned gain (-7.05 dB, the 4 trims export_stems' default
took) still sits at +0.85 dBTP with 382 samples hard clipped at full scale (the default trim reads the decoded
peak, which a clipped decode under-reads, and gives up after 4 trims). v5 stays locked as it is (4c42f368,
rebuilt by iwby_v5_build.py). v6 keeps every v5 gain, so the vocal level and the phrase split are v5's, and
writes the stems as float, so nothing over 0 dBFS is clipped: the clipped vocal peaks come back, nothing else
moves (float stems also carry no dither). iwby_post.py reads stems with soundfile as float32, so values over
1.0 pass through unchanged. The build fails if any stem reports clipped samples or a warning.
--int-exact (opt-in alternative, not v6): 24-bit stems with export_stems' exact_trim (to 0.1 dB under 0 dBTP) and no pins.
The vocal stem then meets -0.10 dBTP with nothing clipped, but the trim takes it down 9.09 dB instead of v5's 7.05, so it sits
2.0 dB lower against an unchanged backing; measured (B54): dry vocal -2.0 dB in iwby_post, 20 phrases but 7 boundaries
move more than 0.02 s (up to 0.35 s), the throw windows move up to 0.14 s, throw energy -2.0..+0.4 dB, total wet -0.6..-2.0 dB.
sha256 a3fd9cca...a91126. v6 keeps float stems at v5's gains (Naman's call, 2026-10-14 2:12 PM).
--f32-tp (opt-in, not v6; Naman approved 2026-10-14 10:19 PM): float stems at v5's pinned gains, then export_stems'
tp_trim "common": ONE extra trim for every stem, set by the hottest stem's true peak read back from its file, aiming
-0.1 dBTP, until every stem is at or under 0.0 dBTP (12 trims at most). The stem-to-stem balance stays v6's exactly.
The build fails unless the trim converged with every stem under 0 dBTP and no warnings; its report is WORKDIR/stems.json
(tp_trim block). iwby_post re-levels the master, so the trim mostly washes out there. Name the output
beatbox_iwby_v6_f32tp.wav. Without the flag nothing changes (v6 stays 8e024845...).
"""
import json, os, shutil, subprocess, sys

HERE = os.path.dirname(os.path.abspath(__file__))
if len(sys.argv) < 5:
    sys.exit(__doc__)
BIN, TAKE, WD, OUT = (os.path.abspath(x) for x in sys.argv[1:5])
SIB = -27.0  # v6: the 7 kHz band is on
if '--no-sibilance-band' in sys.argv:
    SIB = None
elif '--sibilance-band' in sys.argv:
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
INT_EXACT = '--int-exact' in sys.argv  # 24-bit stems, exact_trim to 0.1 dB under 0 dBTP, no pins (the vocal level moves)
F32_TP = '--f32-tp' in sys.argv  # float stems at v5's pins + tp_trim common to <= 0.0 dBTP (balance kept)
if INT_EXACT and F32_TP: sys.exit('--int-exact and --f32-tp are alternatives; pick one')
if F32_TP:
    r = json.loads(call('export_stems', {'dir': STEMS, 'format': 'wav', 'stem_format': 'f32', 'mode': 'pre_master', 'stem_gain_db': PINS, 'tp_trim': 'common'}))
elif INT_EXACT:
    r = json.loads(call('export_stems', {'dir': STEMS, 'format': 'wav', 'mode': 'pre_master', 'exact_trim': True}))
else:
    r = json.loads(call('export_stems', {'dir': STEMS, 'format': 'wav', 'stem_format': 'f32', 'mode': 'pre_master', 'stem_gain_db': PINS}))
json.dump(r, open(os.path.join(WD, 'stems.json'), 'w'), indent=1)
if not INT_EXACT and (r.get('stem_format') != 'f32' or r.get('bits') != 32):
    sys.exit(f"export_stems did not write float stems: {r.get('stem_format')} {r.get('bits')}")
bad = [(s['name'], s.get('clipped_samples'), s.get('warnings')) for s in r['stems'] if s.get('clipped_samples') or s.get('warnings')]
if bad: sys.exit(f'stems not clean: {bad}')
over = [(s['name'], s['true_peak_dbtp']) for s in r['stems'] if s.get('over_ceiling')]
if over: print('note: over 0 dBTP at the pinned gain, carried unclipped in float:', over, file=LOG, flush=True)
if F32_TP:
    tp = r.get('tp_trim') or {}
    hot = [(s['name'], s['true_peak_dbtp']) for s in r['stems'] if s['true_peak_dbtp'] > 0.0]
    if not tp.get('converged') or over or hot or r.get('warnings'):
        sys.exit(f"--f32-tp: stems not under 0 dBTP: {tp} {hot} {r.get('warnings')}")
    print('tp_trim', json.dumps(tp), file=LOG, flush=True)
post = subprocess.run([sys.executable, os.path.join(HERE, 'iwby_post.py'), STEMS, OUT, PROJ], capture_output=True, text=True, timeout=290)
print(post.stdout[-1500:], post.stderr[-1500:], file=LOG, flush=True)
if post.returncode != 0: sys.exit(f'iwby_post failed: {post.stderr[-600:]}')
shutil.rmtree(os.path.join(EW, '.beatbox_cache'), ignore_errors=True)
if not os.environ.get('IWBY_KEEP_STEMS'):  # IWBY_KEEP_STEMS=1 keeps WORKDIR/stems for measuring
    shutil.rmtree(STEMS, ignore_errors=True)
print('wrote', OUT)
