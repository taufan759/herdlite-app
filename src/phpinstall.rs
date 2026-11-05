//! Unduh & pasang PHP resmi dari windows.php.net (build NTS x64), plus shim php/composer.
//! Memakai curl.exe dan tar.exe bawaan Windows 10/11, jadi tidak menambah ukuran exe.

use crate::config::{ver_key, Config};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

const RELEASES: &str = "https://windows.php.net/downloads/releases/releases.json";
const BASE: &str = "https://windows.php.net/downloads/releases/";
const CACERT: &str = "https://curl.se/ca/cacert.pem";
const COMPOSER: &str = "https://getcomposer.org/download/latest-stable/composer.phar";

fn sys_tool(name: &str) -> PathBuf {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    PathBuf::from(root).join("System32").join(name)
}

fn curl_text(url: &str) -> Result<String, String> {
    let out = Command::new(sys_tool("curl.exe"))
        .args(["-fsSL", url])
        .output()
        .map_err(|e| format!("curl.exe tidak bisa dijalankan: {e}"))?;
    if !out.status.success() {
        return Err(format!("gagal mengunduh {url}: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn download(url: &str, dest: &Path) -> Result<(), String> {
    if let Some(d) = dest.parent() {
        fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let st = Command::new(sys_tool("curl.exe"))
        .args(["-fL", "--progress-bar", "-o"])
        .arg(dest)
        .arg(url)
        .status()
        .map_err(|e| format!("curl.exe tidak bisa dijalankan: {e}"))?;
    if !st.success() {
        let _ = fs::remove_file(dest);
        return Err(format!("gagal mengunduh {url}"));
    }
    Ok(())
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

pub struct Release {
    pub version: String,
    pub zip: String,
    pub sha256: String,
}

/// Versi PHP yang tersedia (yang masih didukung) di windows.php.net.
pub fn available() -> Result<Vec<Release>, String> {
    let v: serde_json::Value = serde_json::from_str(&curl_text(RELEASES)?).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    if let Some(obj) = v.as_object() {
        for (minor, info) in obj {
            let Some(o) = info.as_object() else { continue };
            let full = o.get("version").and_then(|x| x.as_str()).unwrap_or("").to_string();
            for (k, build) in o {
                if k.starts_with("nts-") && k.ends_with("-x64") {
                    let zip = build["zip"]["path"].as_str().unwrap_or("").to_string();
                    let sha = build["zip"]["sha256"].as_str().unwrap_or("").to_string();
                    if !zip.is_empty() {
                        out.push(Release { version: if full.is_empty() { minor.clone() } else { full.clone() }, zip, sha256: sha });
                    }
                }
            }
        }
    }
    out.sort_by_key(|r| ver_key(&r.version));
    Ok(out)
}

const EXTENSIONS: &[&str] = &[
    "bz2", "curl", "fileinfo", "gd", "gmp", "intl", "mbstring", "exif", "mysqli", "openssl", "pdo_mysql",
    "pdo_pgsql", "pdo_sqlite", "pgsql", "sodium", "sqlite3", "zip", "soap", "sockets",
];

/// Tulis php.ini (dari php.ini-development + blok herdlite di akhir).
pub fn write_ini(home: &Path, dir: &Path) -> Result<(), String> {
    let base = fs::read_to_string(dir.join("php.ini-development")).unwrap_or_default();
    let ext_dir = dir.join("ext");
    let cacert = home.join("etc").join("cacert.pem");
    let mut s = base;
    s.push_str("\r\n\r\n; ===== herdlite (setelan di bawah menimpa yang di atas) =====\r\n[PHP]\r\n");
    s.push_str(&format!("extension_dir = \"{}\"\r\n", ext_dir.display()));
    for e in EXTENSIONS {
        if ext_dir.join(format!("php_{e}.dll")).is_file() {
            s.push_str(&format!("extension={e}\r\n"));
        }
    }
    if ext_dir.join("php_opcache.dll").is_file() {
        s.push_str("zend_extension=opcache\r\n");
    }
    s.push_str(
        "memory_limit = 512M\r\nupload_max_filesize = 128M\r\npost_max_size = 128M\r\nmax_execution_time = 120\r\n\
display_errors = On\r\nlog_errors = On\r\nerror_reporting = E_ALL\r\nvariables_order = \"EGPCS\"\r\n\
opcache.enable = 1\r\nopcache.enable_cli = 0\r\nopcache.validate_timestamps = 1\r\nopcache.revalidate_freq = 0\r\n\
opcache.memory_consumption = 64\r\nopcache.interned_strings_buffer = 8\r\nrealpath_cache_size = 4096K\r\n",
    );
    if cacert.is_file() {
        s.push_str(&format!("curl.cainfo = \"{0}\"\r\nopenssl.cafile = \"{0}\"\r\n", cacert.display()));
    }
    fs::write(dir.join("php.ini"), s).map_err(|e| e.to_string())
}

pub fn install(home: &Path, ver: &str, force: bool) -> Result<String, String> {
    let rel = available()?
        .into_iter()
        .filter(|r| r.version == ver || r.version.starts_with(&format!("{ver}.")))
        .last()
        .ok_or_else(|| format!("PHP {ver} tidak tersedia di windows.php.net (lihat: hl php:available)"))?;
    let dest = home.join("php").join(ver);
    if dest.join("php-cgi.exe").is_file() && !force {
        return Err(format!("PHP {ver} sudah terpasang di {}. Pakai --force untuk memasang ulang.", dest.display()));
    }
    let dl = home.join("downloads").join(&rel.zip);
    println!("Mengunduh PHP {} ({})", rel.version, rel.zip);
    download(&format!("{BASE}{}", rel.zip), &dl)?;
    if !rel.sha256.is_empty() {
        let got = sha256_hex(&dl)?;
        if !got.eq_ignore_ascii_case(&rel.sha256) {
            let _ = fs::remove_file(&dl);
            return Err(format!("checksum SHA-256 tidak cocok (diharapkan {}, didapat {got})", rel.sha256));
        }
        println!("Checksum SHA-256 cocok.");
    }
    let tmp = home.join("php").join(format!(".tmp-{ver}"));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let st = Command::new(sys_tool("tar.exe"))
        .arg("-xf")
        .arg(&dl)
        .arg("-C")
        .arg(&tmp)
        .status()
        .map_err(|e| format!("tar.exe tidak bisa dijalankan: {e}"))?;
    if !st.success() || !tmp.join("php-cgi.exe").is_file() {
        let _ = fs::remove_dir_all(&tmp);
        return Err("gagal mengekstrak zip PHP".into());
    }
    if dest.exists() {
        fs::remove_dir_all(&dest).map_err(|e| format!("gagal menghapus versi lama (masih dipakai? jalankan hl stop dulu): {e}"))?;
    }
    fs::rename(&tmp, &dest).map_err(|e| e.to_string())?;
    let _ = fs::remove_file(&dl);
    ensure_cacert(home)?;
    write_ini(home, &dest)?;
    Ok(rel.version)
}

pub fn ensure_cacert(home: &Path) -> Result<(), String> {
    let p = home.join("etc").join("cacert.pem");
    if !p.is_file() {
        println!("Mengunduh daftar CA (cacert.pem) untuk curl/openssl PHP");
        download(CACERT, &p)?;
    }
    Ok(())
}

pub fn install_composer(home: &Path) -> Result<PathBuf, String> {
    let p = home.join("bin").join("composer.phar");
    download(COMPOSER, &p)?;
    Ok(p)
}

/// Lokasi laravel.bat dari `composer global require laravel/installer`, kalau ada.
pub fn laravel_bat() -> Option<PathBuf> {
    let base = std::env::var("COMPOSER_HOME")
        .map(PathBuf::from)
        .or_else(|_| std::env::var("APPDATA").map(|a| PathBuf::from(a).join("Composer")))
        .ok()?;
    let p = base.join("vendor").join("bin").join("laravel.bat");
    p.is_file().then_some(p)
}

/// Pasang Laravel installer lewat composer global, lalu buat shim laravel.cmd.
pub fn install_laravel(home: &Path, cfg: &Config) -> Result<PathBuf, String> {
    let g = cfg.global_php(home).ok_or("pasang PHP dulu: hl php:install 8.3")?;
    let php = cfg.php_dir(home, &g).join("php.exe");
    let phar = home.join("bin").join("composer.phar");
    if !phar.is_file() {
        install_composer(home)?;
    }
    let st = Command::new(&php)
        .arg(&phar)
        .args(["global", "require", "laravel/installer", "--no-interaction"])
        .status()
        .map_err(|e| e.to_string())?;
    if !st.success() {
        return Err("composer global require laravel/installer gagal".into());
    }
    write_shims(home, cfg)?;
    laravel_bat().ok_or_else(|| "laravel.bat tidak ditemukan setelah instalasi".into())
}

/// Tulis shim di <home>\bin: php.cmd (versi global), phpXY.cmd, composer.cmd.
pub fn write_shims(home: &Path, cfg: &Config) -> Result<(), String> {
    let bin = home.join("bin");
    fs::create_dir_all(&bin).map_err(|e| e.to_string())?;
    if let Some(g) = cfg.global_php(home) {
        let exe = cfg.php_dir(home, &g).join("php.exe");
        fs::write(bin.join("php.cmd"), format!("@\"{}\" %*\r\n", exe.display())).map_err(|e| e.to_string())?;
        if bin.join("composer.phar").is_file() {
            fs::write(bin.join("composer.cmd"), format!("@\"{}\" \"%~dp0composer.phar\" %*\r\n", exe.display()))
                .map_err(|e| e.to_string())?;
        }
    }
    if let Some(l) = laravel_bat() {
        fs::write(bin.join("laravel.cmd"), format!("@\"{}\" %*\r\n", l.display())).map_err(|e| e.to_string())?;
    }
    for v in cfg.installed_php(home) {
        let exe = cfg.php_dir(home, &v).join("php.exe");
        let name = format!("php{}.cmd", v.replace('.', ""));
        fs::write(bin.join(name), format!("@\"{}\" %*\r\n", exe.display())).map_err(|e| e.to_string())?;
    }
    Ok(())
}
