//! Konfigurasi herdlite (`<home>\config.json`) dan resolusi situs.

use crate::util::{clean_abs, path_key};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Config {
    /// TLD lokal, default "test" → <folder>.test
    pub tld: String,
    /// Alamat yang didengarkan server.
    pub bind: String,
    pub http_port: u16,
    /// 0 = HTTPS mati.
    pub https_port: u16,
    /// Versi PHP global, mis. "8.3". Kosong = versi terbaru yang terpasang.
    pub default_php: String,
    /// Folder yang di-"park": setiap subfolder jadi <subfolder>.test
    pub paths: Vec<String>,
    /// Situs yang di-"link": nama → folder.
    pub links: BTreeMap<String, String>,
    /// Versi PHP khusus per situs.
    pub isolated: BTreeMap<String, String>,
    /// Situs yang dipaksa HTTPS (http → redirect ke https).
    pub secured: Vec<String>,
    /// Paksa HTTPS untuk semua situs.
    pub secure_all: bool,
    /// Folder PHP di luar <home>\php\<versi> (opsional).
    pub php_dirs: BTreeMap<String, String>,
    /// Jumlah worker php-cgi per versi PHP.
    pub php_children: u32,
    /// Matikan php-cgi yang menganggur setelah N menit (0 = tidak pernah).
    pub php_idle_minutes: u64,
    /// Batas ukuran body request (MB).
    pub max_body_mb: u64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            tld: "test".into(),
            bind: "127.0.0.1".into(),
            http_port: 80,
            https_port: 443,
            default_php: String::new(),
            paths: vec![],
            links: BTreeMap::new(),
            isolated: BTreeMap::new(),
            secured: vec![],
            secure_all: false,
            php_dirs: BTreeMap::new(),
            php_children: 2,
            php_idle_minutes: 30,
            max_body_mb: 512,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Parked,
    Linked,
}

#[derive(Clone, Debug)]
pub struct Site {
    /// Nama situs (huruf kecil, `_` → `-`), host = <name>.<tld>
    pub name: String,
    /// Nama folder asli (untuk alias host dengan underscore).
    pub folder: String,
    pub path: PathBuf,
    pub kind: Kind,
}

/// Normalisasi nama situs: huruf kecil, underscore → strip.
pub fn norm(s: &str) -> String {
    s.trim().to_lowercase().replace('_', "-")
}

