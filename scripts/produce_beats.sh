#!/usr/bin/env bash
# Produce the reference beats with produce_track and record wall time,
# scores, iterations (with the ears' accept/reject verdicts), LUFS and TP.
# usage: produce_beats.sh BIN OUT [TAG]   (TAG versions the file names, e.g. s7)
set -euo pipefail
BIN=${1:-target/release/beatbox}
OUT=${2:-beats}
TAG=${3:-${BEATS_TAG:-dev}}
mkdir -p "$OUT"
SUMMARY="$OUT/SUMMARY.md"
echo "| beat | score | technical | musical | iterations (kept) | wall s | LUFS | dBTP | file |" > "$SUMMARY"
echo "|---|---|---|---|---|---|---|---|---|" >> "$SUMMARY"
run() {
  local name=$1 args=$2
  local dir="$OUT/${name}_${TAG}"
  mkdir -p "$dir"
  local t0=$(date +%s.%N)
  "$BIN" call produce_track "${args/__OUT__/$dir}" > "$OUT/${name}_${TAG}.result.json"
  local t1=$(date +%s.%N)
  local audio=$(python3 -c "import json,sys;print(json.load(open(sys.argv[1]))['files'].get('audio',''))" "$OUT/${name}_${TAG}.result.json")
  local final="$OUT/${name}_${TAG}.mp3"
  [ -n "$audio" ] && cp "$audio" "$final"
  local meas="n/a n/a"
  if command -v ffmpeg >/dev/null && [ -f "$final" ]; then
    meas=$(ffmpeg -hide_banner -nostats -i "$final" -af ebur128=peak=true -f null - 2>&1 | awk '/Summary/{s=1} s&&/I:/{i=$2} s&&/Peak:/{p=$2} END{print i, p}')
  fi
  python3 - "$OUT/${name}_${TAG}.result.json" "$name" "$(echo "$t1 - $t0" | bc)" "$meas" "$final" "$SUMMARY" <<'PY'
import json, sys
r = json.load(open(sys.argv[1]))
name, wall, meas, final, summ = sys.argv[2], float(sys.argv[3]), sys.argv[4].split(), sys.argv[5], sys.argv[6]
its = r['iterations']
kept = sum(1 for i in its if i.get('accepted', True))
print(f"{name}: score {r['score']} (tech {r['technical']}, musical {r['musical']}), "
      f"{len(its)} iterations ({kept} kept), {wall:.1f} s wall, LUFS {meas[0]} TP {meas[1]}, "
      f"{r['genre']} {r['key']} {r['bpm']} BPM -> {final}")
for it in its:
    print("   iter", it['iteration'], it['score'], it.get('verdict'), 'kept' if it.get('accepted', True) else 'ROLLED BACK', it.get('revisions', []))
print("   remaining:", r.get('remaining_findings', [])[:4])
with open(summ, 'a') as f:
    f.write(f"| {name} | {r['score']} | {r['technical']} | {r['musical']} | {len(its)} ({kept}) | {wall:.0f} | {meas[0]} | {meas[1]} | {final.split('/')[-1]} |\n")
PY
}
run desi_hiphop '{"brief":"dark desi hip-hop with sitar, tabla and a hard 808, for a gully rap verse","genre":"desi_hiphop","bpm":92,"seed":21,"max_iterations":4,"out_dir":"__OUT__"}'
run melodic_rap '{"brief":"sad melodic trap with piano and gliding 808s","genre":"melodic_rap","bpm":144,"seed":7,"max_iterations":4,"out_dir":"__OUT__"}'
run boom_bap '{"brief":"90s boom bap with dusty jazzy keys","genre":"boom_bap","bpm":90,"seed":3,"max_iterations":4,"out_dir":"__OUT__"}'
if [ "${BEATS_ALL:-1}" = "1" ]; then
  run trap '{"brief":"dark hard trap with rolling hats and a distorted 808","genre":"trap","bpm":140,"seed":11,"max_iterations":4,"out_dir":"__OUT__"}'
  run drill '{"brief":"UK drill with sliding 808s and an eerie choir","genre":"drill","bpm":142,"seed":5,"max_iterations":4,"out_dir":"__OUT__"}'
fi
cat "$SUMMARY"
