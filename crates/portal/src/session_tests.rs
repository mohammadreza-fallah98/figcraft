use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::api::{Access, Login, OpenedProject, PortalError, ProjectEntry};
use crate::pkce;
use crate::secret::{MemoryStore, SecretStore};
use crate::session::{Api, LOGIN_WAIT, Portal, Projects, Status};

type Script<T> = Mutex<VecDeque<Result<T, PortalError>>>;

#[derive(Default)]
struct Fake {
    exchange: Script<Login>,
    refresh: Script<Login>,
    projects: Script<Vec<ProjectEntry>>,
    open: Script<OpenedProject>,
    logout: Script<()>,
    calls: Mutex<Vec<String>>,
}

impl Fake {
    fn note(&self, call: String) {
        self.calls.lock().unwrap().push(call);
    }
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
    fn next<T>(script: &Script<T>) -> Result<T, PortalError> {
        script.lock().unwrap().pop_front().unwrap_or_else(|| Err(PortalError::Network("nothing scripted".into())))
    }
}

impl Api for Fake {
    fn exchange_code(&self, code: &str, verifier: &str) -> Result<Login, PortalError> {
        self.note(format!("exchange {code} {}", pkce::challenge(verifier)));
        Self::next(&self.exchange)
    }
    fn refresh(&self, refresh_token: &str) -> Result<Login, PortalError> {
        self.note(format!("refresh {refresh_token}"));
        Self::next(&self.refresh)
    }
    fn projects(&self, access_token: &str) -> Result<Vec<ProjectEntry>, PortalError> {
        self.note(format!("projects {access_token}"));
        Self::next(&self.projects)
    }
    fn open_project(&self, access_token: &str, project_id: i64) -> Result<OpenedProject, PortalError> {
        self.note(format!("open {access_token} {project_id}"));
        Self::next(&self.open)
    }
    fn logout(&self, access_token: &str, refresh_token: &str) -> Result<(), PortalError> {
        self.note(format!("logout {access_token} {refresh_token}"));
        Self::next(&self.logout)
    }
}

struct Shared(Arc<MemoryStore>);

impl SecretStore for Shared {
    fn load(&self) -> Result<Option<String>, String> {
        self.0.load()
    }
    fn save(&self, secret: &str) -> Result<(), String> {
        self.0.save(secret)
    }
    fn clear(&self) -> Result<(), String> {
        self.0.clear()
    }
    fn persistent(&self) -> bool {
        true
    }
}

fn inline(job: Box<dyn FnOnce() + Send + 'static>) {
    job();
}

const BASE: &str = "https://portal.example";

fn setup() -> (Portal, Arc<Fake>, Arc<MemoryStore>) {
    let fake = Arc::new(Fake::default());
    let store = Arc::new(MemoryStore::default());
    let portal = Portal::new(BASE, fake.clone(), Box::new(Shared(store.clone())), Box::new(|| {}), inline, None);
    (portal, fake, store)
}

fn login(access: &str, refresh: &str, expires: u64) -> Login {
    Login {
        username: "ada".into(),
        first_name: Some("Ada".into()),
        last_name: Some("Lovelace".into()),
        company_name: "Acme".into(),
        role_name: "Member".into(),
        token: access.into(),
        expire_time: expires,
        refresh_token: refresh.into(),
        refresh_expire_time: 100_000,
    }
}

fn project(id: i64) -> ProjectEntry {
    ProjectEntry { id, name: format!("P{id}"), company_name: "Acme".into(), access: Access::Edit }
}

fn push<T>(script: &Script<T>, r: Result<T, PortalError>) {
    script.lock().unwrap().push_back(r);
}

fn challenge_of(url: &str) -> String {
    url.strip_prefix(&format!("{BASE}/app-login?challenge=")).unwrap().to_string()
}

fn unauthorized() -> PortalError {
    PortalError::Unauthorized("no".into())
}

#[test]
fn begin_login_opens_the_login_page_with_the_challenge() {
    let (p, ..) = setup();
    let url = p.begin_login().unwrap();
    assert_eq!(challenge_of(&url).len(), 43);
    assert_eq!(p.view().status, Status::WaitingForBrowser);
}

