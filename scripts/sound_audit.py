#!/usr/bin/env python3
"""Audit rendered one-shots (examples/oneshots.rs output).

usage: sound_audit.py DIR [--ears EARS_DIR] [--compare BEFORE_DIR] [--out OUT_PREFIX]

Per file: peak / clipping, DC, start + end discontinuity (click risk),
spectral centroid, low/body/harsh/air band shares, crest, timbral motion
(how much the spectrum moves over the note), alias energy for harmonic
voices (energy away from the harmonic series), and round-robin variation
across seeds for drums. Flags weak sounds per role with plain reasons.
Writes OUT.md (table + weak list) and OUT.json.
"""
import csv
import json
import math
import os
import sys

import numpy as np
from scipy.io import wavfile

SR = 44100


def load(path):
    sr, x = wavfile.read(path)
    x = x.astype(np.float64)
    if x.ndim > 1:
        x = x.mean(axis=1)
    if x.dtype.kind == "i" or np.abs(x).max() > 2.0:
        x = x / 32768.0
    return x


def band_share(spec, freqs, lo, hi):
    tot = spec.sum() + 1e-20
    m = (freqs >= lo) & (freqs < hi)
    return float(spec[m].sum() / tot)


def stft_mag(x, n=2048, hop=512):
    if len(x) < n:
        x = np.pad(x, (0, n - len(x)))
    w = np.hanning(n)
    frames = []
    for i in range(0, len(x) - n + 1, hop):
        frames.append(np.abs(np.fft.rfft(x[i:i + n] * w)))
    return np.array(frames), np.fft.rfftfreq(n, 1 / SR)


def role_of(name, kind):
    n = name.lower()
    if kind == "drum" or n.startswith(("kick", "snare", "clap", "hat", "open_hat", "rim", "tom", "cowbell", "shaker", "crash", "layered_kick", "layered_snare")):
        for r in ("kick", "snare", "clap", "open_hat", "hat", "rim", "tom", "cowbell", "shaker", "crash", "tabla", "bayan"):
            if r in n:
                return r
        return "perc"
    if kind == "bass808" or "808" in n:
        return "808"
    if "bass" in n or n.startswith("sub"):
        return "bass"
    if "pad" in n or "string" in n or "choir" in n or "tanpura" in n or "cello" in n or "brass_section" in n:
        return "pad"
    if "bell" in n or "marimba" in n or "mallet" in n:
        return "bell"
    if "pluck" in n or "koto" in n or "guitar" in n or "sitar" in n or "santoor" in n:
        return "pluck"
    if "key" in n or "piano" in n or "rhodes" in n or "organ" in n:
        return "keys"
    return "lead"


def alias_db(x, f0):
    """Energy outside +-1.5 bins of the harmonic series (f0 and f0/2 grids),
    relative to total, measured on a steady 8192-sample window."""
    if f0 <= 0 or len(x) < 9000:
        return None
    start = min(int(0.05 * SR), len(x) - 8192)
    seg = x[start:start + 8192]
    if np.abs(seg).max() < 1e-4:
        return None
    w = np.blackman(8192)
    sp = np.abs(np.fft.rfft(seg * w)) ** 2
    freqs = np.fft.rfftfreq(8192, 1 / SR)
    df = freqs[1]
    best = None
    for base in (f0, f0 / 2):
        k = np.round(freqs / base)
        near = np.abs(freqs - k * base) <= 3.5 * df
        near |= freqs < 30
        off = sp[~near].sum()
        r = 10 * math.log10(off / (sp.sum() + 1e-30) + 1e-12)
        best = r if best is None else min(best, r)
    return round(best, 1)


