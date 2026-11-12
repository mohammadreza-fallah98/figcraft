//! Upload the open document to the portal as a new project (an Admin or a CompanyAdmin).
//!
//! The app hands over the document exactly as its own Save as writes it (the native `.figcraft`
//! archive, as base64); a background thread decodes it into a temporary file, sends it as a
//! `multipart/form-data` body (`file`, `name`, and `companyId` for an Admin) to
//! `POST api/v1/projects/upload` with the person's access token, and deletes the temporary file
//! however it ends. The portal answers `202`: the project exists with the status `Importing` and
//! the portal finishes it in the background. The window only reads [`Portal::upload_view`], so
//! nothing here blocks it. The publish key of the portal's file server is never involved: the
//! portal owns it.

use std::fs::{File, OpenOptions};
use std::io::{self, Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use base64::Engine as _;
use serde::Deserialize;

use crate::api::{self, PortalError};
use crate::session::{Portal, lock};

/// The largest document the portal takes (its `FigcraftServer:MaxUploadMb`, 200).
pub const MAX_UPLOAD_BYTES: u64 = 200 << 20;

/// Where the portal takes uploads, and lists the companies, relative to its address.
pub const UPLOAD_PATH: &str = "api/v1/projects/upload";
pub const COMPANIES_PATH: &str = "api/v1/companies?Page=1&PageSize=100";

/// The file name sent with the upload (the portal keeps only its extension).
const UPLOAD_FILE_NAME: &str = "project.figcraft";

/// The portal's limit on a project name.
pub const MAX_NAME_CHARS: usize = 150;

/// Who may upload: the portal's roles of the same names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UploadRole {
    /// Chooses the company.
    Admin,
    /// Always the own company.
    CompanyAdmin,
}

impl UploadRole {
    /// The upload role of the portal role `name`, or `None` for anyone else (a Member).
    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "admin" => Some(UploadRole::Admin),
            "companyadmin" => Some(UploadRole::CompanyAdmin),
            _ => None,
        }
    }
}

/// A company of the portal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Company {
    pub id: i64,
    pub name: String,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawCompany {
    id: i64,
    name: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct CompanyPage {
    items: Vec<RawCompany>,
}

/// Read the company list (`data.items`).
pub fn parse_companies(status: u16, body: &str) -> Result<Vec<Company>, PortalError> {
    let page: CompanyPage = api::parse_response(status, body)?;
    Ok(page.items.into_iter().filter(|c| c.id > 0).map(|c| Company { id: c.id, name: c.name }).collect())
}

/// The project the portal accepted.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct UploadedProject {
    pub id: i64,
    pub name: String,
    /// `Importing` while the portal converts it, then `Ready`.
    pub status: String,
}

/// Why an upload didn't end with an accepted project. The window words each case itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UploadError {
    /// `401`: the session ended.
    Unauthorized,
    /// `403`: this account may not upload (to this company).
    Forbidden,
    /// `400`: the portal refused the file or the name, with its reason.
    Invalid(String),
    /// `409`: a conflict, with the portal's reason.
    Conflict(String),
    /// `413`: the file is larger than the portal takes.
    TooLarge,
    /// Another status (`5xx`, `429`, …).
    Server { status: u16, message: String },
    /// The portal couldn't be reached, or the connection broke.
    Network(String),
    /// The portal's answer wasn't what its contract says.
    BadResponse(String),
    /// The person cancelled.
    Cancelled,
    /// The document is larger than the portal takes (known before sending).
    DocumentTooLarge,
    /// The document couldn't be prepared on this computer.
    Local(String),
}

/// The error for a portal answer that isn't a success.
pub fn error_from_status(status: u16, body: &str) -> UploadError {
    let message = match api::parse_response::<serde_json::Value>(status, body) {
        Err(PortalError::Unauthorized(m) | PortalError::Network(m) | PortalError::BadResponse(m)) => m,
        Err(PortalError::Rejected { message, .. }) => message,
        Ok(_) => format!("The portal answered with status {status}."),
    };
    match status {
        401 => UploadError::Unauthorized,
        403 => UploadError::Forbidden,
        400 => UploadError::Invalid(message),
        409 => UploadError::Conflict(message),
        413 => UploadError::TooLarge,
        _ => UploadError::Server { status, message },
    }
}

/// The accepted project, or why not (`202` is the portal's answer; any 2xx with the project's
/// data is taken).
pub fn parse_uploaded(status: u16, body: &str) -> Result<UploadedProject, UploadError> {
    if !(200..300).contains(&status) {
        return Err(error_from_status(status, body));
    }
    api::parse_response::<UploadedProject>(status, body).map_err(|e| match e {
        PortalError::Rejected { status, message } => UploadError::Server { status, message },
        PortalError::Unauthorized(_) => UploadError::Unauthorized,
        PortalError::Network(m) => UploadError::Network(m),
        PortalError::BadResponse(m) => UploadError::BadResponse(m),
    })
}

