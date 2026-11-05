//! Hosts file: herdlite menulis blok sendiri di antara penanda, tanpa menyentuh baris lain.
//! Menulis hosts butuh hak Administrator; herdlite TIDAK pernah meminta elevasi sendiri.

use crate::config::Config;
use std::collections::BTreeSet;
use std::fs;

pub const HOSTS: &str = r"C:\Windows\System32\drivers\etc\hosts";
const START: &str = "# herdlite-start (dikelola oleh herdlite: hl hosts sync)";
const END: &str = "# herdlite-end";

/// Semua nama host yang dibutuhkan (situs + alias underscore + dashboard).
pub fn wanted(cfg: &Config) -> Vec<String> {
    let mut set = BTreeSet::new();
    set.insert(format!("herdlite.{}", cfg.tld));
    for s in cfg.sites() {
        set.insert(cfg.host(&s.name));
        let raw = s.folder.to_lowercase();
        if !raw.is_empty() && raw != s.name && raw.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            set.insert(format!("{raw}.{}", cfg.tld));
        }
    }
    set.into_iter().collect()
}

/// Nama yang sudah dipetakan ke `ip`. Baris di dalam blok milik Herd
/// (`# Herd generated Hosts` … `# End Herd generated Hosts`) diabaikan,
/// karena blok itu ikut dihapus saat Herd di-uninstall.
fn mapped(text: &str, ip: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut in_herd = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with("# Herd generated Hosts") {
            in_herd = true;
            continue;
        }
        if t.starts_with("# End Herd generated Hosts") {
            in_herd = false;
            continue;
        }
        if in_herd {
            continue;
        }
        let line = line.split('#').next().unwrap_or("");
        let mut it = line.split_whitespace();
        if it.next() == Some(ip) {
            for h in it {
                out.insert(h.to_lowercase());
            }
        }
    }
    out
}

/// Nama host yang belum ada di hosts file.
pub fn missing(cfg: &Config) -> Result<Vec<String>, String> {
    let text = fs::read_to_string(HOSTS).map_err(|e| format!("gagal membaca hosts: {e}"))?;
    let have = mapped(&text, &loopback(cfg));
    Ok(wanted(cfg).into_iter().filter(|h| !have.contains(h)).collect())
}

fn loopback(cfg: &Config) -> String {
    if cfg.bind == "0.0.0.0" { "127.0.0.1".into() } else { cfg.bind.clone() }
}

/// Isi baru hosts file dengan blok herdlite diperbarui.
pub fn render(current: &str, cfg: &Config) -> String {
    let mut kept: Vec<&str> = Vec::new();
    let mut inside = false;
    for line in current.lines() {
        if line.trim_start().starts_with("# herdlite-start") {
            inside = true;
            continue;
        }
        if line.trim_start().starts_with(END) {
            inside = false;
            continue;
        }
        if !inside {
            kept.push(line);
        }
    }
    while kept.last().map(|l| l.trim().is_empty()).unwrap_or(false) {
        kept.pop();
    }
    let ip = loopback(cfg);
    let mut out = kept.join("\r\n");
    out.push_str("\r\n\r\n");
    out.push_str(START);
    out.push_str("\r\n");
    for h in wanted(cfg) {
        out.push_str(&format!("{ip} {h}\r\n"));
    }
    out.push_str(END);
    out.push_str("\r\n");
    out
}

/// Tulis blok herdlite ke hosts. Gagal dengan pesan jelas kalau tidak dijalankan sebagai Administrator.
pub fn sync(cfg: &Config) -> Result<usize, String> {
    let text = fs::read_to_string(HOSTS).map_err(|e| format!("gagal membaca hosts: {e}"))?;
    let new = render(&text, cfg);
    if new == text {
        return Ok(wanted(cfg).len());
    }
    let backup = std::env::temp_dir().join("hosts.herdlite-backup");
    let _ = fs::write(&backup, &text);
    fs::write(HOSTS, new).map_err(|e| {
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            "butuh hak Administrator. Buka terminal \"Run as administrator\" lalu jalankan lagi: hl hosts sync".to_string()
        } else {
            format!("gagal menulis hosts: {e}")
        }
    })?;
    Ok(wanted(cfg).len())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn render_replaces_block() {
        let cfg = Config::default();
        let cur = "127.0.0.1 a.test\r\n# herdlite-start x\r\n127.0.0.1 old.test\r\n# herdlite-end\r\n";
        let out = render(cur, &cfg);
        assert!(out.contains("127.0.0.1 a.test"));
        assert!(!out.contains("old.test"));
        assert!(out.contains("127.0.0.1 herdlite.test"));
        assert_eq!(out.matches("herdlite-start").count(), 1);
    }

    #[test]
    fn herd_block_not_counted() {
        let t = "127.0.0.1 a.test\n# Herd generated Hosts. Do not change.\n127.0.0.1 b.test\n# End Herd generated Hosts\n";
        let m = mapped(t, "127.0.0.1");
        assert!(m.contains("a.test"));
        assert!(!m.contains("b.test"));
    }
}
