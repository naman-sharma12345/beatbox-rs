#!/usr/bin/env python3
"""Raw-sound metrics for whole beats (diagnostics, not a quality score).

usage: raw_metrics.py FILE... [--json OUT]

Per file (decoded with ffmpeg to 44.1 kHz mono, RMS-normalised):
  band shares of the power spectrum: sub 20-60, bass 60-200, mud 200-500,
  mid 500-2k, pres 2k-5k, air >8k (fractions of 20 Hz-20 kHz power)
  st_crest_db: crest factor of the 10 ms RMS envelope (95th pct over median,
               dB) - how much the hits stand out of what sits between them
  flux: 90th pct over median of positive spectral flux (transient sharpness)
  hf_flatness: spectral flatness 6-16 kHz (noise-like cymbals ~0.4,
               tonal/ringing hats ~0.02)
  mud_when_bass: 200-500 Hz share measured only in frames where the
               60-200 Hz band is active (the chords-over-bass case)
"""
import json
import subprocess
import sys

import numpy as np

SR = 44100


def load(path):
    raw = subprocess.run(
        ["ffmpeg", "-v", "quiet", "-i", path, "-ac", "1", "-ar", str(SR), "-f", "f32le", "-"],
        capture_output=True,
        check=True,
    ).stdout
    x = np.frombuffer(raw, dtype=np.float32).astype(np.float64)
    r = np.sqrt(np.mean(x**2)) + 1e-12
    return x / r


def metrics(x):
    n = 4096
    hop = 1024
    win = np.hanning(n)
    frames = np.lib.stride_tricks.sliding_window_view(x, n)[::hop] * win
    spec = np.abs(np.fft.rfft(frames, axis=1)) ** 2
    f = np.fft.rfftfreq(n, 1 / SR)
    total = spec[:, (f >= 20) & (f < 20000)].sum()

    def band(lo, hi, s=spec):
        return s[:, (f >= lo) & (f < hi)].sum()

    out = {
        "sub_20_60": band(20, 60) / total,
        "bass_60_200": band(60, 200) / total,
        "mud_200_500": band(200, 500) / total,
        "mid_500_2k": band(500, 2000) / total,
        "pres_2k_5k": band(2000, 5000) / total,
        "air_8k": band(8000, 20000) / total,
    }
    # mud while the bass plays
    fb = spec[:, (f >= 60) & (f < 200)].sum(1)
    ft = spec[:, (f >= 20) & (f < 20000)].sum(1) + 1e-12
    act = fb > np.percentile(fb, 50)
    if act.any():
        s = spec[act]
        out["mud_when_bass"] = s[:, (f >= 200) & (f < 500)].sum() / ft[act].sum()
    # envelope crest
    m = int(0.01 * SR)
    env = np.sqrt(np.convolve(x**2, np.ones(m) / m, mode="valid")[::m] + 1e-12)
    out["st_crest_db"] = 20 * np.log10(np.percentile(env, 95) / (np.median(env) + 1e-12))
    # spectral flux
    mag = np.sqrt(spec)
    d = np.maximum(np.diff(mag, axis=0), 0).sum(1)
    out["flux"] = float(np.percentile(d, 90) / (np.median(d) + 1e-12))
    hf = spec[:, (f >= 6000) & (f < 16000)] + 1e-18
    loud = hf.sum(1) > np.percentile(hf.sum(1), 50)
    hf = hf[loud] if loud.any() else hf
    flat = np.exp(np.mean(np.log(hf), axis=1)) / np.mean(hf, axis=1)
    out["hf_flatness"] = float(np.median(flat))
    return {k: round(float(v), 4) for k, v in out.items()}


def main():
    args = sys.argv[1:]
    out_json = None
    if "--json" in args:
        i = args.index("--json")
        out_json = args[i + 1]
        del args[i : i + 2]
    res = {}
    for p in args:
        res[p.split("/")[-1]] = metrics(load(p))
    keys = ["sub_20_60", "bass_60_200", "mud_200_500", "mud_when_bass", "pres_2k_5k", "air_8k", "st_crest_db", "flux", "hf_flatness"]
    print("| file | " + " | ".join(keys) + " |")
    print("|---" * (len(keys) + 1) + "|")
    for k, v in res.items():
        print(f"| {k} | " + " | ".join(str(v.get(x, "")) for x in keys) + " |")
    if out_json:
        json.dump(res, open(out_json, "w"), indent=1)


if __name__ == "__main__":
    main()
