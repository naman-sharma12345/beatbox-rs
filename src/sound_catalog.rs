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

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn settings_path() -> PathBuf {
    crate::soundfont::library_dir().join("settings.json")
}

pub fn cache_limit_mb() -> u64 {
    std::fs::read_to_string(settings_path()).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok()).and_then(|v| v["cache_limit_mb"].as_u64()).unwrap_or(1500)
}

/// (id, bytes, last_used) of every installed single-file entry.
pub fn installed() -> Vec<(String, u64, u64)> {
    entries()
        .iter()
        .filter(|e| !e.file.is_empty())
        .filter_map(|e| {
            let p = path_of(e);
            let m = std::fs::metadata(&p).ok()?;
            let used = std::fs::read_to_string(p.with_extension("used")).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
            Some((e.id.clone(), m.len(), used))
        })
        .collect()
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
    let inst = if e.format == "vsco_pack" { Value::Null } else { json!(!e.file.is_empty() && path_of(e).is_file()) };
    json!({"id": e.id, "name": e.name, "family": e.family, "tags": e.tags, "format": e.format, "license": e.license, "source": e.source, "mb": (e.bytes as f64 / 1e6 * 10.0).round() / 10.0, "status": e.status, "installed": inst})
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
    }
}
