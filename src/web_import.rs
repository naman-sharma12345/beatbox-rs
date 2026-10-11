//! Open web import: pull any audio, SoundFont, SFZ or archive from a URL
//! into the library and load it; scrape a page for audio links; keep a
//! sources manifest and print the credits a project needs.
//!
//! Safety: https only (plain http just for localhost), a size cap, request
//! timeouts, archive entries confined to the import folder (no absolute
//! paths, no `..`), nothing from an archive is ever executed or marked
//! executable, and the library's LRU limit applies.

use crate::tools::{f_opt, obj, s_opt, s_req, Tool};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::io::Read;
use std::path::{Path, PathBuf};

const AUDIO: &[&str] = &["wav", "wave", "aif", "aiff", "flac", "mp3", "ogg", "oga"];

fn slug(s: &str) -> String {
    let t: String = s.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    let t = t.split('_').filter(|x| !x.is_empty()).collect::<Vec<_>>().join("_");
    t.chars().take(48).collect()
}

fn local_url(url: &str) -> bool {
    url.starts_with("http://127.0.0.1") || url.starts_with("http://localhost")
}

fn check_url(url: &str) -> Result<()> {
    let local = local_url(url);
    if !(url.starts_with("https://") || local) {
        bail!("only https:// URLs are imported");
    }
    Ok(())
}

/// GET with a byte cap and timeouts; returns (bytes, final url).
pub fn download(url: &str, max_mb: u64) -> Result<(Vec<u8>, String)> {
    check_url(url)?;
    let agent = ureq::AgentBuilder::new().redirects(8).timeout_connect(std::time::Duration::from_secs(20)).timeout_read(std::time::Duration::from_secs(120)).build();
    let resp = agent.get(url).set("User-Agent", "beatbox-rs (music tool)").call().map_err(|e| anyhow!("download {url}: {e}"))?;
    let final_url = resp.get_url().to_string();
    let cap = max_mb * 1_000_000;
    if let Some(len) = resp.header("Content-Length").and_then(|v| v.parse::<u64>().ok()) {
        if len > cap {
            bail!("{url} is {:.0} MB, over max_mb {max_mb}", len as f64 / 1e6);
        }
        if !local_url(url) {
            crate::sound_catalog::ensure_free(len * 3)?;
        }
    }
    let mut buf = Vec::new();
    resp.into_reader().take(cap + 1).read_to_end(&mut buf)?;
    if buf.len() as u64 > cap {
        bail!("{url} is over max_mb {max_mb}");
    }
    Ok((buf, final_url))
}

/// What the bytes are, from magic numbers first, then the URL's extension.
pub fn sniff(b: &[u8], url: &str) -> &'static str {
    let ext = url.split(['?', '#']).next().unwrap_or("").rsplit('.').next().unwrap_or("").to_lowercase();
    if b.len() >= 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WAVE" {
        "wav"
    } else if b.len() >= 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"sfbk" {
        "sf2"
    } else if b.len() >= 12 && &b[0..4] == b"FORM" && (&b[8..12] == b"AIFF" || &b[8..12] == b"AIFC") {
        "aiff"
    } else if b.starts_with(b"fLaC") {
        "flac"
    } else if b.starts_with(b"OggS") {
        "ogg"
    } else if b.starts_with(b"ID3") || (b.len() > 1 && b[0] == 0xFF && b[1] & 0xE0 == 0xE0) {
        "mp3"
    } else if b.starts_with(b"PK\x03\x04") {
        "zip"
    } else if b.starts_with(&[0x1f, 0x8b]) {
        "tar.gz"
    } else if b.starts_with(&[0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C]) {
        "7z"
    } else if ext == "sfz" || String::from_utf8_lossy(&b[..b.len().min(4000)]).contains("<region>") {
        "sfz"
    } else if ext == "sf3" {
        "sf3"
    } else {
        "unknown"
    }
}

fn safe_join(root: &Path, name: &str) -> Option<PathBuf> {
    let n = name.replace('\\', "/");
    if n.starts_with('/') || n.contains(':') {
        return None;
    }
    let mut p = root.to_path_buf();
    for part in n.split('/') {
        match part {
            "" | "." => {}
            ".." => return None,
            x => p.push(x),
        }
    }
    Some(p)
}

