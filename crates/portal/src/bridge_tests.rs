use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::bridge::{Bridge, MAX_BODY, origin_of};

const ORIGIN: &str = "http://192.168.10.5:4000";
const LINK: &str = "figcraft://join?host=127.0.0.1:47900&file=f&token=AbCdEfGhIjKlMnOpQrStUv";

fn start(port: u16) -> (Bridge, Receiver<String>) {
    let (tx, rx) = channel();
    let tx = Mutex::new(tx);
    let sink: crate::bridge::Sink = Arc::new(move |l| {
        let _ = tx.lock().unwrap().send(l);
    });
    (Bridge::start_on(port..=port, ORIGIN, sink).unwrap(), rx)
}

fn send(port: u16, raw: &str) -> String {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let _ = s.write_all(raw.as_bytes());
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    out
}

fn post(port: u16, origin: Option<&str>, host: &str, ctype: &str, body: &str) -> String {
    let origin = origin.map(|o| format!("Origin: {o}\r\n")).unwrap_or_default();
    send(port, &format!("POST /open HTTP/1.1\r\nHost: {host}\r\n{origin}Content-Type: {ctype}\r\nContent-Length: {}\r\n\r\n{body}", body.len()))
}

fn good(port: u16, body: &str) -> String {
    post(port, Some(ORIGIN), &format!("127.0.0.1:{port}"), "application/json", body)
}

fn closed(port: u16) -> bool {
    for _ in 0..250 {
        if TcpStream::connect(("127.0.0.1", port)).is_err() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

#[test]
fn origin_is_scheme_host_and_non_default_port() {
    assert_eq!(origin_of("http://192.168.10.5:4000/app?x=1").as_deref(), Some("http://192.168.10.5:4000"));
    assert_eq!(origin_of("https://Portal.Example:443/").as_deref(), Some("https://portal.example"));
    assert_eq!(origin_of("ftp://x"), None);
    assert_eq!(origin_of("http://u@x"), None);
}

#[test]
fn preflight_allows_only_the_portal_origin() {
    let (b, _rx) = start(48101);
    let p = b.port();
    let ok = send(
        p,
        &format!(
            "OPTIONS /open HTTP/1.1\r\nHost: 127.0.0.1:{p}\r\nOrigin: {ORIGIN}\r\nAccess-Control-Request-Method: POST\r\nAccess-Control-Request-Private-Network: true\r\n\r\n"
        ),
    );
    assert!(ok.starts_with("HTTP/1.1 204"), "{ok}");
    for h in [
        format!("Access-Control-Allow-Origin: {ORIGIN}"),
        "Access-Control-Allow-Methods: GET, POST, OPTIONS".into(),
        "Access-Control-Allow-Headers: content-type".into(),
        "Access-Control-Allow-Private-Network: true".into(),
        "Vary: Origin".into(),
        "Access-Control-Max-Age: 60".into(),
    ] {
        assert!(ok.contains(&h), "{h}");
    }
    for origin in ["http://evil.example", "null", "https://192.168.10.5:4000"] {
        let denied = send(p, &format!("OPTIONS /open HTTP/1.1\r\nHost: 127.0.0.1:{p}\r\nOrigin: {origin}\r\n\r\n"));
        assert!(denied.starts_with("HTTP/1.1 403") && !denied.contains("Access-Control-Allow"), "{denied}");
    }
    let none = send(p, &format!("OPTIONS /open HTTP/1.1\r\nHost: 127.0.0.1:{p}\r\n\r\n"));
    assert!(none.starts_with("HTTP/1.1 403"), "{none}");
}

#[test]
fn ping_answers_the_app_and_version() {
    let (b, _rx) = start(48102);
    let p = b.port();
    for host in [format!("127.0.0.1:{p}"), format!("localhost:{p}")] {
        let r = send(p, &format!("GET /ping HTTP/1.1\r\nHost: {host}\r\nOrigin: {ORIGIN}\r\n\r\n"));
        assert!(r.starts_with("HTTP/1.1 200 OK") && r.ends_with("{\"app\":\"figcraft\",\"v\":1}"), "{r}");
        assert!(r.contains(&format!("Access-Control-Allow-Origin: {ORIGIN}")));
    }
}

#[test]
fn open_hands_the_link_over_once() {
    let (b, rx) = start(48103);
    let r = good(b.port(), &format!("{{\"url\":\"{LINK}\"}}"));
    assert!(r.starts_with("HTTP/1.1 202") && r.ends_with("{\"ok\":true}"), "{r}");
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), LINK);
    assert!(rx.try_recv().is_err());
}

