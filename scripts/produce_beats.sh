#!/usr/bin/env bash
# Produce the reference beats with produce_track (fresh random seeds, logged),
# check novelty against each other and the previous sprint, build a blind
# A/B session for the two trap beats and write SUMMARY.md.
# usage: produce_beats.sh BIN OUT [TAG] [PREV_DIR]
#   PREV_DIR: the previous sprint's beats (folders with project + plan + mp3)
set -euo pipefail
BIN=${1:-target/release/beatbox}
OUT=${2:-beats}
TAG=${3:-${BEATS_TAG:-dev}}
PREV=${4:-}
mkdir -p "$OUT"
OUT=$(cd "$OUT" && pwd)
HIST="$OUT/novelty_history.jsonl"
rm -f "$HIST"
# the previous sprint joins the history first, so near-repeats of it are regenerated
if [ -n "$PREV" ] && [ -d "$PREV" ]; then
  for d in "$PREV"/*/; do
    [ -d "$d" ] || continue
    ls "$d"/*.json >/dev/null 2>&1 || continue
    "$BIN" call novelty_report "{\"project\":\"$d\",\"history_path\":\"$HIST\",\"add_to_history\":true}" > /dev/null || echo "skip $d"
  done
fi
run() {
  local name=$1 args=$2
  local dir="$OUT/${name}_${TAG}"
  mkdir -p "$dir"
  local t0=$(date +%s.%N)
  "$BIN" call produce_track "${args/__OUT__/$dir}" > "$OUT/${name}_${TAG}.result.json"
  local t1=$(date +%s.%N)
  echo "$t1 - $t0" | bc > "$OUT/${name}_${TAG}.wall"
  local audio=$(python3 -c "import json,sys;print(json.load(open(sys.argv[1]))['files'].get('audio',''))" "$OUT/${name}_${TAG}.result.json")
  local final="$OUT/${name}_${TAG}.mp3"
  [ -n "$audio" ] && cp "$audio" "$final"
  if command -v ffmpeg >/dev/null && [ -f "$final" ]; then
    ffmpeg -hide_banner -nostats -i "$final" -af ebur128=peak=true -f null - 2>&1 | awk '/Summary/{s=1} s&&/I:/{i=$2} s&&/Peak:/{p=$2} END{print i, p}' > "$OUT/${name}_${TAG}.loud"
  fi
  python3 - "$OUT/${name}_${TAG}.result.json" "$name" <<'PY'
import json, sys
r = json.load(open(sys.argv[1]))
print(f"{sys.argv[2]}: seed {r['seed']} ({r['plan']['provenance'].get('seed_source')}), method {r['method']}, {r['genre']} {r['key']} {r['bpm']} BPM, score {r['score']}")
print("   direction:", (r.get('direction') or {}).get('identity'))
print("   groove:", r.get('groove'))
print("   wildcards:", [(w['name'], w['role']) for w in r.get('wildcards', [])])
print("   novelty:", (r.get('novelty') or {}).get('nearest_distance'), (r.get('novelty') or {}).get('regenerated'))
PY
}
# no seeds: every beat gets a fresh one (recorded in its plan and the summary)
COMMON="\"max_iterations\":4,\"history_path\":\"$HIST\",\"out_dir\":\"__OUT__\""
run trap_1 "{\"brief\":\"dark hard trap with rolling hats and a distorted 808\",\"genre\":\"trap\",$COMMON}"
run trap_2 "{\"brief\":\"dark hard trap with rolling hats and a distorted 808\",\"genre\":\"trap\",$COMMON}"
run desi_hiphop "{\"brief\":\"dark desi hip-hop with sitar, tabla and a hard 808, for a gully rap verse\",\"genre\":\"desi_hiphop\",$COMMON}"
run boom_bap "{\"brief\":\"90s boom bap with dusty jazzy keys\",\"genre\":\"boom_bap\",$COMMON}"
run drill "{\"brief\":\"UK drill with sliding 808s and an eerie choir\",\"genre\":\"drill\",$COMMON}"
# novelty matrix: s9 against itself and the previous sprint
CMP=""
for n in trap_2 desi_hiphop boom_bap drill; do CMP="$CMP\"$OUT/${n}_${TAG}\","; done
if [ -n "$PREV" ] && [ -d "$PREV" ]; then
  for d in "$PREV"/*/; do ls "$d"/*.json >/dev/null 2>&1 && CMP="$CMP\"${d%/}\","; done
fi
CMP="[${CMP%,}]"
"$BIN" call novelty_report "{\"project\":\"$OUT/trap_1_${TAG}\",\"compare\":$CMP,\"history_path\":\"$OUT/none.jsonl\"}" > "$OUT/novelty_matrix.json"
# blind A/B of the two trap beats (loudness-matched, shuffled, no metadata)
"$BIN" call blind_ab_create "{\"files\":[\"$OUT/trap_1_${TAG}.mp3\",\"$OUT/trap_2_${TAG}.mp3\"],\"names\":[\"trap_1\",\"trap_2\"],\"out_dir\":\"$OUT/blind\"}" > "$OUT/blind_session.json"
python3 "$(dirname "$0")/beats_summary.py" "$OUT" "$TAG"
cat "$OUT/SUMMARY.md"
