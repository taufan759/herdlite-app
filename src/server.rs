//! Server HTTP/HTTPS herdlite: menerjemahkan <situs>.test → folder, melayani
//! file statis langsung, dan meneruskan .php ke php-cgi lewat FastCGI.

use crate::config::{self, Config, Kind, Site};
use crate::fastcgi::{self, Body, ChannelBody};
use crate::pool::Pools;
use crate::util::{self, append_log, html_escape};
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Incoming;
use hyper::header::{self, HeaderValue};
use hyper::{Method, Request, Response, StatusCode, Version};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use std::convert::Infallible;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::net::TcpListener;

pub struct State {
    pub home: PathBuf,
    cfg: Mutex<(Config, Option<SystemTime>)>,
    pub pools: Pools,
    log: PathBuf,
    phplog: PathBuf,
    started: SystemTime,
}

impl State {
    /// Konfigurasi terbaru: dibaca ulang otomatis kalau config.json berubah,
    /// jadi `hl park`, `hl link`, `hl isolate` langsung berlaku tanpa restart.
    pub fn cfg(&self) -> Config {
        let mtime = std::fs::metadata(config::config_path(&self.home)).and_then(|m| m.modified()).ok();
        let mut g = self.cfg.lock().unwrap();
        if mtime != g.1 {
            if let Ok(c) = Config::load(&self.home) {
                g.0 = c;
            }
            g.1 = mtime;
        }
        g.0.clone()
    }
}

pub fn pid_path(home: &Path) -> PathBuf {
    home.join("run").join("herdlited.pid")
}

/// URL dashboard: herdlite.<tld> kalau sudah ada di hosts, selain itu 127.0.0.1.
pub fn dashboard_url(cfg: &Config) -> String {
    let name = format!("herdlite.{}", cfg.tld);
    let host = match crate::hosts::missing(cfg) {
        Ok(m) if !m.contains(&name) => name,
        _ => "127.0.0.1".into(),
    };
    format!("http://{host}{}", port_suffix(false, cfg))
}

/// Buka dashboard sebagai jendela aplikasi (Edge mode --app: tanpa tab/address bar).
/// Edge selalu ada di Windows 10/11; kalau tidak ditemukan, pakai browser bawaan.
pub fn open_app(url: &str) {
    use std::os::windows::process::CommandExt;
    let candidates = [
        std::env::var("ProgramFiles(x86)").ok(),
        std::env::var("ProgramFiles").ok(),
        std::env::var("LOCALAPPDATA").ok(),
    ];
    for base in candidates.into_iter().flatten() {
        let edge = PathBuf::from(base).join(r"Microsoft\Edge\Application\msedge.exe");
        if edge.is_file() {
            let ok = std::process::Command::new(&edge)
                .arg(format!("--app={url}"))
                .arg("--window-size=1100,760")
                .creation_flags(crate::win::CREATE_NO_WINDOW)
                .spawn()
                .is_ok();
            if ok {
                return;
            }
        }
    }
    open_url(url);
}

/// Buka URL di browser bawaan.
pub fn open_url(url: &str) {
    use std::os::windows::process::CommandExt;
    let _ = std::process::Command::new(crate::win::sys("rundll32.exe"))
        .args(["url.dll,FileProtocolHandler", url])
        .creation_flags(crate::win::CREATE_NO_WINDOW)
        .spawn();
}

fn full<T: Into<Bytes>>(b: T) -> Body {
    Full::new(b.into()).map_err(|n: Infallible| match n {}).boxed()
}

fn empty() -> Body {
    full(Bytes::new())
}

fn simple(code: u16, title: &str, msg: &str) -> Response<Body> {
    let html = format!(
        "<!doctype html><meta charset=utf-8><title>{t}</title>{css}<main><h1>{code} · {t}</h1><p>{m}</p><p class=dim>herdlite</p></main>",
        t = html_escape(title),
        m = msg,
        css = CSS
    );
    Response::builder()
        .status(code)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .body(full(html))
        .unwrap()
}

fn redirect(code: u16, to: &str) -> Response<Body> {
    Response::builder()
        .status(code)
        .header(header::LOCATION, to)
        .body(empty())
        .unwrap()
}

pub fn run(home: PathBuf) -> Result<(), String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(serve(home))
}

