#!/usr/bin/env python3
"""Lyrics transcription helper for beatbox (vocal_to_song / transcribe_lyrics).

Runs a small local Whisper model (faster-whisper, CPU, int8) and prints JSON:
{"language", "text", "segments":[{"start","end","text","words":[{"start","end","word","prob"}]}]}
Usage: transcribe_lyrics.py AUDIO [--model base] [--language en] [--prompt "known lyrics"]
Install once: pip install faster-whisper   (the base model is ~145 MB, tiny ~75 MB)
"""
import argparse, json, sys

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("audio")
    ap.add_argument("--model", default="base")
    ap.add_argument("--language", default=None)
    ap.add_argument("--prompt", default=None)
    a = ap.parse_args()
    try:
        from faster_whisper import WhisperModel
    except Exception as e:  # pragma: no cover
        print(json.dumps({"error": f"faster-whisper is not installed ({e}); pip install faster-whisper"}))
        sys.exit(2)
    m = WhisperModel(a.model, device="cpu", compute_type="int8", cpu_threads=2)
    segs, info = m.transcribe(
        a.audio,
        language=a.language,
        initial_prompt=a.prompt,
        word_timestamps=True,
        vad_filter=True,
        beam_size=5,
        condition_on_previous_text=False,
    )
    out = {"language": info.language, "duration": round(info.duration, 3), "model": a.model, "segments": []}
    texts = []
    for s in segs:
        words = [
            {"start": round(w.start, 3), "end": round(w.end, 3), "word": w.word.strip(), "prob": round(w.probability, 3)}
            for w in (s.words or [])
        ]
        out["segments"].append({"start": round(s.start, 3), "end": round(s.end, 3), "text": s.text.strip(), "words": words})
        texts.append(s.text.strip())
    out["text"] = " ".join(texts)
    print(json.dumps(out))

if __name__ == "__main__":
    main()
