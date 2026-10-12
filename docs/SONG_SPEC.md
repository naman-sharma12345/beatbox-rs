# Songs as data: SongSpec

One JSON in, one finished song out. `produce` → `song_create` composes the
beat to your structure and instruments, cuts and places the vocal take,
mixes it to measurable targets, delivers mp3/wav/flac and **checks** the
delivered file. `song_patch` changes any part by JSON path and redoes only
what that path touches.

```json
{"action": "song_create", "params": {
  "title": "nikal ja", "genre": "desi_hiphop", "bpm": 100, "key": "D", "scale": "harmonic_minor",
  "structure": [{"kind": "intro", "bars": 4}, {"kind": "verse", "bars": 16}, {"kind": "hook", "bars": 8},
                {"kind": "verse", "bars": 16}, {"kind": "hook", "bars": 8}, {"kind": "outro", "bars": 4}],
  "instruments": {"harmony": "catalog:euterpea_harmonium", "lead": "catalog:vcsl_alto_recorder",
                  "bayan": {"samples": ["/path/dagga_a.wav", "/path/dagga_b.wav"]}},
  "vocal": {"file": "/path/take.wav",
            "phrases": [{"src_s": 3.34, "len_s": 4.32, "section": "verse1", "bar": 1, "beat": 1},
                        {"src_s": 18.04, "len_s": 3.27, "section": "hook1", "bar": 0, "beat": 4}]},
  "export": {"path": "/tmp/song.mp3", "format": "mp3"}
}}
```

* **structure** is a contract: the sections and bar counts you give are the
  ones you get. Names default to kind + count (`verse1`, `hook2`).
* **instruments**: role → preset name, `catalog:<id>` (a recorded SFZ
  instrument), or `{"samples": [...]}` (one-shots played at the part's pitch).
* **vocal.phrases**: a piece of the take (`src_s` + `len_s`/`end_s`) at
  `section` + `bar` (1-based; 0 = the bar before, for pickups) + `beat`
  (1-based, fractions ok). Or `start_beat` / `at_s`. Phrases play whole, no
  time-stretch. Without phrases the take is cut at its pauses and laid in
  order from the first verse.
* **mix** / **master** / **export**: targets and delivery (defaults below).

## What every render checks (src/mix_rules.rs, the one home of these numbers)

| check | target | how it is reached |
|---|---|---|
| vocal_over_beat | ≥ +6 dB, ≤ +12 dB in every section with vocal | vocal TRACK fader automation + a gain stage after the compressor (never clip gain, which the 3:1 compressor eats) |
| drop_lift | hooks ≥ +2 dB over verses, on the delivered file | master section automation; deliver → measure → deepen verses → deliver |
| not_harsh | 2-5 kHz ≤ −17 dB, 5-10 kHz ≤ −18.5 dB against the 250 Hz-2 kHz body; every beat part ≥ 6 dB under the vocal at 2-5 kHz | the beat's own tone on the `beat` bus (3.15 kHz bell + 7.5 kHz shelf, up to 12 dB); the master tone (≤ 2 dB) only when the beat tone is at its limit, so the vocal keeps its top |
| loudness | −10 LUFS ± 1 | delivery stage (soft clip + true-peak limiter, section contrast kept) |
| true_peak | ≤ −1 dBTP, decoded file | delivery ceiling with margin for lossy encoding |

The vocal chain is the raw-take chain: HPF 85 Hz, light de-ess, 3:1
compression, small reverb/delay sends. No noise reduction, gate, tuning or
extra EQ. The 1.4 kHz pocket in the melodic beds follows any voice, also one
added after the beat.

## Patching

`song_patch {"set": {"/mix/vocal_over_beat_db": 8}}` → re-mix only.
`{"/vocal/phrases/3/beat": 2}` → rebuild the vocal track.
`{"/bpm": 96}`, `{"/instruments/lead": "sitar"}` → recompose with the same seed.
`{"remove": ["/vocal"]}` drops a part. Each call is one undo step.

`song_validate` lists every problem in a spec at once, each with its JSON
path and the fix. `song_schema` shows every field and default.