async fn serve(home: PathBuf) -> Result<(), String> {
    let cfg = Config::load(&home)?;
    let log = home.join("logs").join("herdlite.log");
    let state = Arc::new(State {
        home: home.clone(),
        cfg: Mutex::new((cfg.clone(), std::fs::metadata(config::config_path(&home)).and_then(|m| m.modified()).ok())),
        pools: Pools::new(home.clone()),
        log: log.clone(),
        phplog: home.join("logs").join("php.log"),
        started: SystemTime::now(),
    });

    let http = TcpListener::bind((cfg.bind.as_str(), cfg.http_port))
        .await
        .map_err(|e| format!("gagal membuka port HTTP {}:{}: {e}", cfg.bind, cfg.http_port))?;
    let https = if cfg.https_port != 0 {
        let (ca, _) = crate::tls::Ca::load_or_create(&home, &cfg.tld)?;
        let tls = crate::tls::server_config(ca, &cfg.tld)?;
        let l = TcpListener::bind((cfg.bind.as_str(), cfg.https_port))
            .await
            .map_err(|e| format!("gagal membuka port HTTPS {}:{}: {e}", cfg.bind, cfg.https_port))?;
        Some((l, tokio_rustls::TlsAcceptor::from(Arc::new(tls))))
    } else {
        None
    };

    let pidp = pid_path(&home);
    let _ = std::fs::create_dir_all(pidp.parent().unwrap());
    let _ = std::fs::write(&pidp, std::process::id().to_string());
    append_log(
        &log,
        &format!(
            "herdlite {} berjalan (pid {}) di http://{}:{}{}",
            crate::VERSION,
            std::process::id(),
            cfg.bind,
            cfg.http_port,
            if cfg.https_port != 0 { format!(" dan https port {}", cfg.https_port) } else { String::new() }
        ),
    );

    // Matikan php-cgi yang menganggur.
    {
        let st = state.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(30)).await;
                let idle = st.cfg().php_idle_minutes;
                if idle > 0 {
                    st.pools.reap(Duration::from_secs(idle * 60));
                }
            }
        });
    }

    if let Some((listener, acceptor)) = https {
        let st = state.clone();
        tokio::spawn(async move {
            loop {
                let Ok((stream, remote)) = listener.accept().await else { continue };
                let st = st.clone();
                let acc = acceptor.clone();
                tokio::spawn(async move {
                    let local = stream.local_addr().unwrap_or(remote);
                    let Ok(Ok(tls)) = tokio::time::timeout(Duration::from_secs(15), acc.accept(stream)).await else { return };
                    serve_conn(st, TokioIo::new(tls), true, remote, local).await;
                });
            }
        });
    }

    loop {
        let Ok((stream, remote)) = http.accept().await else { continue };
        let st = state.clone();
        tokio::spawn(async move {
            let local = stream.local_addr().unwrap_or(remote);
            let _ = stream.set_nodelay(true);
            serve_conn(st, TokioIo::new(stream), false, remote, local).await;
        });
    }
}

async fn serve_conn<I>(st: Arc<State>, io: TokioIo<I>, https: bool, remote: SocketAddr, local: SocketAddr)
where
    I: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let svc = hyper::service::service_fn(move |req| {
        let st = st.clone();
        async move { Ok::<_, Infallible>(handle(st, req, https, remote, local).await) }
    });
    let _ = auto::Builder::new(TokioExecutor::new()).serve_connection_with_upgrades(io, svc).await;
}

fn request_host(req: &Request<Incoming>) -> String {
    req.uri()
        .host()
        .map(|h| h.to_string())
        .or_else(|| req.headers().get(header::HOST).and_then(|v| v.to_str().ok()).map(|s| s.to_string()))
        .unwrap_or_default()
}

fn strip_port(h: &str) -> String {
    let h = h.trim();
    let h = if h.starts_with('[') {
        h.split(']').next().unwrap_or(h).trim_start_matches('[')
    } else {
        h.rsplit_once(':').map(|(a, _)| a).unwrap_or(h)
    };
    h.trim_end_matches('.').to_ascii_lowercase()
}

