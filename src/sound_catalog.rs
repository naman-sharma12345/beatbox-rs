//! The sound catalog: an index (metadata only: names, family, tags, licence,
//! URL, size, sha256) of permissively licensed recorded instruments and
//! impulse responses, fetched on demand into the library
//! (~/.beatbox/library) with sha256 checks and an LRU size limit. Audio is
//! never committed. VSCO 2 CE packs (CC0) are listed from `multisample`.

use crate::tools::{obj, s_opt, s_req, Tool};
use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;

const CATALOG_JSON: &str = include_str!("sound_catalog.json");

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub family: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub format: String,
    pub license: String,
    #[serde(default)]
    pub license_url: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub bytes: u64,
    #[serde(default)]
    pub file: String,
    #[serde(default)]
    pub status: String,
    /// sfz_git: owner/repo, pinned tree sha, branch for raw downloads, and
    /// the program's .sfz path inside the repo.
    #[serde(default)]
    pub repo: String,
    #[serde(default)]
    pub tree: String,
    #[serde(default)]
    pub branch: String,
    #[serde(default)]
    pub sfz: String,
    #[serde(default)]
    pub repo_bytes: u64,
}

#[derive(Deserialize)]
struct Cat {
    entries: Vec<Entry>,
}

/// Every catalog entry: the JSON index plus the VSCO 2 CE packs.
pub fn entries() -> Vec<Entry> {
    let mut v: Vec<Entry> = serde_json::from_str::<Cat>(CATALOG_JSON).map(|c| c.entries).unwrap_or_default();
    for p in crate::multisample::PACKS {
        let fam = if p.folder.starts_with("Keys") { "piano" } else if p.folder.starts_with("Strings") { "strings" } else if p.folder.starts_with("Brass") { "brass" } else if p.folder.starts_with("Woodwinds") { "winds" } else { "orchestra" };
        v.push(Entry {
            id: format!("vsco_{}", p.name),
            name: format!("VSCO 2 CE: {}", p.description),
            family: fam.into(),
            tags: vec![p.name.replace('_', " "), "orchestral".into(), "multisample".into()],
            format: "vsco_pack".into(),
            license: crate::multisample::PACK_LICENSE.into(),
            license_url: "https://github.com/sgossner/VSCO-2-CE".into(),
            source: format!("https://github.com/{}/tree/master/{}", crate::multisample::PACK_REPO, p.folder),
            url: String::new(),
            sha256: String::new(),
            bytes: 0,
            file: String::new(),
            status: "pinned".into(),
            repo: String::new(),
            tree: String::new(),
            branch: String::new(),
            sfz: String::new(),
            repo_bytes: 0,
        });
    }
    v
}

pub fn entry(id: &str) -> Option<Entry> {
    entries().into_iter().find(|e| e.id.eq_ignore_ascii_case(id))
}

pub fn path_of(e: &Entry) -> PathBuf {
    crate::soundfont::library_dir().join(&e.id).join(&e.file)
}

/// An installed catalog file for an id (IRs, fonts), or None.
pub fn resolve_file(id: &str) -> Option<PathBuf> {
    let e = entry(id)?;
    if e.file.is_empty() {
        return None;
    }
    let p = path_of(&e);
    if p.is_file() {
        touch(&p);
        return Some(p);
    }
    fetch(&e).ok()
}

fn touch(p: &std::path::Path) {
    // last-use time for LRU eviction
    let _ = std::fs::write(p.with_extension("used"), format!("{}", now()));
}

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn settings_path() -> PathBuf {
    crate::soundfont::library_dir().join("settings.json")
}

pub fn cache_limit_mb() -> u64 {
    std::fs::read_to_string(settings_path()).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok()).and_then(|v| v["cache_limit_mb"].as_u64()).unwrap_or(1500)
}

fn dir_size(p: &std::path::Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(p) else { return 0 };
    rd.filter_map(|e| e.ok()).map(|e| {
        let m = e.metadata();
        match m {
            Ok(m) if m.is_dir() => dir_size(&e.path()),
            Ok(m) => m.len(),
            Err(_) => 0,
        }
    }).sum()
}

