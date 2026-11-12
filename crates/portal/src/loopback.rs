//! The loopback redirect of a sign-in (RFC 8252 section 7.3): the app listens on `127.0.0.1` on a
//! port the system picks, the portal's login page sends the browser to
//! `http://127.0.0.1:<port>/auth?code=…&state=…`, and the page shown tells the person to return to
//! the app. It works in every browser, unlike the `figiran://` scheme some browsers refuse.
//!
//! One listener serves one sign-in. It answers only a request that carries the expected `state`
//! (random, per login) and a `Host` of this very address (so a web page can't reach it through a
//! rebound DNS name); anything else gets an empty error and the listener keeps waiting. It closes
//! on the first valid callback, on [`Loopback::stop`] (or drop), and after its lifetime.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// The most of a request that is read (request line and headers).
pub const MAX_REQUEST: usize = 8 * 1024;

/// How often the listener looks at its stop flag and clock.
const POLL: Duration = Duration::from_millis(20);

/// How long one client gets to send its request.
const CLIENT_TIME: Duration = Duration::from_secs(2);

const PAGE: &str = "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>FigIran</title>\
<style>body{font:16px system-ui,sans-serif;margin:4rem auto;max-width:28rem;text-align:center}</style></head>\
<body><h1>FigIran</h1><p>Signed in. You can close this tab and return to FigIran.</p></body></html>";

/// A running listener; dropping it (or [`Loopback::stop`]) closes the port.
pub struct Loopback {
    port: u16,
    stop: Arc<AtomicBool>,
}

impl Loopback {
    /// Listen on a free `127.0.0.1` port for the callback carrying `state`; `on_code` gets its code
    /// once, and the listener then closes. It also closes after `lifetime`.
    pub fn start(state: String, lifetime: Duration, on_code: impl FnOnce(String) + Send + 'static) -> Result<Self, String> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|e| format!("can't listen on this computer: {e}"))?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        std::thread::Builder::new()
            .name("figiran-login-callback".into())
            .spawn(move || serve(&listener, port, &state, lifetime, &flag, on_code))
            .map_err(|e| e.to_string())?;
        Ok(Self { port, stop })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Close the port (within a few milliseconds).
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

impl Drop for Loopback {
    fn drop(&mut self) {
        self.stop();
    }
}

fn serve(listener: &TcpListener, port: u16, state: &str, lifetime: Duration, stop: &AtomicBool, on_code: impl FnOnce(String)) {
    let end = Instant::now() + lifetime;
    while !stop.load(Ordering::SeqCst) && Instant::now() < end {
        match listener.accept() {
            Ok((stream, _)) => {
                if let Some(code) = answer(stream, port, state) {
                    stop.store(true, Ordering::SeqCst);
                    on_code(code);
                    return;
                }
            }
            Err(_) => std::thread::sleep(POLL),
        }
    }
}

/// Answer one client; the code when it was the valid callback.
fn answer(mut stream: TcpStream, port: u16, state: &str) -> Option<String> {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_write_timeout(Some(CLIENT_TIME));
    let verdict = read_head(&mut stream).and_then(|head| check(&head, port, state));
    match verdict {
        Ok(code) => {
            respond(&mut stream, 200, Some(PAGE));
            Some(code)
        }
        Err(status) => {
            respond(&mut stream, status, None);
            None
        }
    }
}

/// The request line and headers (up to [`MAX_REQUEST`] bytes); `Err` is the status to answer with.
fn read_head(stream: &mut TcpStream) -> Result<String, u16> {
    let deadline = Instant::now() + CLIENT_TIME;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(408);
        }
        let _ = stream.set_read_timeout(Some(left));
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return Err(400),
            Ok(n) => buf.extend_from_slice(chunk.get(..n).unwrap_or_default()),
        }
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            return Ok(String::from_utf8_lossy(&buf).into_owned());
        }
        if buf.len() > MAX_REQUEST {
            return Err(431);
        }
    }
}

/// Is `head` the valid callback? The code, or the status to answer with.
fn check(head: &str, port: u16, state: &str) -> Result<String, u16> {
    let mut lines = head.split("\r\n");
    let mut request = lines.next().unwrap_or_default().split(' ');
    let (method, target) = (request.next().unwrap_or_default(), request.next().unwrap_or_default());
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != "/auth" {
        return Err(404);
    }
    if method != "GET" {
        return Err(405);
    }
    let host = lines
        .take_while(|l| !l.is_empty())
        .filter_map(|l| l.split_once(':'))
        .find(|(name, _)| name.trim().eq_ignore_ascii_case("host"))
        .map(|(_, value)| value.trim());
    if !host.is_some_and(|h| h == format!("127.0.0.1:{port}") || h == format!("localhost:{port}")) {
        return Err(400);
    }
    let param = |key: &str| query.split('&').filter_map(|p| p.split_once('=')).find(|(k, _)| *k == key).map(|(_, v)| v);
    if !param("state").is_some_and(|s| constant_time_eq(s.as_bytes(), state.as_bytes())) {
        return Err(400);
    }
    match param("code") {
        Some(code) if !code.is_empty() => Ok(code.to_string()),
        _ => Err(400),
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn respond(stream: &mut TcpStream, status: u16, page: Option<&str>) {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        431 => "Request Header Fields Too Large",
        _ => "Bad Request",
    };
    let body = page.unwrap_or_default();
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\n\
Referrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: default-src 'none'; style-src 'unsafe-inline'\r\n\
Connection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
    let _ = stream.flush();
    let _ = stream.shutdown(std::net::Shutdown::Both);
}