async fn handle(st: Arc<State>, req: Request<Incoming>, https: bool, remote: SocketAddr, local: SocketAddr) -> Response<Body> {
    let cfg = st.cfg();
    let host_full = request_host(&req);
    let host = strip_port(&host_full);
    let suffix = format!(".{}", cfg.tld);

    if host.is_empty() || host == "localhost" || host.parse::<IpAddr>().is_ok() || host == format!("herdlite{suffix}") {
        if req.uri().path() == "/__herdlite/status" {
            return status_json(&st, &cfg);
        }
        return dashboard(&st, &cfg);
    }
    let Some(prefix) = host.strip_suffix(&suffix) else {
        return simple(404, "Domain tidak dikenal", &format!("herdlite hanya melayani domain <b>*{}</b>.", html_escape(&suffix)));
    };
    let site = cfg
        .find_site(prefix)
        .or_else(|| prefix.rsplit_once('.').and_then(|(_, last)| cfg.find_site(last)));
    let Some(site) = site else {
        return simple(
            404,
            "Situs tidak ditemukan",
            &format!(
                "Tidak ada folder untuk <b>{}</b> di folder park maupun link.<br>Buka <a href=\"http://herdlite{}{}\">dashboard</a> untuk melihat daftar situs.",
                html_escape(&host),
                html_escape(&suffix),
                port_suffix(false, &cfg)
            ),
        );
    };

    if !https && cfg.https_port != 0 && cfg.is_secured(&site.name) {
        let pq = req.uri().path_and_query().map(|p| p.as_str()).unwrap_or("/");
        return redirect(301, &format!("https://{host}{}{pq}", port_suffix(true, &cfg)));
    }

    serve_site(&st, &cfg, &site, req, https, remote, local, &host).await
}

fn port_suffix(https: bool, cfg: &Config) -> String {
    let (p, d) = if https { (cfg.https_port, 443) } else { (cfg.http_port, 80) };
    if p == d { String::new() } else { format!(":{p}") }
}

/// Nama file asli di disk (setelah alias seperti nama pendek 8.3 diselesaikan).
fn real_name(p: &Path) -> Option<String> {
    let c = std::fs::canonicalize(p).ok()?;
    c.file_name().map(|n| n.to_string_lossy().to_string())
}

/// Apakah file ini (menurut path aslinya di disk) berada di dalam folder/berkas tersembunyi
/// di bawah docroot? Menangkap alias seperti nama pendek 8.3 (GIT~1 → .git, ENV~1 → .env).
fn hidden_on_disk(docroot: &Path, target: &Path) -> bool {
    let Ok(real) = std::fs::canonicalize(target) else { return false };
    let hidden = |s: &str| s.starts_with('.') && s != ".well-known";
    match std::fs::canonicalize(docroot).ok().and_then(|r| real.strip_prefix(&r).ok().map(|p| p.to_path_buf())) {
        Some(rel) => rel.components().any(|c| hidden(&c.as_os_str().to_string_lossy())),
        // Di luar docroot (lewat symlink, mis. public/storage): cukup cek nama filenya.
        None => real.file_name().map(|n| hidden(&n.to_string_lossy())).unwrap_or(false),
    }
}

fn join(base: &Path, segs: &[&str]) -> PathBuf {
    let mut p = base.to_path_buf();
    for s in segs {
        p.push(s);
    }
    p
}

