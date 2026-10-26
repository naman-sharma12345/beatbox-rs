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
    /// Pinned-file programs (repos with samples but no .sfz, e.g. VCSL):
    /// the mapping as SFZ text (sample paths relative to the repo root;
    /// `sfz` is then a virtual name) and every file it may use as
    /// [path, git blob sha1, bytes] (a fourth element, a direct https URL,
    /// makes the second a sha256 of that download instead).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sfz_text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<Vec<Value>>,
    /// Styles this sound suits (rap, lofi, indie, rock, jazz, orchestral,
    /// bollywood, classical_indian, edm ...), for search_instruments.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub genres: Vec<String>,
}

/// Free space where the library lives, in MB (None when unknown).
pub fn free_mb() -> Option<u64> {
    let d = crate::soundfont::library_dir();
    let mut p = d.as_path();
    while !p.exists() {
        p = p.parent()?;
    }
    let out = std::process::Command::new("df")
        .arg("-Pk")
        .arg(p)
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    let line = s.lines().nth(1)?;
    line.split_whitespace()
        .nth(3)?
        .parse::<u64>()
        .ok()
        .map(|k| k / 1024)
}

/// Refuse a download that would leave less than BEATBOX_MIN_FREE_MB
/// (default 2048) free on the library's disk.
pub fn ensure_free(need_bytes: u64) -> Result<()> {
    let min: u64 = std::env::var("BEATBOX_MIN_FREE_MB")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2048);
    if let Some(f) = free_mb() {
        let need = need_bytes / 1_000_000;
        if f < min + need {
            bail!("not enough disk: {f} MB free, this needs {need} MB and {min} MB must stay free (BEATBOX_MIN_FREE_MB; remove_instrument frees space)");
        }
    }
    Ok(())
}

#[derive(Deserialize)]
struct Cat {
    entries: Vec<Entry>,
}

/// Every catalog entry: the JSON index plus the VSCO 2 CE packs.
pub fn entries() -> Vec<Entry> {
    let mut v: Vec<Entry> = serde_json::from_str::<Cat>(CATALOG_JSON)
        .map(|c| c.entries)
        .unwrap_or_default();
    for p in crate::multisample::PACKS {
        let fam = if p.folder.starts_with("Keys") {
            "piano"
        } else if p.folder.starts_with("Strings") {
            "strings"
        } else if p.folder.starts_with("Brass") {
            "brass"
        } else if p.folder.starts_with("Woodwinds") {
            "winds"
        } else {
            "orchestra"
        };
        v.push(Entry {
            id: format!("vsco_{}", p.name),
            name: format!("VSCO 2 CE: {}", p.description),
            family: fam.into(),
            tags: vec![
                p.name.replace('_', " "),
                "orchestral".into(),
                "multisample".into(),
            ],
            format: "vsco_pack".into(),
            license: crate::multisample::PACK_LICENSE.into(),
            license_url: "https://github.com/sgossner/VSCO-2-CE".into(),
            source: format!(
                "https://github.com/{}/tree/master/{}",
                crate::multisample::PACK_REPO,
                p.folder
            ),
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
            sfz_text: String::new(),
            files: Vec::new(),
            genres: vec!["orchestral".into(), "cinematic".into()],
        });
    }
    v
}

pub fn entry(id: &str) -> Option<Entry> {
    entries()
        .into_iter()
        .find(|e| e.id.eq_ignore_ascii_case(id))
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
    if cached_file_valid(&p, &e.sha256, e.bytes) {
        touch(&p);
        return Some(p);
    }
    fetch(&e).ok()
}

/// A pinned file must match content, not only its advertised length.
pub(crate) fn cached_file_valid(path: &std::path::Path, sha256: &str, bytes: u64) -> bool {
    if sha256.len() != 64 || !path.is_file() {
        return false;
    }
    if bytes != 0 && std::fs::metadata(path).map(|m| m.len()).ok() != Some(bytes) {
        return false;
    }
    std::fs::read(path)
        .map(|data| crate::voice_ai::sha256_hex(&data) == sha256)
        .unwrap_or(false)
}

fn touch(p: &std::path::Path) {
    // last-use time for LRU eviction
    let _ = std::fs::write(p.with_extension("used"), format!("{}", now()));
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn settings_path() -> PathBuf {
    crate::soundfont::library_dir().join("settings.json")
}

pub fn cache_limit_mb() -> u64 {
    std::fs::read_to_string(settings_path())
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v["cache_limit_mb"].as_u64())
        .unwrap_or(1500)
}