/// Unpack a zip or tar.gz into `dir`, confined to it, with an uncompressed
/// size cap. Returns the files written.
pub fn unpack(bytes: &[u8], kind: &str, dir: &Path, cap: u64) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut total = 0u64;
    let mut write = |name: &str, r: &mut dyn Read, out: &mut Vec<PathBuf>| -> Result<()> {
        let Some(p) = safe_join(dir, name) else { return Ok(()) };
        std::fs::create_dir_all(p.parent().unwrap())?;
        let mut f = std::fs::File::create(&p)?;
        let n = std::io::copy(&mut r.take(cap.saturating_sub(total) + 1), &mut f)?;
        total += n;
        if total > cap {
            bail!("archive unpacks to more than {} MB", cap / 1_000_000);
        }
        out.push(p);
        Ok(())
    };
    match kind {
        "zip" => {
            let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).context("reading zip")?;
            if z.len() > 20_000 {
                bail!("archive has {} entries (limit 20000)", z.len());
            }
            for i in 0..z.len() {
                let mut f = z.by_index(i)?;
                if f.is_dir() {
                    continue;
                }
                let name = f.name().to_string();
                write(&name, &mut f, &mut out)?;
            }
        }
        "tar.gz" => {
            let gz = flate2::read::GzDecoder::new(bytes);
            let mut ar = tar::Archive::new(gz);
            for ent in ar.entries()? {
                let mut ent = ent?;
                if !ent.header().entry_type().is_file() {
                    continue;
                }
                let name = ent.path()?.to_string_lossy().to_string();
                write(&name, &mut ent, &mut out)?;
            }
        }
        k => bail!("{k} archives are not supported (zip and tar.gz are)"),
    }
    Ok(out)
}

/// Licence words found in LICENSE / README / *.txt files of an import.
pub fn detect_license(files: &[PathBuf]) -> Option<String> {
    let mut found = Vec::new();
    for f in files {
        let n = f.file_name().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
        if !(n.contains("licen") || n.contains("readme") || n.contains("copying") || n.ends_with(".txt") || n.ends_with(".md")) {
            continue;
        }
        let Ok(t) = std::fs::read_to_string(f) else { continue };
        let t = t.to_lowercase();
        for (k, label) in [("non-commercial", "NON-COMMERCIAL (check before use)"), ("noncommercial", "NON-COMMERCIAL (check before use)"), ("cc0", "CC0"), ("public domain", "public domain"), ("creative commons attribution", "CC BY"), ("cc-by", "CC BY"), ("cc by", "CC BY"), ("mit license", "MIT"), ("permission is hereby granted", "MIT-style"), ("gnu general public", "GPL")] {
            if t.contains(k) && !found.contains(&label) {
                found.push(label);
            }
        }
    }
    if found.contains(&"CC0") {
        found.retain(|x| *x != "public domain");
    }
    (!found.is_empty()).then(|| found.join(", "))
}

fn manifest_path() -> PathBuf {
    crate::soundfont::library_dir().join("sources.json")
}

pub fn manifest() -> Vec<Value> {
    std::fs::read_to_string(manifest_path()).ok().and_then(|s| serde_json::from_str::<Vec<Value>>(&s).ok()).unwrap_or_default()
}

fn record(v: Value) -> Result<()> {
    let mut m = manifest();
    let slug = v["slug"].clone();
    m.retain(|x| x["slug"] != slug);
    m.push(v);
    std::fs::create_dir_all(crate::soundfont::library_dir())?;
    std::fs::write(manifest_path(), serde_json::to_string_pretty(&m)?)?;
    Ok(())
}

fn drum_role(name: &str) -> Option<&'static str> {
    let n = name.to_lowercase();
    let has = |k: &[&str]| k.iter().any(|w| n.contains(w));
    Some(if has(&["kick", "bd_", "bassdrum", "bass drum"]) {
        "kick"
    } else if has(&["snare", "sd_", "snr"]) {
        "snare"
    } else if has(&["clap"]) {
        "clap"
    } else if has(&["open"]) && has(&["hat", "hh"]) {
        "open_hat"
    } else if has(&["hat", "hh_", "hihat"]) {
        "closed_hat"
    } else if has(&["rim", "stick"]) {
        "rim"
    } else if has(&["tom"]) {
        "tom"
    } else if has(&["crash", "ride", "cymbal"]) {
        "cymbal"
    } else if has(&["shaker", "tamb", "perc", "conga", "bongo", "cowbell"]) {
        "perc"
    } else {
        return None;
    })
}

fn register_file(e: &mut crate::engine::Engine, path: &Path, name: &str, license: &str, source: &str) -> Result<String> {
    let info = crate::samples::SampleInfo { name: crate::samples::sample_name(name), path: path.to_string_lossy().into(), source: source.into(), license: license.into(), author: String::new(), duration: 0.0 };
    let r = crate::tools::register_sample(e, info)?;
    Ok(r["sample"].as_str().unwrap_or(name).to_string())
}

