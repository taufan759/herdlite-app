//! Pengelola Node.js ringan (pengganti nvm bawaan Herd).
//!
//! Versi dipasang dari nodejs.org ke `<home>\node\<versi>` dan versi aktif
//! ditunjuk oleh junction `<home>\node\current` (junction tidak butuh admin,
//! berbeda dengan symlink `C:\Program Files\nodejs` milik nvm-windows).
//! Paket npm global ikut tersimpan di folder versinya, sama seperti nvm.

use crate::phpinstall::download;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

const DIST: &str = "https://nodejs.org/dist/";

pub fn node_root(home: &Path) -> PathBuf {
    home.join("node")
}

pub fn current_link(home: &Path) -> PathBuf {
    node_root(home).join("current")
}

fn sys(name: &str) -> PathBuf {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    PathBuf::from(root).join("System32").join(name)
}

fn curl_text(url: &str) -> Result<String, String> {
    let out = Command::new(sys("curl.exe")).args(["-fsSL", url]).output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!("gagal mengunduh {url}"));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn vkey(v: &str) -> (u32, u32, u32) {
    let mut it = v.trim_start_matches('v').split('.').map(|x| x.parse::<u32>().unwrap_or(0));
    (it.next().unwrap_or(0), it.next().unwrap_or(0), it.next().unwrap_or(0))
}

pub struct Release {
    pub version: String,
    pub lts: Option<String>,
}

/// Daftar rilis Node untuk Windows x64 (terbaru dulu).
pub fn available() -> Result<Vec<Release>, String> {
    let v: serde_json::Value = serde_json::from_str(&curl_text(&format!("{DIST}index.json"))?).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for r in v.as_array().cloned().unwrap_or_default() {
        let has_zip = r["files"].as_array().map(|f| f.iter().any(|x| x.as_str() == Some("win-x64-zip"))).unwrap_or(false);
        if !has_zip {
            continue;
        }
        let version = r["version"].as_str().unwrap_or("").trim_start_matches('v').to_string();
        let lts = r["lts"].as_str().map(|s| s.to_string());
        out.push(Release { version, lts });
    }
    out.sort_by_key(|r| std::cmp::Reverse(vkey(&r.version)));
    Ok(out)
}

/// "26" → 26.x terbaru, "24.1" → 24.1.x terbaru, "lts" → LTS terbaru, "26.12.1" → persis.
pub fn resolve(spec: &str) -> Result<String, String> {
    let spec = spec.trim().trim_start_matches('v').to_lowercase();
    let list = available()?;
    let hit = if spec == "lts" {
        list.iter().find(|r| r.lts.is_some())
    } else if spec == "latest" || spec == "current" {
        list.first()
    } else {
        list.iter().find(|r| r.version == spec || r.version.starts_with(&format!("{spec}.")))
    };
    hit.map(|r| r.version.clone()).ok_or_else(|| format!("Node {spec} tidak ditemukan di nodejs.org"))
}

/// Versi terpasang (urut naik).
pub fn installed(home: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(node_root(home))
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().join("node.exe").is_file() && e.file_name() != "current")
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default();
    v.sort_by_key(|x| vkey(x));
    v
}

/// Versi yang sedang aktif (target junction `current`).
pub fn active(home: &Path) -> Option<String> {
    let t = fs::read_link(current_link(home)).ok()?;
    let name = t.file_name()?.to_string_lossy().to_string();
    Some(name)
}

fn sha256_hex(path: &Path) -> Result<String, String> {
    let mut f = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        ctx.update(&buf[..n]);
    }
    Ok(ctx.finish().as_ref().iter().map(|b| format!("{b:02x}")).collect())
}

pub fn install(home: &Path, spec: &str) -> Result<String, String> {
    let ver = resolve(spec)?;
    let dest = node_root(home).join(&ver);
    if dest.join("node.exe").is_file() {
        return Err(format!("Node {ver} sudah terpasang. Aktifkan: hl node:use {ver}"));
    }
    let zip_name = format!("node-v{ver}-win-x64.zip");
    let base = format!("{DIST}v{ver}/");
    let sums = curl_text(&format!("{base}SHASUMS256.txt"))?;
    let want = sums
        .lines()
        .find_map(|l| {
            let mut it = l.split_whitespace();
            let (h, n) = (it.next()?, it.next()?);
            (n == zip_name).then(|| h.to_string())
        })
        .ok_or("checksum tidak ditemukan di SHASUMS256.txt")?;
    let dl = home.join("downloads").join(&zip_name);
    println!("Mengunduh Node {ver} ({zip_name})");
    download(&format!("{base}{zip_name}"), &dl)?;
    let got = sha256_hex(&dl)?;
    if !got.eq_ignore_ascii_case(&want) {
        let _ = fs::remove_file(&dl);
        return Err(format!("checksum SHA-256 tidak cocok (diharapkan {want}, didapat {got})"));
    }
    println!("Checksum SHA-256 cocok.");
    let tmp = node_root(home).join(format!(".tmp-{ver}"));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let st = Command::new(sys("tar.exe")).arg("-xf").arg(&dl).arg("-C").arg(&tmp).status().map_err(|e| e.to_string())?;
    let inner = tmp.join(format!("node-v{ver}-win-x64"));
    if !st.success() || !inner.join("node.exe").is_file() {
        let _ = fs::remove_dir_all(&tmp);
        return Err("gagal mengekstrak zip Node".into());
    }
    fs::rename(&inner, &dest).map_err(|e| e.to_string())?;
    let _ = fs::remove_dir_all(&tmp);
    let _ = fs::remove_file(&dl);
    Ok(ver)
}

/// Arahkan junction `current` ke versi tertentu ("26" → versi 26.x terpasang yang tertinggi).
pub fn use_version(home: &Path, spec: &str) -> Result<String, String> {
    let spec = spec.trim().trim_start_matches('v');
    let ver = installed(home)
        .into_iter()
        .rev()
        .find(|v| v == spec || v.starts_with(&format!("{spec}.")))
        .ok_or_else(|| format!("Node {spec} belum terpasang. Jalankan: hl node:install {spec}"))?;
    let link = current_link(home);
    if link.exists() || fs::symlink_metadata(&link).is_ok() {
        // remove_dir pada junction hanya menghapus tautannya, bukan isi folder tujuan.
        fs::remove_dir(&link).map_err(|e| format!("gagal mengganti {}: {e}", link.display()))?;
    }
    let target = node_root(home).join(&ver);
    let out = Command::new(crate::win::sys("cmd.exe"))
        .args(["/C", "mklink", "/J"])
        .arg(&link)
        .arg(&target)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!("gagal membuat junction: {}", String::from_utf8_lossy(&out.stdout).trim()));
    }
    Ok(ver)
}