fn dir_size(p: &std::path::Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(p) else {
        return 0;
    };
    rd.filter_map(|e| e.ok())
        .map(|e| {
            let m = e.metadata();
            match m {
                Ok(m) if m.is_dir() => dir_size(&e.path()),
                Ok(m) => m.len(),
                Err(_) => 0,
            }
        })
        .sum()
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
            let used = std::fs::read_to_string(p.with_extension("used"))
                .ok()
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(0);
            Some((e.id.clone(), m.len(), used))
        })
        .collect();
    let mut repos: Vec<String> = entries()
        .iter()
        .filter(|e| e.format == "sfz_git")
        .map(|e| e.repo.clone())
        .collect();
    repos.sort();
    repos.dedup();
    for r in repos {
        let d = crate::soundfont::library_dir().join(r.replace('/', "__"));
        if d.is_dir() {
            let used = std::fs::read_to_string(d.join("used"))
                .ok()
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(0);
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
    let repo = entries()
        .into_iter()
        .find(|e| {
            e.format == "sfz_git"
                && (e.repo.eq_ignore_ascii_case(id) || e.id.eq_ignore_ascii_case(id))
        })
        .map(|e| e.repo);
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
    if cached_file_valid(&p, &e.sha256, e.bytes) {
        touch(&p);
        return Ok(p);
    }
    ensure_free(e.bytes)?;
    let resp = ureq::get(&e.url)
        .timeout(std::time::Duration::from_secs(900))
        .call()
        .map_err(|er| anyhow!("download {}: {er}", e.id))?;
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
        "sfz_git" => json!(crate::soundfont::library_dir()
            .join(e.repo.replace('/', "__"))
            .join(dir_of(&e.sfz))
            .join(format!(
                "{}.beatbox_flat.sfz",
                e.sfz
                    .rsplit('/')
                    .next()
                    .unwrap_or("")
                    .trim_end_matches(".sfz")
            ))
            .is_file()),
        _ => json!(!e.file.is_empty() && path_of(e).is_file()),
    };
    let mb = if e.bytes == 0 {
        Value::Null
    } else {
        json!((e.bytes as f64 / 1e6 * 10.0).round() / 10.0)
    };
    json!({"id": e.id, "name": e.name, "family": e.family, "tags": e.tags, "genres": e.genres, "format": e.format, "license": e.license, "source": e.source, "mb": mb, "status": e.status, "installed": inst})
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
            let t = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*wi);
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
    p.rsplit_once('/')
        .map(|(d, _)| d.to_string())
        .unwrap_or_default()
}

