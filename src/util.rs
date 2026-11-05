use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Tambahkan satu baris ke file log (dibuat bila belum ada).
pub fn append_log(path: &Path, msg: &str) {
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "[{}] {}", now_str(), msg.trim_end());
    }
}

fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn unix(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(_) => 0,
    }
}

/// Waktu sekarang (UTC) untuk log.
pub fn now_str() -> String {
    let s = unix(SystemTime::now());
    let (y, m, d) = civil(s.div_euclid(86400));
    let r = s.rem_euclid(86400);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}Z", r / 3600, r % 3600 / 60, r % 60)
}

/// Format tanggal HTTP (IMF-fixdate).
pub fn http_date(t: SystemTime) -> String {
    const WD: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MO: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let s = unix(t);
    let days = s.div_euclid(86400);
    let (y, m, d) = civil(days);
    let r = s.rem_euclid(86400);
    format!(
        "{}, {:02} {} {:04} {:02}:{:02}:{:02} GMT",
        WD[(days + 4).rem_euclid(7) as usize],
        d,
        MO[(m - 1) as usize],
        y,
        r / 3600,
        r % 3600 / 60,
        r % 60
    )
}

pub fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let h = |c: u8| (c as char).to_digit(16);
            if let (Some(a), Some(c)) = (h(b[i + 1]), h(b[i + 2])) {
                out.push((a * 16 + c) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

pub fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

pub fn mime(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "json" | "map" => "application/json",
        "webmanifest" => "application/manifest+json",
        "xml" => "text/xml",
        "txt" | "log" | "md" => "text/plain; charset=utf-8",
        "csv" => "text/csv",
        "svg" | "svgz" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "bmp" => "image/bmp",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "eot" => "application/vnd.ms-fontobject",
        "wasm" => "application/wasm",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "mp3" => "audio/mpeg",
        "ogg" => "audio/ogg",
        "wav" => "audio/wav",
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        _ => "application/octet-stream",
    }
}

/// Bersihkan path Windows untuk perbandingan: tanpa awalan \\?\, huruf kecil, backslash, tanpa \ di akhir.
pub fn path_key(p: &Path) -> String {
    let s = p.to_string_lossy().replace('/', "\\");
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s).to_string();
    s.trim_end_matches('\\').to_lowercase()
}

/// Path absolut yang rapi (tanpa \\?\) untuk disimpan di konfigurasi.
pub fn clean_abs(p: &Path) -> String {
    let c = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let s = c.to_string_lossy().to_string();
    s.strip_prefix(r"\\?\").unwrap_or(&s).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dates() {
        assert_eq!(http_date(UNIX_EPOCH), "Thu, 01 Jan 1970 00:00:00 GMT");
        let t = UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        assert_eq!(http_date(t), "Tue, 14 Nov 2023 22:13:20 GMT");
    }
    #[test]
    fn decode() {
        assert_eq!(percent_decode("/a%20b%2"), b"/a b%2".to_vec());
        assert_eq!(percent_decode("%41"), b"A".to_vec());
    }
}
