//! `serve <dir> [--port 18082] [--gateway http://127.0.0.1:8080 | --no-proxy] [--log]`: a UI bundle served
//! WITHOUT installing it, for when /opt/aetherrv-rs/ui must stay as deployed. Files come from the directory;
//! `/api/…` and `/ws` go to the gateway on loopback (the page keeps the kiosk's rights), relayed byte for byte after
//! the request head, so a WebSocket message crosses one extra loopback hop and nothing else. The runtimes open
//! http://127.0.0.1:<port>/?bench=1. `--no-proxy`: files only (launcher-check.sh's test origins); `--log`: one line
//! per request on stderr, 404s included.

use crate::{arg, flag};
use anyhow::{Context, Result, bail};
use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

struct Conf {
    root: PathBuf,
    gw: Option<Gateway>,
    log: bool,
}
struct Gateway {
    hostport: String,
    prefix: String,
}

pub fn main(args: &[String]) -> Result<()> {
    let dir = args.first().filter(|a| !a.starts_with("--")).context("usage: serve <dir> [--port N] [--gateway URL | --no-proxy] [--log]")?;
    let port: u16 = arg(args, "--port").unwrap_or("18082").parse()?;
    let url = arg(args, "--gateway").unwrap_or("http://127.0.0.1:8080").trim_end_matches('/');
    let gw = if flag(args, "--no-proxy") {
        None
    } else {
        let Some(rest) = url.strip_prefix("http://") else { bail!("--gateway: an http:// URL") };
        let (hp, prefix) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
        let hostport = if hp.contains(':') { hp.to_string() } else { format!("{hp}:80") };
        Some(Gateway { hostport, prefix: prefix.to_string() })
    };
    let root = std::fs::canonicalize(dir).with_context(|| dir.clone())?;
    let l = TcpListener::bind(("127.0.0.1", port))?;
    match gw {
        Some(_) => println!("bundle {} on http://127.0.0.1:{port}/ (api, ws → {url})", root.display()),
        None => println!("files {} on http://127.0.0.1:{port}/", root.display()),
    }
    std::io::stdout().flush()?;
    let conf = Arc::new(Conf { root, gw, log: flag(args, "--log") });
    for s in l.incoming().flatten() {
        let conf = conf.clone();
        std::thread::spawn(move || {
            let _ = connection(s, &conf);
        });
    }
    Ok(())
}

struct Req {
    line: String,
    method: String,
    target: String,
    version: String,
    headers: Vec<(String, String)>,
}
impl Req {
    fn header(&self, k: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(k)).map(|(_, v)| v.as_str())
    }
}

/// Reads up to the end of a head (request or response) into `buf`: its length with the blank line, or None at a
/// clean end of the stream. What came after it stays in `buf`.
fn read_head(s: &mut TcpStream, buf: &mut Vec<u8>) -> std::io::Result<Option<usize>> {
    let mut b = [0u8; 8192];
    loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            return Ok(Some(i + 4));
        }
        if buf.len() > 65536 {
            return Err(ErrorKind::InvalidData.into());
        }
        match s.read(&mut b)? {
            0 if buf.is_empty() => return Ok(None),
            0 => return Err(ErrorKind::UnexpectedEof.into()),
            n => buf.extend_from_slice(&b[..n]),
        }
    }
}

fn parse(head: &[u8]) -> Option<Req> {
    let head = std::str::from_utf8(head).ok()?;
    let mut lines = head.split("\r\n");
    let line = lines.next()?.to_string();
    let mut p = line.split(' ');
    let (method, target, version) = (p.next()?.to_string(), p.next()?.to_string(), p.next()?.to_string());
    let headers = lines.filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_string(), v.trim().to_string())).collect();
    Some(Req { line, method, target, version, headers })
}