/// How much of the file has been sent.
#[derive(Clone, Debug, Default)]
pub struct Progress {
    sent: Arc<AtomicU64>,
    total: Arc<AtomicU64>,
}

impl Progress {
    pub fn sent(&self) -> u64 {
        self.sent.load(Ordering::Relaxed)
    }
    pub fn total(&self) -> u64 {
        self.total.load(Ordering::Relaxed)
    }
    fn add(&self, bytes: u64) {
        self.sent.fetch_add(bytes, Ordering::Relaxed);
    }
    pub(crate) fn restart(&self, total: u64) {
        self.sent.store(0, Ordering::Relaxed);
        self.total.store(total, Ordering::Relaxed);
    }
}

/// The text of a project name as sent: control characters dropped, trimmed, capped.
pub fn clean_name(name: &str) -> String {
    let cleaned: String = name.chars().filter(|c| !c.is_control()).collect();
    cleaned.trim().chars().take(MAX_NAME_CHARS).collect::<String>().trim().to_string()
}

/// A `multipart/form-data` body around a file: the parts before the file, the file, the closing.
pub struct Multipart {
    prefix: Vec<u8>,
    suffix: Vec<u8>,
    content_type: String,
}

impl Multipart {
    /// `boundary` is random per upload (letters, digits, `-`, `_`); `name` is the project name and
    /// `company_id` the company (an Admin's choice).
    pub fn new(boundary: &str, name: &str, company_id: Option<i64>) -> Self {
        let mut prefix = Vec::new();
        let mut field = |label: &str, value: &str| {
            prefix.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{label}\"\r\n\r\n{value}\r\n").as_bytes());
        };
        field("name", &clean_name(name));
        if let Some(id) = company_id {
            field("companyId", &id.to_string());
        }
        prefix.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{UPLOAD_FILE_NAME}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
            )
            .as_bytes(),
        );
        Self { prefix, suffix: format!("\r\n--{boundary}--\r\n").into_bytes(), content_type: format!("multipart/form-data; boundary={boundary}") }
    }

    pub fn content_type(&self) -> &str {
        &self.content_type
    }

    /// The body's length for a file of `file_len` bytes.
    pub fn length(&self, file_len: u64) -> u64 {
        self.prefix.len() as u64 + file_len + self.suffix.len() as u64
    }

    /// The body: reads the parts and `file`'s first `file_len` bytes, counts them in `progress`,
    /// and fails once `cancel` is set.
    pub fn reader<R: Read>(&self, file: R, file_len: u64, progress: Progress, cancel: Arc<AtomicBool>) -> BodyReader<R> {
        BodyReader {
            prefix: Cursor::new(self.prefix.clone()),
            file: file.take(file_len),
            suffix: Cursor::new(self.suffix.clone()),
            stage: Stage::Prefix,
            progress,
            cancel,
        }
    }
}

enum Stage {
    Prefix,
    File,
    Suffix,
}

/// What [`Multipart::reader`] gives.
pub struct BodyReader<R> {
    prefix: Cursor<Vec<u8>>,
    file: io::Take<R>,
    suffix: Cursor<Vec<u8>>,
    stage: Stage,
    progress: Progress,
    cancel: Arc<AtomicBool>,
}

impl<R: Read> Read for BodyReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(io::Error::other("The upload was cancelled."));
        }
        loop {
            let n = match self.stage {
                Stage::Prefix => self.prefix.read(buf)?,
                Stage::File => {
                    let n = self.file.read(buf)?;
                    self.progress.add(n as u64);
                    n
                }
                Stage::Suffix => return self.suffix.read(buf),
            };
            if n > 0 || buf.is_empty() {
                return Ok(n);
            }
            self.stage = match self.stage {
                Stage::Prefix => Stage::File,
                Stage::File | Stage::Suffix => Stage::Suffix,
            };
        }
    }
}

/// A temporary file that is deleted when this is dropped.
pub struct TempFile {
    path: PathBuf,
}

