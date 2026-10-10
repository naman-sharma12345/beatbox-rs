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
