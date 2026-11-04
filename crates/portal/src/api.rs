//! The portal's JSON, read defensively: every response is `{ isSuccess, data, errorMessages }`
//! (camelCase); a non-2xx answer, a failed flag or a missing `data` is an error with the portal's
//! own message. A join link is only accepted when it is a `figcraft://join` link.

use serde::Deserialize;
use serde::de::DeserializeOwned;

/// Why a portal call didn't give an answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PortalError {
    /// The portal doesn't know this session or code (HTTP 401): sign in again.
    Unauthorized(String),
    /// The portal refused (other statuses, or `isSuccess: false`), with its message.
    Rejected { status: u16, message: String },
    /// The portal couldn't be reached.
    Network(String),
    /// The answer wasn't what the portal's contract says.
    BadResponse(String),
}

impl std::fmt::Display for PortalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PortalError::Unauthorized(m) | PortalError::Network(m) | PortalError::BadResponse(m) => f.write_str(m),
            PortalError::Rejected { message, .. } => f.write_str(message),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Envelope<T> {
    #[serde(default)]
    is_success: bool,
    data: Option<T>,
    #[serde(default)]
    error_messages: Vec<ErrorMessage>,
}

#[derive(Deserialize)]
struct ErrorMessage {
    #[serde(default)]
    message: String,
}

/// A message safe to show: the first one the portal gave, control characters dropped, capped.
fn first_message(messages: &[ErrorMessage], status: u16) -> String {
    let first = messages.iter().map(|m| m.message.as_str()).find(|m| !m.trim().is_empty()).unwrap_or_default();
    let shown: String = first.chars().filter(|c| !c.is_control()).take(200).collect();
    if shown.is_empty() { format!("The portal answered with status {status}.") } else { shown }
}

/// The `data` of a portal answer.
pub fn parse_response<T: DeserializeOwned>(status: u16, body: &str) -> Result<T, PortalError> {
    let envelope: Result<Envelope<T>, _> = serde_json::from_str(body);
    if !(200..300).contains(&status) {
        let message = envelope.map_or_else(|_| format!("The portal answered with status {status}."), |e| first_message(&e.error_messages, status));
        return Err(if status == 401 { PortalError::Unauthorized(message) } else { PortalError::Rejected { status, message } });
    }
    let envelope = envelope.map_err(|e| PortalError::BadResponse(format!("The portal's answer can't be read ({e}).")))?;
    match envelope {
        Envelope { is_success: true, data: Some(data), .. } => Ok(data),
        Envelope { error_messages, .. } if !error_messages.is_empty() => {
            Err(PortalError::Rejected { status, message: first_message(&error_messages, status) })
        }
        _ => Err(PortalError::BadResponse("The portal's answer has no data.".into())),
    }
}

/// A portal answer that carries nothing: any 2xx is success.
pub fn parse_ack(status: u16, body: &str) -> Result<(), PortalError> {
    if (200..300).contains(&status) {
        return Ok(());
    }
    parse_response::<serde_json::Value>(status, body).map(|_| ())
}

/// What `POST app/token` and `POST users/refresh-token` give (the fields used).
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Login {
    pub username: String,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub role_name: String,
    pub company_name: String,
    pub token: String,
    /// Seconds the access token lasts.
    pub expire_time: u64,
    pub refresh_token: String,
    /// Seconds the refresh token lasts.
    pub refresh_expire_time: u64,
}

impl Login {
    /// The name to show and sign comments with: first and last name, else the user name.
    pub fn display_name(&self) -> String {
        let parts = [self.first_name.as_deref(), self.last_name.as_deref()];
        let full = parts.iter().flatten().map(|s| s.trim()).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ");
        if full.is_empty() { self.username.trim().to_string() } else { full }
    }
}

/// Read the login the portal gave.
pub fn parse_login(status: u16, body: &str) -> Result<Login, PortalError> {
    let login: Login = parse_response(status, body)?;
    if login.token.is_empty() {
        return Err(PortalError::BadResponse("The portal gave no access token.".into()));
    }
    Ok(login)
}

/// What a person may do in a project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Access {
    View,
    Edit,
    Other(String),
}

impl Access {
    fn from_name(name: &str) -> Self {
        match name.trim().to_ascii_lowercase().as_str() {
            "view" => Access::View,
            "edit" => Access::Edit,
            _ => Access::Other(name.trim().to_string()),
        }
    }
}

/// One of the person's projects.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectEntry {
    pub id: i64,
    pub name: String,
    pub company_name: String,
    pub access: Access,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawProject {
    id: i64,
    name: String,
    company_name: String,
    access: String,
}

/// Read the project list.
pub fn parse_projects(status: u16, body: &str) -> Result<Vec<ProjectEntry>, PortalError> {
    let raw: Vec<RawProject> = parse_response(status, body)?;
    Ok(raw.into_iter().map(|p| ProjectEntry { id: p.id, name: p.name, company_name: p.company_name, access: Access::from_name(&p.access) }).collect())
}

/// A project opened: the link to join its file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenedProject {
    pub project_id: i64,
    pub project_name: String,
    pub access: Access,
    /// `figcraft://join?…`, carrying the person's own link token: it is never shown or logged.
    pub join_url: String,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawOpened {
    join_url: String,
    access: String,
    project_name: String,
}

/// `url` is a `figcraft://join?…` link (the only thing opening a project may hand to the app).
pub fn is_join_url(url: &str) -> bool {
    url.get(..16).is_some_and(|s| s.eq_ignore_ascii_case("figcraft://join?")) && !url.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// Read the answer to opening project `project_id`.
pub fn parse_opened(project_id: i64, status: u16, body: &str) -> Result<OpenedProject, PortalError> {
    let raw: RawOpened = parse_response(status, body)?;
    if !is_join_url(&raw.join_url) {
        return Err(PortalError::BadResponse("The portal gave no join link for this project.".into()));
    }
    Ok(OpenedProject { project_id, project_name: raw.project_name, access: Access::from_name(&raw.access), join_url: raw.join_url })
}