#[allow(clippy::too_many_arguments)]
async fn serve_site(
    st: &Arc<State>,
    cfg: &Config,
    site: &Site,
    req: Request<Incoming>,
    https: bool,
    remote: SocketAddr,
    local: SocketAddr,
    host: &str,
) -> Response<Body> {
    let docroot = config::docroot(&site.path);
    let raw_path = req.uri().path().to_string();
    let query = req.uri().query().unwrap_or("").to_string();
    let decoded = String::from_utf8_lossy(&util::percent_decode(&raw_path)).into_owned();

    let mut segs: Vec<&str> = Vec::new();
    for s in decoded.split('/') {
        if s.is_empty() || s == "." {
            continue;
        }
        // Windows membuang titik/spasi di akhir nama ("index.php." == "index.php"),
        // jadi segmen seperti itu ditolak supaya file .php tidak terkirim sebagai teks.
        if s == ".."
            || s.contains('\\')
            || s.contains(':')
            || s.chars().any(|c| c.is_control())
            || s.ends_with('.')
            || s.ends_with(' ')
        {
            return simple(400, "Bad Request", "Path tidak valid.");
        }
        segs.push(s);
    }
    if segs.iter().any(|s| s.starts_with('.') && *s != ".well-known") {
        return simple(403, "Forbidden", "File tersembunyi tidak dilayani.");
    }

    // (file skrip, SCRIPT_NAME, PATH_INFO)
    let mut script: Option<(PathBuf, String, String)> = None;
    for i in 0..segs.len() {
        if segs[i].to_ascii_lowercase().ends_with(".php") {
            let f = join(&docroot, &segs[..=i]);
            if f.is_file() {
                let mut pi = String::new();
                if i + 1 < segs.len() {
                    pi = format!("/{}", segs[i + 1..].join("/"));
                    if decoded.ends_with('/') {
                        pi.push('/');
                    }
                }
                script = Some((f, format!("/{}", segs[..=i].join("/")), pi));
            }
            break;
        }
    }

    if script.is_none() {
        let target = join(&docroot, &segs);
        if !segs.is_empty() && target.is_file() {
            // Putuskan dari nama file yang SEBENARNYA di disk (menangkap nama pendek 8.3
            // dan alias lain): file .php tidak pernah dikirim sebagai teks, file tersembunyi
            // seperti .env tidak pernah dilayani.
            if hidden_on_disk(&docroot, &target) {
                return simple(403, "Forbidden", "File tersembunyi tidak dilayani.");
            }
            if real_name(&target).map(|n| n.to_ascii_lowercase().ends_with(".php")).unwrap_or(false) {
                return simple(403, "Forbidden", "File PHP hanya bisa dijalankan dengan nama aslinya.");
            }
            return static_file(&req, &target).await;
        }
        if target.is_dir() {
            let idx_php = target.join("index.php");
            let idx_html = target.join("index.html");
            if idx_php.is_file() || idx_html.is_file() {
                if !segs.is_empty() && !raw_path.ends_with('/') {
                    let q = if query.is_empty() { String::new() } else { format!("?{query}") };
                    // Rapikan "//" di depan supaya redirect tidak jadi URL protocol-relative
                    // (//situs-lain.com/…) alias open redirect.
                    let clean = format!("/{}", raw_path.trim_start_matches('/'));
                    return redirect(301, &format!("{clean}/{q}"));
                }
                if idx_php.is_file() {
                    let sn = if segs.is_empty() { "/index.php".to_string() } else { format!("/{}/index.php", segs.join("/")) };
                    script = Some((idx_php, sn, String::new()));
                } else {
                    return static_file(&req, &idx_html).await;
                }
            }
        }
    }
    if script.is_none() {
        let fc = docroot.join("index.php");
        if fc.is_file() {
            script = Some((fc, "/index.php".into(), String::new()));
        }
    }
    let Some((file, script_name, path_info)) = script else {
        return simple(404, "Not Found", &format!("<code>{}</code> tidak ada di situs ini.", html_escape(&decoded)));
    };
    if hidden_on_disk(&docroot, &file) {
        return simple(403, "Forbidden", "File tersembunyi tidak dilayani.");
    }

    let Some(ver) = cfg.php_for(&st.home, &site.name) else {
        return simple(500, "PHP belum terpasang", "Pasang dulu PHP, misalnya: <code>hl php:install 8.3</code>");
    };
    let port = match st.pools.ensure(&ver, cfg) {
        Ok(p) => p,
        Err(e) => return simple(502, "PHP tidak bisa dijalankan", &html_escape(&e)),
    };

    let (parts, body) = req.into_parts();
    let max = (cfg.max_body_mb.max(1) as usize) * 1024 * 1024;
    let declared_len = parts
        .headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok());
    if declared_len.map(|l| l as usize > max).unwrap_or(false) {
        return simple(413, "Payload Too Large", &format!("Batas upload {} MB (ubah: hl config max_body_mb ...)", cfg.max_body_mb));
    }
    let (fbody, content_len): (Body, Option<u64>) = match declared_len {
        Some(l) => (body.map_err(io::Error::other).boxed(), Some(l)),
        None => match Limited::new(body, max).collect().await {
            Ok(c) => {
                let b = c.to_bytes();
                let l = b.len() as u64;
                (full(b), if l > 0 { Some(l) } else { None })
            }
            Err(_) => return simple(413, "Payload Too Large", "Body request terlalu besar."),
        },
    };

    let docroot_s = docroot.to_string_lossy().to_string();
    let request_uri = parts.uri.path_and_query().map(|p| p.as_str().to_string()).unwrap_or_else(|| "/".into());
    let proto = match parts.version {
        Version::HTTP_2 => "HTTP/2.0",
        Version::HTTP_10 => "HTTP/1.0",
        _ => "HTTP/1.1",
    };
    let mut p: Vec<(String, String)> = vec![
        ("GATEWAY_INTERFACE".into(), "CGI/1.1".into()),
        ("SERVER_SOFTWARE".into(), format!("herdlite/{}", crate::VERSION)),
        ("SERVER_PROTOCOL".into(), proto.into()),
        ("REQUEST_METHOD".into(), parts.method.as_str().into()),
        ("REQUEST_URI".into(), request_uri),
        ("QUERY_STRING".into(), query.clone()),
        ("DOCUMENT_ROOT".into(), docroot_s.clone()),
        ("DOCUMENT_URI".into(), format!("{script_name}{path_info}")),
        ("SCRIPT_FILENAME".into(), file.to_string_lossy().to_string()),
        ("SCRIPT_NAME".into(), script_name.clone()),
        ("SERVER_NAME".into(), host.to_string()),
        ("SERVER_ADDR".into(), local.ip().to_string()),
        ("SERVER_PORT".into(), local.port().to_string()),
        ("REMOTE_ADDR".into(), remote.ip().to_string()),
        ("REMOTE_PORT".into(), remote.port().to_string()),
        ("REDIRECT_STATUS".into(), "200".into()),
        ("HERDLITE".into(), "1".into()),
    ];
    if !path_info.is_empty() {
        p.push(("PATH_INFO".into(), path_info.clone()));
        p.push(("PATH_TRANSLATED".into(), format!("{docroot_s}{}", path_info.replace('/', "\\"))));
    }
    if https {
        p.push(("HTTPS".into(), "on".into()));
        p.push(("REQUEST_SCHEME".into(), "https".into()));
    } else {
        p.push(("REQUEST_SCHEME".into(), "http".into()));
    }
    if let Some(ct) = parts.headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()) {
        p.push(("CONTENT_TYPE".into(), ct.into()));
    }
    if let Some(l) = content_len {
        p.push(("CONTENT_LENGTH".into(), l.to_string()));
    }
    // HTTP_HOST selalu dari sumber yang sama dengan pemilihan situs (authority URI,
    // kalau tidak ada baru header Host), supaya PHP tidak melihat host yang berbeda.
    let http_host = parts
        .uri
        .authority()
        .map(|a| a.as_str().to_string())
        .or_else(|| parts.headers.get(header::HOST).and_then(|v| v.to_str().ok()).map(|s| s.to_string()))
        .unwrap_or_else(|| host.to_string());
    let mut merged: Vec<(String, String)> = vec![("HTTP_HOST".into(), http_host)];
    for (name, value) in parts.headers.iter() {
        let n = name.as_str();
        // Header bergaris bawah (X_Foo) diabaikan seperti nginx, supaya tidak bisa
        // menyamar sebagai X-Foo (keduanya jadi HTTP_X_FOO). httpoxy: Proxy dibuang.
        if n == "content-type" || n == "content-length" || n == "proxy" || n == "host" || n.contains('_') {
            continue;
        }
        let key = format!("HTTP_{}", n.to_ascii_uppercase().replace('-', "_"));
        let v = String::from_utf8_lossy(value.as_bytes()).into_owned();
        if let Some(e) = merged.iter_mut().find(|(k, _)| *k == key) {
            e.1.push_str(if n == "cookie" { "; " } else { ", " });
            e.1.push_str(&v);
        } else {
            merged.push((key, v));
        }
    }
    p.extend(merged);

    let res = fastcgi::run(port, p, fbody, st.phplog.clone()).await;
    st.pools.touch(&ver);
    match res {
        Ok(r) => r,
        Err(e) => {
            append_log(&st.log, &format!("{} {}: {e}", site.name, raw_path));
            simple(502, "Bad Gateway", &html_escape(&e))
        }
    }
}

