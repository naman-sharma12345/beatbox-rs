# Genre coverage and gaps (recorded instruments)

What the catalog (`search_instruments`) covers for each style today, and what is still missing. Every style also has beatbox's synthesized voices (808s, synths, pads, modelled piano and ensembles) and the General MIDI SoundFont; this page is about **real recorded multisamples**. Anything missing can still be pulled from the web with `import_from_url` / `freesound_search`.

`search_instruments {genre: "bollywood"}` lists what is tagged for a style; `audition_instrument {catalog_id}` plays a 4 s preview.

| Style | Covered by recorded instruments | Gaps |
|---|---|---|
| Hip-hop / rap / trap / drill | acoustic kits (Big Rusty, Unruly and other sfzinstruments kits, VCSL kit), electric and upright basses, pianos, Rhodes-like FM keys, sax, recorded hand claps (8 round robins) | 808s and trap kits are synthesized or from the CC0 one-shot palettes, not multisampled |
| Lo-fi | upright and grand pianos, Wurlitzer EP200, Pianet T, FM keys, nylon/electric guitars, kalimba, mallets, tabla, harmonium | a real Rhodes (the free jRhodes set is non-commercial, so it is not listed) |
| Pop / indie / rock | electric guitars (Black and Green), basses (Black and Blue and others), acoustic kits, grand and upright pianos, organs, a ukulele (22 chromatic plucked notes) | 12-string acoustic, mandolin |
| Jazz / soul / funk / R&B | upright bass (arco and pizz), saxes (21 programs), pianos, organs, vibraphone, brush snares (Frankensnare) | trumpet and trombone exist only as sustained orchestral notes (VSCO): no jazz stabs, falls or mutes |
| Orchestral / cinematic | VSCO 2 CE strings (sustain, spiccato, pizzicato, tremolo), woodwinds, horn, trumpet, trombone, tuba, harp; a solo violin (arco with vibrato, and pizzicato); timpani (5 tuned drums, hits and rolls); orchestral cymbals, gong, concert bass drum and snare; VCSL concert harp; cello programs | a recorded choir, true legato transitions on the solo violin |
| Bollywood / classical Indian / sufi / bhangra | tabla (26 bols), harmonium, santoor rolls, dholak (2 strokes), a tanpura drone (Sa-Pa in E, retunes to the song's Sa), a recorded bansuri (2 sustained notes, retuned), darbuka and frame drum, bowed psaltery as a sarangi stand-in, erhu, a solo violin (film-score and Carnatic violin lines), timpani | sitar, a fully multisampled bansuri (only 2 notes so far), shehnai, sarangi, veena, mridangam, a full dhol (dagga + tilli strokes); only phrases exist in CC0 sources so far, not playable multisamples |
| Afro / afrobeats / latin / reggaeton | congas, bongos, cajon, balafon, mbira, kalimba, steel drum, world-percussion kit; a second hand-percussion kit on GM keys that adds short and long guiro, vibraslap, sleigh bells, bell tree and log drums to fresh takes of tambourine, cowbell, claves and triangle | timbales, shekere, talking drum |
| Country / folk / bluegrass | acoustic guitars (Shiny Guitar), a banjo-like ganjo, harmonicas, upright bass, a solo violin for fiddle lines (classical tone, no fiddle ornaments) | mandolin, pedal steel, a real fiddle recording |
| EDM / house / synthwave | recorded kits for layering; recorded cymbal swells, bowed cymbal, gong scrapes and wind-chime glisses for risers and transitions; synths are beatbox's own | recorded analog synth multisamples |
| Arabic / middle eastern | darbuka, frame drum, sax | oud, qanun, ney |
| East Asian | dan tranh (koto-like), erhu, an orchestral gong | koto, shamisen, guzheng, taiko |

## Rules for adding instruments

- Licence CC0, CC BY or MIT-style only, stated in the entry and credited in THIRD_PARTY_NOTICES.md.
- Every file pinned (git blob id at a commit, or sha256 of the exact download).
- Audio is fetched on demand into `~/.beatbox/library` and never committed.