#[test]
fn the_callback_trades_the_code_and_verifier_for_a_session() {
    let (p, fake, store) = setup();
    push(&fake.exchange, Ok(login("AT", "RT", 3600)));
    push(&fake.projects, Ok(vec![project(1), project(2)]));
    let challenge = challenge_of(&p.begin_login().unwrap());
    assert!(p.handle_auth_link("figcraft://auth?code=abc123"));
    let v = p.view();
    assert_eq!(v.status, Status::SignedIn);
    assert_eq!(v.account.unwrap().display_name, "Ada Lovelace");
    assert_eq!(v.projects, Projects::Loaded(vec![project(1), project(2)]));
    // The portal got the code, and a verifier whose hash is the challenge the browser was given.
    assert_eq!(fake.calls(), vec![format!("exchange abc123 {challenge}"), "projects AT".to_string()]);
    assert_eq!(store.load().unwrap().as_deref(), Some("RT"));
    assert_eq!(p.take_display_name().as_deref(), Some("Ada Lovelace"));
    assert_eq!(p.take_display_name(), None);
}

#[test]
fn a_callback_is_used_once() {
    let (p, fake, _) = setup();
    push(&fake.exchange, Ok(login("AT", "RT", 3600)));
    push(&fake.projects, Ok(vec![]));
    p.begin_login().unwrap();
    assert!(p.handle_auth_link("figcraft://auth?code=abc"));
    assert!(p.handle_auth_link("figcraft://auth?code=abc"));
    assert_eq!(fake.calls().iter().filter(|c| c.starts_with("exchange")).count(), 1);
    assert_eq!(p.view().status, Status::SignedIn);
    assert!(p.view().notice.is_some_and(|n| n.contains("No login is waiting")));
}

#[test]
fn a_callback_nobody_asked_for_is_refused() {
    let (p, fake, _) = setup();
    assert!(p.handle_auth_link("figcraft://auth?code=abc"));
    assert!(matches!(p.view().status, Status::Failed(m) if m.contains("No login is waiting")));
    assert!(fake.calls().is_empty());
}

#[test]
fn other_links_are_not_taken() {
    let (p, ..) = setup();
    p.begin_login().unwrap();
    assert!(!p.handle_auth_link("figcraft://join?host=1.2.3.4:5&file=f&token=t"));
    assert_eq!(p.view().status, Status::WaitingForBrowser);
}

#[test]
fn a_bad_code_ends_the_pending_login() {
    let (p, fake, _) = setup();
    p.begin_login().unwrap();
    assert!(p.handle_auth_link("figcraft://auth?code=a%20b"));
    assert!(matches!(p.view().status, Status::Failed(m) if m.contains("isn't valid")));
    assert!(fake.calls().is_empty());
}

#[test]
fn an_old_pending_login_is_refused() {
    let (p, fake, _) = setup();
    p.begin_login().unwrap();
    p.backdate_pending(LOGIN_WAIT + Duration::from_secs(1));
    assert!(p.handle_auth_link("figcraft://auth?code=abc"));
    assert!(matches!(p.view().status, Status::Failed(m) if m.contains("took too long")));
    assert!(fake.calls().is_empty());
}

#[test]
fn a_refused_code_is_a_failed_login_with_nothing_kept() {
    let (p, fake, store) = setup();
    push(&fake.exchange, Err(PortalError::Unauthorized("Code is invalid.".into())));
    p.begin_login().unwrap();
    p.handle_auth_link("figcraft://auth?code=abc");
    assert_eq!(p.view().status, Status::Failed("Code is invalid.".into()));
    assert_eq!(store.load().unwrap(), None);
    assert_eq!(p.view().account, None);
}

#[test]
fn an_expiring_access_token_is_renewed_and_each_new_refresh_token_replaces_the_old() {
    let (p, fake, store) = setup();
    push(&fake.exchange, Ok(login("AT1", "RT1", 0)));
    push(&fake.projects, Ok(vec![]));
    push(&fake.refresh, Ok(login("AT2", "RT2", 0)));
    push(&fake.refresh, Ok(login("AT3", "RT3", 3600)));
    push(&fake.open, Ok(OpenedProject { project_id: 4, project_name: "P4".into(), access: Access::View, join_url: "figcraft://join?x=1".into() }));
    p.begin_login().unwrap();
    p.handle_auth_link("figcraft://auth?code=abc");
    p.open_project(4);
    let calls = fake.calls();
    assert_eq!(&calls[1..], ["refresh RT1", "projects AT2", "refresh RT2", "open AT3 4"]);
    assert_eq!(store.load().unwrap().as_deref(), Some("RT3"));
    let opened = p.take_opened().unwrap();
    assert_eq!((opened.project_id, opened.join_url.as_str()), (4, "figcraft://join?x=1"));
    assert_eq!(p.take_opened(), None);
}

