//! The local bridge: while a person is signed in, the app listens on `127.0.0.1` (first free port of
//! [`PORTS`]) so the portal's web page can hand it a `figcraft://join?…` link by `fetch`, in browsers
//! that have no `figcraft://` handler registered (Firefox on some Windows setups).
//!
//! Endpoints: `OPTIONS *` (CORS preflight), `GET /ping` → `{"app":"figcraft","v":1}` and
//! `POST /open` with `{"url":"figcraft://join?…"}` → `202 {"ok":true}`.
//!
//! Every request must carry an `Origin` equal to the portal's origin and a `Host` of this very
//! address (no DNS rebinding); anything else gets an empty `403` and is never acted on. CORS headers
//! are sent only for the allowed origin. Reads are bounded in size and time, and only a few
//! connections are served at once. The link and tokens are never logged.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, Shutdown, TcpListener, TcpStream};
use std::ops::RangeInclusive;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crate::loopback::MAX_REQUEST;

/// The ports tried, in order.
pub const PORTS: RangeInclusive<u16> = 47955..=47964;

/// The most of a request body that is read.
pub const MAX_BODY: usize = 4 * 1024;

/// The longest link accepted.
const MAX_LINK: usize = 2048;

/// Connections served at the same time.
const MAX_CONNECTIONS: usize = 8;

const POLL: Duration = Duration::from_millis(20);
const CLIENT_TIME: Duration = Duration::from_secs(2);
const PREFIX: &str = "figcraft://join?";

/// Gets each accepted link, once.
pub type Sink = Arc<dyn Fn(String) + Send + Sync>;

/// A running bridge; dropping it (or [`Bridge::stop`]) closes the port.
pub struct Bridge {
    port: u16,
    stop: Arc<AtomicBool>,
}

impl Bridge {
    /// Listen on the first free port of [`PORTS`] for pages of `base_url`'s origin; `None` when the
    /// address has no usable origin or every port is taken (the feature is then simply off).
    pub fn start(base_url: &str, sink: Sink) -> Option<Self> {
        Self::start_on(PORTS, base_url, sink)
    }

    pub(crate) fn start_on(ports: RangeInclusive<u16>, base_url: &str, sink: Sink) -> Option<Self> {
        let origin = origin_of(base_url)?;
        let (listener, port) = ports.into_iter().find_map(|p| TcpListener::bind((Ipv4Addr::LOCALHOST, p)).ok().map(|l| (l, p)))?;
        listener.set_nonblocking(true).ok()?;
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        std::thread::Builder::new().name("figcraft-bridge".into()).spawn(move || serve(&listener, port, &origin, &sink, &flag)).ok()?;
        Some(Self { port, stop })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Close the port (within a few milliseconds).
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.stop();
    }
}

/// `scheme://host[:port]` of `url`, the way a browser writes an `Origin` (lower case, default port left out).
pub fn origin_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.trim().split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    let default = match scheme.as_str() {
        "http" => 80,
        "https" => 443,
        _ => return None,
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') || authority.starts_with('[') {
        return None;
    }
    let (host, port) = match authority.split_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().ok()?),
        None => (authority, default),
    };
    if host.is_empty() || !host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.') {
        return None;
    }
    let host = host.to_ascii_lowercase();
    Some(if port == default { format!("{scheme}://{host}") } else { format!("{scheme}://{host}:{port}") })
}

fn serve(listener: &TcpListener, port: u16, origin: &str, sink: &Sink, stop: &AtomicBool) {
    let busy = Arc::new(AtomicUsize::new(0));
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                if busy.load(Ordering::SeqCst) >= MAX_CONNECTIONS {
                    let _ = stream.shutdown(Shutdown::Both);
                    continue;
                }
                busy.fetch_add(1, Ordering::SeqCst);
                let (counter, origin, sink) = (busy.clone(), origin.to_string(), sink.clone());
                let spawned = std::thread::Builder::new().name("figcraft-bridge-client".into()).spawn(move || {
                    answer(stream, port, &origin, &sink);
                    counter.fetch_sub(1, Ordering::SeqCst);
                });
                if spawned.is_err() {
                    busy.fetch_sub(1, Ordering::SeqCst);
                }
            }
            Err(_) => std::thread::sleep(POLL),
        }
    }
}

struct Request {
    method: String,
    path: String,
    host: Option<String>,
    origin: Option<String>,
    content_type: Option<String>,
    body: Vec<u8>,
}

/// What an answer says: status and JSON body (if any).
type Reply = (u16, Option<&'static str>);

fn answer(mut stream: TcpStream, port: u16, origin: &str, sink: &Sink) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_write_timeout(Some(CLIENT_TIME));
    let (status, body, cors) = match read_request(&mut stream) {
        Ok(req) => {
            let allowed = req.origin.as_deref() == Some(origin);
            let host_ok = req.host.as_deref().is_some_and(|h| h == format!("127.0.0.1:{port}") || h == format!("localhost:{port}"));
            if allowed && host_ok {
                let (s, b) = route(&req, sink);
                (s, b, true)
            } else {
                (403, None, false)
            }
        }
        Err(status) => (status, None, false),
    };
    respond(&mut stream, status, body, cors.then_some(origin));
}