impl TempFile {
    /// Write `bytes` to a new, uniquely named file in `dir` (never over an existing file).
    pub fn create(dir: &Path, bytes: &[u8]) -> Result<Self, UploadError> {
        let unique = crate::pkce::new_verifier().map_err(UploadError::Local)?;
        let path = dir.join(format!("figcraft-upload-{}.figcraft", unique.chars().take(16).collect::<String>()));
        let local = |e: io::Error| UploadError::Local(format!("The temporary file couldn't be written ({e})."));
        let mut file = OpenOptions::new().write(true).create_new(true).open(&path).map_err(local)?;
        let temp = Self { path };
        file.write_all(bytes).and_then(|()| file.flush()).map_err(local)?;
        Ok(temp)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Where an upload is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum UploadPhase {
    #[default]
    Idle,
    /// Decoding the document and writing the temporary file.
    Preparing,
    Uploading,
    /// The portal accepted it (`202`).
    Accepted(UploadedProject),
    Failed(UploadError),
    Cancelled,
}

impl UploadPhase {
    /// Work is going on.
    pub fn is_active(&self) -> bool {
        matches!(self, UploadPhase::Preparing | UploadPhase::Uploading)
    }
}

/// The company list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Companies {
    #[default]
    NotLoaded,
    Loading,
    Loaded(Vec<Company>),
    Failed(String),
}

/// What the window draws.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadView {
    pub phase: UploadPhase,
    pub sent: u64,
    pub total: u64,
    pub companies: Companies,
}

impl UploadView {
    /// How much of the file is sent, 0 to 1.
    pub fn fraction(&self) -> f32 {
        if self.total == 0 { 0.0 } else { (self.sent as f64 / self.total as f64).clamp(0.0, 1.0) as f32 }
    }
}

/// The upload state a [`Portal`] keeps.
#[derive(Default)]
pub struct UploadShared {
    phase: UploadPhase,
    companies: Companies,
    cancel: Option<Arc<AtomicBool>>,
    progress: Progress,
}

/// What to upload.
pub struct UploadJob {
    /// The document as its Save as writes it, base64.
    pub encoded: String,
    pub name: String,
    /// The company (an Admin's choice); a CompanyAdmin's is always their own, the portal's rule.
    pub company_id: Option<i64>,
    /// Where the temporary file goes.
    pub temp_dir: PathBuf,
}

/// The largest base64 text that can hold [`MAX_UPLOAD_BYTES`].
fn max_encoded_len() -> u64 {
    MAX_UPLOAD_BYTES.div_ceil(3) * 4
}

impl Portal {
    fn set_upload(&self, f: impl FnOnce(&mut UploadShared)) {
        f(&mut lock(&self.0.uploads));
        (self.0.wake)();
    }

    pub fn upload_view(&self) -> UploadView {
        let u = lock(&self.0.uploads);
        UploadView { phase: u.phase.clone(), sent: u.progress.sent(), total: u.progress.total(), companies: u.companies.clone() }
    }

    /// Ask for the companies an Admin can upload to, in the background.
    pub fn refresh_companies(&self) {
        self.set_upload(|u| u.companies = Companies::Loading);
        self.run(|me| {
            let result = me.with_token(|api, token| api.companies(token));
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
        self.run(move |me| {
            let result = me.upload(job, &cancel);
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

    /// Stop the upload under way: the connection is dropped at the next chunk.
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

    fn upload(&self, job: UploadJob, cancel: &Arc<AtomicBool>) -> Result<UploadedProject, UploadError> {
        let UploadJob { encoded, name, company_id, temp_dir } = job;
        if encoded.len() as u64 > max_encoded_len() {
            return Err(UploadError::DocumentTooLarge);
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded.as_bytes())
            .map_err(|_| UploadError::Local("The document's data couldn't be read.".into()))?;
        drop(encoded);
        let length = bytes.len() as u64;
        if length == 0 {
            return Err(UploadError::Local("The document is empty.".into()));
        }
        if length > MAX_UPLOAD_BYTES {
            return Err(UploadError::DocumentTooLarge);
        }
        let temp = TempFile::create(&temp_dir, &bytes)?;
        drop(bytes);
        if cancel.load(Ordering::Relaxed) {
            return Err(UploadError::Cancelled);
        }
        let boundary = crate::pkce::new_verifier().map_err(UploadError::Local)?;
        let form = Multipart::new(&boundary, &name, company_id);
        let progress = lock(&self.0.uploads).progress.clone();
        self.set_upload(|u| u.phase = UploadPhase::Uploading);
        let answer = self.with_token(|api, token| {
            let file = File::open(temp.path()).map_err(|e| PortalError::Network(format!("The temporary file couldn't be read ({e}).")))?;
            progress.restart(length);
            let mut body = form.reader(file, length, progress.clone(), cancel.clone());
            api.send_upload(token, form.content_type(), form.length(length), &mut body)
        });
        match answer {
            Ok((status, text)) => parse_uploaded(status, &text),
            Err(_) if cancel.load(Ordering::Relaxed) => Err(UploadError::Cancelled),
            Err(PortalError::Unauthorized(_)) => Err(UploadError::Unauthorized),
            Err(PortalError::Network(m)) => Err(UploadError::Network(m)),
            Err(PortalError::Rejected { status, message }) => Err(UploadError::Server { status, message }),
            Err(PortalError::BadResponse(m)) => Err(UploadError::BadResponse(m)),
        }
    }
}