/// Load whatever an import produced. Returns a JSON description.
/// Put an instrument on a track, creating it when missing. True when created.
fn put_track(e: &mut crate::engine::Engine, track: &str, inst: crate::instruments::Instrument) -> bool {
    match e.project.track_index(track) {
        Ok(i) => {
            e.project.tracks[i].instrument = inst;
            false
        }
        Err(_) => {
            e.project.tracks.push(crate::project::Track::new(track, inst));
            true
        }
    }
}

fn load_import(e: &mut crate::engine::Engine, files: &[PathBuf], base: &str, kind_hint: &str, license: &str, source: &str, track: Option<&str>) -> Result<Value> {
    let ext = |p: &PathBuf| p.extension().map(|x| x.to_string_lossy().to_lowercase()).unwrap_or_default();
    let sf2: Vec<&PathBuf> = files.iter().filter(|p| ext(p) == "sf2").collect();
    let sfz: Vec<&PathBuf> = files.iter().filter(|p| ext(p) == "sfz").collect();
    let audio: Vec<&PathBuf> = files.iter().filter(|p| AUDIO.contains(&ext(p).as_str())).collect();
    if let Some(f) = sf2.first() {
        let font = f.to_string_lossy().to_string();
        let ps = crate::soundfont::presets(&font)?;
        let first = ps.iter().find(|p| p.0 == 0).or(ps.first()).cloned();
        let mut out = json!({"loaded": "soundfont", "font": font, "presets": ps.len(), "first_presets": ps.iter().take(24).map(|p| json!({"bank": p.0, "program": p.1, "name": p.2})).collect::<Vec<_>>(), "next": "set_instrument {track, instrument:{type:'soundfont', font:<font>, bank, program}}"});
        if let (Some(t), Some(p)) = (track, first) {
            let inst = crate::instruments::Instrument::SoundFont(crate::soundfont::SoundFontParams { font: font.clone(), bank: p.0, program: p.1, drums: p.0 == 128, preset_name: p.2.clone(), ..Default::default() });
            let created = put_track(e, t, inst);
            out["track"] = json!(t);
            out["created"] = json!(created);
            out["preset"] = json!({"bank": p.0, "program": p.1, "name": p.2});
        }
        return Ok(out);
    }
    if !sfz.is_empty() {
        let progs: Vec<String> = sfz.iter().map(|p| p.to_string_lossy().to_string()).collect();
        let mut out = json!({"loaded": "sfz", "programs": progs, "next": "add_multisample_track {name, sfz:<program>}"});
        if let Some(t) = track {
            let r = e.call("add_multisample_track", &json!({"name": t, "sfz": progs[0]}))?;
            out["track"] = json!(t);
            out["track_result"] = r;
        }
        return Ok(out);
    }
    if audio.is_empty() {
        bail!("nothing playable found (no audio, .sf2 or .sfz)");
    }
    if kind_hint == "ir" {
        return Ok(json!({"loaded": "ir", "ir_path": audio[0], "next": "add_effect convolution {ir_path} or cab_ir {path}"}));
    }
    // one-shot folder -> kit by drum names, or multisample by note names
    let notes: Vec<(String, u8, i32, u8)> = audio.iter().filter_map(|p| {
        let n = p.file_name()?.to_string_lossy().to_string();
        crate::multisample::parse_sample_name(&n).map(|(k, v, rr)| (p.to_string_lossy().to_string(), k, v, rr))
    }).collect();
    let mut names = Vec::new();
    if audio.len() > 1 && (kind_hint == "kit" || (kind_hint == "auto" && notes.len() * 2 < audio.len())) {
        let mut kit = serde_json::Map::new();
        for p in &audio {
            let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            let s = register_file(e, p, &format!("{base}_{stem}"), license, source)?;
            names.push(s.clone());
            if let Some(role) = drum_role(&stem) {
                kit.entry(role.to_string()).or_insert_with(|| json!([])).as_array_mut().unwrap().push(json!(s));
            }
        }
        return Ok(json!({"loaded": "kit", "samples": names, "by_drum": kit, "next": "add_sample_track {name, sample} per drum, or use the samples in a pattern"}));
    }
    if notes.len() >= 2 && kind_hint != "sample" && kind_hint != "loop" {
        let mut zones_in = Vec::new();
        for (path, k, v, rr) in &notes {
            let stem = Path::new(path).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            let s = register_file(e, Path::new(path), &format!("{base}_{stem}"), license, source)?;
            names.push(s.clone());
            zones_in.push((s, *k, *v, *rr));
        }
        let zones = crate::multisample::zones_from(&zones_in);
        let (lo, hi) = (zones.iter().map(|z| z.lo_note).min().unwrap_or(0), zones.iter().map(|z| z.hi_note).max().unwrap_or(127));
        let n = zones.len();
        let track = track.map(|t| t.to_string()).unwrap_or_else(|| crate::samples::sample_name(base));
        let inst = crate::instruments::Instrument::Multisample(crate::multisample::MultisampleParams { zones, source: format!("{source} ({license})"), ..Default::default() });
        put_track(e, &track, inst);
        return Ok(json!({"loaded": "multisample", "track": track, "zones": n, "key_range": [lo, hi], "samples": names}));
    }
    for p in &audio {
        let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let nm = if audio.len() == 1 { base.to_string() } else { format!("{base}_{stem}") };
        names.push(register_file(e, p, &nm, license, source)?);
    }
    Ok(json!({"loaded": if kind_hint == "loop" { "loop" } else { "sample" }, "samples": names}))
}