fn route(req: &Request, sink: &Sink) -> Reply {
    if req.method == "OPTIONS" {
        return (204, None);
    }
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/ping") => (200, Some("{\"app\":\"figcraft\",\"v\":1}")),
        ("POST", "/open") => open(req, sink),
        (_, "/ping" | "/open") => (405, None),
        _ => (404, None),
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenBody {
    url: String,
}

fn open(req: &Request, sink: &Sink) -> Reply {
    let media = req.content_type.as_deref().and_then(|c| c.split(';').next()).map(|m| m.trim().to_ascii_lowercase());
    if media.as_deref() != Some("application/json") {
        return (400, None);
    }
    let Ok(body) = serde_json::from_slice::<OpenBody>(&req.body) else { return (400, None) };
    if !valid_link(&body.url) {
        return (400, None);
    }
    sink(body.url);
    (202, Some("{\"ok\":true}"))
}

/// A `figcraft://join?…` link made only of printable ASCII, with a query of `key=value` pairs.
fn valid_link(url: &str) -> bool {
    let Some(query) = url.strip_prefix(PREFIX) else { return false };
    url.len() <= MAX_LINK
        && !query.is_empty()
        && url.bytes().all(|b| b.is_ascii_graphic())
        && !url.contains('#')
        && query.split('&').all(|p| p.split_once('=').is_some_and(|(k, _)| !k.is_empty()))
}

/// Read a whole request (head, then the body its `Content-Length` announces); `Err` is the status.
fn read_request(stream: &mut TcpStream) -> Result<Request, u16> {
    let deadline = Instant::now() + CLIENT_TIME;
    let mut buf = Vec::new();
    let end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        if buf.len() > MAX_REQUEST {
            return Err(431);
        }
        fill(stream, &mut buf, deadline)?;
    };
    let head = String::from_utf8_lossy(buf.get(..end).unwrap_or_default()).into_owned();
    let mut body = buf.get(end + 4..).unwrap_or_default().to_vec();
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or_default().split(' ');
    let (method, target) = (first.next().unwrap_or_default().to_string(), first.next().unwrap_or_default());
    let path = target.split('?').next().unwrap_or_default().to_string();
    let (mut host, mut origin, mut content_type, mut length): (Option<String>, Option<String>, Option<String>, Option<usize>) = (None, None, None, None);
    for line in lines {
        let Some((name, value)) = line.split_once(':') else { return Err(400) };
        let slot = match name.trim().to_ascii_lowercase().as_str() {
            "host" => &mut host,
            "origin" => &mut origin,
            "content-type" => &mut content_type,
            "content-length" => {
                if length.is_some() {
                    return Err(400);
                }
                length = Some(value.trim().parse::<usize>().map_err(|_| 400u16)?);
                continue;
            }
            _ => continue,
        };
        if slot.is_some() {
            return Err(400);
        }
        *slot = Some(value.trim().to_string());
    }
    if method == "POST" {
        let want = length.ok_or(400u16)?;
        if want > MAX_BODY {
            return Err(400);
        }
        while body.len() < want {
            fill(stream, &mut body, deadline)?;
        }
        body.truncate(want);
    }
    Ok(Request { method, path, host, origin, content_type, body })
}

fn fill(stream: &mut TcpStream, buf: &mut Vec<u8>, deadline: Instant) -> Result<(), u16> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(408);
    }
    let _ = stream.set_read_timeout(Some(left));
    let mut chunk = [0u8; 1024];
    match stream.read(&mut chunk) {
        Ok(0) | Err(_) => Err(400),
        Ok(n) => {
            buf.extend_from_slice(chunk.get(..n).unwrap_or_default());
            Ok(())
        }
    }
}

fn respond(stream: &mut TcpStream, status: u16, body: Option<&str>, origin: Option<&str>) {
    let reason = match status {
        200 => "OK",
        202 => "Accepted",
        204 => "No Content",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        431 => "Request Header Fields Too Large",
        _ => "Bad Request",
    };
    let body = body.unwrap_or_default();
    let cors = origin.map_or_else(String::new, |o| {
        format!(
            "Access-Control-Allow-Origin: {o}\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Headers: content-type\r\n\
Access-Control-Allow-Private-Network: true\r\nVary: Origin\r\nAccess-Control-Max-Age: 60\r\n"
        )
    });
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{cors}Cache-Control: no-store\r\n\
X-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
    let _ = stream.flush();
    let _ = stream.shutdown(Shutdown::Both);
}
