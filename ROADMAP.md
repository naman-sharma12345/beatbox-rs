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
## Sprint 8 — Real-time engine
- Block-based cpal audio graph shared by playback and offline render, live meters, PDC
## Sprint 9 — Plugins
- CLAP/VST3 hosting with crash isolation and parameter discovery as MCP tools
