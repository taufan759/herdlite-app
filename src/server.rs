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
    /// Token acak per sesi server untuk API dashboard.
    token: String,
    job: Mutex<Option<(String, std::process::Child)>>,
    job_result: Mutex<Option<String>>,
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

fn new_token() -> String {
    let mut b = [0u8; 24];
    let _ = ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
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
        token: new_token(),
        job: Mutex::new(None),
        job_result: Mutex::new(None),
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
        let path = req.uri().path().to_string();
        if path == "/__herdlite/status" {
            return status_json(&st, &cfg);
        }
        if let Some(action) = path.strip_prefix("/__herdlite/api/") {
            return api(&st, req, action).await;
        }
        return dashboard_page(&st);
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

fn json_resp(code: u16, v: serde_json::Value) -> Response<Body> {
    Response::builder()
        .status(code)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CACHE_CONTROL, "no-store")
        .body(full(v.to_string()))
        .unwrap()
}

/// Halaman dashboard. Token disisipkan ke halaman; hanya halaman yang dibuka langsung di
/// herdlite.<tld>/localhost yang bisa membacanya (website lain terhalang same-origin policy).
fn dashboard_page(st: &State) -> Response<Body> {
    let html = include_str!("dashboard.html").replace("{{TOKEN}}", &st.token);
    Response::builder()
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-store")
        .header("X-Frame-Options", "DENY")
        .header(
            "Content-Security-Policy",
            "default-src 'self'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src 'self' data:; frame-ancestors 'none'; base-uri 'none'; form-action 'none'",
        )
        .body(full(html))
        .unwrap()
}

/// Versi PHP yang ditawarkan di dashboard untuk dipasang.
const PHP_INSTALLABLE: &[&str] = &["7.4", "8.0", "8.1", "8.2", "8.3", "8.4", "8.5"];

fn job_status(st: &State) -> (Option<String>, Option<String>) {
    if let Ok(mut j) = st.job.lock() {
        if let Some((label, child)) = j.as_mut() {
            match child.try_wait() {
                Ok(None) => return (Some(format!("Memasang {label}… (biasanya 1–3 menit)")), None),
                Ok(Some(s)) => {
                    let res = if s.success() {
                        format!("✓ {label} terpasang")
                    } else {
                        format!("✗ {label} gagal dipasang (lihat logs\\install.log)")
                    };
                    if let Ok(mut r) = st.job_result.lock() {
                        *r = Some(res);
                    }
                    *j = None;
                }
                Err(_) => *j = None,
            }
        }
    }
    (None, st.job_result.lock().ok().and_then(|r| r.clone()))
}

fn api_state(st: &State) -> serde_json::Value {
    let cfg = st.cfg();
    let home = &st.home;
    let global = cfg.global_php(home);
    let sites: Vec<serde_json::Value> = cfg
        .sites()
        .iter()
        .map(|s| {
            serde_json::json!({
                "name": s.name,
                "host": cfg.host(&s.name),
                "url": cfg.url(&s.name),
                "php": cfg.php_for(home, &s.name),
                "isolated": cfg.isolated.contains_key(&s.name),
                "path": s.path.to_string_lossy(),
                "linked": s.kind == Kind::Linked,
            })
        })
        .collect();
    let (job, job_result) = job_status(st);
    serde_json::json!({
        "version": crate::VERSION,
        "tld": cfg.tld,
        "http_port": cfg.http_port,
        "https_port": cfg.https_port,
        "php_installed": cfg.installed_php(home),
        "php_global": global,
        "php_running": st.pools.info().iter().map(|p| p.version.clone()).collect::<Vec<_>>(),
        "php_installable": PHP_INSTALLABLE,
        "sites": sites,
        "hosts_missing": crate::hosts::missing(&cfg).unwrap_or_default(),
        "sites_dir": cfg.paths.first(),
        "job": job,
        "job_result": job_result,
    })
}

fn hl_exe() -> Result<PathBuf, String> {
    let p = std::env::current_exe().map_err(|e| e.to_string())?.with_file_name("hl.exe");
    if p.is_file() { Ok(p) } else { Err("hl.exe tidak ditemukan di sebelah herdlited.exe".into()) }
}