#[test]
fn open_accepts_a_charset_in_the_content_type() {
    let (b, rx) = start(48104);
    let p = b.port();
    let body = format!("{{\"url\":\"{LINK}\"}}");
    let r = post(p, Some(ORIGIN), &format!("localhost:{p}"), "Application/JSON; charset=utf-8", &body);
    assert!(r.starts_with("HTTP/1.1 202"), "{r}");
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), LINK);
}

#[test]
fn open_rejects_what_is_not_exactly_right() {
    let (b, rx) = start(48105);
    let p = b.port();
    let body = format!("{{\"url\":\"{LINK}\"}}");
    let host = format!("127.0.0.1:{p}");
    assert!(post(p, Some("http://evil.example"), &host, "application/json", &body).starts_with("HTTP/1.1 403"));
    assert!(post(p, None, &host, "application/json", &body).starts_with("HTTP/1.1 403"));
    assert!(post(p, Some(ORIGIN), "evil.example", "application/json", &body).starts_with("HTTP/1.1 403"));
    assert!(post(p, Some(ORIGIN), &format!("127.0.0.1:{}", p + 1), "application/json", &body).starts_with("HTTP/1.1 403"));
    assert!(post(p, Some(ORIGIN), &host, "text/plain", &body).starts_with("HTTP/1.1 400"));
    assert!(post(p, Some(ORIGIN), &host, "application/json", "not json").starts_with("HTTP/1.1 400"));
    assert!(good(p, "{\"url\":\"figcraft://auth?code=abc\"}").starts_with("HTTP/1.1 400"));
    assert!(good(p, "{\"url\":\"http://evil.example\"}").starts_with("HTTP/1.1 400"));
    assert!(good(p, "{\"url\":\"figcraft://join?\"}").starts_with("HTTP/1.1 400"));
    assert!(good(p, "{\"url\":\"figcraft://join?a=b c\"}").starts_with("HTTP/1.1 400"));
    assert!(good(p, &format!("{{\"url\":\"{LINK}\",\"extra\":1}}")).starts_with("HTTP/1.1 400"));
    assert!(good(p, "{}").starts_with("HTTP/1.1 400"));
    let big = format!("{{\"url\":\"figcraft://join?a={}\"}}", "x".repeat(MAX_BODY));
    assert!(good(p, &big).starts_with("HTTP/1.1 400"));
    let no_length = send(p, &format!("POST /open HTTP/1.1\r\nHost: {host}\r\nOrigin: {ORIGIN}\r\nContent-Type: application/json\r\n\r\n"));
    assert!(no_length.starts_with("HTTP/1.1 400"), "{no_length}");
    assert!(send(p, &format!("GET /nope HTTP/1.1\r\nHost: {host}\r\nOrigin: {ORIGIN}\r\n\r\n")).starts_with("HTTP/1.1 404"));
    assert!(rx.try_recv().is_err(), "nothing was handed over");
}

#[test]
fn a_silent_client_does_not_block_others() {
    let (b, _rx) = start(48106);
    let p = b.port();
    let _idle = TcpStream::connect(("127.0.0.1", p)).unwrap();
    let r = send(p, &format!("GET /ping HTTP/1.1\r\nHost: 127.0.0.1:{p}\r\nOrigin: {ORIGIN}\r\n\r\n"));
    assert!(r.starts_with("HTTP/1.1 200"), "{r}");
}

#[test]
fn the_next_port_is_taken_when_the_first_is_busy() {
    let busy = TcpListener::bind((Ipv4Addr::LOCALHOST, 48111)).unwrap();
    let sink: crate::bridge::Sink = Arc::new(|_| {});
    let b = Bridge::start_on(48111..=48112, ORIGIN, sink.clone()).unwrap();
    assert_eq!(b.port(), 48112);
    let none = Bridge::start_on(48111..=48111, ORIGIN, sink);
    assert!(none.is_none(), "every port taken: off");
    drop(busy);
}

#[test]
fn stopping_closes_the_port() {
    let (b, _rx) = start(48107);
    let p = b.port();
    drop(b);
    assert!(closed(p));
}

#[test]
fn an_address_without_an_origin_starts_nothing() {
    let sink: crate::bridge::Sink = Arc::new(|_| {});
    assert!(Bridge::start_on(48108..=48108, "not a url", sink).is_none());
}
