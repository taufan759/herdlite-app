//! herdlite: pengganti Laravel Herd yang sangat ringan untuk Windows.
//!
//! - `hl.exe`        : CLI (park, link, isolate, use, start/stop, dst.)
//! - `herdlited.exe` : server latar belakang (HTTP + HTTPS + FastCGI ke php-cgi)

pub mod config;
pub mod fastcgi;
pub mod herdimport;
pub mod hosts;
pub mod node;
pub mod phpinstall;
pub mod pool;
pub mod server;
pub mod tls;
pub mod util;
pub mod win;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
