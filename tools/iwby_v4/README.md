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
