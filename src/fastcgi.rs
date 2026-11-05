//! Klien FastCGI minimal (cukup untuk php-cgi): body request di-stream ke PHP,
//! output PHP di-stream balik ke browser.

use bytes::Bytes;
use http_body_util::BodyExt;
use hyper::body::{Body as HttpBody, Frame};
use hyper::header::{HeaderName, HeaderValue};
use hyper::{Response, StatusCode};
use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

pub type Body = http_body_util::combinators::BoxBody<Bytes, io::Error>;

const BEGIN_REQUEST: u8 = 1;
const END_REQUEST: u8 = 3;
const PARAMS: u8 = 4;
const STDIN: u8 = 5;
const STDOUT: u8 = 6;
const STDERR: u8 = 7;

/// Body response yang diisi dari channel (dipakai untuk streaming).
pub struct ChannelBody {
    rx: mpsc::Receiver<io::Result<Bytes>>,
}

impl ChannelBody {
    pub fn new(rx: mpsc::Receiver<io::Result<Bytes>>) -> Self {
        ChannelBody { rx }
    }
}

impl HttpBody for ChannelBody {
    type Data = Bytes;
    type Error = io::Error;
    fn poll_frame(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        self.rx.poll_recv(cx).map(|o| o.map(|r| r.map(Frame::data)))
    }
}

fn header(t: u8, len: usize, pad: usize) -> [u8; 8] {
    [1, t, 0, 1, (len >> 8) as u8, (len & 0xff) as u8, pad as u8, 0]
}

async fn write_stream<W: AsyncWriteExt + Unpin>(w: &mut W, t: u8, data: &[u8]) -> io::Result<()> {
    for chunk in data.chunks(65535) {
        let pad = (8 - chunk.len() % 8) % 8;
        w.write_all(&header(t, chunk.len(), pad)).await?;
        w.write_all(chunk).await?;
        w.write_all(&[0u8; 8][..pad]).await?;
    }
    Ok(())
}

async fn write_end<W: AsyncWriteExt + Unpin>(w: &mut W, t: u8) -> io::Result<()> {
    w.write_all(&header(t, 0, 0)).await
}

fn encode_len(buf: &mut Vec<u8>, n: usize) {
    if n < 128 {
        buf.push(n as u8);
    } else {
        buf.extend_from_slice(&((n as u32) | 0x8000_0000).to_be_bytes());
    }
}

