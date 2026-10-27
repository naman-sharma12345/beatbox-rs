//! FX chains as one (FL Patcher's everyday use): a whole effect chain saved
//! under a name and applied to any track, bus or the master in one call,
//! plus built-in chains for the moves producers make all the time.
//! Every chain is plain add_effect JSON, so an AI can read and edit it.

use crate::engine::Engine;
use crate::fx::Effect;
use crate::tools::{b_or, effects_of, f_or, obj, s_opt, s_req, Tool};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

/// Built-in chains: (name, what it is for, effects as add_effect JSON).
fn builtins() -> Vec<(&'static str, &'static str, Value)> {
    vec![
        ("vocal_clean", "a modern lead vocal: low cut, de-mud, compressor, presence, de-esser, short plate", json!([
            {"type": "parametric_eq", "bands": [{"kind": "low_cut", "freq": 90.0, "gain_db": 0.0, "q": 0.707}, {"kind": "bell", "freq": 300.0, "gain_db": -2.5, "q": 1.0}, {"kind": "high_shelf", "freq": 3000.0, "gain_db": 3.0, "q": 0.7}]},
            {"type": "compressor", "threshold_db": -18.0, "ratio": 3.0, "attack_ms": 8.0, "release_ms": 120.0, "makeup_db": 3.0},
            {"type": "deesser"},
            {"type": "reverb", "size": 0.35, "mix": 0.12}
        ])),
        ("vocal_radio", "telephone / radio voice for intros and ad-libs", json!([
            {"type": "parametric_eq", "bands": [{"kind": "low_cut", "freq": 400.0, "gain_db": 0.0, "q": 0.9}, {"kind": "high_cut", "freq": 3200.0, "gain_db": 0.0, "q": 0.9}, {"kind": "bell", "freq": 1500.0, "gain_db": 4.0, "q": 1.2}]},
            {"type": "saturator", "mode": "transistor", "drive_db": 9.0, "mix": 0.6}
        ])),
        ("lofi_keys", "dusty keys/chords: worn vinyl, muffled top, gentle tape saturation", json!([
            {"type": "vinyl", "crackle": 0.0, "hiss_db": -120.0, "wow_cents": 8.0, "flutter_cents": 2.0, "age": 0.5},
            {"type": "saturator", "mode": "tape", "drive_db": 6.0, "mix": 0.5},
            {"type": "parametric_eq", "bands": [{"kind": "high_cut", "freq": 4500.0, "gain_db": 0.0, "q": 0.707}, {"kind": "bell", "freq": 350.0, "gain_db": -3.0, "q": 1.0}]}
        ])),
        ("parallel_drum_punch", "clean trap / boom-bap drum bus: low-rumble cut, sharper attacks, parallel compression for body, gentle tape colour; leaves room for the master", json!([
            {"type": "parametric_eq", "bands": [{"kind": "low_cut", "freq": 25.0, "gain_db": 0.0, "q": 0.707}, {"kind": "bell", "freq": 320.0, "gain_db": -1.5, "q": 0.8}]},
            {"type": "transient", "attack": 0.22, "sustain": -0.12},
            {"type": "compressor", "threshold_db": -24.0, "ratio": 4.0, "attack_ms": 18.0, "release_ms": 95.0, "knee_db": 6.0, "makeup_db": 3.0, "sc_hpf_hz": 70.0, "mix": 0.32},
            {"type": "saturator", "mode": "tape", "drive_db": 3.0, "mix": 0.18, "output_db": -2.0}
        ])),
        ("pluck_space", "plucks / keys: low cut and de-mud, dotted-eighth ping-pong delay, short plate and bass-safe width; tempo follows the project", json!([
            {"type": "parametric_eq", "bands": [{"kind": "low_cut", "freq": 150.0, "gain_db": 0.0, "q": 0.707}, {"kind": "bell", "freq": 420.0, "gain_db": -2.0, "q": 0.9}]},
            {"type": "delay", "steps": 3.0, "feedback": 0.24, "mix": 0.16, "ping_pong": true, "tone": 4500.0},
            {"type": "reverb", "mode": "fdn_plate", "size": 0.35, "decay_s": 0.85, "predelay_ms": 18.0, "low_cut_hz": 250.0, "mix": 0.12},
            {"type": "width", "amount": 1.15, "low_mono_hz": 180.0}
        ])),
        ("warm_recorded_keys", "recorded piano / keys: 75Hz low cut, light 280Hz de-mud, subtle tape colour and short filtered plate. No added hiss/crackle, delay, limiter or automatic gain match; optional starting point", json!([
            {"type":"parametric_eq","bands":[{"kind":"low_cut","freq":75.0,"gain_db":0.0,"q":0.707},{"kind":"bell","freq":280.0,"gain_db":-1.5,"q":0.8}]},
            {"type":"saturator","mode":"tape","drive_db":2.0,"mix":0.18,"output_db":-0.5,"tone_hz":9000.0,"oversample":true},
            {"type":"reverb","mode":"fdn_plate","size":0.3,"decay_s":0.8,"predelay_ms":16.0,"damping":0.6,"diffusion":0.8,"low_cut_hz":220.0,"width":0.9,"mix":0.1}
        ])),
        ("short_drum_room", "short shared space for dry recorded drums: filtered 0.45s FDN room, 8ms predelay, subtle 16% wet. No compressor, limiter or gain matching; optional starting point, not automatic improvement", json!([
            {"type":"reverb","mode":"fdn_room","size":0.25,"decay_s":0.45,"predelay_ms":8.0,"damping":0.6,"diffusion":0.75,"low_cut_hz":180.0,"width":0.8,"mix":0.16}
        ])),
        ("dusty_drums", "boom-bap / lo-fi drum bus: tape crush, dark top, glue", json!([
            {"type": "saturator", "mode": "tape", "drive_db": 8.0, "mix": 0.6},
            {"type": "parametric_eq", "bands": [{"kind": "high_cut", "freq": 7000.0, "gain_db": 0.0, "q": 0.707}]},
            {"type": "compressor", "threshold_db": -16.0, "ratio": 4.0, "attack_ms": 20.0, "release_ms": 90.0, "makeup_db": 3.0}
        ])),
        ("808_grit", "an 808 that reads on phone speakers: soft-clip harmonics, sub kept mono", json!([
            {"type": "saturator", "mode": "tube", "drive_db": 10.0, "mix": 0.5},
            {"type": "soft_clipper"},
            {"type": "width", "amount": 0.0}
        ])),
        ("wide_pad", "a pad that sits behind everything: low cut, slow chorus, wide, long reverb", json!([
            {"type": "parametric_eq", "bands": [{"kind": "low_cut", "freq": 200.0, "gain_db": 0.0, "q": 0.707}]},
            {"type": "chorus", "mix": 0.4},
            {"type": "width", "amount": 1.4},
            {"type": "reverb", "size": 0.85, "mix": 0.35}
        ])),
        ("clean_chorus_guitar", "an indie clean electric: bright clean amp, wide lush chorus, a dotted-eighth echo and a short plate (picked arpeggios, jangly riffs)", json!([
            {"type": "parametric_eq", "bands": [{"kind": "low_cut", "freq": 110.0, "gain_db": 0.0, "q": 0.707}, {"kind": "bell", "freq": 400.0, "gain_db": -2.0, "q": 1.0}, {"kind": "high_shelf", "freq": 3200.0, "gain_db": 4.0, "q": 0.7}]},
            {"type": "amp_sim", "model": "clean", "gain_db": 3.0, "treble": 0.7, "presence": 0.65, "cab": "2x12", "mix": 0.8},
            {"type": "chorus", "rate_hz": 0.8, "depth_ms": 4.0, "mix": 0.5},
            {"type": "width", "amount": 1.6, "low_mono_hz": 180.0},
            {"type": "delay", "steps": 3, "feedback": 0.22, "mix": 0.14, "ping_pong": true, "tone": 5000.0},
            {"type": "reverb", "mode": "fdn_plate", "size": 0.45, "mix": 0.16}
        ])),
        ("bright_cymbals", "a drum bus with air: kick and snare kept, cymbals and hats lifted above 5 kHz, gentle glue", json!([
            {"type": "parametric_eq", "bands": [{"kind": "high_shelf", "freq": 5000.0, "gain_db": 3.5, "q": 0.7}, {"kind": "bell", "freq": 3000.0, "gain_db": 1.5, "q": 0.9}, {"kind": "bell", "freq": 400.0, "gain_db": -1.5, "q": 1.0}]},
            {"type": "compressor", "threshold_db": -14.0, "ratio": 2.0, "attack_ms": 25.0, "release_ms": 120.0, "makeup_db": 1.0}
        ])),
        ("tape_master", "a warm finishing chain for the master: tape, gentle glue, ceiling -1 dBTP", json!([
            {"type": "saturator", "mode": "tape", "drive_db": 3.0, "mix": 0.4},
            {"type": "compressor", "threshold_db": -12.0, "ratio": 1.8, "attack_ms": 30.0, "release_ms": 200.0, "makeup_db": 1.0},
            {"type": "limiter", "ceiling_db": -1.0}
        ])),
        ("old_record", "the whole track as a worn record: crackle, hiss, wow, narrow band, mono", json!([
            {"type": "vinyl", "crackle": 12.0, "hiss_db": -48.0, "wow_cents": 12.0, "flutter_cents": 4.0, "age": 0.8, "mono": 0.7}
        ])),
    ]
}

fn parse_chain(v: &Value, what: &str) -> Result<Vec<Effect>> {
    let arr = v
        .as_array()
        .ok_or_else(|| anyhow!("{what}: a chain is a list of effects"))?;
    let mut out = Vec::new();
    for (i, x) in arr.iter().enumerate() {
        let fx: Effect = serde_json::from_value(x.clone())
            .with_context(|| format!("{what}: effect {i} ({x})"))?;
        fx.validate()
            .map_err(|m| anyhow!("{what}: effect {i}: {m}"))?;
        out.push(fx);
    }
    Ok(out)
}

fn chain_by_name(e: &Engine, name: &str) -> Result<Vec<Effect>> {
    if let Some(c) = e.project.fx_chains.get(name) {
        return Ok(c.clone());
    }
    if let Some((_, _, v)) = builtins()
        .into_iter()
        .find(|b| b.0.eq_ignore_ascii_case(name))
    {
        return parse_chain(&v, name);
    }
    let mut names: Vec<String> = builtins().iter().map(|b| b.0.to_string()).collect();
    names.extend(e.project.fx_chains.keys().cloned());
    bail!("no fx chain '{name}' (have: {})", names.join(", "))
}