fn spawn_quiet(cmd: &mut std::process::Command) -> Result<std::process::Child, String> {
    use std::os::windows::process::CommandExt;
    cmd.stdin(std::process::Stdio::null())
        .creation_flags(crate::win::CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| e.to_string())
}

fn explorer(path: &Path) -> Result<(), String> {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    std::process::Command::new(PathBuf::from(root).join("explorer.exe"))
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Terminal di folder tertentu: Windows Terminal kalau ada, selain itu cmd.
fn terminal(path: &Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    if let Ok(la) = std::env::var("LOCALAPPDATA") {
        let wt = PathBuf::from(la).join(r"Microsoft\WindowsApps\wt.exe");
        if wt.exists() && std::process::Command::new(&wt).arg("-d").arg(path).spawn().is_ok() {
            return Ok(());
        }
    }
    std::process::Command::new(crate::win::sys("cmd.exe"))
        .raw_arg(format!("/c start \"\" /D \"{}\" \"{}\"", path.display(), crate::win::sys("cmd.exe").display()))
        .creation_flags(crate::win::CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

const NEW_SITE_INDEX: &str = "<?php\n// Situs baru dari dashboard herdlite. Ubah file ini lalu refresh browser.\n?>\n<!doctype html>\n<meta charset=\"utf-8\">\n<title><?= htmlspecialchars($_SERVER['HTTP_HOST'] ?? 'situs') ?></title>\n<h1>Halo dari <?= htmlspecialchars($_SERVER['HTTP_HOST'] ?? 'situs') ?></h1>\n<p>PHP <?= PHP_VERSION ?> · file ini: <code><?= htmlspecialchars(__FILE__) ?></code></p>\n";

fn api_action(st: &State, action: &str, v: &serde_json::Value) -> Result<String, String> {
    let home = &st.home;
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
    let mut cfg = Config::load(home)?;
    let site_of = |cfg: &Config, name: &str| cfg.find_site(name).ok_or_else(|| format!("situs '{name}' tidak ditemukan"));
    match action {
        "set_php" => {
            let site = site_of(&cfg, &s("site"))?;
            let ver = s("version");
            if ver.is_empty() {
                cfg.isolated.remove(&site.name);
            } else {
                let ver = config::parse_ver(&ver).ok_or("versi tidak valid")?;
                if !cfg.installed_php(home).contains(&ver) {
                    return Err(format!("PHP {ver} belum terpasang"));
                }
                cfg.isolated.insert(site.name.clone(), ver);
            }
            cfg.save(home)?;
            Ok(String::new())
        }
        "set_global" => {
            let ver = config::parse_ver(&s("version")).ok_or("versi tidak valid")?;
            if !cfg.installed_php(home).contains(&ver) {
                return Err(format!("PHP {ver} belum terpasang"));
            }
            cfg.default_php = ver;
            cfg.save(home)?;
            crate::phpinstall::write_shims(home, &cfg)?;
            Ok(String::new())
        }
        "open_folder" => explorer(&site_of(&cfg, &s("site"))?.path).map(|_| String::new()),
        "open_terminal" => terminal(&site_of(&cfg, &s("site"))?.path).map(|_| String::new()),
        "open_sites_dir" => {
            let d = cfg.paths.first().ok_or("belum ada folder proyek")?;
            explorer(Path::new(d)).map(|_| String::new())
        }
        "new_site" => {
            let name = s("name").to_lowercase();
            let ok = !name.is_empty()
                && name.len() <= 63
                && !name.starts_with('-')
                && !name.ends_with('-')
                && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
            if !ok {
                return Err("nama hanya boleh huruf kecil, angka, dan '-' (mis. toko-online)".into());
            }
            let dir = PathBuf::from(cfg.paths.first().ok_or("belum ada folder proyek")?).join(&name);
            if dir.exists() {
                return Err(format!("folder {} sudah ada", dir.display()));
            }
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            std::fs::write(dir.join("index.php"), NEW_SITE_INDEX).map_err(|e| e.to_string())?;
            let missing = crate::hosts::missing(&cfg).unwrap_or_default().contains(&cfg.host(&name));
            Ok(if missing {
                format!("Situs {} dibuat. Klik \"Daftarkan sekarang\" supaya bisa dibuka.", cfg.host(&name))
            } else {
                format!("Situs {} dibuat", cfg.host(&name))
            })
        }
        "hosts_sync" => {
            spawn_quiet(std::process::Command::new(hl_exe()?).args(["hosts", "sync"]).stdout(std::process::Stdio::null()))?;
            Ok("Pilih Yes pada jendela izin Windows…".into())
        }
        "install_php" => {
            let ver = config::parse_ver(&s("version")).ok_or("versi tidak valid")?;
            if !PHP_INSTALLABLE.contains(&ver.as_str()) {
                return Err("versi tidak tersedia".into());
            }
            let mut job = st.job.lock().map_err(|_| "lock")?;
            if job.is_some() {
                return Err("masih ada pemasangan yang berjalan".into());
            }
            let _ = std::fs::create_dir_all(home.join("logs"));
            let log = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(home.join("logs").join("install.log"))
                .map_err(|e| e.to_string())?;
            let log2 = log.try_clone().map_err(|e| e.to_string())?;
            let child = spawn_quiet(std::process::Command::new(hl_exe()?).args(["php:install", &ver]).stdout(log).stderr(log2))?;
            *job = Some((format!("PHP {ver}"), child));
            if let Ok(mut r) = st.job_result.lock() {
                *r = None;
            }
            Ok(format!("Memasang PHP {ver}…"))
        }
        _ => Err(format!("aksi '{action}' tidak dikenal")),
    }
}

/// API dashboard. Wajib header X-Herdlite-Token (memaksa preflight CORS, jadi website lain
/// tidak bisa memanggilnya) dan token yang hanya ada di halaman dashboard.
async fn api(st: &Arc<State>, req: Request<Incoming>, action: &str) -> Response<Body> {
    let tok_ok = req.headers().get("x-herdlite-token").and_then(|v| v.to_str().ok()) == Some(st.token.as_str());
    let site_ok = match req.headers().get("sec-fetch-site").and_then(|v| v.to_str().ok()) {
        None => true,
        Some(v) => v == "same-origin" || v == "none",
    };
    if !tok_ok || !site_ok {
        return json_resp(403, serde_json::json!({"ok": false, "error": "token tidak valid, muat ulang halaman"}));
    }
    if action == "state" {
        return json_resp(200, api_state(st));
    }
    if req.method() != Method::POST {
        return json_resp(405, serde_json::json!({"ok": false, "error": "harus POST"}));
    }
    let body = match Limited::new(req.into_body(), 64 * 1024).collect().await {
        Ok(c) => c.to_bytes(),
        Err(_) => return json_resp(413, serde_json::json!({"ok": false, "error": "body terlalu besar"})),
    };
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap_or(serde_json::json!({}));
    match api_action(st, action, &v) {
        Ok(msg) => json_resp(200, serde_json::json!({"ok": true, "message": msg})),
        Err(e) => json_resp(400, serde_json::json!({"ok": false, "error": e})),
    }
}

const CSS: &str = "<style>:root{color-scheme:light dark;--bg:#fafaf9;--fg:#1c1917;--dim:#78716c;--line:#e7e5e4;--acc:#dc2626}\
@media (prefers-color-scheme:dark){:root{--bg:#1c1917;--fg:#f5f5f4;--dim:#a8a29e;--line:#44403c;--acc:#f87171}}\
body{margin:0;background:var(--bg);color:var(--fg);font:15px/1.5 system-ui,Segoe UI,sans-serif}main{max-width:1000px;margin:0 auto;padding:24px 16px}\
h1{font-size:22px}a{color:var(--acc);text-decoration:none}a:hover{text-decoration:underline}.dim{color:var(--dim)}\
table{width:100%;border-collapse:collapse;font-size:14px}th,td{text-align:left;padding:6px 8px;border-bottom:1px solid var(--line);vertical-align:top}\
td.dim{word-break:break-all}.tag{font-size:11px;border:1px solid var(--line);border-radius:4px;padding:0 4px;color:var(--dim)}code{font-size:13px}.warn{border:1px solid var(--acc);border-radius:6px;padding:8px 12px}</style>";