#[test]
fn a_token_the_portal_refuses_is_renewed_once_and_the_call_retried() {
    let (p, fake, _) = setup();
    push(&fake.exchange, Ok(login("AT1", "RT1", 3600)));
    push(&fake.projects, Err(unauthorized()));
    push(&fake.refresh, Ok(login("AT2", "RT2", 3600)));
    push(&fake.projects, Ok(vec![project(9)]));
    p.begin_login().unwrap();
    p.handle_auth_link("figcraft://auth?code=abc");
    assert_eq!(fake.calls()[1..], ["projects AT1", "refresh RT1", "projects AT2"]);
    assert_eq!(p.view().projects, Projects::Loaded(vec![project(9)]));
}

#[test]
fn a_session_the_portal_no_longer_knows_ends_the_login() {
    let (p, fake, store) = setup();
    push(&fake.exchange, Ok(login("AT1", "RT1", 3600)));
    push(&fake.projects, Err(unauthorized()));
    push(&fake.refresh, Err(unauthorized()));
    p.begin_login().unwrap();
    p.handle_auth_link("figcraft://auth?code=abc");
    assert!(matches!(p.view().status, Status::Failed(_)));
    assert_eq!(p.view().account, None);
    assert_eq!(store.load().unwrap(), None);
}

#[test]
fn a_project_that_wont_open_is_a_notice_not_a_logout() {
    let (p, fake, _) = setup();
    push(&fake.exchange, Ok(login("AT", "RT", 3600)));
    push(&fake.projects, Ok(vec![]));
    push(&fake.open, Err(PortalError::Rejected { status: 404, message: "No access".into() }));
    p.begin_login().unwrap();
    p.handle_auth_link("figcraft://auth?code=abc");
    p.open_project(3);
    let v = p.view();
    assert_eq!((v.status, v.notice, v.opening), (Status::SignedIn, Some("No access".into()), None));
    assert_eq!(p.take_opened(), None);
    p.dismiss_notice();
    assert_eq!(p.view().notice, None);
}

#[test]
fn logout_forgets_everything() {
    let (p, fake, store) = setup();
    push(&fake.exchange, Ok(login("AT", "RT", 3600)));
    push(&fake.projects, Ok(vec![project(1)]));
    p.begin_login().unwrap();
    p.handle_auth_link("figcraft://auth?code=abc");
    p.logout();
    let v = p.view();
    assert_eq!((v.status, v.account, v.projects), (Status::SignedOut, None, Projects::NotLoaded));
    assert_eq!(store.load().unwrap(), None);
    p.open_project(1);
    assert_eq!(p.take_opened(), None);
}

fn sign_in_for_logout() -> (Portal, Arc<Fake>, Arc<MemoryStore>) {
    let (p, fake, store) = setup();
    push(&fake.exchange, Ok(login("AT", "RT", 3600)));
    push(&fake.projects, Ok(vec![]));
    p.begin_login().unwrap();
    p.handle_auth_link("figcraft://auth?code=abc");
    (p, fake, store)
}

fn assert_logged_out_locally(p: &Portal, store: &MemoryStore) {
    let v = p.view();
    assert_eq!((v.status, v.account), (Status::SignedOut, None));
    assert_eq!(store.load().unwrap(), None);
}

#[test]
fn logout_tells_the_portal_with_the_stored_refresh_token() {
    let (p, fake, store) = sign_in_for_logout();
    store.save("RT-newest").unwrap();
    push(&fake.logout, Ok(()));
    p.logout();
    assert!(fake.calls().contains(&"logout AT RT-newest".to_string()));
    assert_logged_out_locally(&p, &store);
}

