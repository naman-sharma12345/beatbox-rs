#!/usr/bin/env python3
"""IWBY v4 offline post-process (B43, 2026-10-13).

Reproduces v4.wav from the engine's pre-master stems of the v3 project:
  beatbox --workdir WD call export_stems '{"dir":"STEMS","format":"wav","mode":"pre_master"}' --project proj.json
  python3 iwby_v4_post.py STEMS OUT.wav [params.json]
Needs numpy, scipy, soundfile, pyloudnorm. No audio and no vocal take are kept in the repo.
Every random draw comes from numpy default_rng(seed) in a fixed order, so a re-run is bit-identical
on the same numpy/scipy build.
"""
import json, os, sys
import numpy as np, soundfile as sf, scipy.signal as ss
from scipy.ndimage import minimum_filter1d
import pyloudnorm as pyln

STEMS, OUT = sys.argv[1], sys.argv[2]
P = json.load(open(sys.argv[3] if len(sys.argv) > 3 else os.path.join(os.path.dirname(os.path.abspath(__file__)), 'params.json')))
st = {}
for f in sorted(os.listdir(STEMS)):
    if f.endswith('.wav'):
        x, sr = sf.read(os.path.join(STEMS, f), dtype='float32'); st[f[:-4]] = x
SR = 44100; BPM = P['bpm']; BEAT = 60/BPM; BAR = 4*BEAT; Q = BEAT

# ---- phrase detection on the dry vocal stem (10 ms RMS frames)
vm = st['vox'].mean(1); hop = 441; fr = len(vm)//hop
env = np.sqrt(np.mean(vm[:fr*hop].reshape(fr, hop)**2, 1)); edb = 20*np.log10(env+1e-9)
def phrases(thr, gap=0.4, minlen=0.3):
    act = edb > thr; segs = []; i = 0
    while i < fr:
        if act[i]:
            j = i
            while j < fr and act[j]: j += 1
            segs.append([i, j]); i = j
        else: i += 1
    m = []
    for s in segs:
        if m and (s[0]-m[-1][1])*0.01 < gap: m[-1][1] = s[1]
        else: m.append(s)
    return [(a*0.01, b*0.01) for a, b in m if (b-a)*0.01 > minlen]
PH = phrases(P['phrase_thr_db'], P['phrase_gap_s'])
starts = [a for a, b in PH]

N0 = len(vm); TAIL = int(P['tail_s']*SR); N = N0+TAIL
def pad(x): return np.concatenate([x, np.zeros((TAIL,)+x.shape[1:], np.float32)])
S = {k: pad(v) for k, v in st.items()}
t = np.arange(N)/SR
def bq(x, typ, f, order=2):
    sos = ss.butter(order, f, btype=typ, fs=SR, output='sos'); return ss.sosfilt(sos, x, axis=0).astype(np.float32)
def env_pts(pts):
    tt = [p[0] for p in pts]; dd = [p[1] for p in pts]
    return (10**(np.interp(t, tt, dd)/20)).astype(np.float32)[:, None]
sec = dict(intro=(0, 4*BAR), verse=(4*BAR, 12*BAR), chorus1=(12*BAR, 20*BAR), breakdown=(18*BAR, 20*BAR), chorus2=(20*BAR, 26*BAR), outro=(26*BAR, 30*BAR))
i0, i1 = sec['intro']; v0, v1 = sec['verse']; c0, c1 = sec['chorus1']; b0, b1 = sec['breakdown']; d0, d1 = sec['chorus2']; o0, o1 = sec['outro']
OFF = -80
rng = np.random.default_rng(P['seed'])

def smooth(m, att, rel):
    h = 64; md = m[::h]; out = np.zeros_like(md); y = 0; aa = np.exp(-h/(att*SR)); ar = np.exp(-h/(rel*SR))
    for i, xv in enumerate(md):
        c = aa if xv > y else ar; y = c*y+(1-c)*xv; out[i] = y
    return np.interp(np.arange(len(m)), np.arange(len(md))*h, out).astype(np.float32)