/// (id, bytes, last_used) of every installed entry: single files by id,
/// SFZ repos by repo (their programs share samples).
pub fn installed() -> Vec<(String, u64, u64)> {
    let mut out: Vec<(String, u64, u64)> = entries()
        .iter()
        .filter(|e| !e.file.is_empty())
        .filter_map(|e| {
            let p = path_of(e);
            let m = std::fs::metadata(&p).ok()?;
            let used = std::fs::read_to_string(p.with_extension("used")).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
            Some((e.id.clone(), m.len(), used))
        })
        .collect();
    let mut repos: Vec<String> = entries().iter().filter(|e| e.format == "sfz_git").map(|e| e.repo.clone()).collect();
    repos.sort();
    repos.dedup();
    for r in repos {
        let d = crate::soundfont::library_dir().join(r.replace('/', "__"));
        if d.is_dir() {
            let used = std::fs::read_to_string(d.join("used")).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
            out.push((r, dir_size(&d), used));
        }
    }
    out
}

/// Remove least-recently-used entries until the library fits the limit,
/// never removing `keep`.
pub fn evict(keep: &str) -> Vec<String> {
    let limit = cache_limit_mb() * 1_000_000;
    let mut inst = installed();
    inst.sort_by_key(|x| x.2);
    let mut total: u64 = inst.iter().map(|x| x.1).sum();
    let mut gone = Vec::new();
    for (id, b, _) in inst {
        if total <= limit {
            break;
        }
        if id.eq_ignore_ascii_case(keep) {
            continue;
        }
        if remove(&id).is_ok() {
            total -= b;
            gone.push(id);
        }
    }
    gone
}

pub fn remove(id: &str) -> Result<u64> {
    // an SFZ repo (owner/repo) or any of its program ids removes the repo folder
    let repo = entries().into_iter().find(|e| e.format == "sfz_git" && (e.repo.eq_ignore_ascii_case(id) || e.id.eq_ignore_ascii_case(id))).map(|e| e.repo);
    if let Some(r) = repo {
        let d = crate::soundfont::library_dir().join(r.replace('/', "__"));
        let b = dir_size(&d);
        if d.is_dir() {
            std::fs::remove_dir_all(&d)?;
        }
        return Ok(b);
    }
    let e = entry(id).ok_or_else(|| anyhow!("unknown id '{id}'"))?;
    let dir = crate::soundfont::library_dir().join(&e.id);
    let b = std::fs::metadata(path_of(&e)).map(|m| m.len()).unwrap_or(0);
    if dir.is_dir() {
        std::fs::remove_dir_all(&dir)?;
    }
    Ok(b)
}

/// Download a pinned single-file entry once, sha256-checked.
pub fn fetch(e: &Entry) -> Result<PathBuf> {
    if e.status != "pinned" || e.sha256.is_empty() || e.url.is_empty() {
        bail!("'{}' is listed but its download is not pinned to a checksum yet, so it is not fetched (licence: {}; source: {})", e.id, e.license, e.source);
    }
    let p = path_of(e);
    if p.is_file() && (e.bytes == 0 || std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0) == e.bytes) {
        touch(&p);
        return Ok(p);
    }
    let resp = ureq::get(&e.url).timeout(std::time::Duration::from_secs(900)).call().map_err(|er| anyhow!("download {}: {er}", e.id))?;
    let mut buf = Vec::with_capacity(e.bytes as usize);
    std::io::Read::read_to_end(&mut resp.into_reader(), &mut buf)?;
    let got = crate::voice_ai::sha256_hex(&buf);
    if got != e.sha256 {
        bail!("{}: checksum mismatch ({got}), not installed", e.id);
    }
    std::fs::create_dir_all(p.parent().unwrap())?;
    let tmp = p.with_extension("part");
    std::fs::write(&tmp, &buf)?;
    std::fs::rename(&tmp, &p)?;
    touch(&p);
    evict(&e.id);
    Ok(p)
}

