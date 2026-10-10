#!/usr/bin/env python3
"""Guide-vocal helper for beatbox (sing_lyrics / make_beat with lyrics).

Speaks every word of the lyrics with a small local TTS voice (piper, CPU,
~60 MB voice) into its own WAV, so beatbox can place each word on the grid
and tune it onto a written melody. Reads {"lines": [["word", ...], ...]}
on stdin, prints {"words": [{"line", "word", "path"}]}.
Usage: tts_words.py OUT_DIR [--voice en_US-lessac-medium] [--voice-dir DIR]
Install once: pip install piper-tts  (the voice downloads on first use)
"""
import argparse, json, os, sys, wave

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("out")
    ap.add_argument("--voice", default="en_US-lessac-medium")
    ap.add_argument("--voice-dir", default=None)
    ap.add_argument("--length-scale", type=float, default=1.0)
    a = ap.parse_args()
    try:
        from piper import PiperVoice
        from piper.config import SynthesisConfig
    except Exception as e:
        print(json.dumps({"error": f"piper-tts is not installed ({e}); pip install piper-tts"}))
        sys.exit(2)
    vdir = a.voice_dir or os.path.join(a.out, "..")
    model = os.path.join(vdir, a.voice + ".onnx")
    if not os.path.exists(model):
        os.makedirs(vdir, exist_ok=True)
        import subprocess
        subprocess.run([sys.executable, "-m", "piper.download_voices", a.voice], cwd=vdir, check=True,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    v = PiperVoice.load(model)
    cfg = SynthesisConfig(length_scale=a.length_scale)
    req = json.load(sys.stdin)
    os.makedirs(a.out, exist_ok=True)
    out = []
    for li, line in enumerate(req.get("lines", [])):
        for wi, word in enumerate(line):
            p = os.path.join(a.out, f"w_{li:03d}_{wi:03d}.wav")
            with wave.open(p, "wb") as w:
                v.synthesize_wav(word, w, syn_config=cfg)
            out.append({"line": li, "word": word, "path": p})
    print(json.dumps({"words": out, "voice": a.voice, "sample_rate": v.config.sample_rate}))

if __name__ == "__main__":
    main()