def make_ir(rt_lo=3.0, rt_mid=2.7, rt_hi=1.8, length=4.0, pre=0.040):
    n = int(length*SR); tt = np.arange(n)/SR; ir = np.zeros((n, 2), np.float32)
    for ch in range(2):
        w = rng.standard_normal(n).astype(np.float32)
        lo = bq(w, 'lowpass', 1000); hi = bq(w, 'highpass', 4000); mid = w-lo-hi
        y = lo*np.exp(-6.91*tt/rt_lo)+mid*np.exp(-6.91*tt/rt_mid)+hi*np.exp(-6.91*tt/rt_hi)
        y *= np.minimum(1, tt/0.08)
        ir[:, ch] = y
    p = int(pre*SR); ir = np.concatenate([np.zeros((p, 2), np.float32), ir])
    for d, g, pan in [(0.011, 0.5, 0), (0.019, 0.4, 1), (0.027, 0.35, 0), (0.033, 0.3, 1), (0.041, 0.25, 0)]:
        ir[p+int(d*SR), pan] += g*np.sqrt(np.sum(ir[p:p+int(0.1*SR)]**2)/(0.1*SR))*30
    return ir/np.sqrt(np.sum(ir**2)/2)
def conv_st(x, ir):
    m = x.mean(1)
    return np.stack([ss.fftconvolve(m, ir[:, 0])[:len(m)], ss.fftconvolve(m, ir[:, 1])[:len(m)]], 1).astype(np.float32)
def crash(L=3.0):
    n = int(L*SR); tt = np.arange(n)/SR
    w = rng.standard_normal((n, 2)).astype(np.float32)
    w = bq(w, 'highpass', 4500, 4)+0.4*bq(bq(w, 'bandpass', [3000, 6000]), 'highpass', 2500)
    am = np.exp(-tt/0.9)*(1+0.15*np.sin(2*np.pi*7*tt)); am[:int(0.002*SR)] *= np.linspace(0, 1, int(0.002*SR))
    return (w*am[:, None]).astype(np.float32)
def rms(x): return np.sqrt(np.mean(x**2)+1e-20)

# random draws in the order the v4 session made them (an unused first hall IR included)
_unused_ir = make_ir()
IRlong = make_ir(4.0, 3.6, 2.5, 5.0, 0.0)
cr = crash(); cr = cr/rms(cr[:int(0.5*SR)])
sw = crash(BAR)[::-1].copy(); sw = sw/rms(sw[-int(0.5*SR):])
R_ = P['reverb']
IR = make_ir(R_['rt_lo_s'], R_['rt_mid_s'], R_['rt_hi_s'], R_['ir_len_s'], R_['predelay_s'])

vox = S['vox'].copy(); vmono = vox.mean(1)
act = np.zeros(N, np.float32)
for a, b in PH: act[int(a*SR):int(b*SR)] = 1
activeI = act > 0.5
dryA = rms(vox[activeI])
actS = smooth(act, P['duck_attack_s'], P['duck_release_s'])
vin = bq(bq(vox, 'highpass', 150), 'lowpass', 9000)
rev = bq(bq(conv_st(vin, IR), 'highpass', R_['return_hpf_hz']), 'lowpass', R_['return_lpf_hz'])
T_ = P['throws']
def throw_chain(seg, nrep, fb, delay):
    L = len(seg)+int(delay*SR*nrep)+10; out = np.zeros((L, 2), np.float32); cur = bq(seg, 'highpass', 300, 1)
    for k in range(1, nrep+1):
        cur = bq(cur, 'lowpass', T_['first_lpf_hz'] if k == 1 else T_['loop_lpf_hz'], 1)
        g_ = fb**(k-1); d = int(k*delay*SR); panL = 0.85 if k % 2 else 0.35; panR = 1.2-panL
        out[d:d+len(cur), 0] += g_*panL*cur; out[d:d+len(cur), 1] += g_*panR*cur
    return out