async fn static_file(req: &Request<Incoming>, path: &Path) -> Response<Body> {
    let Ok(meta) = tokio::fs::metadata(path).await else {
        return simple(404, "Not Found", "File tidak ditemukan.");
    };
    let len = meta.len();
    let lm = meta.modified().map(util::http_date).unwrap_or_default();
    let ctype = util::mime(path);
    if !lm.is_empty() {
        if let Some(ims) = req.headers().get(header::IF_MODIFIED_SINCE).and_then(|v| v.to_str().ok()) {
            if ims == lm {
                return Response::builder()
                    .status(304)
                    .header(header::LAST_MODIFIED, &lm)
                    .header(header::CACHE_CONTROL, "no-cache")
                    .body(empty())
                    .unwrap();
            }
        }
    }

    // Range sederhana (satu rentang) supaya video/audio bisa di-seek.
    let mut start = 0u64;
    let mut end = len.saturating_sub(1);
    let mut partial = false;
    if let Some(r) = req.headers().get(header::RANGE).and_then(|v| v.to_str().ok()) {
        if let Some(spec) = r.strip_prefix("bytes=") {
            if !spec.contains(',') {
                if let Some((a, b)) = spec.split_once('-') {
                    let (a, b) = (a.trim(), b.trim());
                    let ok = if a.is_empty() {
                        b.parse::<u64>().ok().filter(|n| *n > 0).map(|n| {
                            start = len.saturating_sub(n);
                        })
                    } else {
                        a.parse::<u64>().ok().map(|s| {
                            start = s;
                            if let Ok(e) = b.parse::<u64>() {
                                end = e.min(end);
                            }
                        })
                    };
                    if ok.is_some() {
                        if start >= len || start > end {
                            return Response::builder()
                                .status(416)
                                .header(header::CONTENT_RANGE, format!("bytes */{len}"))
                                .body(empty())
                                .unwrap();
                        }
                        partial = true;
                    }
                }
            }
        }
    }
    let count = if len == 0 { 0 } else { end - start + 1 };
    let mut b = Response::builder()
        .status(if partial { StatusCode::PARTIAL_CONTENT } else { StatusCode::OK })
        .header(header::CONTENT_TYPE, ctype)
        .header(header::CONTENT_LENGTH, count)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CACHE_CONTROL, "no-cache");
    if !lm.is_empty() {
        b = b.header(header::LAST_MODIFIED, HeaderValue::from_str(&lm).unwrap());
    }
    if partial {
        b = b.header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{len}"));
    }
    if req.method() == Method::HEAD || count == 0 {
        return b.body(empty()).unwrap();
    }
    if !partial && len <= 1024 * 1024 {
        return match tokio::fs::read(path).await {
            Ok(data) => b.body(full(data)).unwrap(),
            Err(e) => simple(500, "Gagal membaca file", &html_escape(&e.to_string())),
        };
    }
    let (tx, rx) = tokio::sync::mpsc::channel::<io::Result<Bytes>>(4);
    let path = path.to_path_buf();
    tokio::spawn(async move {
        let mut f = match tokio::fs::File::open(&path).await {
            Ok(f) => f,
            Err(e) => {
                let _ = tx.send(Err(e)).await;
                return;
            }
        };
        if start > 0 {
            if let Err(e) = f.seek(io::SeekFrom::Start(start)).await {
                let _ = tx.send(Err(e)).await;
                return;
            }
        }
        let mut left = count;
        let mut buf = vec![0u8; 64 * 1024];
        while left > 0 {
            let want = (buf.len() as u64).min(left) as usize;
            match f.read(&mut buf[..want]).await {
                Ok(0) => break,
                Ok(n) => {
                    left -= n as u64;
                    if tx.send(Ok(Bytes::copy_from_slice(&buf[..n]))).await.is_err() {
                        break;
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(e)).await;
                    break;
                }
            }
        }
    });
    b.body(ChannelBody::new(rx).boxed()).unwrap()
}

