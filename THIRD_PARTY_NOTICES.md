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
