use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use base64::Engine as _;

use crate::api::{Login, OpenedProject, PortalError, ProjectEntry};
use crate::secret::MemoryStore;
use crate::session::{Api, Portal, Status};
use crate::upload::{
    Companies, Company, MAX_NAME_CHARS, MAX_UPLOAD_BYTES, Multipart, Progress, TempFile, UploadError, UploadJob, UploadPhase, UploadRole, clean_name,
    error_from_status, parse_companies, parse_uploaded,
};

const ACCEPTED: &str = r#"{"isSuccess":true,"data":{"id":7,"name":"HSS","status":"Importing"},"errorMessages":[]}"#;

fn rejected(message: &str) -> String {
    format!(r#"{{"isSuccess":false,"data":null,"errorMessages":[{{"message":"{message}"}}]}}"#)
}

// ---- pure parts -------------------------------------------------------------------------------

#[test]
fn only_admins_and_company_admins_may_upload() {
    assert_eq!(UploadRole::from_name("Admin"), Some(UploadRole::Admin));
    assert_eq!(UploadRole::from_name(" companyadmin "), Some(UploadRole::CompanyAdmin));
    assert_eq!(UploadRole::from_name("CompanyAdmin"), Some(UploadRole::CompanyAdmin));
    assert_eq!(UploadRole::from_name("Member"), None);
    assert_eq!(UploadRole::from_name(""), None);
    assert_eq!(UploadRole::from_name("Administrator"), None);
}

#[test]
fn names_are_cleaned_and_capped() {
    assert_eq!(clean_name("  HSS \r\n Design\t"), "HSS  Design");
    assert_eq!(clean_name("\r\n"), "");
    assert_eq!(clean_name(&"x".repeat(MAX_NAME_CHARS + 20)).chars().count(), MAX_NAME_CHARS);
    assert_eq!(clean_name("طرح من"), "طرح من");
}

fn body_of(form: &Multipart, file: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    form.reader(file, file.len() as u64, Progress::default(), Arc::new(AtomicBool::new(false))).read_to_end(&mut out).unwrap();
    out
}

#[test]
fn the_multipart_body_has_the_name_the_company_and_the_file() {
    let form = Multipart::new("BOUND", "My project", Some(8));
    let file = [0x50u8, 0x4B, 3, 4, 0, 255];
    let body = body_of(&form, &file);
    assert_eq!(form.content_type(), "multipart/form-data; boundary=BOUND");
    assert_eq!(body.len() as u64, form.length(file.len() as u64));
    let text = String::from_utf8_lossy(&body).to_string();
    assert!(text.starts_with("--BOUND\r\nContent-Disposition: form-data; name=\"name\"\r\n\r\nMy project\r\n"), "{text}");
    assert!(text.contains("name=\"companyId\"\r\n\r\n8\r\n"));
    assert!(text.contains("name=\"file\"; filename=\"project.figiran\"\r\nContent-Type: application/octet-stream\r\n\r\n"));
    assert!(text.ends_with("\r\n--BOUND--\r\n"));
    let at = body.windows(4).position(|w| w == [0x50, 0x4B, 3, 4]).unwrap();
    assert_eq!(&body[at..at + 6], &file);
}

#[test]
fn without_a_company_the_form_has_no_company_field() {
    let body = body_of(&Multipart::new("B", "P", None), b"zip");
    assert!(!String::from_utf8_lossy(&body).contains("companyId"));
}

#[test]
fn a_name_with_line_breaks_cannot_add_form_parts() {
    let body = body_of(&Multipart::new("B", "x\r\n--B\r\nContent-Disposition: form-data; name=\"companyId\"\r\n\r\n1", None), b"zip");
    let text = String::from_utf8_lossy(&body).to_string();
    assert_eq!(text.matches("--B\r\n").count(), 2, "{text}");
}

#[test]
fn the_body_reader_counts_only_the_file_and_stops_at_the_declared_length() {
    let form = Multipart::new("B", "P", None);
    let progress = Progress::default();
    progress.restart(5);
    let mut out = Vec::new();
    form.reader(&b"abcdefgh"[..], 5, progress.clone(), Arc::new(AtomicBool::new(false))).read_to_end(&mut out).unwrap();
    assert_eq!((progress.sent(), progress.total()), (5, 5));
    assert!(String::from_utf8_lossy(&out).contains("\r\n\r\nabcde\r\n--B--"));
}

#[test]
fn the_body_reader_fails_once_cancelled() {
    let form = Multipart::new("B", "P", None);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut reader = form.reader(&b"abc"[..], 3, Progress::default(), cancel.clone());
    let mut buf = [0u8; 4];
    assert!(reader.read(&mut buf).is_ok());
    cancel.store(true, Ordering::Relaxed);
    assert_eq!(reader.read(&mut buf).unwrap_err().kind(), std::io::ErrorKind::Other);
}

#[test]
fn companies_are_read_from_the_page() {
    let body = r#"{"isSuccess":true,"data":{"items":[{"id":1,"name":"Acme"},{"id":0,"name":"Bad"},{"id":2,"name":"Beta"}],"totalCount":3,"page":1,"pageSize":100},"errorMessages":[]}"#;
    assert_eq!(parse_companies(200, body).unwrap(), vec![Company { id: 1, name: "Acme".into() }, Company { id: 2, name: "Beta".into() }]);
    assert_eq!(parse_companies(200, r#"{"isSuccess":true,"data":{"items":[]},"errorMessages":[]}"#).unwrap(), vec![]);
    assert!(matches!(parse_companies(403, &rejected("no")), Err(PortalError::Rejected { status: 403, .. })));
}

#[test]
fn the_accepted_answer_is_the_project() {
    let project = parse_uploaded(202, ACCEPTED).unwrap();
    assert_eq!((project.id, project.name.as_str(), project.status.as_str()), (7, "HSS", "Importing"));
}

#[test]
fn every_failure_status_has_its_own_error() {
    assert_eq!(error_from_status(401, ""), UploadError::Unauthorized);
    assert_eq!(error_from_status(403, &rejected("no")), UploadError::Forbidden);
    assert_eq!(error_from_status(400, &rejected("The project file is invalid.")), UploadError::Invalid("The project file is invalid.".into()));
    assert_eq!(error_from_status(409, &rejected("Exists.")), UploadError::Conflict("Exists.".into()));
    assert_eq!(error_from_status(413, "<html>too big</html>"), UploadError::TooLarge);
    assert_eq!(
        error_from_status(503, "<html>down</html>"),
        UploadError::Server { status: 503, message: "The portal answered with status 503.".into() }
    );
    assert_eq!(error_from_status(500, &rejected("Boom")), UploadError::Server { status: 500, message: "Boom".into() });
    assert_eq!(error_from_status(429, ""), UploadError::Server { status: 429, message: "The portal answered with status 429.".into() });
}

#[test]
fn a_success_answer_without_data_is_a_bad_response() {
    assert!(matches!(parse_uploaded(202, "not json"), Err(UploadError::BadResponse(_))));
    assert!(matches!(parse_uploaded(202, r#"{"isSuccess":true}"#), Err(UploadError::BadResponse(_))));
    assert_eq!(parse_uploaded(400, &rejected("bad")), Err(UploadError::Invalid("bad".into())));
}

// ---- the temporary file -----------------------------------------------------------------------

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("figiran-upload-test-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn files_in(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir).map(|d| d.count()).unwrap_or(0)
}

#[test]
fn the_temporary_file_is_written_and_deleted_on_drop() {
    let dir = temp_dir("drop");
    let temp = TempFile::create(&dir, b"PK\x03\x04data").unwrap();
    let path = temp.path().to_path_buf();
    assert_eq!(std::fs::read(&path).unwrap(), b"PK\x03\x04data");
    assert_eq!(path.extension().and_then(|e| e.to_str()), Some("figiran"));
    drop(temp);
    assert!(!path.exists());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_missing_temp_folder_is_a_local_error() {
    let dir = std::env::temp_dir().join("figiran-upload-test-no-such-folder-xyz");
    assert!(matches!(TempFile::create(&dir, b"x"), Err(UploadError::Local(_))));
}

// ---- the portal's upload flow ----------------------------------------------------------------

type Answer = Result<(u16, String), PortalError>;

/// What an upload carried: the token, the content type, the declared length and the body.
type Sent = (String, String, u64, Vec<u8>);

#[derive(Default)]
struct Fake {
    answers: Mutex<VecDeque<Answer>>,
    sent: Mutex<Vec<Sent>>,
    companies: Mutex<VecDeque<Result<Vec<Company>, PortalError>>>,
    /// Called inside an upload, with the body still unread.
    during: Mutex<Option<Box<dyn Fn() + Send>>>,
    refreshes: Mutex<u32>,
}

impl Api for Fake {
    fn exchange_code(&self, _: &str, _: &str) -> Result<Login, PortalError> {
        Ok(Login {
            username: "ada".into(),
            role_name: "CompanyAdmin".into(),
            token: "AT".into(),
            expire_time: 3600,
            refresh_token: "RT".into(),
            refresh_expire_time: 1000,
            ..Login::default()
        })
    }
    fn refresh(&self, _: &str) -> Result<Login, PortalError> {
        *self.refreshes.lock().unwrap() += 1;
        Ok(Login {
            username: "ada".into(),
            token: "AT2".into(),
            expire_time: 3600,
            refresh_token: "RT2".into(),
            refresh_expire_time: 1000,
            ..Login::default()
        })
    }
    fn projects(&self, _: &str) -> Result<Vec<ProjectEntry>, PortalError> {
        Ok(vec![])
    }
    fn open_project(&self, _: &str, _: i64) -> Result<OpenedProject, PortalError> {
        Err(PortalError::Network("unused".into()))
    }
    fn logout(&self, _: &str, _: &str) -> Result<(), PortalError> {
        Ok(())
    }
    fn companies(&self, _: &str) -> Result<Vec<Company>, PortalError> {
        self.companies.lock().unwrap().pop_front().unwrap_or_else(|| Err(PortalError::Network("nothing scripted".into())))
    }
    fn send_upload(&self, token: &str, content_type: &str, length: u64, body: &mut dyn Read) -> Result<(u16, String), PortalError> {
        if let Some(hook) = self.during.lock().unwrap().as_ref() {
            hook();
        }
        let mut bytes = Vec::new();
        body.read_to_end(&mut bytes).map_err(|e| PortalError::Network(e.to_string()))?;
        self.sent.lock().unwrap().push((token.into(), content_type.into(), length, bytes));
        let answer = self.answers.lock().unwrap().pop_front().unwrap_or_else(|| Err(PortalError::Network("nothing scripted".into())));
        match answer {
            Ok((401, text)) => Err(PortalError::Unauthorized(text)),
            other => other,
        }
    }
}

fn inline(job: Box<dyn FnOnce() + Send + 'static>) {
    job();
}

fn signed_in() -> (Portal, Arc<Fake>) {
    let fake = Arc::new(Fake::default());
    let portal = Portal::new("https://portal.example", fake.clone(), Box::new(MemoryStore::default()), Box::new(|| {}), inline, None);
    portal.begin_login().unwrap();
    portal.handle_auth_link("figiran://auth?code=abc");
    assert_eq!(portal.view().status, Status::SignedIn);
    (portal, fake)
}

const PAYLOAD: &[u8] = b"PK\x03\x04 a native figiran file";

fn job(dir: &std::path::Path, company: Option<i64>) -> UploadJob {
    UploadJob {
        encoded: base64::engine::general_purpose::STANDARD.encode(b"PK\x03\x04 a native figiran file"),
        name: "HSS".into(),
        company_id: company,
        temp_dir: dir.to_path_buf(),
    }
}

#[test]
fn an_accepted_upload_sends_the_file_with_the_token_and_cleans_up() {
    let dir = temp_dir("ok");
    let (portal, fake) = signed_in();
    fake.answers.lock().unwrap().push_back(Ok((202, ACCEPTED.into())));
    assert!(portal.start_upload(job(&dir, Some(3))));
    let view = portal.upload_view();
    assert_eq!(view.phase, UploadPhase::Accepted(crate::upload::UploadedProject { id: 7, name: "HSS".into(), status: "Importing".into() }));
    assert_eq!((view.sent, view.total), (PAYLOAD.len() as u64, PAYLOAD.len() as u64));
    assert!((view.fraction() - 1.0).abs() < f32::EPSILON);
    let sent = fake.sent.lock().unwrap();
    let (token, content_type, length, body) = &sent[0];
    assert_eq!(token, "AT");
    assert!(content_type.starts_with("multipart/form-data; boundary="));
    assert_eq!(*length, body.len() as u64);
    let text = String::from_utf8_lossy(body);
    assert!(text.contains("name=\"name\"\r\n\r\nHSS\r\n") && text.contains("name=\"companyId\"\r\n\r\n3\r\n"));
    assert!(body.windows(4).any(|w| w == b"PK\x03\x04"));
    assert_eq!(files_in(&dir), 0, "the temporary file is gone");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn each_refusal_ends_as_a_failure_and_leaves_no_file() {
    let cases = [
        ((400, rejected("The project file is invalid.")), UploadError::Invalid("The project file is invalid.".into())),
        ((403, rejected("no")), UploadError::Forbidden),
        ((409, rejected("dup")), UploadError::Conflict("dup".into())),
        ((413, String::new()), UploadError::TooLarge),
        ((500, rejected("Boom")), UploadError::Server { status: 500, message: "Boom".into() }),
    ];
    for (i, (answer, expected)) in cases.into_iter().enumerate() {
        let dir = temp_dir(&format!("fail{i}"));
        let (portal, fake) = signed_in();
        fake.answers.lock().unwrap().push_back(Ok(answer));
        portal.start_upload(job(&dir, None));
        assert_eq!(portal.upload_view().phase, UploadPhase::Failed(expected));
        assert_eq!(files_in(&dir), 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[test]
fn an_unreachable_portal_is_a_network_failure() {
    let dir = temp_dir("net");
    let (portal, fake) = signed_in();
    fake.answers.lock().unwrap().push_back(Err(PortalError::Network("down".into())));
    portal.start_upload(job(&dir, None));
    assert_eq!(portal.upload_view().phase, UploadPhase::Failed(UploadError::Network("down".into())));
    assert_eq!(files_in(&dir), 0);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_refused_token_is_renewed_once_and_the_upload_sent_again_from_the_start() {
    let dir = temp_dir("renew");
    let (portal, fake) = signed_in();
    fake.answers.lock().unwrap().push_back(Ok((401, "expired".into())));
    fake.answers.lock().unwrap().push_back(Ok((202, ACCEPTED.into())));
    portal.start_upload(job(&dir, None));
    assert!(matches!(portal.upload_view().phase, UploadPhase::Accepted(_)));
    let sent = fake.sent.lock().unwrap();
    assert_eq!((sent[0].0.as_str(), sent[1].0.as_str()), ("AT", "AT2"));
    assert_eq!(sent[0].3, sent[1].3, "the same body both times, read afresh from the file");
    drop(sent);
    assert_eq!(files_in(&dir), 0);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_401_that_survives_a_fresh_token_fails_the_upload_but_keeps_the_login() {
    let dir = temp_dir("auth");
    let (portal, fake) = signed_in();
    fake.answers.lock().unwrap().push_back(Ok((401, "no".into())));
    fake.answers.lock().unwrap().push_back(Ok((401, "no".into())));
    portal.start_upload(job(&dir, None));
    assert_eq!(portal.upload_view().phase, UploadPhase::Failed(UploadError::Unauthorized));
    let v = portal.view();
    assert_eq!(v.status, Status::SignedIn, "a refused upload must not sign the person out");
    assert_eq!(v.account.map(|a| a.username), Some("ada".to_string()));
    assert_eq!(*fake.refreshes.lock().unwrap(), 1, "the token was renewed once");
    assert_eq!(files_in(&dir), 0);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn cancelling_while_sending_stops_the_body_and_cleans_up() {
    let dir = temp_dir("cancel");
    let (portal, fake) = signed_in();
    let p = portal.clone();
    *fake.during.lock().unwrap() = Some(Box::new(move || p.cancel_upload()));
    fake.answers.lock().unwrap().push_back(Ok((202, ACCEPTED.into())));
    portal.start_upload(job(&dir, None));
    assert_eq!(portal.upload_view().phase, UploadPhase::Cancelled);
    assert_eq!(files_in(&dir), 0);
    portal.reset_upload();
    assert_eq!(portal.upload_view().phase, UploadPhase::Idle);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_second_upload_is_refused_while_one_runs() {
    let dir = temp_dir("twice");
    let (portal, fake) = signed_in();
    let (p, d) = (portal.clone(), dir.clone());
    let refused = Arc::new(Mutex::new(None));
    let r = refused.clone();
    *fake.during.lock().unwrap() = Some(Box::new(move || *r.lock().unwrap() = Some(p.start_upload(job(&d, None)))));
    fake.answers.lock().unwrap().push_back(Ok((202, ACCEPTED.into())));
    portal.start_upload(job(&dir, None));
    assert_eq!(*refused.lock().unwrap(), Some(false));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_document_over_the_cap_is_refused_before_anything_is_written_or_sent() {
    let dir = temp_dir("big");
    let (portal, fake) = signed_in();
    let mut big = job(&dir, None);
    big.encoded = "A".repeat(((MAX_UPLOAD_BYTES / 3 + 2) * 4) as usize);
    portal.start_upload(big);
    assert_eq!(portal.upload_view().phase, UploadPhase::Failed(UploadError::DocumentTooLarge));
    assert!(fake.sent.lock().unwrap().is_empty());
    assert_eq!(files_in(&dir), 0);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn bad_or_empty_document_data_is_a_local_failure() {
    let dir = temp_dir("bad");
    let (portal, fake) = signed_in();
    let mut broken = job(&dir, None);
    broken.encoded = "!!! not base64".into();
    portal.start_upload(broken);
    assert!(matches!(portal.upload_view().phase, UploadPhase::Failed(UploadError::Local(_))));
    let mut empty = job(&dir, None);
    empty.encoded = String::new();
    portal.start_upload(empty);
    assert!(matches!(portal.upload_view().phase, UploadPhase::Failed(UploadError::Local(_))));
    assert!(fake.sent.lock().unwrap().is_empty());
    assert_eq!(files_in(&dir), 0);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_missing_temp_folder_fails_without_sending() {
    let (portal, fake) = signed_in();
    portal.start_upload(job(&std::env::temp_dir().join("figiran-upload-test-no-such-folder-abc"), None));
    assert!(matches!(portal.upload_view().phase, UploadPhase::Failed(UploadError::Local(_))));
    assert!(fake.sent.lock().unwrap().is_empty());
}

#[test]
fn signed_out_there_is_nothing_to_send_with() {
    let dir = temp_dir("out");
    let fake = Arc::new(Fake::default());
    let portal = Portal::new("https://portal.example", fake.clone(), Box::new(MemoryStore::default()), Box::new(|| {}), inline, None);
    portal.start_upload(job(&dir, None));
    assert!(matches!(portal.upload_view().phase, UploadPhase::Failed(UploadError::Unauthorized)));
    assert_eq!(files_in(&dir), 0);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn companies_load_in_the_background() {
    let (portal, fake) = signed_in();
    assert_eq!(portal.upload_view().companies, Companies::NotLoaded);
    fake.companies.lock().unwrap().push_back(Ok(vec![Company { id: 1, name: "Acme".into() }]));
    portal.refresh_companies();
    assert_eq!(portal.upload_view().companies, Companies::Loaded(vec![Company { id: 1, name: "Acme".into() }]));
    fake.companies.lock().unwrap().push_back(Err(PortalError::Rejected { status: 403, message: "no".into() }));
    portal.refresh_companies();
    assert_eq!(portal.upload_view().companies, Companies::Failed("no".into()));
}

#[test]
fn the_progress_fraction_is_zero_without_a_total() {
    let (portal, _) = signed_in();
    assert!(portal.upload_view().fraction().abs() < f32::EPSILON);
}

// ---- the real HTTP client, against a local socket --------------------------------------------

/// Read one request: its head, and `Content-Length` bytes of body.
fn read_request(stream: &mut std::net::TcpStream) -> (String, Vec<u8>) {
    let mut data = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        let n = stream.read(&mut chunk).unwrap();
        assert!(n > 0, "closed before the head ended");
        data.extend_from_slice(&chunk[..n]);
        if let Some(i) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
    };
    let head = String::from_utf8_lossy(&data[..head_end]).to_string();
    let length: usize = head
        .lines()
        .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().to_string()))
        .map_or(0, |v| v.parse().unwrap());
    while data.len() < head_end + length {
        let n = stream.read(&mut chunk).unwrap();
        assert!(n > 0, "closed before the body ended");
        data.extend_from_slice(&chunk[..n]);
    }
    (head, data[head_end..head_end + length].to_vec())
}

#[test]
fn the_http_client_posts_the_multipart_body_with_a_content_length_and_the_bearer_token() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let (head, body) = read_request(&mut stream);
        let reply = format!(
            "HTTP/1.1 202 Accepted\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{ACCEPTED}",
            ACCEPTED.len()
        );
        stream.write_all(reply.as_bytes()).unwrap();
        (head, body)
    });
    let client = crate::client::Client::new(&format!("http://127.0.0.1:{port}"));
    let form = Multipart::new("BOUNDARY123", "HSS", Some(4));
    let file = b"PK\x03\x04 native";
    let mut body = form.reader(&file[..], file.len() as u64, Progress::default(), Arc::new(AtomicBool::new(false)));
    let (status, text) = client.send_upload("SECRET-TOKEN", form.content_type(), form.length(file.len() as u64), &mut body).unwrap();
    assert_eq!(status, 202);
    assert_eq!(parse_uploaded(status, &text).unwrap().id, 7);
    let (head, received) = server.join().unwrap();
    let lower = head.to_ascii_lowercase();
    assert!(head.starts_with("POST /api/v1/projects/upload HTTP/1.1"), "{head}");
    assert!(lower.contains("authorization: bearer secret-token"));
    assert!(lower.contains("content-type: multipart/form-data; boundary=boundary123"));
    assert!(!lower.contains("transfer-encoding"), "a length, not chunked: {head}");
    let text = String::from_utf8_lossy(&received);
    assert!(text.contains("name=\"companyId\"\r\n\r\n4\r\n") && text.contains("PK\u{3}\u{4} native"));
}

#[test]
fn the_http_client_reports_a_refusal_status_and_a_401() {
    for (code, reply) in [(409u16, "Conflict"), (401, "Unauthorized")] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let body = rejected("nope");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            read_request(&mut stream);
            let r = format!(
                "HTTP/1.1 {code} {reply}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(r.as_bytes()).unwrap();
        });
        let client = crate::client::Client::new(&format!("http://127.0.0.1:{port}"));
        let form = Multipart::new("B", "P", None);
        let mut body = form.reader(&b"zip"[..], 3, Progress::default(), Arc::new(AtomicBool::new(false)));
        let result = client.send_upload("T", form.content_type(), form.length(3), &mut body);
        server.join().unwrap();
        if code == 401 {
            assert!(matches!(result, Err(PortalError::Unauthorized(_))));
        } else {
            assert_eq!(result.unwrap().0, 409);
        }
    }
}
