//! The sign-in state the window shows, and the work behind it. [`Portal`] is cheap to clone (all
//! clones are one portal); the window asks [`Portal::view`] every frame and starts work with
//! [`Portal::begin_login`], [`Portal::handle_auth_link`], [`Portal::refresh_projects`] and
//! [`Portal::open_project`], each of which does its network calls on a background thread and
//! wakes the window when it is done.
//!
//! Tokens: the access token lives in memory only; the refresh token is rotated by the portal on
//! every use, so refreshes are serialised (two at once would present a used token and the portal
//! would end the whole session) and the new token replaces the old one in the credential store at
//! once. The pending login (its verifier) is held in memory, answers one callback and expires.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use crate::api::{Login, OpenedProject, PortalError, ProjectEntry};
use crate::deeplink::{AuthLink, parse_auth_link};
use crate::handoff::{self, Note};
use crate::secret::SecretStore;
use crate::{config, pkce};

/// The portal's calls (the HTTPS [`crate::client::Client`], or a fake in tests).
pub trait Api: Send + Sync {
    /// Trade a one-time code and its verifier for a session.
    fn exchange_code(&self, code: &str, verifier: &str) -> Result<Login, PortalError>;
    /// Trade a refresh token for a new session (the old refresh token is spent).
    fn refresh(&self, refresh_token: &str) -> Result<Login, PortalError>;
    fn projects(&self, access_token: &str) -> Result<Vec<ProjectEntry>, PortalError>;
    fn open_project(&self, access_token: &str, project_id: i64) -> Result<OpenedProject, PortalError>;
    /// End the app session on the portal (`401` is an error the caller may ignore: already ended).
    fn logout(&self, access_token: &str, refresh_token: &str) -> Result<(), PortalError>;
}

/// How long a started login waits for its browser callback.
pub const LOGIN_WAIT: Duration = Duration::from_secs(10 * 60);

/// An access token is renewed when it has less than this left.
const RENEW_BEFORE: Duration = Duration::from_secs(30);

/// Where a sign-in is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    SignedOut,
    /// The login page is open in the browser; the app waits for `figcraft://auth`.
    WaitingForBrowser,
    /// Trading the code (or the remembered refresh token) for a session.
    SigningIn,
    SignedIn,
    Failed(String),
}

/// The person who signed in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Account {
    pub display_name: String,
    pub username: String,
    pub company: String,
    pub role: String,
}

/// The project list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Projects {
    NotLoaded,
    Loading,
    Loaded(Vec<ProjectEntry>),
    Failed(String),
}

/// What the window draws.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View {
    pub status: Status,
    pub account: Option<Account>,
    pub projects: Projects,
    /// The project being opened.
    pub opening: Option<i64>,
    /// Something to tell the person that isn't a sign-in failure (a project that wouldn't open).
    pub notice: Option<String>,
    /// The refresh token outlives this run (it is in the system's credential store).
    pub remembered: bool,
    /// Something is in flight: the window keeps asking.
    pub busy: bool,
}

