# Beatbox-rs Roadmap (merged: external blueprint + AI producer + AI builder)

Principle: **MCP first.** Every GUI action, CLI command and engine capability has an MCP tool.
Each sprint ends by re-rendering a reference beat with the new tools and comparing it with the previous version.

## Sprint 5 — AI ears, delivery, MCP parity, SoundCraft DSP ports
- analyze_sections, windowed analyze_mix, detect_artifacts (clicks, end-level jumps, noise floor/hiss beds), render_spectrogram (PNG returned as an MCP image)
- render_preview (region render returned as an MCP audio resource), compare_to_reference / analyze_reference, master_assistant, export_audio (wav/flac/mp3, codec-aware true peak, dither), export_stems
- Close the parity audit: transport/play/seek, get_history, screenshot tool, waveform peaks, spectrum, eq curve, render with a region/tail
- Port from storytold/soundcraft (MIT/Apache, with attribution): windowed-sinc resampler, FDN reverb, soft-knee lookahead compressor, YIN pitch detection (analyze_track: fundamental and key fit), TPDF dither + FLAC, transient detection / strip silence
- Stable effect IDs (cheapest high-value ID fix)
## Sprint 6 — Identity and clips
- Persistent IDs for tracks, clips, notes, buses and lanes; schema version and migration
- Independent MIDI/audio/automation clips on a timeline replacing sections; "hook2:1.1" addressing
- expected_revision, dry_run, scoped_edit with diff verification
## Sprint 7 — Autonomy
- produce_track, critique_mix, candidate branches with loudness-matched A/B, golden-render tests, haas, saturation types, place_fx, Wikimedia sample search
- DONE: saturator (tape/tube/transistor/diode/fold/exciter), haas widener, place_fx, move_effect, ab_compare (loudness-matched). TODO: golden-render tests, Wikimedia sample search
## Sprint 6-7 — Status (shipped)
- Autonomy: produce_track / plan_track / apply_plan / critique_track / revise_track; revisions kept only when diff_renders agrees
- Ears: ears_report, diff_renders, loudness_report, masking_matrix, groove/hook/structure analysis, stereo_image, punch, vocal_pocket, reference_match
- Nine genre playbooks (incl. desi_hiphop: sitar, tabla, tanpura); flip_sample, vocal_chop, sample index/kits
- FL parity: playlist pattern clips, automation clips, route_bus with cycle check, scale_snap, ghost notes; saturator, haas, place_fx, move_effect, ab_compare
- Registry: 163 MCP tools
- Still open: persistent IDs + clip timeline (Sprint 6 identity work), golden-render tests, Wikimedia sample search
## Studio UI port (SoundCraft) — Status (shipped)
- Studio look: SoundCraft design tokens (`gui/theme.rs`) and console widgets (`gui/console.rs`): flat charcoal surfaces, square S/M toggles, selector boxes, pan knob over a counter, zoned peak meters with hold + clip LED, console fader with dB scale, Bars|Beats + Min:Secs LCD counters
- Shortcut table (`gui/shortcuts.rs`): Space, Cmd+Z / Cmd+Shift+Z, Cmd+S, Cmd+R, Cmd+1..4 / F5-F9 views, [ ] patterns, Alt+Up/Down tracks, M / S, Home
- Mixer: console strips (INSERTS, SENDS, OUTPUT routing menu, pan, S/M, fader + stereo meters, dB counter, name plate) and a master strip with loudness rows
- Piano roll: velocity-shaded notes, snap (1/4..1/32, off) from the timebase, drag move/resize, rubber-band + shift selection, arrows (Shift = octave/bar), Delete, Quantize / Vel± / Legato, velocity lane; every gesture is one engine call (one undo step)
- Timebase groundwork (`src/timebase/`): 960 PPQ ticks, tempo + meter map, grids, timecode, five counter formats, step↔tick bridge; renderer not rewired yet
- Playlist view: arrangement as clips per track lane on the tick timebase with Bars|Beats + tempo rulers
- Next: native clips on the tick timeline (move/resize clips), tempo changes in the renderer, CLAP host
## Sprint 8 — Real-time engine
- Block-based cpal audio graph shared by playback and offline render, live meters, PDC
## Sprint 9 — Plugins
- CLAP/VST3 hosting with crash isolation and parameter discovery as MCP tools
