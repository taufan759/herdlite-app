//! Hosts file: herdlite menulis blok sendiri di antara penanda, tanpa menyentuh baris lain.
//! Menulis hosts butuh hak Administrator; herdlite TIDAK pernah meminta elevasi sendiri.

use crate::config::Config;
use std::collections::BTreeSet;
use std::fs;

pub const HOSTS: &str = r"C:\Windows\System32\drivers\etc\hosts";
const START: &str = "# herdlite-start (dikelola oleh herdlite: hl hosts sync)";
const END: &str = "# herdlite-end";

/// TLD yang boleh ditulis ke hosts (sama seperti batasan HerdHelper milik Herd).
/// Mencegah config.json yang diubah orang lain membelokkan domain sungguhan.
pub const ALLOWED_TLDS: &[&str] = &["test", "localhost", "local", "wip"];

fn safe_label(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 63
        && !n.starts_with('-')
        && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_')
}

/// Periksa bahwa konfigurasi aman untuk ditulis ke hosts.
pub fn check_safe(cfg: &Config) -> Result<(), String> {
    if !ALLOWED_TLDS.contains(&cfg.tld.as_str()) {
        return Err(format!("tld '{}' tidak diizinkan untuk hosts (pilihan: {})", cfg.tld, ALLOWED_TLDS.join(", ")));
    }
    let ip: std::net::Ipv4Addr = loopback(cfg).parse().map_err(|_| format!("bind '{}' bukan alamat IPv4", cfg.bind))?;
    if !ip.is_loopback() {
        return Err(format!("bind '{}' bukan alamat loopback 127.x.x.x", cfg.bind));
    }
    Ok(())
}

/// Semua nama host yang dibutuhkan (situs + alias underscore + dashboard).
pub fn wanted(cfg: &Config) -> Vec<String> {
    let mut set = BTreeSet::new();
    set.insert(format!("herdlite.{}", cfg.tld));
    for s in cfg.sites() {
        if !safe_label(&s.name) {
            continue;
        }
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

/// Apakah hosts file masih berisi blok herdlite?
pub fn has_block() -> Result<bool, String> {
    let text = fs::read_to_string(HOSTS).map_err(|e| format!("gagal membaca hosts: {e}"))?;
    Ok(text.lines().any(|l| l.trim_start().starts_with("# herdlite-start")))
}

/// Isi hosts tanpa blok herdlite (baris lain tidak disentuh).
pub fn strip(current: &str) -> String {
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
    let mut out = kept.join("\r\n");
    out.push_str("\r\n");
    out
}

/// Hapus blok herdlite dari hosts (dipakai saat uninstall).
pub fn clear() -> Result<(), String> {
    let text = fs::read_to_string(HOSTS).map_err(|e| format!("gagal membaca hosts: {e}"))?;
    if !text.lines().any(|l| l.trim_start().starts_with("# herdlite-start")) {
        return Ok(());
    }
    write_hosts(&text, &strip(&text))
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
    check_safe(cfg)?;
    let text = fs::read_to_string(HOSTS).map_err(|e| format!("gagal membaca hosts: {e}"))?;
    let new = render(&text, cfg);
    if new != text {
        write_hosts(&text, &new)?;
    }
    Ok(wanted(cfg).len())
}

/// Backup lalu tulis hosts. Gagal dengan pesan "butuh hak Administrator" kalau ditolak.
fn write_hosts(old: &str, new: &str) -> Result<(), String> {
    // Backup dengan nama acak + create_new: proses ini berjalan sebagai admin, jadi jangan
    // menulis ke nama tetap yang bisa disiapkan lebih dulu sebagai symlink oleh proses lain.
    let mut rnd = [0u8; 8];
    let _ = ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut rnd);
    let tag: String = rnd.iter().map(|b| format!("{b:02x}")).collect();
    let backup = std::env::temp_dir().join(format!("hosts.herdlite-{tag}.bak"));
    if let Ok(mut f) = fs::OpenOptions::new().write(true).create_new(true).open(&backup) {
        use std::io::Write;
        let _ = f.write_all(old.as_bytes());
    }
    fs::write(HOSTS, new).map_err(|e| {
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            "butuh hak Administrator untuk mengubah hosts".to_string()
        } else {
            format!("gagal menulis hosts: {e}")
        }
    })
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
    fn strip_removes_only_our_block() {
        let cur = "127.0.0.1 a.test\r\n\r\n# herdlite-start x\r\n127.0.0.1 b.test\r\n# herdlite-end\r\n";
        let out = strip(cur);
        assert!(out.contains("a.test"));
        assert!(!out.contains("b.test"));
        assert!(!out.contains("herdlite"));
    }

    #[test]
    fn unsafe_config_rejected() {
        let mut cfg = Config::default();
        assert!(check_safe(&cfg).is_ok());
        cfg.tld = "com".into();
        assert!(check_safe(&cfg).is_err());
        cfg.tld = "test".into();
        cfg.bind = "10.0.0.5".into();
        assert!(check_safe(&cfg).is_err());
        cfg.bind = "0.0.0.0".into(); // ditulis sebagai 127.0.0.1
        assert!(check_safe(&cfg).is_ok());
        assert!(!safe_label("foo bar"));
        assert!(!safe_label("a\nb"));
    }

    #[test]
    fn herd_block_not_counted() {
        let t = "127.0.0.1 a.test\n# Herd generated Hosts. Do not change.\n127.0.0.1 b.test\n# End Herd generated Hosts\n";
        let m = mapped(t, "127.0.0.1");
        assert!(m.contains("a.test"));
        assert!(!m.contains("b.test"));
    }
}
