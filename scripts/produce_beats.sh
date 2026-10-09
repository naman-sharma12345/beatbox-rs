#!/usr/bin/env bash
# Produce the reference beats with produce_track and record wall time / scores.
set -euo pipefail
BIN=${1:-target/release/beatbox}
OUT=${2:-beats}
mkdir -p "$OUT"
run() {
  local name=$1 args=$2
  local t0=$(date +%s.%N)
  "$BIN" call produce_track "$args" > "$OUT/$name.result.json"
  local t1=$(date +%s.%N)
  python3 - "$OUT/$name.result.json" "$name" "$(echo "$t1 - $t0" | bc)" <<'PY'
import json, sys
r = json.load(open(sys.argv[1]))
print(f"{sys.argv[2]}: score {r['score']} (tech {r['technical']}, musical {r['musical']}), "
      f"{len(r['iterations'])} iterations, {r['wall_seconds']} s inside, {float(sys.argv[3]):.1f} s wall, "
      f"{r['genre']} {r['key']} {r['bpm']} BPM -> {r['files'].get('audio')}")
for it in r['iterations']:
    print("   iter", it['iteration'], it['score'], it.get('revisions', []))
PY
}
run desi_hiphop '{"brief":"dark desi hip-hop with sitar, tabla and a hard 808, for a gully rap verse","genre":"desi_hiphop","bpm":92,"seed":21,"max_iterations":4,"out_dir":"'"$OUT"'"}'
run melodic_trap '{"brief":"sad melodic trap with piano and gliding 808s","genre":"melodic_rap","bpm":144,"seed":7,"max_iterations":4,"out_dir":"'"$OUT"'"}'
run boom_bap '{"brief":"90s boom bap with dusty jazzy keys","genre":"boom_bap","bpm":90,"seed":3,"max_iterations":4,"out_dir":"'"$OUT"'"}'
