#!/usr/bin/env python3
"""Fetch the vocal regression corpus: CC0 voice recordings from Wikimedia
Commons (different singers, mics, rooms and noise), each pinned by its
SHA-1, cut to a 12 s window and written as 48 kHz mono WAV.

Nothing here is committed: the files land in $BEATBOX_VOCAL_CORPUS (default
~/.beatbox/vocal_corpus) with a corpus.json the Rust tests read
(`cargo test vocal_corpus -- --nocapture`). Never add a private take
(yours or a client's) to this list or to git.

Usage: python3 scripts/fetch_vocal_corpus.py [--dir DIR] [--only NAME ...]
Needs ffmpeg on PATH. Standard library only.
"""
import argparse, hashlib, json, os, subprocess, sys, tempfile, urllib.request

CORPUS = [
 {
  "name": "female_vibrato",
  "page": "https://commons.wikimedia.org/wiki/File:Female-vibrato-1.wav",
  "url": "https://upload.wikimedia.org/wikipedia/commons/c/c3/Female-vibrato-1.wav",
  "sha1": "38a996baeeb86fa2e90ef89d0ca11ec3151caa59",
  "bytes": 1674808,
  "license": "CC0-1.0",
  "what": "solo female singing with vibrato, close mic, quiet room",
  "start_s": 0,
  "length_s": 12
 },
 {
  "name": "tuvan_self_recorded",
  "page": "https://commons.wikimedia.org/wiki/File:Tuvan_Internationale_Vocal,_Self_Recorded.wav",
  "url": "https://upload.wikimedia.org/wikipedia/commons/a/a8/Tuvan_Internationale_Vocal%2C_Self_Recorded.wav",
  "sha1": "435268f909dfaae03c84a34c50dea24f2393dc96",
  "bytes": 8902734,
  "license": "CC0-1.0",
  "what": "male singer, home recording, untreated room",
  "start_s": 0,
  "length_s": 12
 },
 {
  "name": "favorite_song_phrase",
  "page": "https://commons.wikimedia.org/wiki/File:My_favorite_song_is_(song).wav",
  "url": "https://upload.wikimedia.org/wikipedia/commons/f/f8/My_favorite_song_is_%28song%29.wav",
  "sha1": "e9152ad31781134397a220d145fc13de104b0dfa",
  "bytes": 171726,
  "license": "CC0-1.0",
  "what": "short sung phrase, consumer mic",
  "start_s": 0,
  "length_s": 12
 },
 {
  "name": "favela_street",
  "page": "https://commons.wikimedia.org/wiki/File:410435_felix-blume_two-people-singing-in-the-street-in-a-favela-rio-de-janeiro.ogg",
  "url": "https://upload.wikimedia.org/wikipedia/commons/c/ce/410435_felix-blume_two-people-singing-in-the-street-in-a-favela-rio-de-janeiro.ogg",
  "sha1": "5c49128063f1ed015ce7163d4c97c6013226717a",
  "bytes": 6636027,
  "license": "CC0-1.0",
  "what": "two people singing outdoors, street noise, field recorder",
  "start_s": 0,
  "length_s": 12
 },
 {
  "name": "kanun_live",
  "page": "https://commons.wikimedia.org/wiki/File:Kanun_music_and_singing_during_the_5th_anniversary_of_Wikimedia_Armenia.ogg",
  "url": "https://upload.wikimedia.org/wikipedia/commons/5/51/Kanun_music_and_singing_during_the_5th_anniversary_of_Wikimedia_Armenia.ogg",
  "sha1": "fce9d7553566f4bf4a87095875c00cce3701fd20",
  "bytes": 662167,
  "license": "CC0-1.0",
  "what": "singing over a kanun at a live event, room and crowd",
  "start_s": 0,
  "length_s": 12
 },
 {
  "name": "cogito_lofi",
  "page": "https://commons.wikimedia.org/wiki/File:Baz-gibbons_-_147_-_The_Cogito_Song_I_Dreamt_of_Descartes.ogg",
  "url": "https://upload.wikimedia.org/wikipedia/commons/8/86/Baz-gibbons_-_147_-_The_Cogito_Song_I_Dreamt_of_Descartes.ogg",
  "sha1": "e37ddf9cb4757424cad2b87c13fd98ecd63abda6",
  "bytes": 268736,
  "license": "CC0-1.0",
  "what": "lo-fi home song, voice with guitar",
  "start_s": 0,
  "length_s": 12
 },
 {
  "name": "church_choir",
  "page": "https://commons.wikimedia.org/wiki/File:Russian_Orthodox_Christmas_Church_singing_1.ogg",
  "url": "https://upload.wikimedia.org/wikipedia/commons/f/f2/Russian_Orthodox_Christmas_Church_singing_1.ogg",
  "sha1": "301bf7743e19a240c0eba5faa7646fddf0f4f35e",
  "bytes": 2775807,
  "license": "CC0-1.0",
  "what": "choir in a reverberant church, distant mic",
  "start_s": 0,
  "length_s": 12
 },
 {
  "name": "vocal_fry_speech",
  "page": "https://commons.wikimedia.org/wiki/File:Vocal_Fry.ogg",
  "url": "https://upload.wikimedia.org/wikipedia/commons/7/77/Vocal_Fry.ogg",
  "sha1": "5e561ea5e2f28e1a9287cfac2539eddf49617a3f",
  "bytes": 54291,
  "license": "CC0-1.0",
  "what": "speech in vocal fry register",
  "start_s": 0,
  "length_s": 12
 }
]

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dir", default=os.environ.get("BEATBOX_VOCAL_CORPUS", os.path.expanduser("~/.beatbox/vocal_corpus")))
    ap.add_argument("--only", nargs="*")
    a = ap.parse_args()
    os.makedirs(a.dir, exist_ok=True)
    done = []
    for c in CORPUS:
        if a.only and c["name"] not in a.only:
            continue
        out = os.path.join(a.dir, c["name"] + ".wav")
        if not os.path.exists(out):
            req = urllib.request.Request(c["url"], headers={"User-Agent": "beatbox-rs vocal corpus (https://github.com/naman-sharma12345/beatbox-rs)"})
            data = urllib.request.urlopen(req, timeout=120).read()
            got = hashlib.sha1(data).hexdigest()
            if got != c["sha1"]:
                print(f"{c['name']}: sha1 {got} != pinned {c['sha1']}, skipped", file=sys.stderr)
                continue
            with tempfile.NamedTemporaryFile(suffix=os.path.splitext(c["url"])[1]) as f:
                f.write(data)
                f.flush()
                subprocess.run(["ffmpeg", "-v", "error", "-y", "-ss", str(c["start_s"]), "-t", str(c["length_s"]), "-i", f.name,
                                "-ac", "1", "-ar", "48000", "-c:a", "pcm_s16le", out], check=True)
        done.append({k: c[k] for k in ("name", "page", "license", "what")} | {"file": out})
        print("ok", c["name"])
    with open(os.path.join(a.dir, "corpus.json"), "w") as f:
        json.dump({"clips": done, "source": "Wikimedia Commons, CC0-1.0, pinned by SHA-1"}, f, indent=1)
    print(f"{len(done)} clips in {a.dir}")

if __name__ == "__main__":
    main()
