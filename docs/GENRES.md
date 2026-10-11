# Genre coverage and gaps (recorded instruments)

What the catalog (`search_instruments`) covers for each style today, and what is still missing. Every style also has beatbox's synthesized voices (808s, synths, pads, modelled piano and ensembles) and the General MIDI SoundFont; this page is about **real recorded multisamples**. Anything missing can still be pulled from the web with `import_from_url` / `freesound_search`.

`search_instruments {genre: "bollywood"}` lists what is tagged for a style; `audition_instrument {catalog_id}` plays a 4 s preview.

| Style | Covered by recorded instruments | Gaps |
|---|---|---|
| Hip-hop / rap / trap / drill | acoustic kits (Big Rusty, Unruly and other sfzinstruments kits, VCSL kit), electric and upright basses, pianos, Rhodes-like FM keys, sax | 808s and trap kits are synthesized or from the CC0 one-shot palettes, not multisampled; no recorded hand claps beyond body percussion |
| Lo-fi | upright and grand pianos, FM keys, nylon/electric guitars, kalimba, mallets, tabla, harmonium | a real tape-worn Rhodes / Wurlitzer |
| Pop / indie / rock | electric guitars (Black and Green), basses (Black and Blue and others), acoustic kits, grand and upright pianos, organs | 12-string acoustic, mandolin, ukulele |
| Jazz / soul / funk / R&B | upright bass (arco and pizz), saxes (21 programs), pianos, organs, vibraphone, brush snares (Frankensnare) | trumpet and trombone exist only as sustained orchestral notes (VSCO): no jazz stabs, falls or mutes |
| Orchestral / cinematic | VSCO 2 CE strings (sustain, spiccato, pizzicato, tremolo), woodwinds, horn, trumpet, trombone, tuba, harp; VCSL concert harp; cello programs | a recorded choir, timpani, solo violin with legato |
| Bollywood / classical Indian / sufi / bhangra | tabla (26 bols), harmonium, santoor rolls, dholak (2 strokes), darbuka and frame drum, recorders/flute as a bansuri stand-in, bowed psaltery as a sarangi stand-in, erhu | sitar, bansuri, shehnai, sarangi, tanpura drone, veena, mridangam, a full dhol (dagga + tilli strokes); only phrases exist in CC0 sources so far, not playable multisamples |
| Afro / afrobeats / latin / reggaeton | congas, bongos, cajon, balafon, mbira, kalimba, steel drum, world-percussion kit | timbales, shekere, talking drum, guiro as separate programs |
| Country / folk / bluegrass | acoustic guitars (Shiny Guitar), a banjo-like ganjo, harmonicas, upright bass | fiddle, mandolin, pedal steel |
| EDM / house / synthwave | recorded kits for layering; synths are beatbox's own | recorded analog synth multisamples |
| Arabic / middle eastern | darbuka, frame drum, sax | oud, qanun, ney |
| East Asian | dan tranh (koto-like), erhu | koto, shamisen, guzheng, taiko, gongs |

## Rules for adding instruments

- Licence CC0, CC BY or MIT-style only, stated in the entry and credited in THIRD_PARTY_NOTICES.md.
- Every file pinned (git blob id at a commit, or sha256 of the exact download).
- Audio is fetched on demand into `~/.beatbox/library` and never committed.
