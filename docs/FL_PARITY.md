# FL Studio parity

What FL Studio gives a producer, tool by tool, and where beatbox stands. Every
beatbox entry is an MCP tool (and most have a studio view), so an AI can do
anything listed as **have**.

**Score: 77 have, 6 partial, 8 missing of 91** (parity 77/91; counting partial as half: 80/91).

Build order for the gaps, by musical impact: noise reduction as its own tool, bounce to audio,
tempo automation and markers, additive synth, then live input and plugin
hosting.

| Area | FL Studio | beatbox | How |
|---|---|---|---|
| Windows | Channel rack | have | add_track, set_instrument, set_mixer; studio SEQUENCER view |
| Windows | Step sequencer | have | set_steps, toggle_step, generate_drums |
| Windows | Piano roll | have | add_notes, edit_notes, delete_notes; studio piano roll |
| Windows | Playlist (pattern clips) | have | place_pattern, list_playlist, remove_clip, arrangement_to_playlist; PLAYLIST view |
| Windows | Playlist audio clips | have | add_audio_clip, list_audio_clips, remove_audio_clip |
| Windows | Automation clips | have | create_automation_clip, place_automation_clip, add_automation; AUTOMATION view |
| Windows | Mixer (inserts, sends, routing) | have | set_mixer, add_bus, set_send, route_track, route_bus; MIXER view |
| Windows | Browser | have | search_samples, list_samples, find_samples, list_palettes, list_presets; studio BROWSER view (F8): searchable instruments (LOAD / +TRACK), effects (ADD), palettes (APPLY), project samples (+TRACK), each button the same MCP call |
| Windows | Undo history | have | undo, redo, get_history (100 steps) |
| Windows | Edison (audio editor) | partial | edit_sample, strip_silence, slice_sample, analyze_audio (no recording, no spectral edit) |
| Piano roll tools | Quantize | have | quantize |
| Piano roll tools | Randomize / humanize | have | humanize, generate_variation |
| Piano roll tools | Arpeggiate | have | arpeggiate |
| Piano roll tools | Strum | have | strum |
| Piano roll tools | Chop | have | chop_notes (pattern x/X/-/. on the grid), split_notes |
| Piano roll tools | Flam | have | roll_notes mode flam (grace hit before), also roll / ratchet |
| Piano roll tools | Legato | have | legato |
| Piano roll tools | Articulate (note length) | partial | edit_notes length |
| Piano roll tools | Glue | have | merge_notes |
| Piano roll tools | Limit (clamp to range / scale) | have | limit_notes (octave-fold into lo..hi), scale_snap |
| Piano roll tools | Flip (invert / reverse notes) | have | flip_notes mode pitch (mirror) / time (reverse) |
| Piano roll tools | Chord stamps | have | chord_voicing, generate_chords, theory_chords |
| Piano roll tools | Scale highlighting / snap | have | scale_snap, theory_scale |
| Piano roll tools | Riff machine | have | write_riff, generate_melody, counter_melody |
| Piano roll tools | LFO tool (CC/velocity shapes) | partial | velocity_curve, generate_automation |
| Piano roll tools | Slide / portamento notes | have | add_slides |
| Piano roll tools | Ghost notes | have | generate_ghost_notes |
| Piano roll tools | Velocity editing | have | velocity_curve, edit_notes |
| Instruments | FL Keys / FL Grand (piano) | have | piano instrument |
| Instruments | 3x Osc / GMS (subtractive) | have | synth instrument, design_sound |
| Instruments | Sytrus / Toxic Biohazard (FM) | have | fm instrument |
| Instruments | Sakura (physical strings) | have | pluck (Karplus-Strong strings: guitar, sitar, santoor, koto) |
| Instruments | FPC / drum pads | have | drum instrument + sampler, install_kit |
| Instruments | Fruity DrumSynth / Kick | have | drum synthesis (kick, snare, hats, 808) |
| Instruments | Slicex | have | slice_sample, flip_sample, vocal_chop |
| Instruments | DirectWave / Sampler | have | sampler, multisample, install_instrument_pack |
| Instruments | Fruity Granulizer | have | granular instrument |
| Instruments | Wavetable (Harmless-style) | have | wavetable instrument |
| Instruments | Harmor / Morphine (additive, resynthesis) | missing |  |
| Instruments | Transistor Bass (303) | have | acid_bass preset |
| Instruments | BooBass / 808 | have | bass808 instrument |
| Instruments | FLEX (preset player) | partial | design_sound presets, list_presets |
| Instruments | Layer channel | have | layer_instrument |
| Effects | Parametric EQ 2 | have | parametric_eq, eq, eq_curve |
| Effects | Fruity Limiter / Compressor | have | limiter, compressor |
| Effects | Maximus (multiband) | have | multiband |
| Effects | Reeverb 2 | have | reverb |
| Effects | Convolver | have | convolution |
| Effects | Delay 3 | have | delay |
| Effects | Chorus / Flanger / Phaser | have | chorus, flanger, phaser |
| Effects | Gross Beat (time + volume gating) | have | gross_beat effect: 13 time presets (half speed, repeats, reverse, tape stop, scratch, freeze) + 11 volume presets (trance gate, pump, tresillo) or drawn time_points / volume_points |
| Effects | Blood Overdrive / Fast Dist / Distructor | have | distortion, saturator, soft_clipper |
| Effects | Bitcrush / Squeeze | have | bitcrush |
| Effects | Stereo Enhancer | have | width, haas |
| Effects | Stereo Shaper | have | add_effect type stereo_shaper: L/R matrix (left_from_left ... right_from_right), delay_ms on one side, phase invert per side, presets mono/swap/wide/narrow/side_only/pseudo_stereo; plus width, haas, autopan |
| Effects | Transient Processor | have | transient |
| Effects | Soundgoodizer | partial | master_assistant, multiband |
| Effects | Love Philter / Fruity Filter | have | filter + automation |
| Effects | Vocoder / Vocodex | have | add_effect type vocoder: the track's own vocal modulates a built-in saw chord (notes), 4-40 bands, carrier noise + sibilance pass-through |
| Effects | Pitcher / NewTone (pitch correction) | have | tune_vocal (PSOLA autotune), pitch_shift |
| Effects | Newtime (time warping) | partial | stretch_sample, vocal warp in vocal_to_song |
| Effects | Gate | have | gate |
| Effects | De-esser | have | deesser |
| Effects | Peak controller / sidechain | have | sidechain, carve_mix |
| Effects | Frequency shifter | have | add_effect type freq_shift: shift_hz (every partial by Hz, analytic-signal shifter), stereo_hz drift, feedback, mix |
| Effects | Ring modulator | have | add_effect type ring_mod: carrier freq_hz, shape sine/triangle/square/saw, stereo_phase, lfo_hz/lfo_depth wobble, mix |
| Effects | Waveshaper | have | add_effect type waveshaper: drawn curve 'x:y, x:y' (mirrored when symmetric) or presets soft/hard/tube/fold/sine/steps/crush_curve, drive_db, output_db; also saturator, soft_clipper |
| Effects | Wave Candy / Spectroman | have | spectrum, render_spectrogram, waveform_peaks, loudness_report |
| Effects | Panning / Balance | have | set_mixer pan, autopan |
| Effects | Tape stop / vinyl | have | add_effect type vinyl: crackle, surface hiss, wow + flutter (modulated delay), age (worn band), mono; stutter tape_stop, gross_beat tape_stop / tape_stop_end |
| Audio | Time stretching | have | stretch_sample |
| Audio | Audio recording | missing | (produce_song takes a recorded file) |
| Audio | Noise reduction | have | denoise_sample: spectral gate (noise floor read from the quietest frames) into a new sample, strength + floor_db; also inside produce_song |
| Audio | Stem separation | missing |  |
| Audio | Vocal chops | have | vocal_chop |
| Audio | Reverse samples | have | edit_sample reverse |
| Mixing | Mixer track groups / buses | have | add_bus, route_bus |
| Mixing | Patcher (FX chains as one) | have | list_fx_chains, apply_fx_chain (built-in or saved chain, or an effects list, append/replace), save_fx_chain, copy_fx_chain; built-ins vocal_clean, vocal_radio, lofi_keys, dusty_drums, 808_grit, wide_pad, tape_master, old_record; BROWSER view FX CHAINS (APPLY / SAVE) |
| Mixing | Sidechain routing | have | sidechain source track |
| Arrange | Pattern variants | have | add_pattern copy_from, vary_section, generate_variation |
| Arrange | Transitions / risers / fills | have | add_transition, generate_fill, add_roll |
| Arrange | Markers / time signature | missing |  |
| Arrange | Tempo automation | missing |  |
| Export | WAV / MP3 / FLAC | have | export_audio |
| Export | Stems | have | export_stems |
| Export | MIDI import / export | have | import_midi, export_midi |
| Export | Render to audio clip (bounce) | have | bounce_track: instrument + FX + fader + pan + automation printed to a WAV sample, placed as an audio clip on a new track with the same routing/sends; original muted |
| Live | MIDI controller input | missing |  |
| Live | Performance mode | missing |  |
| Live | Metronome | have | export_click: click WAV in tempo with count-in and accented downbeats, returns where beat 0 falls for lining up a take |
| Plugins | VST/CLAP hosting | missing | (roadmap sprint 12) |
