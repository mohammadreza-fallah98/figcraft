use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::config::{login_page_url, login_page_url_loopback};
use crate::loopback::{Loopback, MAX_REQUEST};

const STATE: &str = "s3cr3t-state_ABCDEFGHIJKLMNOPQRSTUVWXYZ012";

fn start(lifetime: Duration) -> (Loopback, Receiver<String>) {
    let (tx, rx) = channel();
    let l = Loopback::start(STATE.into(), lifetime, move |code| {
        let _ = tx.send(code);
    })
    .unwrap();
    (l, rx)
}

fn send(port: u16, raw: &str) -> String {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let _ = s.write_all(raw.as_bytes());
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    out
}

fn get(port: u16, target: &str, host: &str) -> String {
    send(port, &format!("GET {target} HTTP/1.1\r\nHost: {host}\r\n\r\n"))
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

const LONG: Duration = Duration::from_secs(60);

#[test]
fn a_valid_callback_delivers_the_code_once_and_closes() {
    let (l, rx) = start(LONG);
    let port = l.port();
    let reply = get(port, &format!("/auth?code=abc&state={STATE}"), &format!("127.0.0.1:{port}"));
    assert!(reply.starts_with("HTTP/1.1 200 OK"), "{reply}");
    for h in [
        "Cache-Control: no-store",
        "Referrer-Policy: no-referrer",
        "X-Content-Type-Options: nosniff",
        "Content-Security-Policy: default-src 'none'; style-src 'unsafe-inline'",
    ] {
        assert!(reply.contains(h), "{h}");
    }
    assert!(reply.contains("<title>FigCraft</title>") && reply.contains("Signed in. You can close this tab and return to FigCraft."));
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), "abc");
    assert!(closed(port));
    assert!(rx.try_recv().is_err());
}

#[test]
fn localhost_is_an_accepted_host() {
    let (l, rx) = start(LONG);
    let port = l.port();
    assert!(get(port, &format!("/auth?state={STATE}&code=x"), &format!("localhost:{port}")).starts_with("HTTP/1.1 200"));
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), "x");
}

#[test]
fn wrong_state_host_path_or_method_is_refused_and_listening_continues() {
    let (l, rx) = start(LONG);
    let port = l.port();
    let host = format!("127.0.0.1:{port}");
    assert!(get(port, "/auth?code=abc&state=nope", &host).starts_with("HTTP/1.1 400"));
    assert!(get(port, "/auth?code=abc", &host).starts_with("HTTP/1.1 400"));
    assert!(get(port, &format!("/auth?state={STATE}"), &host).starts_with("HTTP/1.1 400"));
    assert!(get(port, &format!("/auth?code=abc&state={STATE}"), "evil.example").starts_with("HTTP/1.1 400"));
    assert!(get(port, &format!("/auth?code=abc&state={STATE}"), "127.0.0.1:1").starts_with("HTTP/1.1 400"));
    assert!(get(port, &format!("/auth?code=abc&state={STATE}"), "127.0.0.1").starts_with("HTTP/1.1 400"));
    assert!(get(port, &format!("/other?code=abc&state={STATE}"), &host).starts_with("HTTP/1.1 404"));
    assert!(get(port, "/favicon.ico", &host).starts_with("HTTP/1.1 404"));
    let post = send(port, &format!("POST /auth?code=abc&state={STATE} HTTP/1.1\r\nHost: {host}\r\n\r\n"));
    assert!(post.starts_with("HTTP/1.1 405"), "{post}");
    assert!(!send(port, &format!("GET /auth?code=abc&state={STATE} HTTP/1.1\r\n\r\n")).starts_with("HTTP/1.1 200"));
    assert!(rx.try_recv().is_err(), "no code was delivered");
    // A refused request body shows nothing of why.
    assert!(!get(port, "/auth?code=abc&state=nope", &host).contains("state"));
    // The right one still works.
    assert!(get(port, &format!("/auth?code=abc&state={STATE}"), &host).starts_with("HTTP/1.1 200"));
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), "abc");
}

#[test]
fn an_oversize_request_is_refused() {
    let (l, rx) = start(LONG);
    let port = l.port();
    let junk = "a".repeat(MAX_REQUEST + 2048);
    let reply = send(port, &format!("GET /auth?code=abc&state={STATE} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nX-Junk: {junk}\r\n\r\n"));
    assert!(!reply.starts_with("HTTP/1.1 200"), "{reply:.60}");
    assert!(rx.try_recv().is_err());
    // Still listening.
    assert!(get(port, &format!("/auth?code=z&state={STATE}"), &format!("127.0.0.1:{port}")).starts_with("HTTP/1.1 200"));
}

#[test]
fn a_stuck_client_does_not_block_the_next_one() {
    let (l, rx) = start(LONG);
    let port = l.port();
    let _stuck = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let reply = get(port, &format!("/auth?code=q&state={STATE}"), &format!("127.0.0.1:{port}"));
    assert!(reply.starts_with("HTTP/1.1 200"));
    assert_eq!(rx.recv_timeout(Duration::from_secs(10)).unwrap(), "q");
}

#[test]
fn stop_and_timeout_close_the_port() {
    let (l, _rx) = start(LONG);
    let port = l.port();
    assert!(TcpStream::connect(("127.0.0.1", port)).is_ok());
    l.stop();
    assert!(closed(port));
    let (l, _rx) = start(Duration::from_millis(100));
    assert!(closed(l.port()));
    let (l, _rx) = start(LONG);
    let port = l.port();
    drop(l);
    assert!(closed(port));
}

#[test]
fn two_quick_requests_deliver_one_code() {
    let (l, _) = start(LONG);
    let port = l.port();
    let count = Arc::new(Mutex::new(0));
    drop(l);
    let c = count.clone();
    let l = Loopback::start(STATE.into(), LONG, move |_| *c.lock().unwrap() += 1).unwrap();
    let port2 = l.port();
    let _ = port;
    let target = format!("/auth?code=abc&state={STATE}");
    let raw = format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1:{port2}\r\n\r\n");
    let (a, b) = (raw.clone(), raw);
    let t1 = std::thread::spawn(move || send(port2, &a));
    let t2 = std::thread::spawn(move || send(port2, &b));
    let _ = (t1.join(), t2.join());
    assert!(closed(port2));
    assert_eq!(*count.lock().unwrap(), 1);
}

#[test]
fn the_login_page_address_carries_port_and_state() {
    let url = login_page_url_loopback("https://p.example", "chal", 5123, "st");
    assert_eq!(url, "https://p.example/app-login?challenge=chal&port=5123&state=st");
    assert!(url.starts_with(&login_page_url("https://p.example", "chal")));
}