/// Links on a page that point at audio, SoundFonts, SFZ or archives.
pub fn page_links(html: &str, base: &str) -> Vec<(String, String)> {
    let exts = [".wav", ".flac", ".mp3", ".ogg", ".aif", ".aiff", ".sf2", ".sf3", ".sfz", ".zip", ".tar.gz", ".tgz", ".7z"];
    let mut out: Vec<(String, String)> = Vec::new();
    let lower = html.to_lowercase();
    for attr in ["href=", "src="] {
        let mut i = 0;
        while let Some(j) = lower[i..].find(attr) {
            let s = i + j + attr.len();
            let q = html[s..].chars().next().unwrap_or(' ');
            let (start, endc) = if q == '"' || q == '\'' { (s + 1, q) } else { (s, ' ') };
            let end = html[start..].find(|c: char| c == endc || c == '>' || (endc == ' ' && c.is_whitespace())).map(|k| start + k).unwrap_or(html.len());
            let link = html[start..end].trim().to_string();
            i = end.max(s);
            let l = link.to_lowercase();
            let path = l.split(['?', '#']).next().unwrap_or("");
            if !exts.iter().any(|x| path.ends_with(x)) {
                continue;
            }
            let abs = if link.starts_with("http") {
                link.clone()
            } else if link.starts_with("//") {
                format!("https:{link}")
            } else if link.starts_with('/') {
                let origin: String = base.splitn(4, '/').take(3).collect::<Vec<_>>().join("/");
                format!("{origin}{link}")
            } else {
                format!("{}/{link}", base.rsplit_once('/').map(|x| x.0).unwrap_or(base))
            };
            // the link text, when it is an <a>
            let text = html[end..].find('>').and_then(|g| {
                let t0 = end + g + 1;
                html[t0..].find('<').map(|k| html[t0..t0 + k].trim().to_string())
            }).unwrap_or_default();
            if !out.iter().any(|(u, _)| *u == abs) {
                out.push((abs, text));
            }
        }
    }
    out
}

/// The whole import_from_url action (also used by freesound_fetch and the GUI).
pub fn import_url(e: &mut crate::engine::Engine, a: &Value) -> Result<Value> {
    let url = s_req(a, "url")?;
    let max = a.get("max_mb").and_then(|v| v.as_u64()).unwrap_or(200);
    let (bytes, final_url) = download(&url, max)?;
    let kind = sniff(&bytes, &final_url);
    let fname = final_url.split(['?', '#']).next().unwrap_or("").rsplit('/').next().unwrap_or("file").to_string();
    let base = s_opt(a, "name").map(|s| slug(&s)).unwrap_or_else(|| slug(fname.split('.').next().unwrap_or("import")));
    let base = if base.is_empty() { "import".to_string() } else { base };
    let dir = crate::soundfont::library_dir().join("imports").join(&base);
    std::fs::create_dir_all(&dir)?;
    let sha = crate::voice_ai::sha256_hex(&bytes);
    let files: Vec<PathBuf> = match kind {
        "zip" | "tar.gz" => unpack(&bytes, kind, &dir, max * 4_000_000)?,
        "7z" => bail!("7z archives are not supported; look for a .zip of the same pack"),
        "sf3" => bail!("SF3 (compressed SoundFont) is not supported yet; look for the .sf2 version"),
        "unknown" if String::from_utf8_lossy(&bytes[..bytes.len().min(512)]).to_lowercase().contains("<html") => bail!("{url} is a web page, not a sound file; import_from_page lists the sound links on it"),
        "unknown" => bail!("{url}: not audio, a SoundFont, an SFZ or an archive (first bytes {:02x?})", &bytes[..bytes.len().min(8)]),
        k => {
            let ext = if k == "aiff" { "aif" } else { k };
            let n = if fname.to_lowercase().ends_with(&format!(".{ext}")) { fname.clone() } else { format!("{base}.{ext}") };
            let p = crate::web_import::safe_join(&dir, &n).ok_or_else(|| anyhow!("bad file name"))?;
            std::fs::write(&p, &bytes)?;
            vec![p]
        }
    };
    if kind == "sfz" {
        bail!("a lone .sfz has no samples; import the zip of its instrument instead (or search_instruments for SFZ instruments the catalog can fetch with their samples)");
    }
    let detected = detect_license(&files);
    let license = s_opt(a, "license").or(detected.clone()).unwrap_or_else(|| "unknown (not stated)".into());
    let source = s_opt(a, "source_page").unwrap_or_else(|| url.clone());
    let mut hint = s_opt(a, "kind").unwrap_or_else(|| "auto".into());
    if hint == "instrument" {
        hint = "auto".into();
    }
    let loaded = load_import(e, &files, &base, &hint, &license, &source, s_opt(a, "track").as_deref())?;
    record(json!({"slug": base, "url": url, "final_url": final_url, "source_page": s_opt(a, "source_page"), "license": license, "license_detected": detected, "sha256": sha, "bytes": bytes.len(), "kind": kind, "date": crate::sound_catalog::now(), "loaded": loaded["loaded"]}))?;
    crate::sound_catalog::evict("imports");
    let mut out = json!({"name": base, "kind": kind, "sha256": sha, "mb": (bytes.len() as f64 / 1e5).round() / 10.0, "license": license, "folder": dir, "files": files.len()});
    if license.to_lowercase().contains("non-commercial") || license.starts_with("unknown") {
        out["license_warning"] = json!("check the licence before releasing music with this");
    }
    if let (Value::Object(o), Value::Object(l)) = (&mut out, loaded) {
        o.extend(l);
    }
    Ok(out)
}

