//! The portal as the browser build uses it: the same window and the same answers as [`crate::session::Portal`],
//! but the page has no threads, no credential store, no `figiran://auth` and no loopback port. So:
//!
//! * requests are `fetch` calls (an `XMLHttpRequest` for the upload, which reports its progress), run as
//!   futures on the page's single thread; nothing blocks a frame;
//! * the login page opens in a tab this page keeps a handle to, and its one-time code comes back as a
//!   `message` from the portal's origin ([`crate::webflow`] decides what counts as the answer);
//! * the session lives in memory only: the access token and the rotating refresh token are never written
//!   to `localStorage` or cookies, so a page that is closed or reloaded asks for the login again (the
//!   portal's own session makes that one click). Requests send no cookies (`credentials: "omit"`): the
//!   portal accepts this page's origin for the app endpoints only, with the bearer token.
//!
//! The calls are the app's own (`app/token`, `users/refresh-token`, `app/projects`, `app/logout`,
//! `projects/upload`), parsed by the same functions as the desktop client's.

use std::collections::VecDeque;
use std::future::Future;
use std::io::{Cursor, Read};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use base64::Engine as _;
use js_sys::{Date, Function, Promise, Reflect};
use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;
use wasm_bindgen::prelude::Closure;
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{AbortSignal, Headers, MessageEvent, ProgressEvent, Request, RequestCredentials, RequestInit, RequestMode, Response, XmlHttpRequest};

use crate::api::{self, Login, OpenedProject, PortalError};
use crate::config::{self, ConfigError};
use crate::session::{Account, Projects, Status, View};
use crate::upload::{self, Companies, MAX_UPLOAD_BYTES, Multipart, Progress, UploadError, UploadJob, UploadPhase, UploadShared, UploadView, UploadedProject};
use crate::{pkce, webflow};

/// A call that isn't an upload gives up after this long (milliseconds).
const TIMEOUT_MS: u32 = 20_000;

/// An upload sends up to 200 MB and then waits for the portal to store it (milliseconds).
const UPLOAD_TIMEOUT_MS: u32 = 30 * 60 * 1000;

/// How often a running upload looks at its Cancel flag (milliseconds).
const CANCEL_POLL_MS: i32 = 250;

/// Answers waiting for the window (a login's code): a message from anyone else never gets in.
const MAX_ANSWERS: usize = 4;

/// Fill `bytes` with the browser's secure random numbers (also what [`pkce::new_verifier`] uses here).
pub(crate) fn fill_random(bytes: &mut [u8]) -> Result<(), String> {
    let crypto = web_sys::window().ok_or("no window")?.crypto().map_err(|_| "no secure random numbers in this browser")?;
    crypto.get_random_values_with_u8_array(bytes).map_err(|_| "no secure random numbers in this browser")?;
    Ok(())
}

fn now_ms() -> f64 {
    Date::now()
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn network(what: &str) -> PortalError {
    PortalError::Network(format!("The portal can't be reached ({what})."))
}

/// A promise that settles after `ms`.
async fn sleep(ms: i32) {
    let promise = Promise::new(&mut |resolve: Function, _reject: Function| {
        if let Some(window) = web_sys::window() {
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms);
        }
    });
    let _ = JsFuture::from(promise).await;
}

struct Tokens {
    access: String,
    /// `Date.now()` when the access token ends.
    access_until_ms: f64,
    refresh: String,
}

struct Pending {
    verifier: String,
    state: String,
    started_ms: f64,
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
    /// A refresh token is being traded: a second trade would present a spent token and the portal would end the session.
    refreshing: bool,
}

struct Inner {
    base: String,
    wake: Arc<dyn Fn() + Send + Sync>,
    state: Mutex<State>,
    uploads: Mutex<UploadShared>,
    /// Codes the portal's page posted (see [`webflow::parse_callback`]), for [`Portal::poll_handoff`].
    answers: Arc<Mutex<VecDeque<webflow::Callback>>>,
}

