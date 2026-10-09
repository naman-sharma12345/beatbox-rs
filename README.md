# Beatbox 🥁

**An AI-native beat maker written in Rust.** Every feature — synths, drums, samples, effects, music theory, arrangement, mixing and mix analysis — is a tool an AI model can call over [MCP](https://modelcontextprotocol.io). Humans get a CLI and a native desktop studio (egui) on the same engine, and can watch an AI produce live.

> FL Studio was built for hands on a mouse. Beatbox is built for models: every knob is addressable, every action is undoable, and the engine can *listen back* to its own mix and tell the AI what to fix.

![Beatbox Studio: mixer view](docs/studio.png)

![Automation editor](docs/automation.png)

## Why it's different

| | Typical DAW | Beatbox |
|---|---|---|
| AI control | none / plugins | 106 MCP tools, every feature; `batch` makes 50 edits one atomic undo step |
| Feedback loop | your ears | `analyze_mix`: LUFS, true peak, spectrum balance, stereo, masking, 0-100 score + fixes |
| Trying ideas | save-as copies | `snapshot` A/B variants, `diff_project` structured diffs, `compare_variants` renders and scores each |
| Delivery | trust the meters | `validate_project` / `check_master`: overs, true peak, LUFS target, DC, mono, missing samples, broken routing |
| Sounds | ship with sample packs | synthesizes everything (incl. modelled piano, strings, choir), installs CC0 multisampled orchestral packs, fetches sounds from Freesound or any URL |
| Theory | piano roll | roman-numeral progressions, voice leading, scale-locked melodies |
| Mistakes | Ctrl+Z if you're lucky | every tool call is undoable, failed calls roll back |
| Format | binary project | one readable JSON file |

## Features

- **Realistic acoustic sources** — a modelled acoustic piano (inharmonic stiff strings, 1-3 detuned strings per key, hammer comb + thump, two-stage decay, soundboard; `grand_piano`, `felt_piano`, `upright_keys`), modelled ensembles (strings / choir / brass sections with per-player vibrato, drift and onset jitter through body/formant spectra: `string_section`, `staccato_strings`, `cello_section`, `choir`, `choir_aah`, `brass_section`), and a **multisample** engine (key zones x velocity layers with crossfades x round robin, SFZ loading). `install_instrument_pack` downloads real CC0 instruments from VSCO 2 Community Edition (upright piano, violin/viola/cello sections sustain/spiccato/pizzicato/tremolo, harp, woodwinds, brass) and maps the zones from the file names, recording the licence.
- **More synth engines** — band-limited **wavetable** synth with 6 morphing tables (position envelope / LFO, automatable), **granular** sampler (grain size, density, position, spray, scan, reverse), **layer** instrument (stack up to 4 sources with key/velocity splits and per-layer delay), stereo unison spread on synths, and **808 / synth glides** (`slide_to` on notes, `glide_ms`).
- **Instruments** — 10 synthesized drum voices (808-style kick, snare, clap, metallic hats, rim, tom, cowbell, shaker, crash), a subtractive synth (polyBLEP oscillators, 9-voice unison/supersaw, SVF filter with envelope + LFO, sub, drive), FM synth (e-piano, bells, mallets), Karplus-Strong plucked strings, an 808 bass, and a pitched sampler. 28 presets.
- **Studio effects** — N-band parametric EQ (bell / shelves / 12-48 dB cuts / notch), 3-band multiband compressor, dynamic EQ, de-esser, soft clipper (oversampled), gate, phaser, flanger, gross-beat-style **stutter** (gate pattern / stutter / half-time / reverse / tape stop, tempo-synced), granular pitch shifter, **convolution reverb** with synthesized room / hall / plate / chamber / spring / cathedral IRs (partitioned FFT), tempo-synced auto-pan / tremolo. Every effect can be bypassed; `describe_effect` / `describe_instrument` return parameter schemas (type, range, unit, enum options) and `set_parameters` changes many at once, atomically.
- **Effects** — filter, 3-band EQ, distortion, bitcrush, tempo-synced ping-pong delay, Freeverb reverb, chorus, compressor, **sidechain ducking**, stereo width, transient shaper, lookahead limiter. Per track and on the master.
- **Generators** — 11 drum styles (house, techno, trap, drill, boom bap, lofi, dnb, reggaeton, afrobeats, phonk, garage), basslines that follow a progression (808, offbeat, walking…), chords with smooth voice leading (block, stabs, arps…), melodies with motif repetition and chord-tone targeting, and `generate_beat` for a full arranged song in one call.
- **Samples from the internet** — `search_samples` on Freesound (CC0-only by default), `download_sample` by id or any audio URL; wav/mp3/flac/ogg decoding; licenses tracked for credits.
- **Ears** — `analyze_mix` reports peak/RMS, crest factor, energy across sub/bass/low-mid/mid/presence/air, stereo correlation, per-track energy share, low-end masking, and concrete suggestions.
- **Song structure** — patterns (1–64 bars), copy-to-vary, arrangement, WAV render and stem export. `build_structure` turns one full pattern into intro / verse / build / hook / bridge / outro sections by musical role, varies repeats and adds transitions; `add_transition` writes risers, reverse cymbals, impacts, downlifters, fills, filter builds, stutters, tape stops and drop gaps at a boundary (shared patterns are split so only that boundary changes).
- **Piano roll** — select notes by range / pitch / velocity and edit, delete, quantize (strength + swing), split / merge / legato, arpeggiate, strum, rolls / ratchets / flams, chord voicings (inversions, drop-2/3, spread), diatonic harmonize, key detection, velocity shaping, grooves (MPC swing, lazy/Dilla, push) and per-note **probability** and **microtiming**; variations, fills and counter-melodies that stay in key. Standard MIDI File **import / export**.
- **One idea = one call** — `write_riff` (chord-relative figure over a progression, any rhythm, many sections at once), `add_roll`, `write_phrase` (motif repeated with variation / sequence), `copy_notes` (one track between patterns), `add_slides`, `set_section_mix`, `batch`.
- **Samples** — transient `slice_sample` into a playable slice kit, WSOLA `stretch_sample` (tempo-match a loop) and pitch shift, `edit_sample` (trim silence, region in seconds or beats, reverse, fades, normalize peak / LUFS, high-pass), `analyze_audio` (BPM, key, onsets, LUFS, spectrum, width of any sample, file or the mix).
- **Automation** — lanes on any track, bus or the master for `volume`, `pan`, any numeric instrument parameter (`instrument.cutoff`, `instrument.amp_env.release`; sampled per note) or any numeric effect parameter (`fx.0.cutoff`, `fx.2.mix`; evaluated every 64 samples). Breakpoints in song beats with linear / step / smooth curves. `generate_automation` writes musical shapes over a section: riser, sweep down, fade in/out, tempo-synced pump and LFO (`1/8`, `1/4t`, `2 bars`), with log-scaled frequency sweeps.
- **Buses, sends and returns** — named buses with their own effect chains (reverb/room/delay returns, drum bus, parallel comp, glue presets), per-track pre/post-fader sends in dB and output routing to a bus; buses sum into the master before the master chain.
- **A/B variants + diff** — `snapshot` named versions (persisted next to the project by `save_project` and restored by `load_project`), `restore_snapshot` (undoable), `diff_project` returns changes in producer terms (`tracks.bass.volume_db -2 -> -5`, `patterns.A.clips.kick +4 notes`), `compare_variants` renders each version and ranks them by mix score, LUFS and true peak.
- **Robust analysis** — the analyzer measures each track during the render instead of keeping every stem in RAM (a 31-track, 3-minute song no longer exhausts memory), per-track `active_rms_db` / `active_percent` read sparse parts honestly, a panicking tool returns an error instead of killing the MCP server, and every mutation returns its `revision` and an exact `changes` list.
- **Validation + mastering QC** — integrated loudness (ITU-R BS.1770 K-weighting with gating), short-term max, loudness range, 4x-oversampled true peak, DC offset, mono fold-down, silence detection; structural checks for missing samples, broken sends/routing, dead automation, NaN or out-of-range values, notes outside patterns, empty or muted tracks. Every item says pass / warn / fail and which tool fixes it.
- **Studio** — native egui desktop app: arrangement strip, step sequencer, piano roll, **mixer** (channel strips with live meters from stems, faders, pan, sends, bus and master strips with LUFS / true peak), **automation editor** (lane list, curve view over the song with sections, click to add points, one-click shapes), inspector with knobs, live waveform / spectrum, mix score and the AI activity feed. Every click is a tool call, so the AI and you share one undo history.

## Quick start

```bash
cargo install --path .

# a full trap beat, rendered and scored
beatbox beat trap --key A -o trap.wav --save trap.json

# call any tool from the shell
beatbox call add_effect '{"track":"lead","type":"reverb","params":{"size":0.9}}' --project trap.json
beatbox analyze trap.json
beatbox tools            # everything an AI can do
```

## Use it from an AI (MCP)

Add to Claude Desktop / Cursor / any MCP client:

```json
{
  "mcpServers": {
    "beatbox": { "command": "beatbox", "args": ["mcp"] }
  }
}
```

Then ask: *"Make a dark phonk beat at 130 BPM, sidechain the 808 to the kick, check the mix and fix whatever it complains about."*

Set `FREESOUND_API_KEY` (free at freesound.org/apiv2/apply) to let the AI pull real sounds.

## Tool surface (106 tools)

Discovery `get_guide` `list_presets` `list_tools` · Project `new_project` `get_project` `save_project` `load_project` `set_tempo` `set_key` `undo` `redo` `batch` · Tracks `add_track` `remove_track` `set_instrument` `tweak_instrument` `set_mixer` · Sound design `design_sound` `layer_instrument` `create_macro` `set_macro` `describe_instrument` `set_parameters` · Multisample `add_multisample_track` `install_instrument_pack` · FX `add_effect` `tweak_effect` `remove_effect` `get_effects` `describe_effect` `bypass_effect` `reorder_effects` · Patterns `add_pattern` `remove_pattern` `set_pattern_length` `set_steps` `toggle_step` `add_notes` `clear` `get_pattern` `transpose` `humanize` · Piano roll `edit_notes` `delete_notes` `quantize` `split_notes` `merge_notes` `legato` `arpeggiate` `strum` `roll_notes` `chord_voicing` `harmonize` `detect_key` `velocity_curve` `apply_groove` · Writing `write_riff` `add_roll` `write_phrase` `copy_notes` `add_slides` · Generators `generate_drums` `generate_bassline` `generate_chords` `generate_melody` `generate_beat` `generate_variation` `generate_fill` `counter_melody` · Theory `theory_scale` `theory_chords` · Song `set_arrangement` `build_structure` `vary_section` `add_transition` `set_section_mix` · MIDI `import_midi` `export_midi` · Samples `search_samples` `download_sample` `import_sample` `list_samples` `add_sample_track` `slice_sample` `stretch_sample` `edit_sample` `analyze_audio` · Output `render` `analyze_mix` · Automation `add_automation` `set_automation_points` `clear_automation` `list_automation` `generate_automation` · Routing `add_bus` `remove_bus` `set_send` `route_track` `list_routing` · Variants `snapshot` `list_snapshots` `restore_snapshot` `diff_project` `compare_variants` · QC `validate_project` `check_master`

## Architecture

```
            ┌──────────── CLI ────────────┐
 MCP client ┤                             ├─► Engine::call(tool, json) ─► tools registry
            └── Studio (native GUI) ──────┘        │  undo/redo, rollback on error
                                                   ▼
          theory ─► project (JSON) ─► render: instruments → automation → fx → fader
                                        → sends / bus routing → buses → master → WAV
                                                   ▼
                               analysis ("ears": LUFS, true peak) + validate (QC)
```

## Roadmap

- [x] Engine, MCP tools, CLI
- [x] Native desktop studio (egui): step sequencer, piano roll, knobs, mixer, live waveform/spectrum, live AI activity feed
- [x] Studio ↔ MCP live link (`beatbox mcp --connect`) so you watch the AI produce
- [x] Automation lanes, buses/sends/returns, A/B snapshots + diff, validation and master QC
- [x] Piano roll + MIDI import/export, song structure + transitions, studio FX rack, wavetable / granular / modelled piano + ensembles / multisample instruments, sample slicing and stretching, persistent snapshots, batch
- [ ] Per-section analysis (`analyze_sections`), artifact / click detection, spectrogram render, reference-track compare, mastering assistant, export formats with dither, scoped edits, `produce_track` autonomy loop

## License

MIT © Naman Sharma
