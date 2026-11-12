//! File ▸ Publish to server… with a portal: only Admin and CompanyAdmin get the upload dialog (a Member gets a
//! message, a signed-out person the portal login and nothing is sent), the dialog asks
//! for the name (and the company, for an Admin), the document goes to the portal as Save as writes
//! it without changing the document, and every failure has words.

use std::sync::{Arc, Mutex};

use egui::Pos2;
use figiran_portal::api::{Login, OpenedProject, PortalError, ProjectEntry};
use figiran_portal::secret::MemoryStore;
use figiran_portal::session::{Account, Api, Portal, Projects, Status, View};
use figiran_portal::upload::Company;
use figiran_portal::{UploadError, UploadPhase, UploadRole};

use crate::live_tests::{Harness, has_text, text_center, texts};
use crate::portal_upload::{Problem, error_text, problem_with, role_of};

const BASE: &str = "https://portal.example";
const ACCEPTED: &str = r#"{"isSuccess":true,"data":{"id":7,"name":"Untitled","status":"Importing"},"errorMessages":[]}"#;

/// What the portal was sent: the token and the body.
type Uploads = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

/// A portal that signs anyone in with `role`, lists two companies and records what is uploaded.
struct Scripted {
    role: &'static str,
    uploads: Uploads,
}

impl Api for Scripted {
    fn exchange_code(&self, _: &str, _: &str) -> Result<Login, PortalError> {
        Ok(Login {
            username: "ada".into(),
            first_name: Some("Ada".into()),
            last_name: Some("Lovelace".into()),
            company_name: "Acme".into(),
            role_name: self.role.into(),
            token: "AT".into(),
            expire_time: 3600,
            refresh_token: "RT".into(),
            refresh_expire_time: 100_000,
        })
    }
    fn refresh(&self, _: &str) -> Result<Login, PortalError> {
        Err(PortalError::Network("not in this test".into()))
    }
    fn projects(&self, _: &str) -> Result<Vec<ProjectEntry>, PortalError> {
        Ok(vec![])
    }
    fn open_project(&self, _: &str, _: i64) -> Result<OpenedProject, PortalError> {
        Err(PortalError::Network("not in this test".into()))
    }
    fn logout(&self, _: &str, _: &str) -> Result<(), PortalError> {
        Ok(())
    }
    fn companies(&self, _: &str) -> Result<Vec<Company>, PortalError> {
        Ok(vec![Company { id: 1, name: "Acme".into() }, Company { id: 2, name: "Beta".into() }])
    }
    fn send_upload(&self, token: &str, _: &str, _: u64, body: &mut dyn std::io::Read) -> Result<(u16, String), PortalError> {
        let mut bytes = Vec::new();
        body.read_to_end(&mut bytes).map_err(|e| PortalError::Network(e.to_string()))?;
        self.uploads.lock().unwrap().push((token.into(), bytes));
        Ok((202, ACCEPTED.into()))
    }
}

fn inline(job: Box<dyn FnOnce() + Send + 'static>) {
    job();
}

/// A harness signed in with `role`.
fn signed_in(role: &'static str) -> (Harness, Portal, Uploads) {
    let (mut h, _) = Harness::new();
    let uploads = Arc::new(Mutex::new(Vec::new()));
    let portal =
        Portal::new(BASE, Arc::new(Scripted { role, uploads: uploads.clone() }), Box::new(MemoryStore::default()), Box::new(|| {}), inline, None);
    h.app.ui.portal.install(Ok(portal.clone()));
    portal.begin_login().unwrap();
    portal.handle_auth_link("figiran://auth?code=abc");
    assert_eq!(portal.view().status, Status::SignedIn);
    (h, portal, uploads)
}

fn open_profile_menu(h: &mut Harness) -> Vec<egui::Shape> {
    let shapes = h.frames(3);
    let (_, p, size) = texts(&shapes).into_iter().rev().find(|(t, _, _)| t == "AL").expect("the avatar");
    let avatar = p + size / 2.0;
    h.click(Pos2::new(avatar.x + 24.0, avatar.y));
    h.frames(3)
}

fn view(status: Status, role: &str) -> View {
    View {
        status,
        account: Some(Account { display_name: "Ada".into(), username: "ada".into(), company: "Acme".into(), role: role.into() }),
        projects: Projects::NotLoaded,
        opening: None,
        notice: None,
        remembered: true,
        busy: false,
    }
}

