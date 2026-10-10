#!/usr/bin/env bash
# Render an 8-bar demo per sound palette: the same generated beat (same
# style, key, seed) with the stock voices and with the palette applied
# (synth voices, and the CC0 sample kit where the palette has one).
# usage: palette_demos.sh BIN OUT_DIR [BASE_BIN]
#   BASE_BIN (optional): an older beatbox binary for "before" renders of
#   the identical beat on the previous engine.
set -euo pipefail
BIN=$(realpath "$1")
OUT=$(realpath -m "${2:-palette_demos}")
BASE=${3:-}
mkdir -p "$OUT"
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
# palette  style  key  bpm  seed
DEMOS="dark_trap trap F 140 11
dhh_grit desi_hiphop D 92 21
boom_bap_dusty boom_bap A 90 3
drill_slide drill G 142 5
melodic_airy trap C 136 7"
gen() { # bin project style key bpm seed
  "$1" --workdir "$WORK" call generate_beat "{\"style\":\"$3\",\"key\":\"$4\",\"bars\":2,\"seed\":$6}" --project "$2" >/dev/null 2>&1
  "$1" --workdir "$WORK" call set_tempo "{\"bpm\":$5}" --project "$2" >/dev/null 2>&1 || true
}
render() { # bin project wav
  "$1" --workdir "$WORK" call render "{\"path\":\"$3\"}" --project "$2" >/dev/null
}
echo "$DEMOS" | while read -r pal style key bpm seed; do
  p="$WORK/$pal.json"
  gen "$BIN" "$p" "$style" "$key" "$bpm" "$seed"
  render "$BIN" "$p" "$OUT/${pal}__stock_voices.wav"
  cp "$p" "$WORK/${pal}_samples.json"
  "$BIN" --workdir "$WORK" call apply_palette "{\"palette\":\"$pal\"}" --project "$p" > "$OUT/${pal}__apply.json"
  render "$BIN" "$p" "$OUT/${pal}__palette.wav"
  q="$WORK/${pal}_samples.json"
  "$BIN" --workdir "$WORK" call apply_palette "{\"palette\":\"$pal\",\"use_samples\":true}" --project "$q" > "$OUT/${pal}__apply_samples.json"
  render "$BIN" "$q" "$OUT/${pal}__palette_samples.wav"
  if [ -n "$BASE" ]; then
    b="$WORK/${pal}_base.json"
    gen "$BASE" "$b" "$style" "$key" "$bpm" "$seed"
    render "$BASE" "$b" "$OUT/${pal}__before_engine.wav"
  fi
  echo "rendered $pal"
done