// ------------------------------------------------------------ freesound

fn freesound_key() -> Result<String> {
    std::env::var("BEATBOX_FREESOUND_KEY").or_else(|_| std::env::var("FREESOUND_API_KEY")).ok().filter(|k| !k.trim().is_empty()).ok_or_else(|| anyhow!("set BEATBOX_FREESOUND_KEY to a freesound.org API key (free at freesound.org/apiv2/apply) to search Freesound; import_from_url works without one"))
}

fn freesound_base() -> String {
    std::env::var("BEATBOX_FREESOUND_API").unwrap_or_else(|_| "https://freesound.org/apiv2".into())
}

/// Freesound's licence URL or name -> a short label.
pub fn license_label(l: &str) -> String {
    let x = l.to_lowercase();
    if x.contains("zero") || x.contains("cc0") || x.contains("creative commons 0") {
        "CC0".into()
    } else if x.contains("by-nc") || x.contains("noncommercial") {
        "CC BY-NC (non-commercial)".into()
    } else if x.contains("sampling+") {
        "Sampling+".into()
    } else if x.contains("/by/") || x == "attribution" {
        "CC BY".into()
    } else {
        l.to_string()
    }
}

fn fs_get(path_and_query: &str) -> Result<Value> {
    let key = freesound_key()?;
    let sep = if path_and_query.contains('?') { '&' } else { '?' };
    let url = format!("{}{path_and_query}{sep}token={key}", freesound_base());
    let (b, _) = download(&url, 8)?;
    Ok(serde_json::from_slice(&b).context("freesound answer")?)
}

fn enc(s: &str) -> String {
    s.bytes().map(|c| if c.is_ascii_alphanumeric() || b"-_.~".contains(&c) { (c as char).to_string() } else { format!("%{c:02X}") }).collect()
}

fn fs_row(r: &Value) -> Value {
    json!({
        "id": r["id"], "name": r["name"], "user": r["username"], "duration": r["duration"],
        "license": license_label(r["license"].as_str().unwrap_or("")), "license_url": r["license"],
        "page": r["url"], "tags": r["tags"].as_array().map(|t| t.iter().take(8).cloned().collect::<Vec<_>>()).unwrap_or_default(),
        "preview": r["previews"]["preview-hq-ogg"].as_str().or(r["previews"]["preview-hq-mp3"].as_str()),
    })
}