fn types(c: &[Effect]) -> Vec<String> {
    c.iter().map(|x| x.type_name()).collect()
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "list_fx_chains",
            description: "FX chains (FL Patcher-style presets of a whole effect chain): the built-ins (including parallel_drum_punch, pluck_space, dusty_drums, 808_grit and vocal chains) and any saved in this project, each with its effects as add_effect JSON.",
            mutates: false,
            schema: || obj(json!({}), &[]),
            run: |e, _| {
                let mut v: Vec<Value> = builtins()
                    .into_iter()
                    .map(|(n, d, c)| json!({"name": n, "builtin": true, "use": d, "effects": c}))
                    .collect();
                for (n, c) in &e.project.fx_chains {
                    v.push(json!({"name": n, "builtin": false, "effects": serde_json::to_value(c)?}));
                }
                Ok(json!({"chains": v}))
            },
        },
        Tool {
            name: "apply_fx_chain",
            description: "Put a whole FX chain on a track, bus or 'master' in one call: a built-in or saved chain by `chain` name, or your own `effects` list (add_effect JSON, e.g. [{type:'saturator', mode:'tape'}, {type:'reverb', mix:0.2}]). replace=true clears the existing chain first (default: append).",
            mutates: true,
            schema: || obj(json!({
                "track": {"type": "string", "description": "Track, bus or 'master'"},
                "chain": {"type": "string", "description": "Built-in or saved chain name (list_fx_chains)"},
                "effects": {"type": "array", "items": {"type": "object"}, "description": "Instead of a name: the effects, each {type, ...params}"},
                "replace": {"type": "boolean", "description": "Clear the existing chain first (default false)"}
            }), &["track"]),
            run: |e, a| {
                let track = s_req(a, "track")?;
                let chain = match (a.get("effects"), a.get("chain").and_then(|v| v.as_str())) {
                    (Some(fx), _) => parse_chain(fx, "apply_fx_chain")?,
                    (None, Some(n)) => chain_by_name(e, n)?,
                    (None, None) => bail!("apply_fx_chain: give `chain` (a name) or `effects` (a list)"),
                };
                let n = chain.len();
                let replace = b_or(a, "replace", false);
                let dst = effects_of(&mut e.project, &track)?;
                if replace {
                    dst.clear();
                }
                dst.extend(chain);
                let now = types(dst);
                e.project.ensure_fx_ids();
                Ok(json!({"track": track, "added": n, "replaced": replace, "chain": now}))
            },
        },
        Tool {
            name: "preview_return",
            description: "Non-destructive bus_only audition of ONE bus output; mode=mix_ab instead plays full mix WITHOUT selected bus, 0.6s gap, WITH bus. mix_ab includes master/downstream FX, one shared peak reduction, no loudness match. bus_only retains full track/send/sidechain context. First1..8bars(default4), max20seconds incltail. No project/history edits or mute/solo changes. bus_only excludes master FX/fader/downstream bus processing; mix_ab includes them. Muted bus rejected (no hidden unmute). Preview-only attenuation if peak>0.95, never boosts. Writes new WAV, optionally plays; stop=true stops preview. GUI+MCP.",
            mutates:false,
            schema:||obj(json!({"bus":{"type":"string"},"mode":{"type":"string","enum":["bus_only","mix_ab"]},"bars":{"type":"integer","minimum":1,"maximum":8},"play":{"type":"boolean"},"stop":{"type":"boolean"}}), &[]),
            run:|e,a|{
                if b_or(a,"stop",false){e.transport.pending_audition=Some(None);return Ok(json!({"stopped":true}));}
                let mode=match a.get("mode"){Some(v)=>v.as_str().filter(|v|["bus_only","mix_ab"].contains(v)).ok_or_else(||anyhow!("mode must be bus_only or mix_ab"))?,None=>"bus_only"};
                let bi=e.project.bus_index(&s_req(a,"bus")?)?;let bus=&e.project.buses[bi];if bus.mute{bail!("selected bus is muted; unmute explicitly before auditioning");}let name=bus.name.clone();let output=bus.output.clone();
                let bars=match a.get("bars"){Some(v)=>v.as_u64().filter(|v|(1..=8).contains(v)).ok_or_else(||anyhow!("bars must be integer1..8"))?,None=>4};
                e.bank.sync(&e.project.samples);let end=e.project.beat_at_bar(bars as f32+1.0)*4.0;
                let mut mix=crate::render::render(&e.project,&e.bank,&crate::render::RenderOptions{step_range:Some((0,end.round() as u32)),tail:1.0,keep_stems:true,..Default::default()})?;
                let(mut l,mut r)=if mode=="mix_ab"{
                    let mut without=e.project.clone();without.buses[bi].mute=true;
                    let dry=crate::render::render(&without,&e.bank,&crate::render::RenderOptions{step_range:Some((0,end.round()as u32)),tail:1.0,..Default::default()})?;
                    let n=dry.left.len().min(mix.left.len()).min((crate::dsp::SR*20.0)as usize);let gap=(crate::dsp::SR*0.6)as usize;
                    let mut l=dry.left[..n].to_vec();let mut r=dry.right[..n].to_vec();l.extend(std::iter::repeat_n(0.0,gap));r.extend(std::iter::repeat_n(0.0,gap));l.extend_from_slice(&mix.left[..n]);r.extend_from_slice(&mix.right[..n]);(l,r)
                }else{let stem=mix.bus_stems.iter_mut().find(|s|s.name==name).context("selected bus was not rendered")?;(std::mem::take(&mut stem.left),std::mem::take(&mut stem.right))};
                let cap=if mode=="mix_ab"{40.6}else{20.0};let n=l.len().min(r.len()).min((crate::dsp::SR*cap)as usize);l.truncate(n);r.truncate(n);if n==0||!l.iter().chain(&r).all(|v|v.is_finite()){bail!("bus audio must be nonempty and finite");}
                let peak=l.iter().chain(&r).fold(0f32,|m,x|m.max(x.abs()));let gain=if peak>0.95{0.95/peak}else{1.0};for x in l.iter_mut().chain(&mut r){*x*=gain;}
                let mut i=0u64;let path=loop{let p=e.renders_dir().join(format!("return_audition_{i}.wav"));match crate::render::write_wav_new(&p,&l,&r){Ok(())=>break p,Err(err)if err.downcast_ref::<std::io::Error>().is_some_and(|e|e.kind()==std::io::ErrorKind::AlreadyExists)=>i+=1,Err(err)=>return Err(err)}};
                let playback=if b_or(a,"play",true){e.transport.pending.push(crate::engine::TransportCmd::Stop);crate::tools_library::queue_audition(e,l,r,0.0)}else{json!({"playing":false})};
                Ok(json!({"bus":name,"downstream_excluded":if mode=="bus_only"{output}else{None},"path":path,"seconds":n as f32/crate::dsp::SR,"preview_gain_db":20.0*gain.log10(),"playback":playback,"mode":mode,"order":if mode=="mix_ab"{"without bus, 0.6s gap, with bus"}else{"bus only"},"note":if mode=="mix_ab"{"full mix incl master/downstream; shared peak reduction, no loudness match; project unchanged"}else{"bus post-FX/fader only; excludes master/downstream FX; project unchanged"}}))
            }
        },
        Tool {
            name: "set_return_filter",
            description: "Add a dedicated two-band low/high-cut EQ at the END of a bus chain; retunes retain its current position. bus required; low_cut_hz20..1000 default180, high_cut_hz1000..18000 default6000, low must be below high. Preserves unrelated EQ/effects, dry tracks and sends. Reapply enables/retunes only this shortcut's EQ ID (returnfilter prefix), no stacking; no transparent/off mode. Rejects bus automation, including disabled lanes, rather than overwrite automated sound. Shared bus: all contributors affected. Non-brickwall filters, no gain match or limiter; Undo restores.",
            mutates:true,
            schema:||obj(json!({"bus":{"type":"string"},"low_cut_hz":{"type":"number","minimum":20,"maximum":1000},"high_cut_hz":{"type":"number","minimum":1000,"maximum":18000}}), &["bus"]),
            run:|e,a|{
                let bi=e.project.bus_index(&s_req(a,"bus")?)?;let bus=&e.project.buses[bi];
                if e.project.automation.iter().any(|l|l.target.eq_ignore_ascii_case(&bus.name)||l.target==bus.id){bail!("bus has automation; edit/remove its lanes before using this shortcut");}
                let read=|k:&str,d:f64,lo:f64,hi:f64|->Result<f32>{let v=match a.get(k){Some(v)=>v.as_f64().ok_or_else(||anyhow!("{k} must be numeric"))?,None=>d};if !v.is_finite()||v<lo||v>hi{bail!("{k} must be {lo}..{hi}");}Ok(v as f32)};
                let low=read("low_cut_hz",180.,20.,1000.)?;let high=read("high_cut_hz",6000.,1000.,18000.)?;if low>=high{bail!("low cut must be below high cut");}
                let idx=bus.effects.iter().position(|f|matches!(f,Effect::ParametricEq(p) if p.id.starts_with("returnfilter")));
                let id=if let Some(i)=idx{bus.effects[i].id().to_string()}else{crate::ids::next_id(&mut e.project,"returnfilter")};
                let mut fx=parse_chain(&json!([{"type":"parametric_eq","id":id,"bands":[{"kind":"low_cut","freq":low,"q":0.707,"gain_db":0.0},{"kind":"high_cut","freq":high,"q":0.707,"gain_db":0.0}],"output_db":0.0}]),"return filter")?;
                let bus=&mut e.project.buses[bi];if let Some(i)=idx{bus.effects[i]=fx.remove(0);}else{bus.effects.push(fx.remove(0));}
                Ok(json!({"bus":bus.name,"low_cut_hz":low,"high_cut_hz":high,"retuned":idx.is_some(),"note":"only dedicated bus filter changed; all bus contributors affected"}))
            }
        },
        Tool {
            name: "create_echo_send",
            description: "Create a NEW shared fully-wet tempo-synced ping-pong echo return and a post-fader send from one track, in one Undo step. Dry route, track effects and existing sends unchanged. bus must be a unique new name (default track_echo). send_db -36..0 default-12; division eighth|dotted_eighth|quarter (default dotted_eighth), feedback0..0.7 default0.3, tone_hz500..12000 default4500. Timing follows project base BPM, not tempo automation. Return goes to master, not the source's group. No limiter or automatic gain matching. Use set_send to share it with more tracks or change level; ordinary effect controls edit return settings. Existing buses are never retuned.",
            mutates:true,
            schema:||obj(json!({"track":{"type":"string"},"bus":{"type":"string"},"send_db":{"type":"number","minimum":-36,"maximum":0},"division":{"type":"string","enum":["eighth","dotted_eighth","quarter"]},"feedback":{"type":"number","minimum":0,"maximum":0.7},"tone_hz":{"type":"number","minimum":500,"maximum":12000}}), &["track"]),
            run:|e,a| {
                let ti=e.project.track_index(&s_req(a,"track")?)?;
                let source=e.project.tracks[ti].name.clone();
                let bus=match a.get("bus") {Some(v)=>v.as_str().ok_or_else(||anyhow!("bus must be text"))?.trim().to_string(),None=>format!("{source}_echo")};
                if bus.is_empty()||bus.len()>80||bus.chars().any(char::is_control)||bus.eq_ignore_ascii_case("master")||bus.parse::<usize>().is_ok()||e.project.track_index(&bus).is_ok()||e.project.bus_index(&bus).is_ok() {bail!("bus must be a unique new nonempty name, max80characters, not master or an index");}
                if e.project.tracks.iter().any(|t|t.sends.iter().any(|s|s.bus.eq_ignore_ascii_case(&bus))||t.output.as_ref().is_some_and(|b|b.eq_ignore_ascii_case(&bus)))||e.project.buses.iter().any(|b|b.output.as_ref().is_some_and(|b|b.eq_ignore_ascii_case(&bus)))||e.project.automation.iter().any(|l|l.target.eq_ignore_ascii_case(&bus)) {bail!("bus name already has routing or automation references; choose another name");}
                let read=|k:&str,default:f64,lo:f64,hi:f64|->Result<f32>{let v=match a.get(k){Some(v)=>v.as_f64().ok_or_else(||anyhow!("{k} must be numeric"))?,None=>default};if !v.is_finite()||v<lo||v>hi{bail!("{k} must be {lo}..{hi}");}Ok(v as f32)};
                let send=read("send_db",-12.,-36.,0.)?;let feedback=read("feedback",0.3,0.,0.7)?;let tone=read("tone_hz",4500.,500.,12000.)?;
                let division=match a.get("division"){Some(v)=>v.as_str().ok_or_else(||anyhow!("division must be text"))?,None=>"dotted_eighth"};
                let steps=match division{"eighth"=>2.,"dotted_eighth"=>3.,"quarter"=>4.,_=>bail!("division must be eighth, dotted_eighth or quarter")};
                let fx=parse_chain(&json!([{"type":"delay","steps":steps,"feedback":feedback,"mix":1.0,"wet_only":true,"ping_pong":true,"tone":tone}]),"shared echo")?;
                let mut b=crate::project::Bus::new(&bus);b.effects=fx;e.project.buses.push(b);
                e.project.tracks[ti].sends.push(crate::project::Send{bus:bus.clone(),db:send,pre_fader:false,id:String::new()});
                Ok(json!({"track":source,"bus":bus,"send_db":send,"division":division,"steps":steps,"feedback":feedback,"tone_hz":tone,"wet":1.0,"note":"dry route preserved; new fully-wet echo return to master, post-fader send. Base BPM sync, not tempo automation. No level matching or limiter."}))
            }
        },
        Tool {
            name: "create_parallel_send",
            description: "Create a NEW parallel compression bus and post-fader send, preserving dry route, track FX and existing sends. bus defaults track_parallel and must be unique. Compressor mix=1 (processed only), makeup=0, 6dB knee, detector highpass80Hz. send_db-36..0 default-12; threshold_db-40..0 default-24; ratio1..12 default8; attack_ms0.1..100 default20; release_ms10..1000 default120. Return to master. Adds signal and can raise peaks; no limiter or automatic gain matching. Share/adjust with set_send; edit compressor with ordinary effect tools. Undo whole setup.",
            mutates:true,
            schema:||obj(json!({"track":{"type":"string"},"bus":{"type":"string"},"send_db":{"type":"number","minimum":-36,"maximum":0},"threshold_db":{"type":"number","minimum":-40,"maximum":0},"ratio":{"type":"number","minimum":1,"maximum":12},"attack_ms":{"type":"number","minimum":0.1,"maximum":100},"release_ms":{"type":"number","minimum":10,"maximum":1000}}), &["track"]),
            run:|e,a| {
                let ti=e.project.track_index(&s_req(a,"track")?)?;
                let source=e.project.tracks[ti].name.clone();
                let bus=match a.get("bus") {Some(v)=>v.as_str().ok_or_else(||anyhow!("bus must be text"))?.trim().to_string(),None=>format!("{source}_parallel")};
                if bus.is_empty()||bus.len()>80||bus.chars().any(char::is_control)||bus.eq_ignore_ascii_case("master")||bus.parse::<usize>().is_ok()||e.project.track_index(&bus).is_ok()||e.project.bus_index(&bus).is_ok() {bail!("bus must be a unique new nonempty name, max80characters, not master or an index");}
                if e.project.tracks.iter().any(|t|t.sends.iter().any(|s|s.bus.eq_ignore_ascii_case(&bus))||t.output.as_ref().is_some_and(|b|b.eq_ignore_ascii_case(&bus)))||e.project.buses.iter().any(|b|b.output.as_ref().is_some_and(|b|b.eq_ignore_ascii_case(&bus)))||e.project.automation.iter().any(|l|l.target.eq_ignore_ascii_case(&bus)) {bail!("bus name already has routing or automation references; choose another name");}
                let read=|k:&str,default:f64,lo:f64,hi:f64|->Result<f32>{let v=match a.get(k){Some(v)=>v.as_f64().ok_or_else(||anyhow!("{k} must be numeric"))?,None=>default};if !v.is_finite()||v<lo||v>hi{bail!("{k} must be {lo}..{hi}");}Ok(v as f32)};
                let send=read("send_db",-12.,-36.,0.)?;let threshold=read("threshold_db",-24.,-40.,0.)?;let ratio=read("ratio",8.,1.,12.)?;let attack=read("attack_ms",20.,0.1,100.)?;let release=read("release_ms",120.,10.,1000.)?;
                let fx=parse_chain(&json!([{"type":"compressor","threshold_db":threshold,"ratio":ratio,"attack_ms":attack,"release_ms":release,"mix":1.0,"makeup_db":0.0,"knee_db":6.0,"sc_hpf_hz":80.0}]),"parallel compression")?;
                let mut b=crate::project::Bus::new(&bus);b.effects=fx;e.project.buses.push(b);
                e.project.tracks[ti].sends.push(crate::project::Send{bus:bus.clone(),db:send,pre_fader:false,id:String::new()});
                Ok(json!({"track":source,"bus":bus,"send_db":send,"threshold_db":threshold,"ratio":ratio,"attack_ms":attack,"release_ms":release,"processed_mix":1.0,"makeup_db":0.0,"note":"dry route preserved; new processed-only compressor return to master with post-fader send. No limiter or level matching; adds signal and may raise peaks."}))
            }
        },
        Tool {
            name: "create_room_send",
            description: "Create a NEW shared fully-wet FDN room return and a post-fader send from one track, in one Undo step. Dry route, track effects and existing sends unchanged. bus must be a unique new name (default track_room). send_db -36..0 default-12; decay_s0.15..3 default0.65; predelay_ms0..80 default12; low_cut_hz20..1000 default180. Return goes to master, not the source's group. No limiter or automatic gain matching. Use set_send to share it with more tracks or change level; ordinary effect controls edit return settings. Existing buses are never retuned.",
            mutates:true,
            schema:||obj(json!({"track":{"type":"string"},"bus":{"type":"string"},"send_db":{"type":"number","minimum":-36,"maximum":0},"decay_s":{"type":"number","minimum":0.15,"maximum":3},"predelay_ms":{"type":"number","minimum":0,"maximum":80},"low_cut_hz":{"type":"number","minimum":20,"maximum":1000}}), &["track"]),
            run:|e,a| {
                let ti=e.project.track_index(&s_req(a,"track")?)?;
                let source=e.project.tracks[ti].name.clone();
                let bus=match a.get("bus") {Some(v)=>v.as_str().ok_or_else(||anyhow!("bus must be text"))?.trim().to_string(),None=>format!("{source}_room")};
                if bus.is_empty()||bus.len()>80||bus.chars().any(char::is_control)||bus.eq_ignore_ascii_case("master")||bus.parse::<usize>().is_ok()||e.project.track_index(&bus).is_ok()||e.project.bus_index(&bus).is_ok() {bail!("bus must be a unique new nonempty name, max80characters, not master or an index");}
                if e.project.tracks.iter().any(|t|t.sends.iter().any(|s|s.bus.eq_ignore_ascii_case(&bus))||t.output.as_ref().is_some_and(|b|b.eq_ignore_ascii_case(&bus)))||e.project.buses.iter().any(|b|b.output.as_ref().is_some_and(|b|b.eq_ignore_ascii_case(&bus)))||e.project.automation.iter().any(|l|l.target.eq_ignore_ascii_case(&bus)) {bail!("bus name already has routing or automation references; choose another name");}
                let read=|k:&str,default:f64,lo:f64,hi:f64|->Result<f32>{let v=match a.get(k){Some(v)=>v.as_f64().ok_or_else(||anyhow!("{k} must be numeric"))?,None=>default};if !v.is_finite()||v<lo||v>hi{bail!("{k} must be {lo}..{hi}");}Ok(v as f32)};
                let send=read("send_db",-12.,-36.,0.)?;let decay=read("decay_s",0.65,0.15,3.)?;let predelay=read("predelay_ms",12.,0.,80.)?;let low=read("low_cut_hz",180.,20.,1000.)?;
                let fx=parse_chain(&json!([{"type":"reverb","mode":"fdn_room","size":0.3,"decay_s":decay,"predelay_ms":predelay,"damping":0.6,"diffusion":0.75,"low_cut_hz":low,"width":0.9,"mix":1.0}]),"shared room")?;
                let mut b=crate::project::Bus::new(&bus);b.effects=fx;e.project.buses.push(b);
                e.project.tracks[ti].sends.push(crate::project::Send{bus:bus.clone(),db:send,pre_fader:false,id:String::new()});
                Ok(json!({"track":source,"bus":bus,"send_db":send,"decay_s":decay,"predelay_ms":predelay,"low_cut_hz":low,"wet":1.0,"note":"dry route preserved; new fully-wet return to master, post-fader send. No level matching or limiter. Use set_send for other channels."}))
            }
        },
        Tool {
            name: "extract_kit_piece",
            description: "Move one mapped multisample drum key to a new independently mixable channel across every pattern. Copies instrument, effects, fader, pan, output and sends; moves existing notes unchanged (not duplicated), keeps sample files shared. Source automation blocks extraction; enabled GM hat choke blocks extracting keys42/44/46. Audio clips stay on source. Splitting a shared nonlinear FX chain changes processing, not a guaranteed identical mix. Undo restores both channels.",
            mutates: true,
            schema: || obj(json!({"track":{"type":"string"},"key":{"type":"integer","enum":[36,38,39,42,44,46,45,48,49]},"new_track":{"type":"string","description":"optional unique destination name; default source_keyN"}}), &["track","key"]),
            run: |e,a| {
                let ti=e.project.track_index(&s_req(a,"track")?)?;
                let key=a.get("key").and_then(|v|v.as_u64()).filter(|k|[36,38,39,42,44,46,45,48,49].contains(k)).ok_or_else(||anyhow!("key must be a supported integer GM drum key"))? as u8;
                let src=&e.project.tracks[ti];
                let crate::instruments::Instrument::Multisample(p)=&src.instrument else {bail!("requires a mapped multisample drum kit");};
                if p.zones.is_empty() || p.zones.iter().any(|z|z.lo_note!=z.hi_note||z.root!=z.lo_note||!(35..=81).contains(&z.root)) {bail!("requires single-key drum mapping");}
                if !p.zones.iter().any(|z|z.root==key) {bail!("key not mapped in this kit");}
                if p.gm_hat_choke && [42,44,46].contains(&key) {bail!("disable GM hat choke before splitting hat keys; cross-channel choke is not supported");}
                if e.project.automation.iter().any(|l|l.target.eq_ignore_ascii_case(&src.name)||l.target==src.id) {bail!("source has automation; transfer or remove its lanes before extraction");}
                let name=match a.get("new_track") {Some(v)=>v.as_str().ok_or_else(||anyhow!("new_track must be text"))?.trim().to_string(),None=>format!("{}_key{key}",src.name)};
                if name.is_empty() || name.len()>80 || name.chars().any(char::is_control) || name.eq_ignore_ascii_case("master") || name.parse::<usize>().is_ok() || e.project.tracks.iter().any(|t|t.name.eq_ignore_ascii_case(&name)||t.id==name) || e.project.buses.iter().any(|b|b.name.eq_ignore_ascii_case(&name)) {bail!("new_track must be a unique nonempty channel name (max80characters), not master, an ID or numeric index");}
                if e.project.patterns.iter().any(|p|p.clips.keys().any(|k|k.eq_ignore_ascii_case(&name))) {bail!("destination already has pattern notes; choose another name");}
                let source=src.name.clone();
                let count:usize=e.project.patterns.iter().map(|p|p.notes(&source).iter().filter(|n|n.pitch==key).count()).sum();
                if count==0 {bail!("no key{key} notes on {source}; no channel created");}
                if e.project.patterns.iter().any(|p|p.notes(&source).iter().any(|n|n.slide_to.is_some()&&(n.pitch==key||n.slide_to==Some(key)))) {bail!("slides involving this piece are not supported; no notes moved");}
                let mut dest=src.clone();dest.name=name.clone();dest.id.clear();
                for pat in &mut e.project.patterns {let notes=pat.notes_mut(&source);let mut moved=Vec::new();notes.retain(|n|{if n.pitch==key {moved.push(n.clone());false}else{true}});if !moved.is_empty(){pat.clips.insert(name.clone(),moved);}}
                e.project.tracks.push(dest);
                Ok(json!({"source":source,"track":name,"key":key,"notes_moved":count,"note":"moved existing notes across all patterns; copied starting mix/effects/routing; audio clips and source automation not moved. Shared nonlinear FX can sound different once split."}))
            }
        },
        Tool {
            name: "add_kit_piece_flams",
            description: "Add quiet MIDI grace hits before one mapped drum key's MAIN notes in one required pattern. spacing_ms5..40 default18 converts at Apply baseBPM; strength0.1..0.8 default0.45 multiplies main velocity. Original notes unchanged. Generated grace IDs start kitflam and are excluded as sources. Repeated same setup skips collisions instead of stacking; changing spacing may add another layer, so Undo/remove previous flams first. Copies inherit main timing offset/chance, clear slide, short0.1stepgate. Before-zero effective starts and same-key overlaps within0.02steps are skipped. Requires global swing0 (grace positions are fractional). Tempo changes scale spacing. One Undo step; not audio layering or new recordings.",
            mutates:true,
            schema:||obj(json!({"track":{"type":"string"},"key":{"type":"integer","enum":[36,38,39,42,44,46,45,48,49]},"pattern":{"type":"string"},"spacing_ms":{"type":"number","minimum":5,"maximum":40},"strength":{"type":"number","minimum":0.1,"maximum":0.8}}), &["track","key","pattern"]),
            run:|e,a|{
                let ti=e.project.track_index(&s_req(a,"track")?)?;let key=a.get("key").and_then(|v|v.as_u64()).filter(|k|[36,38,39,42,44,46,45,48,49].contains(k)).ok_or_else(||anyhow!("key must be supported GM drum key"))? as u8;
                let src=&e.project.tracks[ti];let crate::instruments::Instrument::Multisample(p)=&src.instrument else{bail!("requires mapped multisample drum kit");};
                if p.zones.is_empty()||p.zones.iter().any(|z|z.lo_note!=z.hi_note||z.root!=z.lo_note||!(35..=81).contains(&z.root))||!p.zones.iter().any(|z|z.root==key){bail!("requires chosen key in a single-key drum mapping");}
                let source=src.name.clone();if e.project.swing!=0.0{bail!("set global swing to zero first; MIDI grace positions do not preserve global swing spacing");}let pi=e.project.pattern_index(&s_req(a,"pattern")?)?;
                let read=|k:&str,d:f64,lo:f64,hi:f64|->Result<f32>{let v=match a.get(k){Some(v)=>v.as_f64().ok_or_else(||anyhow!("{k} must be numeric"))?,None=>d};if !v.is_finite()||v<lo||v>hi{bail!("{k} must be {lo}..{hi}");}Ok(v as f32)};
                let ms=read("spacing_ms",18.,5.,40.)?;let strength=read("strength",0.45,0.1,0.8)?;let bpm=e.project.bpm;if !bpm.is_finite()||bpm<=0.{bail!("tempo must be finite and positive");}let spacing=ms*bpm/15000.;
                let notes=e.project.patterns[pi].notes(&source);let mains:Vec<_>=notes.iter().filter(|n|n.pitch==key&&!n.id.starts_with("kitflam")).cloned().collect();if mains.is_empty(){bail!("no main notes for piece in selected pattern");}
                if mains.iter().any(|n|!n.start.is_finite()||!n.offset.is_finite()||!n.vel.is_finite()){bail!("invalid main note timing or velocity");}
                let mut added=Vec::new();let mut skipped=0;
                for main in mains{let start=main.start-spacing;let actual=start+main.offset;if start<0.||actual<0.||start>=e.project.patterns[pi].steps() as f32||notes.iter().chain(added.iter()).any(|n:&crate::project::Note|n.pitch==key&&((n.start+n.offset)-actual).abs()<0.02){skipped+=1;continue;}let mut grace=main;grace.start=start;grace.len=0.1;grace.vel=(grace.vel*strength).clamp(0.0,1.0);grace.slide_to=None;grace.id.clear();added.push(grace);}
                let count=added.len();for n in &mut added{n.id=crate::ids::next_id(&mut e.project,"kitflam");}if count>0{e.project.patterns[pi].notes_mut(&source).extend(added);}
                Ok(json!({"track":source,"pattern":e.project.patterns[pi].name,"key":key,"added":count,"skipped":skipped,"spacing_ms":ms,"bpm_at_apply":bpm,"note":"main notes intact; changing spacing can add a second grace layer, Undo before changing setup"}))
            }
        },
        Tool {
            name: "set_kit_piece_swing",
            description: "Apply absolute per-piece sixteenth swing to one mapped multisample drum key across ALL patterns. amount0..1 default0.35 delays odd integer sixteenth-grid starts by amount*0.5 steps; even grid starts get zero offset. Only chosen key's on-grid offsets replaced; fractional/off-grid starts and other keys untouched. 0 resets chosen grid notes, not off-grid notes. Existing grid microtiming replaced, not additive. GLOBAL swing still adds on odd grid notes; use global swing0 for piece-only shuffle. Grid starts, velocities, IDs, lengths/chance intact. Tempo changes scale milliseconds; no triplet detection. Undo restores.",
            mutates:true,
            schema:||obj(json!({"track":{"type":"string"},"key":{"type":"integer","enum":[36,38,39,42,44,46,45,48,49]},"amount":{"type":"number","minimum":0,"maximum":1}}), &["track","key"]),
            run:|e,a|{
                let ti=e.project.track_index(&s_req(a,"track")?)?;let key=a.get("key").and_then(|v|v.as_u64()).filter(|k|[36,38,39,42,44,46,45,48,49].contains(k)).ok_or_else(||anyhow!("key must be supported GM drum key"))? as u8;
                let src=&e.project.tracks[ti];let crate::instruments::Instrument::Multisample(p)=&src.instrument else{bail!("requires mapped multisample drum kit");};
                if p.zones.is_empty()||p.zones.iter().any(|z|z.lo_note!=z.hi_note||z.root!=z.lo_note||!(35..=81).contains(&z.root))||!p.zones.iter().any(|z|z.root==key){bail!("requires chosen key in a single-key drum mapping");}
                let amount=match a.get("amount"){Some(v)=>v.as_f64().filter(|v|v.is_finite()&&(0.0..=1.0).contains(v)).ok_or_else(||anyhow!("amount must be numeric 0..1"))?,None=>0.35} as f32;
                let source=src.name.clone();let selected=|n:&&crate::project::Note|n.pitch==key&&n.start.is_finite()&&n.start.fract()==0.0;
                let count:usize=e.project.patterns.iter().map(|p|p.notes(&source).iter().filter(selected).count()).sum();if count==0{bail!("no on-grid notes for selected piece");}
                for pat in &mut e.project.patterns{if let Some(notes)=pat.clips.get_mut(&source.to_lowercase()){for n in notes.iter_mut().filter(|n|n.pitch==key&&n.start.is_finite()&&n.start.fract()==0.0){n.offset=if(n.start as i64)%2==1{amount*0.5}else{0.0};}}}
                Ok(json!({"track":source,"key":key,"notes":count,"amount":amount,"offbeat_offset_steps":amount*0.5,"global_swing":e.project.swing,"note":"replaced chosen grid offsets only; fractional starts untouched; global swing adds separately"}))
            }
        },
        Tool {
            name:"add_kit_piece_roll",
            description:"Add a mapped drum roll into an EMPTY chosen-piece range of ONE pattern, with velocity build/fade, one Undo. Required track/pattern/key/from_step/to_step are zero-based sixteenth steps [from,to). subdivision eighth|sixteenth|thirty_second|eighth_triplet|sixteenth_triplet maps2|1|0.5|4/3|2/3 steps. Legacy interval_steps0.5|1|2 still accepted; never supply both. Default sixteenth. Range must fit pattern, exact multiple of interval, max128hits, at least2hits. Rejects any chosen-key grid/effective onset in range or within0.02steps of new hits, never deletes/overwrites notes. Requires global swing0 for exact subdivisions. start/end_velocity0.02..1 defaults0.25/0.95, curve0.25..4 default1 normalized onset order. Notes have short0.1stepgate/prob1/offset0/no slide, unique IDs. Existing notes/other keys intact. Recorded kit layers only, no gain matching; one-shot may let hits overlap.",
            mutates:true,
            schema:||obj(json!({"track":{"type":"string"},"pattern":{"type":"string"},"key":{"type":"integer","enum":[36,38,39,42,44,46,45,48,49]},"from_step":{"type":"number","minimum":0},"to_step":{"type":"number","minimum":0},"interval_steps":{"type":"number","enum":[0.5,1,2]},"subdivision":{"type":"string","enum":["eighth","sixteenth","thirty_second","eighth_triplet","sixteenth_triplet"]},"start_velocity":{"type":"number","minimum":0.02,"maximum":1},"end_velocity":{"type":"number","minimum":0.02,"maximum":1},"curve":{"type":"number","minimum":0.25,"maximum":4}}), &["track","pattern","key","from_step","to_step"]),
            run:|e,a|{
                let ti=e.project.track_index(&s_req(a,"track")?)?;let pi=e.project.pattern_index(&s_req(a,"pattern")?)?;
                let key=a.get("key").and_then(|v|v.as_u64()).filter(|k|[36,38,39,42,44,46,45,48,49].contains(k)).ok_or_else(||anyhow!("key must be supported GM drum key"))?as u8;
                let src=&e.project.tracks[ti];let crate::instruments::Instrument::Multisample(p)=&src.instrument else{bail!("requires mapped multisample drum kit");};if p.zones.is_empty()||p.zones.iter().any(|z|z.lo_note!=z.hi_note||z.root!=z.lo_note||!(35..=81).contains(&z.root))||!p.zones.iter().any(|z|z.root==key){bail!("requires chosen key in a single-key drum mapping");}
                if e.project.swing!=0.{bail!("set global swing0 before adding exact roll subdivisions");}let source=src.name.clone();
                let read=|k:&str,d:Option<f64>,lo:f64,hi:f64|->Result<f64>{let v=match a.get(k){Some(v)=>v.as_f64().ok_or_else(||anyhow!("{k} must be numeric"))?,None=>d.ok_or_else(||anyhow!("{k} required"))?};if !v.is_finite()||v<lo||v>hi{bail!("{k} must be {lo}..{hi}");}Ok(v)};
                let from=read("from_step",None,0.,e.project.patterns[pi].steps()as f64)?;let to=read("to_step",None,0.,e.project.patterns[pi].steps()as f64)?;if from>=to{bail!("from_step must be less than to_step");}
                if a.get("subdivision").is_some()&&a.get("interval_steps").is_some(){bail!("choose subdivision OR interval_steps, not both");}
                let interval=if let Some(v)=a.get("subdivision"){match v.as_str(){Some("eighth")=>2.,Some("sixteenth")=>1.,Some("thirty_second")=>0.5,Some("eighth_triplet")=>4./3.,Some("sixteenth_triplet")=>2./3.,_=>bail!("unsupported subdivision")}}else{let i=read("interval_steps",Some(1.),0.5,2.)?;if ![0.5,1.,2.].contains(&i){bail!("interval_steps must be0.5,1or2");}i};let count=(to-from)/interval;if(count-count.round()).abs()>1e-6||!(2.0..=128.).contains(&count){bail!("range must hold2..128hits, exact multiple of interval");}let count=count.round()as usize;
                let start=read("start_velocity",Some(0.25),0.02,1.)?as f32;let end=read("end_velocity",Some(0.95),0.02,1.)?as f32;let curve=read("curve",Some(1.),0.25,4.)?as f32;
                let notes=e.project.patterns[pi].notes(&source);if notes.iter().any(|n|n.pitch==key&&(!n.start.is_finite()||!n.offset.is_finite())){bail!("chosen piece has invalid note timing");}
                if notes.iter().filter(|n|n.pitch==key).any(|n|{let grid=n.start as f64;let actual=(n.start+n.offset)as f64;(grid>=from&&grid<to)||(actual>=from&&actual<to)||(0..count).any(|i|(actual-(from+i as f64*interval)).abs()<0.02)}){bail!("chosen-piece range occupied or collides with nearby hit; no notes added");}
                let mut added=Vec::with_capacity(count);for i in 0..count{let v=if i==0{start}else if i+1==count{end}else{start+(end-start)*(i as f32/(count-1)as f32).powf(curve)};let mut n=crate::project::Note::new((from+i as f64*interval)as f32,0.1,key,v);n.id=crate::ids::next_id(&mut e.project,"kitroll");added.push(n);}e.project.patterns[pi].notes_mut(&source).extend(added);
                Ok(json!({"track":source,"pattern":e.project.patterns[pi].name,"key":key,"added":count,"from_step":from,"to_step":to,"interval_steps":interval,"subdivision":a.get("subdivision").and_then(Value::as_str),"note":"new MIDI roll only, existing notes intact; Undo restores"}))
            }
        },
        Tool {
            name: "set_kit_piece_ramp",
            description: "Replace one mapped drum key's MIDI velocities in ONE pattern with a shaped build or fade. Optional from_step/to_step select grid starts in [from,to), both required together, 0<=from<to<=pattern steps. Omitted means full pattern. curve0.25..4 default1: >1 delays change, <1 changes early, normalized onset index raised to curve. pattern/track/key required; start_velocity/end_velocity0.02..1 default0.25/0.95. Sort distinct grid starts chronologically, offsets do not reorder groups; simultaneous hits share level. At least two distinct starts required. Replaces prior velocities including grace hits, not a multiplier. Timing, IDs, chance, expression, other keys/patterns unchanged. Recorded layers may change naturally. No added notes, limiter or gain matching. Undo restores.",
            mutates:true,
            schema:||obj(json!({"track":{"type":"string"},"pattern":{"type":"string"},"key":{"type":"integer","enum":[36,38,39,42,44,46,45,48,49]},"start_velocity":{"type":"number","minimum":0.02,"maximum":1},"end_velocity":{"type":"number","minimum":0.02,"maximum":1},"from_step":{"type":"number","minimum":0},"to_step":{"type":"number","minimum":0},"curve":{"type":"number","minimum":0.25,"maximum":4}}), &["track","pattern","key"]),
            run:|e,a|{
                let ti=e.project.track_index(&s_req(a,"track")?)?;let pi=e.project.pattern_index(&s_req(a,"pattern")?)?;
                let key=a.get("key").and_then(|v|v.as_u64()).filter(|k|[36,38,39,42,44,46,45,48,49].contains(k)).ok_or_else(||anyhow!("key must be supported GM drum key"))? as u8;
                let src=&e.project.tracks[ti];let crate::instruments::Instrument::Multisample(p)=&src.instrument else{bail!("requires mapped multisample drum kit");};
                if p.zones.is_empty()||p.zones.iter().any(|z|z.lo_note!=z.hi_note||z.root!=z.lo_note||!(35..=81).contains(&z.root))||!p.zones.iter().any(|z|z.root==key){bail!("requires chosen key in a single-key drum mapping");}
                let read=|k:&str,d:f64|->Result<f32>{let v=match a.get(k){Some(v)=>v.as_f64().ok_or_else(||anyhow!("{k} must be numeric"))?,None=>d};if !v.is_finite()||!(0.02..=1.0).contains(&v){bail!("{k} must be 0.02..1");}Ok(v as f32)};
                let start=read("start_velocity",0.25)?;let end=read("end_velocity",0.95)?;let source=src.name.clone();
                let steps=e.project.patterns[pi].steps()as f64;
                let (from,to)=match(a.get("from_step"),a.get("to_step")){(None,None)=>(0.,steps),(Some(f),Some(t))=>{let f=f.as_f64().filter(|v|v.is_finite()).ok_or_else(||anyhow!("from_step must be numeric"))?;let t=t.as_f64().filter(|v|v.is_finite()).ok_or_else(||anyhow!("to_step must be numeric"))?;if f<0.||t>steps||f>=t{bail!("range must satisfy 0<=from_step<to_step<=pattern steps");}(f,t)},_=>bail!("from_step and to_step must be supplied together")};
                let curve=match a.get("curve"){Some(v)=>v.as_f64().filter(|v|v.is_finite()&&(0.25..=4.).contains(v)).ok_or_else(||anyhow!("curve must be numeric0.25..4"))?,None=>1.}as f32;
                let selected=|n:&crate::project::Note|n.pitch==key&&(n.start as f64)>=from&&(n.start as f64)<to;
                let notes=e.project.patterns[pi].notes(&source);if notes.iter().any(|n|n.pitch==key&&!n.start.is_finite()){bail!("selected notes have invalid start positions");}
                let mut starts:Vec<f32>=notes.iter().filter(|n|selected(n)).map(|n|n.start).collect();starts.sort_by(f32::total_cmp);starts.dedup_by(|a,b|*a==*b);if starts.len()<2{bail!("requires at least two distinct starts for chosen piece in selected pattern");}
                let mut count=0;for n in e.project.patterns[pi].notes_mut(&source).iter_mut().filter(|n|selected(n)){let i=starts.iter().position(|s|*s==n.start).unwrap();n.vel=if i==0{start}else if i+1==starts.len(){end}else{if curve==1.{start+(end-start)*i as f32/(starts.len()-1)as f32}else{start+(end-start)*(i as f32/(starts.len()-1)as f32).powf(curve)}};count+=1;}
                Ok(json!({"track":source,"pattern":e.project.patterns[pi].name,"key":key,"notes":count,"onset_groups":starts.len(),"start_velocity":start,"end_velocity":end,"from_step":from,"to_step":to,"curve":curve,"note":"replaced velocities in one pattern only, including grace hits; timing and other pieces unchanged"}))
            }
        },
        Tool {
            name: "set_kit_piece_accents",
            description: "Replace MIDI velocities of one mapped multisample drum key across ALL patterns with repeating accents. accent0.02..1 default0.85, soft0.02..1 default0.45, every2or4 default2, phase0..every-1 default0. Counts distinct grid starts in each pattern, sorted chronologically; simultaneous hits share velocity; phase restarts per pattern, offsets do not reorder groups. All selected piece velocities replaced, not scaled or stacked; other keys, timing, IDs and expression unchanged. Uses existing recorded layers only. Undo restores prior dynamics. No automatic gain matching or quality judgment.",
            mutates:true,
            schema:||obj(json!({"track":{"type":"string"},"key":{"type":"integer","enum":[36,38,39,42,44,46,45,48,49]},"accent":{"type":"number","minimum":0.02,"maximum":1},"soft":{"type":"number","minimum":0.02,"maximum":1},"every":{"type":"integer","enum":[2,4]},"phase":{"type":"integer","minimum":0,"maximum":3}}), &["track","key"]),
            run:|e,a|{
                let ti=e.project.track_index(&s_req(a,"track")?)?;let key=a.get("key").and_then(|v|v.as_u64()).filter(|k|[36,38,39,42,44,46,45,48,49].contains(k)).ok_or_else(||anyhow!("key must be supported GM drum key"))? as u8;
                let src=&e.project.tracks[ti];let crate::instruments::Instrument::Multisample(p)=&src.instrument else{bail!("requires mapped multisample drum kit");};
                if p.zones.is_empty()||p.zones.iter().any(|z|z.lo_note!=z.hi_note||z.root!=z.lo_note||!(35..=81).contains(&z.root))||!p.zones.iter().any(|z|z.root==key){bail!("requires chosen key in a single-key drum mapping");}
                let read=|k:&str,d:f64|->Result<f32>{let v=match a.get(k){Some(v)=>v.as_f64().ok_or_else(||anyhow!("{k} must be numeric"))?,None=>d};if !v.is_finite()||!(0.02..=1.0).contains(&v){bail!("{k} must be 0.02..1");}Ok(v as f32)};
                let accent=read("accent",0.85)?;let soft=read("soft",0.45)?;
                let every=match a.get("every"){Some(v)=>v.as_u64().filter(|v|[2,4].contains(v)).ok_or_else(||anyhow!("every must be 2 or 4"))?,None=>2};
                let phase=match a.get("phase"){Some(v)=>v.as_u64().filter(|v|*v<every).ok_or_else(||anyhow!("phase must be integer below every"))?,None=>0};
                let source=src.name.clone();let count:usize=e.project.patterns.iter().map(|p|p.notes(&source).iter().filter(|n|n.pitch==key).count()).sum();if count==0{bail!("no notes for selected piece");}
                if e.project.patterns.iter().flat_map(|p|p.notes(&source)).any(|n|n.pitch==key&&!n.start.is_finite()){bail!("selected notes have invalid start positions");}
                for pat in &mut e.project.patterns{if let Some(notes)=pat.clips.get_mut(&source.to_lowercase()){let mut starts:Vec<f32>=notes.iter().filter(|n|n.pitch==key).map(|n|n.start).collect();starts.sort_by(f32::total_cmp);starts.dedup_by(|a,b|*a==*b);for n in notes.iter_mut().filter(|n|n.pitch==key){let idx=starts.iter().position(|s|*s==n.start).unwrap();n.vel=if idx as u64%every==phase{accent}else{soft};}}}
                Ok(json!({"track":source,"key":key,"notes":count,"accent":accent,"soft":soft,"every":every,"phase":phase,"note":"replaced selected piece velocities across all patterns; pattern-local onset groups, grid and other keys unchanged"}))
            }
        },
        Tool {
            name: "set_kit_piece_pocket",
            description: "Set ABSOLUTE timing offsets on one mapped multisample drum key across all patterns. offset_ms -50..50 converts at Apply base BPM into sixteenth-step offset (must fit -0.5..0.5). Positive lays back, negative pushes ahead; replaces prior offsets, not additive. Zero resets. Grid starts, velocity, IDs, chance, length, pitch and other keys unchanged. Existing per-note timing on this piece is replaced. Later tempo changes scale milliseconds; not a live ms link. First negative events may clamp at song start; no wrap. No randomization or automatic feel judgment. Undo restores.",
            mutates:true,
            schema:||obj(json!({"track":{"type":"string"},"key":{"type":"integer","enum":[36,38,39,42,44,46,45,48,49]},"offset_ms":{"type":"number","minimum":-50,"maximum":50}}), &["track","key","offset_ms"]),
            run:|e,a|{
                let ti=e.project.track_index(&s_req(a,"track")?)?;let key=a.get("key").and_then(|v|v.as_u64()).filter(|k|[36,38,39,42,44,46,45,48,49].contains(k)).ok_or_else(||anyhow!("key must be supported GM drum key"))? as u8;
                let ms=a.get("offset_ms").and_then(|v|v.as_f64()).filter(|v|v.is_finite()&&(-50.0..=50.0).contains(v)).ok_or_else(||anyhow!("offset_ms must be numeric -50..50"))?;
                let src=&e.project.tracks[ti];let crate::instruments::Instrument::Multisample(p)=&src.instrument else{bail!("requires mapped multisample drum kit");};
                if p.zones.is_empty()||p.zones.iter().any(|z|z.lo_note!=z.hi_note||z.root!=z.lo_note||!(35..=81).contains(&z.root))||!p.zones.iter().any(|z|z.root==key){bail!("requires chosen key in a single-key drum mapping");}
                let bpm=e.project.bpm;if !bpm.is_finite()||bpm<=0.0{bail!("tempo must be finite and positive");}let offset=(ms*bpm as f64/15000.0) as f32;if offset.abs()>0.5{bail!("offset exceeds half a sixteenth step at this tempo; choose fewer milliseconds");}
                let source=src.name.clone();let count:usize=e.project.patterns.iter().map(|p|p.notes(&source).iter().filter(|n|n.pitch==key).count()).sum();if count==0{bail!("no notes for selected key");}
                for pat in &mut e.project.patterns{if let Some(notes)=pat.clips.get_mut(&source.to_lowercase()){for n in notes.iter_mut().filter(|n|n.pitch==key){n.offset=offset;}}}
                Ok(json!({"track":source,"key":key,"notes":count,"offset_ms":ms,"offset_steps":offset,"bpm_at_apply":bpm,"note":"replaced piece offsets across all patterns; grid/velocities/IDs unchanged. Tempo changes scale milliseconds."}))
            }
        },
        Tool {
            name: "set_kit_piece_length",
            description: "Absolute per-GM-key maximum playback length for existing single-key multisample drum kit. length_ms:0 off or10..5000. Shortens playback with5ms endfade;never extends original sound. Applies after pitch resampling and gate/release. Repeat replaces,0resets. MIDI,zones,otherkeysettings unchanged;not time stretch.",
            mutates:true,
            schema:||obj(json!({"track":{"type":"string"},"key":{"type":"integer","enum":[36,38,39,42,44,46,45,48,49]},"length_ms":{"type":"number","minimum":0,"maximum":5000}}), &["track","key","length_ms"]),
            run:|e,a|{
                let ti=e.project.track_index(&s_req(a,"track")?)?;let key=a.get("key").and_then(|v|v.as_u64()).ok_or_else(||anyhow!("key must be integer"))?;if ![36,38,39,42,44,46,45,48,49].contains(&key){bail!("unsupported GM drum key");}
                let gain=a.get("length_ms").and_then(|v|v.as_f64()).ok_or_else(||anyhow!("length_ms must be numeric"))?;if !gain.is_finite()||!(gain==0.0||(10.0..=5000.0).contains(&gain)){bail!("length_ms must be 0(off) or 10..5000");}
                let crate::instruments::Instrument::Multisample(p)=&mut e.project.tracks[ti].instrument else{bail!("requires a multisample drum kit");};
                if p.zones.is_empty()||p.zones.iter().any(|z|z.lo_note!=z.hi_note||z.root!=z.lo_note||!(35..=81).contains(&z.root)){bail!("requires single-key drum mapping,not melodic zones");}
if !p.zones.iter().any(|z|z.root==key as u8){bail!("chosen key not mapped in this kit");}
                if gain==0.0{p.key_length_ms.remove(&(key as u8));}else{p.key_length_ms.insert(key as u8,gain as f32);}
                Ok(json!({"track":e.project.tracks[ti].name,"key":key,"length_ms":gain,"note":"absolute playback cap with5ms endfade;0off;does not lengthen source or edit MIDI"}))
            }
        },
        Tool {
            name:"set_kit_piece_pan",
            description:"Absolute per-key stereo balance on existing mapped multisample drum kit, pan-1left..1right,0reset. Attenuates opposite side, no gain boost or synthesized stereo. Applied to note voices before channel effects/pan, all patterns. Other keys/MIDI/zones intact; channel pan and stereo FX still apply. Shared nonlinear FX may change interactions. Direct kit track only; instrument audition is mono and does not reflect this mix placement. Undo restores.",
            mutates:true,
            schema:||obj(json!({"track":{"type":"string"},"key":{"type":"integer","enum":[36,38,39,42,44,46,45,48,49]},"pan":{"type":"number","minimum":-1,"maximum":1}}), &["track","key","pan"]),
            run:|e,a|{
                let ti=e.project.track_index(&s_req(a,"track")?)?;let key=a.get("key").and_then(|v|v.as_u64()).filter(|k|[36,38,39,42,44,46,45,48,49].contains(k)).ok_or_else(||anyhow!("unsupported GM drum key"))?as u8;
                let pan=a.get("pan").and_then(Value::as_f64).filter(|v|v.is_finite()&&(-1.0..=1.).contains(v)).ok_or_else(||anyhow!("pan must be numeric-1..1"))?as f32;
                let crate::instruments::Instrument::Multisample(p)=&mut e.project.tracks[ti].instrument else{bail!("requires mapped multisample kit");};if p.zones.is_empty()||p.zones.iter().any(|z|z.lo_note!=z.hi_note||z.root!=z.lo_note||!(35..=81).contains(&z.root))||!p.zones.iter().any(|z|z.root==key){bail!("requires chosen key in single-key drum mapping");}
                if pan==0.{p.key_pan.remove(&key);}else{p.key_pan.insert(key,pan);}
                Ok(json!({"track":e.project.tracks[ti].name,"key":key,"pan":pan,"note":"per-key linear balance before channel FX/pan; no gain compensation; mono instrument audition unaffected"}))
            }
        },
        Tool {
            name:"set_kit_piece_attack",
            description:"Absolute per-key attack override for an existing mapped multisample drum kit. attack_ms0reset/use kit envelope, or0.5..200ms linear fade-in, selected piece across all patterns. Softens onset, does not move note start or extend recording. Preserves other keys, MIDI, zones and kit-wide attack. Reapply replaces, no stacking. Instrument audition reflects envelope; channel effects/pan still apply in mix. No limiter/gain matching. Undo restores.",
            mutates:true,
            schema:||obj(json!({"track":{"type":"string"},"key":{"type":"integer","enum":[36,38,39,42,44,46,45,48,49]},"attack_ms":{"type":"number","minimum":0,"maximum":200}}), &["track","key","attack_ms"]),
            run:|e,a|{
                let ti=e.project.track_index(&s_req(a,"track")?)?;let key=a.get("key").and_then(|v|v.as_u64()).filter(|k|[36,38,39,42,44,46,45,48,49].contains(k)).ok_or_else(||anyhow!("unsupported GM drum key"))?as u8;
                let ms=a.get("attack_ms").and_then(Value::as_f64).filter(|v|v.is_finite()&&(*v==0.||(0.5..=200.).contains(v))).ok_or_else(||anyhow!("attack_ms must be0(reset)or0.5..200"))?as f32;
                let crate::instruments::Instrument::Multisample(p)=&mut e.project.tracks[ti].instrument else{bail!("requires mapped multisample kit");};if p.zones.is_empty()||p.zones.iter().any(|z|z.lo_note!=z.hi_note||z.root!=z.lo_note||!(35..=81).contains(&z.root))||!p.zones.iter().any(|z|z.root==key){bail!("requires chosen key in single-key drum mapping");}
                if ms==0.{p.key_attack_ms.remove(&key);}else{p.key_attack_ms.insert(key,ms);}
                Ok(json!({"track":e.project.tracks[ti].name,"key":key,"attack_ms":ms,"note":"selected-key attack override;0uses kit envelope, no MIDI change or gain match"}))
            }
        },
        Tool {
            name:"set_kit_piece_velocity_curve",
            description:"Absolute per-key velocity response override for a mapped drum kit. curve0reset/use kit curve,or0.25..4. Below1brings quiet hits forward,above1makes them softer;1linear bypasses kit curve for this piece. Replaces, not multiplied with kit curve. Affects existing recorded velocity-layer selection and velocity gain, not MIDI. No layers created or loudness match;1does not mean reset if kit curve differs. Selected key across all patterns; other keys/zones intact; Undo restores.",
            mutates:true,
            schema:||obj(json!({"track":{"type":"string"},"key":{"type":"integer","enum":[36,38,39,42,44,46,45,48,49]},"curve":{"type":"number","minimum":0,"maximum":4}}), &["track","key","curve"]),
            run:|e,a|{
                let ti=e.project.track_index(&s_req(a,"track")?)?;let key=a.get("key").and_then(Value::as_u64).filter(|k|[36,38,39,42,44,46,45,48,49].contains(k)).ok_or_else(||anyhow!("unsupported GM drum key"))?as u8;
                let hz=a.get("curve").and_then(Value::as_f64).filter(|v|v.is_finite()&&(*v==0.||(0.25..=4.).contains(v))).ok_or_else(||anyhow!("curve must be0(use kit)or0.25..4"))?as f32;
                let crate::instruments::Instrument::Multisample(p)=&mut e.project.tracks[ti].instrument else{bail!("requires mapped multisample kit");};if p.zones.is_empty()||p.zones.iter().any(|z|z.lo_note!=z.hi_note||z.root!=z.lo_note||!(35..=81).contains(&z.root))||!p.zones.iter().any(|z|z.root==key){bail!("requires chosen key in single-key drum mapping");}
                if hz==0.{p.key_velocity_curve.remove(&key);}else{p.key_velocity_curve.insert(key,hz);}
                Ok(json!({"track":e.project.tracks[ti].name,"key":key,"curve":hz,"note":"per-key response replaces kit curve for layers and gain;0uses kit curve;MIDI unchanged"}))
            }
        },
        Tool {
            name:"set_kit_piece_highpass",
            description:"Remove low frequencies from one mapped drum key with two cascaded DC-blocking high-pass stages after optional low-pass and before envelope/channel effects. cutoff_hz0bypass or20..4000, absolute replaces, not stacking. Higher values remove more body. Per-stage pole frequency, not calibrated combined -3dB cutoff. No resonance/gain matching. Same length/MIDI/mapping/other keys, all patterns. State resets per zone/voice, no tail extension; individual samples can change peak. Zero removes override; Undo restores.",
            mutates:true,
            schema:||obj(json!({"track":{"type":"string"},"key":{"type":"integer","enum":[36,38,39,42,44,46,45,48,49]},"cutoff_hz":{"type":"number","minimum":0,"maximum":4000}}), &["track","key","cutoff_hz"]),
            run:|e,a|{
                let ti=e.project.track_index(&s_req(a,"track")?)?;let key=a.get("key").and_then(Value::as_u64).filter(|k|[36,38,39,42,44,46,45,48,49].contains(k)).ok_or_else(||anyhow!("unsupported GM drum key"))?as u8;
                let hz=a.get("cutoff_hz").and_then(Value::as_f64).filter(|v|v.is_finite()&&(*v==0.||(20.0..=4000.).contains(v))).ok_or_else(||anyhow!("cutoff_hz must be0(bypass)or20..4000"))?as f32;
                let crate::instruments::Instrument::Multisample(p)=&mut e.project.tracks[ti].instrument else{bail!("requires mapped multisample kit");};if p.zones.is_empty()||p.zones.iter().any(|z|z.lo_note!=z.hi_note||z.root!=z.lo_note||!(35..=81).contains(&z.root))||!p.zones.iter().any(|z|z.root==key){bail!("requires chosen key in single-key drum mapping");}
                if hz==0.{p.key_highpass_hz.remove(&key);}else{p.key_highpass_hz.insert(key,hz);}
                Ok(json!({"track":e.project.tracks[ti].name,"key":key,"cutoff_hz":hz,"note":"two cascaded DC-blocking high-pass stages before envelope;0bypass;no MIDI/length change or gain match"}))
            }
        },
        Tool {
            name:"set_kit_piece_lowpass",
            description:"Darken one mapped drum key with two cascaded one-pole low-pass stages before its envelope and channel effects. cutoff_hz0bypass or100..18000, absolute replaces, not stacking. Higher values brighter; no resonance or gain matching. Value is the per-stage cutoff, not a calibrated combined -3dB point. Selected piece across all patterns; other keys, MIDI, mapping and playback length unchanged. Filter resets per zone/voice, tail not extended, Undo restores.",
            mutates:true,
            schema:||obj(json!({"track":{"type":"string"},"key":{"type":"integer","enum":[36,38,39,42,44,46,45,48,49]},"cutoff_hz":{"type":"number","minimum":0,"maximum":18000}}), &["track","key","cutoff_hz"]),
            run:|e,a|{
                let ti=e.project.track_index(&s_req(a,"track")?)?;let key=a.get("key").and_then(Value::as_u64).filter(|k|[36,38,39,42,44,46,45,48,49].contains(k)).ok_or_else(||anyhow!("unsupported GM drum key"))?as u8;
                let hz=a.get("cutoff_hz").and_then(Value::as_f64).filter(|v|v.is_finite()&&(*v==0.||(100.0..=18000.).contains(v))).ok_or_else(||anyhow!("cutoff_hz must be0(bypass)or100..18000"))?as f32;
                let crate::instruments::Instrument::Multisample(p)=&mut e.project.tracks[ti].instrument else{bail!("requires mapped multisample kit");};if p.zones.is_empty()||p.zones.iter().any(|z|z.lo_note!=z.hi_note||z.root!=z.lo_note||!(35..=81).contains(&z.root))||!p.zones.iter().any(|z|z.root==key){bail!("requires chosen key in single-key drum mapping");}
                if hz==0.{p.key_lowpass_hz.remove(&key);}else{p.key_lowpass_hz.insert(key,hz);}
                Ok(json!({"track":e.project.tracks[ti].name,"key":key,"cutoff_hz":hz,"note":"two cascaded one-pole stages before envelope;0bypass;no MIDI/length change or gain match"}))
            }
        },
        Tool {
            name:"set_kit_piece_decay",
            description:"Absolute per-key exponential decay for a mapped drum kit. half_life_ms0off or10..2000: amplitude halves each interval, including during attack, from note start. Selected piece across all patterns, existing recording length and MIDI unchanged. Independent of attack/gate/length cap; never extends or boosts sound. Not a sustain compressor, hard gate or loudness match. Other keys/zones/kit envelope intact. Reapply replaces, zero removes override, Undo restores.",
            mutates:true,
            schema:||obj(json!({"track":{"type":"string"},"key":{"type":"integer","enum":[36,38,39,42,44,46,45,48,49]},"half_life_ms":{"type":"number","minimum":0,"maximum":2000}}), &["track","key","half_life_ms"]),
            run:|e,a|{
                let ti=e.project.track_index(&s_req(a,"track")?)?;let key=a.get("key").and_then(Value::as_u64).filter(|k|[36,38,39,42,44,46,45,48,49].contains(k)).ok_or_else(||anyhow!("unsupported GM drum key"))?as u8;
                let ms=a.get("half_life_ms").and_then(Value::as_f64).filter(|v|v.is_finite()&&(*v==0.||(10.0..=2000.).contains(v))).ok_or_else(||anyhow!("half_life_ms must be0(off)or10..2000"))?as f32;
                let crate::instruments::Instrument::Multisample(p)=&mut e.project.tracks[ti].instrument else{bail!("requires mapped multisample kit");};if p.zones.is_empty()||p.zones.iter().any(|z|z.lo_note!=z.hi_note||z.root!=z.lo_note||!(35..=81).contains(&z.root))||!p.zones.iter().any(|z|z.root==key){bail!("requires chosen key in single-key drum mapping");}
                if ms==0.{p.key_decay_ms.remove(&key);}else{p.key_decay_ms.insert(key,ms);}
                Ok(json!({"track":e.project.tracks[ti].name,"key":key,"half_life_ms":ms,"note":"exponential amplitude decay from note start;0off;no length/MIDI change or gain match"}))
            }
        },
        Tool {
            name:"set_kit_piece_reverse",
            description:"Reverse the source recordings for one mapped drum key across all patterns. enabled required bool, false resets. Reverses each selected recorded zone before resampling, attack/gate/length; MIDI start stays fixed, so reverse transient arrives later rather than automatically leading into beat. Full sample reversed even with length cap (cap can cut off transient); one-shot recommended to hear full swell. Other keys, mapping, recordings and MIDI unchanged. Uses existing layers/takes. Reapply replaces, Undo restores, no gain match.",
            mutates:true,
            schema:||obj(json!({"track":{"type":"string"},"key":{"type":"integer","enum":[36,38,39,42,44,46,45,48,49]},"enabled":{"type":"boolean"}}), &["track","key","enabled"]),
            run:|e,a|{
                let ti=e.project.track_index(&s_req(a,"track")?)?;let key=a.get("key").and_then(Value::as_u64).filter(|k|[36,38,39,42,44,46,45,48,49].contains(k)).ok_or_else(||anyhow!("unsupported GM drum key"))?as u8;
                let enabled=a.get("enabled").and_then(Value::as_bool).ok_or_else(||anyhow!("enabled must be boolean"))?;
                let crate::instruments::Instrument::Multisample(p)=&mut e.project.tracks[ti].instrument else{bail!("requires mapped multisample kit");};if p.zones.is_empty()||p.zones.iter().any(|z|z.lo_note!=z.hi_note||z.root!=z.lo_note||!(35..=81).contains(&z.root))||!p.zones.iter().any(|z|z.root==key){bail!("requires chosen key in single-key drum mapping");}
                if enabled{p.reverse_keys.insert(key);}else{p.reverse_keys.remove(&key);}
                Ok(json!({"track":e.project.tracks[ti].name,"key":key,"enabled":enabled,"note":"reversed source before envelope/length; note positions unchanged, no automatic lead-in"}))
            }
        },
        Tool {
            name: "set_kit_piece_tuning",
            description: "Absolute per-GM-key tuning for existing single-key multisample drum kit. key36/38/39/42/44/46/45/48/49;cents-1200..1200. RepeatedApply replaces tuning,not stacked. Zero resets. Preserves recorded zone calibration,mapping,notes,otherkeytuning. Resampling changes pitch and duration together;not time stretch or automatic key detection.",
            mutates:true,
            schema:||obj(json!({"track":{"type":"string"},"key":{"type":"integer","enum":[36,38,39,42,44,46,45,48,49]},"cents":{"type":"number","minimum":-1200,"maximum":1200}}), &["track","key","cents"]),
            run:|e,a|{
                let ti=e.project.track_index(&s_req(a,"track")?)?;let key=a.get("key").and_then(|v|v.as_u64()).ok_or_else(||anyhow!("key must be integer"))?;if ![36,38,39,42,44,46,45,48,49].contains(&key){bail!("unsupported GM drum key");}
                let gain=a.get("cents").and_then(|v|v.as_f64()).ok_or_else(||anyhow!("cents must be numeric"))?;if !gain.is_finite()||!(-1200.0..=1200.0).contains(&gain){bail!("cents must be -1200..1200");}
                let crate::instruments::Instrument::Multisample(p)=&mut e.project.tracks[ti].instrument else{bail!("requires a multisample drum kit");};
                if p.zones.is_empty()||p.zones.iter().any(|z|z.lo_note!=z.hi_note||z.root!=z.lo_note||!(35..=81).contains(&z.root)){bail!("requires single-key drum mapping,not melodic zones");}
if !p.zones.iter().any(|z|z.root==key as u8){bail!("chosen key not mapped in this kit");}
                if gain==0.0{p.key_tune_cents.remove(&(key as u8));}else{p.key_tune_cents.insert(key as u8,gain as f32);}
                Ok(json!({"track":e.project.tracks[ti].name,"key":key,"cents":gain,"note":"absolute cents;resampled pitch and duration change together;mapping and MIDI unchanged"}))
            }
        },
        Tool {
            name: "set_kit_piece_level",
            description: "Absolute per-GM-key trim for existing single-key multisample drum kit. key36/38/39/42/44/46/45/48/49;gain_db-24..6. RepeatedApply replaces trim,not stacked. Zero resets. Preserves recorded zone calibration,mapping,notes,otherkeytrims. Positive trim can clip;no limiter/gainmatch/downloads.",
            mutates:true,
            schema:||obj(json!({"track":{"type":"string"},"key":{"type":"integer","enum":[36,38,39,42,44,46,45,48,49]},"gain_db":{"type":"number","minimum":-24,"maximum":6}}), &["track","key","gain_db"]),
            run:|e,a|{
                let ti=e.project.track_index(&s_req(a,"track")?)?;let key=a.get("key").and_then(|v|v.as_u64()).ok_or_else(||anyhow!("key must be integer"))?;if ![36,38,39,42,44,46,45,48,49].contains(&key){bail!("unsupported GM drum key");}
                let gain=a.get("gain_db").and_then(|v|v.as_f64()).ok_or_else(||anyhow!("gain_db must be numeric"))?;if !gain.is_finite()||!(-24.0..=6.0).contains(&gain){bail!("gain_db must be -24..6");}
                let crate::instruments::Instrument::Multisample(p)=&mut e.project.tracks[ti].instrument else{bail!("requires a multisample drum kit");};
                if p.zones.is_empty()||p.zones.iter().any(|z|z.lo_note!=z.hi_note||z.root!=z.lo_note||!(35..=81).contains(&z.root)){bail!("requires single-key drum mapping,not melodic zones");}
if !p.zones.iter().any(|z|z.root==key as u8){bail!("chosen key not mapped in this kit");}
                if gain==0.0{p.key_trim_db.remove(&(key as u8));}else{p.key_trim_db.insert(key as u8,gain as f32);}
                Ok(json!({"track":e.project.tracks[ti].name,"key":key,"gain_db":gain,"note":"absolute trim,no gainmatch or limiter;check output peak"}))
            }
        },
        Tool {
            name: "set_sample_performance",
            description: "Adjust existing multisample instrument only: attack_ms0.5..200,release_ms5..3000,tune_cents-100..100 (instrument-wide sample-rate fine tuning, changes pitch and recorded duration together, not time-stretch),velocity_amount0..1,velocity_curve0.25..4 (default1, <1 stronger touch, >1 softer; maps velocity for recorded layers and level without MIDI edits),one_shot/round_robin/velocity_blend/gm_hat_choke bool. GMhat choke is opt-in: kit key46 fades at later42/44 on same channel, not cross-channel. Round robin varies recorded takes when available; blend crossfades adjacent recorded velocity layers, not new layers. Omitted fields preserved; zones,mapping,recordings,gain and notes unchanged. One-shot ignores note gate/release and plays whole sample; gated mode uses note length plus release. No downloads or gain matching.",
            mutates: true,
            schema: || obj(json!({"track":{"type":"string"},"attack_ms":{"type":"number","minimum":0.5,"maximum":200},"release_ms":{"type":"number","minimum":5,"maximum":3000},"velocity_amount":{"type":"number","minimum":0,"maximum":1},"velocity_curve":{"type":"number","minimum":0.25,"maximum":4},"tune_cents":{"type":"number","minimum":-100,"maximum":100},"one_shot":{"type":"boolean"},"round_robin":{"type":"boolean"},"velocity_blend":{"type":"boolean"},"gm_hat_choke":{"type":"boolean"}}), &["track"]),
            run: |e,a|{
                let ti=e.project.track_index(&s_req(a,"track")?)?;
                let crate::instruments::Instrument::Multisample(old)=&e.project.tracks[ti].instrument else{bail!("requires a loaded multisample instrument");};let mut p=old.clone();
                let read=|k:&str,lo:f64,hi:f64|->Result<Option<f32>>{match a.get(k){None=>Ok(None),Some(v)=>{let x=v.as_f64().ok_or_else(||anyhow!("{k} must be numeric"))?;if !x.is_finite()||x<lo||x>hi{bail!("{k} must be {lo}..{hi}");}Ok(Some(x as f32))}}};
                if let Some(x)=read("attack_ms",0.5,200.0)?{p.attack=x/1000.0;}
if let Some(x)=read("release_ms",5.0,3000.0)?{p.release=x/1000.0;}
if let Some(x)=read("velocity_amount",0.0,1.0)?{p.vel_to_gain=x;}
if let Some(x)=read("velocity_curve",0.25,4.0)?{p.velocity_curve=x;}
if let Some(x)=read("tune_cents",-100.0,100.0)?{p.tune_cents=x;}
                if let Some(v)=a.get("one_shot"){p.one_shot=v.as_bool().ok_or_else(||anyhow!("one_shot must be boolean"))?;}
                if let Some(v)=a.get("round_robin"){p.round_robin=v.as_bool().ok_or_else(||anyhow!("round_robin must be boolean"))?;}
                if let Some(v)=a.get("velocity_blend"){p.vel_crossfade=v.as_bool().ok_or_else(||anyhow!("velocity_blend must be boolean"))?;}
                if let Some(v)=a.get("gm_hat_choke"){p.gm_hat_choke=v.as_bool().ok_or_else(||anyhow!("gm_hat_choke must be boolean"))?;}
                let out=json!({"track":e.project.tracks[ti].name,"attack_ms":p.attack*1000.0,"release_ms":p.release*1000.0,"velocity_amount":p.vel_to_gain,"velocity_curve":p.velocity_curve,"tune_cents":p.tune_cents,"one_shot":p.one_shot,"round_robin":p.round_robin,"velocity_blend":p.vel_crossfade,"gm_hat_choke":p.gm_hat_choke,"note":"one-shot ignores note gate/release; no downloads or gain matching"});e.project.tracks[ti].instrument=crate::instruments::Instrument::Multisample(p);Ok(out)
            }
        },
        Tool {
            name: "map_drum_notes",
            description: "Explicitly map every note of one existing drum track in one pattern (default first) to a chosen GM drum key:36kick,38snare,39clap,42closedhat,46openhat,45lowtom,48hightom,49crash. Only pitch changes; timing,velocity,chance,offset,length,IDs preserved. Rejects slides and mixed-pitch parts to avoid destroying melodies/wholekit patterns. Does not load samples or alter sound source. Undoable.",
            mutates: true,
            schema: || obj(json!({"track":{"type":"string"},"pattern":{"type":"string"},"key":{"type":"integer","enum":[36,38,39,42,46,45,48,49]}}), &["track","key"]),
            run: |e,a|{
                let ti=e.project.track_index(&s_req(a,"track")?)?;let track=e.project.tracks[ti].name.clone();
                let inst=&e.project.tracks[ti].instrument;let mapped_kit=matches!(inst,crate::instruments::Instrument::Multisample(m) if m.one_shot&&!m.zones.is_empty()&&m.zones.iter().all(|z|z.lo_note==z.hi_note&&z.root==z.lo_note&&(35..=81).contains(&z.root)));
                if !inst.is_drum()&&!mapped_kit{bail!("map_drum_notes requires a drum or single-key mapped sampler kit, not a melodic instrument");}
                let key=a.get("key").and_then(|v|v.as_u64()).ok_or_else(||anyhow!("key must be an integer GM drum key"))?;
                if ![36,38,39,42,46,45,48,49].contains(&key){bail!("unsupported GM drum key");}
                let pi=match s_opt(a,"pattern"){Some(n)=>e.project.pattern_index(&n)?,None=>{if e.project.patterns.is_empty(){bail!("no pattern");}0}};
                let notes=e.project.patterns[pi].notes(&track);
                if notes.iter().any(|n|n.slide_to.is_some()){bail!("slides are not supported; no notes changed");}
                if let Some(first)=notes.first(){if notes.iter().any(|n|n.pitch!=first.pitch){bail!("mixed-pitch part: this action is for one-drum channels only; no notes changed");}}
                if let crate::instruments::Instrument::Multisample(m)=&e.project.tracks[ti].instrument{if !m.zones.iter().any(|z|z.lo_note<=key as u8&&z.hi_note>=key as u8){bail!("chosen GM key is not mapped by this multisample kit");}}
                let changed=notes.iter().filter(|n|n.pitch!=key as u8).count();if changed>0{for n in e.project.patterns[pi].notes_mut(&track){n.pitch=key as u8;}}
                Ok(json!({"track":track,"pattern":e.project.patterns[pi].name,"key":key,"changed":changed,"note":"explicit pitch-only mapping; no sample downloads or instrument changes"}))
            }
        },
        Tool {
            name: "set_bus_glue",
            description: "Set a soft-knee parallel compressor on track,bus or master. threshold_db-40..0,ratio1..8,attack_ms0.1..100,release_ms10..1000,mix0..1,sc_hpf_hz0..300. Retunes/enables first compressor preserving ID and other effects. 6dB knee,no lookahead,no makeup gain,no limiter. Detector low-cut reduces bass-triggered compression,not bass in output. Compare matched A/B.",
            mutates: true,
            schema: || obj(json!({"track":{"type":"string"},"threshold_db":{"type":"number","minimum":-40,"maximum":0},"ratio":{"type":"number","minimum":1,"maximum":8},"attack_ms":{"type":"number","minimum":0.1,"maximum":100},"release_ms":{"type":"number","minimum":10,"maximum":1000},"mix":{"type":"number","minimum":0,"maximum":1},"sc_hpf_hz":{"type":"number","minimum":0,"maximum":300}}), &["track"]),
            run: |e,a|{
                let track=s_req(a,"track")?;
                let read=|k:&str,d:f32,lo:f32,hi:f32|->Result<f32>{let v=match a.get(k){Some(v)=>v.as_f64().ok_or_else(||anyhow!("{k} must be numeric"))? as f32,None=>d};if !v.is_finite()||v<lo||v>hi{bail!("{k} must be {lo}..{hi}");}Ok(v)};
                let threshold=read("threshold_db",-18.0,-40.0,0.0)?;let ratio=read("ratio",2.0,1.0,8.0)?;let attack=read("attack_ms",25.0,0.1,100.0)?;let release=read("release_ms",120.0,10.0,1000.0)?;let mix=read("mix",0.5,0.0,1.0)?;let hpf=read("sc_hpf_hz",90.0,0.0,300.0)?;
                let dst=effects_of(&mut e.project,&track)?;let idx=dst.iter().position(|f|matches!(f,Effect::Compressor(_)));let retuned=idx.is_some();
                let mut p=crate::fx::CompressorFx{threshold_db:threshold,ratio,attack_ms:attack,release_ms:release,mix,sc_hpf_hz:hpf,knee_db:6.0,makeup_db:0.0,lookahead_ms:0.0,..Default::default()};if let Some(i)=idx{p.id=dst[i].id().to_string();dst[i]=Effect::Compressor(p);}else{dst.push(Effect::Compressor(p));}
                e.project.ensure_fx_ids();Ok(json!({"track":track,"threshold_db":threshold,"ratio":ratio,"attack_ms":attack,"release_ms":release,"mix":mix,"sc_hpf_hz":hpf,"retuned":retuned,"note":"6dB soft knee,no lookahead,makeup gain or limiter; detector filter does not cut output bass"}))
            }
        },
        Tool {
            name: "set_track_colour",
            description: "Set tape,tube or transistor saturation on track,bus or master. drive_db0..24,mix0..1,output_db-24..0,tone_hz500..16000. Retunes/enables the first saturator preserving ID and other effects; oversampling enabled. No automatic level match or limiter. Use output trim and matched A/B to compare tone, not loudness.",
            mutates: true,
            schema: || obj(json!({"track":{"type":"string"},"mode":{"type":"string","enum":["tape","tube","transistor"]},"drive_db":{"type":"number","minimum":0,"maximum":24},"mix":{"type":"number","minimum":0,"maximum":1},"output_db":{"type":"number","minimum":-24,"maximum":0},"tone_hz":{"type":"number","minimum":500,"maximum":16000}}), &["track"]),
            run: |e,a|{
                let track=s_req(a,"track")?;let mode=s_opt(a,"mode").unwrap_or_else(||"tape".into());
                let sat=match mode.as_str(){"tape"=>crate::fx::SatMode::Tape,"tube"=>crate::fx::SatMode::Tube,"transistor"=>crate::fx::SatMode::Transistor,_=>bail!("mode must be tape,tube or transistor")};
                let read=|k:&str,d:f32,lo:f32,hi:f32|->Result<f32>{let v=match a.get(k){Some(v)=>v.as_f64().ok_or_else(||anyhow!("{k} must be numeric"))? as f32,None=>d};if !v.is_finite()||v<lo||v>hi{bail!("{k} must be {lo}..{hi}");}Ok(v)};
                let drive=read("drive_db",6.0,0.0,24.0)?;let mix=read("mix",0.3,0.0,1.0)?;let output=read("output_db",-3.0,-24.0,0.0)?;let tone=read("tone_hz",12000.0,500.0,16000.0)?;
                let dst=effects_of(&mut e.project,&track)?;let idx=dst.iter().position(|f|matches!(f,Effect::Saturator(_)));let retuned=idx.is_some();
                let mut p=crate::fx::SaturatorFx{mode:sat,drive_db:drive,mix,output_db:output,tone_hz:tone,oversample:true,..Default::default()};if let Some(i)=idx{p.id=dst[i].id().to_string();dst[i]=Effect::Saturator(p);}else{dst.push(Effect::Saturator(p));}
                e.project.ensure_fx_ids();Ok(json!({"track":track,"mode":mode,"drive_db":drive,"mix":mix,"output_db":output,"tone_hz":tone,"retuned":retuned,"note":"no automatic level match or limiter; compare with matched A/B"}))
            }
        },
        Tool {
            name: "set_drum_punch",
            description: "Shape attacks and tails on a track,bus or master using the existing transient processor. attack -1..1 (negative softens,positive sharpens), sustain -1..1 (negative shortens body,positive lifts it). Retunes/enables first transient effect preserving ID and other effects. No MIDI, automatic gain compensation or peak limiting; check level after applying.",
            mutates: true,
            schema: || obj(json!({"track":{"type":"string"},"attack":{"type":"number","minimum":-1,"maximum":1,"default":0.25},"sustain":{"type":"number","minimum":-1,"maximum":1,"default":-0.15}}), &["track"]),
            run: |e,a|{
                let track=s_req(a,"track")?;
                let read=|k:&str,d:f32|->Result<f32>{let v=match a.get(k){Some(v)=>v.as_f64().ok_or_else(||anyhow!("{k} must be numeric"))? as f32,None=>d};if !v.is_finite()||!(-1.0..=1.0).contains(&v){bail!("{k} must be -1..1");}Ok(v)};
                let attack=read("attack",0.25)?;let sustain=read("sustain",-0.15)?;
                let dst=effects_of(&mut e.project,&track)?;let idx=dst.iter().position(|f|matches!(f,Effect::Transient(_)));let retuned=idx.is_some();
                let mut p=crate::fx::TransientFx{attack,sustain,..Default::default()};if let Some(i)=idx{p.id=dst[i].id().to_string();dst[i]=Effect::Transient(p);}else{dst.push(Effect::Transient(p));}
                e.project.ensure_fx_ids();Ok(json!({"track":track,"attack":attack,"sustain":sustain,"retuned":retuned,"note":"no automatic gain or peak compensation; compare levels"}))
            }
        },
        Tool {
            name: "set_musical_delay",
            description: "Set a tempo-linked delay on a track, bus or master. division eighth=2sixteenth steps,dotted_eighth=3,quarter=4,eighth_triplet=4/3. mix0..0.6,feedback0..0.75,tone_hz500..16000,ping_pong(defaulttrue). Retunes/enables first delay, preserving ID and other effects. Unlike millisecond delay, musical timing uses project base BPM when rendering; not tempo-map tracking.",
            mutates: true,
            schema: || obj(json!({"track":{"type":"string"},"division":{"type":"string","enum":["eighth","dotted_eighth","quarter","eighth_triplet"]},"mix":{"type":"number","minimum":0,"maximum":0.6},"feedback":{"type":"number","minimum":0,"maximum":0.75},"tone_hz":{"type":"number","minimum":500,"maximum":16000},"ping_pong":{"type":"boolean"}}), &["track"]),
            run: |e,a|{
                let track=s_req(a,"track")?;let division=s_opt(a,"division").unwrap_or_else(||"dotted_eighth".into());
                let steps=match division.as_str(){"eighth"=>2.0,"dotted_eighth"=>3.0,"quarter"=>4.0,"eighth_triplet"=>4.0/3.0,_=>bail!("division must be eighth,dotted_eighth,quarter or eighth_triplet")};
                let read=|k:&str,d:f32,lo:f32,hi:f32|->Result<f32>{let v=match a.get(k){Some(v)=>v.as_f64().ok_or_else(||anyhow!("{k} must be numeric"))? as f32,None=>d};if !v.is_finite()||v<lo||v>hi{bail!("{k} must be {lo}..{hi}");}Ok(v)};
                let mix=read("mix",0.18,0.0,0.6)?;let feedback=read("feedback",0.25,0.0,0.75)?;let tone=read("tone_hz",4500.0,500.0,16000.0)?;
                let ping=match a.get("ping_pong"){Some(v)=>v.as_bool().ok_or_else(||anyhow!("ping_pong must be boolean"))?,None=>true};
                let dst=effects_of(&mut e.project,&track)?;let idx=dst.iter().position(|f|matches!(f,Effect::Delay(_)));let retuned=idx.is_some();
                let mut p=crate::fx::DelayFx{steps,feedback,mix,ping_pong:ping,tone,..Default::default()};
                if let Some(i)=idx{p.id=dst[i].id().to_string();dst[i]=Effect::Delay(p);}else{dst.push(Effect::Delay(p));}
                e.project.ensure_fx_ids();Ok(json!({"track":track,"division":division,"steps":steps,"mix":mix,"feedback":feedback,"tone_hz":tone,"ping_pong":ping,"retuned":retuned}))
            }
        },
        Tool {
            name: "set_bass_safe_width",
            description: "Set stereo width on a track, bus or master with reduced low-frequency side energy. width 0=mono,1=original side level,2=double; mono_below_hz 0=off,40..500Hz high-passes the side signal (not a brick wall). Retunes/enables the first width effect, preserving its ID and other effects. Cannot recover bass already cancelled in the mono sum. Compare using preview_mix_translation.",
            mutates: true,
            schema: || obj(json!({"track":{"type":"string"},"width":{"type":"number","minimum":0,"maximum":2,"default":1},"mono_below_hz":{"type":"number","minimum":0,"maximum":500,"default":120}}), &["track"]),
            run: |e,a| {
                let track=s_req(a,"track")?;
                let read=|k:&str,d:f32,lo:f32,hi:f32|->Result<f32>{let v=match a.get(k){Some(v)=>v.as_f64().ok_or_else(||anyhow!("{k} must be numeric"))? as f32,None=>d};if !v.is_finite()||v<lo||v>hi{bail!("{k} must be {lo}..{hi}");}Ok(v)};
                let width=read("width",1.0,0.0,2.0)?;let cutoff=read("mono_below_hz",120.0,0.0,500.0)?;
                if cutoff>0.0&&cutoff<40.0{bail!("mono_below_hz must be0(off)or40..500Hz");}
                let dst=effects_of(&mut e.project,&track)?;let idx=dst.iter().position(|f|matches!(f,Effect::Width(_)));let retuned=idx.is_some();
                let mut p=crate::fx::WidthFx{amount:width,low_mono_hz:cutoff,..Default::default()};
                if let Some(i)=idx{p.id=dst[i].id().to_string();dst[i]=Effect::Width(p);}else{dst.push(Effect::Width(p));}
                e.project.ensure_fx_ids();Ok(json!({"track":track,"width":width,"mono_below_hz":cutoff,"retuned":retuned,"note":"side high-pass, not brick-wall mono; no automatic level compensation"}))
            }
        },
        Tool {
            name: "preview_mix_translation",
            description: "Non-destructive monitoring of the first 1-8 bars (default 4) of the mix, or a local stereo audio file (first 20 seconds). Optional start_s (0..86400) and length_s (1..20, default 8) select an elapsed-time excerpt instead of bars. Full project renders then crops to preserve preceding notes/effect state; may take time. Partial ending is reported. mode stereo, mono (L+R)/2, or phone (mono plus 180Hz low cut / 5kHz high cut, an approximation not a device simulation). No loudness boost: bass loss and cancellation remain audible. Writes a new WAV and optionally queues it on studio speakers. Project/history unchanged.",
            mutates: false,
            schema: || obj(json!({"mode":{"type":"string","enum":["stereo","mono","phone"]},"bars":{"type":"integer","minimum":1,"maximum":8},"start_s":{"type":"number","minimum":0,"maximum":86400},"length_s":{"type":"number","minimum":1,"maximum":20},"path":{"type":"string"},"play":{"type":"boolean","default":true},"stop":{"type":"boolean","description":"Stop a queued/playing translation preview without rendering"}}), &[]),
            run: |e,a| {
                if b_or(a,"stop",false){e.transport.pending_audition=Some(None);return Ok(json!({"stopped":true}));}
                let mode=s_opt(a,"mode").unwrap_or_else(||"mono".into());
                if !["stereo","mono","phone"].contains(&mode.as_str()){bail!("mode must be stereo, mono or phone");}
                let bars=match a.get("bars"){Some(v)=>v.as_u64().filter(|v|(1..=8).contains(v)).ok_or_else(||anyhow!("bars must be an integer 1..8"))?,None=>4};
                let excerpt=a.get("start_s").is_some() || a.get("length_s").is_some();
                if excerpt && a.get("bars").is_some(){bail!("Choose seconds or bars, not both");}
                let number=|key:&str,default:f64,lo:f64,hi:f64|->Result<f64>{match a.get(key){None=>Ok(default),Some(v)=>v.as_f64().filter(|v|v.is_finite() && (lo..=hi).contains(v)).ok_or_else(||anyhow!("{key} must be finite in {lo}..{hi}"))}};
                let start=number("start_s",0.0,0.0,86400.0)?;
                let length=number("length_s",8.0,1.0,20.0)?;
                let (mut l,mut r)=if let Some(path)=s_opt(a,"path"){
                    crate::samples::decode_stereo(&e.resolve(&path))?
                }else{
                    e.bank.sync(&e.project.samples);
                    let end=e.project.beat_at_bar(bars as f32+1.0)*4.0;
                    // Crop a full render for seconds mode: a window-only render would
                    // lose held notes and the delay/reverb state preceding the excerpt.
                    let opts=if excerpt{crate::render::RenderOptions::default()}else{crate::render::RenderOptions{step_range:Some((0,end.round() as u32)),tail:0.5,..Default::default()}};
                    let m=crate::render::render(&e.project,&e.bank,&opts)?;
                    (m.left,m.right)
                };
                let available=l.len().min(r.len());
                let first=if excerpt{(start*crate::dsp::SR as f64).floor() as usize}else{0};
                if first>=available{bail!("Excerpt start is past the available audio");}
                let wanted=if excerpt{(length*crate::dsp::SR as f64).floor() as usize}else{(crate::dsp::SR*20.0) as usize};
                let last=first.saturating_add(wanted).min(available);
                l=l[first..last].to_vec();r=r[first..last].to_vec();let n=last-first;
                if n==0 || !l.iter().chain(&r).all(|v|v.is_finite()){bail!("preview audio must be nonempty and finite");}
                if mode!="stereo"{for i in 0..n{let m=(l[i]+r[i])*0.5;l[i]=m;r[i]=m;}}
                if mode=="phone"{
                    use crate::dsp::{Biquad,BiquadKind};
                    let mut hp=Biquad::new(BiquadKind::LowCut,180.0,0.707,0.0);
                    let mut lp=Biquad::new(BiquadKind::HighCut,5000.0,0.707,0.0);
                    for i in 0..n{let y=lp.process(hp.process(l[i]));l[i]=y;r[i]=y;}
                }
                let peak=l.iter().chain(&r).fold(0.0f32,|m,x|m.max(x.abs()));
                let gain=if peak>0.95 {0.95/peak}else{1.0};
                for x in l.iter_mut().chain(&mut r){*x*=gain;}
                let mut i=0u64;
                let path=loop{let p=e.renders_dir().join(format!("translation_{mode}_{i}.wav"));match crate::render::write_wav_new(&p,&l,&r){Ok(())=>break p,Err(err) if err.downcast_ref::<std::io::Error>().is_some_and(|e|e.kind()==std::io::ErrorKind::AlreadyExists)=>i+=1,Err(err)=>return Err(err)}};
                let playback=if b_or(a,"play",true){e.transport.pending.push(crate::engine::TransportCmd::Stop);crate::tools_library::queue_audition(e,l,r,0.0)}else{json!({"playing":false})};
                Ok(json!({"mode":mode,"path":path,"seconds":n as f32/crate::dsp::SR,"start_s":first as f64/crate::dsp::SR as f64,"requested_length_s":if excerpt{Some(length)}else{None},"truncated":excerpt && n<wanted,"safety_gain_db":20.0*gain.log10(),"playback":playback,"note":"monitoring approximation only; no mix changes or loudness boost"}))
            }
        },
        Tool {
            name: "duck_to_track",
            description: "Set up note-trigger ducking on a track or bus from a source track (for example kick into 808). Depth is dB attenuation at full duck; release_beats converts at the current tempo when applied, not a live tempo link. Follows source note triggers, not audio level. Retunes the first sidechain from that source instead of stacking; enables it. Other effects stay untouched.",
            mutates: true,
            schema: || obj(json!({
                "track": {"type":"string"}, "source": {"type":"string"},
                "depth_db": {"type":"number", "minimum":0, "maximum":24, "default":6},
                "release_beats": {"type":"number", "minimum":0.0625, "maximum":4, "default":0.5},
                "attack_ms": {"type":"number", "minimum":0.1, "maximum":30, "default":3}
            }), &["track", "source"]),
            run: |e,a| {
                let track=s_req(a,"track")?; let source=s_req(a,"source")?;
                let si=e.project.track_index(&source).context("ducking source must be a track")?;
                let source=e.project.tracks[si].name.clone();
                if track.eq_ignore_ascii_case(&source) { bail!("ducking source and target must differ"); }
                if track.eq_ignore_ascii_case("master") { bail!("choose a track or bus target, not master"); }
                let read=|k:&str,d:f32,lo:f32,hi:f32| -> Result<f32> {
                    let v=match a.get(k) {Some(v)=>v.as_f64().ok_or_else(||anyhow!("{k} must be a number"))? as f32,None=>d};
                    if !v.is_finite() || v<lo || v>hi {bail!("{k} must be {lo}..{hi}");} Ok(v)
                };
                let depth=read("depth_db",6.0,0.0,24.0)?;
                let beats=read("release_beats",0.5,0.0625,4.0)?;
                let attack=read("attack_ms",3.0,0.1,30.0)?;
                let bpm=e.project.bpm;
                if !bpm.is_finite() || bpm<=0.0 {bail!("project tempo must be positive and finite");}
                let release=beats*60000.0/bpm;
                let dst=effects_of(&mut e.project,&track)?;
                let idx=dst.iter().position(|fx|matches!(fx,Effect::Sidechain(p) if p.source.eq_ignore_ascii_case(&source)));
                let retuned=idx.is_some();
                let mut p=crate::fx::SidechainFx {source:source.clone(),amount:1.0-crate::dsp::db_to_gain(-depth),release_ms:release,attack_ms:attack,..Default::default()};
                if let Some(i)=idx {p.id=dst[i].id().to_string();dst[i]=Effect::Sidechain(p);}else{dst.push(Effect::Sidechain(p));}
                e.project.ensure_fx_ids();
                Ok(json!({"track":track,"source":source,"depth_db":depth,"release_beats":beats,"release_ms":release,"bpm_at_apply":bpm,"retuned":retuned,"detector":"note triggers"}))
            }
        },
        Tool {
            name: "soundgoodizer",
            description: "One-knob loudness and polish (FL Soundgoodizer) on a track, bus or 'master': a 3-band compressor tuned by mode, blended in by amount, then a -1 dB ceiling. mode A (default) = even lift, B = forward mids (vocals, leads), C = brighter top (air, hats), D = heavy low end and loud. amount 0-100 (default 50). Calling it again on the same track retunes the existing one instead of stacking.",
            mutates: true,
            schema: || obj(json!({
                "track": {"type": "string", "description": "Track, bus or 'master'"},
                "mode": {"type": "string", "enum": ["A", "B", "C", "D"]},
                "amount": {"type": "number", "description": "0-100, default 50"}
            }), &["track"]),
            run: |e, a| {
                let track = s_req(a, "track")?;
                let mode = s_opt(a, "mode").unwrap_or_else(|| "A".into()).to_uppercase();
                let amount = f_or(a, "amount", 50.0);
                if !(0.0..=100.0).contains(&amount) {
                    bail!("amount is 0-100");
                }
                let fx = goodizer_chain(&mode, amount / 100.0)?;
                let chain = parse_chain(&fx, "soundgoodizer")?;
                let dst = effects_of(&mut e.project, &track)?;
                // retune: drop the previous soundgoodizer pair (tagged multiband + limiter right after it)
                let mut retuned = false;
                if let Some(i) = dst.iter().position(|f| serde_json::to_value(f).map(|v| v["low_freq"] == json!(GOOD_TAG)).unwrap_or(false)) {
                    let n = if i + 1 < dst.len() && serde_json::to_value(&dst[i + 1]).map(|v| v["type"] == "limiter").unwrap_or(false) { 2 } else { 1 };
                    dst.drain(i..i + n);
                    for (k, f) in chain.into_iter().enumerate() {
                        dst.insert(i + k, f);
                    }
                    retuned = true;
                } else {
                    dst.extend(chain);
                }
                let now = types(dst);
                e.project.ensure_fx_ids();
                Ok(json!({"track": track, "mode": mode, "amount": amount, "retuned": retuned, "chain": now}))
            },
        },
        Tool {
            name: "save_fx_chain",
            description: "Save the effect chain of a track, bus or 'master' under a name in the project (FL Patcher-style preset), to apply_fx_chain elsewhere. Overwrites a saved chain with the same name.",
            mutates: true,
            schema: || obj(json!({
                "track": {"type": "string", "description": "Track, bus or 'master' to save from"},
                "name": {"type": "string", "description": "Chain name"}
            }), &["track", "name"]),
            run: |e, a| {
                let track = s_req(a, "track")?;
                let name = s_req(a, "name")?;
                if name.trim().is_empty() {
                    bail!("save_fx_chain: empty name");
                }
                if builtins().iter().any(|b| b.0.eq_ignore_ascii_case(&name)) {
                    bail!("save_fx_chain: '{name}' is a built-in chain; pick another name");
                }
                let mut c = effects_of(&mut e.project, &track)?.clone();
                for fx in c.iter_mut() {
                    fx.set_id("");
                }
                let t = types(&c);
                e.project.fx_chains.insert(name.clone(), c);
                Ok(json!({"saved": name, "from": track, "chain": t}))
            },
        },
        Tool {
            name: "copy_fx_chain",
            description: "Copy the whole effect chain from one track/bus/'master' to another (replace=true clears the destination first, default append).",
            mutates: true,
            schema: || obj(json!({
                "from": {"type": "string"},
                "to": {"type": "string"},
                "replace": {"type": "boolean"}
            }), &["from", "to"]),
            run: |e, a| {
                let from = s_req(a, "from")?;
                let to = s_req(a, "to")?;
                let mut c = effects_of(&mut e.project, &from)?.clone();
                for fx in c.iter_mut() {
                    fx.set_id("");
                }
                let n = c.len();
                let replace = b_or(a, "replace", false);
                let dst = effects_of(&mut e.project, &to)?;
                if replace {
                    dst.clear();
                }
                dst.extend(c);
                let now = types(dst);
                e.project.ensure_fx_ids();
                Ok(json!({"from": from, "to": to, "copied": n, "chain": now}))
            },
        },
    ]
}