thr_sig = np.zeros((N, 2), np.float32)
KEEP = P.get('throw_keep_s')  # None = a throw on every phrase end; else only phrase ends within 50 ms of these times
thrown = []
for i, (a, b) in enumerate(PH):
    if KEEP is not None and not any(abs(b-k) < 0.05 for k in KEEP): continue
    thrown.append(round(b, 2))
    last = (i == len(PH)-1); w = T_['last_window_s'] if last else T_['window_s']
    s0 = int(max(a, b-w)*SR); s1 = int(b*SR+0.05*SR)
    seg = vmono[s0:s1].copy(); fl = int(0.02*SR); seg[:fl] *= np.linspace(0, 1, fl); seg[-fl:] *= np.linspace(1, 0, fl)
    nrep, fb = (T_['last_repeats'], T_['last_feedback']) if last else (T_['repeats'], T_['feedback'])
    o = throw_chain(seg, nrep, fb, Q); e = min(N, s0+len(o)); thr_sig[s0:e] += o[:e-s0]
duck_rev = (10**(-P['reverb_duck_db']*actS/20))[:, None]; duck_thr = (10**(-P['throw_duck_db']*actS/20))[:, None]
rev_thr = bq(bq(conv_st(thr_sig, IR), 'highpass', R_['return_hpf_hz']), 'lowpass', R_['return_lpf_hz'])
revsend = env_pts([(0, 0), (c0-0.3, 0), (c0, 2.5), (b0, 2.5), (b0+0.5, 3), (d0-0.3, 3), (d0, 2.5), (o0, 2.5), (o0+1, 0), (PH[-1][0]-0.5, 0), (PH[-1][0], 4), (N/SR, 4)])
vh = bq(vmono, 'highpass', 300)
def moddelay(x, base_ms, depth_ms, rate, phase=0):
    n = len(x); d = (base_ms+depth_ms*np.sin(2*np.pi*rate*t[:n]+phase))*SR/1000
    idx = np.arange(n)-d; i0_ = np.floor(idx).astype(int); fr_ = idx-i0_; i0_ = np.clip(i0_, 0, n-2)
    return (x[i0_]*(1-fr_)+x[i0_+1]*fr_).astype(np.float32)
Dl = P['doubler']
dbl = np.stack([moddelay(vh, Dl['ms'][0], Dl['depth_ms'], Dl['rate_hz'][0]), moddelay(vh, Dl['ms'][1], Dl['depth_ms'], Dl['rate_hz'][1], 1.3)], 1)*10**(Dl['level_db']/20)
def vocal_fx(g_rev_db, g_thr_db):
    Rr = rev*10**(g_rev_db/20)*duck_rev*revsend + rev_thr*10**((g_thr_db-8)/20)
    Tt = thr_sig*10**(g_thr_db/20)*duck_thr*revsend**0.5
    return Rr, Tt
R, T = vocal_fx(P['reverb_return_db'], P['throw_send_db'])
WB = P.get('wet_bell')
if WB:  # RBJ peaking bell on both wet returns (reverb and echo throws)
    A = 10**(WB['db']/40); w0 = 2*np.pi*WB['hz']/SR; al = np.sin(w0)/(2*WB['q'])
    bb = np.array([1+al*A, -2*np.cos(w0), 1-al*A]); aa = np.array([1+al/A, -2*np.cos(w0), 1-al/A])
    R = ss.lfilter(bb/aa[0], aa/aa[0], R, axis=0).astype(np.float32); T = ss.lfilter(bb/aa[0], aa/aa[0], T, axis=0).astype(np.float32)
slap = np.zeros_like(vh); sd = int(P['slap_ms']/1000*SR); slap[sd:] = bq(vh, 'lowpass', 3500)[:-sd]*10**(P['slap_db']/20); slapst = np.stack([slap*0.6, slap*0.9], 1)
VOXBUS = vox+dbl+slapst+R+T