/// Flatten an SFZ: inline `#include "file"` (relative to the main file's
/// folder) and substitute `#define $NAME value`. `read` returns a file's text.
pub fn flatten_sfz(main: &str, read: &mut dyn FnMut(&str) -> Result<String>) -> Result<String> {
    let base = dir_of(main);
    let mut defs: Vec<(String, String)> = Vec::new();
    fn go(
        path: &str,
        base: &str,
        defs: &mut Vec<(String, String)>,
        read: &mut dyn FnMut(&str) -> Result<String>,
        depth: usize,
        out: &mut String,
    ) -> Result<()> {
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
    let resp = ureq::get(url)
        .set("User-Agent", "beatbox-rs")
        .timeout(std::time::Duration::from_secs(300))
        .call()
        .map_err(|e| anyhow!("{url}: {e}"))?;
    let mut buf = Vec::new();
    std::io::Read::read_to_end(&mut resp.into_reader(), &mut buf)?;
    Ok(buf)
}

fn url_path(p: &str) -> String {
    p.split('/')
        .map(|s| {
            s.bytes()
                .map(|b| {
                    if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                        (b as char).to_string()
                    } else {
                        format!("%{b:02X}")
                    }
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Fetch one program of an SFZ instrument from its pinned git tree: the SFZ,
/// its includes and only the samples its playable regions use, each checked
/// against the tree's blob id. Returns the flattened local .sfz (next to the
/// original, so sample paths resolve) and the bytes downloaded now.
fn cached_blob_valid(data: &[u8], sha: &str, direct: bool, verified: bool) -> bool {
    if direct {
        crate::voice_ai::sha256_hex(data) == sha
    } else if verified && sha.len() == 40 {
        git_blob_sha1(data) == sha
    } else {
        true
    }
}

pub fn fetch_sfz_git(e: &Entry, max_mb: u64, max_rr: u8) -> Result<(PathBuf, u64)> {
    let (repo, tree, branch, sfz) = (
        e.repo.as_str(),
        e.tree.as_str(),
        e.branch.as_str(),
        e.sfz.as_str(),
    );
    if repo.is_empty() || tree.is_empty() || sfz.is_empty() {
        bail!("{}: catalog entry lacks repo/tree/sfz", e.id);
    }
    let dir = crate::soundfont::library_dir().join(repo.replace('/', "__"));
    std::fs::create_dir_all(&dir)?;
    // The listing lets every file be checked against its git blob id; when the
    // API is unreachable (rate limit, offline) files still come from raw URLs
    // pinned to the commit, which cannot change.
    let tf = dir.join("tree.json");
    let pinned = !e.files.is_empty();
    let tj: Option<Value> = if pinned {
        Some(
            json!({"tree": e.files.iter().map(|f| json!({"type": "blob", "path": f.first(), "sha": f.get(1), "size": f.get(2), "url": f.get(3)})).collect::<Vec<_>>()}),
        )
    } else {
        match std::fs::read(&tf)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .filter(|v| v["tree"].is_array())
        {
            Some(v) => Some(v),
            None => {
                let url =
                    format!("https://api.github.com/repos/{repo}/git/trees/{tree}?recursive=1");
                let mut req = ureq::get(&url)
                    .set("User-Agent", "beatbox-rs")
                    .timeout(std::time::Duration::from_secs(60));
                if let Ok(t) =
                    std::env::var("BEATBOX_GITHUB_TOKEN").or_else(|_| std::env::var("GITHUB_TOKEN"))
                {
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
        }
    };
    let verified = tj.is_some();
    let mut blobs: std::collections::HashMap<String, (String, String, u64)> =
        std::collections::HashMap::new();
    let mut direct: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    if let Some(tj) = &tj {
        for it in tj["tree"].as_array().cloned().unwrap_or_default() {
            if it["type"] == "blob" {
                let p = it["path"].as_str().unwrap_or("").to_string();
                if let Some(u) = it["url"].as_str().filter(|u| u.starts_with("https://")) {
                    direct.insert(p.to_lowercase(), u.to_string());
                }
                blobs.insert(
                    p.to_lowercase(),
                    (
                        p,
                        it["sha"].as_str().unwrap_or("").to_string(),
                        it["size"].as_u64().unwrap_or(0),
                    ),
                );
            }
        }
    }
    let _ = branch;
    let mut fetched = 0u64;
    let get = |p: &str, fetched: &mut u64| -> Result<PathBuf> {
        let np = norm_path(p);
        let (rp, sha, size) = if verified {
            blobs
                .get(&np.to_lowercase())
                .cloned()
                .ok_or_else(|| anyhow!("{p} is not in {repo}"))?
        } else {
            (np.clone(), String::new(), 0)
        };
        let local = dir.join(&rp);
        if local.is_file()
            && (size == 0
                || std::fs::metadata(&local)
                    .map(|m| m.len())
                    .unwrap_or(u64::MAX)
                    == size)
        {
            let cached = std::fs::read(&local)?;
            let valid = cached_blob_valid(
                &cached,
                &sha,
                direct.contains_key(&rp.to_lowercase()),
                verified,
            );
            if valid {
                return Ok(local);
            }
        }
        let data = match direct.get(&rp.to_lowercase()) {
            Some(u) => {
                let d = http_bytes(u)?;
                if crate::voice_ai::sha256_hex(&d) != sha {
                    bail!(
                        "{rp}: download from {u} does not match its pinned sha256; not installed"
                    );
                }
                d
            }
            None => http_bytes(&format!(
                "https://raw.githubusercontent.com/{repo}/{tree}/{}",
                url_path(&rp)
            ))?,
        };
        if verified && sha.len() == 40 && git_blob_sha1(&data) != sha {
            bail!("{rp}: content does not match the pinned tree; not installed");
        }
        std::fs::create_dir_all(local.parent().unwrap())?;
        std::fs::write(&local, &data)?;
        *fetched += data.len() as u64;
        Ok(local)
    };
    let flat = flatten_sfz(sfz, &mut |p| {
        if pinned && !e.sfz_text.is_empty() && p == sfz {
            return Ok(e.sfz_text.clone());
        }
        let lp = get(p, &mut fetched)?;
        Ok(String::from_utf8_lossy(&std::fs::read(lp)?).to_string())
    })?;
    let (regions, _) = crate::multisample::parse_sfz(&flat)?;
    let mut regions = crate::multisample::playable_regions(regions);
    // keep at most max_rr round robins (disk and memory); the zones still
    // alternate between the ones kept
    regions.retain(|r| r.seq <= max_rr.max(1));
    let base = dir_of(sfz);
    let mut need: Vec<String> = regions
        .iter()
        .map(|r| norm_path(&format!("{base}/{}", r.sample)))
        .collect();
    need.sort();
    need.dedup();
    let total: u64 = need
        .iter()
        .filter_map(|p| blobs.get(&p.to_lowercase()).map(|b| b.2))
        .sum();
    let missing: u64 = need
        .iter()
        .filter_map(|p| blobs.get(&p.to_lowercase()))
        .filter(|b| !dir.join(&b.0).is_file())
        .map(|b| b.2)
        .sum();
    ensure_free(missing)?;
    if total > max_mb * 1_000_000 {
        bail!("{}: this program needs {:.0} MB of samples, over max_mb {max_mb}; pick a smaller program or raise max_mb", e.id, total as f64 / 1e6);
    }
    for p in &need {
        get(p, &mut fetched)?;
        if fetched > max_mb * 1_000_000 {
            bail!("{}: stopped after {:.0} MB, over max_mb {max_mb} (files so far stay in the library)", e.id, fetched as f64 / 1e6);
        }
    }
    let name = sfz
        .rsplit('/')
        .next()
        .unwrap_or("program.sfz")
        .trim_end_matches(".sfz");
    let out = dir.join(&base).join(format!("{name}.beatbox_flat.sfz"));
    std::fs::write(&out, &flat)?;
    let _ = std::fs::write(dir.join("used"), format!("{}", now()));
    Ok((out, fetched))
}

/// Families and tag words that fit a musical role, for pick_for_role.
fn role_words(role: &str) -> (Vec<&'static str>, Vec<&'static str>) {
    let r = role.to_lowercase().replace([' ', '-'], "_");
    let (f, t): (&[&str], &[&str]) = match r.as_str() {
        "drums" | "kit" | "drum_kit" | "beat" => (&["drums"], &["kit", "drum kit", "full"]),
        "kick" => (&["drums"], &["kick", "bass drum", "kit"]),
        "snare" => (&["drums"], &["snare"]),
        "hats" | "hihat" | "hi_hat" | "cymbals" => (&["drums"], &["hat", "hihat", "cymbal"]),
        "percussion" | "perc" => (
            &["percussion", "world"],
            &["percussion", "conga", "bongo", "shaker", "cajon"],
        ),
        "tabla" => (&["percussion"], &["tabla"]),
        "dhol" | "dholak" => (&["percussion"], &["dholak", "dhol"]),
        "bass" | "electric_bass" => (&["bass"], &["electric", "finger", "pick", "pluck"]),
        "upright_bass" | "double_bass" => (&["bass"], &["upright", "arco", "pizz", "double bass"]),
        "guitar" | "clean_guitar" | "electric_guitar" => {
            (&["guitar"], &["electric", "clean", "di"])
        }
        "overdriven_guitar" | "distorted_guitar" | "rock_guitar" => {
            (&["guitar"], &["electric", "di", "crunch"])
        }
        "acoustic_guitar" => (&["guitar"], &["acoustic", "nylon", "steel"]),
        "piano" | "keys" => (&["piano", "keys"], &["grand", "piano", "upright"]),
        "epiano" | "e_piano" | "rhodes" => (&["keys", "piano"], &["fm", "electric", "rhodes"]),
        "organ" => (&["keys"], &["organ"]),
        "strings" | "pad" => (
            &["strings", "orchestra"],
            &["violins", "strings", "sus", "ensemble"],
        ),
        "cello" => (&["strings"], &["cello"]),
        "brass" | "horns" => (&["brass"], &["trumpet", "trombone", "horn"]),
        "sax" | "saxophone" => (&["winds"], &["sax"]),
        "flute" | "bansuri" => (&["winds"], &["flute", "recorder", "bansuri"]),
        "lead" => (
            &["winds", "synth", "keys"],
            &["lead", "sax", "flute", "harmonica"],
        ),
        "harmonium" => (&["keys", "world"], &["harmonium"]),
        "santoor" => (&["world", "strings"], &["santoor", "dulcimer", "psaltery"]),
        "mallets" | "bells" | "marimba" | "vibes" => {
            (&["mallets"], &["marimba", "vibraphone", "glock", "kalimba"])
        }
        "choir" | "vocals" => (&["choir"], &["choir", "voice"]),
        _ => (&[], &[]),
    };
    (f.to_vec(), t.to_vec())
}

/// Rank catalog entries for a role (kick, drums, bass, guitar, piano ...)
/// in a genre: family fit, tag fit, genre tag, size under max_mb, already
/// installed first among equals. Returns (score, entry, reasons).
pub fn pick_for_role(
    role: &str,
    genre: Option<&str>,
    max_mb: Option<f64>,
) -> Vec<(f32, Entry, Vec<String>)> {
    let (fams, words) = role_words(role);
    let rl = role.to_lowercase();
    let g = genre.map(|g| g.to_lowercase().replace([' ', '-'], "_"));
    let mut out = Vec::new();
    for e in entries() {
        if e.format == "sf2" || e.family.starts_with("ir") || e.status == "listed" {
            continue;
        }
        if let (Some(m), true) = (max_mb, e.bytes > 0) {
            if e.bytes as f64 / 1e6 > m {
                continue;
            }
        }
        let blob = format!("{} {} {}", e.id, e.name, e.tags.join(" ")).to_lowercase();
        let mut s = 0.0f32;
        let mut why = Vec::new();
        if fams.iter().any(|f| e.family.eq_ignore_ascii_case(f)) {
            s += 3.0;
            why.push(format!("family {}", e.family));
        }
        let hits: Vec<&str> = words.iter().copied().filter(|w| blob.contains(w)).collect();
        if !hits.is_empty() {
            s += 1.5 + 0.5 * hits.len().min(3) as f32;
            why.push(format!("tags {}", hits.join("/")));
        } else if fams.is_empty() && blob.contains(&rl) {
            s += 3.0;
            why.push(format!("matches '{rl}'"));
        }
        if s <= 0.0 {
            continue;
        }
        if let Some(g) = &g {
            if e.genres.iter().any(|x| x.eq_ignore_ascii_case(g)) {
                s += 2.0;
                why.push(format!("genre {g}"));
            }
        }
        if blob.contains("noises") || blob.contains("keyswitch") || blob.contains("fx") {
            s -= 1.0;
        }
        if entry_json(&e)["installed"] == true {
            s += 0.5;
            why.push("installed".into());
        }
        out.push((s, e, why));
    }
    out.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.1.bytes.cmp(&b.1.bytes))
    });
    out
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "pick_instrument_for_role",
            description: "Shortlist recorded catalog instruments for a role in a song: role kick|snare|hats|drums|percussion|tabla|dholak|bass|upright_bass|guitar|overdriven_guitar|acoustic_guitar|piano|epiano|organ|strings|cello|brass|sax|flute|lead|harmonium|santoor|mallets|choir (other words match names and tags), genre (a style from search_instruments), max_mb. Ranked by family and tag fit, genre tag, then already-installed and smaller downloads first; each with the reasons. Audition the top few with audition_instrument and choose by ear, then load_instrument {track, catalog_id}.",
            mutates: false,
            schema: || obj(json!({"role": {"type": "string"}, "genre": {"type": "string"}, "max_mb": {"type": "number"}, "limit": {"type": "integer", "description": "default 5"}}), &["role"]),
            run: |_e, a| {
                let role = crate::tools::s_req(a, "role")?;
                let genre = s_opt(a, "genre");
                let picks = pick_for_role(&role, genre.as_deref(), a.get("max_mb").and_then(|v| v.as_f64()));
                let n = picks.len();
                let lim = a.get("limit").and_then(|v| v.as_u64()).unwrap_or(5) as usize;
                let rows: Vec<Value> = picks.into_iter().take(lim).map(|(s, e, why)| {
                    let mut j = entry_json(&e);
                    j["score"] = json!((s * 10.0).round() / 10.0);
                    j["why"] = json!(why);
                    j
                }).collect();
                if rows.is_empty() {
                    return Ok(json!({"role": role, "matches": 0, "results": [], "hint": "nothing in the catalog fits; try search_instruments with a query, or import_from_url / freesound_search"}));
                }
                Ok(json!({"role": role, "genre": genre, "matches": n, "results": rows, "next": "audition_instrument {catalog_id} for the top 2-3, then load_instrument {track, catalog_id}"}))
            },
        },
        Tool {
            name: "search_instruments",
            description: "Search the sound catalog of real recorded instruments and impulse responses (permissive licences only: CC0, CC-BY, MIT-style): query matches name/tags/genres, family filters (gm, guitar, bass, drums, percussion, piano, keys, mallets, strings, brass, winds, choir, world, synth, orchestra, ir_room, ir_cab), genre filters by style (lofi, indie, jazz, bollywood, classical_indian, cinematic ...), max_mb by download size, license by substring (e.g. cc0). Returns ids for fetch_instrument / load_instrument, size, licence and whether each is installed.",
            mutates: false,
            schema: || obj(json!({"query": {"type": "string"}, "family": {"type": "string"}, "genre": {"type": "string", "description": "a style: rap, trap, lofi, pop, indie, rock, metal, jazz, blues, funk, soul, edm, house, ambient, cinematic, orchestral, classical, folk, world, bollywood, classical_indian, latin, afro, reggae, country, gospel"}, "license": {"type": "string"}, "max_mb": {"type": "number", "description": "only items whose download is at most this size (unknown sizes are kept)"}, "limit": {"type": "integer"}}), &[]),
            run: |_e, a| {
                let q = s_opt(a, "query").map(|s| s.to_lowercase());
                let fam = s_opt(a, "family").map(|s| s.to_lowercase());
                let lic = s_opt(a, "license").map(|s| s.to_lowercase());
                let genre = s_opt(a, "genre").map(|s| s.to_lowercase().replace([' ', '-'], "_"));
                let max_mb = a.get("max_mb").and_then(|v| v.as_f64());
                let all = entries();
                let mut hits: Vec<Value> = all
                    .iter()
                    .filter(|e| fam.as_ref().is_none_or(|f| e.family.to_lowercase() == *f || e.tags.iter().any(|t| t.to_lowercase().contains(f.as_str()))))
                    .filter(|e| lic.as_ref().is_none_or(|l| e.license.to_lowercase().contains(l.as_str())))
                    .filter(|e| genre.as_ref().is_none_or(|g| e.genres.iter().any(|x| x.to_lowercase() == *g) || e.tags.iter().any(|t| t.to_lowercase().replace([' ', '-'], "_") == *g)))
                    .filter(|e| max_mb.is_none_or(|m| e.bytes == 0 || e.bytes as f64 / 1e6 <= m))
                    .filter(|e| q.as_ref().is_none_or(|q| q.split_whitespace().all(|w| e.name.to_lowercase().contains(w) || e.id.contains(w) || e.family.to_lowercase().contains(w) || e.genres.iter().any(|t| t.contains(w)) || e.tags.iter().any(|t| t.to_lowercase().contains(w)))))
                    .map(entry_json)
                    .collect();
                let n = hits.len();
                if let Some(l) = a.get("limit").and_then(|v| v.as_u64()) {
                    hits.truncate(l as usize);
                }
                let mut fams: Vec<String> = all.iter().map(|e| e.family.clone()).collect();
                fams.sort();
                fams.dedup();
                Ok(json!({"catalog_size": all.len(), "matches": n, "results": hits, "families": fams, "audition": "audition_instrument {catalog_id} renders a 4 s preview to hear before loading"}))
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
            assert!(
                !l.contains("non-commercial") && !l.contains("noncommercial") && !l.contains("-nc"),
                "{}: {}",
                e.id,
                e.license
            );
            if e.status == "pinned" && !e.file.is_empty() {
                assert_eq!(e.sha256.len(), 64, "{}", e.id);
                assert!(e.url.starts_with("https://"), "{}", e.id);
            }
        }
        let ids: std::collections::HashSet<&str> = all.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids.len(), all.len(), "duplicate ids");
        for e in all.iter().filter(|e| e.format == "sfz_git") {
            let direct = !e.files.is_empty() && e.files.iter().all(|f| f.len() == 4);
            assert!(
                !e.repo.is_empty() && (e.tree.len() == 40 || direct) && e.sfz.ends_with(".sfz"),
                "{}",
                e.id
            );
        }
        // pinned-file programs: every region's sample is pinned, with a checksum
        for e in all.iter().filter(|e| !e.sfz_text.is_empty()) {
            let (regions, _) = crate::multisample::parse_sfz(&e.sfz_text).unwrap();
            let regions = crate::multisample::playable_regions(regions);
            assert!(!regions.is_empty(), "{}", e.id);
            let pinned: std::collections::HashMap<String, &Vec<Value>> = e
                .files
                .iter()
                .map(|f| (f[0].as_str().unwrap_or("").to_lowercase(), f))
                .collect();
            for r in &regions {
                let f = pinned
                    .get(&r.sample.to_lowercase())
                    .unwrap_or_else(|| panic!("{}: {} not pinned", e.id, r.sample));
                let sha = f[1].as_str().unwrap_or("");
                assert!(
                    sha.len() == 40 || (sha.len() == 64 && f.len() == 4),
                    "{}: {}",
                    e.id,
                    r.sample
                );
            }
            assert!(e.bytes > 0, "{}", e.id);
        }
        for fam in [
            "guitar",
            "bass",
            "drums",
            "piano",
            "strings",
            "percussion",
            "world",
            "mallets",
            "keys",
        ] {
            assert!(all.iter().any(|e| e.family == fam), "no {fam}");
        }
    }

    #[test]
    fn pick_for_role_ranks_fitting_families_first() {
        let p = super::pick_for_role("tabla", Some("bollywood"), None);
        assert_eq!(
            p[0].1.id,
            "mmiron_tabla_bols",
            "{:?}",
            p.iter().take(3).map(|x| &x.1.id).collect::<Vec<_>>()
        );
        let p = super::pick_for_role("bass", Some("rock"), Some(400.0));
        assert!(
            !p.is_empty() && p.iter().take(5).all(|x| x.1.family == "bass"),
            "{:?}",
            p.iter().take(5).map(|x| &x.1.id).collect::<Vec<_>>()
        );
        let p = super::pick_for_role("drums", None, None);
        assert!(p.iter().take(5).all(|x| x.1.family == "drums"));
    }

    #[test]
    fn search_by_genre_family_and_size() {
        let mut e = crate::engine::Engine::new(std::env::temp_dir().join("beatbox_catalog_search"));
        let r = e
            .call(
                "search_instruments",
                &json!({"genre": "bollywood", "query": "tabla"}),
            )
            .unwrap();
        assert!(
            r["results"]
                .as_array()
                .unwrap()
                .iter()
                .any(|x| x["id"] == "mmiron_tabla_bols"),
            "{r}"
        );
        let r = e
            .call("search_instruments", &json!({"query": "harmonium"}))
            .unwrap();
        assert!(r["matches"].as_u64().unwrap() >= 1, "{r}");
        let r = e
            .call(
                "search_instruments",
                &json!({"family": "piano", "max_mb": 150}),
            )
            .unwrap();
        for x in r["results"].as_array().unwrap() {
            assert!(
                x["mb"].is_null() || x["mb"].as_f64().unwrap() <= 150.0,
                "{x}"
            );
        }
        let r = e
            .call(
                "search_instruments",
                &json!({"genre": "classical indian", "limit": 3}),
            )
            .unwrap();
        assert!(
            r["matches"].as_u64().unwrap() >= 3 && r["results"].as_array().unwrap().len() == 3,
            "{r}"
        );
        assert!(free_mb().is_some());
    }

    #[test]
    fn sha1_and_git_blob_ids() {
        assert_eq!(sha1_hex(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
        // `git hash-object` of "hello\n"
        assert_eq!(
            git_blob_sha1(b"hello\n"),
            "ce013625030ba8dba906f756967f9e9ca394464a"
        );
    }

    #[test]
    fn sfz_includes_defines_and_playable_regions() {
        let files: std::collections::HashMap<&str, &str> = [
            ("Programs/main.sfz", "#define $VOL -3\n<control> default_path=../Samples/\n#include \"mappings/map.sfz\"\n"),
            ("Programs/mappings/map.sfz", "<group> volume=$VOL\n<region> sample=a.wav key=60 sw_last=24\n<region> sample=b.wav key=60 sw_last=25\n<region> sample=rel.wav key=60 trigger=release\n<region> sample=c.wav key=62 lorand=0 hirand=0.5\n<region> sample=d.wav key=62 lorand=0.5 hirand=1\n"),
        ]
        .into_iter()
        .collect();
        let flat = flatten_sfz("Programs/main.sfz", &mut |p| {
            files
                .get(p)
                .map(|s| s.to_string())
                .ok_or_else(|| anyhow!("missing {p}"))
        })
        .unwrap();
        assert!(flat.contains("volume=-3"), "{flat}");
        let (r, _) = crate::multisample::parse_sfz(&flat).unwrap();
        let r = crate::multisample::playable_regions(r);
        let names: Vec<&str> = r.iter().map(|x| x.sample.as_str()).collect();
        assert!(
            names.contains(&"../Samples/a.wav")
                && !names.contains(&"../Samples/b.wav")
                && !names.iter().any(|n| n.contains("rel")),
            "{names:?}"
        );
        let c = r.iter().find(|x| x.sample.ends_with("c.wav")).unwrap().seq;
        let d = r.iter().find(|x| x.sample.ends_with("d.wav")).unwrap().seq;
        assert_ne!(c, d);
        assert_eq!(norm_path("Programs/../Samples/x.wav"), "Samples/x.wav");
    }
}

#[cfg(test)]
mod recorded_cache_tests {
    use super::*;
    #[test]
    fn same_size_corruption_is_not_a_valid_cached_sample() {
        let real = b"recorded-audio";
        let corrupt = b"corruptd-audio";
        assert_eq!(real.len(), corrupt.len());
        for direct in [false, true] {
            let sha = if direct {
                crate::voice_ai::sha256_hex(real)
            } else {
                git_blob_sha1(real)
            };
            assert!(cached_blob_valid(real, &sha, direct, true));
            assert!(!cached_blob_valid(corrupt, &sha, direct, true));
        }
    }
    #[test]
    fn starter_samples_are_cc0_small_pinned_and_mapped() {
        for id in [
            "vcsl_acoustic_drum_kit",
            "vcsl_upright_piano_yamaha_lite",
            "vcsl_balafon",
        ] {
            let e = entry(id).unwrap();
            assert!(e.license.starts_with("CC0"));
            assert!(e.bytes < 100_000_000);
            assert!(!e.files.is_empty());
            assert!(!e.sfz_text.is_empty());
            assert!(e.source.starts_with("https://"));
            let (regions, _) = crate::multisample::parse_sfz(&e.sfz_text).unwrap();
            assert!(!regions.is_empty());
        }
    }
    #[test]
    fn starter_manifests_cover_regions_and_default_download_sizes() {
        for id in [
            "vcsl_acoustic_drum_kit",
            "vcsl_upright_piano_yamaha_lite",
            "vcsl_balafon",
        ] {
            let e = entry(id).unwrap();
            let (regions, _) = crate::multisample::parse_sfz(&e.sfz_text).unwrap();
            let regions = crate::multisample::playable_regions(regions);
            let mut files = std::collections::HashMap::new();
            for f in &e.files {
                let path = f[0].as_str().unwrap();
                assert!(
                    files.insert(path, f[2].as_u64().unwrap()).is_none(),
                    "duplicate {path}"
                );
                assert_eq!(f[1].as_str().unwrap().len(), 40);
                assert!(regions.iter().any(|r| r.sample == path), "unused {path}");
            }
            let mut default_paths = std::collections::HashSet::new();
            for r in &regions {
                assert!(
                    files.contains_key(r.sample.as_str()),
                    "unlisted {}",
                    r.sample
                );
                assert!(r.lo <= r.hi && r.vel_lo <= r.vel_hi);
                if r.seq <= 2 {
                    default_paths.insert(r.sample.as_str());
                }
            }
            assert_eq!(
                default_paths.iter().map(|p| files[p]).sum::<u64>(),
                e.bytes,
                "{id}"
            );
        }
    }
    #[test]
    fn starter_mappings_have_no_promised_key_or_velocity_gaps() {
        for id in [
            "vcsl_acoustic_drum_kit",
            "vcsl_upright_piano_yamaha_lite",
            "vcsl_balafon",
        ] {
            let e = entry(id).unwrap();
            let (regions, _) = crate::multisample::parse_sfz(&e.sfz_text).unwrap();
            let regions = crate::multisample::playable_regions(regions);
            let keys: Vec<u8> = if e.family == "drums" {
                vec![36, 37, 38, 39, 40, 42, 44, 45, 46, 48, 49, 51, 53]
            } else {
                (0..=127).collect()
            };
            for k in keys {
                for v in 1..=127 {
                    assert!(
                        regions.iter().any(|r| r.lo <= k
                            && k <= r.hi
                            && r.vel_lo <= v
                            && v <= r.vel_hi
                            && r.seq == 1),
                        "{id}: key {k} velocity {v}"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod pinned_file_cache_tests {
    use super::*;
    #[test]
    fn fetch_reuses_verified_cache_but_rejects_same_size_damage_without_replacing_it() {
        let dir = std::env::temp_dir().join(format!("beatbox-fetch-cache-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("recording.bin");
        let data = b"recorded-audio";
        let mut e = entries().into_iter().find(|e| !e.file.is_empty()).unwrap();
        // Absolute scratch id avoids global library environment changes.
        e.id = dir.to_string_lossy().into_owned();
        e.file = "recording.bin".into();
        e.bytes = data.len() as u64;
        e.sha256 = crate::voice_ai::sha256_hex(data);
        e.url = "offline-invalid://no-download".into();
        std::fs::write(&file, data).unwrap();
        assert_eq!(fetch(&e).unwrap(), file);
        assert_eq!(std::fs::read(&file).unwrap(), data);
        let corrupt = b"corruptd-audio";
        assert_eq!(corrupt.len(), data.len());
        std::fs::write(&file, corrupt).unwrap();
        assert!(fetch(&e).is_err());
        assert_eq!(std::fs::read(&file).unwrap(), corrupt);
        assert!(!file.with_extension("part").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn cached_file_requires_correct_pinned_content_and_known_size() {
        let dir = std::env::temp_dir().join(format!("beatbox-pinned-cache-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("recording.bin");
        let data = b"recorded-audio";
        let sha = crate::voice_ai::sha256_hex(data);
        assert!(!cached_file_valid(&file, &sha, data.len() as u64));
        std::fs::write(&file, data).unwrap();
        assert!(cached_file_valid(&file, &sha, data.len() as u64));
        assert!(cached_file_valid(&file, &sha, 0));
        assert!(!cached_file_valid(&file, &sha, data.len() as u64 + 1));
        assert!(!cached_file_valid(&file, "", data.len() as u64));
        assert!(!cached_file_valid(&dir, &sha, 0));
        let corrupt = b"corruptd-audio";
        assert_eq!(data.len(), corrupt.len());
        std::fs::write(&file, corrupt).unwrap();
        assert!(!cached_file_valid(&file, &sha, data.len() as u64));
        assert!(!cached_file_valid(&file, &sha, 0));
        assert_eq!(std::fs::read(&file).unwrap(), corrupt);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