fn freesound_tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "freesound_search",
            description: "Search Freesound.org for sounds (needs BEATBOX_FREESOUND_KEY). query, license cc0|cc_by|any_open (default any_open = CC0 or CC BY; non-commercial licences are left out unless license=all), max_duration (s), limit (default 15). Each hit has id, name, author, duration, licence and page; fetch one with freesound_fetch.",
            mutates: false,
            schema: || obj(json!({"query": {"type": "string"}, "license": {"type": "string", "enum": ["cc0", "cc_by", "any_open", "all"]}, "max_duration": {"type": "number"}, "limit": {"type": "integer"}}), &["query"]),
            run: |_e, a| {
                let q = s_req(a, "query")?;
                let mut filt = match s_opt(a, "license").as_deref().unwrap_or("any_open") {
                    "cc0" => "license:\"Creative Commons 0\"".to_string(),
                    "cc_by" => "license:\"Attribution\"".to_string(),
                    "all" => String::new(),
                    _ => "license:(\"Creative Commons 0\" OR \"Attribution\")".to_string(),
                };
                if let Some(d) = f_opt(a, "max_duration") {
                    filt = format!("{filt} duration:[0 TO {d}]").trim().to_string();
                }
                let n = a.get("limit").and_then(|v| v.as_u64()).unwrap_or(15).clamp(1, 50);
                let mut path = format!("/search/text/?query={}&page_size={n}&fields=id,name,username,duration,license,url,tags,previews", enc(&q));
                if !filt.is_empty() {
                    path.push_str(&format!("&filter={}", enc(&filt)));
                }
                let v = fs_get(&path)?;
                let hits: Vec<Value> = v["results"].as_array().map(|r| r.iter().map(fs_row).collect()).unwrap_or_default();
                Ok(json!({"count": v["count"], "results": hits}))
            },
        },
        Tool {
            name: "freesound_fetch",
            description: "Download a Freesound sound by id (its high-quality preview, needs BEATBOX_FREESOUND_KEY) and register it as a sample, like import_from_url, with the author, page and licence recorded in sources.json for export_credits. name, kind sample|loop|ir.",
            mutates: true,
            schema: || obj(json!({"id": {"type": "integer"}, "name": {"type": "string"}, "kind": {"type": "string", "enum": ["sample", "loop", "ir"]}}), &["id"]),
            run: |e, a| {
                let id = a.get("id").and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))).ok_or_else(|| anyhow!("id is required"))?;
                let v = fs_get(&format!("/sounds/{id}/?fields=id,name,username,duration,license,url,tags,previews"))?;
                let row = fs_row(&v);
                let prev = row["preview"].as_str().ok_or_else(|| anyhow!("sound {id} has no preview"))?.to_string();
                let name = s_opt(a, "name").unwrap_or_else(|| format!("fs{id}_{}", slug(row["name"].as_str().unwrap_or("sound"))));
                let lic = format!("{} by {}", row["license"].as_str().unwrap_or("?"), row["user"].as_str().unwrap_or("?"));
                let mut out = import_url(e, &json!({"url": prev, "name": name, "kind": s_opt(a, "kind").unwrap_or_else(|| "sample".into()), "license": lic, "source_page": row["page"], "max_mb": 60}))?;
                out["freesound"] = row;
                Ok(out)
            },
        },
    ]
}