def analyse(path, name, kind, pitch):
    x = load(path)
    n = len(x)
    peak = float(np.abs(x).max()) if n else 0.0
    clip = int((np.abs(x) >= 0.999).sum())
    dc = float(x.mean()) if n else 0.0
    start_jump = float(abs(x[0])) if n else 0.0
    tail = x[-int(0.002 * SR):] if n > 100 else x
    end_level = float(np.abs(tail).max()) if n else 0.0
    rms = float(np.sqrt((x ** 2).mean())) if n else 0.0
    crest = 20 * math.log10(peak / (rms + 1e-12) + 1e-12) if n else 0.0
    mags, freqs = stft_mag(x)
    pw = (mags ** 2)
    tot = pw.sum(axis=0)
    cent_frames = (pw * freqs).sum(axis=1) / (pw.sum(axis=1) + 1e-20)
    loud = pw.sum(axis=1) > pw.sum(axis=1).max() * 1e-3
    cent = float((tot * freqs).sum() / (tot.sum() + 1e-20))
    motion = float(np.std(np.log2(cent_frames[loud] + 1))) if loud.sum() > 3 else 0.0
    # spectral flux of the normalised spectrum (timbre change frame to frame)
    if loud.sum() > 3:
        nm = mags[loud] / (mags[loud].sum(axis=1, keepdims=True) + 1e-20)
        flux = float(np.abs(np.diff(nm, axis=0)).sum(axis=1).mean())
    else:
        flux = 0.0
    f0 = 440.0 * 2 ** ((pitch - 69) / 12)
    harmonic = kind in ("synth", "wavetable", "fm", "piano", "ensemble", "layer") and "noise" not in name
    return {
        "file": os.path.basename(path),
        "name": name,
        "kind": kind,
        "role": role_of(name, kind),
        "dur_s": round(n / SR, 3),
        "peak_db": round(20 * math.log10(peak + 1e-12), 2),
        "clipped": clip,
        "dc": round(dc, 5),
        "start_jump": round(start_jump, 4),
        "end_level": round(end_level, 5),
        "crest_db": round(crest, 1),
        "centroid_hz": round(cent),
        "sub_lt60": round(band_share(tot, freqs, 20, 60), 3),
        "low_lt150": round(band_share(tot, freqs, 20, 150), 3),
        "body_150_600": round(band_share(tot, freqs, 150, 600), 3),
        "harsh_2k_5k": round(band_share(tot, freqs, 2000, 5000), 3),
        "air_gt8k": round(band_share(tot, freqs, 8000, 22050), 3),
        "motion": round(motion, 3),
        "flux": round(flux, 4),
        "alias_db": alias_db(x, f0) if harmonic else None,
        "_x": x,
    }


def flags(r):
    out = []
    role = r["role"]
    if r["clipped"] > 0 or r["peak_db"] > -0.1:
        out.append(f"clips ({r['clipped']} samples at full scale)")
    if abs(r["dc"]) > 0.01:
        out.append(f"DC offset {r['dc']:+.3f}")
    if r["start_jump"] > 0.05:
        out.append(f"starts on a non-zero sample ({r['start_jump']:.3f}) -> click")
    if r["end_level"] > 0.01:
        out.append(f"ends abruptly (last 2 ms peak {r['end_level']:.3f}) -> click")
    if r["alias_db"] is not None and r["alias_db"] > -45:
        out.append(f"aliasing / inharmonic junk {r['alias_db']} dB")
    if role == "kick":
        if r["low_lt150"] < 0.55:
            out.append(f"thin: only {r['low_lt150']:.0%} of energy below 150 Hz")
        if r["crest_db"] < 9:
            out.append(f"no transient (crest {r['crest_db']} dB)")
    if role == "808":
        if r["low_lt150"] < 0.7:
            out.append(f"weak sub ({r['low_lt150']:.0%} below 150 Hz)")
        if r["body_150_600"] < 0.02:
            out.append("pure sine: no harmonics to read on small speakers")
    if role in ("snare", "clap"):
        if r["body_150_600"] < 0.08:
            out.append(f"no body ({r['body_150_600']:.0%} in 150-600 Hz)")
        if r["harsh_2k_5k"] > 0.45:
            out.append(f"harsh ({r['harsh_2k_5k']:.0%} in 2-5 kHz)")
    if role in ("hat", "open_hat", "shaker", "crash"):
        if r["harsh_2k_5k"] > 0.3:
            out.append(f"harsh/clangy ({r['harsh_2k_5k']:.0%} in 2-5 kHz)")
        if r["air_gt8k"] < 0.3:
            out.append(f"dull ({r['air_gt8k']:.0%} above 8 kHz)")
    if role in ("pad", "keys", "lead", "bell", "pluck") and r["dur_s"] > 0.8:
        if r["motion"] < 0.03 and r["flux"] < 0.02:
            out.append(f"static (centroid motion {r['motion']}, flux {r['flux']})")
    return out


