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
| not_harsh | on the delivered file, each band (4th-order HP + LP) minus integrated LUFS: 2-5 kHz between −23.0 and −22.4 dB (the reference master the person approved reads −22.4; under −23.0 the beat turns dull), 5-10 kHz between −26.0 and −24.3 dB (under −26 the hats have lost their sparkle) | 1) at the source, in every song (an instrumental keeps its bright parts 2 dB nearer the snare): hats/cymbals voiced darker (`SOURCE_TONE`), a 7.5 kHz shelf on the snare (−6) and clap (−4), hats/cymbals 13 dB and shakers 12 dB under the snare (`UNDER_SNARE_DB`, faders, only ever down), drum-bus grit at most 0.15; 2) every beat part ≥ 6 dB under the vocal at 2-5 kHz (a 3 kHz bell, up to 7 dB); 3) the `beat` bus takes what is left (aim ≤ 5 dB, at most 12); 4) the master tone (≤ 2 dB) only when the bus is at its limit. The delivery loop aims 2-5 kHz at the middle of its window from both sides; if 5-10 kHz reads dull it gives the bright parts back part of what step 1 took (≤ 4 dB a round, never more than was taken) |
| quiet sections | intro / outro / breakdown at most 9 dB under the verses (mix RMS) | master section automation (beat and vocal together, so the vocal over beat holds) |
| loudness | −10 LUFS ± 1 | delivery stage (soft clip + true-peak limiter, section contrast kept) |
| true_peak | ≤ −1 dBTP, decoded file | delivery ceiling with margin for lossy encoding |

The vocal chain is the raw-take chain: HPF 85 Hz, light de-ess, 3:1
compression, small reverb/delay sends. No noise reduction, gate, tuning or
extra EQ. The 1.4 kHz pocket in the melodic beds follows any voice, also one
added after the beat.

Calling an action on the wrong tool (for example `export {"action":"render"}`)
runs it in the tool it belongs to and adds a `note` naming that tool: the
call never fails just because the category guess was off.

## Genres in one word

`genre` picks a playbook (src/playbooks.json): tempo range, scales,
progressions, drum grammar, instruments, arrangement and the genre's own
level offsets (`mix.offsets`). Aliases and artist names resolve too.

| genre | say it as | what you get |
|---|---|---|
| `punjabi_party` | `honey_singh`, `badshah`, `bhangra`, `punjabi`, `desi_party`; in a prompt "yo yo honey singh type beat", "badshah party song" | 96-104 BPM natural minor, a dhol chaal on the tabla/bayan roles (dholak voices; give real dhol one-shots with `{"samples": [...]}`), 808 kick and bass, claps on 2 and 4, a `tumbi` hook riff, brass/string stabs; the dhol sits forward |
| `desi_hiphop` | `desi`, `punjabi_rap`, `bollywood_rap`, "bohemia type beat" | 88-96 BPM, tabla bols, sitar/bansuri lead in a raag-flavoured scale, tanpura drone, 808 |
| `drill` | `punjabi_drill`, "sidhu type beat" | 138-146 BPM sliding 808s |

The smallest Punjabi party song is one call:

```json
{"action": "song_create", "params": {"genre": "punjabi_party", "vocal": {"file": "/path/take.wav"}}}
```

## Self-critique: song_critique and song_improve

A song judges itself; no ear in the loop is needed.

* `song_critique` measures the delivered file against the genre's **target
  profile** (data: `src/genre_profiles.json`, one entry per playbook genre,
  each filled from `default`): sub/808 weight (30-80 Hz minus LUFS), hook lift
  over the verse, loudness range, stereo width (side over mid), groove density
  (onsets per second), the hook's build (hook onsets over verse onsets), plus
  the song checks' own tone floor/ceiling and vocal-over-beat. Every weakness
  comes back in plain words with the range it was held to, a severity
  (range-widths outside, biggest first) and the spec change that owns it.
  `{"file": ..., "genre": ...}` judges any audio file (no fixes: a file has no spec).
* `song_improve {"rounds": 2}` applies the fixes for the biggest weaknesses
  (up to 3 a round), rebuilds, critiques again and **keeps the best take**
  (a round that scores clearly worse is rolled back and its fix is not tried again; one that comes out only a little worse is built on, so a hook lift that cost a little air gets the air back next round).
  The song's file ends as the best take; the spec keeps the knobs that made it.
  `song_create {..., "improve": 2}` builds and improves in one call.
* The knobs it turns: `/mix/levels/<role>` (fader offsets, -12..12 dB, applied
  when the beat is composed: a change recomposes with the same seed),
  `/mix/drop_lift_db`, `/density`, `/master/preserve_dynamics`,
  `/mix/vocal_over_beat_db`. Width, a hook that jumps too far and harshness
  are named but have no automatic fix (the render's own tone rules already
  own the top end). The vocal is never EQ'd for any of it.
* The profiles are calibrated on the mix the person approved (song2) and on
  genre conventions, not on commercial reference files (none are bundled).

## Patching

`song_patch {"set": {"/mix/vocal_over_beat_db": 8}}` → re-mix only.
`{"/vocal/phrases/3/beat": 2}` → rebuild the vocal track.
`{"/bpm": 96}`, `{"/instruments/lead": "sitar"}` → recompose with the same seed.
`{"remove": ["/vocal"]}` drops a part. Each call is one undo step.

`song_validate` lists every problem in a spec at once, each with its JSON
path and the fix. `song_schema` shows every field and default.
