//! herdlite: pengganti Laravel Herd yang sangat ringan untuk Windows.
//!
//! - `hl.exe`        : CLI (park, link, isolate, use, start/stop, dst.)
//! - `herdlited.exe` : server latar belakang (HTTP + HTTPS + FastCGI ke php-cgi)

pub mod util;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