/// Crossover the soundgoodizer's compressor uses; also how a re-call finds it.
const GOOD_TAG: f32 = 180.0;
/// Band level the makeup is calibrated to.
const NOMINAL_DB: f32 = -14.0;

/// Mode presets: (low thr, ratio, gain), (mid ...), (high ...); amount 0..1
/// blends the compressor in and scales the band lifts.
fn goodizer_chain(mode: &str, k: f32) -> Result<Value> {
    let p: [(f32, f32, f32); 3] = match mode {
        "A" => [(-20.0, 2.5, 2.0), (-20.0, 2.5, 2.0), (-22.0, 2.5, 2.5)],
        "B" => [(-20.0, 2.5, 1.0), (-24.0, 3.5, 3.5), (-22.0, 2.5, 2.0)],
        "C" => [(-20.0, 2.0, 1.0), (-20.0, 2.5, 1.5), (-26.0, 3.0, 4.5)],
        "D" => [(-26.0, 4.0, 4.0), (-24.0, 3.0, 3.0), (-24.0, 3.0, 3.0)],
        m => bail!("mode '{m}': use A, B, C or D"),
    };
    let k = k.clamp(0.0, 1.0);
    // amount scales the compression itself (threshold toward 0 dB, ratio
    // toward 1:1) rather than a dry/wet blend: the crossover's phase shift
    // would comb against the dry signal. Makeup gives back what the
    // compressor takes from a band at a typical mix level (-14 dBFS) plus
    // the mode's lift, so the result is denser AND louder, as in FL.
    let eff = |b: (f32, f32, f32)| {
        let (t, r) = (b.0 * k, 1.0 + (b.1 - 1.0) * k);
        (t, r, (NOMINAL_DB - t).max(0.0) * (1.0 - 1.0 / r) + b.2 * k)
    };
    let (l, m, h) = (eff(p[0]), eff(p[1]), eff(p[2]));
    Ok(json!([
        {"type": "multiband", "low_freq": GOOD_TAG, "high_freq": 4000.0,
         "low_threshold_db": l.0, "low_ratio": l.1, "low_gain_db": l.2,
         "mid_threshold_db": m.0, "mid_ratio": m.1, "mid_gain_db": m.2,
         "high_threshold_db": h.0, "high_ratio": h.1, "high_gain_db": h.2,
         "attack_ms": 10.0, "release_ms": 120.0, "mix": 1.0},
        {"type": "limiter", "ceiling_db": -1.0}
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soundgoodizer_adds_then_retunes() {
        let mut e = Engine::new(std::env::temp_dir().join("beatbox_chain_tests"));
        e.call("add_track", &json!({"name": "keys"})).unwrap();
        let r = e
            .call(
                "soundgoodizer",
                &json!({"track": "keys", "mode": "b", "amount": 70}),
            )
            .unwrap();
        assert_eq!(r["chain"], json!(["multiband", "limiter"]), "{r}");
        assert_eq!(r["retuned"], false);
        let r = e
            .call(
                "soundgoodizer",
                &json!({"track": "keys", "mode": "D", "amount": 30}),
            )
            .unwrap();
        assert_eq!(r["chain"], json!(["multiband", "limiter"]), "{r}");
        assert_eq!(r["retuned"], true);
        let fx = serde_json::to_value(
            &e.project
                .tracks
                .iter()
                .find(|t| t.name == "keys")
                .unwrap()
                .effects[0],
        )
        .unwrap();
        // D at 30%: threshold -26*0.3, ratio 1+3*0.3
        assert!(
            (fx["low_threshold_db"].as_f64().unwrap() + 7.8).abs() < 1e-3,
            "{fx}"
        );
        assert!(
            (fx["low_ratio"].as_f64().unwrap() - 1.9).abs() < 1e-3,
            "{fx}"
        );
        assert!(e
            .call("soundgoodizer", &json!({"track": "keys", "mode": "Z"}))
            .is_err());
        assert!(e
            .call("soundgoodizer", &json!({"track": "keys", "amount": 150}))
            .is_err());
        e.call("soundgoodizer", &json!({"track": "master"}))
            .unwrap();
    }

    #[test]
    fn builtin_chains_parse_and_apply() {
        for (n, _, v) in builtins() {
            parse_chain(&v, n).unwrap_or_else(|err| panic!("{n}: {err:#}"));
        }
        let mut e = Engine::new(std::env::temp_dir().join("bb_chain_test"));
        e.call("add_track", &json!({"name": "keys", "preset": "piano"}))
            .unwrap();
        e.call("add_track", &json!({"name": "pad", "preset": "piano"}))
            .unwrap();
        let r = e
            .call(
                "apply_fx_chain",
                &json!({"track": "keys", "chain": "lofi_keys"}),
            )
            .unwrap();
        assert_eq!(r["added"], 3);
        e.call("save_fx_chain", &json!({"track": "keys", "name": "mine"}))
            .unwrap();
        let r = e
            .call(
                "apply_fx_chain",
                &json!({"track": "pad", "chain": "mine", "replace": true}),
            )
            .unwrap();
        assert_eq!(r["chain"].as_array().unwrap().len(), 3);
        let r = e
            .call(
                "apply_fx_chain",
                &json!({"track": "pad", "effects": [{"type": "reverb", "mix": 0.2}]}),
            )
            .unwrap();
        assert_eq!(r["chain"].as_array().unwrap().len(), 4);
        e.call(
            "copy_fx_chain",
            &json!({"from": "pad", "to": "keys", "replace": true}),
        )
        .unwrap();
        assert_eq!(e.project.tracks[0].effects.len(), 4);
        assert!(e
            .call("apply_fx_chain", &json!({"track": "pad", "chain": "nope"}))
            .is_err());
        assert!(e
            .call(
                "apply_fx_chain",
                &json!({"track": "pad", "effects": [{"type": "vinyl", "age": 3.0}]})
            )
            .is_err());
        assert!(e
            .call(
                "save_fx_chain",
                &json!({"track": "pad", "name": "lofi_keys"})
            )
            .is_err());
    }
}

#[cfg(test)]
mod production_preset_tests {
    use super::*;
    #[test]
    fn production_chains_routed_undo_and_roundtrip() {
        let dir = std::env::temp_dir().join("beatbox-production-presets-test");
        let mut e = Engine::new(dir);
        e.call("add_track", &json!({"name":"drums","preset":"kick"}))
            .unwrap();
        e.call(
            "add_effect",
            &json!({"track":"drums","type":"gain","params":{"db":-3}}),
        )
        .unwrap();
        for name in ["parallel_drum_punch", "pluck_space"] {
            let before = serde_json::to_value(&e.project).unwrap();
            let r=e.call("effects",&json!({"action":"apply_fx_chain","params":{"track":"drums","chain":name,"replace":true}})).unwrap();
            assert_eq!(r["added"], 4);
            let after = serde_json::to_value(&e.project).unwrap();
            assert_eq!(e.project.tracks[0].effects.len(), 4);
            assert!(e.project.tracks[0]
                .effects
                .iter()
                .all(|f| f.validate().is_ok() && !f.id().is_empty()));
            e.call("undo", &json!({})).unwrap();
            assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
            e.call("redo", &json!({})).unwrap();
            assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
            e.call("save_project", &json!({"path":"presets.json"}))
                .unwrap();
            e.call("load_project", &json!({"path":"presets.json"}))
                .unwrap();
            assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        }
        let before = serde_json::to_value(&e.project).unwrap();
        assert!(e
            .call(
                "apply_fx_chain",
                &json!({"track":"drums","chain":"not-a-chain","replace":true})
            )
            .is_err());
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        e.call(
            "apply_fx_chain",
            &json!({"track":"drums","chain":"parallel_drum_punch"}),
        )
        .unwrap();
        assert_eq!(e.project.tracks[0].effects.len(), 8);
    }
}

#[cfg(test)]
mod duck_tests {
    use super::*;
    #[test]
    fn duck_setup_retunes_preserves_history_and_rejects_bad_input() {
        let mut e = Engine::new(std::env::temp_dir().join("beatbox-duck-regression"));
        e.call("add_track", &json!({"name":"kick","preset":"kick"}))
            .unwrap();
        e.call("add_track", &json!({"name":"bass","preset":"808"}))
            .unwrap();
        e.call("set_tempo", &json!({"bpm":120})).unwrap();
        e.call(
            "add_effect",
            &json!({"track":"bass","type":"gain","params":{"db":-2}}),
        )
        .unwrap();
        let before = serde_json::to_value(&e.project).unwrap();
        let args =
            json!({"track":"bass","source":"kick","depth_db":6,"release_beats":0.5,"attack_ms":3});
        let out = e
            .call("effects", &json!({"action":"duck_to_track","params":args}))
            .unwrap();
        assert_eq!(out["release_ms"], 250.0);
        assert_eq!(out["retuned"], false);
        let after = serde_json::to_value(&e.project).unwrap();
        let idx = e.project.track_index("bass").unwrap();
        let fx = after["tracks"][idx]["effects"][1].clone();
        assert!((fx["amount"].as_f64().unwrap() - (1.0 - 10f64.powf(-6.0 / 20.0))).abs() < 1e-6);
        assert_eq!(
            after["tracks"][idx]["effects"][0],
            before["tracks"][idx]["effects"][0]
        );
        let h = e.history().0.len();
        e.call("duck_to_track", &args).unwrap();
        assert_eq!(e.history().0.len(), h);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        let id = e.project.tracks[idx].effects[1].id().to_string();
        e.call(
            "bypass_effect",
            &json!({"track":"bass","index":1,"on":true}),
        )
        .unwrap();
        let mut changed = args.clone();
        changed["depth_db"] = json!(12);
        e.call("duck_to_track", &changed).unwrap();
        assert_eq!(e.project.tracks[idx].effects.len(), 2);
        assert_eq!(e.project.tracks[idx].effects[1].id(), id);
        assert!(!e.project.tracks[idx].effects[1].bypassed());
        e.call("save_project", &json!({"path":"duck.json"}))
            .unwrap();
        let saved = serde_json::to_value(&e.project).unwrap();
        e.call("load_project", &json!({"path":"duck.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), saved);
        for bad in [
            json!({"track":"bass","source":"missing"}),
            json!({"track":"kick","source":"kick"}),
            json!({"track":"master","source":"kick"}),
            json!({"track":"bass","source":"kick","depth_db":25}),
            json!({"track":"bass","source":"kick","release_beats":"0.5"}),
            json!({"track":"bass","source":"kick","attack_ms":0}),
        ] {
            assert!(e.call("duck_to_track", &bad).is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), saved);
        }
    }
}

#[cfg(test)]
mod translation_tests {
    use super::*;
    #[test]
    fn translation_preserves_stereo_fold_cancellation_and_project() {
        let dir = std::env::temp_dir().join(format!("translation-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("stereo.wav");
        let n = 44100;
        let l: Vec<_> = (0..n)
            .map(|i| (i as f32 * std::f32::consts::TAU * 1000. / 44100.).sin() * 0.25)
            .collect();
        let r: Vec<_> = l.iter().map(|x| -x).collect();
        crate::render::write_wav(&p, &l, &r).unwrap();
        let mut e = Engine::new(dir.clone());
        let before = serde_json::to_value(&e.project).unwrap();
        let h = e.history().0.len();
        let a=e.call("effects",&json!({"action":"preview_mix_translation","params":{"mode":"stereo","path":p,"play":true}})).unwrap();
        let q = e
            .transport
            .pending_audition
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap();
        assert!(q.0.iter().zip(&q.1).all(|(a, b)| (a + b).abs() < 1e-4));
        assert!(q.0.iter().any(|x| x.abs() > 0.2));
        let b = e
            .call(
                "preview_mix_translation",
                &json!({"mode":"mono","path":p,"play":true}),
            )
            .unwrap();
        let q = e
            .transport
            .pending_audition
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap();
        assert!(q.0.iter().chain(&q.1).all(|x| x.abs() < 1e-4));
        assert_ne!(a["path"], b["path"]);
        for mode in ["stereo", "mono", "phone"] {
            e.call(
                "preview_mix_translation",
                &json!({"mode":mode,"path":p,"play":false}),
            )
            .unwrap();
            assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
            assert_eq!(e.history().0.len(), h);
        }
        for bad in [
            json!({"mode":"wat"}),
            json!({"bars":0}),
            json!({"bars":1.5}),
            json!({"bars":9}),
        ] {
            assert!(e.call("preview_mix_translation", &bad).is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        }
        // Centred 40Hz loses energy in phone mode without gain compensation.
        let x: Vec<_> = (0..n)
            .map(|i| (i as f32 * std::f32::consts::TAU * 40. / 44100.).sin() * 0.3)
            .collect();
        crate::render::write_wav(&p, &x, &x).unwrap();
        e.call(
            "preview_mix_translation",
            &json!({"mode":"phone","path":p,"play":true}),
        )
        .unwrap();
        let q = e
            .transport
            .pending_audition
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap();
        let rms = |x: &[f32]| (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt();
        assert!(rms(&q.0) < rms(&x) * 0.1);
        assert_eq!(q.0, q.1);
        e.call(
            "preview_mix_translation",
            &json!({"mode":"mono","bars":1,"play":false}),
        )
        .unwrap();
    }
}

#[cfg(test)]
mod safe_width_tests {
    use crate as beatbox;
    use serde_json::json;
    #[test]
    fn width_retune_history_validation_and_low_side() {
        let mut e = beatbox::Engine::new("/tmp/width-regression".into());
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        e.call(
            "add_effect",
            &json!({"track":"keys","type":"gain","params":{"db":-3}}),
        )
        .unwrap();
        let before = serde_json::to_value(&e.project).unwrap();
        let args = json!({"track":"keys","width":1.0,"mono_below_hz":180});
        e.call(
            "effects",
            &json!({"action":"set_bass_safe_width","params":args}),
        )
        .unwrap();
        let after = serde_json::to_value(&e.project).unwrap();
        let h = e.history().0.len();
        e.call("set_bass_safe_width", &args).unwrap();
        assert_eq!(e.history().0.len(), h);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        let id = e.project.tracks[0].effects[1].id().to_string();
        e.call(
            "set_bass_safe_width",
            &json!({"track":"keys","width":1.5,"mono_below_hz":200}),
        )
        .unwrap();
        assert_eq!(e.project.tracks[0].effects.len(), 2);
        assert_eq!(e.project.tracks[0].effects[1].id(), id);
        e.call("save_project", &json!({"path":"width.json"}))
            .unwrap();
        let saved = serde_json::to_value(&e.project).unwrap();
        e.call("load_project", &json!({"path":"width.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), saved);
        for bad in [
            json!({"track":"keys","width":2.1}),
            json!({"track":"keys","mono_below_hz":20}),
            json!({"track":"keys","width":"1"}),
        ] {
            assert!(e.call("set_bass_safe_width", &bad).is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), saved);
        }
        // Measured side attenuation, identical mono sum, no invented recovered bass.
        let n = 44100;
        let mut l = vec![0.; n];
        let mut r = vec![0.; n];
        for i in 0..n {
            let t = i as f32 / 44100.;
            let m = (std::f32::consts::TAU * 220. * t).sin() * 0.1;
            let s = (std::f32::consts::TAU * 40. * t).sin() * 0.2;
            l[i] = m + s;
            r[i] = m - s;
        }
        let dry = l.clone();
        let dr = r.clone();
        let fx: beatbox::fx::Effect =
            serde_json::from_value(json!({"type":"width","amount":1,"low_mono_hz":180})).unwrap();
        let map = std::collections::HashMap::new();
        fx.process(
            &mut l,
            &mut r,
            &beatbox::fx::FxContext {
                step_secs: 0.125,
                triggers: &map,
                levels: None,
                offset: 0,
            },
        );
        let mut a = 0.;
        let mut b = 0.;
        for i in 4410..n {
            a += (dry[i] - dr[i]).powi(2);
            b += (l[i] - r[i]).powi(2);
            assert!(((l[i] + r[i]) - (dry[i] + dr[i])).abs() < 1e-6);
        }
        assert!(b < a * 0.02, "side ratio {}", b / a);
    }
}

#[cfg(test)]
mod musical_delay_tests {
    use crate as beatbox;
    use serde_json::json;
    #[test]
    fn musical_delay_retunes_history_and_timing() {
        let mut e = beatbox::Engine::new("/tmp/musical-delay-test".into());
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        e.call(
            "add_effect",
            &json!({"track":"keys","type":"gain","params":{"db":-2}}),
        )
        .unwrap();
        let before = serde_json::to_value(&e.project).unwrap();
        let args = json!({"track":"keys","division":"dotted_eighth","mix":0.2,"feedback":0.25,"tone_hz":4500,"ping_pong":true});
        e.call(
            "effects",
            &json!({"action":"set_musical_delay","params":args}),
        )
        .unwrap();
        let after = serde_json::to_value(&e.project).unwrap();
        let h = e.history().0.len();
        e.call("set_musical_delay", &args).unwrap();
        assert_eq!(e.history().0.len(), h);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        let id = e.project.tracks[0].effects[1].id().to_string();
        for (division, steps) in [
            ("eighth", 2.),
            ("dotted_eighth", 3.),
            ("quarter", 4.),
            ("eighth_triplet", 4. / 3.),
        ] {
            e.call(
                "set_musical_delay",
                &json!({"track":"keys","division":division}),
            )
            .unwrap();
            assert_eq!(e.project.tracks[0].effects.len(), 2);
            assert_eq!(e.project.tracks[0].effects[1].id(), id);
            for bpm in [120., 90.] {
                let mut l = vec![0.; 88200];
                let mut r = l.clone();
                l[0] = 0.5;
                r[0] = 0.5;
                let map = std::collections::HashMap::new();
                e.project.tracks[0].effects[1].process(
                    &mut l,
                    &mut r,
                    &beatbox::fx::FxContext {
                        step_secs: 60. / bpm / 4.,
                        triggers: &map,
                        levels: None,
                        offset: 0,
                    },
                );
                let expected = (steps * 60. / bpm / 4. * 44100.) as usize;
                let first = (1..l.len()).find(|&i| l[i].abs() > 1e-7).unwrap();
                assert_eq!(first, expected);
                assert!(r[expected].abs() < 1e-7);
            }
        }
        e.call("save_project", &json!({"path":"delay.json"}))
            .unwrap();
        let saved = serde_json::to_value(&e.project).unwrap();
        e.call("load_project", &json!({"path":"delay.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), saved);
        for bad in [
            json!({"track":"keys","division":"half"}),
            json!({"track":"keys","feedback":0.8}),
            json!({"track":"keys","mix":"0.2"}),
            json!({"track":"keys","ping_pong":"true"}),
        ] {
            assert!(e.call("set_musical_delay", &bad).is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), saved);
        }
    }
}

#[cfg(test)]
mod punch_tests {
    use crate as beatbox;
    use serde_json::json;
    #[test]
    fn punch_history_and_audio() {
        let mut e = beatbox::Engine::new("/tmp/punch-test-work".into());
        e.call("add_track", &json!({"name":"kick","preset":"kick"}))
            .unwrap();
        let before = serde_json::to_value(&e.project).unwrap();
        let a = json!({"track":"kick","attack":0.5,"sustain":-0.3});
        e.call("effects", &json!({"action":"set_drum_punch","params":a}))
            .unwrap();
        let after = serde_json::to_value(&e.project).unwrap();
        let h = e.history().0.len();
        e.call("set_drum_punch", &a).unwrap();
        assert_eq!(h, e.history().0.len());
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        let id = e.project.tracks[0].effects[0].id().to_string();
        e.call("set_drum_punch", &json!({"track":"kick","attack":0.25}))
            .unwrap();
        assert_eq!(e.project.tracks[0].effects.len(), 1);
        assert_eq!(e.project.tracks[0].effects[0].id(), id);
        let good = serde_json::to_value(&e.project).unwrap();
        for a in [
            json!({"track":"kick","attack":1.1}),
            json!({"track":"kick","sustain":"0"}),
        ] {
            assert!(e.call("set_drum_punch", &a).is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), good);
        }
        let mut l = vec![0.2; 44100];
        let mut r = l.clone();
        let map = std::collections::HashMap::new();
        e.project.tracks[0].effects[0].process(
            &mut l,
            &mut r,
            &beatbox::fx::FxContext {
                step_secs: 0.125,
                triggers: &map,
                levels: None,
                offset: 0,
            },
        );
        assert!(l[0] > 0.2);
        assert_eq!(l, r);
        assert!(l.iter().all(|x| x.is_finite()));
        e.call("save_project", &json!({"path":"punch.json"}))
            .unwrap();
        e.call("load_project", &json!({"path":"punch.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), good);
    }
}

#[cfg(test)]
mod colour_tests {
    use crate as beatbox;
    use serde_json::json;
    #[test]
    fn colour_history_and_audio() {
        let mut e = beatbox::Engine::new("/tmp/colour-test-work".into());
        e.call("add_track", &json!({"name":"kick","preset":"kick"}))
            .unwrap();
        let before = serde_json::to_value(&e.project).unwrap();
        let a = json!({"track":"kick","mode":"tube","drive_db":9,"mix":0.4,"output_db":-3});
        e.call("effects", &json!({"action":"set_track_colour","params":a}))
            .unwrap();
        let after = serde_json::to_value(&e.project).unwrap();
        let h = e.history().0.len();
        e.call("set_track_colour", &a).unwrap();
        assert_eq!(h, e.history().0.len());
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        let id = e.project.tracks[0].effects[0].id().to_string();
        e.call("set_track_colour", &json!({"track":"kick","mode":"tape"}))
            .unwrap();
        assert_eq!(e.project.tracks[0].effects.len(), 1);
        assert_eq!(e.project.tracks[0].effects[0].id(), id);
        let good = serde_json::to_value(&e.project).unwrap();
        for a in [
            json!({"track":"kick","drive_db":25}),
            json!({"track":"kick","mix":"0"}),
        ] {
            assert!(e.call("set_track_colour", &a).is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), good);
        }
        let mut l = vec![0.2; 44100];
        let mut r = l.clone();
        let map = std::collections::HashMap::new();
        e.project.tracks[0].effects[0].process(
            &mut l,
            &mut r,
            &beatbox::fx::FxContext {
                step_secs: 0.125,
                triggers: &map,
                levels: None,
                offset: 0,
            },
        );
        assert!(l.iter().any(|v| (*v - 0.2).abs() > 0.01));
        assert_eq!(l, r);
        assert!(l.iter().all(|x| x.is_finite()));
        e.call("save_project", &json!({"path":"colour.json"}))
            .unwrap();
        e.call("load_project", &json!({"path":"colour.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), good);
    }
}

#[cfg(test)]
mod glue_tests {
    use crate as beatbox;
    use beatbox::fx::{Effect, FxContext};
    use serde_json::json;
    fn process(fx: &Effect, x: &[f32]) -> Vec<f32> {
        let (mut l, mut r) = (x.to_vec(), x.to_vec());
        let map = std::collections::HashMap::new();
        fx.process(
            &mut l,
            &mut r,
            &FxContext {
                step_secs: 0.125,
                triggers: &map,
                levels: None,
                offset: 0,
            },
        );
        assert_eq!(l, r);
        assert!(l.iter().all(|x| x.is_finite()));
        l
    }
    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|x| x * x).sum::<f32>() / x.len() as f32).sqrt()
    }
    #[test]
    fn glue_history_and_audio() {
        let mut e = beatbox::Engine::new("/tmp/glue-test-work".into());
        e.call("add_track", &json!({"name":"drums","preset":"kick"}))
            .unwrap();
        let before = serde_json::to_value(&e.project).unwrap();
        let a = json!({"track":"drums","threshold_db":-24,"ratio":4,"attack_ms":5,"release_ms":100,"mix":1,"sc_hpf_hz":0});
        e.call("effects", &json!({"action":"set_bus_glue","params":a}))
            .unwrap();
        let after = serde_json::to_value(&e.project).unwrap();
        let h = e.history().0.len();
        e.call("set_bus_glue", &a).unwrap();
        assert_eq!(h, e.history().0.len());
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        let id = e.project.tracks[0].effects[0].id().to_string();
        let dry: Vec<_> = (0..44100)
            .map(|i| (std::f32::consts::TAU * 40. * i as f32 / 44100.).sin() * 0.6)
            .collect();
        let wet = process(&e.project.tracks[0].effects[0], &dry);
        assert!(rms(&wet[22050..]) < rms(&dry[22050..]) * 0.7);
        e.call("set_bus_glue",&json!({"track":"drums","threshold_db":-24,"ratio":4,"attack_ms":5,"release_ms":100,"mix":1,"sc_hpf_hz":300})).unwrap();
        let filtered = process(&e.project.tracks[0].effects[0], &dry);
        assert!(rms(&filtered[22050..]) > rms(&wet[22050..]) * 1.5);
        e.call("set_bus_glue", &json!({"track":"drums","mix":0}))
            .unwrap();
        assert_eq!(process(&e.project.tracks[0].effects[0], &dry), dry);
        assert_eq!(e.project.tracks[0].effects.len(), 1);
        assert_eq!(e.project.tracks[0].effects[0].id(), id);
        let good = serde_json::to_value(&e.project).unwrap();
        for a in [
            json!({"track":"drums","ratio":9}),
            json!({"track":"drums","mix":"0"}),
            json!({"track":"drums","sc_hpf_hz":301}),
        ] {
            assert!(e.call("set_bus_glue", &a).is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), good);
        }
        e.call("save_project", &json!({"path":"glue.json"}))
            .unwrap();
        e.call("load_project", &json!({"path":"glue.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), good);
    }
}

#[cfg(test)]
mod drum_map_tests {
    use crate as beatbox;
    use serde_json::json;
    #[test]
    fn map_pitch_only_history_and_guards() {
        let mut e = beatbox::Engine::new("/tmp/drum-map-test".into());
        e.call("add_track", &json!({"name":"kick","preset":"kick"}))
            .unwrap();
        e.call("add_notes",&json!({"track":"kick","notes":[{"start":0,"pitch":60,"len":2,"vel":0.8,"offset":0.1,"prob":0.6},{"start":4.5,"pitch":60,"len":1,"vel":0.4}]})).unwrap();
        let b = serde_json::to_value(&e.project).unwrap();
        e.call(
            "pattern",
            &json!({"action":"map_drum_notes","params":{"track":"kick","key":36}}),
        )
        .unwrap();
        let a = serde_json::to_value(&e.project).unwrap();
        let mut ex = b.clone();
        for n in ex["patterns"][0]["clips"]["kick"].as_array_mut().unwrap() {
            n["pitch"] = json!(36);
        }
        assert_eq!(ex, a);
        let h = e.history().0.len();
        e.call("map_drum_notes", &json!({"track":"kick","key":36}))
            .unwrap();
        assert_eq!(h, e.history().0.len());
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), a);
        for args in [
            json!({"track":"kick","key":36.5}),
            json!({"track":"kick","key":60}),
        ] {
            assert!(e.call("map_drum_notes", &args).is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), a);
        }
        e.project.patterns[0].notes_mut("kick")[0].pitch = 38;
        let mixed = serde_json::to_value(&e.project).unwrap();
        assert!(e
            .call("map_drum_notes", &json!({"track":"kick","key":36}))
            .is_err());
        assert_eq!(serde_json::to_value(&e.project).unwrap(), mixed);
        e.project.patterns[0].notes_mut("kick")[0].pitch = 36;
        e.project.patterns[0].notes_mut("kick")[0].slide_to = Some(40);
        assert!(e
            .call("map_drum_notes", &json!({"track":"kick","key":36}))
            .is_err());
    }
}

#[cfg(test)]
mod sample_perf_tests {
    use crate as beatbox;
    use serde_json::json;
    #[test]
    fn performance_history_preservation_and_audio() {
        let mut e = beatbox::Engine::new("/tmp/perf-test-work".into());
        e.call(
            "load_instrument",
            &json!({"track":"piano","catalog_id":"vcsl_upright_piano_yamaha_lite","max_mb":100}),
        )
        .unwrap();
        let b = serde_json::to_value(&e.project).unwrap();
        let a = json!({"track":"piano","attack_ms":5,"release_ms":80,"velocity_amount":0.8,"one_shot":false});
        e.call(
            "instrument",
            &json!({"action":"set_sample_performance","params":a}),
        )
        .unwrap();
        let after = serde_json::to_value(&e.project).unwrap();
        let mut expected = b.clone();
        let p = &mut expected["tracks"][0]["instrument"];
        p["attack"] = after["tracks"][0]["instrument"]["attack"].clone();
        p["release"] = after["tracks"][0]["instrument"]["release"].clone();
        p["vel_to_gain"] = json!(0.8f32);
        assert_eq!(after, expected);
        let h = e.history().0.len();
        e.call("set_sample_performance", &a).unwrap();
        assert_eq!(h, e.history().0.len());
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        for bad in [
            json!({"track":"piano","release_ms":3001}),
            json!({"track":"piano","one_shot":1}),
            json!({"track":"piano","attack_ms":"5"}),
        ] {
            assert!(e.call("set_sample_performance", &bad).is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        }
        e.bank.sync(&e.project.samples);
        if let beatbox::instruments::Instrument::Multisample(p) = &e.project.tracks[0].instrument {
            let short = beatbox::multisample::render_multisample(p, 60., 0.7, 0.2, &e.bank, 0);
            let mut long = p.clone();
            long.release = 1.2;
            let longer = beatbox::multisample::render_multisample(&long, 60., 0.7, 0.2, &e.bank, 0);
            assert!(longer.len() > short.len());
            assert!(longer.iter().all(|x| x.is_finite()));
            long.one_shot = true;
            let s1 = beatbox::multisample::render_multisample(&long, 60., 0.7, 0.1, &e.bank, 0);
            long.release = 0.01;
            let s2 = beatbox::multisample::render_multisample(&long, 60., 0.7, 1., &e.bank, 0);
            assert_eq!(s1, s2);
        } else {
            panic!()
        }
    }
}

#[cfg(test)]
mod takes_tests {
    use crate as beatbox;
    use serde_json::json;
    #[test]
    fn takes_and_blend_history_and_actual_render() {
        let mut e = beatbox::Engine::new("/tmp/takes-test-work".into());
        e.project.bpm = 120.;
        e.project.swing = 0.;
        e.project.patterns[0].bars = 4;
        e.project.tracks.clear();
        e.project.master_effects.clear();
        e.call(
            "load_instrument",
            &json!({"track":"kit","catalog_id":"vcsl_acoustic_drum_kit","max_mb":100}),
        )
        .unwrap();
        e.call("add_notes",&json!({"track":"kit","notes":[{"start":0,"pitch":38,"len":1,"vel":0.8},{"start":24,"pitch":38,"len":1,"vel":0.8}]})).unwrap();
        let b = serde_json::to_value(&e.project).unwrap();
        e.call(
            "set_sample_performance",
            &json!({"track":"kit","round_robin":false,"velocity_blend":false}),
        )
        .unwrap();
        let after = serde_json::to_value(&e.project).unwrap();
        let mut ex = b.clone();
        ex["tracks"][0]["instrument"]["round_robin"] = json!(false);
        ex["tracks"][0]["instrument"]["vel_crossfade"] = json!(false);
        assert_eq!(after, ex);
        let h = e.history().0.len();
        e.call(
            "set_sample_performance",
            &json!({"track":"kit","round_robin":false,"velocity_blend":false}),
        )
        .unwrap();
        assert_eq!(e.history().0.len(), h);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        assert!(e
            .call(
                "set_sample_performance",
                &json!({"track":"kit","round_robin":1})
            )
            .is_err());
        let off = e.mix().unwrap();
        e.call(
            "set_sample_performance",
            &json!({"track":"kit","round_robin":true}),
        )
        .unwrap();
        let on = e.mix().unwrap();
        let offset = (e.project.step_secs() * 24. * 44100.).round() as usize;
        let a = &off.left[..5000];
        let b = &off.left[offset..offset + 5000];
        assert!(
            a.iter().zip(b).all(|(a, b)| (a - b).abs() < 1e-5),
            "off repeats differ"
        );
        let a = &on.left[..5000];
        let b = &on.left[offset..offset + 5000];
        assert!(
            a.iter().zip(b).any(|(a, b)| (a - b).abs() > 0.001),
            "song cache froze round-robin"
        );
        if let beatbox::instruments::Instrument::Multisample(p) = &e.project.tracks[0].instrument {
            let mut p = p.clone();
            p.vel_crossfade = true;
            let mut found = false;
            for v in (1..100).map(|v| v as f32 / 100.) {
                if beatbox::multisample::pick_zones(&p, 38, v, 0).len() > 1 {
                    found = true;
                    break;
                }
            }
            assert!(found);
            p.vel_crossfade = false;
            for v in (1..100).map(|v| v as f32 / 100.) {
                assert_eq!(beatbox::multisample::pick_zones(&p, 38, v, 0).len(), 1);
            }
        }
    }
}

#[cfg(test)]
mod short_room_tests {
    use crate as beatbox;
    use serde_json::json;
    #[test]
    fn short_room_chain_history_roundtrip_audio() {
        let mut e = beatbox::Engine::new("/tmp/room-test-work".into());
        e.call("add_track", &json!({"name":"drums","preset":"kick"}))
            .unwrap();
        let b = serde_json::to_value(&e.project).unwrap();
        let a = json!({"track":"drums","chain":"short_drum_room","replace":true});
        let r = e
            .call("effects", &json!({"action":"apply_fx_chain","params":a}))
            .unwrap();
        assert_eq!(r["added"], 1);
        let after = serde_json::to_value(&e.project).unwrap();
        let fx = &e.project.tracks[0].effects[0];
        assert!(fx.validate().is_ok());
        let v = serde_json::to_value(fx).unwrap();
        assert_eq!(v["mode"], "fdn_room");
        assert!((v["decay_s"].as_f64().unwrap() - 0.45).abs() < 1e-6);
        let mut l = vec![0.; 88200];
        let mut rr = l.clone();
        l[0] = 0.5;
        rr[0] = 0.5;
        let map = std::collections::HashMap::new();
        fx.process(
            &mut l,
            &mut rr,
            &beatbox::fx::FxContext {
                step_secs: 0.125,
                triggers: &map,
                levels: None,
                offset: 0,
            },
        );
        assert!(l[1000..22050].iter().any(|x| x.abs() > 1e-6));
        assert!(l.iter().chain(&rr).all(|x| x.is_finite()));
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        e.call("save_project", &json!({"path":"room.json"}))
            .unwrap();
        e.call("load_project", &json!({"path":"room.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
    }
}

#[cfg(test)]
mod warm_keys_tests {
    use crate as beatbox;
    use serde_json::json;
    #[test]
    fn keys_chain_routing_history_and_reopen() {
        let mut e = beatbox::Engine::new("/tmp/keys-chain-test-work".into());
        e.call("add_track", &json!({"name":"piano","preset":"epiano"}))
            .unwrap();
        e.call(
            "add_effect",
            &json!({"track":"piano","type":"gain","params":{"db":-2}}),
        )
        .unwrap();
        let b = serde_json::to_value(&e.project).unwrap();
        e.call("effects",&json!({"action":"apply_fx_chain","params":{"track":"piano","chain":"warm_recorded_keys"}})).unwrap();
        let a = serde_json::to_value(&e.project).unwrap();
        assert_eq!(e.project.tracks[0].effects.len(), 4);
        assert_eq!(a["tracks"][0]["effects"][0], b["tracks"][0]["effects"][0]);
        assert!(e.project.tracks[0]
            .effects
            .iter()
            .all(|f| f.validate().is_ok()));
        assert_eq!(a["tracks"][0]["effects"][2]["mode"], "tape");
        assert_eq!(a["tracks"][0]["effects"][2]["oversample"], true);
        assert_eq!(a["tracks"][0]["effects"][3]["mode"], "fdn_plate");
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), a);
        e.call("save_project", &json!({"path":"keys.json"}))
            .unwrap();
        e.call("load_project", &json!({"path":"keys.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), a);
    }
}

#[cfg(test)]
mod recorded_velocity_curve_tests {
    use serde_json::json;
    #[test]
    fn curve_history_layer_gain_persistence_streaming() {
        let mut e = crate::Engine::new("/tmp/curve-test-work".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        let b = serde_json::to_value(&e.project).unwrap();
        let i = e.project.track_index("piano").unwrap();
        let p = if let crate::instruments::Instrument::Multisample(p) =
            &e.project.tracks[i].instrument
        {
            p.clone()
        } else {
            panic!()
        };
        assert_eq!(p.velocity_curve, 1.0);
        assert_eq!(crate::multisample::response_velocity(&p, 0.4), 0.4);
        e.bank.sync(&e.project.samples);
        let dry = crate::multisample::render_multisample(&p, 60., 0.4, 0.2, &e.bank, 0);
        e.call("instrument",&json!({"action":"set_sample_performance","params":{"track":"piano","velocity_curve":2}})).unwrap();
        let a = serde_json::to_value(&e.project).unwrap();
        let mut expected = b.clone();
        expected["tracks"][i]["instrument"]["velocity_curve"] = json!(2.0);
        assert_eq!(a, expected);
        let h = e.history().0.len();
        e.call(
            "set_sample_performance",
            &json!({"track":"piano","velocity_curve":2}),
        )
        .unwrap();
        assert_eq!(h, e.history().0.len());
        for v in [json!(0), json!(4.1), json!("2"), json!(true)] {
            assert!(e
                .call(
                    "set_sample_performance",
                    &json!({"track":"piano","velocity_curve":v})
                )
                .is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), a);
        }
        let mut soft = p.clone();
        soft.velocity_curve = 2.;
        assert!((crate::multisample::response_velocity(&soft, 0.4) - 0.16).abs() < 1e-6);
        assert_ne!(
            dry,
            crate::multisample::render_multisample(&soft, 60., 0.4, 0.2, &e.bank, 0)
        );
        let mut strong = p.clone();
        strong.velocity_curve = 0.5;
        assert!(crate::multisample::response_velocity(&strong, 0.4) > 0.6);
        assert_eq!(crate::multisample::response_velocity(&strong, 0.), 0.);
        assert_eq!(crate::multisample::response_velocity(&soft, 1.), 1.);
        // Curve is applied exactly once for layer selection and once to the independent gain term.
        for z in &mut strong.zones {
            z.vel_lo = 0.;
            z.vel_hi = 1.;
        }
        strong.vel_to_gain = 1.;
        strong.velocity_curve = 1.;
        let s1 = crate::multisample::render_multisample(&strong, 60., 0.5, 0.2, &e.bank, 0);
        strong.velocity_curve = 2.;
        let s2 = crate::multisample::render_multisample(&strong, 60., 0.5, 0.2, &e.bank, 0);
        assert_eq!(s1.len(), s2.len());
        assert!(s1.iter().zip(&s2).all(|(x, y)| (y - x * 0.5).abs() < 1e-6));
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), a);
        let opts = crate::render::RenderOptions {
            keep_stems: true,
            ..Default::default()
        };
        let full = crate::render::render(&e.project, &e.bank, &opts).unwrap();
        for block in [63, 512, 4096] {
            let mut stream = crate::rt::MixNode::new(&e.project, &e.bank, &opts, block);
            let (l, r) = stream.render();
            for (x, y) in l.iter().zip(&full.left) {
                assert!((x - y).abs() < 1e-6);
            }
            for (x, y) in r.iter().zip(&full.right) {
                assert!((x - y).abs() < 1e-6);
            }
        }
        e.call("save_project", &json!({"path":"curve.json"}))
            .unwrap();
        e.call("load_project", &json!({"path":"curve.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), a);
        let mut old = serde_json::to_value(&p).unwrap();
        old.as_object_mut().unwrap().remove("velocity_curve");
        let old: crate::multisample::MultisampleParams = serde_json::from_value(old).unwrap();
        assert_eq!(
            dry,
            crate::multisample::render_multisample(&old, 60., 0.4, 0.2, &e.bank, 0)
        );
        let mut p = crate::multisample::MultisampleParams::default();
        p.vel_crossfade = false;
        p.zones = vec![
            crate::multisample::Zone {
                sample: "soft".into(),
                root: 60,
                lo_note: 0,
                hi_note: 127,
                vel_lo: 0.,
                vel_hi: 0.5,
                tune_cents: 0.,
                gain_db: 0.,
                rr: 0,
            },
            crate::multisample::Zone {
                sample: "hard".into(),
                vel_lo: 0.5,
                vel_hi: 1.,
                ..p.zones
                    .first()
                    .cloned()
                    .unwrap_or(crate::multisample::Zone {
                        sample: "".into(),
                        root: 60,
                        lo_note: 0,
                        hi_note: 127,
                        vel_lo: 0.,
                        vel_hi: 1.,
                        tune_cents: 0.,
                        gain_db: 0.,
                        rr: 0,
                    })
            },
        ];
        assert_eq!(crate::multisample::pick_zones(&p, 60, 0.6, 0)[0].0, 1);
        p.velocity_curve = 2.;
        assert_eq!(crate::multisample::pick_zones(&p, 60, 0.6, 0)[0].0, 0);
    }
}

#[cfg(test)]
mod recorded_kit_curve_tests {
    use serde_json::json;
    #[test]
    fn actual_layer_curve_preview_and_region_export() {
        let mut e = crate::Engine::new("/tmp/curve-kit-work".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("room-demo.json")}),
        )
        .unwrap();
        e.call(
            "set_sample_performance",
            &json!({"track":"drums","velocity_blend":false,"round_robin":false,"velocity_curve":1}),
        )
        .unwrap();
        let b = serde_json::to_value(&e.project).unwrap();
        let old = e
            .call(
                "preview_kit_piece",
                &json!({"track":"drums","key":38,"velocity":0.6,"play":true}),
            )
            .unwrap();
        let old_pcm = e
            .transport
            .pending_audition
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap()
            .0
            .clone();
        println!("curve1 sources {}", old["samples"]);
        e.call(
            "set_sample_performance",
            &json!({"track":"drums","velocity_curve":2}),
        )
        .unwrap();
        let a = serde_json::to_value(&e.project).unwrap();
        let now = e
            .call(
                "preview_kit_piece",
                &json!({"track":"drums","key":38,"velocity":0.6,"play":true}),
            )
            .unwrap();
        println!("curve2 sources {}", now["samples"]);
        assert_ne!(old["samples"], now["samples"]);
        assert_ne!(
            old_pcm,
            e.transport
                .pending_audition
                .as_ref()
                .unwrap()
                .as_ref()
                .unwrap()
                .0
        );
        assert_eq!(serde_json::to_value(&e.project).unwrap(), a);
        let p = match &e.project.tracks[0].instrument {
            crate::instruments::Instrument::Multisample(p) => p,
            _ => panic!(),
        };
        let expected = crate::multisample::render_multisample(p, 38., 0.6, 0.25, &e.bank, 0);
        assert!(expected
            .iter()
            .zip(
                &e.transport
                    .pending_audition
                    .as_ref()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .0
            )
            .all(|(a, b)| (a * 10f32.powf(-6. / 20.) - b).abs() < 1e-7));
        e.call(
            "apply_fx_chain",
            &json!({"track":"drums","chain":"short_drum_room"}),
        )
        .unwrap();
        for range in [None, Some((16, 32))] {
            let o = crate::render::RenderOptions {
                step_range: range,
                tail: 0.5,
                ..Default::default()
            };
            let want = crate::render::render(&e.project, &e.bank, &o).unwrap();
            for block in [63, 512, 4096] {
                let mut m = crate::rt::MixNode::new(&e.project, &e.bank, &o, block);
                let (l, r) = m.render();
                assert_eq!(l, want.left);
                assert_eq!(r, want.right);
            }
        }
        e.call("undo", &json!({})).unwrap();
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
    }
}

#[cfg(test)]
mod global_sample_tuning_tests {
    use serde_json::json;
    #[test]
    fn global_tuning_compatibility_history_and_streaming() {
        let mut e = crate::Engine::new("/tmp/global-tune-test".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        e.bank.sync(&e.project.samples);
        let b = serde_json::to_value(&e.project).unwrap();
        let p = match &e.project.tracks[1].instrument {
            crate::instruments::Instrument::Multisample(p) => p.clone(),
            _ => panic!(),
        };
        let dry = crate::multisample::render_multisample(&p, 60., 0.7, 0.25, &e.bank, 0);
        let mut old = serde_json::to_value(&p).unwrap();
        old.as_object_mut().unwrap().remove("tune_cents");
        let old: crate::multisample::MultisampleParams = serde_json::from_value(old).unwrap();
        assert_eq!(
            dry,
            crate::multisample::render_multisample(&old, 60., 0.7, 0.25, &e.bank, 0)
        );
        e.call(
            "instrument",
            &json!({"action":"set_sample_performance","params":{"track":"piano","tune_cents":-50}}),
        )
        .unwrap();
        let a = serde_json::to_value(&e.project).unwrap();
        let mut expected = b.clone();
        expected["tracks"][1]["instrument"]["tune_cents"] = json!(-50.);
        assert_eq!(a, expected);
        let h = e.history().0.len();
        e.call(
            "set_sample_performance",
            &json!({"track":"piano","tune_cents":-50}),
        )
        .unwrap();
        assert_eq!(h, e.history().0.len());
        for v in [json!(-101), json!(101), json!("5"), json!(true)] {
            assert!(e
                .call(
                    "set_sample_performance",
                    &json!({"track":"piano","tune_cents":v})
                )
                .is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), a);
        }
        let mut tuned = p.clone();
        tuned.tune_cents = -50.;
        assert_ne!(
            dry,
            crate::multisample::render_multisample(&tuned, 60., 0.7, 0.25, &e.bank, 0)
        );
        let mut zone = p.clone();
        for z in &mut zone.zones {
            z.tune_cents -= 50.;
        }
        assert_eq!(
            crate::multisample::render_multisample(&zone, 60., 0.7, 0.25, &e.bank, 0),
            crate::multisample::render_multisample(&tuned, 60., 0.7, 0.25, &e.bank, 0)
        );
        assert_eq!(
            crate::multisample::pick_zones(&p, 60, 0.7, 0),
            crate::multisample::pick_zones(&tuned, 60, 0.7, 0)
        );
        tuned.one_shot = true;
        zone = p.clone();
        zone.one_shot = true;
        assert!(
            crate::multisample::render_multisample(&tuned, 60., 0.7, 0.25, &e.bank, 0).len()
                > crate::multisample::render_multisample(&zone, 60., 0.7, 0.25, &e.bank, 0).len()
        );
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), a);
        e.call("save_project", &json!({"path":"tuned.json"}))
            .unwrap();
        e.call("load_project", &json!({"path":"tuned.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), a);
        for range in [None, Some((16, 32))] {
            let o = crate::render::RenderOptions {
                step_range: range,
                tail: 0.5,
                ..Default::default()
            };
            let want = crate::render::render(&e.project, &e.bank, &o).unwrap();
            for block in [63, 512, 4096] {
                let mut m = crate::rt::MixNode::new(&e.project, &e.bank, &o, block);
                let (l, r) = m.render();
                assert_eq!(l, want.left);
                assert_eq!(r, want.right);
            }
        }
    }
}

#[cfg(test)]
mod extract_kit_tests {
    use crate as beatbox;
    use serde_json::json;
    #[test]
    fn extraction_history_roundtrip_and_independent_processing() {
        let mut e = beatbox::Engine::new("/tmp/extract-test-work".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        e.bank.sync(&e.project.samples);
        e.project.tracks[0].effects.clear();
        let mut second = e.project.patterns[0].clone();
        second.id.clear();
        second.name = "second".into();
        for ns in second.clips.values_mut() {
            for n in ns {
                n.id.clear();
            }
        }
        e.project.patterns.push(second);
        e.project.ensure_fx_ids();
        let b = serde_json::to_value(&e.project).unwrap();
        let r=e.call("instrument",&json!({"action":"extract_kit_piece","params":{"track":"drums","key":38,"new_track":"snare"}})).unwrap();
        assert_eq!(r["notes_moved"], 20);
        let a = serde_json::to_value(&e.project).unwrap();
        assert_eq!(e.project.tracks.len(), 3);
        let mut want = b.clone();
        let mut d = want["tracks"][0].clone();
        d["name"] = json!("snare");
        d["id"] = a["tracks"][2]["id"].clone();
        want["tracks"].as_array_mut().unwrap().push(d);
        for pat in want["patterns"].as_array_mut().unwrap() {
            let src = pat["clips"]["drums"].as_array_mut().unwrap();
            let moved = src
                .iter()
                .filter(|n| n["pitch"] == 38)
                .cloned()
                .collect::<Vec<_>>();
            src.retain(|n| n["pitch"] != 38);
            pat["clips"]["snare"] = json!(moved);
        }
        want["id_seq"] = a["id_seq"].clone();
        assert!(a == want, "unexpected extraction diff");
        assert_ne!(a["tracks"][0]["id"], a["tracks"][2]["id"]);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), a);
        e.call("save_project", &json!({"path":"extracted.json"}))
            .unwrap();
        e.call("load_project", &json!({"path":"extracted.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), a);
        let srcfx = e.project.tracks[0].effects.clone();
        e.call(
            "apply_fx_chain",
            &json!({"track":"snare","chain":"short_drum_room"}),
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(&e.project.tracks[0].effects).unwrap(),
            serde_json::to_value(srcfx).unwrap()
        );
        assert_eq!(e.project.tracks[2].effects.len(), 1);
        for range in [None, Some((16, 32))] {
            let o = beatbox::render::RenderOptions {
                step_range: range,
                tail: 0.5,
                ..Default::default()
            };
            let want = beatbox::render::render(&e.project, &e.bank, &o).unwrap();
            for block in [63, 512, 4096] {
                let mut m = beatbox::rt::MixNode::new(&e.project, &e.bank, &o, block);
                let (l, r) = m.render();
                assert_eq!(l, want.left);
                assert_eq!(r, want.right);
            }
        }
    }
    #[test]
    fn extraction_rejects_ambiguous_or_unsafe_changes_atomically() {
        let mut e = beatbox::Engine::new("/tmp/extract-reject-work".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        let b = serde_json::to_value(&e.project).unwrap();
        for a in [
            json!({"track":"drums","key":38,"new_track":"piano"}),
            json!({"track":"drums","key":38,"new_track":"master"}),
            json!({"track":"drums","key":38,"new_track":""}),
            json!({"track":"drums","key":38,"new_track":"2"}),
            json!({"track":"drums","key":38.1}),
            json!({"track":"piano","key":38}),
            json!({"track":"drums","key":44}),
        ] {
            assert!(e.call("extract_kit_piece", &a).is_err(), "{a}");
            assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        }
        e.project
            .automation
            .push(beatbox::automation::AutomationLane::new("drums", "volume"));
        let b = serde_json::to_value(&e.project).unwrap();
        assert!(e
            .call("extract_kit_piece", &json!({"track":"drums","key":38}))
            .is_err());
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.project.automation.clear();
        e.call(
            "set_sample_performance",
            &json!({"track":"drums","gm_hat_choke":true}),
        )
        .unwrap();
        let b = serde_json::to_value(&e.project).unwrap();
        assert!(e
            .call("extract_kit_piece", &json!({"track":"drums","key":42}))
            .is_err());
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
    }
}

#[cfg(test)]
mod shared_room_send_tests {
    use crate as beatbox;
    use serde_json::json;
    #[test]
    fn room_return_history_routing_and_audio() {
        let mut e = beatbox::Engine::new("/tmp/room-send-work".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        e.bank.sync(&e.project.samples);
        let b = serde_json::to_value(&e.project).unwrap();
        let o = beatbox::render::RenderOptions {
            tail: 1.,
            ..Default::default()
        };
        let dry = beatbox::render::render(&e.project, &e.bank, &o).unwrap();
        e.call("effects",&json!({"action":"create_room_send","params":{"track":"drums","send_db":-9,"decay_s":0.8}})).unwrap();
        let a = serde_json::to_value(&e.project).unwrap();
        assert_eq!(e.project.buses.len(), 1);
        let mut expected = b.clone();
        expected["tracks"][0]["sends"] = a["tracks"][0]["sends"].clone();
        expected["buses"] = a["buses"].clone();
        expected["id_seq"] = a["id_seq"].clone();
        assert!(a == expected);
        let bus = &e.project.buses[0];
        assert_eq!(bus.name, "drums_room");
        assert!(bus.output.is_none());
        let v = serde_json::to_value(&bus.effects[0]).unwrap();
        assert_eq!(v["mix"], 1.);
        assert_eq!(v["mode"], "fdn_room");
        assert_eq!(e.project.tracks[0].sends[0].db, -9.);
        assert!(!e.project.tracks[0].sends[0].pre_fader);
        let wet = beatbox::render::render(&e.project, &e.bank, &o).unwrap();
        assert_ne!(wet.left, dry.left);
        e.project.buses[0].mute = true;
        let bypass = beatbox::render::render(&e.project, &e.bank, &o).unwrap();
        assert_eq!(bypass.left, dry.left);
        assert_eq!(bypass.right, dry.right);
        e.project.buses[0].mute = false;
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), a);
        e.call("save_project", &json!({"path":"room-return.json"}))
            .unwrap();
        e.call("load_project", &json!({"path":"room-return.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), a);
        e.call(
            "set_send",
            &json!({"track":"piano","bus":"drums_room","db":-18}),
        )
        .unwrap();
        assert_eq!(e.project.buses.len(), 1);
        for range in [None, Some((16, 32))] {
            let o = beatbox::render::RenderOptions {
                step_range: range,
                tail: 0.5,
                keep_stems: true,
                ..Default::default()
            };
            let want = beatbox::render::render(&e.project, &e.bank, &o).unwrap();
            assert_eq!(want.bus_stems.len(), 1);
            assert!(want.bus_stems[0].left.iter().any(|x| x.abs() > 1e-6));
            let mut cache = beatbox::render::TrackCache::default();
            for _ in 0..2 {
                let c = beatbox::render::render_cached(&e.project, &e.bank, &o, Some(&mut cache))
                    .unwrap();
                assert_eq!(c.left, want.left);
                assert_eq!(c.right, want.right);
            }
        }
    }
    #[test]
    fn room_send_rejection_atomicity() {
        let mut e = beatbox::Engine::new("/tmp/room-send-reject".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        let b = serde_json::to_value(&e.project).unwrap();
        for a in [
            json!({"track":"drums","bus":"master"}),
            json!({"track":"drums","bus":"piano"}),
            json!({"track":"drums","bus":""}),
            json!({"track":"drums","send_db":1}),
            json!({"track":"drums","send_db":"-12"}),
            json!({"track":"drums","decay_s":4}),
            json!({"track":"drums","predelay_ms":-1}),
            json!({"track":"drums","low_cut_hz":1001}),
        ] {
            assert!(e.call("create_room_send", &a).is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        }
        e.call("create_room_send", &json!({"track":"drums"}))
            .unwrap();
        let a = serde_json::to_value(&e.project).unwrap();
        assert!(e
            .call("create_room_send", &json!({"track":"drums"}))
            .is_err());
        assert_eq!(serde_json::to_value(&e.project).unwrap(), a);
    }
}

#[cfg(test)]
mod echo_send_tests {
    use crate as beatbox;
    use serde_json::json;
    #[test]
    fn echo_return_history_routing_and_audio() {
        let mut e = beatbox::Engine::new("/tmp/echo-send-work".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        e.bank.sync(&e.project.samples);
        let b = serde_json::to_value(&e.project).unwrap();
        let o = beatbox::render::RenderOptions {
            tail: 1.,
            ..Default::default()
        };
        let dry = beatbox::render::render(&e.project, &e.bank, &o).unwrap();
        e.call("effects",&json!({"action":"create_echo_send","params":{"track":"drums","send_db":-9,"feedback":0.4}})).unwrap();
        let a = serde_json::to_value(&e.project).unwrap();
        assert_eq!(e.project.buses.len(), 1);
        let mut expected = b.clone();
        expected["tracks"][0]["sends"] = a["tracks"][0]["sends"].clone();
        expected["buses"] = a["buses"].clone();
        expected["id_seq"] = a["id_seq"].clone();
        assert!(a == expected);
        let bus = &e.project.buses[0];
        assert_eq!(bus.name, "drums_echo");
        assert!(bus.output.is_none());
        let v = serde_json::to_value(&bus.effects[0]).unwrap();
        assert_eq!(v["mix"], 1.);
        assert_eq!(v["steps"], 3.);
        assert_eq!(v["ping_pong"], true);
        assert_eq!(e.project.tracks[0].sends[0].db, -9.);
        assert!(!e.project.tracks[0].sends[0].pre_fader);
        let wet = beatbox::render::render(&e.project, &e.bank, &o).unwrap();
        assert_ne!(wet.left, dry.left);
        e.project.buses[0].mute = true;
        let bypass = beatbox::render::render(&e.project, &e.bank, &o).unwrap();
        assert_eq!(bypass.left, dry.left);
        assert_eq!(bypass.right, dry.right);
        e.project.buses[0].mute = false;
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), a);
        e.call("save_project", &json!({"path":"room-return.json"}))
            .unwrap();
        e.call("load_project", &json!({"path":"room-return.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), a);
        e.call(
            "set_send",
            &json!({"track":"piano","bus":"drums_echo","db":-18}),
        )
        .unwrap();
        assert_eq!(e.project.buses.len(), 1);
        for range in [None, Some((16, 32))] {
            let o = beatbox::render::RenderOptions {
                step_range: range,
                tail: 0.5,
                keep_stems: true,
                ..Default::default()
            };
            let want = beatbox::render::render(&e.project, &e.bank, &o).unwrap();
            assert_eq!(want.bus_stems.len(), 1);
            assert!(want.bus_stems[0].left.iter().any(|x| x.abs() > 1e-6));
            let mut cache = beatbox::render::TrackCache::default();
            for _ in 0..2 {
                let c = beatbox::render::render_cached(&e.project, &e.bank, &o, Some(&mut cache))
                    .unwrap();
                assert_eq!(c.left, want.left);
                assert_eq!(c.right, want.right);
            }
        }
    }
    #[test]
    fn echo_send_rejection_atomicity() {
        let mut e = beatbox::Engine::new("/tmp/echo-send-reject".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        let b = serde_json::to_value(&e.project).unwrap();
        for a in [
            json!({"track":"drums","bus":"master"}),
            json!({"track":"drums","bus":"piano"}),
            json!({"track":"drums","bus":""}),
            json!({"track":"drums","send_db":1}),
            json!({"track":"drums","send_db":"-12"}),
            json!({"track":"drums","feedback":0.8}),
            json!({"track":"drums","division":"wrong"}),
            json!({"track":"drums","tone_hz":100}),
        ] {
            assert!(e.call("create_echo_send", &a).is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        }
        e.call("create_echo_send", &json!({"track":"drums"}))
            .unwrap();
        let a = serde_json::to_value(&e.project).unwrap();
        assert!(e
            .call("create_echo_send", &json!({"track":"drums"}))
            .is_err());
        assert_eq!(serde_json::to_value(&e.project).unwrap(), a);
    }
}

#[cfg(test)]
mod echo_timing_tests {
    use crate as beatbox;
    use serde_json::json;
    #[test]
    fn fully_wet_echo_timing_base_bpm() {
        let mut e = beatbox::Engine::new("/tmp/echo-timing-work".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        for (i, division, steps) in [
            (0, "eighth", 2.),
            (1, "dotted_eighth", 3.),
            (2, "quarter", 4.),
        ] {
            e.call(
                "create_echo_send",
                &json!({"track":"piano","bus":format!("echo{i}"),"division":division,"feedback":0}),
            )
            .unwrap();
            for bpm in [90., 120.] {
                let delay = (steps * (60. / bpm / 4.) * beatbox::dsp::SR) as usize;
                let mut l = vec![0.; delay * 2 + 100];
                let mut r = l.clone();
                l[0] = 1.;
                r[0] = 1.;
                let triggers = std::collections::HashMap::new();
                let ctx = beatbox::fx::FxContext {
                    step_secs: 60. / bpm / 4.,
                    triggers: &triggers,
                    offset: 0,
                    levels: None,
                };
                e.project.buses[i].effects[0].process(&mut l, &mut r, &ctx);
                let first = l
                    .iter()
                    .zip(&r)
                    .position(|(l, r)| l.abs() + r.abs() > 0.)
                    .unwrap();
                assert_eq!(first, delay, "{division} bpm{bpm}");
                assert!(l.iter().chain(&r).all(|x| x.is_finite()));
            }
        }
    }
}

#[cfg(test)]
mod wet_delay_tests {
    use crate as beatbox;
    use serde_json::json;
    #[test]
    fn wet_delay_legacy_compatibility_and_streaming() {
        let old = json!({"type":"delay","steps":3.,"feedback":0.4,"mix":0.7,"tone":4500.,"ping_pong":true});
        let implicit: beatbox::fx::Effect = serde_json::from_value(old.clone()).unwrap();
        let mut explicit = old.clone();
        explicit["wet_only"] = json!(false);
        let explicit: beatbox::fx::Effect = serde_json::from_value(explicit).unwrap();
        let trig = std::collections::HashMap::new();
        let ctx = beatbox::fx::FxContext {
            step_secs: 0.125,
            triggers: &trig,
            offset: 0,
            levels: None,
        };
        let sig: Vec<f32> = (0..60000)
            .map(|i| {
                if i < 5000 {
                    (i as f32 * 0.03).sin() * 0.2
                } else {
                    0.
                }
            })
            .collect();
        let (mut a, mut b) = (sig.clone(), sig.clone());
        implicit.process(&mut a, &mut b, &ctx);
        let (mut c, mut d) = (sig.clone(), sig.clone());
        explicit.process(&mut c, &mut d, &ctx);
        assert_eq!(a, c);
        assert_eq!(b, d);
        for wet in [false, true] {
            let mut v = old.clone();
            v["wet_only"] = json!(wet);
            let fx: beatbox::fx::Effect = serde_json::from_value(v).unwrap();
            let (mut wantl, mut wantr) = (sig.clone(), sig.clone());
            fx.process(&mut wantl, &mut wantr, &ctx);
            if wet {
                assert_eq!(wantl[0], 0.);
                assert_eq!(wantr[0], 0.);
            }
            for block in [1, 63, 512, 4096] {
                let mut chain = beatbox::rt::ChainStreamer::build(
                    std::slice::from_ref(&fx),
                    &[],
                    ctx.step_secs,
                    &trig,
                    sig.len(),
                )
                .unwrap();
                let (mut l, mut r) = (sig.clone(), sig.clone());
                for (l, r) in l.chunks_mut(block).zip(r.chunks_mut(block)) {
                    chain.process_block(l, r);
                }
                assert_eq!(l, wantl);
                assert_eq!(r, wantr);
            }
        }
    }
}

#[cfg(test)]
mod return_duck_workflow_tests {
    use crate as beatbox;
    use serde_json::json;
    #[test]
    fn duck_return_preserves_routes_and_history() {
        let mut e = beatbox::Engine::new("/tmp/return-duck-test".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        e.bank.sync(&e.project.samples);
        e.call("create_echo_send", &json!({"track":"piano"}))
            .unwrap();
        let before = serde_json::to_value(&e.project).unwrap();
        let args = json!({"track":"piano_echo","source":"piano","depth_db":9,"release_beats":0.5,"attack_ms":3});
        let out = e
            .call("effects", &json!({"action":"duck_to_track","params":args}))
            .unwrap();
        assert!((out["release_ms"].as_f64().unwrap() - 333.3333).abs() < 0.001);
        let after = serde_json::to_value(&e.project).unwrap();
        let mut expected = before.clone();
        expected["buses"][0]["effects"] = after["buses"][0]["effects"].clone();
        expected["id_seq"] = after["id_seq"].clone();
        assert_eq!(after, expected);
        assert_eq!(
            after["buses"][0]["effects"][0],
            before["buses"][0]["effects"][0]
        );
        let count = e.history().0.len();
        e.call("duck_to_track", &args).unwrap();
        assert_eq!(e.history().0.len(), count);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        let id = e.project.buses[0].effects[1].id().to_string();
        e.call(
            "bypass_effect",
            &json!({"track":"piano_echo","index":1,"on":true}),
        )
        .unwrap();
        e.call(
            "duck_to_track",
            &json!({"track":"piano_echo","source":"piano","depth_db":12,"release_beats":1}),
        )
        .unwrap();
        assert_eq!(e.project.buses[0].effects.len(), 2);
        assert_eq!(e.project.buses[0].effects[1].id(), id);
        assert!(!e.project.buses[0].effects[1].bypassed());
        e.call("save_project", &json!({"path":"roundtrip.json"}))
            .unwrap();
        let saved = serde_json::to_value(&e.project).unwrap();
        e.call("load_project", &json!({"path":"roundtrip.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), saved);
        for range in [None, Some((16, 32))] {
            let o = beatbox::render::RenderOptions {
                tail: 0.7,
                keep_stems: true,
                step_range: range,
                ..Default::default()
            };
            let mut cache = beatbox::render::TrackCache::default();
            let want = beatbox::render::render(&e.project, &e.bank, &o).unwrap();
            for _ in 0..2 {
                let got = beatbox::render::render_cached(&e.project, &e.bank, &o, Some(&mut cache))
                    .unwrap();
                assert_eq!(want.left, got.left);
                assert_eq!(want.right, got.right);
            }
            e.project.buses[0].effects[1].set_bypass(true);
            let dryduck =
                beatbox::render::render_cached(&e.project, &e.bank, &o, Some(&mut cache)).unwrap();
            assert_ne!(dryduck.left, want.left);
            let fresh = beatbox::render::render(&e.project, &e.bank, &o).unwrap();
            assert_eq!(dryduck.left, fresh.left);
            assert_eq!(dryduck.right, fresh.right);
            assert_eq!(dryduck.stems[1].left, want.stems[1].left);
            e.project.buses[0].effects[1].set_bypass(false);
        }
    }
}

#[cfg(test)]
mod parallel_return_tests {
    use crate as beatbox;
    use serde_json::json;
    #[test]
    fn parallel_return_setup_audio_history_cache() {
        let mut e = beatbox::Engine::new("/tmp/parallel-test-work".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        e.bank.sync(&e.project.samples);
        let before = serde_json::to_value(&e.project).unwrap();
        let o = beatbox::render::RenderOptions {
            keep_stems: true,
            tail: 0.4,
            ..Default::default()
        };
        let dry = beatbox::render::render(&e.project, &e.bank, &o).unwrap();
        e.call(
            "effects",
            &json!({"action":"create_parallel_send","params":{"track":"drums","send_db":0}}),
        )
        .unwrap();
        let after = serde_json::to_value(&e.project).unwrap();
        let mut expected = before.clone();
        expected["tracks"][0]["sends"] = after["tracks"][0]["sends"].clone();
        expected["buses"] = after["buses"].clone();
        expected["id_seq"] = after["id_seq"].clone();
        assert_eq!(after, expected);
        assert_eq!(e.project.buses[0].name, "drums_parallel");
        let fx = &after["buses"][0]["effects"][0];
        assert_eq!(fx["mix"], 1.0);
        assert_eq!(fx["makeup_db"], 0.0);
        assert_eq!(fx["ratio"], 8.0);
        let wet = beatbox::render::render(&e.project, &e.bank, &o).unwrap();
        assert_eq!(wet.stems[0].left, dry.stems[0].left);
        assert_ne!(wet.left, dry.left);
        e.project.buses[0].mute = true;
        let muted = beatbox::render::render(&e.project, &e.bank, &o).unwrap();
        assert_eq!(muted.left, dry.left);
        assert_eq!(muted.right, dry.right);
        e.project.buses[0].mute = false;
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        e.call("save_project", &json!({"path":"parallel.json"}))
            .unwrap();
        e.call("load_project", &json!({"path":"parallel.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        e.call(
            "set_send",
            &json!({"track":"piano","bus":"drums_parallel","db":-18}),
        )
        .unwrap();
        for range in [None, Some((16, 32))] {
            let o = beatbox::render::RenderOptions {
                step_range: range,
                tail: 0.3,
                ..Default::default()
            };
            let want = beatbox::render::render(&e.project, &e.bank, &o).unwrap();
            let mut c = beatbox::render::TrackCache::default();
            for _ in 0..2 {
                let got =
                    beatbox::render::render_cached(&e.project, &e.bank, &o, Some(&mut c)).unwrap();
                assert_eq!(got.left, want.left);
                assert_eq!(got.right, want.right);
            }
        }
    }
    #[test]
    fn parallel_bad_inputs_atomic() {
        let mut e = beatbox::Engine::new("/tmp/parallel-reject".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        let before = serde_json::to_value(&e.project).unwrap();
        for a in [
            json!({"track":"drums","bus":"master"}),
            json!({"track":"drums","bus":"piano"}),
            json!({"track":"drums","bus":""}),
            json!({"track":"drums","ratio":13}),
            json!({"track":"drums","attack_ms":0}),
            json!({"track":"drums","threshold_db":"-24"}),
            json!({"track":"drums","release_ms":1001}),
            json!({"track":"drums","send_db":1}),
        ] {
            assert!(e.call("create_parallel_send", &a).is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        }
        e.call("create_parallel_send", &json!({"track":"drums"}))
            .unwrap();
        let a = serde_json::to_value(&e.project).unwrap();
        assert!(e
            .call("create_parallel_send", &json!({"track":"drums"}))
            .is_err());
        assert_eq!(serde_json::to_value(&e.project).unwrap(), a);
    }
    #[test]
    fn fully_processed_compressor_no_hidden_dry() {
        use beatbox::{
            dsp::SR,
            fx::{Effect, FxContext},
        };
        let mut e = beatbox::Engine::new("/tmp/parallel-dsp".into());
        e.call("add_track", &json!({"name":"drums","preset":"kick"}))
            .unwrap();
        e.call("create_parallel_send",&json!({"track":"drums","threshold_db":-30,"ratio":12,"attack_ms":0.1,"release_ms":120})).unwrap();
        let n = SR as usize;
        let original: Vec<f32> = (0..n)
            .map(|i| 0.7 * (std::f32::consts::TAU * 1000.0 * i as f32 / SR).sin())
            .collect();
        let mut l = original.clone();
        let mut r = l.clone();
        let t = std::collections::HashMap::new();
        let ctx = FxContext {
            step_secs: 0.125,
            triggers: &t,
            levels: None,
            offset: 0,
        };
        let fx = &e.project.buses[0].effects[0];
        fx.process(&mut l, &mut r, &ctx);
        let rms = (l[n / 2..].iter().map(|x| x * x).sum::<f32>() / (n - n / 2) as f32).sqrt();
        assert!(rms < 0.15, "hidden dry contribution RMS: {rms}");
        let Effect::Compressor(p) = fx else { panic!() };
        let mut p = p.clone();
        p.mix = 0.;
        let mut dry = original.clone();
        let mut dr = dry.clone();
        Effect::Compressor(p).process(&mut dry, &mut dr, &ctx);
        assert_eq!(dry, original);
    }
}

#[cfg(test)]
mod kit_pocket_tests {
    use crate as beatbox;
    use serde_json::json;
    #[test]
    fn kit_pocket_exact_edits_history_reopen_cache() {
        let mut e = beatbox::Engine::new("/tmp/pocket-test-work".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        e.bank.sync(&e.project.samples);
        let mut empty = e.project.patterns[0].clone();
        empty.name = "empty".into();
        empty.id = "pat_empty_test".into();
        empty.clips.clear();
        e.project.patterns.push(empty);
        let before = serde_json::to_value(&e.project).unwrap();
        let args = json!({"track":"drums","key":38,"offset_ms":24});
        let out = e
            .call(
                "instrument",
                &json!({"action":"set_kit_piece_pocket","params":args}),
            )
            .unwrap();
        assert!((out["offset_steps"].as_f64().unwrap() - 0.144).abs() < 1e-6);
        let after = serde_json::to_value(&e.project).unwrap();
        let mut expected = before.clone();
        for p in expected["patterns"].as_array_mut().unwrap() {
            if let Some(ns) = p["clips"].get_mut("drums").and_then(|v| v.as_array_mut()) {
                for n in ns {
                    if n["pitch"] == 38 {
                        n["offset"] = json!(0.144f32);
                    }
                }
            }
        }
        assert!(after == expected, "unexpected complete project diff");
        let h = e.history().0.len();
        e.call("set_kit_piece_pocket", &args).unwrap();
        assert_eq!(e.history().0.len(), h);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        e.call("save_project", &json!({"path":"pocket.json"}))
            .unwrap();
        e.call("load_project", &json!({"path":"pocket.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        for range in [None, Some((16, 32))] {
            let o = beatbox::render::RenderOptions {
                step_range: range,
                tail: 0.2,
                ..Default::default()
            };
            let mut cache = beatbox::render::TrackCache::default();
            let want =
                beatbox::render::render_cached(&e.project, &e.bank, &o, Some(&mut cache)).unwrap();
            assert_eq!(
                want.left,
                beatbox::render::render(&e.project, &e.bank, &o)
                    .unwrap()
                    .left
            );
            e.call(
                "set_kit_piece_pocket",
                &json!({"track":"drums","key":38,"offset_ms":0}),
            )
            .unwrap();
            let reset =
                beatbox::render::render_cached(&e.project, &e.bank, &o, Some(&mut cache)).unwrap();
            assert_ne!(reset.left, want.left);
            assert_eq!(
                reset.left,
                beatbox::render::render(&e.project, &e.bank, &o)
                    .unwrap()
                    .left
            );
            e.call("set_kit_piece_pocket", &args).unwrap();
        }
    }
    #[test]
    fn kit_pocket_strict_rejection() {
        let mut e = beatbox::Engine::new("/tmp/pocket-reject".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        let b = serde_json::to_value(&e.project).unwrap();
        for a in [
            json!({"track":"piano","key":38,"offset_ms":20}),
            json!({"track":"drums","key":38,"offset_ms":51}),
            json!({"track":"drums","key":38,"offset_ms":"20"}),
            json!({"track":"drums","key":38.5,"offset_ms":20}),
            json!({"track":"drums","key":39,"offset_ms":20}),
        ] {
            assert!(e.call("set_kit_piece_pocket", &a).is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        }
        e.project.bpm = 200.;
        let b = serde_json::to_value(&e.project).unwrap();
        assert!(e
            .call(
                "set_kit_piece_pocket",
                &json!({"track":"drums","key":38,"offset_ms":50})
            )
            .is_err());
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call(
            "set_kit_piece_pocket",
            &json!({"track":"drums","key":38,"offset_ms":-30}),
        )
        .unwrap();
        assert!(e.project.patterns[0]
            .notes("drums")
            .iter()
            .filter(|n| n.pitch == 38)
            .all(|n| (n.offset + 0.4).abs() < 1e-6));
    }
}

#[cfg(test)]
mod kit_accent_tests {
    use crate as beatbox;
    use serde_json::json;
    #[test]
    fn accent_exact_dynamics_history_cache() {
        let mut e = beatbox::Engine::new("/tmp/accent-test-work".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        e.bank.sync(&e.project.samples);
        let before = serde_json::to_value(&e.project).unwrap();
        let args = json!({"track":"drums","key":42,"accent":0.9,"soft":0.3,"every":2,"phase":0});
        e.call(
            "instrument",
            &json!({"action":"set_kit_piece_accents","params":args}),
        )
        .unwrap();
        let after = serde_json::to_value(&e.project).unwrap();
        let mut expected = before.clone();
        for p in expected["patterns"].as_array_mut().unwrap() {
            if let Some(ns) = p["clips"].get_mut("drums").and_then(|v| v.as_array_mut()) {
                let mut starts: Vec<f64> = ns
                    .iter()
                    .filter(|n| n["pitch"] == 42)
                    .map(|n| n["start"].as_f64().unwrap())
                    .collect();
                starts.sort_by(f64::total_cmp);
                starts.dedup();
                for n in ns {
                    if n["pitch"] == 42 {
                        let idx = starts
                            .iter()
                            .position(|s| *s == n["start"].as_f64().unwrap())
                            .unwrap();
                        n["vel"] = json!(if idx % 2 == 0 { 0.9f32 } else { 0.3f32 });
                    }
                }
            }
        }
        assert!(after == expected, "non-velocity project changes");
        let h = e.history().0.len();
        e.call("set_kit_piece_accents", &args).unwrap();
        assert_eq!(e.history().0.len(), h);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        e.call("save_project", &json!({"path":"accent.json"}))
            .unwrap();
        e.call("load_project", &json!({"path":"accent.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        for range in [None, Some((16, 32))] {
            let o = beatbox::render::RenderOptions {
                step_range: range,
                tail: 0.2,
                ..Default::default()
            };
            let mut cache = beatbox::render::TrackCache::default();
            let want =
                beatbox::render::render_cached(&e.project, &e.bank, &o, Some(&mut cache)).unwrap();
            assert_eq!(
                want.left,
                beatbox::render::render(&e.project, &e.bank, &o)
                    .unwrap()
                    .left
            );
            e.call(
                "set_kit_piece_accents",
                &json!({"track":"drums","key":42,"accent":0.7,"soft":0.7}),
            )
            .unwrap();
            let flat =
                beatbox::render::render_cached(&e.project, &e.bank, &o, Some(&mut cache)).unwrap();
            assert_ne!(flat.left, want.left);
            assert_eq!(
                flat.left,
                beatbox::render::render(&e.project, &e.bank, &o)
                    .unwrap()
                    .left
            );
            e.call("set_kit_piece_accents", &args).unwrap();
        }
    }
    #[test]
    fn accents_order_groups_and_rejection() {
        let mut e = beatbox::Engine::new("/tmp/accent-order".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        let b = serde_json::to_value(&e.project).unwrap();
        for a in [
            json!({"track":"piano","key":42}),
            json!({"track":"drums","key":42,"accent":1.1}),
            json!({"track":"drums","key":42,"soft":"0.3"}),
            json!({"track":"drums","key":42,"every":3}),
            json!({"track":"drums","key":42,"phase":2}),
            json!({"track":"drums","key":39}),
        ] {
            assert!(e.call("set_kit_piece_accents", &a).is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        }
        let mut notes = e.project.patterns[0]
            .notes("drums")
            .iter()
            .filter(|n| n.pitch == 42)
            .take(5)
            .cloned()
            .collect::<Vec<_>>();
        notes[1].start = notes[0].start;
        notes.reverse();
        e.project.patterns[0].clips.insert("drums".into(), notes);
        e.call(
            "set_kit_piece_accents",
            &json!({"track":"drums","key":42,"accent":0.8,"soft":0.2,"every":4,"phase":1}),
        )
        .unwrap();
        let notes = e.project.patterns[0].notes("drums");
        let mut starts: Vec<_> = notes.iter().map(|n| n.start).collect();
        starts.sort_by(f32::total_cmp);
        starts.dedup();
        for n in notes {
            let i = starts.iter().position(|s| *s == n.start).unwrap();
            assert_eq!(n.vel, if i % 4 == 1 { 0.8 } else { 0.2 });
        }
    }
}

#[cfg(test)]
mod return_filter_tests {
    use crate as beatbox;
    use serde_json::json;
    #[test]
    fn return_filter_history_preserves_unrelated_eq_cache() {
        let mut e = beatbox::Engine::new("/tmp/return-filter-test-work".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        e.call("create_echo_send", &json!({"track":"piano"}))
            .unwrap();
        e.bank.sync(&e.project.samples);
        e.call("add_effect",&json!({"track":"piano_echo","type":"parametric_eq","params":{"bands":[{"kind":"bell","freq":400,"gain_db":-2,"q":1}]}})).unwrap();
        let before = serde_json::to_value(&e.project).unwrap();
        let args = json!({"bus":"piano_echo","low_cut_hz":300,"high_cut_hz":4000});
        e.call(
            "effects",
            &json!({"action":"set_return_filter","params":args}),
        )
        .unwrap();
        let after = serde_json::to_value(&e.project).unwrap();
        let mut want = before.clone();
        want["buses"][0]["effects"] = after["buses"][0]["effects"].clone();
        want["id_seq"] = after["id_seq"].clone();
        assert_eq!(after, want);
        assert_eq!(
            after["buses"][0]["effects"][0],
            before["buses"][0]["effects"][0]
        );
        assert_eq!(
            after["buses"][0]["effects"][1],
            before["buses"][0]["effects"][1]
        );
        assert_eq!(e.project.buses[0].effects.len(), 3);
        let id = e.project.buses[0].effects[2].id().to_string();
        assert!(id.starts_with("returnfilter"));
        let h = e.history().0.len();
        e.call("set_return_filter", &args).unwrap();
        assert_eq!(e.history().0.len(), h);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        e.call("save_project", &json!({"path":"filter.json"}))
            .unwrap();
        e.call("load_project", &json!({"path":"filter.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        e.call(
            "bypass_effect",
            &json!({"track":"piano_echo","index":2,"on":true}),
        )
        .unwrap();
        e.call("set_return_filter", &args).unwrap();
        assert!(!e.project.buses[0].effects[2].bypassed());
        assert_eq!(e.project.buses[0].effects[2].id(), id);
        for range in [None, Some((16, 32))] {
            let o = beatbox::render::RenderOptions {
                step_range: range,
                tail: 0.4,
                keep_stems: true,
                ..Default::default()
            };
            let mut c = beatbox::render::TrackCache::default();
            let a = beatbox::render::render_cached(&e.project, &e.bank, &o, Some(&mut c)).unwrap();
            e.call(
                "set_return_filter",
                &json!({"bus":"piano_echo","high_cut_hz":1500}),
            )
            .unwrap();
            let b = beatbox::render::render_cached(&e.project, &e.bank, &o, Some(&mut c)).unwrap();
            assert_ne!(a.left, b.left);
            assert_eq!(a.stems[1].left, b.stems[1].left);
            assert_eq!(
                b.left,
                beatbox::render::render(&e.project, &e.bank, &o)
                    .unwrap()
                    .left
            );
            e.call("set_return_filter", &args).unwrap();
        }
    }
    #[test]
    fn return_filter_rejection_atomic() {
        let mut e = beatbox::Engine::new("/tmp/return-filter-reject".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        e.call("create_echo_send", &json!({"track":"piano"}))
            .unwrap();
        let b = serde_json::to_value(&e.project).unwrap();
        for a in [
            json!({"bus":"piano"}),
            json!({"bus":"master"}),
            json!({"bus":"piano_echo","low_cut_hz":1001}),
            json!({"bus":"piano_echo","high_cut_hz":"4000"}),
            json!({"bus":"piano_echo","low_cut_hz":1000,"high_cut_hz":1000}),
        ] {
            assert!(e.call("set_return_filter", &a).is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        }
        let mut value = serde_json::to_value(&e.project).unwrap();
        value["automation"] =
            json!([{"target":"piano_echo","param":"volume_db","points":[[0,0]],"enabled":false}]);
        e.project = serde_json::from_value(value).unwrap();
        let b = serde_json::to_value(&e.project).unwrap();
        assert!(e
            .call("set_return_filter", &json!({"bus":"piano_echo"}))
            .is_err());
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
    }
}

#[cfg(test)]
mod piece_swing_tests {
    use crate as beatbox;
    use serde_json::json;
    #[test]
    fn swing_exact_groups_history_reopen_cache() {
        let mut e = beatbox::Engine::new("/tmp/piece-swing-work".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        e.bank.sync(&e.project.samples);
        let notes = e.project.patterns[0].notes_mut("drums");
        for (i, n) in notes.iter_mut().filter(|n| n.pitch == 42).enumerate() {
            n.start = i as f32;
            n.offset = 0.03;
        }
        let mut off = notes.iter().find(|n| n.pitch == 42).unwrap().clone();
        off.start = 3.5;
        off.id = "swing_fraction_test".into();
        off.offset = -0.1;
        notes.push(off);
        let before = serde_json::to_value(&e.project).unwrap();
        let args = json!({"track":"drums","key":42,"amount":0.4});
        e.call(
            "instrument",
            &json!({"action":"set_kit_piece_swing","params":args}),
        )
        .unwrap();
        let after = serde_json::to_value(&e.project).unwrap();
        let mut expected = before.clone();
        for pat in expected["patterns"].as_array_mut().unwrap() {
            if let Some(ns) = pat["clips"].get_mut("drums").and_then(|v| v.as_array_mut()) {
                for n in ns {
                    let start = n["start"].as_f64().unwrap();
                    if n["pitch"] == 42 && start.fract() == 0. {
                        if start as i64 % 2 == 1 {
                            n["offset"] = json!(0.2f32);
                        } else {
                            n.as_object_mut().unwrap().remove("offset");
                        }
                    }
                }
            }
        }
        assert!(after == expected, "swing changed unrelated fields");
        let h = e.history().0.len();
        e.call("set_kit_piece_swing", &args).unwrap();
        assert_eq!(e.history().0.len(), h);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        e.call("save_project", &json!({"path":"swing.json"}))
            .unwrap();
        e.call("load_project", &json!({"path":"swing.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        for range in [None, Some((0, 16))] {
            let o = beatbox::render::RenderOptions {
                step_range: range,
                tail: 0.2,
                ..Default::default()
            };
            let mut c = beatbox::render::TrackCache::default();
            let a = beatbox::render::render_cached(&e.project, &e.bank, &o, Some(&mut c)).unwrap();
            e.call(
                "set_kit_piece_swing",
                &json!({"track":"drums","key":42,"amount":0}),
            )
            .unwrap();
            let b = beatbox::render::render_cached(&e.project, &e.bank, &o, Some(&mut c)).unwrap();
            assert_ne!(a.left, b.left);
            assert_eq!(
                b.left,
                beatbox::render::render(&e.project, &e.bank, &o)
                    .unwrap()
                    .left
            );
            e.call("set_kit_piece_swing", &args).unwrap();
        }
    }
    #[test]
    fn swing_rejection_atomic() {
        let mut e = beatbox::Engine::new("/tmp/piece-swing-reject".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        let b = serde_json::to_value(&e.project).unwrap();
        for a in [
            json!({"track":"piano","key":42}),
            json!({"track":"drums","key":39}),
            json!({"track":"drums","key":42,"amount":1.1}),
            json!({"track":"drums","key":42,"amount":"0.4"}),
            json!({"track":"drums","key":42,"amount":-0.1}),
        ] {
            assert!(e.call("set_kit_piece_swing", &a).is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        }
    }
}

#[cfg(test)]
mod kit_flam_tests {
    use crate as beatbox;
    use serde_json::json;
    #[test]
    fn flam_main_preservation_repeat_history_and_reopen() {
        let mut e = beatbox::Engine::new("/tmp/flam-test-work".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        e.bank.sync(&e.project.samples);
        let before = serde_json::to_value(&e.project).unwrap();
        let originals = e.project.patterns[0].notes("drums").to_vec();
        let args =
            json!({"track":"drums","key":38,"pattern":"groove","spacing_ms":18,"strength":0.45});
        let out = e
            .call(
                "instrument",
                &json!({"action":"add_kit_piece_flams","params":args}),
            )
            .unwrap();
        assert_eq!(out["added"], 10);
        let after = serde_json::to_value(&e.project).unwrap();
        let ns = e.project.patterns[0].notes("drums");
        assert_eq!(&ns[..originals.len()], originals);
        let grace: Vec<_> = ns.iter().filter(|n| n.id.starts_with("kitflam")).collect();
        assert_eq!(grace.len(), 10);
        for g in grace {
            assert!(originals.iter().any(|n| n.pitch == 38
                && (n.start - g.start - 0.108).abs() < 1e-5
                && (g.vel - n.vel * 0.45).abs() < 1e-6
                && g.offset == n.offset
                && g.prob == n.prob));
        }
        let mut expected = before.clone();
        expected["patterns"][0]["clips"]["drums"] = after["patterns"][0]["clips"]["drums"].clone();
        expected["id_seq"] = after["id_seq"].clone();
        assert!(after == expected);
        let h = e.history().0.len();
        assert_eq!(e.call("add_kit_piece_flams", &args).unwrap()["added"], 0);
        assert_eq!(e.history().0.len(), h);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        e.call("save_project", &json!({"path":"flam.json"}))
            .unwrap();
        e.call("load_project", &json!({"path":"flam.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        assert_eq!(e.call("add_kit_piece_flams", &args).unwrap()["added"], 0);
        for range in [None, Some((16, 32))] {
            let o = beatbox::render::RenderOptions {
                step_range: range,
                tail: 0.3,
                ..Default::default()
            };
            let a = beatbox::render::render(&e.project, &e.bank, &o).unwrap();
            let mut c = beatbox::render::TrackCache::default();
            let b = beatbox::render::render_cached(&e.project, &e.bank, &o, Some(&mut c)).unwrap();
            assert_eq!(a.left, b.left);
            assert_eq!(a.right, b.right);
        }
    }
    #[test]
    fn flam_edges_strict_and_global_swing_guard() {
        let mut e = beatbox::Engine::new("/tmp/flam-reject".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        let b = serde_json::to_value(&e.project).unwrap();
        for a in [
            json!({"track":"piano","key":38,"pattern":"groove"}),
            json!({"track":"drums","key":38,"pattern":"missing"}),
            json!({"track":"drums","key":38,"pattern":"groove","spacing_ms":41}),
            json!({"track":"drums","key":38,"pattern":"groove","strength":".4"}),
        ] {
            assert!(e.call("add_kit_piece_flams", &a).is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        }
        e.project.swing = 0.3;
        let b = serde_json::to_value(&e.project).unwrap();
        assert!(e
            .call(
                "add_kit_piece_flams",
                &json!({"track":"drums","key":38,"pattern":"groove"})
            )
            .is_err());
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.project.swing = 0.;
        e.project.patterns[0]
            .notes_mut("drums")
            .retain(|n| n.pitch == 38);
        let ns = e.project.patterns[0].notes_mut("drums");
        ns.truncate(1);
        ns[0].start = 0.;
        let b = serde_json::to_value(&e.project).unwrap();
        assert_eq!(
            e.call(
                "add_kit_piece_flams",
                &json!({"track":"drums","key":38,"pattern":"groove"})
            )
            .unwrap()["added"],
            0
        );
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
    }
}

#[cfg(test)]
mod return_audition_tests {
    use crate as beatbox;
    use serde_json::json;
    #[test]
    fn audition_exact_bus_full_context_readonly_unique_stop() {
        let mut e = beatbox::Engine::new("/tmp/preview-return-work".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        e.call("create_echo_send", &json!({"track":"piano","send_db":-6}))
            .unwrap();
        e.call(
            "duck_to_track",
            &json!({"track":"piano_echo","source":"piano","depth_db":9}),
        )
        .unwrap();
        e.bank.sync(&e.project.samples);
        let before = serde_json::to_value(&e.project).unwrap();
        let h = e.history().0.len();
        let out = e
            .call(
                "effects",
                &json!({"action":"preview_return","params":{"bus":"piano_echo","play":false}}),
            )
            .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        assert_eq!(e.history().0.len(), h);
        assert_eq!(out["preview_gain_db"], 0.0);
        let path = out["path"].as_str().unwrap();
        let (l, r) = beatbox::samples::decode_stereo(std::path::Path::new(path)).unwrap();
        let end = e.project.beat_at_bar(5.) * 4.;
        let want = beatbox::render::render(
            &e.project,
            &e.bank,
            &beatbox::render::RenderOptions {
                step_range: Some((0, end.round() as u32)),
                tail: 1.,
                keep_stems: true,
                ..Default::default()
            },
        )
        .unwrap();
        let stem = &want.bus_stems[0];
        assert_eq!(l.len(), stem.left.len());
        let max = l
            .iter()
            .chain(&r)
            .zip(stem.left.iter().chain(&stem.right))
            .map(|(a, b)| (a - b).abs())
            .fold(0f32, f32::max);
        assert!(max < 0.00004, "bus audition mismatch {max}");
        assert!(l.iter().any(|x| x.abs() > 1e-6));
        let second = e
            .call("preview_return", &json!({"bus":"piano_echo","play":true}))
            .unwrap();
        assert_ne!(out["path"], second["path"]);
        assert!(e.transport.pending_audition.as_ref().unwrap().is_some());
        e.call("preview_return", &json!({"stop":true})).unwrap();
        assert!(e.transport.pending_audition.as_ref().unwrap().is_none());
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        assert_eq!(e.history().0.len(), h);
    }
    #[test]
    fn audition_rejection_and_peak_reduction() {
        let mut e = beatbox::Engine::new("/tmp/preview-return-limits".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        e.call("create_echo_send", &json!({"track":"piano"}))
            .unwrap();
        for a in [
            json!({"bus":"piano"}),
            json!({"bus":"piano_echo","bars":0}),
            json!({"bus":"piano_echo","bars":9}),
            json!({"bus":"piano_echo","bars":1.5}),
        ] {
            let b = serde_json::to_value(&e.project).unwrap();
            assert!(e.call("preview_return", &a).is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        }
        e.project.buses[0].mute = true;
        assert!(e
            .call("preview_return", &json!({"bus":"piano_echo"}))
            .is_err());
        e.project.buses[0].mute = false;
        e.project.buses[0].volume_db = 60.;
        let b = serde_json::to_value(&e.project).unwrap();
        let out = e
            .call("preview_return", &json!({"bus":"piano_echo","play":false}))
            .unwrap();
        assert!(out["preview_gain_db"].as_f64().unwrap() < 0.);
        let (l, r) =
            beatbox::samples::decode_stereo(std::path::Path::new(out["path"].as_str().unwrap()))
                .unwrap();
        let peak = l.iter().chain(&r).map(|x| x.abs()).fold(0f32, f32::max);
        assert!(peak <= 0.951);
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
    }
}

#[cfg(test)]
mod return_mix_ab_tests {
    use crate as beatbox;
    use serde_json::json;
    #[test]
    fn mix_ab_exact_order_shared_gain_and_readonly() {
        let mut e = beatbox::Engine::new("/tmp/return-ab-work".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        e.call("create_echo_send", &json!({"track":"piano","send_db":-3}))
            .unwrap();
        e.call(
            "duck_to_track",
            &json!({"track":"piano_echo","source":"piano"}),
        )
        .unwrap();
        e.call(
            "add_effect",
            &json!({"track":"master","type":"gain","params":{"db":24}}),
        )
        .unwrap();
        e.bank.sync(&e.project.samples);
        let before = serde_json::to_value(&e.project).unwrap();
        let h = e.history().0.len();
        let out = e
            .call(
                "preview_return",
                &json!({"bus":"piano_echo","mode":"mix_ab","play":false}),
            )
            .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        assert_eq!(e.history().0.len(), h);
        let (l, r) =
            beatbox::samples::decode_stereo(std::path::Path::new(out["path"].as_str().unwrap()))
                .unwrap();
        let end = e.project.beat_at_bar(5.) * 4.;
        let o = beatbox::render::RenderOptions {
            step_range: Some((0, end.round() as u32)),
            tail: 1.,
            ..Default::default()
        };
        let wet = beatbox::render::render(&e.project, &e.bank, &o).unwrap();
        let mut copy = e.project.clone();
        copy.buses[0].mute = true;
        let dry = beatbox::render::render(&copy, &e.bank, &o).unwrap();
        let n = dry
            .left
            .len()
            .min(wet.left.len())
            .min((beatbox::dsp::SR * 20.) as usize);
        let gap = (beatbox::dsp::SR * 0.6) as usize;
        assert_eq!(l.len(), n * 2 + gap);
        let g = 10f32.powf(out["preview_gain_db"].as_f64().unwrap() as f32 / 20.);
        assert!(g < 1.0);
        assert!(l[n..n + gap].iter().all(|v| *v == 0.));
        for (got, a, b) in [(&l, &dry.left, &wet.left), (&r, &dry.right, &wet.right)] {
            let err1 = got[..n]
                .iter()
                .zip(&a[..n])
                .map(|(x, y)| (x - ((y * g).clamp(-1., 1.) * 32767.) as i16 as f32 / 32768.).abs())
                .fold(0f32, f32::max);
            let err2 = got[n + gap..]
                .iter()
                .zip(&b[..n])
                .map(|(x, y)| (x - ((y * g).clamp(-1., 1.) * 32767.) as i16 as f32 / 32768.).abs())
                .fold(0f32, f32::max);
            // Permit one PCM16 LSB for dB-to-linear gain roundtrip at quantization boundaries.
            assert!(
                err1 <= 1.01 / 32768. && err2 <= 1.01 / 32768.,
                "A/B stem mismatch"
            );
        }
        assert_ne!(dry.left, wet.left);
        assert_eq!(out["downstream_excluded"], serde_json::Value::Null);
    }
    #[test]
    fn mix_ab_invalid_mode_no_effect() {
        let mut e = beatbox::Engine::new("/tmp/return-ab-reject".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        e.call("create_echo_send", &json!({"track":"piano"}))
            .unwrap();
        let b = serde_json::to_value(&e.project).unwrap();
        assert!(e
            .call(
                "preview_return",
                &json!({"bus":"piano_echo","mode":"other","play":false})
            )
            .is_err());
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
    }
}

#[cfg(test)]
mod kit_ramp_tests {
    use crate as beatbox;
    use serde_json::json;
    #[test]
    fn ramp_exact_scope_groups_undo_reopen() {
        let mut e = beatbox::Engine::new("/tmp/ramp-tests".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        let p = e.project.patterns[0].clone();
        e.project.patterns.push(p);
        e.project.patterns[1].name = "untouched".into();
        e.project.patterns[1].id = "ramp_other_pattern".into();
        for n in e.project.patterns[1].clips.values_mut().flatten() {
            n.id = format!("other_{}", n.id);
        }
        let n = e.project.patterns[0].notes_mut("drums");
        let sn = n.iter().find(|n| n.pitch == 38).unwrap().clone();
        let mut sn = sn;
        sn.id = "ramp_duplicate_note".into();
        n.push(sn);
        n.reverse();
        let before = e.project.clone();
        let b = serde_json::to_value(&before).unwrap();
        let a = json!({"track":"drums","key":38,"pattern":"groove","start_velocity":0.2,"end_velocity":0.9});
        e.call("set_kit_piece_ramp", &a).unwrap();
        let mut expected = before.clone();
        let mut starts: Vec<_> = expected.patterns[0]
            .notes("drums")
            .iter()
            .filter(|n| n.pitch == 38)
            .map(|n| n.start)
            .collect();
        starts.sort_by(f32::total_cmp);
        starts.dedup();
        for n in expected.patterns[0]
            .notes_mut("drums")
            .iter_mut()
            .filter(|n| n.pitch == 38)
        {
            let i = starts.iter().position(|s| *s == n.start).unwrap();
            n.vel = if i == 0 {
                0.2
            } else if i + 1 == starts.len() {
                0.9
            } else {
                0.2 + (0.9 - 0.2) * i as f32 / (starts.len() - 1) as f32
            };
        }
        std::fs::write(
            "/tmp/ramp-actual.json",
            serde_json::to_vec_pretty(&e.project).unwrap(),
        )
        .unwrap();
        std::fs::write(
            "/tmp/ramp-expected.json",
            serde_json::to_vec_pretty(&expected).unwrap(),
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(&e.project).unwrap(),
            serde_json::to_value(&expected).unwrap()
        );
        let h = e.history().0.len();
        e.call("set_kit_piece_ramp", &a).unwrap();
        assert_eq!(h, e.history().0.len());
        e.call("save_project", &json!({"path":"/tmp/ramp-reopen.json"}))
            .unwrap();
        let mut f = beatbox::Engine::new("/tmp/ramp-reloaded".into());
        f.call("load_project", &json!({"path":"/tmp/ramp-reopen.json"}))
            .unwrap();
        assert_eq!(
            serde_json::to_value(&f.project).unwrap(),
            serde_json::to_value(&expected).unwrap()
        );
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("set_kit_piece_ramp",&json!({"track":"drums","key":38,"pattern":"groove","start_velocity":0.9,"end_velocity":0.2})).unwrap();
        let n = e.project.patterns[0].notes("drums");
        assert_eq!(
            n.iter()
                .find(|n| n.pitch == 38 && n.start == starts[0])
                .unwrap()
                .vel,
            0.9
        );
    }
    #[test]
    fn ramp_rejects_without_mutation() {
        let mut e = beatbox::Engine::new("/tmp/ramp-bad".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        let b = serde_json::to_value(&e.project).unwrap();
        for a in [
            json!({"track":"drums","pattern":"groove","key":38,"start_velocity":".3"}),
            json!({"track":"drums","pattern":"groove","key":38,"end_velocity":1.1}),
            json!({"track":"drums","pattern":"absent","key":38}),
            json!({"track":"piano","pattern":"groove","key":38}),
            json!({"track":"drums","pattern":"groove","key":38.5}),
        ] {
            assert!(e.call("set_kit_piece_ramp", &a).is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        }
        e.project.patterns[0]
            .notes_mut("drums")
            .retain(|n| n.pitch != 38);
        assert!(e
            .call(
                "set_kit_piece_ramp",
                &json!({"track":"drums","pattern":"groove","key":38})
            )
            .is_err());
    }
}

#[cfg(test)]
mod kit_ramp_range_tests {
    use crate as beatbox;
    use serde_json::json;
    #[test]
    fn ramp_range_curve_exact_boundaries() {
        let mut e = beatbox::Engine::new("/tmp/ramp-range-test".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        let src = e.project.patterns[0]
            .notes("drums")
            .iter()
            .find(|n| n.pitch == 38)
            .unwrap()
            .clone();
        let notes = e.project.patterns[0].notes_mut("drums");
        notes.retain(|n| n.pitch != 38);
        for (i, start) in [0., 15.9, 16., 20., 24., 28., 32., 48.].iter().enumerate() {
            let mut n = src.clone();
            n.start = *start;
            n.id = format!("range_note_{i}");
            n.offset = if i % 2 == 0 { 0.2 } else { -0.2 };
            notes.push(n);
        }
        let before = e.project.clone();
        let b = serde_json::to_value(&before).unwrap();
        let out=e.call("set_kit_piece_ramp",&json!({"track":"drums","pattern":"groove","key":38,"from_step":16,"to_step":32,"curve":2,"start_velocity":0.2,"end_velocity":0.8})).unwrap();
        assert_eq!(out["notes"], 4);
        let mut expected = before;
        for n in expected.patterns[0]
            .notes_mut("drums")
            .iter_mut()
            .filter(|n| n.pitch == 38 && n.start >= 16. && n.start < 32.)
        {
            let i = (n.start - 16.) / 4.;
            n.vel = if i == 0. {
                0.2
            } else if i == 3. {
                0.8
            } else {
                0.2 + (0.8 - 0.2) * (i / 3.).powf(2.)
            };
        }
        assert_eq!(
            serde_json::to_value(&e.project).unwrap(),
            serde_json::to_value(&expected).unwrap()
        );
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("set_kit_piece_ramp",&json!({"track":"drums","pattern":"groove","key":38,"from_step":16,"to_step":32,"curve":0.5})).unwrap();
        let n = e.project.patterns[0]
            .notes("drums")
            .iter()
            .find(|n| n.pitch == 38 && n.start == 20.)
            .unwrap();
        assert!((n.vel - (0.25 + (0.95 - 0.25) * (1f32 / 3.).sqrt())).abs() < 1e-6);
    }
    #[test]
    fn ramp_range_rejects_atomic() {
        let mut e = beatbox::Engine::new("/tmp/ramp-range-bad".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        let b = serde_json::to_value(&e.project).unwrap();
        for x in [
            json!({"from_step":1}),
            json!({"to_step":32}),
            json!({"from_step":32,"to_step":32}),
            json!({"from_step":-1,"to_step":32}),
            json!({"from_step":0,"to_step":65}),
            json!({"curve":0}),
            json!({"curve":"2"}),
            json!({"from_step":0,"to_step":0.1}),
        ] {
            let mut a = json!({"track":"drums","pattern":"groove","key":38});
            for (k, v) in x.as_object().unwrap() {
                a[k] = v.clone();
            }
            assert!(e.call("set_kit_piece_ramp", &a).is_err(), "accepted {a}");
            assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        }
    }
}

#[cfg(test)]
mod kit_roll_tests {
    use crate as beatbox;
    use serde_json::json;
    #[test]
    fn roll_exact_notes_scope_repeat_and_undo() {
        let mut e = beatbox::Engine::new("/tmp/roll-test".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        e.project.patterns[0]
            .notes_mut("drums")
            .retain(|n| n.pitch != 38 || n.start < 48.);
        let before = e.project.clone();
        let a = json!({"track":"drums","pattern":"groove","key":38,"from_step":48,"to_step":64,"interval_steps":0.5,"start_velocity":0.2,"end_velocity":0.9,"curve":2});
        let out = e.call("add_kit_piece_roll", &a).unwrap();
        assert_eq!(out["added"], 32);
        let notes = e.project.patterns[0].notes("drums");
        let added: Vec<_> = notes
            .iter()
            .filter(|n| n.id.starts_with("kitroll"))
            .collect();
        assert_eq!(added.len(), 32);
        let ids: std::collections::HashSet<_> = added.iter().map(|n| &n.id).collect();
        assert_eq!(ids.len(), 32);
        for (i, n) in added.iter().enumerate() {
            assert_eq!(n.start, 48. + i as f32 * 0.5);
            assert_eq!(n.pitch, 38);
            assert_eq!(n.offset, 0.);
            assert_eq!(n.prob, 1.);
            assert_eq!(n.len, 0.1);
            assert!(n.slide_to.is_none());
            let v = if i == 0 {
                0.2
            } else if i == 31 {
                0.9
            } else {
                0.2 + (0.9 - 0.2) * (i as f32 / 31.).powf(2.)
            };
            assert_eq!(n.vel, v);
        }
        let mut stripped = e.project.clone();
        stripped.patterns[0]
            .notes_mut("drums")
            .retain(|n| !n.id.starts_with("kitroll"));
        let mut x = serde_json::to_value(&stripped).unwrap();
        let b = serde_json::to_value(&before).unwrap();
        x["id_seq"] = b["id_seq"].clone();
        assert_eq!(x, b);
        let after = serde_json::to_value(&e.project).unwrap();
        assert!(e.call("add_kit_piece_roll", &a).is_err());
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        e.call("save_project", &json!({"path":"/tmp/roll-reopen.json"}))
            .unwrap();
        let mut f = beatbox::Engine::new("/tmp/roll-reload".into());
        f.call("load_project", &json!({"path":"/tmp/roll-reopen.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&f.project).unwrap(), after);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
    }
    #[test]
    fn roll_collision_and_validation_atomic() {
        let mut e = beatbox::Engine::new("/tmp/roll-bad".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        let b = serde_json::to_value(&e.project).unwrap();
        for x in [
            json!({}),
            json!({"to_step":65}),
            json!({"interval_steps":0.7}),
            json!({"from_step":48,"to_step":48.5}),
            json!({"from_step":0,"to_step":63.9}),
            json!({"key":38.5}),
            json!({"curve":0}),
            json!({"start_velocity":".2"}),
        ] {
            let mut a =
                json!({"track":"drums","pattern":"groove","key":38,"from_step":48,"to_step":64});
            for (k, v) in x.as_object().unwrap() {
                a[k] = v.clone();
            }
            assert!(e.call("add_kit_piece_roll", &a).is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        }
        e.project.patterns[0]
            .notes_mut("drums")
            .retain(|n| n.pitch != 38);
        e.project.swing = 0.2;
        assert!(e
            .call(
                "add_kit_piece_roll",
                &json!({"track":"drums","pattern":"groove","key":38,"from_step":48,"to_step":64})
            )
            .is_err());
    }
}

#[cfg(test)]
mod triplet_roll_tests {
    use crate as beatbox;
    use serde_json::json;
    #[test]
    fn roll_triplet_positions_and_reopen() {
        for (div, count, step) in [
            ("eighth_triplet", 12, 4f64 / 3.),
            ("sixteenth_triplet", 24, 2f64 / 3.),
        ] {
            let mut e = beatbox::Engine::new("/tmp/triplet-tests".into());
            e.call(
                "load_project",
                &json!({"path":crate::test_fixtures::path("roll-empty-fixture.json")}),
            )
            .unwrap();
            let b = serde_json::to_value(&e.project).unwrap();
            let a = json!({"track":"drums","pattern":"groove","key":38,"from_step":48,"to_step":64,"subdivision":div});
            let out = e.call("add_kit_piece_roll", &a).unwrap();
            assert_eq!(out["added"], count);
            let ns: Vec<_> = e.project.patterns[0]
                .notes("drums")
                .iter()
                .filter(|n| n.id.starts_with("kitroll"))
                .collect();
            assert_eq!(ns.len(), count);
            for (i, n) in ns.iter().enumerate() {
                assert_eq!(n.start, (48. + i as f64 * step) as f32);
                assert!(n.start < 64.);
                assert_eq!(n.offset, 0.);
            }
            assert_eq!(ns[0].vel, 0.25);
            assert_eq!(ns[count - 1].vel, 0.95);
            let after = serde_json::to_value(&e.project).unwrap();
            assert!(e.call("add_kit_piece_roll", &a).is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
            e.call("save_project", &json!({"path":"/tmp/triplet-reopen.json"}))
                .unwrap();
            let mut f = beatbox::Engine::new("/tmp/triplet-reload".into());
            f.call("load_project", &json!({"path":"/tmp/triplet-reopen.json"}))
                .unwrap();
            assert_eq!(serde_json::to_value(&f.project).unwrap(), after);
            e.call("undo", &json!({})).unwrap();
            assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        }
    }
    #[test]
    fn roll_subdivision_strict_and_straight_compat() {
        let mut e = beatbox::Engine::new("/tmp/triplet-bad".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("roll-empty-fixture.json")}),
        )
        .unwrap();
        let b = serde_json::to_value(&e.project).unwrap();
        for x in [
            json!({"subdivision":"sixteenth_triplet","interval_steps":1}),
            json!({"subdivision":12}),
            json!({"subdivision":"triplet"}),
            json!({"subdivision":"eighth_triplet","to_step":63}),
            json!({"subdivision":"sixteenth_triplet","from_step":0,"to_step":64,"key":36}),
        ] {
            let mut a =
                json!({"track":"drums","pattern":"groove","key":38,"from_step":48,"to_step":64});
            for (k, v) in x.as_object().unwrap() {
                a[k] = v.clone();
            }
            assert!(e.call("add_kit_piece_roll", &a).is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        }
        let mut f = beatbox::Engine::new("/tmp/triplet-straight".into());
        f.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("roll-empty-fixture.json")}),
        )
        .unwrap();
        e.call("add_kit_piece_roll",&json!({"track":"drums","pattern":"groove","key":38,"from_step":48,"to_step":64,"interval_steps":1})).unwrap();
        f.call("add_kit_piece_roll",&json!({"track":"drums","pattern":"groove","key":38,"from_step":48,"to_step":64,"subdivision":"sixteenth"})).unwrap();
        assert_eq!(
            serde_json::to_value(&e.project).unwrap(),
            serde_json::to_value(&f.project).unwrap()
        );
    }
}

#[cfg(test)]
mod piece_balance_tests {
    use crate as beatbox;
    use serde_json::json;
    fn fixture() -> beatbox::Engine {
        let mut e = beatbox::Engine::new("/tmp/piece-pan-test".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        e.project.master_effects.clear();
        for t in &mut e.project.tracks {
            t.effects.clear();
            t.pan = 0.;
            t.volume_db = 0.;
            t.mute = t.name != "drums";
        }
        e.bank.sync(&e.project.samples);
        e
    }
    #[test]
    fn piece_pan_exact_scope_reopen_and_reset() {
        let mut e = fixture();
        let before = serde_json::to_value(&e.project).unwrap();
        e.call(
            "instrument",
            &json!({"action":"set_kit_piece_pan","params":{"track":"drums","key":42,"pan":0.8}}),
        )
        .unwrap();
        let mut ex = before.clone();
        ex["tracks"][0]["instrument"]["key_pan"] = json!({"42":0.8f32});
        let a = serde_json::to_value(&e.project).unwrap();
        assert_eq!(a, ex);
        let h = e.history().0.len();
        e.call(
            "set_kit_piece_pan",
            &json!({"track":"drums","key":42,"pan":0.8}),
        )
        .unwrap();
        assert_eq!(e.history().0.len(), h);
        e.call(
            "save_project",
            &json!({"path":"/tmp/piece-pan-reopen.json"}),
        )
        .unwrap();
        let mut f = fixture();
        f.call(
            "load_project",
            &json!({"path":"/tmp/piece-pan-reopen.json"}),
        )
        .unwrap();
        assert_eq!(serde_json::to_value(&f.project).unwrap(), a);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        e.call("redo", &json!({})).unwrap();
        e.call(
            "set_kit_piece_pan",
            &json!({"track":"drums","key":42,"pan":0}),
        )
        .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        for a in [
            json!({"track":"piano","key":42,"pan":0.5}),
            json!({"track":"drums","key":42,"pan":1.1}),
            json!({"track":"drums","key":42,"pan":".5"}),
            json!({"track":"drums","key":42.5,"pan":0.5}),
        ] {
            assert!(e.call("set_kit_piece_pan", &a).is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        }
    }
    #[test]
    fn piece_pan_render_cache_stream_and_unrelated_keys() {
        let mut e = fixture();
        let opts = beatbox::render::RenderOptions {
            step_range: Some((0, 16)),
            tail: 0.,
            keep_stems: true,
            ..Default::default()
        };
        let original = e.project.clone();
        let base = beatbox::render::render(&e.project, &e.bank, &opts).unwrap();
        let mut cache = beatbox::render::TrackCache::default();
        beatbox::render::render_cached(&e.project, &e.bank, &opts, Some(&mut cache)).unwrap();
        e.call(
            "set_kit_piece_pan",
            &json!({"track":"drums","key":42,"pan":1}),
        )
        .unwrap();
        let a =
            beatbox::render::render_cached(&e.project, &e.bank, &opts, Some(&mut cache)).unwrap();
        let fresh = beatbox::render::render(&e.project, &e.bank, &opts).unwrap();
        assert_eq!(a.left, fresh.left);
        assert_eq!(a.right, fresh.right);
        assert_ne!(base.left, a.left);
        assert_eq!(base.stems[0].right, a.stems[0].right);
        let mut stream = beatbox::rt::VoiceStreamer::new(&e.project, &e.bank, &opts, 127, 0);
        let (l, r) = stream.render_all();
        let dl = l
            .iter()
            .zip(&a.stems[0].left)
            .map(|(a, b)| (a - b).abs())
            .fold(0f32, f32::max);
        let dr = r
            .iter()
            .zip(&a.stems[0].right)
            .map(|(a, b)| (a - b).abs())
            .fold(0f32, f32::max);
        assert!(dl < 1e-7 && dr < 1e-7, "stream mismatch left{dl} right{dr}");
        let mut only_other = original.clone();
        for p in &mut only_other.patterns {
            p.notes_mut("drums").retain(|n| n.pitch != 42);
        }
        let b = beatbox::render::render(&only_other, &e.bank, &opts).unwrap();
        if let beatbox::instruments::Instrument::Multisample(p) =
            &mut only_other.tracks[0].instrument
        {
            p.key_pan.insert(42, 1.);
        }
        let c = beatbox::render::render(&only_other, &e.bank, &opts).unwrap();
        assert_eq!(b.left, c.left);
        assert_eq!(b.right, c.right);
        e.call(
            "set_kit_piece_pan",
            &json!({"track":"drums","key":42,"pan":-1}),
        )
        .unwrap();
        let left = beatbox::render::render(&e.project, &e.bank, &opts).unwrap();
        assert_eq!(base.left, left.left);
        assert_ne!(base.right, left.right);
        e.call(
            "set_kit_piece_pan",
            &json!({"track":"drums","key":42,"pan":0}),
        )
        .unwrap();
        let reset = beatbox::render::render(&e.project, &e.bank, &opts).unwrap();
        assert_eq!(reset.left, base.left);
        assert_eq!(reset.right, base.right);
    }
}

#[cfg(test)]
mod piece_attack_tests {
    use crate as beatbox;
    use serde_json::json;
    fn fixture() -> beatbox::Engine {
        let mut e = beatbox::Engine::new("/tmp/piece-attack-test".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        e.project.master_effects.clear();
        for t in &mut e.project.tracks {
            t.effects.clear();
            t.pan = 0.;
            t.volume_db = 0.;
            t.mute = t.name != "drums";
        }
        e.bank.sync(&e.project.samples);
        e
    }
    #[test]
    fn piece_attack_scope_reopen_reset() {
        let mut e = fixture();
        let before = serde_json::to_value(&e.project).unwrap();
        e.call("instrument",&json!({"action":"set_kit_piece_attack","params":{"track":"drums","key":38,"attack_ms":40}})).unwrap();
        let mut ex = before.clone();
        ex["tracks"][0]["instrument"]["key_attack_ms"] = json!({"38":40.0});
        assert_eq!(serde_json::to_value(&e.project).unwrap(), ex);
        let h = e.history().0.len();
        e.call(
            "set_kit_piece_attack",
            &json!({"track":"drums","key":38,"attack_ms":40}),
        )
        .unwrap();
        assert_eq!(h, e.history().0.len());
        e.call(
            "save_project",
            &json!({"path":"/tmp/piece-attack-reopen.json"}),
        )
        .unwrap();
        let mut f = fixture();
        f.call(
            "load_project",
            &json!({"path":"/tmp/piece-attack-reopen.json"}),
        )
        .unwrap();
        assert_eq!(serde_json::to_value(&f.project).unwrap(), ex);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        e.call("redo", &json!({})).unwrap();
        e.call(
            "set_kit_piece_attack",
            &json!({"track":"drums","key":38,"attack_ms":0}),
        )
        .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        for a in [
            json!({"track":"piano","key":38,"attack_ms":40}),
            json!({"track":"drums","key":38,"attack_ms":0.2}),
            json!({"track":"drums","key":38,"attack_ms":201}),
            json!({"track":"drums","key":38,"attack_ms":"40"}),
            json!({"track":"drums","key":38.5,"attack_ms":40}),
        ] {
            assert!(e.call("set_kit_piece_attack", &a).is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        }
    }
    #[test]
    fn piece_attack_audio_formula_cache_stream() {
        let mut e = fixture();
        let opts = beatbox::render::RenderOptions {
            step_range: Some((0, 16)),
            tail: 0.,
            keep_stems: true,
            ..Default::default()
        };
        let original = e.project.clone();
        let base = beatbox::render::render(&e.project, &e.bank, &opts).unwrap();
        let mut cache = beatbox::render::TrackCache::default();
        beatbox::render::render_cached(&e.project, &e.bank, &opts, Some(&mut cache)).unwrap();
        e.call(
            "set_kit_piece_attack",
            &json!({"track":"drums","key":38,"attack_ms":40}),
        )
        .unwrap();
        let changed =
            beatbox::render::render_cached(&e.project, &e.bank, &opts, Some(&mut cache)).unwrap();
        let fresh = beatbox::render::render(&e.project, &e.bank, &opts).unwrap();
        assert_eq!(changed.left, fresh.left);
        assert_ne!(base.left, changed.left);
        let mut stream = beatbox::rt::VoiceStreamer::new(&e.project, &e.bank, &opts, 127, 0);
        let (l, r) = stream.render_all();
        assert_eq!(l, changed.stems[0].left);
        assert_eq!(r, changed.stems[0].right);
        let beatbox::instruments::Instrument::Multisample(p) = &original.tracks[0].instrument
        else {
            panic!()
        };
        let mut wide = p.clone();
        wide.key_attack_ms.insert(38, 40.);
        let a = beatbox::multisample::render_multisample(p, 38., 0.8, 0.1, &e.bank, 0);
        let b = beatbox::multisample::render_multisample(&wide, 38., 0.8, 0.1, &e.bank, 0);
        assert_eq!(a.len(), b.len());
        assert!(a.iter().zip(&b).any(|(a, b)| (a - b).abs() > 0.001));
        let start = (0.04 * beatbox::dsp::SR) as usize;
        assert_eq!(&a[start..], &b[start..]);
        let other = beatbox::multisample::render_multisample(p, 36., 0.8, 0.1, &e.bank, 0);
        let other2 = beatbox::multisample::render_multisample(&wide, 36., 0.8, 0.1, &e.bank, 0);
        assert_eq!(other, other2);
        e.call(
            "set_kit_piece_attack",
            &json!({"track":"drums","key":38,"attack_ms":0}),
        )
        .unwrap();
        let reset = beatbox::render::render(&e.project, &e.bank, &opts).unwrap();
        assert_eq!(base.left, reset.left);
        assert_eq!(base.right, reset.right);
    }
}

#[cfg(test)]
mod piece_reverse_tests {
    use crate as beatbox;
    use serde_json::json;
    fn fixture() -> beatbox::Engine {
        let mut e = beatbox::Engine::new("/tmp/reverse-test".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        e.project.master_effects.clear();
        for t in &mut e.project.tracks {
            t.effects.clear();
            t.pan = 0.;
            t.volume_db = 0.;
            t.mute = t.name != "drums";
        }
        e.bank.sync(&e.project.samples);
        e
    }
    #[test]
    fn reverse_scope_reopen_reset() {
        let mut e = fixture();
        let b = serde_json::to_value(&e.project).unwrap();
        e.call("instrument",&json!({"action":"set_kit_piece_reverse","params":{"track":"drums","key":38,"enabled":true}})).unwrap();
        let mut x = b.clone();
        x["tracks"][0]["instrument"]["reverse_keys"] = json!([38]);
        assert_eq!(serde_json::to_value(&e.project).unwrap(), x);
        let h = e.history().0.len();
        e.call(
            "set_kit_piece_reverse",
            &json!({"track":"drums","key":38,"enabled":true}),
        )
        .unwrap();
        assert_eq!(h, e.history().0.len());
        e.call("save_project", &json!({"path":"/tmp/reverse-reopen.json"}))
            .unwrap();
        let mut f = fixture();
        f.call("load_project", &json!({"path":"/tmp/reverse-reopen.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&f.project).unwrap(), x);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("redo", &json!({})).unwrap();
        e.call(
            "set_kit_piece_reverse",
            &json!({"track":"drums","key":38,"enabled":false}),
        )
        .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        for a in [
            json!({"track":"piano","key":38,"enabled":true}),
            json!({"track":"drums","key":38,"enabled":"true"}),
            json!({"track":"drums","key":38.5,"enabled":true}),
            json!({"track":"drums","key":38}),
        ] {
            assert!(e.call("set_kit_piece_reverse", &a).is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        }
    }
    #[test]
    fn reverse_render_cache_stream_other_keys() {
        let mut e = fixture();
        let opts = beatbox::render::RenderOptions {
            step_range: Some((0, 16)),
            tail: 0.,
            keep_stems: true,
            ..Default::default()
        };
        let base = beatbox::render::render(&e.project, &e.bank, &opts).unwrap();
        let mut cache = beatbox::render::TrackCache::default();
        beatbox::render::render_cached(&e.project, &e.bank, &opts, Some(&mut cache)).unwrap();
        let beatbox::instruments::Instrument::Multisample(p) =
            e.project.tracks[0].instrument.clone()
        else {
            panic!()
        };
        let mut rev = p.clone();
        rev.reverse_keys.insert(38);
        let a = beatbox::multisample::render_multisample(&p, 38., 0.8, 0.1, &e.bank, 0);
        let b = beatbox::multisample::render_multisample(&rev, 38., 0.8, 0.1, &e.bank, 0);
        assert_eq!(a.len(), b.len());
        assert_ne!(a, b);
        assert_eq!(
            beatbox::multisample::render_multisample(&p, 36., 0.8, 0.1, &e.bank, 0),
            beatbox::multisample::render_multisample(&rev, 36., 0.8, 0.1, &e.bank, 0)
        );
        e.call(
            "set_kit_piece_reverse",
            &json!({"track":"drums","key":38,"enabled":true}),
        )
        .unwrap();
        let changed =
            beatbox::render::render_cached(&e.project, &e.bank, &opts, Some(&mut cache)).unwrap();
        let fresh = beatbox::render::render(&e.project, &e.bank, &opts).unwrap();
        assert_eq!(changed.left, fresh.left);
        assert_ne!(base.left, changed.left);
        let mut stream = beatbox::rt::VoiceStreamer::new(&e.project, &e.bank, &opts, 127, 0);
        let (l, r) = stream.render_all();
        assert_eq!(l, changed.stems[0].left);
        assert_eq!(r, changed.stems[0].right);
        e.call(
            "set_kit_piece_reverse",
            &json!({"track":"drums","key":38,"enabled":false}),
        )
        .unwrap();
        let reset = beatbox::render::render(&e.project, &e.bank, &opts).unwrap();
        assert_eq!(base.left, reset.left);
        assert_eq!(base.right, reset.right);
    }
    #[test]
    fn reverse_is_source_order_before_cap() {
        let e = fixture();
        let beatbox::instruments::Instrument::Multisample(p) =
            e.project.tracks[0].instrument.clone()
        else {
            panic!()
        };
        let mut reversed_bank = beatbox::samples::SampleBank::default();
        for z in &p.zones {
            let d = e.bank.get(&z.sample).unwrap();
            reversed_bank.insert(&z.sample, d.iter().rev().copied().collect());
        }
        let mut rev = p.clone();
        rev.reverse_keys.insert(38);
        rev.key_length_ms.insert(38, 120.);
        rev.key_tune_cents.insert(38, 300.);
        rev.key_attack_ms.insert(38, 10.);
        let mut expected = rev.clone();
        expected.reverse_keys.clear();
        let a = beatbox::multisample::render_multisample(&rev, 38., 0.8, 0.1, &e.bank, 1);
        let b =
            beatbox::multisample::render_multisample(&expected, 38., 0.8, 0.1, &reversed_bank, 1);
        assert_eq!(a, b);
        assert_eq!(a.len(), (beatbox::dsp::SR * 0.12) as usize);
    }
}

#[cfg(test)]
mod piece_decay_tests {
    use serde_json::json;
    fn fixture() -> crate::Engine {
        let mut e = crate::Engine::new("/tmp/decay-test".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        e.project.master_effects.clear();
        for t in &mut e.project.tracks {
            t.effects.clear();
            t.pan = 0.;
            t.volume_db = 0.;
            t.mute = t.name != "drums";
        }
        e.bank.sync(&e.project.samples);
        e
    }
    #[test]
    fn decay_scope_reopen_reset() {
        let mut e = fixture();
        let b = serde_json::to_value(&e.project).unwrap();
        e.call("instrument",&json!({"action":"set_kit_piece_decay","params":{"track":"drums","key":38,"half_life_ms":80}})).unwrap();
        let mut x = b.clone();
        x["tracks"][0]["instrument"]["key_decay_ms"] = json!({"38":80.0});
        assert_eq!(serde_json::to_value(&e.project).unwrap(), x);
        let h = e.history().0.len();
        e.call(
            "set_kit_piece_decay",
            &json!({"track":"drums","key":38,"half_life_ms":80}),
        )
        .unwrap();
        assert_eq!(h, e.history().0.len());
        e.call("save_project", &json!({"path":"/tmp/decay-reopen.json"}))
            .unwrap();
        let mut f = fixture();
        f.call("load_project", &json!({"path":"/tmp/decay-reopen.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&f.project).unwrap(), x);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("redo", &json!({})).unwrap();
        e.call(
            "set_kit_piece_decay",
            &json!({"track":"drums","key":38,"half_life_ms":0}),
        )
        .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        for a in [
            json!({"track":"piano","key":38,"half_life_ms":80}),
            json!({"track":"drums","key":38,"half_life_ms":5}),
            json!({"track":"drums","key":38,"half_life_ms":2001}),
            json!({"track":"drums","key":38.5,"half_life_ms":80}),
            json!({"track":"drums","key":38,"half_life_ms":"80"}),
        ] {
            assert!(e.call("set_kit_piece_decay", &a).is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        }
    }
    #[test]
    fn decay_formula_otherkeys_cache_stream() {
        let mut e = fixture();
        let crate::instruments::Instrument::Multisample(p) = e.project.tracks[0].instrument.clone()
        else {
            panic!()
        };
        let mut d = p.clone();
        d.key_decay_ms.insert(38, 80.);
        let a = crate::multisample::render_multisample(&p, 38., 0.8, 0.1, &e.bank, 0);
        let b = crate::multisample::render_multisample(&d, 38., 0.8, 0.1, &e.bank, 0);
        assert_eq!(a.len(), b.len());
        assert_ne!(a, b);
        for (i, (x, y)) in a.iter().zip(&b).enumerate() {
            let expected = x * 2f32.powf(-(i as f32) / (0.08 * crate::dsp::SR));
            assert!((expected - y).abs() < 2e-7, "{} {} {}", i, expected, y);
            assert!(y.abs() <= x.abs() + 1e-7);
        }
        assert_eq!(
            crate::multisample::render_multisample(&p, 36., 0.8, 0.1, &e.bank, 0),
            crate::multisample::render_multisample(&d, 36., 0.8, 0.1, &e.bank, 0)
        );
        let opts = crate::render::RenderOptions {
            step_range: Some((0, 16)),
            tail: 0.,
            keep_stems: true,
            ..Default::default()
        };
        let base = crate::render::render(&e.project, &e.bank, &opts).unwrap();
        let mut cache = crate::render::TrackCache::default();
        crate::render::render_cached(&e.project, &e.bank, &opts, Some(&mut cache)).unwrap();
        e.call(
            "set_kit_piece_decay",
            &json!({"track":"drums","key":38,"half_life_ms":80}),
        )
        .unwrap();
        let changed =
            crate::render::render_cached(&e.project, &e.bank, &opts, Some(&mut cache)).unwrap();
        let fresh = crate::render::render(&e.project, &e.bank, &opts).unwrap();
        assert_eq!(changed.left, fresh.left);
        assert_ne!(base.left, changed.left);
        let mut stream = crate::rt::VoiceStreamer::new(&e.project, &e.bank, &opts, 127, 0);
        let (l, r) = stream.render_all();
        assert_eq!(l, changed.stems[0].left);
        assert_eq!(r, changed.stems[0].right);
        e.call(
            "set_kit_piece_decay",
            &json!({"track":"drums","key":38,"half_life_ms":0}),
        )
        .unwrap();
        let reset = crate::render::render(&e.project, &e.bank, &opts).unwrap();
        assert_eq!(base.left, reset.left);
    }
}

#[cfg(test)]
mod piece_lowpass_tests {
    use serde_json::json;
    fn fixture() -> crate::Engine {
        let mut e = crate::Engine::new("/tmp/piece_lowpass-test".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        e.project.master_effects.clear();
        for t in &mut e.project.tracks {
            t.effects.clear();
            t.pan = 0.;
            t.volume_db = 0.;
            t.mute = t.name != "drums";
        }
        e.bank.sync(&e.project.samples);
        e
    }
    #[test]
    fn piece_lowpass_scope_reopen_reset() {
        let mut e = fixture();
        let b = serde_json::to_value(&e.project).unwrap();
        e.call("instrument",&json!({"action":"set_kit_piece_lowpass","params":{"track":"drums","key":38,"cutoff_hz":2000}})).unwrap();
        let mut x = b.clone();
        x["tracks"][0]["instrument"]["key_lowpass_hz"] = json!({"38":2000.0});
        assert_eq!(serde_json::to_value(&e.project).unwrap(), x);
        let h = e.history().0.len();
        e.call(
            "set_kit_piece_lowpass",
            &json!({"track":"drums","key":38,"cutoff_hz":2000}),
        )
        .unwrap();
        assert_eq!(h, e.history().0.len());
        e.call(
            "save_project",
            &json!({"path":"/tmp/piece_lowpass-reopen.json"}),
        )
        .unwrap();
        let mut f = fixture();
        f.call(
            "load_project",
            &json!({"path":"/tmp/piece_lowpass-reopen.json"}),
        )
        .unwrap();
        assert_eq!(serde_json::to_value(&f.project).unwrap(), x);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("redo", &json!({})).unwrap();
        e.call(
            "set_kit_piece_lowpass",
            &json!({"track":"drums","key":38,"cutoff_hz":0}),
        )
        .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        for a in [
            json!({"track":"piano","key":38,"cutoff_hz":2000}),
            json!({"track":"drums","key":38,"cutoff_hz":5}),
            json!({"track":"drums","key":38,"cutoff_hz":18001}),
            json!({"track":"drums","key":38.5,"cutoff_hz":2000}),
            json!({"track":"drums","key":38,"cutoff_hz":"2000"}),
        ] {
            assert!(e.call("set_kit_piece_lowpass", &a).is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        }
    }
    #[test]
    fn lowpass_reference_and_scope() {
        let e = fixture();
        let mut bank = crate::samples::SampleBank::default();
        let data: Vec<f32> = (0..4410)
            .map(|i| if i % 2 == 0 { 0.8 } else { -0.8 })
            .collect();
        bank.insert("tone", data.clone());
        let mut p = crate::multisample::MultisampleParams {
            zones: vec![crate::multisample::Zone {
                sample: "tone".into(),
                root: 38,
                lo_note: 38,
                hi_note: 38,
                vel_lo: 0.,
                vel_hi: 1.,
                tune_cents: 0.,
                gain_db: 0.,
                rr: 0,
            }],
            one_shot: true,
            gain: 1.,
            vel_to_gain: 0.,
            vel_crossfade: false,
            ..Default::default()
        };
        let dry = crate::multisample::render_multisample(&p, 38., 1., 0.1, &bank, 0);
        p.key_lowpass_hz.insert(38, 2000.);
        let wet = crate::multisample::render_multisample(&p, 38., 1., 0.1, &bank, 0);
        let raw = crate::resample::pitch_read(&data, 1., dry.len());
        let alpha = 1. - (-2. * std::f32::consts::PI * 2000. / crate::dsp::SR).exp();
        let (mut l1, mut l2) = (0f32, 0f32);
        for (i, (&x, &y)) in raw.iter().zip(&wet).enumerate() {
            l1 += alpha * (x - l1);
            l2 += alpha * (l1 - l2);
            let a = (i as f32 / (p.attack * crate::dsp::SR)).min(1.);
            let tail = ((wet.len() - i) as f32 / 64.).min(1.);
            assert!((y - l2 * a * tail).abs() < 2e-7);
        }
        let energy = |x: &[f32]| x.iter().map(|v| v * v).sum::<f32>();
        assert!(energy(&wet) < energy(&dry) * 0.02);
        let crate::instruments::Instrument::Multisample(real) =
            e.project.tracks[0].instrument.clone()
        else {
            panic!()
        };
        let mut lp = real.clone();
        lp.key_lowpass_hz.insert(38, 2000.);
        assert_eq!(
            crate::multisample::render_multisample(&real, 36., 0.8, 0.1, &e.bank, 0),
            crate::multisample::render_multisample(&lp, 36., 0.8, 0.1, &e.bank, 0)
        );
    }
    #[test]
    fn lowpass_cache_stream_reset() {
        let mut e = fixture();
        let opts = crate::render::RenderOptions {
            step_range: Some((0, 16)),
            tail: 0.,
            keep_stems: true,
            ..Default::default()
        };
        let base = crate::render::render(&e.project, &e.bank, &opts).unwrap();
        let mut cache = crate::render::TrackCache::default();
        crate::render::render_cached(&e.project, &e.bank, &opts, Some(&mut cache)).unwrap();
        e.call(
            "set_kit_piece_lowpass",
            &json!({"track":"drums","key":38,"cutoff_hz":2000}),
        )
        .unwrap();
        let c = crate::render::render_cached(&e.project, &e.bank, &opts, Some(&mut cache)).unwrap();
        let f = crate::render::render(&e.project, &e.bank, &opts).unwrap();
        assert_eq!(c.left, f.left);
        assert_ne!(c.left, base.left);
        let mut stream = crate::rt::VoiceStreamer::new(&e.project, &e.bank, &opts, 127, 0);
        let (l, r) = stream.render_all();
        assert_eq!(l, c.stems[0].left);
        assert_eq!(r, c.stems[0].right);
        e.call(
            "set_kit_piece_lowpass",
            &json!({"track":"drums","key":38,"cutoff_hz":0}),
        )
        .unwrap();
        let reset = crate::render::render(&e.project, &e.bank, &opts).unwrap();
        assert_eq!(base.left, reset.left);
        assert_eq!(base.right, reset.right);
    }
}

#[cfg(test)]
mod piece_highpass_tests {
    use serde_json::json;
    fn fixture() -> crate::Engine {
        let mut e = crate::Engine::new("/tmp/piece_highpass-test".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        e.project.master_effects.clear();
        for t in &mut e.project.tracks {
            t.effects.clear();
            t.pan = 0.;
            t.volume_db = 0.;
            t.mute = t.name != "drums";
        }
        e.bank.sync(&e.project.samples);
        e
    }
    #[test]
    fn piece_highpass_scope_reopen_reset() {
        let mut e = fixture();
        let b = serde_json::to_value(&e.project).unwrap();
        e.call("instrument",&json!({"action":"set_kit_piece_highpass","params":{"track":"drums","key":38,"cutoff_hz":2000}})).unwrap();
        let mut x = b.clone();
        x["tracks"][0]["instrument"]["key_highpass_hz"] = json!({"38":2000.0});
        assert_eq!(serde_json::to_value(&e.project).unwrap(), x);
        let h = e.history().0.len();
        e.call(
            "set_kit_piece_highpass",
            &json!({"track":"drums","key":38,"cutoff_hz":2000}),
        )
        .unwrap();
        assert_eq!(h, e.history().0.len());
        e.call(
            "save_project",
            &json!({"path":"/tmp/piece_highpass-reopen.json"}),
        )
        .unwrap();
        let mut f = fixture();
        f.call(
            "load_project",
            &json!({"path":"/tmp/piece_highpass-reopen.json"}),
        )
        .unwrap();
        assert_eq!(serde_json::to_value(&f.project).unwrap(), x);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("redo", &json!({})).unwrap();
        e.call(
            "set_kit_piece_highpass",
            &json!({"track":"drums","key":38,"cutoff_hz":0}),
        )
        .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        for a in [
            json!({"track":"piano","key":38,"cutoff_hz":2000}),
            json!({"track":"drums","key":38,"cutoff_hz":5}),
            json!({"track":"drums","key":38,"cutoff_hz":4001}),
            json!({"track":"drums","key":38.5,"cutoff_hz":2000}),
            json!({"track":"drums","key":38,"cutoff_hz":"2000"}),
        ] {
            assert!(e.call("set_kit_piece_highpass", &a).is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        }
    }
    #[test]
    fn highpass_reference_and_scope() {
        let e = fixture();
        let mut bank = crate::samples::SampleBank::default();
        let data: Vec<f32> = (0..4410)
            .map(|i| 0.8 + (i as f32 * 0.003).sin() * 0.1)
            .collect();
        bank.insert("tone", data.clone());
        let mut p = crate::multisample::MultisampleParams {
            zones: vec![crate::multisample::Zone {
                sample: "tone".into(),
                root: 38,
                lo_note: 38,
                hi_note: 38,
                vel_lo: 0.,
                vel_hi: 1.,
                tune_cents: 0.,
                gain_db: 0.,
                rr: 0,
            }],
            one_shot: true,
            gain: 1.,
            vel_to_gain: 0.,
            vel_crossfade: false,
            ..Default::default()
        };
        let dry = crate::multisample::render_multisample(&p, 38., 1., 0.1, &bank, 0);
        p.key_highpass_hz.insert(38, 2000.);
        let wet = crate::multisample::render_multisample(&p, 38., 1., 0.1, &bank, 0);
        let raw = crate::resample::pitch_read(&data, 1., dry.len());
        let pole = (-2. * std::f32::consts::PI * 2000. / crate::dsp::SR).exp();
        let gain = (1. + pole) * 0.5;
        let (mut x1, mut y1, mut x2, mut y2) = (0f32, 0f32, 0f32, 0f32);
        for (i, (&x, &y)) in raw.iter().zip(&wet).enumerate() {
            let first = gain * (x - x1) + pole * y1;
            x1 = x;
            y1 = first;
            let second = gain * (first - x2) + pole * y2;
            x2 = first;
            y2 = second;
            let a = (i as f32 / (p.attack * crate::dsp::SR)).min(1.);
            let tail = ((wet.len() - i) as f32 / 64.).min(1.);
            assert!((y - second * a * tail).abs() < 2e-7);
        }
        let energy = |x: &[f32]| x.iter().map(|v| v * v).sum::<f32>();
        assert!(energy(&wet) < energy(&dry) * 0.02);
        let crate::instruments::Instrument::Multisample(real) =
            e.project.tracks[0].instrument.clone()
        else {
            panic!()
        };
        let mut lp = real.clone();
        lp.key_highpass_hz.insert(38, 2000.);
        assert_eq!(
            crate::multisample::render_multisample(&real, 36., 0.8, 0.1, &e.bank, 0),
            crate::multisample::render_multisample(&lp, 36., 0.8, 0.1, &e.bank, 0)
        );
    }
    #[test]
    fn highpass_cache_stream_reset() {
        let mut e = fixture();
        let opts = crate::render::RenderOptions {
            step_range: Some((0, 16)),
            tail: 0.,
            keep_stems: true,
            ..Default::default()
        };
        let base = crate::render::render(&e.project, &e.bank, &opts).unwrap();
        let mut cache = crate::render::TrackCache::default();
        crate::render::render_cached(&e.project, &e.bank, &opts, Some(&mut cache)).unwrap();
        e.call(
            "set_kit_piece_highpass",
            &json!({"track":"drums","key":38,"cutoff_hz":2000}),
        )
        .unwrap();
        let c = crate::render::render_cached(&e.project, &e.bank, &opts, Some(&mut cache)).unwrap();
        let f = crate::render::render(&e.project, &e.bank, &opts).unwrap();
        assert_eq!(c.left, f.left);
        assert_ne!(c.left, base.left);
        let mut stream = crate::rt::VoiceStreamer::new(&e.project, &e.bank, &opts, 127, 0);
        let (l, r) = stream.render_all();
        assert_eq!(l, c.stems[0].left);
        assert_eq!(r, c.stems[0].right);
        e.call(
            "set_kit_piece_highpass",
            &json!({"track":"drums","key":38,"cutoff_hz":0}),
        )
        .unwrap();
        let reset = crate::render::render(&e.project, &e.bank, &opts).unwrap();
        assert_eq!(base.left, reset.left);
        assert_eq!(base.right, reset.right);
    }
}

#[cfg(test)]
mod piece_response_tests {
    use serde_json::json;
    fn fixture() -> crate::Engine {
        let mut e = crate::Engine::new("/tmp/piece_response-test".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        e.project.master_effects.clear();
        for t in &mut e.project.tracks {
            t.effects.clear();
            t.pan = 0.;
            t.volume_db = 0.;
            t.mute = t.name != "drums";
        }
        e.bank.sync(&e.project.samples);
        e
    }
    #[test]
    fn piece_response_scope_reopen_reset() {
        let mut e = fixture();
        let b = serde_json::to_value(&e.project).unwrap();
        e.call("instrument",&json!({"action":"set_kit_piece_velocity_curve","params":{"track":"drums","key":38,"curve":0.5}})).unwrap();
        let mut x = b.clone();
        x["tracks"][0]["instrument"]["key_velocity_curve"] = json!({"38":0.5});
        assert_eq!(serde_json::to_value(&e.project).unwrap(), x);
        let h = e.history().0.len();
        e.call(
            "set_kit_piece_velocity_curve",
            &json!({"track":"drums","key":38,"curve":0.5}),
        )
        .unwrap();
        assert_eq!(h, e.history().0.len());
        e.call(
            "save_project",
            &json!({"path":"/tmp/piece_response-reopen.json"}),
        )
        .unwrap();
        let mut f = fixture();
        f.call(
            "load_project",
            &json!({"path":"/tmp/piece_response-reopen.json"}),
        )
        .unwrap();
        assert_eq!(serde_json::to_value(&f.project).unwrap(), x);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("redo", &json!({})).unwrap();
        e.call(
            "set_kit_piece_velocity_curve",
            &json!({"track":"drums","key":38,"curve":0}),
        )
        .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        for a in [
            json!({"track":"piano","key":38,"curve":0.5}),
            json!({"track":"drums","key":38,"curve":0.1}),
            json!({"track":"drums","key":38,"curve":4.1}),
            json!({"track":"drums","key":38.5,"curve":0.5}),
            json!({"track":"drums","key":38,"curve":"0.5"}),
        ] {
            assert!(e.call("set_kit_piece_velocity_curve", &a).is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        }
    }
    #[test]
    fn response_cache_stream_reset() {
        let mut e = fixture();
        let opts = crate::render::RenderOptions {
            step_range: Some((0, 16)),
            tail: 0.,
            keep_stems: true,
            ..Default::default()
        };
        let base = crate::render::render(&e.project, &e.bank, &opts).unwrap();
        let mut cache = crate::render::TrackCache::default();
        crate::render::render_cached(&e.project, &e.bank, &opts, Some(&mut cache)).unwrap();
        e.call(
            "set_kit_piece_velocity_curve",
            &json!({"track":"drums","key":38,"curve":0.5}),
        )
        .unwrap();
        let c = crate::render::render_cached(&e.project, &e.bank, &opts, Some(&mut cache)).unwrap();
        let f = crate::render::render(&e.project, &e.bank, &opts).unwrap();
        assert_eq!(c.left, f.left);
        assert_ne!(c.left, base.left);
        let mut stream = crate::rt::VoiceStreamer::new(&e.project, &e.bank, &opts, 127, 0);
        let (l, r) = stream.render_all();
        assert_eq!(l, c.stems[0].left);
        assert_eq!(r, c.stems[0].right);
        e.call(
            "set_kit_piece_velocity_curve",
            &json!({"track":"drums","key":38,"curve":0}),
        )
        .unwrap();
        let reset = crate::render::render(&e.project, &e.bank, &opts).unwrap();
        assert_eq!(base.left, reset.left);
        assert_eq!(base.right, reset.right);
    }

    #[test]
    fn response_layers_replace_not_multiply() {
        use crate::multisample::*;
        let mut p = MultisampleParams {
            velocity_curve: 2.,
            vel_crossfade: false,
            zones: vec![
                Zone {
                    sample: "soft".into(),
                    root: 38,
                    lo_note: 38,
                    hi_note: 38,
                    vel_lo: 0.,
                    vel_hi: 0.5,
                    tune_cents: 0.,
                    gain_db: 0.,
                    rr: 0,
                },
                Zone {
                    sample: "hard".into(),
                    root: 38,
                    lo_note: 38,
                    hi_note: 38,
                    vel_lo: 0.50001,
                    vel_hi: 1.,
                    tune_cents: 0.,
                    gain_db: 0.,
                    rr: 0,
                },
            ],
            ..Default::default()
        };
        assert!((response_velocity_for_key(&p, 38, 0.5) - 0.25).abs() < 1e-7);
        p.key_velocity_curve.insert(38, 0.5);
        assert!((response_velocity_for_key(&p, 38, 0.5) - 0.5f32.sqrt()).abs() < 1e-7);
        assert_eq!(pick_zones(&p, 38, 0.5, 0)[0].0, 1);
        assert_eq!(response_velocity_for_key(&p, 36, 0.5), 0.25);
        let mut bank = crate::samples::SampleBank::default();
        bank.insert("soft", vec![0.2; 441]);
        bank.insert("hard", vec![0.8; 441]);
        let overridden = render_multisample(&p, 38., 0.5, 0.1, &bank, 0);
        p.key_velocity_curve.clear();
        p.velocity_curve = 0.5;
        assert_eq!(overridden, render_multisample(&p, 38., 0.5, 0.1, &bank, 0));
        p.velocity_curve = 2.;
        p.key_velocity_curve.insert(38, 1.);
        assert_eq!(response_velocity_for_key(&p, 38, 0.5), 0.5);
        p.key_velocity_curve.remove(&38);
        assert_eq!(response_velocity_for_key(&p, 38, 0.5), 0.25);
    }
}

#[cfg(test)]
mod translation_excerpt_tests {
    use crate as beatbox;
    use beatbox::Engine;
    use serde_json::json;
    fn fixture(tag: &str) -> (Engine, std::path::PathBuf, Vec<f32>, Vec<f32>) {
        let dir = std::env::temp_dir().join(format!("bb229_{tag}_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("source.wav");
        let l: Vec<f32> = (0..3 * 44100)
            .map(|i| {
                if i < 44100 {
                    0.0
                } else {
                    0.25 * (std::f32::consts::TAU * 1000.0 * i as f32 / 44100.0).sin()
                }
            })
            .collect();
        let r: Vec<f32> = l.iter().map(|x| -x).collect();
        beatbox::render::write_wav(&path, &l, &r).unwrap();
        (Engine::new(dir), path, l, r)
    }
    #[test]
    fn stereo_excerpt_matches_actual_later_file_samples() {
        let (mut e, p, _, _) = fixture("exact");
        let (l, r) = beatbox::samples::decode_stereo(&p).unwrap();
        let before = serde_json::to_value(&e.project).unwrap();
        let rev = e.revision;
        let undo = e.undo_depth();
        let v = e
            .call(
                "preview_mix_translation",
                &json!({"mode":"stereo","path":p,"start_s":1.25,"length_s":1.0,"play":true}),
            )
            .unwrap();
        let q = e
            .transport
            .pending_audition
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap();
        let a = (1.25 * 44100.0) as usize;
        assert_eq!(q.0, l[a..a + 44100]);
        assert_eq!(q.1, r[a..a + 44100]);
        assert_eq!(v["truncated"], false);
        assert_eq!(v["safety_gain_db"], 0.0);
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        assert_eq!(e.revision, rev);
        assert_eq!(e.undo_depth(), undo);
    }
    #[test]
    fn later_mono_excerpt_keeps_cancellation_and_phone_loses_bass() {
        let (mut e, p, _, _) = fixture("fold");
        e.call(
            "preview_mix_translation",
            &json!({"mode":"mono","path":p,"start_s":1.0,"length_s":1.0,"play":true}),
        )
        .unwrap();
        let q = e
            .transport
            .pending_audition
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap();
        assert!(q.0.iter().chain(&q.1).all(|v| v.abs() < 1e-4));
        let x: Vec<f32> = (0..3 * 44100)
            .map(|i| 0.3 * (std::f32::consts::TAU * 40.0 * i as f32 / 44100.0).sin())
            .collect();
        beatbox::render::write_wav(&p, &x, &x).unwrap();
        let v = e
            .call(
                "preview_mix_translation",
                &json!({"mode":"phone","path":p,"start_s":1.0,"length_s":1.0,"play":true}),
            )
            .unwrap();
        let q = e
            .transport
            .pending_audition
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap();
        let rms = (q.0.iter().map(|v| v * v).sum::<f32>() / q.0.len() as f32).sqrt();
        assert!(rms < 0.03);
        assert_eq!(q.0, q.1);
        assert!(v["safety_gain_db"].as_f64().unwrap() <= 0.0);
    }
    #[test]
    fn shortened_end_is_explicit_and_invalid_inputs_do_not_queue_or_write() {
        let (mut e, p, _, _) = fixture("bounds");
        let v = e
            .call(
                "preview_mix_translation",
                &json!({"path":p,"start_s":2.5,"length_s":2.0,"play":false}),
            )
            .unwrap();
        assert_eq!(v["truncated"], true);
        assert!((v["seconds"].as_f64().unwrap() - 0.5).abs() < 1e-6);
        let count = std::fs::read_dir(e.renders_dir()).unwrap().count();
        for bad in [
            json!({"start_s":3.0}),
            json!({"start_s":-1}),
            json!({"start_s":"1"}),
            json!({"length_s":0}),
            json!({"length_s":21}),
            json!({"start_s":1,"bars":4}),
        ] {
            let mut a = bad;
            a["path"] = json!(p);
            assert!(e.call("preview_mix_translation", &a).is_err());
            assert!(e.transport.pending_audition.is_none());
            assert_eq!(std::fs::read_dir(e.renders_dir()).unwrap().count(), count);
        }
    }
    #[test]
    fn excerpt_safety_only_attenuates_and_saves_unique_files() {
        let (mut e, p, _, _) = fixture("safety");
        let x = vec![2.0; 3 * 44100];
        crate::render::write_wav(&p, &x, &x).unwrap();
        let a = e
            .call(
                "preview_mix_translation",
                &json!({"mode":"stereo","path":p,"start_s":1.0,"length_s":1.0,"play":true}),
            )
            .unwrap();
        assert!(a["safety_gain_db"].as_f64().unwrap() < 0.0);
        let q = e
            .transport
            .pending_audition
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap();
        assert!(q.0.iter().all(|x| x.abs() <= 0.95001));
        let b = e
            .call(
                "preview_mix_translation",
                &json!({"mode":"stereo","path":p,"start_s":1.0,"length_s":1.0,"play":false}),
            )
            .unwrap();
        assert_ne!(a["path"], b["path"]);
    }
    #[test]
    fn project_excerpt_matches_full_render_with_preceding_note_and_tempo_curve() {
        let (mut e, _, _, _) = fixture("project");
        e.call("new_project", &json!({"name":"carry","bpm":120}))
            .unwrap();
        e.call("add_track", &json!({"name":"tone","instrument":"sine"}))
            .unwrap();
        e.call("add_notes",&json!({"track":"tone","pattern":"A","notes":[{"pitch":72,"start":0,"len":32,"vel":0.5}]})).unwrap();
        let mut inst = serde_json::to_value(&e.project.tracks[0].instrument).unwrap();
        inst["amp_env"]["sustain"] = json!(1.0);
        e.project.tracks[0].instrument = serde_json::from_value(inst).unwrap();
        e.call(
            "add_effect",
            &json!({"track":"tone","type":"delay","params":{"steps":3.0,"feedback":0.3,"mix":0.3}}),
        )
        .unwrap();
        e.project.tempo_points = vec![beatbox::project::TempoPoint {
            beat: 4.0,
            bpm: 90.0,
            ramp: true,
        }];
        let m = beatbox::render::render(&e.project, &e.bank, &Default::default()).unwrap();
        let v = e
            .call(
                "preview_mix_translation",
                &json!({"mode":"stereo","start_s":1.25,"length_s":1.0,"play":true}),
            )
            .unwrap();
        let q = e
            .transport
            .pending_audition
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap();
        let a = (1.25 * 44100.0) as usize;
        let g = 10f32.powf(v["safety_gain_db"].as_f64().unwrap() as f32 / 20.0);
        assert_eq!(q.0.len(), 44100);
        assert!(q
            .0
            .iter()
            .zip(&m.left[a..a + 44100])
            .all(|(x, y)| (x - y * g).abs() < 1e-6));
        assert!(q.0.iter().any(|x| x.abs() > 0.01));
    }
}