/// Runs a job off the window's thread.
pub type Spawn = fn(Box<dyn FnOnce() + Send + 'static>);

/// One thread per job.
pub fn spawn_thread(job: Box<dyn FnOnce() + Send + 'static>) {
    std::thread::spawn(job);
}

struct Tokens {
    access: String,
    access_until: Instant,
    refresh: String,
}

struct Pending {
    verifier: String,
    started: Instant,
}

/// A login a callback can answer.
struct Waiting {
    verifier: String,
    expired: bool,
    /// Another copy of the app started it.
    from_other_copy: bool,
}

struct State {
    status: Status,
    pending: Option<Pending>,
    tokens: Option<Tokens>,
    account: Option<Account>,
    projects: Projects,
    opening: Option<i64>,
    opened: Option<OpenedProject>,
    name: Option<String>,
    notice: Option<String>,
    restored: bool,
}

struct Inner {
    base: String,
    api: Arc<dyn Api>,
    store: Box<dyn SecretStore>,
    wake: Box<dyn Fn() + Send + Sync>,
    spawn: Spawn,
    /// The settings folder, where a login leaves its note for another copy of the app.
    notes: Option<PathBuf>,
    state: Mutex<State>,
    /// Held while a refresh token is being traded.
    refreshing: Mutex<()>,
}

/// The portal at one address.
#[derive(Clone)]
pub struct Portal(Arc<Inner>);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Portal {
    pub fn new(
        base: &str,
        api: Arc<dyn Api>,
        store: Box<dyn SecretStore>,
        wake: Box<dyn Fn() + Send + Sync>,
        spawn: Spawn,
        notes: Option<PathBuf>,
    ) -> Self {
        let state = State {
            status: Status::SignedOut,
            pending: None,
            tokens: None,
            account: None,
            projects: Projects::NotLoaded,
            opening: None,
            opened: None,
            name: None,
            notice: None,
            restored: false,
        };
        Portal(Arc::new(Inner { base: base.to_string(), api, store, wake, spawn, notes, state: Mutex::new(state), refreshing: Mutex::new(()) }))
    }

    /// The portal at the configured address, over HTTPS, with the system's credential store.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn system(wake: Box<dyn Fn() + Send + Sync>, notes: Option<PathBuf>) -> Result<Self, config::ConfigError> {
        let base = config::base_url()?;
        let api = Arc::new(crate::client::Client::new(&base));
        let store = crate::secret::system_store(&base);
        Ok(Self::new(&base, api, store, wake, spawn_thread, notes))
    }

    /// The address signing in goes to.
    pub fn base_url(&self) -> &str {
        &self.0.base
    }

    pub fn view(&self) -> View {
        let s = lock(&self.0.state);
        let busy = matches!(s.status, Status::SigningIn) || s.projects == Projects::Loading || s.opening.is_some();
        View {
            status: s.status.clone(),
            account: s.account.clone(),
            projects: s.projects.clone(),
            opening: s.opening,
            notice: s.notice.clone(),
            remembered: self.0.store.persistent(),
            busy,
        }
    }

    fn set(&self, f: impl FnOnce(&mut State)) {
        f(&mut lock(&self.0.state));
        (self.0.wake)();
    }

    fn run(&self, job: impl FnOnce(Portal) + Send + 'static) {
        let me = self.clone();
        (self.0.spawn)(Box::new(move || job(me)));
    }

    /// Start a login: the address of the login page to open in the browser.
    pub fn begin_login(&self) -> Result<String, String> {
        let verifier = pkce::new_verifier()?;
        let url = config::login_page_url(&self.0.base, &pkce::challenge(&verifier));
        if let Some(dir) = &self.0.notes {
            handoff::write(dir, &Note::Waiting { verifier: verifier.clone(), started: handoff::now() })?;
        }
        self.set(|s| {
            s.pending = Some(Pending { verifier, started: Instant::now() });
            s.restored = true;
            s.status = Status::WaitingForBrowser;
            s.notice = None;
        });
        Ok(url)
    }

    fn clear_note(&self) {
        if let Some(dir) = &self.0.notes {
            handoff::clear(dir);
        }
    }

    fn leave_note(&self, note: &Note) {
        if let Some(dir) = &self.0.notes {
            let _ = handoff::write(dir, note);
        }
    }

    /// Stop waiting for the browser.
    pub fn cancel_login(&self) {
        self.clear_note();
        self.set(|s| {
            s.pending = None;
            if s.status == Status::WaitingForBrowser {
                s.status = Status::SignedOut;
            }
        });
    }

    /// The login this callback answers: this copy's own, else the note another copy left (the
    /// system starts a new process for the link). `Err` says why there is none.
    fn waiting_login(&self) -> Result<Waiting, String> {
        let own = lock(&self.0.state).pending.take();
        if let Some(p) = own {
            return Ok(Waiting { verifier: p.verifier, expired: p.started.elapsed() > LOGIN_WAIT, from_other_copy: false });
        }
        match self.0.notes.as_deref().and_then(handoff::read) {
            Some(Note::Waiting { verifier, started }) => {
                Ok(Waiting { verifier, expired: handoff::now().saturating_sub(started) > LOGIN_WAIT.as_secs(), from_other_copy: true })
            }
            _ => Err("No login is waiting for this link. Press Log in first.".to_string()),
        }
    }

    /// `link` was handed to the app: when it is a `figcraft://auth` link, use it (true); any other
    /// link isn't this module's (false).
    pub fn handle_auth_link(&self, link: &str) -> bool {
        if !crate::deeplink::is_auth_link(link) {
            return false;
        }
        // A login is under way: the refresh token kept by an earlier run is not signed in from on top of it.
        lock(&self.0.state).restored = true;
        let verdict = self.waiting_login().and_then(|w| {
            if w.expired {
                return Err("The login took too long. Press Log in to try again.".to_string());
            }
            match parse_auth_link(link) {
                Ok(AuthLink::Code(code)) => Ok((code, w)),
                Err(_) => Err("The login link isn't valid. Press Log in to try again.".to_string()),
            }
        });
        match verdict {
            Ok((code, waiting)) => {
                self.set(|s| s.status = Status::SigningIn);
                self.run(move |me| me.exchange(&code, &waiting));
            }
            Err(message) => {
                if self.0.notes.as_deref().is_some_and(|d| matches!(handoff::read(d), Some(Note::Waiting { .. }))) {
                    self.leave_note(&Note::Failed(message.clone()));
                }
                self.set(|s| {
                    if s.status == Status::SignedIn {
                        s.notice = Some(message);
                    } else {
                        s.status = Status::Failed(message);
                    }
                });
            }
        }
        true
    }

    fn exchange(&self, code: &str, waiting: &Waiting) {
        match self.0.api.exchange_code(code, &waiting.verifier) {
            Ok(login) => {
                self.install(login, true);
                // The copy that pressed Log in finishes from the stored refresh token.
                if waiting.from_other_copy {
                    self.leave_note(&Note::Done);
                } else {
                    self.clear_note();
                }
                self.load_projects();
            }
            Err(e) => {
                if waiting.from_other_copy {
                    self.leave_note(&Note::Failed(e.to_string()));
                } else {
                    self.clear_note();
                }
                self.set(|s| s.status = Status::Failed(e.to_string()));
            }
        }
    }

    /// While the browser is awaited: has another copy of the app handled the callback? Then sign
    /// in here from the refresh token it stored (the window asks about once a second).
    pub fn poll_handoff(&self) {
        if lock(&self.0.state).status != Status::WaitingForBrowser {
            return;
        }
        match self.0.notes.as_deref().and_then(handoff::read) {
            Some(Note::Done) => {
                self.clear_note();
                self.set(|s| {
                    s.pending = None;
                    s.status = Status::SigningIn;
                });
                self.run(|me| me.sign_in_from_store(true));
            }
            Some(Note::Failed(message)) => {
                self.clear_note();
                self.set(|s| {
                    s.pending = None;
                    s.status = Status::Failed(message);
                });
            }
            _ => {}
        }
    }

    /// Trade the stored refresh token for a session of this copy.
    fn sign_in_from_store(&self, apply_name: bool) {
        let token = self.0.store.load().ok().flatten().filter(|t| !t.is_empty());
        let Some(token) = token else {
            self.set(|s| s.status = Status::SignedOut);
            return;
        };
        let result = {
            let _one = lock(&self.0.refreshing);
            self.0.api.refresh(&token)
        };
        match result {
            Ok(login) => {
                self.install(login, apply_name);
                self.load_projects();
            }
            Err(PortalError::Unauthorized(_)) => {
                let _ = self.0.store.clear();
                self.set(|s| s.status = Status::SignedOut);
            }
            Err(e) => self.set(|s| {
                s.status = Status::SignedOut;
                s.notice = Some(e.to_string());
            }),
        }
    }

    /// Keep a new session: the tokens, who it is, and the refresh token in the credential store.
    fn install(&self, login: Login, apply_name: bool) {
        let account = Account {
            display_name: login.display_name(),
            username: login.username.clone(),
            company: login.company_name.clone(),
            role: login.role_name.clone(),
        };
        let saved = if login.refresh_token.is_empty() { self.0.store.clear() } else { self.0.store.save(&login.refresh_token) };
        let notice = saved.err().map(|_| "The login couldn't be remembered on this computer; you'll log in again next time.".to_string());
        let until = Instant::now() + Duration::from_secs(login.expire_time);
        self.set(|s| {
            s.tokens = Some(Tokens { access: login.token, access_until: until, refresh: login.refresh_token });
            if apply_name && !account.display_name.is_empty() {
                s.name = Some(account.display_name.clone());
            }
            s.account = Some(account);
            s.status = Status::SignedIn;
            s.notice = notice;
        });
    }

    /// Log out: tell the portal in the background (best effort: a failure changes nothing), and
    /// forget the session on this computer at once.
    pub fn logout(&self) {
        let held = lock(&self.0.state).tokens.as_ref().map(|t| (t.access.clone(), t.refresh.clone()));
        if let Some((access, held_refresh)) = held {
            // The credential store has the newest refresh token (another copy may have rotated it).
            let refresh = self.0.store.load().ok().flatten().filter(|t| !t.is_empty()).unwrap_or(held_refresh);
            if !refresh.is_empty() {
                self.run(move |me| {
                    let _ = me.0.api.logout(&access, &refresh);
                });
            }
        }
        self.forget();
    }

    /// Forget everything about the session on this computer.
    fn forget(&self) {
        let _ = self.0.store.clear();
        self.clear_note();
        self.set(|s| {
            s.pending = None;
            s.tokens = None;
            s.account = None;
            s.projects = Projects::NotLoaded;
            s.opening = None;
            s.opened = None;
            s.notice = None;
            s.status = Status::SignedOut;
        });
    }

    /// Sign in again from the refresh token kept by an earlier run (once per run).
    pub fn restore(&self) {
        let first = !std::mem::replace(&mut lock(&self.0.state).restored, true);
        if !first || !self.0.store.load().ok().flatten().is_some_and(|t| !t.is_empty()) {
            return;
        }
        self.set(|s| s.status = Status::SigningIn);
        self.run(|me| me.sign_in_from_store(false));
    }

    /// An access token that is good for a while: the one held, or a new one from the refresh token.
    fn access_token(&self, force: bool) -> Result<String, PortalError> {
        let _one = lock(&self.0.refreshing);
        let (access, held) = {
            let s = lock(&self.0.state);
            let Some(t) = s.tokens.as_ref() else { return Err(PortalError::Unauthorized("You are not logged in.".into())) };
            if !force && t.access_until > Instant::now() + RENEW_BEFORE {
                return Ok(t.access.clone());
            }
            (t.access.clone(), t.refresh.clone())
        };
        // Another copy of the app may have rotated the token since: the credential store has the newest.
        let refresh = self.0.store.load().ok().flatten().filter(|t| !t.is_empty()).unwrap_or(held);
        if refresh.is_empty() {
            return if force { Err(PortalError::Unauthorized("The session ended; log in again.".into())) } else { Ok(access) };
        }
        self.0.api.refresh(&refresh).map(|login| {
            let token = login.token.clone();
            self.install(login, false);
            token
        })
    }

    /// `call` with an access token; a token the portal refuses is renewed once, and a session the
    /// portal no longer knows ends here.
    fn with_token<T>(&self, call: impl Fn(&dyn Api, &str) -> Result<T, PortalError>) -> Result<T, PortalError> {
        let result = self.access_token(false).and_then(|t| match call(&*self.0.api, &t) {
            Err(PortalError::Unauthorized(_)) => self.access_token(true).and_then(|t| call(&*self.0.api, &t)),
            other => other,
        });
        if let Err(PortalError::Unauthorized(_)) = &result {
            self.forget();
            self.set(|s| s.status = Status::Failed("Your session ended. Log in again.".into()));
        }
        result
    }

    /// Ask for the person's projects in the background.
    pub fn refresh_projects(&self) {
        self.set(|s| s.projects = Projects::Loading);
        self.run(|me| me.load_projects());
    }

    fn load_projects(&self) {
        self.set(|s| s.projects = Projects::Loading);
        let result = self.with_token(|api, token| api.projects(token));
        self.set(|s| {
            if s.status == Status::SignedIn {
                s.projects = match result {
                    Ok(list) => Projects::Loaded(list),
                    Err(e) => Projects::Failed(e.to_string()),
                };
            }
        });
    }

    /// Ask for the join link of project `id` in the background; [`Portal::take_opened`] has it.
    pub fn open_project(&self, id: i64) {
        self.set(|s| {
            s.opening = Some(id);
            s.notice = None;
        });
        self.run(move |me| {
            let result = me.with_token(|api, token| api.open_project(token, id));
            me.set(|s| {
                s.opening = None;
                match result {
                    Ok(opened) => s.opened = Some(opened),
                    Err(e) => s.notice = Some(e.to_string()),
                }
            });
        });
    }

    /// The project opened since the last call, with its join link.
    pub fn take_opened(&self) -> Option<OpenedProject> {
        lock(&self.0.state).opened.take()
    }

    /// The name to put in Preferences after a login (once).
    pub fn take_display_name(&self) -> Option<String> {
        lock(&self.0.state).name.take()
    }

    pub fn dismiss_notice(&self) {
        lock(&self.0.state).notice = None;
    }

    /// A login that was started `by` ago (tests).
    #[cfg(test)]
    pub(crate) fn backdate_pending(&self, by: Duration) {
        if let Some(p) = lock(&self.0.state).pending.as_mut() {
            p.started = Instant::now().checked_sub(by).unwrap_or_else(Instant::now);
        }
    }
}