fn connection(mut c: TcpStream, conf: &Conf) -> std::io::Result<()> {
    let peer = c.peer_addr().map(|a| a.ip().to_string()).unwrap_or_default();
    let mut buf = Vec::new();
    loop {
        c.set_read_timeout(Some(Duration::from_secs(75)))?; // an idle keep-alive connection
        let Some(n) = read_head(&mut c, &mut buf)? else { return Ok(()) };
        let Some(req) = parse(&buf[..n]) else {
            return respond(&mut c, "400 Bad Request", "text/plain; charset=utf-8", &[], b"400: Bad Request", true, false);
        };
        buf.drain(..n);
        let path = req.target.split(['?', '#']).next().unwrap_or("").to_string();
        if let Some(gw) = &conf.gw
            && (path.starts_with("/api/") || path == "/ws")
        {
            if conf.log {
                eprintln!("{peer} \"{}\" → gateway", req.line);
            }
            return relay(c, &req, std::mem::take(&mut buf), gw, path == "/ws");
        }
        // files: no request body expected; one that has one ends the connection rather than be parsed
        let close = req.header("connection").is_some_and(|v| v.eq_ignore_ascii_case("close"))
            || req.version == "HTTP/1.0" && !req.header("connection").is_some_and(|v| v.eq_ignore_ascii_case("keep-alive"))
            || req.header("content-length").is_some_and(|v| v != "0")
            || req.header("transfer-encoding").is_some();
        let status = file(&mut c, conf, &req, &path, close)?;
        if conf.log {
            eprintln!("{peer} \"{}\" {status}", req.line);
        }
        if close {
            return Ok(());
        }
    }
}

fn respond(c: &mut TcpStream, status: &str, ctype: &str, extra: &[(&str, String)], body: &[u8], close: bool, head_only: bool) -> std::io::Result<()> {
    let mut h = format!("HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\n", body.len());
    for (k, v) in extra {
        h += &format!("{k}: {v}\r\n");
    }
    if close {
        h += "Connection: close\r\n";
    }
    h += "\r\n";
    c.write_all(h.as_bytes())?;
    if !head_only {
        c.write_all(body)?;
    }
    c.flush()
}

fn file(c: &mut TcpStream, conf: &Conf, req: &Req, path: &str, close: bool) -> std::io::Result<u16> {
    let head_only = req.method == "HEAD";
    if req.method != "GET" && !head_only {
        respond(c, "405 Method Not Allowed", "text/plain; charset=utf-8", &[("Allow", "GET,HEAD".into())], b"405: Method Not Allowed", close, false)?;
        return Ok(405);
    }
    let rel = String::from_utf8_lossy(&percent_decode(path)).trim_start_matches('/').to_string();
    let rel = if rel.is_empty() { "index.html".to_string() } else { rel };
    // only a file inside the directory, links resolved (no way out by .. or a symlink)
    let f = conf.root.join(&rel).canonicalize().ok().filter(|f| f.starts_with(&conf.root) && f != &conf.root && f.is_file());
    let Some(f) = f else {
        respond(c, "404 Not Found", "text/plain; charset=utf-8", &[], b"404: Not Found", close, head_only)?;
        return Ok(404);
    };
    let meta = std::fs::metadata(&f)?;
    let mtime = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos());
    let etag = format!("\"{mtime:x}-{:x}\"", meta.len());
    let cache = if f.file_name().is_some_and(|n| n == "index.html") { "no-cache" } else { "public, max-age=31536000, immutable" };
    let extra = [("Cache-Control", cache.to_string()), ("ETag", etag.clone())];
    if req.header("if-none-match").is_some_and(|v| v.split(',').any(|t| t.trim() == etag)) {
        let mut h = format!("HTTP/1.1 304 Not Modified\r\nCache-Control: {cache}\r\nETag: {etag}\r\n");
        if close {
            h += "Connection: close\r\n";
        }
        c.write_all(format!("{h}\r\n").as_bytes())?;
        return Ok(304);
    }
    respond(c, "200 OK", mime(&f), &extra, &std::fs::read(&f)?, close, head_only)?;
    Ok(200)
}

