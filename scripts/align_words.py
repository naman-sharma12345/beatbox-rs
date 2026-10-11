#!/usr/bin/env python3
"""Line sync helper for beatbox (align_phrases method "words").

Transcribes a sung take and a record's vocal stem with faster-whisper (word
timestamps), matches the two word sequences in memory (longest common
subsequence of normalised tokens) and prints ONLY phrase times as JSON:
{"phrases": [{"from_s", "to_s", "at_s", "score", "followed"}], "matched", "take_words"}.
No words are written, printed or logged: the transcripts live only in this
process. Model: faster-whisper (MIT), using a model already in the local cache
unless --allow-download.

Usage: align_words.py TAKE REF_VOCAL [--model small] [--threads 2] [--pause 0.45] [--max-phrase 6] [--allow-download]
"""
import argparse, json, os, re, sys, unicodedata


def norm(w):
    w = unicodedata.normalize("NFKD", w).lower()
    return re.sub(r"[^a-z0-9']", "", w).strip("'")


def words(model, path):
    segs, _ = model.transcribe(path, word_timestamps=True, vad_filter=True, beam_size=1, condition_on_previous_text=False)
    out = []
    for s in segs:
        for w in s.words or []:
            t = norm(w.word)
            if t:
                out.append((t, float(w.start), float(w.end)))
    return out


def lcs_pairs(a, b):
    n, m = len(a), len(b)
    # banded memory: full table is fine for a few thousand words
    L = [[0] * (m + 1) for _ in range(n + 1)]
    for i in range(n - 1, -1, -1):
        Li, Li1 = L[i], L[i + 1]
        ai = a[i]
        for j in range(m - 1, -1, -1):
            Li[j] = Li1[j + 1] + 1 if ai == b[j] else max(Li1[j], Li[j + 1])
    i = j = 0
    pairs = []
    while i < n and j < m:
        if a[i] == b[j]:
            pairs.append((i, j)); i += 1; j += 1
        elif L[i + 1][j] >= L[i][j + 1]:
            i += 1
        else:
            j += 1
    return pairs


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("take"); ap.add_argument("ref")
    ap.add_argument("--model", default="small")
    ap.add_argument("--threads", type=int, default=2)
    ap.add_argument("--pause", type=float, default=0.45)
    ap.add_argument("--max-phrase", type=float, default=6.0)
    ap.add_argument("--allow-download", action="store_true")
    a = ap.parse_args()
    try:
        from faster_whisper import WhisperModel
    except Exception as e:
        print(json.dumps({"error": f"needs faster-whisper ({e})"})); return 2
    try:
        m = WhisperModel(a.model, device="cpu", compute_type="int8", cpu_threads=max(1, a.threads),
                         local_files_only=not a.allow_download)
    except Exception as e:
        print(json.dumps({"error": f"speech model '{a.model}' not available locally ({type(e).__name__})"})); return 2
    tw = words(m, a.take)
    rw = words(m, a.ref)
    if len(tw) < 4 or len(rw) < 4:
        print(json.dumps({"error": "too few sung words found to sync lines"})); return 2
    pairs = lcs_pairs([w[0] for w in tw], [w[0] for w in rw])
    rec_of = {i: rw[j][1] for i, j in pairs}
    # phrases: take words split at pauses, long ones at their widest gap
    ph = []
    s = 0
    for i in range(1, len(tw) + 1):
        if i == len(tw) or tw[i][1] - tw[i - 1][2] > a.pause:
            ph.append([s, i]); s = i
    k = 0
    while k < len(ph):
        p0, p1 = ph[k]
        if tw[p1 - 1][2] - tw[p0][1] > a.max_phrase and p1 - p0 >= 4:
            cands = [q for q in range(p0 + 1, p1) if tw[q][1] - tw[p0][1] >= 1.5 and tw[p1 - 1][2] - tw[q][1] >= 1.5]
            if cands:
                q = max(cands, key=lambda q: tw[q][1] - tw[q - 1][2])
                ph[k:k + 1] = [[p0, q], [q, p1]]
                continue
        k += 1
    out = []
    for p0, p1 in ph:
        offs = [rec_of[i] - tw[i][1] for i in range(p0, p1) if i in rec_of]
        n = p1 - p0
        if offs:
            offs.sort()
            off = offs[len(offs) // 2]
            out.append({"from_s": round(tw[p0][1], 3), "to_s": round(tw[p1 - 1][2], 3), "off": off,
                        "score": round(len(offs) / n, 3), "followed": False})
        else:
            out.append({"from_s": round(tw[p0][1], 3), "to_s": round(tw[p1 - 1][2], 3), "off": None, "score": 0.0, "followed": True})
    # unmatched phrases keep the singer's own spacing from the nearest matched neighbour
    for idx, p in enumerate(out):
        if p["off"] is None:
            prev = next((out[j]["off"] for j in range(idx - 1, -1, -1) if out[j]["off"] is not None and not out[j]["followed"]), None)
            nxt = next((out[j]["off"] for j in range(idx + 1, len(out)) if out[j]["off"] is not None and not out[j]["followed"]), None)
            p["off"] = prev if prev is not None else (nxt if nxt is not None else 0.0)
    # keep lines in order and apart: a phrase never starts before the previous one ends
    last_end = -1e9
    for p in out:
        at = p["from_s"] + p["off"]
        if at < last_end + 0.05:
            # the singer runs a little ahead of the record: start the line as the last one ends
            p["nudged_s"] = round(last_end + 0.05 - at, 3)
            at = last_end + 0.05
        p["at_s"] = round(at, 3)
        last_end = at + (p["to_s"] - p["from_s"])
        del p["off"]
    print(json.dumps({"phrases": out, "matched": len(pairs), "take_words": len(tw), "method": "speech-recognised word timing (words kept in memory only)"}))
    return 0


if __name__ == "__main__":
    sys.exit(main())