if os.environ.get('IWBY_STATS'):  # measurement only; does not touch the audio
    db = lambda r: float(20*np.log10(r+1e-20))
    gapI = (~activeI) & (t > PH[0][0]) & (t < PH[-1][1]+3)
    print(json.dumps({'dryA_rms': float(dryA), 'rev_active_db': db(rms(R[activeI])/dryA), 'rev_gaps_db': db(rms(R[gapI])/dryA),
                      'thr_active_db': db(rms(T[activeI])/dryA), 'thr_gaps_db': db(rms(T[gapI])/dryA),
                      'wet_gaps_db': db(rms((R+T)[gapI])/dryA), 'dbl_db': db(rms(dbl[activeI])/dryA)}))

# ---- arrangement (stem gain envelopes, dB breakpoints)
g = {}
g['drums'] = env_pts([(0, OFF), (v0-0.02, OFF), (v0, -3), (c0-0.02, -3), (c0, 1), (b0-0.1, 1), (b0, OFF), (d0-0.02, OFF), (d0, 1.5), (o0-0.05, 1.5), (o0+BAR, OFF), (N/SR, OFF)])
g['bass'] = env_pts([(0, OFF), (v0-0.02, OFF), (v0, -1), (c0, 0), (b0-0.2, 0), (b0+0.4, OFF), (d0-0.02, OFF), (d0, 0.5), (o0, 0.5), (o0+2*BAR, OFF), (N/SR, OFF)])
g['comp'] = env_pts([(0, -2), (c0, 0), (b0-0.2, 0), (b0+1.0, OFF), (d0-0.02, OFF), (d0, 0), (o0, 0), (o1, -14), (o1+1.5, OFF), (N/SR, OFF)])
g['comp2'] = env_pts([(0, OFF), (c0-0.02, OFF), (c0, 0), (b0-0.2, 0), (b0+0.6, OFF), (d0-0.02, OFF), (d0, 1), (o0, -4), (o0+2*BAR, OFF), (N/SR, OFF)])
g['organ'] = env_pts([(0, 5), (v0, 2), (c0, 6), (b0, 6), (b0+1, 10), (d0, 7), (o0, 7), (o1, -6), (o1+2, OFF), (N/SR, OFF)])
busg = env_pts([(0, -3), (c0, 0), (b0, 0), (b0+1, -6), (d0, 0), (o0, 0), (o1, -10), (N/SR, -30)])
dr = S['drums']; drv = bq(dr, 'lowpass', 3500); vmask = env_pts([(0, 0), (c0-0.02, 0), (c0, OFF), (N/SR, OFF)])
drum_proc = dr*(1-vmask)+drv*vmask
MUS = drum_proc*g['drums']
for k in ['bass', 'comp', 'comp2', 'organ']: MUS = MUS+S[k]*g[k]
for k in ['bus_hall', 'bus_plate', 'bus_vdelay']: MUS = MUS+S[k]*busg
fillmask = env_pts([(0, 0), (c0-2*BEAT-0.01, 0), (c0-2*BEAT, OFF), (c0-0.01, OFF), (c0, 0), (N/SR, 0)])
MUS = MUS-drum_proc*g['drums']*(1-fillmask)
wide = env_pts([(0, 0), (c0-0.3, 0), (c0, 3), (b0, 3), (b0+0.5, 0), (d0-0.3, 0), (d0, 3), (o0, 3), (o0+2, 0), (N/SR, 0)])
Mid = (MUS[:, 0]+MUS[:, 1])/2; Sd = (MUS[:, 0]-MUS[:, 1])/2*wide[:, 0]
MUS = np.stack([Mid+Sd, Mid-Sd], 1).astype(np.float32)

# ---- transitions
TR = np.zeros((N, 2), np.float32)
def add(sig, at):
    s = int(at*SR); e = min(N, s+len(sig))
    if s < 0: sig = sig[-s:]; s = 0
    TR[s:e] += sig[:e-s]