#[test]
fn only_admins_and_company_admins_are_offered_the_entry() {
    assert_eq!(role_of(&view(Status::SignedIn, "Admin")), Some(UploadRole::Admin));
    assert_eq!(role_of(&view(Status::SignedIn, "CompanyAdmin")), Some(UploadRole::CompanyAdmin));
    assert_eq!(role_of(&view(Status::SignedIn, "Member")), None);
    assert_eq!(role_of(&view(Status::SignedIn, "")), None);
    assert_eq!(role_of(&view(Status::SignedOut, "Admin")), None, "signed out, no role counts");
    let no_account = View { account: None, ..view(Status::SignedIn, "Admin") };
    assert_eq!(role_of(&no_account), None);
}

#[test]
fn the_form_needs_a_name_and_an_admin_needs_a_company() {
    assert_eq!(problem_with(UploadRole::Admin, "  ", Some(1)), Some(Problem::NoName));
    assert_eq!(problem_with(UploadRole::Admin, "HSS", None), Some(Problem::NoCompany));
    assert_eq!(problem_with(UploadRole::Admin, "HSS", Some(1)), None);
    assert_eq!(problem_with(UploadRole::CompanyAdmin, "HSS", None), None, "a CompanyAdmin's company is their own");
}

#[test]
fn every_failure_has_words_with_the_reason_in_them() {
    assert!(error_text(&UploadError::Unauthorized).contains("still logged in"));
    assert!(error_text(&UploadError::Forbidden).contains("isn't allowed"));
    assert!(error_text(&UploadError::Invalid("bad zip".into())).contains("bad zip"));
    assert!(error_text(&UploadError::Conflict("exists".into())).contains("exists"));
    assert!(error_text(&UploadError::TooLarge).contains("200 MB"));
    assert!(error_text(&UploadError::DocumentTooLarge).contains("200 MB"));
    let server = error_text(&UploadError::Server { status: 503, message: "down".into() });
    assert!(server.contains("503") && server.contains("down"), "{server}");
    assert!(error_text(&UploadError::Network("timeout".into())).contains("timeout"));
    assert!(error_text(&UploadError::Local("disk full".into())).contains("disk full"));
    assert!(error_text(&UploadError::Cancelled).contains("cancelled"));
    assert!(!error_text(&UploadError::BadResponse("secret detail".into())).contains("secret detail"));
}

fn publish(h: &mut Harness) {
    crate::menus::invoke(&mut h.app, "collab.publish");
}

fn toast_of(h: &Harness) -> String {
    h.app.ui.toast.as_ref().map(|(t, _)| t.clone()).unwrap_or_default()
}

#[test]
fn the_profile_menu_has_no_upload_row_any_more() {
    for role in ["Member", "Admin", "CompanyAdmin"] {
        let (mut h, _, _) = signed_in(role);
        let shapes = open_profile_menu(&mut h);
        for row in ["Projects", "Refresh", "Open portal", "Profile and access", "Log out"] {
            assert!(has_text(&shapes, row), "{role}: {row}");
        }
        assert!(!has_text(&shapes, "Upload current file to portal…"), "{role}");
    }
}

#[test]
fn a_member_who_publishes_gets_a_message_and_no_dialog() {
    let (mut h, portal, uploads) = signed_in("Member");
    publish(&mut h);
    let shapes = h.frames(3);
    assert!(toast_of(&h).contains("Only a company admin can publish"), "{}", toast_of(&h));
    assert!(!has_text(&shapes, "Upload to portal") && !has_text(&shapes, "Publish to server"));
    assert!(!h.app.ui.portal.upload.is_open());
    assert_eq!(portal.upload_view().phase, UploadPhase::Idle);
    assert!(uploads.lock().unwrap().is_empty());
}

#[test]
fn signed_out_publish_opens_the_login_and_sends_nothing() {
    let (mut h, _) = Harness::new();
    let uploads = Arc::new(Mutex::new(Vec::new()));
    let portal = Portal::new(
        BASE,
        Arc::new(Scripted { role: "Admin", uploads: uploads.clone() }),
        Box::new(MemoryStore::default()),
        Box::new(|| {}),
        inline,
        None,
    );
    h.app.ui.portal.install(Ok(portal.clone()));
    publish(&mut h);
    let shapes = h.frames(3);
    assert_eq!(portal.view().status, Status::WaitingForBrowser, "the portal login was started");
    assert!(toast_of(&h).contains("Log in to the portal"), "{}", toast_of(&h));
    assert!(!has_text(&shapes, "Upload to portal"), "no upload dialog");
    assert!(!has_text(&shapes, "Server address") && !has_text(&shapes, "Publish to server"), "no address and key dialog");
    assert!(h.app.ui.live.publish.is_none());
    assert!(uploads.lock().unwrap().is_empty());
}