#[test]
fn logout_still_clears_when_the_portal_says_unauthorized() {
    let (p, fake, store) = sign_in_for_logout();
    push(&fake.logout, Err(unauthorized()));
    p.logout();
    assert!(fake.calls().contains(&"logout AT RT".to_string()));
    assert_logged_out_locally(&p, &store);
}

#[test]
fn logout_still_clears_when_the_portal_is_unreachable() {
    let (p, fake, store) = sign_in_for_logout();
    push(&fake.logout, Err(PortalError::Network("down".into())));
    p.logout();
    assert_logged_out_locally(&p, &store);
    assert_eq!(p.view().notice, None);
}

#[test]
fn logout_when_signed_out_calls_nothing() {
    let (p, fake, _store) = setup();
    p.logout();
    assert!(!fake.calls().iter().any(|c| c.starts_with("logout")));
}

#[test]
fn a_login_without_a_refresh_token_keeps_none() {
    let (p, fake, store) = setup();
    store.save("old").unwrap();
    push(&fake.exchange, Ok(login("AT", "", 3600)));
    push(&fake.projects, Ok(vec![]));
    p.begin_login().unwrap();
    p.handle_auth_link("figcraft://auth?code=abc");
    assert_eq!(p.view().status, Status::SignedIn);
    assert_eq!(store.load().unwrap(), None);
}

#[test]
fn restore_signs_in_from_the_kept_refresh_token_once() {
    let (p, fake, store) = setup();
    store.save("RT0").unwrap();
    push(&fake.refresh, Ok(login("AT", "RT1", 3600)));
    push(&fake.projects, Ok(vec![project(1)]));
    p.restore();
    p.restore();
    let v = p.view();
    assert_eq!(v.status, Status::SignedIn);
    assert!(v.remembered);
    assert_eq!(fake.calls(), ["refresh RT0", "projects AT"]);
    assert_eq!(store.load().unwrap().as_deref(), Some("RT1"));
    // A name typed in Preferences isn't overwritten by a restore.
    assert_eq!(p.take_display_name(), None);
}

#[test]
fn restore_with_a_spent_token_forgets_it() {
    let (p, fake, store) = setup();
    store.save("RT0").unwrap();
    push(&fake.refresh, Err(unauthorized()));
    p.restore();
    assert_eq!(p.view().status, Status::SignedOut);
    assert_eq!(store.load().unwrap(), None);
}

#[test]
fn restore_when_the_portal_is_down_keeps_the_token() {
    let (p, fake, store) = setup();
    store.save("RT0").unwrap();
    push(&fake.refresh, Err(PortalError::Network("down".into())));
    p.restore();
    let v = p.view();
    assert_eq!((v.status, v.notice), (Status::SignedOut, Some("down".into())));
    assert_eq!(store.load().unwrap().as_deref(), Some("RT0"));
}

#[test]
fn restore_with_nothing_kept_does_nothing() {
    let (p, fake, _) = setup();
    p.restore();
    assert_eq!(p.view().status, Status::SignedOut);
    assert!(fake.calls().is_empty());
}