def revswell(onset, ln=0.55, gain_db=-4):
    s = int(onset*SR); seg = vmono[s:s+int(ln*SR)][::-1].copy()
    w = np.stack([ss.fftconvolve(seg, IRlong[:, 0]), ss.fftconvolve(seg, IRlong[:, 1])], 1)[::-1].astype(np.float32)
    w = bq(bq(w, 'highpass', 250), 'lowpass', 7000)
    w = w/(rms(w[-int(0.5*SR):])+1e-9)*dryA*10**(gain_db/20)
    fade = np.ones(len(w), np.float32); fl = int(0.03*SR); fade[-fl:] = np.linspace(1, 0, fl)
    add(w*fade[:, None], onset-len(w)/SR+0.02)
ph_c1 = min(starts, key=lambda x: abs(x-c0)); ph_c2 = min([s for s in starts if s > d0], key=lambda x: x-d0)
s16 = BEAT/4; src = int((c0-BAR)*SR)
sl = dr[src:src+int(s16*SR)].copy(); f5 = int(0.005*SR)
sl[:f5] *= np.linspace(0, 1, f5)[:, None]; sl[-f5:] *= np.linspace(1, 0, f5)[:, None]
cr2 = cr.copy(); cr2[:f5] *= np.linspace(0, 1, f5)[:, None]
CRASH_DB = P['crash_db']
revswell(ph_c1, gain_db=-2); revswell(ph_c2, gain_db=-2)
for at, gd in [(v0, -6), (c0, 0), (d0, 2)]: add(cr2*dryA*10**((CRASH_DB+gd)/20), at)
for at in [c0, d0]: add(sw*dryA*10**((CRASH_DB-2)/20), at-len(sw)/SR)
for k in range(8): add(bq(sl, 'highpass', 80+60*k)*10**((-10+9*k/7)/20), c0-2*BEAT+k*s16)

# ---- sum, end fade, master
fade = env_pts([(0, 0), (105.0, 0), (108.6, -60), (N/SR, -200)])
MIX = (VOXBUS+MUS+TR).astype(np.float64)*fade; MIX = MIX[:int(P['length_s']*SR)]
M_ = P['master']
MIXs = MIX+(10**(M_['shelf_db']/20)-1)*ss.sosfilt(ss.butter(2, M_['shelf_hz'], 'highpass', fs=SR, output='sos'), MIX, axis=0)
meter = pyln.Meter(SR)
def limit(x, ceil_db=-1.3, os_=4, look=0.005, rel=0.08):
    up = ss.resample_poly(x, os_, 1, axis=0); pk = np.abs(up).max(1).reshape(-1, os_).max(1)[:len(x)]
    if len(pk) < len(x): pk = np.pad(pk, (0, len(x)-len(pk)))
    c = 10**(ceil_db/20); need = np.minimum(1, c/np.maximum(pk, 1e-9))
    L = int(look*SR); need = minimum_filter1d(need, 2*L+1)
    gsm = np.empty_like(need); y = 1.0; ar = np.exp(-1/(rel*SR))
    for i in range(len(need)):
        v_ = need[i]; y = v_ if v_ < y else ar*y+(1-ar)*v_
        gsm[i] = y
    return x*gsm[:, None]
def master(ceil, target):
    gg = 10**((target-meter.integrated_loudness(MIXs))/20)
    for it in range(2):
        Yy = limit(MIXs*gg, ceil_db=ceil); gg *= 10**((target-meter.integrated_loudness(Yy))/20)
    return limit(MIXs*gg, ceil_db=ceil)
Y = master(M_['ceiling_db'], M_['target_lufs'])
from scipy.io import wavfile
wavfile.write(OUT, SR, Y.astype(np.float32))  # no PEAK chunk/timestamp: byte-stable
print(json.dumps({'phrases': len(PH), 'throw_times_s': thrown, 'lufs': round(meter.integrated_loudness(Y), 2)}))
