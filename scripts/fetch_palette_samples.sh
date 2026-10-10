#!/usr/bin/env bash
# Fetch the curated CC0 palette one-shots (src/palette_samples.json) into
# DIR (default beatbox_samples/palette) and verify every SHA-256.
# Beatbox does the same on first use; this is for offline prep / CI caches.
set -euo pipefail
DIR=${1:-beatbox_samples/palette}
mkdir -p "$DIR"
python3 - "$DIR" <<'PY'
import hashlib, json, os, sys, urllib.request
d = sys.argv[1]
m = json.load(open("src/palette_samples.json"))  # run from the repo root
bad = 0
for s in m:
    ext = s["url"].rsplit(".", 1)[-1].lower()
    p = os.path.join(d, f"{s['id']}.{ext}")
    if not (os.path.exists(p) and hashlib.sha256(open(p, "rb").read()).hexdigest() == s["sha256"]):
        b = urllib.request.urlopen(s["url"], timeout=30).read()
        if hashlib.sha256(b).hexdigest() != s["sha256"]:
            print("CHECKSUM MISMATCH", s["id"]); bad += 1; continue
        open(p, "wb").write(b)
    print("ok", s["id"], s["license"], s["author"])
open(os.path.join(d, "LICENSE.txt"), "w").write("CC0 1.0 Universal. Sources: SAMPLES_LICENSES.md\n")
sys.exit(1 if bad else 0)
PY
