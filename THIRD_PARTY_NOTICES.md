# Third-party notices

Beatbox is MIT licensed. It includes code adapted from the projects below.

## SoundCraft

- Source: https://github.com/storytold/soundcraft (commit `eac0edd`)
- Copyright (c) 2026 ArtCraft Team and the SoundCraft contributors
- License: MIT OR Apache-2.0, used here under the MIT License (reproduced below)
- Changes: ported from Rust edition 2024 to 2021, rewritten around beatbox's
  whole-buffer stereo `f32` processing, unused generality removed, tests added.
  The ArtCraft name and logos are trademarks and are not used.

| Beatbox file | Adapted from (SoundCraft) | What |
|---|---|---|
| `src/resample.rs` | `crates/dsp/src/offline.rs` (`SincTable`, `resample_channel`, `resample_ratio`, `bessel_i0`) | Kaiser windowed-sinc resampler |
| `src/export.rs` | `crates/audio-io/src/flac.rs`, `crates/audio-io/src/pcm.rs` (`Quantizer`), `crates/dsp/src/plugins/utility.rs` (`Dither`) | FLAC encoder, TPDF dither, noise shaping |
| `src/sc_dsp.rs` | `crates/dsp/src/plugins/reverb.rs`, `crates/dsp/src/plugins/dynamics.rs`, `crates/dsp/src/pitch_detect.rs`, `crates/dsp/src/offline.rs` (`detect_transients`, `non_silent_ranges`) | FDN reverb, soft-knee lookahead compressor, YIN pitch detection, spectral-flux transients, strip silence |
| `src/timebase/mod.rs` | `crates/time/src/lib.rs` (`SampleRate`, `Range`, `to_samples`, `TimeError`) | PPQ timebase root; `thiserror` derive replaced by a manual `Display`; Beatbox step↔tick bridge and `TempoMap::for_project` added |
| `src/timebase/tempo.rs` | `crates/time/src/tempo.rs` | Tempo + meter map (960 PPQ), ticks↔samples, Bars\|Beats |
| `src/timebase/grid.rs` | `crates/time/src/grid.rs` | Grid values (note/dotted/triplet, seconds, frames, samples), grid lines, snapping |
| `src/timebase/timecode.rs` | `crates/time/src/timecode.rs` | SMPTE frame rates, drop-frame timecode, Feet+Frames |
| `src/timebase/format.rs` | `crates/time/src/format.rs` | Counter formats: format/parse positions and lengths in five timebases |
| `src/gui/theme.rs` | `crates/ui-egui/src/theme.rs` | Design tokens (dark studio palette), `clip_colors`, font helpers, `apply` (egui 0.36 → 0.29) |
| `src/gui/console.rs` | `crates/ui-egui/src/widgets.rs` (`text_toggle`, `selector_box`, `pan_knob`, `meter`, `fader`) | Console widgets: S/M toggles, selector box, pan knob, zoned peak meter with hold + clip, console fader; counter box and name plate in the same style |
| `src/console_law.rs` | `crates/model/src/mixer.rs` (`fader_pos_to_db`, `fader_db_to_pos`), `crates/ui-egui/src/widgets.rs` (`meter_pos`, `pan_text`, `db_text`) | Fader taper, hardware meter scale, pan/dB readouts (headless-tested) |
| `src/gui/shortcuts.rs` | `crates/ui-egui/src/shortcuts.rs` (`parse`, `mods_match`, key-event loop) | Shortcut string parser and a Beatbox shortcut table |
| `src/gui/views.rs` (mixer strips, master strip) | `crates/ui-egui/src/mix_window.rs` (`strip`, `insert_slot`, `section_label`) | Console strip layout: INSERTS / SENDS / OUTPUT sections, pan over counter, S/M, fader + stereo meters, dB counter, name plate |
| `src/gui/piano.rs` | `crates/ui-egui/src/midi_editor.rs` | Piano roll: keyboard, grid, velocity-shaded notes, drag move/resize with snap, rubber-band + shift selection, arrows/Delete, toolbar ops, velocity lane |
| `src/note_edit.rs` | `crates/ui-egui/src/midi_editor.rs` (drag math) | Snap and move/resize of a note selection, written back through `add_notes` |
| `src/fx_sc.rs` | `crates/dsp/src/plugins/modulation.rs` (`ModDelay`), `crates/dsp/src/plugins/dynamics.rs` (`Maximizer`), `crates/dsp/src/plugins/harmonic.rs` (`LoFi`, `Rectifier`), `crates/dsp/src/util.rs` (`DelayLine`, `DcBlocker`, `Rng`), at commit `7414719` | Insert effects `mod_delay`, `maximizer` (latency compensated), `lofi`, `rectifier` |
| `src/sc_strip.rs` | `crates/dsp/src/biquad.rs` (`Coeffs::design`, `Biquad::process_sample`, `butterworth_qs`), `crates/dsp/src/plugins/eq.rs` (`pass_filter`, `design_eq7`, `design_eq1`), `crates/dsp/src/plugins/dynamics.rs` (`ExpanderGate`, `DeEsser`), `crates/dsp/src/plugins/strip.rs` (`ChannelStrip`) | EQ 7-Band, EQ 1-Band, expander/gate with hold, dynamic-EQ de-esser, channel strip (trim, gate, comp, EQ); offline whole-buffer versions with automatic thresholds |