fn entry_json(e: &Entry) -> Value {
    let inst = match e.format.as_str() {
        "vsco_pack" => Value::Null,
        "sfz_git" => json!(crate::soundfont::library_dir().join(e.repo.replace('/', "__")).join(dir_of(&e.sfz)).join(format!("{}.beatbox_flat.sfz", e.sfz.rsplit('/').next().unwrap_or("").trim_end_matches(".sfz"))).is_file()),
        _ => json!(!e.file.is_empty() && path_of(e).is_file()),
    };
    let mb = if e.format == "sfz_git" { Value::Null } else { json!((e.bytes as f64 / 1e6 * 10.0).round() / 10.0) };
    json!({"id": e.id, "name": e.name, "family": e.family, "tags": e.tags, "format": e.format, "license": e.license, "source": e.source, "mb": mb, "status": e.status, "installed": inst})
}


// ------------------------------------------------------------ SFZ from git

/// Git blob id (SHA-1 of "blob <len>\0" + data), to verify files fetched
/// from a pinned tree.
pub fn git_blob_sha1(data: &[u8]) -> String {
    let mut msg = format!("blob {}\0", data.len()).into_bytes();
    msg.extend_from_slice(data);
    sha1_hex(&msg)
}

pub fn sha1_hex(data: &[u8]) -> String {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut m = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    m.push(0x80);
    while m.len() % 64 != 56 {
        m.push(0);
    }
    m.extend_from_slice(&bits.to_be_bytes());
    let mut w = [0u32; 80];
    for c in m.chunks(64) {
        for i in 0..16 {
            w[i] = u32::from_be_bytes([c[4 * i], c[4 * i + 1], c[4 * i + 2], c[4 * i + 3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut cc, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & cc) | (!b & d), 0x5A827999),
                20..=39 => (b ^ cc ^ d, 0x6ED9EBA1),
                40..=59 => ((b & cc) | (b & d) | (cc & d), 0x8F1BBCDC),
                _ => (b ^ cc ^ d, 0xCA62C1D6),
            };
            let t = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(*wi);
            e = d;
            d = cc;
            cc = b.rotate_left(30);
            b = a;
            a = t;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(cc);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}

fn norm_path(p: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for part in p.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            x => out.push(x),
        }
    }
    out.join("/")
}

fn dir_of(p: &str) -> String {
    p.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default()
}

/// Flatten an SFZ: inline `#include "file"` (relative to the main file's
/// folder) and substitute `#define $NAME value`. `read` returns a file's text.
pub fn flatten_sfz(main: &str, read: &mut dyn FnMut(&str) -> Result<String>) -> Result<String> {
    let base = dir_of(main);
    let mut defs: Vec<(String, String)> = Vec::new();
    fn go(path: &str, base: &str, defs: &mut Vec<(String, String)>, read: &mut dyn FnMut(&str) -> Result<String>, depth: usize, out: &mut String) -> Result<()> {
        if depth > 12 {
            bail!("#include nested too deep at {path}");
        }
        let text = read(path)?;
        for line in text.lines() {
            let mut l = line.to_string();
            let t = l.trim_start();
            if let Some(rest) = t.strip_prefix("#define") {
                let mut it = rest.split_whitespace();
                if let (Some(k), Some(v)) = (it.next(), it.next()) {
                    defs.retain(|(a, _)| a != k);
                    defs.push((k.to_string(), v.to_string()));
                }
                continue;
            }
            if let Some(rest) = t.strip_prefix("#include") {
                let inc = rest.trim().trim_matches('"').replace('\\', "/");
                let full = norm_path(&format!("{base}/{inc}"));
                go(&full, base, defs, read, depth + 1, out)?;
                continue;
            }
            // longest names first so $VEL2 is not eaten by $VEL
            let mut ds = defs.clone();
            ds.sort_by_key(|(k, _)| std::cmp::Reverse(k.len()));
            for (k, v) in &ds {
                if l.contains(k.as_str()) {
                    l = l.replace(k.as_str(), v);
                }
            }
            out.push_str(&l);
            out.push('\n');
        }
        Ok(())
    }
    let mut out = String::new();
    go(&norm_path(main), &base, &mut defs, read, 0, &mut out)?;
    Ok(out)
}

