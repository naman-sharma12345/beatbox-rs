#!/usr/bin/env python3
"""DNSMOS P.835 scorer (SIG / BAK / OVRL) for beatbox's score_voice_quality.

Uses Microsoft's DNSMOS sig_bak_ovr.onnx (MIT, github.com/microsoft/DNS-Challenge,
DNSMOS/DNSMOS/sig_bak_ovr.onnx), downloaded once into the model dir given with
--models (sha256 checked). The scoring follows the reference dnsmos_local.py:
16 kHz mono, 9.01 s windows hopped by 1 s, raw outputs mapped through the
published non-personalized polynomials, averaged over windows.

Usage: dnsmos.py FILE [FILE ...] --models DIR   -> JSON on stdout
Needs numpy and onnxruntime. Decodes WAV itself; other formats need ffmpeg.
"""
import json, os, subprocess, sys, urllib.request, hashlib, wave

URL = "https://github.com/microsoft/DNS-Challenge/raw/master/DNSMOS/DNSMOS/sig_bak_ovr.onnx"
SHA = "269fbebdb513aa23cddfbb593542ecc540284a91849ac50516870e1ac78f6edd"
SR = 16000
LEN = 9.01


def model(dirpath):
    os.makedirs(dirpath, exist_ok=True)
    p = os.path.join(dirpath, "sig_bak_ovr.onnx")
    if not os.path.exists(p):
        tmp = p + ".part"
        urllib.request.urlretrieve(URL, tmp)
        h = hashlib.sha256(open(tmp, "rb").read()).hexdigest()
        if h != SHA:
            os.remove(tmp)
            raise RuntimeError("DNSMOS model checksum mismatch")
        os.replace(tmp, p)
    return p


def load(path):
    import numpy as np
    try:
        with wave.open(path, "rb") as w:
            n, ch, sw, sr = w.getnframes(), w.getnchannels(), w.getsampwidth(), w.getframerate()
            raw = w.readframes(n)
        if sw != 2:
            raise ValueError("not 16-bit")
        x = np.frombuffer(raw, dtype=np.int16).astype(np.float32) / 32768.0
        if ch > 1:
            x = x.reshape(-1, ch).mean(axis=1)
        if sr != SR:
            t = np.arange(int(len(x) * SR / sr)) * (sr / SR)
            x = np.interp(t, np.arange(len(x)), x).astype(np.float32)
        return x
    except Exception:
        out = subprocess.run(["ffmpeg", "-v", "quiet", "-i", path, "-ac", "1", "-ar", str(SR), "-f", "f32le", "-"], capture_output=True, check=True).stdout
        return np.frombuffer(out, dtype=np.float32).copy()


def score(sess, x):
    import numpy as np
    p_ovr = np.poly1d([-0.06766283, 1.11546468, 0.04602535])
    p_sig = np.poly1d([-0.08397278, 1.22083953, 0.0052439])
    p_bak = np.poly1d([-0.13166888, 1.60915514, -0.39604546])
    n = int(LEN * SR)
    while len(x) < n:
        x = np.append(x, x)
    hops = int(np.floor(len(x) / SR) - LEN) + 1
    s, b, o = [], [], []
    for i in range(max(hops, 1)):
        seg = x[i * SR: i * SR + n]
        if len(seg) < n:
            continue
        r = sess.run(None, {"input_1": seg[np.newaxis, :].astype(np.float32)})[0][0]
        s.append(p_sig(r[0])); b.append(p_bak(r[1])); o.append(p_ovr(r[2]))
    return {"SIG": round(float(np.mean(s)), 3), "BAK": round(float(np.mean(b)), 3), "OVRL": round(float(np.mean(o)), 3), "windows": len(s)}


def main():
    args = sys.argv[1:]
    mdir = os.path.expanduser("~/.cache/beatbox_dnsmos")
    if "--models" in args:
        i = args.index("--models"); mdir = args[i + 1]; del args[i:i + 2]
    try:
        import onnxruntime as ort
        sess = ort.InferenceSession(model(mdir), providers=["CPUExecutionProvider"])
        res = {}
        for f in args:
            res[f] = score(sess, load(f))
        print(json.dumps({"scores": res}))
    except Exception as e:
        print(json.dumps({"error": f"DNSMOS helper: {e}"}))


if __name__ == "__main__":
    main()
