# Sample licenses

Curated one-shots used by the sound palettes (`list_palettes`, `apply_palette use_samples:true`, `install_palette_samples`).
**None of these files are committed to the repository.** Beatbox downloads each one on first use from the pinned upstream commit below and refuses it unless its SHA-256 matches (`src/palette_samples.json`, `src/palette.rs::fetch_sample`). `scripts/fetch_palette_samples.sh` does the same from the shell.

Every file is **CC0 1.0 Universal** (public-domain dedication). Each license was checked at the source:

- **Sonic Pi bundled samples** (`sp_*`): Sonic Pi's LICENSE.md states that all bundled samples are individually CC0 1.0 and obtained from freesound.org ([LICENSE.md](https://github.com/sonic-pi-net/sonic-pi/blob/main/LICENSE.md), [etc/samples/README.md](https://github.com/sonic-pi-net/sonic-pi/blob/main/etc/samples/README.md)); every freesound page listed below was opened on 2026-10-10 and shows *Creative Commons 0*. Files are pinned to sonic-pi commit `cf21987233e415ed365c50e7c038b8383c0cd4d7`. Sonic Pi notes that many samples were slightly trimmed for Sonic Pi.
- **TR-808 set by Michael Fischer (1994)** (`tr808_*`): real Roland TR-808 (serial 103852) recorded from its individual outputs; repository licensed CC0 1.0 ([tidalcycles/sounds-tr808-fischer](https://github.com/tidalcycles/sounds-tr808-fischer), LICENSE file = CC0 1.0 legal code). Pinned to commit `85fbecf1bec32553395625ea659e2a56dfd7c0e1`.

Skipped on purpose: Sonic Pi's `tabla_*` samples (their freesound source pages by dio_333 now return 404, so the license could not be verified at the source) and `hat_metal` (a metal-sheet hit, not a hat).

Pre-existing kits (`install_kit`: tr808 / lm2 / rz1 from smpldsnds/drum-machines) are unchanged; that repository describes itself as "a collection of public domain samples" without per-machine provenance.

| id | role | tags | author | source (license page) | sha256 | bytes |
|---|---|---|---|---|---|---|
| `sp_bd_808` | kick | trap drill 808 deep dark sub | EKVelika | [208447](https://freesound.org/people/EKVelika/sounds/208447/) | `f8cae3f73c93622a…` | 19122 |
| `sp_bd_boom` | kick | trap boom long dark sub | Snapper4298 | [157245](https://freesound.org/people/Snapper4298/sounds/157245/) | `1ece3f3fc0c5d7d2…` | 45634 |
| `sp_bd_fat` | kick | boom_bap lofi fat short warm | cubix | [124386](https://freesound.org/people/cubix/sounds/124386/) | `3c53f83cd05d8bf3…` | 4945 |
| `sp_bd_haus` | kick | house punchy clicky bright | Rodrigo The Mad | [137722](https://freesound.org/people/Rodrigo%20The%20Mad/sounds/137722/) | `b9aa2aa81a8ccabc…` | 19237 |
| `sp_bd_jazz` | kick | boom_bap jazz acoustic dusty warm | tripjazz | [511139](https://freesound.org/people/tripjazz/sounds/511139/) | `9847388d0ddedb1f…` | 46517 |
| `sp_bd_klub` | kick | drill club punchy tight | zgump | [83262](https://freesound.org/people/zgump/sounds/83262/) | `40beea807c90d0c3…` | 21246 |
| `sp_bd_tek` | kick | techno hard punchy | DWSD | [171104](https://freesound.org/people/DWSD/sounds/171104/) | `031cd7d4b3b8ccd9…` | 21858 |
| `sp_bd_zum` | kick | dnb tight clicky bright | n1ghthawk | [172124](https://freesound.org/people/n1ghthawk/sounds/172124/) | `1a364730e092da95…` | 14354 |
| `sp_bd_gas` | kick | hiphop round warm | menegass | [89460](https://freesound.org/people/menegass/sounds/89460/) | `ac568aa6b75d973f…` | 18455 |
| `sp_bd_pure` | 808 | 808 sub clean pure | EKVelika | [209571](https://freesound.org/people/EKVelika/sounds/209571/) | `cd70fc3260302262…` | 18056 |
| `sp_drum_heavy_kick` | kick | acoustic rock heavy punchy | Zajo | [4832](https://freesound.org/people/Zajo/sounds/4832/) | `9c141c8de85e695b…` | 19715 |
| `sp_drum_bass_hard` | kick | acoustic boom_bap desi_hiphop dusty punchy gritty | menegass | [100051](https://freesound.org/people/menegass/sounds/100051/) | `ed3ac2187d679ca1…` | 33950 |
| `sp_drum_bass_soft` | kick | acoustic soft warm | menegass | [100052](https://freesound.org/people/menegass/sounds/100052/) | `87bfb846ea7adf74…` | 25370 |
| `sp_sn_dub` | snare | trap dubstep crack bright | Adriak909 | [173142](https://freesound.org/people/Adriak909/sounds/173142/) | `165ddba961a14430…` | 43927 |
| `sp_sn_dolf` | snare | boom_bap jazz dusty warm | Dolfeus | [57534](https://freesound.org/people/Dolfeus/sounds/57534/) | `88d04c0cf427f1af…` | 50242 |
| `sp_sn_generic` | snare | hiphop melodic clean | hullum | [415582](https://freesound.org/people/hullum/sounds/415582/) | `c7682e3916a5eee1…` | 53335 |
| `sp_sn_zome` | snare | boom_bap lofi fat dark | Dolfeus | [55232](https://freesound.org/people/Dolfeus/sounds/55232/) | `9621c28afb20fd13…` | 56666 |
| `sp_drum_snare_hard` | snare | acoustic boom_bap crack live | menegass | [100058](https://freesound.org/people/menegass/sounds/100058/) | `1b2325523ed2a93d…` | 29657 |
| `sp_drum_snare_soft` | snare | acoustic soft ghost | menegass | [100059](https://freesound.org/people/menegass/sounds/100059/) | `3da004f932da94c4…` | 23031 |
| `sp_elec_snare` | snare | boom_bap desi_hiphop sp1200 dusty gritty | looppool | [13146](https://freesound.org/people/looppool/sounds/13146/) | `a5548c4e418864f9…` | 17894 |
| `sp_elec_hi_snare` | snare | drill tight bright metallic | looppool | [13125](https://freesound.org/people/looppool/sounds/13125/) | `73dca64f3c994ebd…` | 20908 |
| `sp_elec_lo_snare` | snare | boom_bap sp1200 low dusty | looppool | [13145](https://freesound.org/people/looppool/sounds/13145/) | `720dc00b02969e59…` | 26946 |
| `sp_hat_cab` | hat | trap desi_hiphop modular crisp | cabled_mess | [363203](https://freesound.org/people/cabled_mess/sounds/363203/) | `049d90dfaddb9459…` | 10294 |
| `sp_hat_tap` | hat | clean soft tight | TheEndOfACycle | [674296](https://freesound.org/people/TheEndOfACycle/sounds/674296/) | `7007e5b02d8f86e1…` | 10047 |
| `sp_hat_star` | hat | electronic tight crisp | IanStarGem | [269720](https://freesound.org/people/IanStarGem/sounds/269720/) | `e02b72878188621b…` | 7314 |
| `sp_hat_raw` | hat | trap drill crisp raw | cabled_mess | [339278](https://freesound.org/people/cabled_mess/sounds/339278/) | `e92e7722dfd75063…` | 20044 |
| `sp_hat_psych` | hat | lofi soft dark | PSYCHO BOOMER | [42548](https://freesound.org/people/PSYCHO%20BOOMER/sounds/42548/) | `ee3269e2c134b29c…` | 14779 |
| `sp_hat_bdu` | hat | acoustic dusty dark | bdu | [802](https://freesound.org/people/bdu/sounds/802/) | `b6335e7f423c2223…` | 6271 |
| `sp_hat_snap` | hat | melodic bright airy tight | TheEndOfACycle | [674294](https://freesound.org/people/TheEndOfACycle/sounds/674294/) | `f2c1aa0febba2b8c…` | 24493 |
| `sp_hat_gem` | hat | electronic bright airy | IanStarGem | [273186](https://freesound.org/people/IanStarGem/sounds/273186/) | `5ebd691714178833…` | 18884 |
| `sp_drum_cymbal_closed` | hat | acoustic boom_bap live | menegass | [100053](https://freesound.org/people/menegass/sounds/100053/) | `f3b9d6bb14f75ba0…` | 20943 |
| `sp_drum_cymbal_pedal` | hat | acoustic pedal dusty | menegass | [100054](https://freesound.org/people/menegass/sounds/100054/) | `af2cf5e259f3671d…` | 22633 |
| `sp_drum_cymbal_open` | open_hat | acoustic boom_bap live long | menegass | [100055](https://freesound.org/people/menegass/sounds/100055/) | `3a9cf6aacab05003…` | 119223 |
| `sp_perc_snap` | clap | snap rnb melodic airy | SoundCollectah | [109400](https://freesound.org/people/SoundCollectah/sounds/109400/) | `09d5cc75cd9ef183…` | 22089 |
| `sp_elec_wood` | perc | wood block lofi desi_hiphop | looppool | [13135](https://freesound.org/people/looppool/sounds/13135/) | `f046ea904a91f742…` | 24617 |
| `sp_drum_cowbell` | perc | cowbell phonk | Neotone | [75338](https://freesound.org/people/Neotone/sounds/75338/) | `e2ee5f732b675858…` | 18281 |
| `sp_ride_tri` | perc | ride cymbal jazz airy melodic | trivialAccapella | [425449](https://freesound.org/people/trivialAccapella/sounds/425449/) | `acff7b0ddb105ecc…` | 138176 |
| `sp_vinyl_hiss` | fx | lofi dusty vinyl texture boom_bap | veezay | [130393](https://freesound.org/people/veezay/sounds/130393/) | `4e3fdaaff3399328…` | 670171 |
| `sp_vinyl_scratch` | fx | boom_bap desi_hiphop scratch | hello_flowers | [28681](https://freesound.org/people/hello_flowers/sounds/28681/) | `2dc0fedcfeaa3088…` | 15944 |
| `tr808_bd5010` | 808 | 808 trap drill long sub boom | Michael Fischer (1994 TR-808 sample set) | [repo](https://github.com/tidalcycles/sounds-tr808-fischer) | `42c7c5eebf440437…` | 264646 |
| `tr808_bd0050` | kick | 808 deep long dark | Michael Fischer (1994 TR-808 sample set) | [repo](https://github.com/tidalcycles/sounds-tr808-fischer) | `7f25e10c782f4a99…` | 132346 |
| `tr808_bd1025` | kick | 808 trap short punchy | Michael Fischer (1994 TR-808 sample set) | [repo](https://github.com/tidalcycles/sounds-tr808-fischer) | `7d8f85101121d895…` | 44146 |
| `tr808_sd5050` | snare | 808 trap classic | Michael Fischer (1994 TR-808 sample set) | [repo](https://github.com/tidalcycles/sounds-tr808-fischer) | `6dcbf8acd5cee6d6…` | 44146 |
| `tr808_sd2575` | snare | 808 bright snappy drill | Michael Fischer (1994 TR-808 sample set) | [repo](https://github.com/tidalcycles/sounds-tr808-fischer) | `7f591eaeffa2e65e…` | 44146 |
| `tr808_sd0050` | snare | 808 dark low | Michael Fischer (1994 TR-808 sample set) | [repo](https://github.com/tidalcycles/sounds-tr808-fischer) | `667eca1430ac5f69…` | 44146 |
| `tr808_cp` | clap | 808 trap drill classic | Michael Fischer (1994 TR-808 sample set) | [repo](https://github.com/tidalcycles/sounds-tr808-fischer) | `376429bb81cb48d1…` | 176446 |
| `tr808_ch` | hat | 808 trap drill classic crisp | Michael Fischer (1994 TR-808 sample set) | [repo](https://github.com/tidalcycles/sounds-tr808-fischer) | `c9f30ff2b4d73b03…` | 22094 |
| `tr808_oh25` | open_hat | 808 short drill | Michael Fischer (1994 TR-808 sample set) | [repo](https://github.com/tidalcycles/sounds-tr808-fischer) | `2674962ca376c0d0…` | 22096 |
| `tr808_oh50` | open_hat | 808 trap | Michael Fischer (1994 TR-808 sample set) | [repo](https://github.com/tidalcycles/sounds-tr808-fischer) | `84ae3220fab23963…` | 44146 |
| `tr808_rs` | perc | 808 rim | Michael Fischer (1994 TR-808 sample set) | [repo](https://github.com/tidalcycles/sounds-tr808-fischer) | `20d5cd385c0f8c3a…` | 22096 |
| `tr808_cb` | perc | 808 cowbell phonk | Michael Fischer (1994 TR-808 sample set) | [repo](https://github.com/tidalcycles/sounds-tr808-fischer) | `1468cbd6c75a1f23…` | 132346 |
| `tr808_ma` | hat | 808 maraca shaker | Michael Fischer (1994 TR-808 sample set) | [repo](https://github.com/tidalcycles/sounds-tr808-fischer) | `7b17cc2973fe5523…` | 22094 |
| `tr808_cl` | perc | 808 clave | Michael Fischer (1994 TR-808 sample set) | [repo](https://github.com/tidalcycles/sounds-tr808-fischer) | `4f5b72674791ce13…` | 22094 |

Total: 53 files, 2.83 MB.