def rr_variation(rows):
    """For each drum with rr1..rr4 renders: mean normalised difference."""
    by = {}
    for r in rows:
        if "__rr" in r["file"]:
            by.setdefault(r["name"], []).append(r["_x"])
    out = {}
    for name, xs in by.items():
        if len(xs) < 2:
            continue
        n = min(len(x) for x in xs)
        a = xs[0][:n]
        diffs = []
        for b in xs[1:]:
            b = b[:n]
            diffs.append(float(np.sqrt(((a - b) ** 2).mean()) / (np.sqrt((a ** 2).mean()) + 1e-12)))
        out[name] = round(float(np.mean(diffs)), 4)
    return out


def run(d, ears_dir=None):
    rows = []
    with open(os.path.join(d, "index.csv")) as f:
        for row in csv.DictReader(f):
            p = os.path.join(d, row["file"])
            if not os.path.exists(p):
                continue
            r = analyse(p, row["name"], row["kind"], float(row["pitch"]))
            r["point"] = row["point"]
            if ears_dir:
                ep = os.path.join(ears_dir, row["file"] + ".json")
                if os.path.exists(ep):
                    try:
                        e = json.load(open(ep))
                        r["ears"] = {k: e.get(k) for k in ("integrated_lufs", "true_peak_dbtp", "crest_db", "spectral_balance", "onsets", "lufs", "true_peak") if k in e}
                    except Exception:
                        pass
            r["flags"] = flags(r)
            rows.append(r)
    rr = rr_variation(rows)
    for r in rows:
        if r["name"] in rr:
            r["rr_variation"] = rr[r["name"]]
            if r["kind"] == "drum" and rr[r["name"]] < 0.01 and "__rr1" in r["file"]:
                r["flags"].append("machine-gun: identical on every hit (no round-robin)")
    return rows


def main():
    args = sys.argv[1:]
    d = args[0]
    ears = None
    cmp_dir = None
    out = os.path.join(d, "audit")
    if "--ears" in args:
        ears = args[args.index("--ears") + 1]
    if "--compare" in args:
        cmp_dir = args[args.index("--compare") + 1]
    if "--out" in args:
        out = args[args.index("--out") + 1]
    rows = run(d, ears)
    before = {r["file"]: r for r in run(cmp_dir)} if cmp_dir else {}
    clean = [{k: v for k, v in r.items() if k != "_x"} for r in rows]
    json.dump(clean, open(out + ".json", "w"), indent=1)
    lines = ["# Sound audit: " + d, ""]
    weak = [r for r in rows if r["flags"] and "__rr" not in r["file"] or ("__rr1" in r["file"] and r["flags"])]
    lines.append(f"{len(rows)} renders, {len(weak)} flagged.")
    lines.append("")
    lines.append("## Weak sounds")
    lines.append("")
    for r in weak:
        b = before.get(r["file"])
        was = f" (before: {'; '.join(b['flags']) or 'clean'})" if b else ""
        lines.append(f"- **{r['name']}** `{r['point']}` [{r['role']}]: {'; '.join(r['flags'])}{was}")
    if before:
        fixed = [f for f, b in before.items() if b["flags"] and f in {r['file'] for r in rows} and not next(r for r in rows if r['file'] == f)["flags"]]
        lines.append("")
        lines.append("## Fixed since the compared render")
        lines.append("")
        for f in fixed:
            lines.append(f"- {f}: was {'; '.join(before[f]['flags'])}")
    lines.append("")
    lines.append("## All renders")
    lines.append("")
    cols = ["file", "role", "peak_db", "dc", "start_jump", "end_level", "crest_db", "centroid_hz", "low_lt150", "body_150_600", "harsh_2k_5k", "air_gt8k", "motion", "alias_db", "rr_variation"]
    lines.append("| " + " | ".join(cols) + " |")
    lines.append("|" + "---|" * len(cols))
    for r in rows:
        lines.append("| " + " | ".join(str(r.get(c, "")) for c in cols) + " |")
    open(out + ".md", "w").write("\n".join(lines) + "\n")
    print("\n".join(lines[:4 + len(weak) + 2]))


if __name__ == "__main__":
    main()
