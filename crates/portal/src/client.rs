//! The HTTPS side: one blocking request per call (the caller runs them on a background thread).
//! A short timeout bounds every call; bodies are JSON; the access token goes in `Authorization`
//! and nothing secret is ever logged.

use std::io::Read;
use std::time::Duration;

use crate::api::{self, Login, OpenedProject, PortalError, ProjectEntry};
use crate::session::Api;
use crate::upload::{self, Company};

const TIMEOUT: Duration = Duration::from_secs(20);

/// An upload sends up to 200 MB and then waits for the portal to store it.
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// The portal at one base address.
pub struct Client {
    base: String,
    agent: ureq::Agent,
    upload_agent: ureq::Agent,
}

impl Client {
    pub fn new(base: &str) -> Self {
        let config = ureq::Agent::config_builder().timeout_global(Some(TIMEOUT)).http_status_as_error(false).max_redirects(0).build();
        let upload = ureq::Agent::config_builder().timeout_global(Some(UPLOAD_TIMEOUT)).http_status_as_error(false).max_redirects(0).build();
        Self { base: base.trim_end_matches('/').to_string(), agent: config.into(), upload_agent: upload.into() }
    }

    /// `(status, body)` of `method path`; `json` is the body of a POST, `bearer` the access token.
    fn send(&self, method: &str, path: &str, bearer: Option<&str>, json: Option<String>) -> Result<(u16, String), PortalError> {
        let url = format!("{}/{path}", self.base);
        let net = |e: ureq::Error| PortalError::Network(format!("The portal can't be reached ({e})."));
        let auth = bearer.map(|t| format!("Bearer {t}"));
        let result = if method == "GET" {
            let mut req = self.agent.get(&url).header("X-Language", "en");
            if let Some(a) = &auth {
                req = req.header("Authorization", a);
            }
            req.call()
        } else {
            let mut req = self.agent.post(&url).header("X-Language", "en").header("Content-Type", "application/json");
            if let Some(a) = &auth {
                req = req.header("Authorization", a);
            }
            req.send(json.unwrap_or_else(|| "{}".into()))
        };
        let mut response = result.map_err(net)?;
        let status = response.status().as_u16();
        let body = response.body_mut().read_to_string().map_err(net)?;
        Ok((status, body))
    }
}

impl Api for Client {
    fn exchange_code(&self, code: &str, verifier: &str) -> Result<Login, PortalError> {
        let body = serde_json::json!({"code": code, "verifier": verifier}).to_string();
        let (status, text) = self.send("POST", "api/v1/app/token", None, Some(body))?;
        api::parse_login(status, &text)
    }

    fn refresh(&self, refresh_token: &str) -> Result<Login, PortalError> {
        let body = serde_json::json!({"refreshToken": refresh_token}).to_string();
        let (status, text) = self.send("POST", "api/v1/users/refresh-token", None, Some(body))?;
        api::parse_login(status, &text)
    }

    fn projects(&self, access_token: &str) -> Result<Vec<ProjectEntry>, PortalError> {
        let (status, text) = self.send("GET", "api/v1/app/projects", Some(access_token), None)?;
        api::parse_projects(status, &text)
    }

    fn open_project(&self, access_token: &str, project_id: i64) -> Result<OpenedProject, PortalError> {
        let (status, text) = self.send("POST", &format!("api/v1/app/projects/{project_id}/open"), Some(access_token), None)?;
        api::parse_opened(project_id, status, &text)
    }

    fn logout(&self, access_token: &str, refresh_token: &str) -> Result<(), PortalError> {
        let body = serde_json::json!({"refreshToken": refresh_token}).to_string();
        let (status, text) = self.send("POST", "api/v1/app/logout", Some(access_token), Some(body))?;
        api::parse_ack(status, &text)
    }

    fn companies(&self, access_token: &str) -> Result<Vec<Company>, PortalError> {
        let (status, text) = self.send("GET", upload::COMPANIES_PATH, Some(access_token), None)?;
        upload::parse_companies(status, &text)
    }

    fn send_upload(&self, access_token: &str, content_type: &str, length: u64, body: &mut dyn Read) -> Result<(u16, String), PortalError> {
        let url = format!("{}/{}", self.base, upload::UPLOAD_PATH);
        let net = |e: ureq::Error| PortalError::Network(format!("The portal can't be reached ({e})."));
        let request = self
            .upload_agent
            .post(&url)
            .header("X-Language", "en")
            .header("Authorization", format!("Bearer {access_token}"))
            .header("Content-Type", content_type)
            .header("Content-Length", length.to_string());
        let mut response = request.send(ureq::SendBody::from_reader(body)).map_err(net)?;
        let status = response.status().as_u16();
        let text = response.body_mut().read_to_string().map_err(net)?;
        if status == 401 {
            return Err(PortalError::Unauthorized("Your session ended.".into()));
        }
        Ok((status, text))
    }
}