fn http_bytes(url: &str) -> Result<Vec<u8>> {
    let resp = ureq::get(url).set("User-Agent", "beatbox-rs").timeout(std::time::Duration::from_secs(300)).call().map_err(|e| anyhow!("{url}: {e}"))?;
    let mut buf = Vec::new();
    std::io::Read::read_to_end(&mut resp.into_reader(), &mut buf)?;
    Ok(buf)
}

fn url_path(p: &str) -> String {
    p.split('/').map(|s| s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect::<String>()).collect::<Vec<_>>().join("/")
}

/// Fetch one program of an SFZ instrument from its pinned git tree: the SFZ,
/// its includes and only the samples its playable regions use, each checked
/// against the tree's blob id. Returns the flattened local .sfz (next to the
/// original, so sample paths resolve) and the bytes downloaded now.
pub fn fetch_sfz_git(e: &Entry, max_mb: u64, max_rr: u8) -> Result<(PathBuf, u64)> {
    let (repo, tree, branch, sfz) = (e.repo.as_str(), e.tree.as_str(), e.branch.as_str(), e.sfz.as_str());
    if repo.is_empty() || tree.is_empty() || sfz.is_empty() {
        bail!("{}: catalog entry lacks repo/tree/sfz", e.id);
    }
    let dir = crate::soundfont::library_dir().join(repo.replace('/', "__"));
    std::fs::create_dir_all(&dir)?;
    // The listing lets every file be checked against its git blob id; when the
    // API is unreachable (rate limit, offline) files still come from raw URLs
    // pinned to the commit, which cannot change.
    let tf = dir.join("tree.json");
    let tj: Option<Value> = match std::fs::read(&tf).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()).filter(|v| v["tree"].is_array()) {
        Some(v) => Some(v),
        None => {
            let url = format!("https://api.github.com/repos/{repo}/git/trees/{tree}?recursive=1");
            let mut req = ureq::get(&url).set("User-Agent", "beatbox-rs").timeout(std::time::Duration::from_secs(60));
            if let Ok(t) = std::env::var("BEATBOX_GITHUB_TOKEN").or_else(|_| std::env::var("GITHUB_TOKEN")) {
                req = req.set("Authorization", &format!("Bearer {t}"));
            }
            match req.call() {
                Ok(r) => {
                    let mut b = Vec::new();
                    std::io::Read::read_to_end(&mut r.into_reader(), &mut b)?;
                    let v: Value = serde_json::from_slice(&b)?;
                    std::fs::write(&tf, &b)?;
                    Some(v)
                }
                Err(_) => None,
            }
        }
    };
    let verified = tj.is_some();
    let mut blobs: std::collections::HashMap<String, (String, String, u64)> = std::collections::HashMap::new();
    if let Some(tj) = &tj {
        for it in tj["tree"].as_array().cloned().unwrap_or_default() {
            if it["type"] == "blob" {
                let p = it["path"].as_str().unwrap_or("").to_string();
                blobs.insert(p.to_lowercase(), (p, it["sha"].as_str().unwrap_or("").to_string(), it["size"].as_u64().unwrap_or(0)));
            }
        }
    }
    let _ = branch;
    let mut fetched = 0u64;
    let get = |p: &str, fetched: &mut u64| -> Result<PathBuf> {
        let np = norm_path(p);
        let (rp, sha, size) = if verified {
            blobs.get(&np.to_lowercase()).cloned().ok_or_else(|| anyhow!("{p} is not in {repo}"))?
        } else {
            (np.clone(), String::new(), 0)
        };
        let local = dir.join(&rp);
        if local.is_file() && (size == 0 || std::fs::metadata(&local).map(|m| m.len()).unwrap_or(u64::MAX) == size) {
            return Ok(local);
        }
        let data = http_bytes(&format!("https://raw.githubusercontent.com/{repo}/{tree}/{}", url_path(&rp)))?;
        if verified && git_blob_sha1(&data) != sha {
            bail!("{rp}: content does not match the pinned tree; not installed");
        }
        std::fs::create_dir_all(local.parent().unwrap())?;
        std::fs::write(&local, &data)?;
        *fetched += data.len() as u64;
        Ok(local)
    };
    let flat = flatten_sfz(sfz, &mut |p| {
        let lp = get(p, &mut fetched)?;
        Ok(String::from_utf8_lossy(&std::fs::read(lp)?).to_string())
    })?;
    let (regions, _) = crate::multisample::parse_sfz(&flat)?;
    let mut regions = crate::multisample::playable_regions(regions);
    // keep at most max_rr round robins (disk and memory); the zones still
    // alternate between the ones kept
    regions.retain(|r| r.seq <= max_rr.max(1));
    let base = dir_of(sfz);
    let mut need: Vec<String> = regions.iter().map(|r| norm_path(&format!("{base}/{}", r.sample))).collect();
    need.sort();
    need.dedup();
    let total: u64 = need.iter().filter_map(|p| blobs.get(&p.to_lowercase()).map(|b| b.2)).sum();
    if total > max_mb * 1_000_000 {
        bail!("{}: this program needs {:.0} MB of samples, over max_mb {max_mb}; pick a smaller program or raise max_mb", e.id, total as f64 / 1e6);
    }
    for p in &need {
        get(p, &mut fetched)?;
        if fetched > max_mb * 1_000_000 {
            bail!("{}: stopped after {:.0} MB, over max_mb {max_mb} (files so far stay in the library)", e.id, fetched as f64 / 1e6);
        }
    }
    let name = sfz.rsplit('/').next().unwrap_or("program.sfz").trim_end_matches(".sfz");
    let out = dir.join(&base).join(format!("{name}.beatbox_flat.sfz"));
    std::fs::write(&out, &flat)?;
    let _ = std::fs::write(dir.join("used"), format!("{}", now()));
    Ok((out, fetched))
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "search_instruments",
            description: "Search the sound catalog of real recorded instruments and impulse responses (permissive licences only: CC0, CC-BY, MIT-style): query matches name/tags, family filters (gm, guitar, bass, drums, piano, keys, strings, brass, winds, choir, orchestra, ir_room, ir_cab), license filters by substring (e.g. cc0). Returns ids for fetch_instrument / load_instrument, size, licence and whether each is installed.",
            mutates: false,
            schema: || obj(json!({"query": {"type": "string"}, "family": {"type": "string"}, "license": {"type": "string"}}), &[]),
            run: |_e, a| {
                let q = s_opt(a, "query").map(|s| s.to_lowercase());
                let fam = s_opt(a, "family").map(|s| s.to_lowercase());
                let lic = s_opt(a, "license").map(|s| s.to_lowercase());
                let all = entries();
                let hits: Vec<Value> = all
                    .iter()
                    .filter(|e| fam.as_ref().map_or(true, |f| e.family.to_lowercase() == *f || e.tags.iter().any(|t| t.to_lowercase().contains(f.as_str()))))
                    .filter(|e| lic.as_ref().map_or(true, |l| e.license.to_lowercase().contains(l.as_str())))
                    .filter(|e| q.as_ref().map_or(true, |q| q.split_whitespace().all(|w| e.name.to_lowercase().contains(w) || e.id.contains(w) || e.tags.iter().any(|t| t.to_lowercase().contains(w)))))
                    .map(entry_json)
                    .collect();
                Ok(json!({"catalog_size": all.len(), "matches": hits.len(), "results": hits}))
            },
        },
        Tool {
            name: "list_installed",
            description: "Instruments and IRs downloaded into the library (~/.beatbox/library): id, size, last used, plus the total and the cache limit (cache_limit_mb).",
            mutates: false,
            schema: || obj(json!({}), &[]),
            run: |_e, _a| {
                let inst = installed();
                let total: u64 = inst.iter().map(|x| x.1).sum();
                Ok(json!({"library": crate::soundfont::library_dir(), "installed": inst.iter().map(|x| json!({"id": x.0, "mb": (x.1 as f64 / 1e5).round() / 10.0, "last_used_unix": x.2})).collect::<Vec<_>>(), "total_mb": (total as f64 / 1e5).round() / 10.0, "cache_limit_mb": cache_limit_mb()}))
            },
        },
        Tool {
            name: "remove_instrument",
            description: "Delete a downloaded instrument or IR from the library (it can be fetched again).",
            mutates: false,
            schema: || obj(json!({"id": {"type": "string"}}), &["id"]),
            run: |_e, a| {
                let id = s_req(a, "id")?;
                let b = remove(&id)?;
                Ok(json!({"removed": id, "freed_mb": (b as f64 / 1e5).round() / 10.0}))
            },
        },
        Tool {
            name: "cache_limit_mb",
            description: "Show or set the library size limit in MB (default 1500). When a download pushes the library over it, the least recently used instruments are removed.",
            mutates: false,
            schema: || obj(json!({"mb": {"type": "integer"}}), &[]),
            run: |_e, a| {
                if let Some(mb) = a.get("mb").and_then(|v| v.as_u64()) {
                    std::fs::create_dir_all(crate::soundfont::library_dir())?;
                    std::fs::write(settings_path(), json!({"cache_limit_mb": mb.max(50)}).to_string())?;
                    let gone = evict("");
                    return Ok(json!({"cache_limit_mb": cache_limit_mb(), "evicted": gone}));
                }
                Ok(json!({"cache_limit_mb": cache_limit_mb()}))
            },
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_parses_and_every_pinned_file_entry_has_a_checksum() {
        let all = entries();
        assert!(all.len() >= 20, "{}", all.len());
        for e in &all {
            assert!(!e.license.is_empty(), "{} has no licence", e.id);
            let l = e.license.to_lowercase();
            assert!(!l.contains("non-commercial") && !l.contains("noncommercial") && !l.contains("-nc"), "{}: {}", e.id, e.license);
            if e.status == "pinned" && !e.file.is_empty() {
                assert_eq!(e.sha256.len(), 64, "{}", e.id);
                assert!(e.url.starts_with("https://"), "{}", e.id);
            }
        }
        let ids: std::collections::HashSet<&str> = all.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids.len(), all.len(), "duplicate ids");
        for e in all.iter().filter(|e| e.format == "sfz_git") {
            assert!(!e.repo.is_empty() && e.tree.len() == 40 && e.sfz.ends_with(".sfz"), "{}", e.id);
        }
        for fam in ["guitar", "bass", "drums", "piano", "strings"] {
            assert!(all.iter().any(|e| e.family == fam), "no {fam}");
        }
    }

    #[test]
    fn sha1_and_git_blob_ids() {
        assert_eq!(sha1_hex(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
        // `git hash-object` of "hello\n"
        assert_eq!(git_blob_sha1(b"hello\n"), "ce013625030ba8dba906f756967f9e9ca394464a");
    }

    #[test]
    fn sfz_includes_defines_and_playable_regions() {
        let files: std::collections::HashMap<&str, &str> = [
            ("Programs/main.sfz", "#define $VOL -3\n<control> default_path=../Samples/\n#include \"mappings/map.sfz\"\n"),
            ("Programs/mappings/map.sfz", "<group> volume=$VOL\n<region> sample=a.wav key=60 sw_last=24\n<region> sample=b.wav key=60 sw_last=25\n<region> sample=rel.wav key=60 trigger=release\n<region> sample=c.wav key=62 lorand=0 hirand=0.5\n<region> sample=d.wav key=62 lorand=0.5 hirand=1\n"),
        ]
        .into_iter()
        .collect();
        let flat = flatten_sfz("Programs/main.sfz", &mut |p| files.get(p).map(|s| s.to_string()).ok_or_else(|| anyhow!("missing {p}"))).unwrap();
        assert!(flat.contains("volume=-3"), "{flat}");
        let (r, _) = crate::multisample::parse_sfz(&flat).unwrap();
        let r = crate::multisample::playable_regions(r);
        let names: Vec<&str> = r.iter().map(|x| x.sample.as_str()).collect();
        assert!(names.contains(&"../Samples/a.wav") && !names.contains(&"../Samples/b.wav") && !names.iter().any(|n| n.contains("rel")), "{names:?}");
        let c = r.iter().find(|x| x.sample.ends_with("c.wav")).unwrap().seq;
        let d = r.iter().find(|x| x.sample.ends_with("d.wav")).unwrap().seq;
        assert_ne!(c, d);
        assert_eq!(norm_path("Programs/../Samples/x.wav"), "Samples/x.wav");
    }
}