#[test]
fn without_a_portal_the_server_dialog_stays() {
    let (mut h, _) = Harness::new();
    publish(&mut h);
    assert!(h.app.ui.live.publish.is_some(), "upstream behaviour when no portal is configured");
}

#[test]
fn an_admin_and_a_company_admin_get_the_upload_dialog_and_never_the_address_dialog() {
    for role in ["Admin", "CompanyAdmin"] {
        let (mut h, _, _) = signed_in(role);
        publish(&mut h);
        let shapes = h.frames(3);
        assert!(has_text(&shapes, "Upload to portal"), "{role}");
        assert!(h.app.ui.live.publish.is_none(), "{role}: no address and key dialog");
    }
}

#[test]
fn a_company_admin_uploads_the_document_as_save_as_writes_it_and_the_document_stays_as_it_was() {
    let (mut h, portal, uploads) = signed_in("CompanyAdmin");
    let (path_before, dirty_before) = (h.app.session.docs[0].path.clone(), h.app.session.docs[0].is_dirty());
    publish(&mut h);
    let shapes = h.frames(3);
    for t in ["Upload to portal", "Project name", "Company: Acme", "Upload", "Close"] {
        assert!(has_text(&shapes, t), "{t}");
    }
    assert!(!has_text(&shapes, "Choose a company."));
    h.click(text_center(&shapes, "Upload"));
    h.frames(2);
    let view = portal.upload_view();
    assert!(matches!(view.phase, UploadPhase::Accepted(_)), "{:?}", view.phase);
    let sent = uploads.lock().unwrap();
    assert_eq!(sent.len(), 1);
    let (token, body) = &sent[0];
    assert_eq!(token, "AT");
    let text = String::from_utf8_lossy(body);
    assert!(text.contains("name=\"name\"\r\n\r\nUntitled\r\n"), "the project is named after the document");
    assert!(!text.contains("companyId"), "a CompanyAdmin never names a company");
    assert!(body.windows(4).any(|w| w == b"PK\x03\x04"), "a native .figiran archive");
    drop(sent);
    assert_eq!((h.app.session.docs[0].path.clone(), h.app.session.docs[0].is_dirty()), (path_before, dirty_before), "nothing was saved or marked");
    let shapes = h.frames(2);
    assert!(has_text(&shapes, "The project was uploaded and is being imported. It appears in your project list when it is ready."));
    h.click(text_center(&shapes, "Close"));
    assert!(!has_text(&h.frames(2), "Upload to portal"));
    assert_eq!(portal.upload_view().phase, UploadPhase::Idle);
}

#[test]
fn an_admin_picks_the_company_and_it_is_sent() {
    let (mut h, portal, uploads) = signed_in("Admin");
    publish(&mut h);
    let shapes = h.frames(3);
    assert!(has_text(&shapes, "Beta"), "the companies are listed");
    h.click(text_center(&shapes, "Upload"));
    let shapes = h.frames(2);
    assert!(has_text(&shapes, "Choose a company."), "an Admin must choose");
    assert!(uploads.lock().unwrap().is_empty());
    h.click(text_center(&shapes, "Beta"));
    let shapes = h.frames(2);
    h.click(text_center(&shapes, "Upload"));
    h.frames(2);
    assert!(matches!(portal.upload_view().phase, UploadPhase::Accepted(_)));
    let sent = uploads.lock().unwrap();
    assert!(String::from_utf8_lossy(&sent[0].1).contains("name=\"companyId\"\r\n\r\n2\r\n"));
}

#[test]
fn logging_out_closes_the_dialog() {
    let (mut h, portal, _) = signed_in("CompanyAdmin");
    publish(&mut h);
    assert!(has_text(&h.frames(3), "Upload to portal"));
    portal.logout();
    let shapes = h.frames(3);
    assert!(!has_text(&shapes, "Upload to portal"));
    assert!(!h.app.ui.portal.upload.is_open());
}
