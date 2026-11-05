//! Pengelola proses php-cgi: dinyalakan saat ada request pertama untuk versi
//! tersebut, dimatikan lagi kalau menganggur. Versi yang tidak dipakai = 0 MB RAM.

use crate::config::{ver_key, Config};
use crate::util::append_log;
use crate::win;
use std::collections::HashMap;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

struct Proc {
    child: Child,
    port: u16,
    last: Instant,
}

pub struct Pools {
    home: PathBuf,
    inner: Mutex<HashMap<String, Proc>>,
}

pub struct PoolInfo {
    pub version: String,
    pub port: u16,
    pub pid: u32,
    pub idle_secs: u64,
}

/// Port FastCGI tetap per versi: 8.3 → 9283 (tidak bentrok dengan Herd yang pakai 90xx).
pub fn port_for(ver: &str) -> u16 {
    let (a, b) = ver_key(ver);
    9200 + (a * 10 + b) as u16
}

impl Pools {
    pub fn new(home: PathBuf) -> Self {
        Pools { home, inner: Mutex::new(HashMap::new()) }
    }

    /// Pastikan php-cgi versi `ver` jalan; kembalikan port-nya.
    pub fn ensure(&self, ver: &str, cfg: &Config) -> Result<u16, String> {
        let mut m = self.inner.lock().map_err(|_| "lock".to_string())?;
        if let Some(p) = m.get_mut(ver) {
            if let Ok(None) = p.child.try_wait() {
                p.last = Instant::now();
                return Ok(p.port);
            }
            m.remove(ver);
        }
        let dir = cfg.php_dir(&self.home, ver);
        let exe = dir.join("php-cgi.exe");
        if !exe.is_file() {
            return Err(format!(
                "PHP {ver} belum terpasang (tidak ada {}). Jalankan: hl php:install {ver}",
                exe.display()
            ));
        }
        let port = port_for(ver);
        let mut cmd = Command::new(&exe);
        cmd.arg("-b").arg(format!("127.0.0.1:{port}"));
        let ini = dir.join("php.ini");
        if ini.is_file() {
            cmd.arg("-c").arg(&ini);
        }
        cmd.env("PHP_FCGI_CHILDREN", cfg.php_children.max(1).to_string())
            .env("PHP_FCGI_MAX_REQUESTS", "1000")
            .current_dir(&dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(win::CREATE_NO_WINDOW);
        let child = cmd.spawn().map_err(|e| format!("gagal menjalankan {}: {e}", exe.display()))?;
        append_log(
            &self.home.join("logs").join("herdlite.log"),
            &format!("php-cgi {ver} dinyalakan (pid {}, port {port}, {} worker)", child.id(), cfg.php_children.max(1)),
        );
        m.insert(ver.to_string(), Proc { child, port, last: Instant::now() });
        Ok(port)
    }

    pub fn touch(&self, ver: &str) {
        if let Ok(mut m) = self.inner.lock() {
            if let Some(p) = m.get_mut(ver) {
                p.last = Instant::now();
            }
        }
    }

    /// Matikan versi yang menganggur lebih lama dari `idle`.
    pub fn reap(&self, idle: Duration) {
        let Ok(mut m) = self.inner.lock() else { return };
        let old: Vec<String> = m.iter().filter(|(_, p)| p.last.elapsed() > idle).map(|(k, _)| k.clone()).collect();
        for v in old {
            if let Some(p) = m.remove(&v) {
                win::kill_tree(p.child.id());
                append_log(
                    &self.home.join("logs").join("herdlite.log"),
                    &format!("php-cgi {v} dimatikan karena menganggur"),
                );
            }
        }
    }

    pub fn info(&self) -> Vec<PoolInfo> {
        let Ok(mut m) = self.inner.lock() else { return vec![] };
        let mut out: Vec<PoolInfo> = m
            .iter_mut()
            .filter_map(|(v, p)| {
                matches!(p.child.try_wait(), Ok(None)).then(|| PoolInfo {
                    version: v.clone(),
                    port: p.port,
                    pid: p.child.id(),
                    idle_secs: p.last.elapsed().as_secs(),
                })
            })
            .collect();
        out.sort_by_key(|p| ver_key(&p.version));
        out
    }
}
