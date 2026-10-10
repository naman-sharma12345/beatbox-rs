# The creative producer (sprint 9)

Sprint 8's beats were bit-identical to sprint 7: fixed seeds, fixed drum
templates, and a critic that pulled every beat toward the same arrangement.
Sprint 9 changes how decisions are made.

## Hierarchy (recorded in `plan.decisions`, each with its reason)

1. **Method**: `template` (the genre's own patterns, varied), `procedural`
   (generated from the genre's distributions), `reference_guided`
   (a reference recording's loudness and onset density steer energy, kick
   density and hat rate) or `ai_authored` (an agent passes notes, see below).
2. **Direction**: mood, energy, emotional intent, hero element
   (motif / groove / bass / texture), density, identity.
3. **Core material chosen by the direction**: tempo (position in the genre
   range follows energy), mode (moods prefer modes), key, harmony (the mood's
   progressions, the genre's others, modal/borrowed colour; a bridge
   progression; chord colour; harmonic rhythm), the main motif (contour and
   rhythm cell suit the mood and hero), the drum groove, swing, 808 mode and
   glide, chord rhythm, palette and kit, arrangement form, mix character.
4. **Controlled variation**: per-section groove treatment (verse thins, hook
   adds and may speed the hats, bridge strips or halves), phrase-end
   mutations, rolls (32nds, triplets, 1/32 and 1/64 bursts) and rate changes,
   ghost notes, generated fills (drop and roll, snare fill, triplet fill,
   stutter, kick drop, silence, turnaround, fade), motif development per
   section, a counter line that echoes the motif, and 1-3 wildcards.

## Genres as distributions

`playbooks.json` keeps every template. Each genre also has an optional
`generative` block (snare modes and variations, hat rates, roll and ghost
ranges, swing range, kick density, perc modes, extra modes and progressions,
harmonic rhythm, 808 modes, glide, chord styles, kits, drum sound
alternatives, arrangement forms). Missing fields are derived from the
playbook; kick probability grids come from the templates (the convention
prior) blended with a metric prior.

## Motif development

`hook1` states the motif with call and response; later hooks displace it,
sequence it up a third or restate it an octave down; the last hook lifts it
an octave; verses quote fragments; bridges invert it or play it backwards.
Operations: statement, call_response, inversion, retrograde, displace,
fragment, sequence, register_up, register_down.

## Wildcards with a purpose

| wildcard | role |
|---|---|
| half_time_hook | contrast |
| silence_before_drop | tension |
| beat_switch (a second groove for the back half) | contrast |
| odd_phrase (one bar shorter or longer, e.g. 7 bars) | tension |
| drum_dropout (two bars mid-verse) | release |
| unusual_instrument (counter/texture outside the genre palette) | emotion |
| key_change (last hook up 1-2 semitones) | emotion |
| bass_kick_call_response (808 plays in the kick's gaps) | groove |
| sparse_to_dense (verse builds hats, then kick, then everything) | tension |

## Structured intent

`plan_track` and `produce_track` take `intent`, which the connected model
fills in instead of relying on keyword matching of the brief:

```json
{"mood": "dark", "energy": 0.8, "emotion": "cold confidence", "hero": "groove",
 "density": "dense", "rhythmic_feel": ["half_time", "rolling"],
 "motif": {"contour": "descending", "rhythm": "syncopated", "density": 0.5},
 "palette": {"lead": "koto", "snare": "clap"},
 "contrasts": ["silence_before_drop", "key_change"]}
```

The result's `constraints` lists every requested constraint (top-level args
included) as applied, adjusted or ignored, with the reason.

## Seeds and reproducibility

Every seeded tool gets a fresh seed (OS entropy + time) when the call omits
it and returns it as `seed` with `seed_source: "fresh"`. An explicit seed
reproduces exactly. Plans carry `provenance`: seed, seed source, generator
version, build commit, the full config and an asset manifest (presets, kit
files and licence).

## Critics

- **Technical validator** (`critique_track`, the produce loop): vetoes
  failures only (loudness, true peak, clicks, masking/mud, a dead hook). It
  changes dynamics and levels (offsets capped at ±3 dB), never which parts
  play where. Its numbers are diagnostics, not quality.
- **Creative critic** (blind A/B): `blind_ab_create` loudness-matches 2-8
  renders into anonymous A/B/... WAVs with a rubric (identity, groove,
  development, memorability, emotion, production); the key is stored outside
  the session folder. `blind_ab_rate` records a human's preference and notes;
  `blind_ab_judge` asks a pluggable listening backend (`command`: an
  executable given the session folder that prints
  `{"preference", "notes": {label: {criterion: text}}, "comment"}`; set
  `BEATBOX_AB_JUDGE_CMD`). No paid backend is wired in; the default `stub`
  explains this. `blind_ab_reveal` joins notes to sources as revision hints
  and adds the result to the blind preference tally (`blind_ab_preferences`),
  which is kept apart from diagnostic scores.

## Novelty

`novelty.rs` fingerprints each beat (per-voice onset grids with genre
conventions masked, hat subdivision profile, transposition-invariant melodic
intervals and contour, key-relative bass roots, palette, section map,
tempo/key, key-relative chroma and spectrum; `embedding` is reserved for an
audio model). `produce_track` regenerates candidates closer than the
threshold (default 0.25) to recent beats when the seed is fresh, and records
every delivered beat in the history. `novelty_report` compares a project
with the history or with other projects (pairwise matrix). Novelty says a beat
is not a repeat; it is not a quality score.

## AI-authored MIDI

`author_midi` writes notes into a plan for a section (name, kind or `*`) and
role; `produce_track`/`plan_track` also accept `authored`. The composer plays
them verbatim and generates the rest.