/// The types Python's mimetypes gives (the old server's); module scripts need a JavaScript type.
fn mime(p: &Path) -> &'static str {
    match p.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("html" | "htm") => "text/html",
        Some("js" | "mjs") => "text/javascript",
        Some("css") => "text/css",
        Some("json") => "application/json",
        Some("webmanifest") => "application/manifest+json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("avif") => "image/avif",
        Some("ico") => "image/vnd.microsoft.icon",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        Some("otf") => "font/otf",
        Some("wasm") => "application/wasm",
        Some("txt") => "text/plain",
        Some("wav") => "audio/x-wav",
        Some("mp3") => "audio/mpeg",
        Some("ogg") => "audio/ogg",
        Some("glb") => "model/gltf-binary",
        _ => "application/octet-stream",
    }
}

fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |x: u8| (x as char).to_digit(16);
        if b[i] == b'%'
            && let (Some(h), Some(l)) = (b.get(i + 1).and_then(|x| hex(*x)), b.get(i + 2).and_then(|x| hex(*x)))
        {
            out.push((h * 16 + l) as u8);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

/// `/api/…` (one request, then the connection ends) and `/ws` (both ways until either side closes), relayed to the
/// gateway with a head of its own: the gateway's Host, and only the headers the request needs.
fn relay(mut c: TcpStream, req: &Req, early: Vec<u8>, gw: &Gateway, ws: bool) -> std::io::Result<()> {
    c.set_read_timeout(None)?;
    let Ok(mut up) = TcpStream::connect(&gw.hostport) else {
        return respond(&mut c, "502 Bad Gateway", "text/plain; charset=utf-8", &[], b"502: Bad Gateway", true, false);
    };
    let keep: &[&str] = if ws {
        &["upgrade", "connection", "sec-websocket-key", "sec-websocket-version", "sec-websocket-extensions", "sec-websocket-protocol"]
    } else {
        &["content-type", "accept", "content-length", "transfer-encoding"]
    };
    let mut h = format!("{} {}{} HTTP/1.1\r\nHost: {}\r\n", req.method, gw.prefix, req.target, gw.hostport);
    for (k, v) in req.headers.iter().filter(|(k, _)| keep.contains(&k.to_ascii_lowercase().as_str())) {
        h += &format!("{k}: {v}\r\n");
    }
    if !ws {
        h += "Connection: close\r\n";
    }
    up.write_all(format!("{h}\r\n").as_bytes())?;
    up.write_all(&early)?; // a body or the first WebSocket frames, already read with the head
    let (mut c2, mut up2) = (c.try_clone()?, up.try_clone()?);
    let to_gw = std::thread::spawn(move || {
        let _ = std::io::copy(&mut c2, &mut up2);
        let _ = up2.shutdown(Shutdown::Write);
    });
    if ws {
        let _ = std::io::copy(&mut up, &mut c);
    } else {
        // the page must not reuse this connection: the answer says so
        let mut b = Vec::new();
        if let Ok(Some(n)) = read_head(&mut up, &mut b) {
            let head = String::from_utf8_lossy(&b[..n]).to_string();
            let mut lines: Vec<&str> = head.trim_end().split("\r\n").filter(|l| {
                let k = l.split(':').next().unwrap_or("").trim();
                !k.eq_ignore_ascii_case("connection") && !k.eq_ignore_ascii_case("keep-alive")
            }).collect();
            lines.push("Connection: close");
            c.write_all(format!("{}\r\n\r\n", lines.join("\r\n")).as_bytes())?;
            c.write_all(&b[n..])?;
        } else {
            c.write_all(&b)?;
        }
        let _ = std::io::copy(&mut up, &mut c);
    }
    let _ = c.shutdown(Shutdown::Both); // ends the other direction too
    let _ = up.shutdown(Shutdown::Both);
    let _ = to_gw.join();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode() {
        assert_eq!(percent_decode("/a%20b%2e%2Ejs%zz%4"), b"/a b..js%zz%4");
    }
}
