# IWBY v4 / v4b offline post-process (B43, 2026-10-13)

Vocal FX (hall reverb with ducking, phrase-end echo throws, doubler, slap), the arrangement (stem gain envelopes,
fill, swells, crashes) and the master for "I Wanna Be Yours" v4 and v4b. Applied to the engine's pre-master stems.
No audio and no vocal take are in the repo.

Reproduce:
    beatbox --workdir WD call export_stems '{"dir":"STEMS","format":"wav","mode":"pre_master"}' --project proj.json   # the IWBY v3 project, 11d587c binary
    python3 iwby_v4_post.py STEMS v4.wav  params.json        # v4  (sha256 bb091780...d80a4)
    python3 iwby_v4_post.py STEMS v4b.wav params_v4b.json    # v4b (sha256 8da5b07c...a8887e)
Needs numpy, scipy, soundfile, pyloudnorm. Random draws come from default_rng(seed) in a fixed order, and the WAV is written
without a PEAK/timestamp chunk, so a re-run is byte-identical on the same numpy/scipy build (checked twice for each).
IWBY_STATS=1 prints the wet/dry levels and leaves the audio untouched.

## iwby_post.py: the song map read from the project (B51, 2026-10-14)

`iwby_v4_post.py` pins the v4b song map to the 30-bar legacy plan (section bars, seven throw times in absolute
seconds, end fade and length). Under the yin_gated default, cover_song plans chorus 2 at 5 bars and puts the take
0.915 s earlier, so that map no longer fits. `iwby_post.py` runs the same processing with nothing pinned:

    python3 iwby_post.py STEMS OUT.wav proj.json [params_iwby.json]

- bpm and section spans come from the project's arrangement (pattern bars x repeats): intro, verse (every verse*
  entry), chorus1, chorus2, outro; the breakdown is the last `breakdown_bars` (2) before chorus2;
- throws are phrase indices on the dry vocal stem (`throw_phrases`, 0-based, the 7 phrases v4b kept out of 20);
  `expect_phrases` refuses a stem that splits into a different number of phrases;
- end fade and length are offsets from the song end (`end_fade_from_end_s`, `length_from_end_s`), which are
  v4b's 105.0 / 108.6 / 110.0 s measured against its 30-bar end.

The map logic is in `iwby_map.py` (pure, no audio). `python3 test_iwby_map.py` checks it (6 checks).
Checked byte-identical against the existing renders, on stems exported by 9c148854:
the legacy-pitch IWBY v4b project (proj4) gives sha256 4cb0889a...e4280, the same as iwby_v4_post.py + params_v4b.json;
the v5 project (yin_gated plan) gives 4c42f368...90441, the same as the hand-adapted v5 render.
`iwby_v4_post.py` and its params stay as the record of the v4 and v4b renders on 11d587c.

## iwby_v5_build.py: IWBY v5 end to end, stem gains pinned (B53, 2026-10-14)

    python3 iwby_v5_build.py BEATBOX_BIN take_dn2.wav WORKDIR v5.wav                      # canonical v5, sha256 4c42f368...90441
    python3 iwby_v5_build.py BEATBOX_BIN take_dn2.wav WORKDIR v5_7k.wav --sibilance-band  # opt-in 7 kHz band, threshold -27 dB

The whole v5 recipe in one place: cover_song with `params_v5_build.json` (the v1 args, key E minor), the v3/cc4 project edits
and v4b's later values (applied by role, not by effect index), export_stems, then `iwby_post.py`. The take is not in the repo.

Stem gains are pinned (`params_v5_stems.json`). export_stems' default trims each stem until its own decoded true peak meets
0 dBTP, so a stem's level follows its own peak: v5's vocal (+12 dB fader) was turned down 7.05 dB in 4 trim steps, and a
de-essed vocal (lower peaks) came out 2.35 dB louder and split into 12 phrases instead of 20. The pins are the exact steps
the trim took for v5 (`stem_gain_db_pin` in export_stems' report, as text), so v5 rebuilds bit for bit and a variant keeps v5's
stem levels whatever its peaks do. Only vox was trimmed; the other stems' pins are empty (no gain).
`--sibilance-band [DB]` asks cover_song for its opt-in `sibilance_band` (7 kHz dynamic bell, Q2, ratio 4, at most -4 dB,
attack 1 ms, release 60 ms, default threshold -27 dB) and moves it LAST on the vocal chain, after this recipe's shelves and
exciter. `IWBY_KEEP_STEMS=1` keeps WORKDIR/stems for measuring.