#[test]
fn cancel_stops_waiting_and_the_late_callback_is_refused() {
    let (p, fake, _) = setup();
    p.begin_login().unwrap();
    p.cancel_login();
    assert_eq!(p.view().status, Status::SignedOut);
    assert!(p.handle_auth_link("figcraft://auth?code=abc"));
    assert!(matches!(p.view().status, Status::Failed(_)));
    assert!(fake.calls().is_empty());
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("figcraft-portal-session-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// Another process of the app: the same portal, credential store and settings folder.
fn copy_of(fake: &Arc<Fake>, store: &Arc<MemoryStore>, dir: &std::path::Path) -> Portal {
    Portal::new(BASE, fake.clone(), Box::new(Shared(store.clone())), Box::new(|| {}), inline, Some(dir.to_path_buf()))
}

#[test]
fn a_new_copy_started_by_the_link_finishes_the_login_for_the_waiting_copy() {
    let dir = temp_dir("handoff");
    let (fake, store) = (Arc::new(Fake::default()), Arc::new(MemoryStore::default()));
    let (first, second) = (copy_of(&fake, &store, &dir), copy_of(&fake, &store, &dir));
    push(&fake.exchange, Ok(login("AT-second", "RT1", 3600)));
    push(&fake.projects, Ok(vec![project(1)]));
    push(&fake.refresh, Ok(login("AT-first", "RT2", 3600)));
    push(&fake.projects, Ok(vec![project(1)]));
    let challenge = challenge_of(&first.begin_login().unwrap());
    // The system starts the second copy with the link; the first is still waiting.
    assert!(second.handle_auth_link("figcraft://auth?code=c1"));
    assert_eq!(second.view().status, Status::SignedIn);
    assert_eq!(first.view().status, Status::WaitingForBrowser);
    first.poll_handoff();
    assert_eq!(first.view().status, Status::SignedIn);
    assert_eq!(first.take_display_name().as_deref(), Some("Ada Lovelace"));
    assert_eq!(fake.calls(), vec![format!("exchange c1 {challenge}"), "projects AT-second".into(), "refresh RT1".into(), "projects AT-first".into()]);
    assert_eq!(store.load().unwrap().as_deref(), Some("RT2"));
    assert_eq!(crate::handoff::read(&dir), None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_failure_in_the_new_copy_reaches_the_waiting_copy() {
    let dir = temp_dir("handoff-fail");
    let (fake, store) = (Arc::new(Fake::default()), Arc::new(MemoryStore::default()));
    let (first, second) = (copy_of(&fake, &store, &dir), copy_of(&fake, &store, &dir));
    push(&fake.exchange, Err(PortalError::Unauthorized("Code is invalid.".into())));
    first.begin_login().unwrap();
    second.handle_auth_link("figcraft://auth?code=c1");
    assert_eq!(second.view().status, Status::Failed("Code is invalid.".into()));
    first.poll_handoff();
    assert_eq!(first.view().status, Status::Failed("Code is invalid.".into()));
    assert_eq!(crate::handoff::read(&dir), None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_new_copy_with_no_note_has_no_login_to_answer() {
    let dir = temp_dir("handoff-none");
    let (fake, store) = (Arc::new(Fake::default()), Arc::new(MemoryStore::default()));
    let second = copy_of(&fake, &store, &dir);
    second.handle_auth_link("figcraft://auth?code=c1");
    assert!(matches!(second.view().status, Status::Failed(m) if m.contains("No login is waiting")));
    assert!(fake.calls().is_empty());
    assert_eq!(crate::handoff::read(&dir), None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_old_note_is_refused_by_the_new_copy() {
    let dir = temp_dir("handoff-old");
    let (fake, store) = (Arc::new(Fake::default()), Arc::new(MemoryStore::default()));
    let (first, second) = (copy_of(&fake, &store, &dir), copy_of(&fake, &store, &dir));
    first.begin_login().unwrap();
    let Some(crate::handoff::Note::Waiting { verifier, .. }) = crate::handoff::read(&dir) else { panic!("no note") };
    let started = crate::handoff::now() - LOGIN_WAIT.as_secs() - 5;
    crate::handoff::write(&dir, &crate::handoff::Note::Waiting { verifier, started }).unwrap();
    second.handle_auth_link("figcraft://auth?code=c1");
    assert!(matches!(second.view().status, Status::Failed(m) if m.contains("took too long")));
    first.poll_handoff();
    assert!(matches!(first.view().status, Status::Failed(m) if m.contains("took too long")));
    assert!(fake.calls().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_refresh_uses_the_newest_token_in_the_credential_store() {
    let (p, fake, store) = setup();
    push(&fake.exchange, Ok(login("AT1", "RT1", 0)));
    push(&fake.refresh, Ok(login("AT2", "RT2", 0)));
    push(&fake.projects, Ok(vec![]));
    p.begin_login().unwrap();
    p.handle_auth_link("figcraft://auth?code=abc");
    // Another copy of the app rotated the token since.
    store.save("RT9").unwrap();
    push(&fake.refresh, Ok(login("AT3", "RT3", 3600)));
    push(&fake.projects, Ok(vec![]));
    p.refresh_projects();
    assert_eq!(fake.calls()[1..], ["refresh RT1", "projects AT2", "refresh RT9", "projects AT3"]);
    assert_eq!(store.load().unwrap().as_deref(), Some("RT3"));
}