/// Nama situs yang sah: label DNS (huruf/angka/'-') dipisah titik, tanpa label kosong.
/// Menolak ".", "..", "...", ".a", "a." sehingga nama tidak bisa menunjuk ke luar folder park.
fn valid_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 200
        && n.split('.').all(|l| {
            !l.is_empty() && !l.starts_with('-') && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
}

/// Folder data herdlite.
/// Urutan: env HERDLITE_HOME → folder induk dari `bin\` tempat exe berada → %USERPROFILE%\.herdlite
pub fn home() -> PathBuf {
    if let Ok(h) = std::env::var("HERDLITE_HOME") {
        if !h.trim().is_empty() {
            return PathBuf::from(h.trim());
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            if dir.file_name().map(|n| n.eq_ignore_ascii_case("bin")).unwrap_or(false) {
                if let Some(p) = dir.parent() {
                    return p.to_path_buf();
                }
            }
        }
    }
    let up = std::env::var("USERPROFILE").unwrap_or_else(|_| ".".into());
    PathBuf::from(up).join(".herdlite")
}

pub fn config_path(home: &Path) -> PathBuf {
    home.join("config.json")
}

/// Versi "8.3" → (8, 3) untuk pengurutan.
pub fn ver_key(v: &str) -> (u32, u32) {
    let mut it = v.split('.').map(|x| x.parse::<u32>().unwrap_or(0));
    (it.next().unwrap_or(0), it.next().unwrap_or(0))
}

/// Terima "8.3", "83", "php83", "8.3.12" → "8.3".
pub fn parse_ver(v: &str) -> Option<String> {
    let v = v.trim().trim_start_matches("php").trim_start_matches('@');
    let parts: Vec<&str> = v.split('.').collect();
    let (a, b) = if parts.len() >= 2 {
        (parts[0].parse::<u32>().ok()?, parts[1].parse::<u32>().ok()?)
    } else if v.len() == 2 && v.chars().all(|c| c.is_ascii_digit()) {
        (v[..1].parse().ok()?, v[1..].parse().ok()?)
    } else {
        return None;
    };
    if !(5..=9).contains(&a) {
        return None;
    }
    Some(format!("{a}.{b}"))
}

/// Folder dokumen: `public\` kalau berisi index.php/index.html (Laravel dkk), selain itu root situs.
pub fn docroot(site: &Path) -> PathBuf {
    let p = site.join("public");
    if p.join("index.php").is_file() || p.join("index.html").is_file() {
        p
    } else {
        site.to_path_buf()
    }
}

impl Config {
    pub fn load(home: &Path) -> Result<Config, String> {
        match fs::read_to_string(config_path(home)) {
            Ok(s) => serde_json::from_str(s.trim_start_matches('\u{feff}'))
                .map_err(|e| format!("config.json rusak: {e}")),
            Err(_) => Ok(Config::default()),
        }
    }

    pub fn save(&self, home: &Path) -> Result<(), String> {
        fs::create_dir_all(home).map_err(|e| e.to_string())?;
        let p = config_path(home);
        let tmp = home.join("config.json.tmp");
        let s = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        fs::write(&tmp, s).map_err(|e| format!("gagal menulis {}: {e}", tmp.display()))?;
        fs::rename(&tmp, &p).map_err(|e| format!("gagal menulis {}: {e}", p.display()))
    }

    pub fn php_dir(&self, home: &Path, ver: &str) -> PathBuf {
        match self.php_dirs.get(ver) {
            Some(d) => PathBuf::from(d),
            None => home.join("php").join(ver),
        }
    }

    /// Versi PHP yang terpasang (punya php-cgi.exe), urut naik.
    pub fn installed_php(&self, home: &Path) -> Vec<String> {
        let mut set: HashSet<String> = HashSet::new();
        if let Ok(rd) = fs::read_dir(home.join("php")) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().to_string();
                if parse_ver(&n).as_deref() == Some(n.as_str()) && e.path().join("php-cgi.exe").is_file() {
                    set.insert(n);
                }
            }
        }
        for (v, d) in &self.php_dirs {
            if Path::new(d).join("php-cgi.exe").is_file() {
                set.insert(v.clone());
            }
        }
        let mut v: Vec<String> = set.into_iter().collect();
        v.sort_by_key(|x| ver_key(x));
        v
    }

    pub fn global_php(&self, home: &Path) -> Option<String> {
        if !self.default_php.is_empty() {
            return Some(self.default_php.clone());
        }
        self.installed_php(home).pop()
    }

    pub fn php_for(&self, home: &Path, site: &str) -> Option<String> {
        self.isolated.get(&norm(site)).cloned().or_else(|| self.global_php(home))
    }

    pub fn is_secured(&self, site: &str) -> bool {
        self.secure_all || self.secured.iter().any(|s| *s == norm(site))
    }

    pub fn host(&self, site: &str) -> String {
        format!("{}.{}", site, self.tld)
    }

    pub fn url(&self, site: &str) -> String {
        let secure = self.is_secured(site) && self.https_port != 0;
        let (scheme, port, def) = if secure { ("https", self.https_port, 443) } else { ("http", self.http_port, 80) };
        if port == def {
            format!("{scheme}://{}", self.host(site))
        } else {
            format!("{scheme}://{}:{port}", self.host(site))
        }
    }

    /// Semua situs: link dulu, lalu isi folder park.
    pub fn sites(&self) -> Vec<Site> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for (name, path) in &self.links {
            if seen.insert(name.clone()) {
                let p = PathBuf::from(path);
                let folder = p.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
                out.push(Site { name: name.clone(), folder, path: p, kind: Kind::Linked });
            }
        }
        for park in &self.paths {
            let Ok(rd) = fs::read_dir(park) else { continue };
            for e in rd.flatten() {
                let folder = e.file_name().to_string_lossy().to_string();
                if folder.starts_with('.') {
                    continue;
                }
                let path = e.path();
                if !path.is_dir() {
                    continue;
                }
                let n = norm(&folder);
                if valid_name(&n) && seen.insert(n.clone()) {
                    out.push(Site { name: n, folder, path, kind: Kind::Parked });
                }
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// Cari situs dari nama (bagian host sebelum .tld).
    pub fn find_site(&self, name: &str) -> Option<Site> {
        let n = norm(name);
        if n.is_empty() || !valid_name(&n) {
            return None;
        }
        if let Some(p) = self.links.get(&n) {
            let p = PathBuf::from(p);
            let folder = p.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
            return Some(Site { name: n, folder, path: p, kind: Kind::Linked });
        }
        for park in &self.paths {
            let direct = Path::new(park).join(name);
            if direct.is_dir() {
                return Some(Site { name: n, folder: name.to_string(), path: direct, kind: Kind::Parked });
            }
            let Ok(rd) = fs::read_dir(park) else { continue };
            for e in rd.flatten() {
                let folder = e.file_name().to_string_lossy().to_string();
                if norm(&folder) == n && e.path().is_dir() {
                    return Some(Site { name: n, folder, path: e.path(), kind: Kind::Parked });
                }
            }
        }
        None
    }

    /// Situs yang memuat folder `dir` (untuk perintah yang dijalankan dari dalam folder proyek).
    pub fn site_for_dir(&self, dir: &Path) -> Option<Site> {
        let d = path_key(Path::new(&clean_abs(dir)));
        let mut best: Option<(usize, Site)> = None;
        for s in self.sites() {
            let sp = path_key(Path::new(&clean_abs(&s.path)));
            if d == sp || d.starts_with(&format!("{sp}\\")) {
                if best.as_ref().map(|(l, _)| sp.len() > *l).unwrap_or(true) {
                    best = Some((sp.len(), s));
                }
            }
        }
        best.map(|(_, s)| s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn versions() {
        assert_eq!(parse_ver("8.3").as_deref(), Some("8.3"));
        assert_eq!(parse_ver("83").as_deref(), Some("8.3"));
        assert_eq!(parse_ver("php84").as_deref(), Some("8.4"));
        assert_eq!(parse_ver("8.4.27").as_deref(), Some("8.4"));
        assert_eq!(parse_ver("abc"), None);
        assert_eq!(norm("Lunaray_New"), "lunaray-new");
    }

    #[test]
    fn names_cannot_escape() {
        for bad in ["", ".", "..", "...", ".a", "a.", "a..b", "-a", "a/b", "a\\b", "a b"] {
            assert!(!valid_name(bad), "{bad:?} harus ditolak");
        }
        for ok in ["raystore", "lunaray-new", "foo.bar", "app2"] {
            assert!(valid_name(ok), "{ok:?} harus diterima");
        }
        let mut cfg = Config::default();
        cfg.paths = vec![std::env::temp_dir().to_string_lossy().to_string()];
        assert!(cfg.find_site("..").is_none());
        assert!(cfg.find_site(".").is_none());
    }
}
