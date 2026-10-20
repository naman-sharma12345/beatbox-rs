#!/usr/bin/env python3
"""Lyrics transcription helper for beatbox (vocal_to_song / transcribe_lyrics).

Runs a small local Whisper model (faster-whisper, CPU, int8) and prints JSON:
{"language", "text", "segments":[{"start","end","text","words":[{"start","end","word","prob"}]}]}
Usage: transcribe_lyrics.py AUDIO [--model base] [--language en] [--prompt "known lyrics"] [--chunk-s 30]
--chunk-s N: the fallback for long free-flowing takes (a Hindi rap): N-second
chunks, voice-activity filter, no word timestamps (segment times only), and
a segment that repeats itself (a decoder loop) is dropped and counted.
Install once: pip install faster-whisper   (the base model is ~145 MB, tiny ~75 MB)
"""
import argparse, json, sys

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("audio")
    ap.add_argument("--model", default="base")
    ap.add_argument("--language", default=None)
    ap.add_argument("--prompt", default=None)
    ap.add_argument("--no-vad", action="store_true", help="whole records: keep lines sung under a band")
    ap.add_argument("--beam", type=int, default=5)
    ap.add_argument("--threads", type=int, default=2)
    ap.add_argument("--chunk-s", type=float, default=0.0)
    a = ap.parse_args()
    try:
        from faster_whisper import WhisperModel
    except Exception as e:  # pragma: no cover
        print(json.dumps({"error": f"faster-whisper is not installed ({e}); pip install faster-whisper"}))
        sys.exit(2)
    m = WhisperModel(a.model, device="cpu", compute_type="int8", cpu_threads=a.threads)
    if a.chunk_s > 0:
        return chunked(m, a)
    segs, info = m.transcribe(
        a.audio,
        language=a.language,
        initial_prompt=a.prompt,
        word_timestamps=True,
        vad_filter=not a.no_vad,
        beam_size=a.beam,
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

def looped(text):
    """A decoder loop: the same short phrase over and over."""
    w = text.split()
    if len(w) < 8:
        return False
    for n in (1, 2, 3, 4):
        grams = [" ".join(w[i:i + n]) for i in range(0, len(w) - n + 1)]
        if grams:
            top = max(grams.count(g) for g in set(grams))
            if top * n >= 0.6 * len(w) and top >= 4:
                return True
    return len(set(w)) <= max(2, len(w) // 6)


def chunked(m, a):
    from faster_whisper import decode_audio
    x = decode_audio(a.audio, sampling_rate=16000)
    sr = 16000
    step = int(a.chunk_s * sr)
    out = {"language": a.language, "duration": round(len(x) / sr, 3), "model": a.model, "mode": f"chunked {a.chunk_s:g} s, no word times", "segments": [], "dropped_loops": 0}
    texts = []
    for c0 in range(0, len(x), step):
        seg_audio = x[c0:c0 + step]
        if len(seg_audio) < sr // 2:
            continue
        segs, info = m.transcribe(
            seg_audio,
            language=a.language,
            initial_prompt=a.prompt,
            word_timestamps=False,
            vad_filter=True,
            beam_size=a.beam,
            condition_on_previous_text=False,
            compression_ratio_threshold=2.2,
            # a rapped Hindi take reads as "no speech" (p ~0.9) to the model;
            # with these on, whole chunks vanished (2026-10-12: 9 lines in
            # 246 s). VAD already drops the silences and `looped` the loops.
            no_speech_threshold=None,
            log_prob_threshold=None,
        )
        if out["language"] is None:
            out["language"] = info.language
        t0 = c0 / sr
        for s in segs:
            t = s.text.strip()
            if not t:
                continue
            if looped(t) or (s.compression_ratio or 0) > 2.4:
                out["dropped_loops"] += 1
                continue
            out["segments"].append({"start": round(t0 + s.start, 3), "end": round(t0 + s.end, 3), "text": t, "words": []})
            texts.append(t)
    out["text"] = " ".join(texts)
    print(json.dumps(out, ensure_ascii=False))


if __name__ == "__main__":
    main()
