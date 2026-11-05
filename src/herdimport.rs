//! Impor pengaturan dari Laravel Herd (hanya membaca, tidak mengubah apa pun milik Herd).

use crate::config::{norm, parse_ver, Config};
use crate::util::clean_abs;
use std::fs;
use std::path::{Path, PathBuf};

pub struct Report {
    pub paths: Vec<String>,
    pub links: Vec<(String, String)>,
    pub isolated: Vec<(String, String)>,
    pub secured: Vec<String>,
    pub default_php: Option<String>,
    pub herd_php: Vec<(String, String)>,
}

pub fn herd_home() -> PathBuf {
    let up = std::env::var("USERPROFILE").unwrap_or_default();
    PathBuf::from(up).join(".config").join("herd")
}

pub fn read() -> Result<Report, String> {
    let h = herd_home();
    let valet = h.join("config").join("valet");
    let vc: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(valet.join("config.json")).map_err(|e| format!("konfigurasi Herd tidak ditemukan di {}: {e}", valet.display()))?,
    )
    .map_err(|e| e.to_string())?;
    let sites_dir = valet.join("Sites");
    let mut paths = Vec::new();
    for p in vc["paths"].as_array().cloned().unwrap_or_default() {
        let Some(p) = p.as_str() else { continue };
        let pb = PathBuf::from(p);
        // Folder "Sites" milik Herd berisi symlink → diimpor sebagai link, bukan park.
        if crate::util::path_key(&pb) == crate::util::path_key(&sites_dir) || !pb.is_dir() {
            continue;
        }
        paths.push(clean_abs(&pb));
    }

    let mut links = Vec::new();
    if let Ok(rd) = fs::read_dir(&sites_dir) {
        for e in rd.flatten() {
            let name = norm(&e.file_name().to_string_lossy());
            let target = fs::read_link(e.path()).map(|t| if t.is_absolute() { t } else { sites_dir.join(t) }).unwrap_or_else(|_| e.path());
            let target = clean_abs(&target);
            if !Path::new(&target).is_dir() {
                continue;
            }
            // Link yang namanya sama dengan folder di path park tidak perlu diimpor.
            let folder = Path::new(&target).file_name().map(|f| norm(&f.to_string_lossy())).unwrap_or_default();
            let parent = Path::new(&target).parent().map(clean_abs).unwrap_or_default();
            let redundant = folder == name && paths.iter().any(|p| crate::util::path_key(Path::new(p)) == crate::util::path_key(Path::new(&parent)));
            if !redundant {
                links.push((name, target));
            }
        }
    }

    let mut isolated = Vec::new();
    if let Ok(rd) = fs::read_dir(valet.join("Nginx")) {
        for e in rd.flatten() {
            let fname = e.file_name().to_string_lossy().to_string();
            let Ok(text) = fs::read_to_string(e.path()) else { continue };
            if let Some(v) = text.lines().find_map(|l| l.trim().strip_prefix("# ISOLATED_PHP_VERSION=")) {
                let site = fname.trim_end_matches(".conf").trim_end_matches(".test");
                if let Some(v) = parse_ver(v) {
                    isolated.push((norm(site), v));
                }
            }
        }
    }

    let mut secured = Vec::new();
    if let Ok(rd) = fs::read_dir(valet.join("Certificates")) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if let Some(s) = n.strip_suffix(".test.crt") {
                secured.push(norm(s));
            }
        }
    }

    let default_php = fs::read_to_string(h.join("config").join("config.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v["activeVersion"].as_str().and_then(parse_ver));

    let mut herd_php = Vec::new();
    if let Ok(rd) = fs::read_dir(h.join("bin")) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if let Some(v) = n.strip_prefix("php").and_then(parse_ver) {
                if e.path().join("php-cgi.exe").is_file() {
                    herd_php.push((v, clean_abs(&e.path())));
                }
            }
        }
    }

    Ok(Report { paths, links, isolated, secured, default_php, herd_php })
}

/// Gabungkan hasil impor ke konfigurasi herdlite.
pub fn apply(cfg: &mut Config, r: &Report, use_herd_php: bool) {
    for p in &r.paths {
        if !cfg.paths.iter().any(|x| crate::util::path_key(Path::new(x)) == crate::util::path_key(Path::new(p))) {
            cfg.paths.push(p.clone());
        }
    }
    for (n, t) in &r.links {
        cfg.links.entry(n.clone()).or_insert_with(|| t.clone());
    }
    for (n, v) in &r.isolated {
        cfg.isolated.entry(n.clone()).or_insert_with(|| v.clone());
    }
    for s in &r.secured {
        if !cfg.secured.contains(s) {
            cfg.secured.push(s.clone());
        }
    }
    if cfg.default_php.is_empty() {
        if let Some(v) = &r.default_php {
            cfg.default_php = v.clone();
        }
    }
    if use_herd_php {
        for (v, d) in &r.herd_php {
            cfg.php_dirs.entry(v.clone()).or_insert_with(|| d.clone());
        }
    }
}