async fn read_rec<R: AsyncReadExt + Unpin>(r: &mut R) -> io::Result<Option<(u8, Vec<u8>)>> {
    let mut h = [0u8; 8];
    match r.read_exact(&mut h).await {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u16::from_be_bytes([h[4], h[5]]) as usize;
    let pad = h[6] as usize;
    let mut buf = vec![0u8; len + pad];
    r.read_exact(&mut buf).await?;
    buf.truncate(len);
    Ok(Some((h[1], buf)))
}

async fn connect(port: u16) -> io::Result<TcpStream> {
    // php-cgi yang baru dinyalakan butuh sesaat sebelum siap menerima koneksi.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match TcpStream::connect(("127.0.0.1", port)).await {
            Ok(s) => return Ok(s),
            Err(e) => {
                if Instant::now() >= deadline {
                    return Err(e);
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
}

fn find_header_end(b: &[u8]) -> Option<(usize, usize)> {
    if let Some(p) = b.windows(4).position(|w| w == b"\r\n\r\n") {
        return Some((p, p + 4));
    }
    b.windows(2).position(|w| w == b"\n\n").map(|p| (p, p + 2))
}

fn log_stderr(log: &PathBuf, data: &[u8]) {
    let s = String::from_utf8_lossy(data);
    for line in s.lines().filter(|l| !l.trim().is_empty()) {
        crate::util::append_log(log, line);
    }
}

/// Kirim satu request ke php-cgi di `port` dan kembalikan response HTTP.
pub async fn run(port: u16, params: Vec<(String, String)>, mut body: Body, errlog: PathBuf) -> Result<Response<Body>, String> {
    let stream = connect(port).await.map_err(|e| format!("tidak bisa terhubung ke php-cgi (port {port}): {e}"))?;
    let _ = stream.set_nodelay(true);
    let (rd, wr) = stream.into_split();
    let mut rd = BufReader::with_capacity(64 * 1024, rd);
    let mut w = BufWriter::with_capacity(64 * 1024, wr);
    let io_err = |e: io::Error| format!("koneksi ke php-cgi putus: {e}");

    w.write_all(&header(BEGIN_REQUEST, 8, 0)).await.map_err(io_err)?;
    w.write_all(&[0, 1, 0, 0, 0, 0, 0, 0]).await.map_err(io_err)?;

    let mut pbuf = Vec::with_capacity(2048);
    for (k, v) in &params {
        encode_len(&mut pbuf, k.len());
        encode_len(&mut pbuf, v.len());
        pbuf.extend_from_slice(k.as_bytes());
        pbuf.extend_from_slice(v.as_bytes());
    }
    write_stream(&mut w, PARAMS, &pbuf).await.map_err(io_err)?;
    write_end(&mut w, PARAMS).await.map_err(io_err)?;

    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|e| format!("gagal membaca body request: {e}"))?;
        if let Ok(data) = frame.into_data() {
            write_stream(&mut w, STDIN, &data).await.map_err(io_err)?;
        }
    }
    write_end(&mut w, STDIN).await.map_err(io_err)?;
    w.flush().await.map_err(io_err)?;

    // Baca sampai header CGI lengkap.
    let mut head: Vec<u8> = Vec::new();
    let mut ended = false;
    let split = loop {
        match read_rec(&mut rd).await.map_err(io_err)? {
            None => {
                ended = true;
                break None;
            }
            Some((STDOUT, data)) => {
                head.extend_from_slice(&data);
                if let Some(s) = find_header_end(&head) {
                    break Some(s);
                }
            }
            Some((STDERR, data)) => log_stderr(&errlog, &data),
            Some((END_REQUEST, _)) => {
                ended = true;
                break None;
            }
            Some(_) => {}
        }
    };

    let (head_text, rest) = match split {
        Some((end, start)) => (String::from_utf8_lossy(&head[..end]).into_owned(), head[start..].to_vec()),
        None if head.is_empty() => return Err("php-cgi tidak mengirim respons apa pun (lihat logs\\php.log)".into()),
        None => (String::new(), head.clone()),
    };

    let mut builder = Response::builder();
    let mut status: Option<u16> = None;
    let mut has_location = false;
    for line in head_text.lines() {
        let Some((k, v)) = line.split_once(':') else { continue };
        let (k, v) = (k.trim(), v.trim());
        if k.eq_ignore_ascii_case("status") {
            status = v.split_whitespace().next().and_then(|c| c.parse().ok());
            continue;
        }
        let lk = k.to_ascii_lowercase();
        if matches!(lk.as_str(), "connection" | "transfer-encoding" | "keep-alive") {
            continue;
        }
        if lk == "location" {
            has_location = true;
        }
        if let (Ok(n), Ok(val)) = (HeaderName::from_bytes(k.as_bytes()), HeaderValue::from_str(v)) {
            builder = builder.header(n, val);
        }
    }
    let code = status.unwrap_or(if has_location { 302 } else { 200 });
    builder = builder.status(StatusCode::from_u16(code).unwrap_or(StatusCode::OK));

    let (tx, rx) = mpsc::channel::<io::Result<Bytes>>(16);
    if !rest.is_empty() {
        let _ = tx.try_send(Ok(Bytes::from(rest)));
    }
    if !ended {
        tokio::spawn(async move {
            let _keep_writer_open = w;
            loop {
                match read_rec(&mut rd).await {
                    Ok(Some((STDOUT, data))) => {
                        if !data.is_empty() && tx.send(Ok(Bytes::from(data))).await.is_err() {
                            break; // browser sudah menutup koneksi
                        }
                    }
                    Ok(Some((STDERR, data))) => log_stderr(&errlog, &data),
                    Ok(Some((END_REQUEST, _))) | Ok(None) => break,
                    Ok(Some(_)) => {}
                    Err(e) => {
                        let _ = tx.send(Err(e)).await;
                        break;
                    }
                }
            }
        });
    }
    builder
        .body(ChannelBody::new(rx).boxed())
        .map_err(|e| format!("header respons PHP tidak valid: {e}"))
}
