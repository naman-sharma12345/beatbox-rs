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
ROOT=$(cd "$(dirname "$0")/.." && pwd)
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
# a checked-in fixed project: deterministic (no seed), rendered as-is and
# loudness-measured like the generated beats. Not part of the blind A/B or
# the novelty matrix (beats_summary.py reads only the five beats above).
run_fixed() {
  local name=$1 proj=$2
  local dir="$OUT/${name}_${TAG}"
  mkdir -p "$dir"
  cp "$proj" "$dir/project.json"
  local t0=$(date +%s.%N)
  "$BIN" render "$proj" -o "$dir/render.wav" > "$dir/render.log" 2>&1
  local t1=$(date +%s.%N)
  echo "$t1 - $t0" | bc > "$OUT/${name}_${TAG}.wall"
  local final="$OUT/${name}_${TAG}.mp3"
  if command -v ffmpeg >/dev/null && [ -f "$dir/render.wav" ]; then
    ffmpeg -hide_banner -nostats -y -i "$dir/render.wav" -codec:a libmp3lame -b:a 192k "$final" 2>/dev/null || cp "$dir/render.wav" "${final%.mp3}.wav"
    ffmpeg -hide_banner -nostats -i "$final" -af ebur128=peak=true -f null - 2>&1 | awk '/Summary/{s=1} s&&/I:/{i=$2} s&&/Peak:/{p=$2} END{print i, p}' > "$OUT/${name}_${TAG}.loud"
  fi
  echo "$name: fixed project $(basename "$proj") rendered"
}
run desi_hiphop "{\"brief\":\"dark desi hip-hop with sitar, tabla and a hard 808, for a gully rap verse\",\"genre\":\"desi_hiphop\",$COMMON}"
run boom_bap "{\"brief\":\"90s boom bap with dusty jazzy keys\",\"genre\":\"boom_bap\",$COMMON}"
run drill "{\"brief\":\"UK drill with sliding 808s and an eerie choir\",\"genre\":\"drill\",$COMMON}"
# RT-effects showcase: fixed deterministic project (slice_repeat, streamed
# stutter, vocoder, granular, pitch shift, freq splitter), no novelty check
run_fixed rt_showcase "$ROOT/examples/beats/rt_showcase.json"
# lo-fi demo: the newly streamed effects (mod_delay, lofi, vinyl, freq_shift)
run_fixed rt_lofi "$ROOT/examples/beats/rt_lofi.json"
# house demo: the windowed-offset fixes (phaser, pitch_shift, ring_mod) on a
# generated house project (generate_beat style=house seed=7, self-contained)
run_fixed rt_house "$ROOT/examples/beats/rt_house.json"
# afrobeats demo: more streamed effects (chorus, flanger, autopan,
# transient) on a generated afrobeats project (generate_beat style=afrobeats
# seed=11, effects added, self-contained)
run_fixed rt_afro "$ROOT/examples/beats/rt_afro.json"
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