fn status_json(st: &State, cfg: &Config) -> Response<Body> {
    let pools: Vec<serde_json::Value> = st
        .pools
        .info()
        .into_iter()
        .map(|p| serde_json::json!({"version": p.version, "port": p.port, "pid": p.pid, "idle_secs": p.idle_secs}))
        .collect();
    let up = st.started.elapsed().map(|d| d.as_secs()).unwrap_or(0);
    let v = serde_json::json!({
        "version": crate::VERSION,
        "pid": std::process::id(),
        "uptime_secs": up,
        "http_port": cfg.http_port,
        "https_port": cfg.https_port,
        "php_running": pools,
    });
    Response::builder()
        .header(header::CONTENT_TYPE, "application/json")
        .body(full(v.to_string()))
        .unwrap()
}

fn dashboard(st: &State, cfg: &Config) -> Response<Body> {
    let sites = cfg.sites();
    let installed = cfg.installed_php(&st.home);
    let global = cfg.global_php(&st.home).unwrap_or_else(|| "-".into());
    let running: Vec<String> = st.pools.info().iter().map(|p| format!("{} (pid {})", p.version, p.pid)).collect();
    let missing = crate::hosts::missing(cfg).unwrap_or_default();
    let warn = if missing.is_empty() {
        String::new()
    } else {
        format!(
            "<p class=warn>{} nama belum ada di hosts file ({}). Jalankan <code>hl hosts sync</code> lalu klik Yes pada UAC.</p>",
            missing.len(),
            html_escape(&missing.iter().take(5).cloned().collect::<Vec<_>>().join(", "))
        )
    };
    let mut rows = String::new();
    for s in &sites {
        let php = cfg.php_for(&st.home, &s.name).unwrap_or_else(|| "-".into());
        let iso = cfg.isolated.contains_key(&s.name);
        let url = cfg.url(&s.name);
        rows.push_str(&format!(
            "<tr><td><a href=\"{u}\">{h}</a>{lock}</td><td>{php}{isotag}</td><td>{kind}</td><td class=dim>{path}</td></tr>",
            u = html_escape(&url),
            h = html_escape(&cfg.host(&s.name)),
            lock = if cfg.is_secured(&s.name) { " 🔒" } else { "" },
            php = html_escape(&php),
            isotag = if iso { " <span class=tag>isolated</span>" } else { "" },
            kind = if s.kind == Kind::Linked { "link" } else { "park" },
            path = html_escape(&s.path.to_string_lossy()),
        ));
    }
    let html = format!(
        "<!doctype html><meta charset=utf-8><meta name=viewport content=\"width=device-width,initial-scale=1\"><title>herdlite</title>{CSS}\
<main><h1>herdlite <span class=dim>{ver}</span></h1>{warn}\
<p>PHP global: <b>{global}</b> · terpasang: {inst} · php-cgi aktif: {run}</p>\
<p class=dim>Folder park: {paths}</p>\
<table><thead><tr><th>Situs ({n})</th><th>PHP</th><th>Jenis</th><th>Folder</th></tr></thead><tbody>{rows}</tbody></table>\
<p class=dim>Ubah lewat terminal: <code>hl park</code>, <code>hl link</code>, <code>hl isolate 8.4</code>, <code>hl secure</code>. Halaman ini hanya menampilkan.</p></main>",
        ver = crate::VERSION,
        warn = warn,
        global = html_escape(&global),
        inst = if installed.is_empty() { "-".into() } else { installed.join(", ") },
        run = if running.is_empty() { "tidak ada (dinyalakan saat dibutuhkan)".into() } else { running.join(", ") },
        paths = if cfg.paths.is_empty() { "-".into() } else { html_escape(&cfg.paths.join(" · ")) },
        n = sites.len(),
    );
    Response::builder()
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .body(full(html))
        .unwrap()
}

const CSS: &str = "<style>:root{color-scheme:light dark;--bg:#fafaf9;--fg:#1c1917;--dim:#78716c;--line:#e7e5e4;--acc:#dc2626}\
@media (prefers-color-scheme:dark){:root{--bg:#1c1917;--fg:#f5f5f4;--dim:#a8a29e;--line:#44403c;--acc:#f87171}}\
body{margin:0;background:var(--bg);color:var(--fg);font:15px/1.5 system-ui,Segoe UI,sans-serif}main{max-width:1000px;margin:0 auto;padding:24px 16px}\
h1{font-size:22px}a{color:var(--acc);text-decoration:none}a:hover{text-decoration:underline}.dim{color:var(--dim)}\
table{width:100%;border-collapse:collapse;font-size:14px}th,td{text-align:left;padding:6px 8px;border-bottom:1px solid var(--line);vertical-align:top}\
td.dim{word-break:break-all}.tag{font-size:11px;border:1px solid var(--line);border-radius:4px;padding:0 4px;color:var(--dim)}code{font-size:13px}.warn{border:1px solid var(--acc);border-radius:6px;padding:8px 12px}</style>";