/// The portal at one address, in the browser. Cheap to clone (all clones are one portal).
#[derive(Clone)]
pub struct Portal(Arc<Inner>);

impl Portal {
    /// The portal at the configured address (`FIGIRAN_PORTAL_URL` at build time). `notes` is the desktop app's
    /// settings folder: not used here.
    pub fn system(wake: Box<dyn Fn() + Send + Sync>, _notes: Option<PathBuf>) -> Result<Self, ConfigError> {
        let base = config::base_url()?;
        let origin = webflow::origin_of(&base).ok_or_else(|| ConfigError::Invalid("has no host".into()))?;
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::from(wake);
        let answers = Arc::new(Mutex::new(VecDeque::new()));
        listen(origin, answers.clone(), wake.clone());
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
            refreshing: false,
        };
        Ok(Portal(Arc::new(Inner { base, wake, state: Mutex::new(state), uploads: Mutex::new(UploadShared::default()), answers })))
    }

    /// The address signing in goes to.
    pub fn base_url(&self) -> &str {
        &self.0.base
    }

    pub fn view(&self) -> View {
        let s = lock(&self.0.state);
        let busy = matches!(s.status, Status::SigningIn) || s.projects == Projects::Loading || s.opening.is_some();
        View { status: s.status.clone(), account: s.account.clone(), projects: s.projects.clone(), opening: s.opening, notice: s.notice.clone(), remembered: false, busy }
    }

    fn set(&self, f: impl FnOnce(&mut State)) {
        f(&mut lock(&self.0.state));
        (self.0.wake)();
    }

    fn set_upload(&self, f: impl FnOnce(&mut UploadShared)) {
        f(&mut lock(&self.0.uploads));
        (self.0.wake)();
    }

    /// Nothing is remembered between page loads.
    pub fn restore(&self) {}

    /// `figiran://auth` links are the desktop app's: the page gets its answer as a message.
    pub fn handle_auth_link(&self, _link: &str) -> bool {
        false
    }

    /// Start a login: the address of the login page to open in a tab this page keeps a handle to.
    pub fn begin_login(&self) -> Result<String, String> {
        let verifier = pkce::new_verifier()?;
        let state = pkce::new_verifier()?;
        let url = webflow::login_url(&self.0.base, &pkce::challenge(&verifier), &state);
        self.set(|s| {
            s.pending = Some(Pending { verifier, state, started_ms: now_ms() });
            s.status = Status::WaitingForBrowser;
            s.notice = None;
        });
        Ok(url)
    }

    pub fn cancel_login(&self) {
        self.set(|s| {
            s.pending = None;
            if s.status == Status::WaitingForBrowser {
                s.status = Status::SignedOut;
            }
        });
    }

    /// While the login tab is awaited: take the code the portal's page posted (the window asks every frame).
    pub fn poll_handoff(&self) {
        loop {
            let Some(answer) = lock(&self.0.answers).pop_front() else { return };
            let waiting = {
                let s = lock(&self.0.state);
                match (&s.status, &s.pending) {
                    (Status::WaitingForBrowser, Some(p)) => Some((p.verifier.clone(), webflow::check_answer(&p.state, &answer.state, now_ms() - p.started_ms))),
                    _ => None,
                }
            };
            // A message while no login is under way, or for another login, is not this page's business.
            let Some((verifier, verdict)) = waiting else { continue };
            match verdict {
                Ok(()) => {
                    self.set(|s| {
                        s.pending = None;
                        s.status = Status::SigningIn;
                    });
                    let me = self.clone();
                    spawn_local(async move { me.exchange(&answer.code, &verifier).await });
                }
                Err(message) => self.set(|s| {
                    s.pending = None;
                    s.status = Status::Failed(message.to_string());
                }),
            }
        }
    }

    async fn exchange(&self, code: &str, verifier: &str) {
        let body = serde_json::json!({"code": code, "verifier": verifier}).to_string();
        let result = match self.request("POST", "api/v1/app/token", None, Some(body)).await {
            Ok((status, text)) => api::parse_login(status, &text),
            Err(e) => Err(e),
        };
        match result {
            Ok(login) => {
                self.install(login, true);
                self.load_projects().await;
            }
            Err(e) => self.set(|s| s.status = Status::Failed(e.to_string())),
        }
    }

    /// Keep a new session: the tokens (in memory only), who it is.
    fn install(&self, login: Login, apply_name: bool) {
        let account = Account { display_name: login.display_name(), username: login.username.clone(), company: login.company_name.clone(), role: login.role_name.clone() };
        let until = now_ms() + login.expire_time as f64 * 1000.0;
        self.set(|s| {
            s.tokens = Some(Tokens { access: login.token, access_until_ms: until, refresh: login.refresh_token });
            if apply_name && !account.display_name.is_empty() {
                s.name = Some(account.display_name.clone());
            }
            s.account = Some(account);
            s.status = Status::SignedIn;
            s.notice = None;
        });
    }

    pub fn logout(&self) {
        let held = lock(&self.0.state).tokens.as_ref().map(|t| (t.access.clone(), t.refresh.clone()));
        if let Some((access, refresh)) = held.filter(|(_, refresh)| !refresh.is_empty()) {
            let me = self.clone();
            spawn_local(async move {
                let body = serde_json::json!({"refreshToken": refresh}).to_string();
                // Best effort: a portal that can't be reached has nothing more to end.
                let _ = me.request("POST", "api/v1/app/logout", Some(&access), Some(body)).await;
            });
        }
        self.forget();
    }

    /// Forget everything about the session in this page.
    fn forget(&self) {
        lock(&self.0.answers).clear();
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

    /// One call: `(status, body)`. No cookies are sent; `bearer` is the access token.
    async fn request(&self, method: &str, path: &str, bearer: Option<&str>, json: Option<String>) -> Result<(u16, String), PortalError> {
        let url = format!("{}/{path}", self.0.base);
        let window = web_sys::window().ok_or_else(|| network("no window"))?;
        let headers = Headers::new().map_err(|_| network("headers"))?;
        headers.set("X-Language", "en").map_err(|_| network("headers"))?;
        // A POST always carries a JSON body (empty when it has nothing to say), as the desktop client's does.
        let body = json.or_else(|| (method == "POST").then(|| "{}".to_string()));
        if body.is_some() {
            headers.set("Content-Type", "application/json").map_err(|_| network("headers"))?;
        }
        if let Some(token) = bearer {
            headers.set("Authorization", &format!("Bearer {token}")).map_err(|_| network("headers"))?;
        }
        let init = RequestInit::new();
        init.set_method(method);
        init.set_mode(RequestMode::Cors);
        init.set_credentials(RequestCredentials::Omit);
        init.set_headers(&headers);
        init.set_signal(Some(&AbortSignal::timeout_with_u32(TIMEOUT_MS)));
        if let Some(body) = body.as_deref() {
            init.set_body(&JsValue::from_str(body));
        }
        let request = Request::new_with_str_and_init(&url, &init).map_err(|_| network("request"))?;
        let response = JsFuture::from(window.fetch_with_request(&request)).await.map_err(|_| network("no answer, or the portal doesn't accept this page"))?;
        let response: Response = response.dyn_into().map_err(|_| network("answer"))?;
        let status = response.status();
        let text = JsFuture::from(response.text().map_err(|_| network("answer"))?).await.map_err(|_| network("answer"))?;
        Ok((status, text.as_string().unwrap_or_default()))
    }

    /// An access token that is good for a while: the one held, or a new one from the refresh token. Renewals are serialised.
    async fn access_token(&self, force: bool) -> Result<String, PortalError> {
        while !self.claim_refresh() {
            sleep(40).await;
        }
        let result = self.access_token_held(force).await;
        lock(&self.0.state).refreshing = false;
        result
    }

    fn claim_refresh(&self) -> bool {
        let mut s = lock(&self.0.state);
        if s.refreshing {
            return false;
        }
        s.refreshing = true;
        true
    }

    async fn access_token_held(&self, force: bool) -> Result<String, PortalError> {
        let (access, refresh) = {
            let s = lock(&self.0.state);
            let Some(t) = s.tokens.as_ref() else { return Err(PortalError::Unauthorized("You are not logged in.".into())) };
            if !force && !webflow::needs_renewal(now_ms(), t.access_until_ms) {
                return Ok(t.access.clone());
            }
            (t.access.clone(), t.refresh.clone())
        };
        if refresh.is_empty() {
            return if force { Err(PortalError::Unauthorized("The session ended; log in again.".into())) } else { Ok(access) };
        }
        let body = serde_json::json!({"refreshToken": refresh}).to_string();
        let (status, text) = self.request("POST", "api/v1/users/refresh-token", None, Some(body)).await?;
        let login = api::parse_login(status, &text)?;
        let token = login.token.clone();
        self.install(login, false);
        Ok(token)
    }

    /// `call` with an access token; a token the portal refuses is renewed once. The session ends here only when the renewal itself
    /// is refused (as the desktop app does).
    async fn with_token<T, F, Fut>(&self, call: F) -> Result<T, PortalError>
    where
        F: Fn(String) -> Fut,
        Fut: Future<Output = Result<T, PortalError>>,
    {
        let result = match self.access_token(false).await {
            Err(e) => Err(e),
            Ok(token) => match call(token).await {
                Err(PortalError::Unauthorized(_)) => match self.access_token(true).await {
                    Ok(fresh) => return call(fresh).await,
                    Err(e) => Err(e),
                },
                other => other,
            },
        };
        if let Err(PortalError::Unauthorized(_)) = &result {
            self.forget();
            self.set(|s| s.status = Status::Failed("Your session ended. Log in again.".into()));
        }
        result
    }

    /// Ask for the person's projects in the background.
    pub fn refresh_projects(&self) {
        let me = self.clone();
        spawn_local(async move { me.load_projects().await });
    }

    async fn load_projects(&self) {
        self.set(|s| s.projects = Projects::Loading);
        let result = self
            .with_token(|token| async move {
                let (status, text) = self.request("GET", "api/v1/app/projects", Some(&token), None).await?;
                api::parse_projects(status, &text)
            })
            .await;
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
        let me = self.clone();
        spawn_local(async move {
            let result = me
                .with_token(|token| {
                    let me = me.clone();
                    async move {
                        let (status, text) = me.request("POST", &format!("api/v1/app/projects/{id}/open"), Some(&token), None).await?;
                        api::parse_opened(id, status, &text)
                    }
                })
                .await;
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

    /// The desktop app's local bridge hands invite links over; the page has none.
    pub fn take_invite(&self) -> Option<String> {
        None
    }

    pub fn dismiss_notice(&self) {
        lock(&self.0.state).notice = None;
    }

    pub fn upload_view(&self) -> UploadView {
        let u = lock(&self.0.uploads);
        UploadView { phase: u.phase.clone(), sent: u.progress.sent(), total: u.progress.total(), companies: u.companies.clone() }
    }

    /// Ask for the companies an Admin can upload to, in the background.
    pub fn refresh_companies(&self) {
        self.set_upload(|u| u.companies = Companies::Loading);
        let me = self.clone();
        spawn_local(async move {
            let result = me
                .with_token(|token| {
                    let me = me.clone();
                    async move {
                        let (status, text) = me.request("GET", upload::COMPANIES_PATH, Some(&token), None).await?;
                        upload::parse_companies(status, &text)
                    }
                })
                .await;
            me.set_upload(|u| {
                u.companies = match result {
                    Ok(list) => Companies::Loaded(list),
                    Err(e) => Companies::Failed(e.to_string()),
                }
            });
        });
    }

    /// Start uploading `job` in the background; false when an upload is already under way.
    pub fn start_upload(&self, job: UploadJob) -> bool {
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut u = lock(&self.0.uploads);
            if u.phase.is_active() {
                return false;
            }
            u.phase = UploadPhase::Preparing;
            u.cancel = Some(cancel.clone());
            u.progress.restart(0);
        }
        (self.0.wake)();
        let me = self.clone();
        spawn_local(async move {
            let result = me.upload(job, &cancel).await;
            me.set_upload(|u| {
                u.cancel = None;
                u.phase = match result {
                    Ok(project) => UploadPhase::Accepted(project),
                    Err(UploadError::Cancelled) => UploadPhase::Cancelled,
                    Err(e) => UploadPhase::Failed(e),
                };
            });
        });
        true
    }

    /// Stop the upload under way: the request is aborted at the next look at the flag.
    pub fn cancel_upload(&self) {
        if let Some(cancel) = lock(&self.0.uploads).cancel.as_ref() {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Forget a finished upload's outcome (the window closes); a running one is left alone.
    pub fn reset_upload(&self) {
        self.set_upload(|u| {
            if !u.phase.is_active() {
                u.phase = UploadPhase::Idle;
                u.progress.restart(0);
            }
        });
    }

    async fn upload(&self, job: UploadJob, cancel: &Arc<AtomicBool>) -> Result<UploadedProject, UploadError> {
        let UploadJob { encoded, name, company_id, .. } = job;
        if encoded.len() as u64 > upload::max_encoded_len() {
            return Err(UploadError::DocumentTooLarge);
        }
        let bytes = base64::engine::general_purpose::STANDARD.decode(encoded.as_bytes()).map_err(|_| UploadError::Local("The document's data couldn't be read.".into()))?;
        drop(encoded);
        let length = bytes.len() as u64;
        if length == 0 {
            return Err(UploadError::Local("The document is empty.".into()));
        }
        if length > MAX_UPLOAD_BYTES {
            return Err(UploadError::DocumentTooLarge);
        }
        let boundary = pkce::new_verifier().map_err(UploadError::Local)?;
        let form = Multipart::new(&boundary, &name, company_id);
        let mut body = Vec::with_capacity(form.length(length) as usize);
        form.reader(Cursor::new(bytes), length, Progress::default(), cancel.clone())
            .read_to_end(&mut body)
            .map_err(|e| UploadError::Local(format!("The upload couldn't be prepared ({e}).")))?;
        let body = Arc::new(body);
        if cancel.load(Ordering::Relaxed) {
            return Err(UploadError::Cancelled);
        }
        let progress = lock(&self.0.uploads).progress.clone();
        progress.restart(body.len() as u64);
        self.set_upload(|u| u.phase = UploadPhase::Uploading);
        let content_type = form.content_type().to_string();
        let answer = self
            .with_token(|token| {
                let (base, content_type, body, progress, cancel) = (self.0.base.clone(), content_type.clone(), body.clone(), progress.clone(), cancel.clone());
                async move {
                    progress.restart(body.len() as u64);
                    send_upload(&format!("{base}/{}", upload::UPLOAD_PATH), &token, &content_type, &body, &progress, &cancel).await
                }
            })
            .await;
        match answer {
            Ok((status, text)) => upload::parse_uploaded(status, &text),
            Err(_) if cancel.load(Ordering::Relaxed) => Err(UploadError::Cancelled),
            Err(PortalError::Unauthorized(_)) => Err(UploadError::Unauthorized),
            Err(PortalError::Network(m)) => Err(UploadError::Network(m)),
            Err(PortalError::Rejected { status, message }) => Err(UploadError::Server { status, message }),
            Err(PortalError::BadResponse(m)) => Err(UploadError::BadResponse(m)),
        }
    }
}

/// `POST` the multipart `body` with an `XMLHttpRequest` (it reports how much has gone out, which `fetch` doesn't). A `401` is
/// [`PortalError::Unauthorized`]; any other status is the caller's to read.
async fn send_upload(url: &str, token: &str, content_type: &str, body: &[u8], progress: &Progress, cancel: &Arc<AtomicBool>) -> Result<(u16, String), PortalError> {
    let xhr = XmlHttpRequest::new().map_err(|_| network("no XMLHttpRequest"))?;
    xhr.open_with_async("POST", url, true).map_err(|_| network("request"))?;
    xhr.set_request_header("Authorization", &format!("Bearer {token}")).map_err(|_| network("request"))?;
    xhr.set_request_header("Content-Type", content_type).map_err(|_| network("request"))?;
    xhr.set_request_header("X-Language", "en").map_err(|_| network("request"))?;
    xhr.set_with_credentials(false);
    xhr.set_timeout(UPLOAD_TIMEOUT_MS);

    let sent = progress.clone();
    let on_progress = Closure::<dyn FnMut(ProgressEvent)>::new(move |event: ProgressEvent| sent.set_sent(event.loaded() as u64));
    if let Ok(upload) = xhr.upload() {
        upload.set_onprogress(Some(on_progress.as_ref().unchecked_ref()));
    }

    let target = xhr.clone();
    let settled = Promise::new(&mut |resolve: Function, reject: Function| {
        target.set_onload(Some(&resolve));
        target.set_onerror(Some(&reject));
        target.set_ontimeout(Some(&reject));
        target.set_onabort(Some(&reject));
    });

    // The Cancel flag is looked at on a timer: the page's window has no other way to reach a request under way.
    let (watched, flag) = (xhr.clone(), cancel.clone());
    let watcher = Closure::<dyn FnMut()>::new(move || {
        if flag.load(Ordering::Relaxed) {
            let _ = watched.abort();
        }
    });
    let timer = web_sys::window().and_then(|w| w.set_interval_with_callback_and_timeout_and_arguments_0(watcher.as_ref().unchecked_ref(), CANCEL_POLL_MS).ok());

    let started = xhr.send_with_opt_u8_array(Some(body));
    let outcome = match started {
        Ok(()) => JsFuture::from(settled).await,
        Err(e) => Err(e),
    };
    if let (Some(id), Some(window)) = (timer, web_sys::window()) {
        window.clear_interval_with_handle(id);
    }
    drop(watcher);
    drop(on_progress);

    outcome.map_err(|_| network("the connection broke"))?;
    let status = xhr.status().map_err(|_| network("answer"))?;
    if status == 401 {
        return Err(PortalError::Unauthorized("Your session ended.".into()));
    }
    let text = xhr.response_text().ok().flatten().unwrap_or_default();
    Ok((status, text))
}

/// Receive the portal page's `message`s: only its own origin, the right type and well-formed values reach `answers`.
fn listen(portal_origin: String, answers: Arc<Mutex<VecDeque<webflow::Callback>>>, wake: Arc<dyn Fn() + Send + Sync>) {
    let Some(window) = web_sys::window() else { return };
    let handler = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
        let data = event.data();
        let field = |key: &str| Reflect::get(&data, &JsValue::from_str(key)).ok().and_then(|v| v.as_string());
        let (Some(kind), Some(code), Some(state)) = (field("type"), field("code"), field("state")) else { return };
        let Some(answer) = webflow::parse_callback(&event.origin(), &portal_origin, &kind, &code, &state) else { return };
        let mut queue = lock(&answers);
        if queue.len() < MAX_ANSWERS {
            queue.push_back(answer);
        }
        drop(queue);
        wake();
    });
    if window.add_event_listener_with_callback("message", handler.as_ref().unchecked_ref()).is_ok() {
        // Lives as long as the page does.
        handler.forget();
    }
}