pub fn tools() -> Vec<Tool> {
    let mut v = freesound_tools();
    v.extend(vec![
        Tool {
            name: "import_from_url",
            description: "Download a sound from the web and load it: url (https) to a WAV/AIFF/FLAC/MP3/OGG (-> sample), .sf2 (-> SoundFont instrument), .sfz, or a .zip/.tar.gz (unpacked safely; then its SF2/SFZ, or its one-shots as a kit by drum names, or a multisample by note names in the filenames). kind auto|sample|loop|instrument|kit|ir. Saved under ~/.beatbox/library/imports/<name> with a sha256, and recorded with url, source_page and licence (as given, or found in the archive's LICENSE/README) in sources.json; export_credits lists them. max_mb (default 200) caps the download. track: also put an SF2/SFZ/multisample on this track (created if missing).",
            mutates: true,
            schema: || obj(json!({"url": {"type": "string"}, "name": {"type": "string"}, "kind": {"type": "string", "enum": ["auto", "sample", "loop", "instrument", "kit", "ir"]}, "license": {"type": "string"}, "source_page": {"type": "string"}, "max_mb": {"type": "integer"}, "track": {"type": "string"}}), &["url"]),
            run: |e, a| import_url(e, a),
        },
        Tool {
            name: "import_from_page",
            description: "List the downloadable sounds on a web page (links to audio, .sf2/.sfz and .zip/.tar.gz/.7z archives) with their link text, so you can pick one for import_from_url. Downloads nothing else.",
            mutates: false,
            schema: || obj(json!({"url": {"type": "string"}}), &["url"]),
            run: |_e, a| {
                let url = s_req(a, "url")?;
                let (b, final_url) = download(&url, 8)?;
                let html = String::from_utf8_lossy(&b);
                let links = page_links(&html, &final_url);
                Ok(json!({"page": final_url, "links": links.iter().take(300).map(|(u, t)| json!({"url": u, "text": t})).collect::<Vec<_>>(), "count": links.len()}))
            },
        },
        Tool {
            name: "list_imports",
            description: "Everything imported from the web so far (sources.json in the library): name, url, source page, licence, sha256, size and what it loaded as.",
            mutates: false,
            schema: || obj(json!({}), &[]),
            run: |_e, _a| {
                let m = manifest();
                Ok(json!({"count": m.len(), "imports": m, "file": manifest_path()}))
            },
        },
        Tool {
            name: "export_credits",
            description: "Credits for the current project: every sample, recorded instrument and import it uses, with source and licence (from the sample list, soundfont/multisample tracks, the catalog and sources.json). format text|json.",
            mutates: false,
            schema: || obj(json!({"format": {"type": "string", "enum": ["text", "json"]}}), &[]),
            run: |e, a| {
                let mut rows: Vec<Value> = Vec::new();
                let man = manifest();
                for s in &e.project.samples {
                    let imp = man.iter().find(|m| s.path.contains(&format!("/imports/{}/", m["slug"].as_str().unwrap_or("\u{0}"))));
                    rows.push(json!({"what": s.name, "source": imp.and_then(|m| m["url"].as_str()).unwrap_or(&s.source), "license": imp.and_then(|m| m["license"].as_str()).unwrap_or(&s.license), "author": s.author}));
                }
                for t in &e.project.tracks {
                    match &t.instrument {
                        crate::instruments::Instrument::SoundFont(p) => {
                            let ent = crate::soundfont::font_entry(&p.font);
                            rows.push(json!({"what": format!("{} (track {})", if p.preset_name.is_empty() { "SoundFont preset".to_string() } else { p.preset_name.clone() }, t.name), "source": ent.map(|x| x.source).unwrap_or(&p.font), "license": ent.map(|x| x.license).unwrap_or("see the font")}));
                        }
                        crate::instruments::Instrument::Multisample(p) if !p.source.is_empty() => {
                            rows.push(json!({"what": format!("multisample (track {})", t.name), "source": p.source, "license": "see source"}));
                        }
                        _ => {}
                    }
                }
                rows.sort_by(|x, y| x["source"].as_str().cmp(&y["source"].as_str()));
                if s_opt(a, "format").as_deref() == Some("json") {
                    return Ok(json!({"credits": rows}));
                }
                let text: Vec<String> = rows.iter().map(|r| format!("{} -- {} -- {}", r["what"].as_str().unwrap_or(""), r["source"].as_str().unwrap_or(""), r["license"].as_str().unwrap_or(""))).collect();
                Ok(json!({"credits": text.join("\n"), "count": rows.len()}))
            },
        },
    ]);
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;
    use std::collections::HashMap;
    use std::io::Write;

    fn serve(files: HashMap<String, Vec<u8>>) -> String {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        std::thread::spawn(move || {
            for req in server.incoming_requests() {
                let path = req.url().split('?').next().unwrap_or("").to_string();
                match files.get(&path) {
                    Some(b) => req.respond(tiny_http::Response::from_data(b.clone())).ok(),
                    None => req.respond(tiny_http::Response::from_string("nope").with_status_code(404)).ok(),
                };
            }
        });
        format!("http://127.0.0.1:{port}")
    }

    fn tone(freq: f32, secs: f32) -> Vec<u8> {
        let n = (44100.0 * secs) as usize;
        let x: Vec<f32> = (0..n).map(|i| 0.5 * (i as f32 * freq * std::f32::consts::TAU / 44100.0).sin() * (1.0 - i as f32 / n as f32)).collect();
        crate::media::wav_bytes(&x, &x, 44100)
    }

    fn zip_of(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opt = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (n, b) in entries {
            w.start_file(*n, opt).unwrap();
            w.write_all(b).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    #[test]
    fn import_wav_kit_zip_sf2_and_credits() {
        let lib = std::env::temp_dir().join(format!("beatbox_import_tests_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&lib);
        std::env::set_var("BEATBOX_LIBRARY", &lib);
        let mut files = HashMap::new();
        files.insert("/sounds/clean_hit.wav".to_string(), tone(220.0, 0.3));
        let kit = zip_of(&[
            ("MyKit/Kick 01.wav", tone(60.0, 0.4)),
            ("MyKit/Snare 01.wav", tone(200.0, 0.2)),
            ("MyKit/HiHat closed.wav", tone(4000.0, 0.1)),
            ("MyKit/LICENSE.txt", b"These samples are released under CC0 1.0 (public domain).".to_vec()),
            ("../escape.wav", tone(100.0, 0.1)),
        ]);
        files.insert("/kits/mykit.zip".to_string(), kit);
        files.insert("/page.html".to_string(), b"<html><a href=\"/kits/mykit.zip\">My kit</a> <a href='sounds/clean_hit.wav'>hit</a> <a href=\"/about\">about</a></html>".to_vec());
        let font = crate::soundfont::font_entry("generaluser_gs").map(|f| crate::soundfont::installed_path(f)).filter(|p| p.is_file());
        if let Some(p) = &font {
            files.insert("/fonts/small.sf2".to_string(), std::fs::read(p).unwrap());
        }
        let base = serve(files);
        let mut e = Engine::new(lib.join("work"));

        // plain https rule
        assert!(e.call("import_from_url", &json!({"url": "http://example.com/a.wav"})).is_err());

        // 1. one CC0 wav -> sample with licence
        let r = e.call("import_from_url", &json!({"url": format!("{base}/sounds/clean_hit.wav"), "license": "CC0", "source_page": "https://example.org/hit"})).unwrap();
        assert_eq!(r["loaded"], "sample", "{r}");
        assert_eq!(r["license"], "CC0");
        assert!(e.project.samples.iter().any(|s| s.name == "clean_hit"), "{r}");

        // 2. zip of one-shots -> kit by drum names, licence found in the archive, no escape
        let r = e.call("import_from_url", &json!({"url": format!("{base}/kits/mykit.zip"), "name": "my kit"})).unwrap();
        assert_eq!(r["loaded"], "kit", "{r}");
        assert_eq!(r["license"], "CC0", "{r}");
        assert!(r["by_drum"]["kick"].is_array() && r["by_drum"]["snare"].is_array() && r["by_drum"]["closed_hat"].is_array(), "{r}");
        assert!(!lib.join("imports").join("escape.wav").exists() && !lib.join("escape.wav").exists());
        for f in walk(&lib.join("imports")) {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(std::fs::metadata(&f).unwrap().permissions().mode() & 0o111, 0, "{f:?} is executable");
            }
        }

        // 3. page scrape
        let p = e.call("import_from_page", &json!({"url": format!("{base}/page.html")})).unwrap();
        assert_eq!(p["count"], 2, "{p}");

        // 4. sf2 -> soundfont track
        if font.is_some() {
            let r = e.call("import_from_url", &json!({"url": format!("{base}/fonts/small.sf2"), "track": "keys"})).unwrap();
            assert_eq!(r["loaded"], "soundfont", "{r}");
            assert!(r["presets"].as_u64().unwrap() > 10);
            assert!(e.project.track_index("keys").is_ok());
        }

        // sources.json + credits
        let m = manifest();
        assert!(m.iter().any(|x| x["slug"] == "my_kit" && x["sha256"].as_str().map(|s| s.len() == 64).unwrap_or(false)), "{m:?}");
        let c = e.call("export_credits", &json!({"format": "json"})).unwrap();
        let rows = c["credits"].as_array().unwrap();
        assert!(rows.iter().any(|r| r["source"].as_str().unwrap_or("").contains("mykit.zip") && r["license"] == "CC0"), "{c}");
        let l = e.call("list_imports", &json!({})).unwrap();
        assert!(l["count"].as_u64().unwrap() >= 2, "{l}");
        std::env::remove_var("BEATBOX_LIBRARY");
        let _ = std::fs::remove_dir_all(&lib);
    }

    fn walk(d: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(d) {
            for x in rd.flatten() {
                let p = x.path();
                if p.is_dir() {
                    out.extend(walk(&p));
                } else {
                    out.push(p);
                }
            }
        }
        out
    }

    #[test]
    fn unpack_is_confined_and_capped() {
        let d = std::env::temp_dir().join(format!("beatbox_unpack_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let z = zip_of(&[("/abs.wav", vec![1; 10]), ("a/../../up.wav", vec![1; 10]), ("ok/x.wav", vec![1; 10])]);
        let out = unpack(&z, "zip", &d, 1_000_000).unwrap();
        assert_eq!(out.len(), 1);
        assert!(out[0].starts_with(&d));
        let big = zip_of(&[("big.wav", vec![0; 3000])]);
        assert!(unpack(&big, "zip", &d.join("b"), 1000).is_err());
        let _ = std::fs::remove_dir_all(&d);
        assert_eq!(license_label("http://creativecommons.org/publicdomain/zero/1.0/"), "CC0");
        assert_eq!(license_label("https://creativecommons.org/licenses/by/4.0/"), "CC BY");
        assert!(license_label("http://creativecommons.org/licenses/by-nc/3.0/").contains("non-commercial"));
        assert_eq!(sniff(b"RIFF\0\0\0\0WAVEfmt ", "x"), "wav");
    }
}
