#!/usr/bin/env python3
"""Stem separation helper for beatbox (separate_stems): drums / bass / other / vocals.

For ANALYSIS of a reference record (transcription, sound matching, A/B scoring);
the stems are never meant to be mixed into a release.

Model: htdemucs (Defossez et al., Meta, MIT) exported to ONNX by StemSplitio
(huggingface.co/StemSplitio/htdemucs-onnx, MIT), fp16-weights file, 166 MB,
fetched once into ~/.beatbox/models and checked against its sha256.
Runs on CPU with onnxruntime + numpy (+ soundfile); ~1.1 GB RAM per 7.8 s segment.
Chunked overlap-add follows the model card's reference inference (MIT).

Usage: separate_stems.py AUDIO OUT_DIR [--stems drums,bass,other,vocals] [--threads 2] [--fetch]
Prints JSON: {"stems": {name: path}, "seconds", "model", "chunks"} or {"error": ...}.
"""
import argparse, hashlib, json, os, subprocess, sys, time, urllib.request
from pathlib import Path

REV = "d54ed9eb60e258ea82131c6ee14578628816456a"
FILE = "htdemucs_fp16weights.onnx"
SHA256 = "d05c269d0178d2a72ad484b10b11dd370193fc923201c3b27a99f848745db70a"
URL = f"https://huggingface.co/StemSplitio/htdemucs-onnx/resolve/{REV}/{FILE}"
SOURCES = ("drums", "bass", "other", "vocals")
SR = 44100
N = int(7.8 * SR)


def model_path(fetch):
    d = Path(os.environ.get("BEATBOX_MODELS", Path.home() / ".beatbox" / "models"))
    p = d / FILE
    if p.exists():
        return p
    if not fetch:
        raise RuntimeError(f"stem model not installed ({FILE}, 166 MB); run with --fetch (or fetch=true on the tool)")
    d.mkdir(parents=True, exist_ok=True)
    tmp = p.with_suffix(".part")
    h = hashlib.sha256()
    with urllib.request.urlopen(URL, timeout=60) as r, open(tmp, "wb") as f:
        while True:
            b = r.read(1 << 20)
            if not b:
                break
            h.update(b)
            f.write(b)
    if h.hexdigest() != SHA256:
        tmp.unlink(missing_ok=True)
        raise RuntimeError("stem model checksum mismatch; not installed")
    tmp.rename(p)
    return p


def load(path):
    """Any format -> (2, samples) float32 at 44.1 kHz via ffmpeg."""
    import numpy as np
    raw = subprocess.run(["ffmpeg", "-v", "quiet", "-i", str(path), "-ac", "2", "-ar", str(SR), "-f", "f32le", "-"],
                         capture_output=True, check=True).stdout
    x = np.frombuffer(raw, dtype=np.float32).reshape(-1, 2).T
    return np.ascontiguousarray(x)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("audio")
    ap.add_argument("out_dir")
    ap.add_argument("--stems", default=",".join(SOURCES))
    ap.add_argument("--threads", type=int, default=2)
    ap.add_argument("--fetch", action="store_true")
    a = ap.parse_args()
    try:
        import numpy as np
        import onnxruntime as ort
        import soundfile as sf
    except Exception as e:
        print(json.dumps({"error": f"needs numpy, onnxruntime, soundfile ({e}); pip install onnxruntime soundfile"}))
        return 2
    t0 = time.time()
    try:
        mp = model_path(a.fetch)
    except Exception as e:
        print(json.dumps({"error": str(e)}))
        return 2
    want = [s for s in a.stems.split(",") if s in SOURCES]
    mix = load(a.audio)
    total = mix.shape[1]
    so = ort.SessionOptions()
    so.intra_op_num_threads = max(1, a.threads)
    so.inter_op_num_threads = 1
    so.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_ALL
    sess = ort.InferenceSession(str(mp), sess_options=so, providers=["CPUExecutionProvider"])
    ov = N // 4
    stride = N - ov
    nch = max(1, (total + stride - 1) // stride)
    w = np.ones(N, dtype=np.float32)
    fade = np.linspace(0, 1, ov, dtype=np.float32)
    w[:ov] = fade
    w[-ov:] = fade[::-1]
    idx = [SOURCES.index(s) for s in want]
    out = np.zeros((len(idx), 2, total), dtype=np.float32)
    wt = np.zeros(total, dtype=np.float32)
    for i in range(nch):
        s0 = i * stride
        s1 = min(s0 + N, total)
        ch = mix[:, s0:s1]
        if ch.shape[1] < N:
            ch = np.pad(ch, ((0, 0), (0, N - ch.shape[1])))
        st = sess.run(["stems"], {"mix": ch[None].astype(np.float32)})[0][0]
        n = s1 - s0
        for j, k in enumerate(idx):
            out[j, :, s0:s1] += st[k, :, :n] * w[:n]
        wt[s0:s1] += w[:n]
    out /= np.maximum(wt, 1e-8)
    od = Path(a.out_dir)
    od.mkdir(parents=True, exist_ok=True)
    paths = {}
    for j, s in enumerate(want):
        p = od / f"{s}.wav"
        sf.write(str(p), out[j].T, SR, subtype="PCM_16")
        paths[s] = str(p)
    print(json.dumps({"stems": paths, "seconds": round(time.time() - t0, 1), "model": f"htdemucs (ONNX, {FILE})", "chunks": nch, "duration_s": round(total / SR, 2)}))
    return 0


if __name__ == "__main__":
    sys.exit(main())