```
MIT License

Copyright (c) 2026 ArtCraft Team and the SoundCraft contributors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## Sound palette samples (downloaded on first use, not bundled)

53 CC0 1.0 one-shots from Sonic Pi's bundled freesound CC0 set and Michael Fischer's 1994 TR-808 set (tidalcycles/sounds-tr808-fischer, CC0). Per-file author, source page, pinned URL and SHA-256: [SAMPLES_LICENSES.md](SAMPLES_LICENSES.md).

## Recorded instruments, neural voice tools (fetched on first use, not bundled)

- **rustysynth** (Rust crate, MIT, Nobuaki Tanaka): SoundFont (SF2) synthesis for `soundfont` tracks.
- **GeneralUser GS 2.0.3** (S. Christian Collins), fetched by `fetch_instrument` from the author's GitHub repo at a pinned commit, sha256-checked, into `~/.beatbox/library`. GeneralUser GS License v2.0: "You may use GeneralUser GS without restriction for your own music creation, private or commercial ... Please feel free to use it in your software projects, and to modify the SoundFont bank or its packaging." https://github.com/mrbumpy409/GeneralUser-GS
- **nnnoiseless** (Rust crate, BSD-3-Clause; a port of Xiph's RNNoise, BSD-3-Clause): fallback backend of `denoise_voice`.
- **DeepFilterNet 3** (`deep-filter` v0.5.6 release binary, MIT OR Apache-2.0, Hendrik Schröter), fetched once by `denoise_voice` and sha256-checked; it is run as a separate program and not redistributed here. https://github.com/Rikorose/DeepFilterNet
- **DNSMOS P.835** (`sig_bak_ovr.onnx`, MIT, Microsoft DNS Challenge), fetched once by `scripts/dnsmos.py` (embedded in the binary) and sha256-checked, for `score_voice_quality`. https://github.com/microsoft/DNS-Challenge
- **Versilian Community Sample Library (VCSL)** (Sam Gossner and contributors), CC0 1.0. Catalog programs `vcsl_*` fetch individual files from github.com/sgossner/VCSL at commit c1ea7bc, each checked against its git blob id; the key / velocity / round-robin mapping is generated from the file names. https://github.com/sgossner/VCSL
- **Euterpea Studio Harmonium** (Evan Yerburgh; recorded at the Euterpea Studio, Yale Department of Computer Science), CC0 1.0. Catalog program `euterpea_harmonium`, files from commit a7cb611, blob-id checked. https://github.com/evanyerburgh/euterpea-harmonium
- **Tabla bols** pack by mmiron on Freesound (pack 8162), CC0 1.0. Catalog program `mmiron_tabla_bols` downloads Freesound's high-quality previews, each pinned by sha256. https://freesound.org/people/mmiron/packs/8162/
- **India Santoor** pack by batchku on Freesound (pack 581), CC0 1.0. Catalog program `batchku_santoor_rolls` (12 tremolo-roll notes) downloads Freesound's high-quality previews, each pinned by sha256. https://freesound.org/people/batchku/packs/581/
- **Dholak** bass and treble skin strokes from the Percussion pack by curesforbrokenhearts on Freesound (pack 24506), CC0 1.0. Catalog program `cfbh_dholak`, previews pinned by sha256. https://freesound.org/people/curesforbrokenhearts/packs/24506/
- **Tanpura in E** by iluppai on Freesound (sound 148850), CC0 1.0. Catalog program `iluppai_tanpura_e` (a 22 s Sa-Pa drone) downloads Freesound's high-quality preview, pinned by sha256. https://freesound.org/people/iluppai/sounds/148850/
- **Greg Sullivan E-Pianos** (Yamaha CP80, Hohner Pianet T, Wurlitzer EP200; SFZ conversion by kinwie), CC BY 3.0, credit Greg Sullivan. Catalog programs `gregsullivan_*` fetch files from github.com/sfzinstruments/GregSullivan.E-Pianos at commit 8c3e581, blob-id checked. https://github.com/sfzinstruments/GregSullivan.E-Pianos
- **zip** (MIT), **tar** (MIT OR Apache-2.0), **flate2** (MIT OR Apache-2.0) Rust crates: archive unpacking for `import_from_url`.
- The sound catalog (`src/sound_catalog.json`) is metadata only; each entry carries its own licence and source, and only CC0 / CC BY / MIT-style items are listed.
