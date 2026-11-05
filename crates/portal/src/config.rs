//! Where the portal is: the address comes from the `FIGCRAFT_PORTAL_URL` environment variable, else
//! from the value a build baked in (`FIGCRAFT_PORTAL_URL` at compile time); with neither, signing in
//! is off. Nothing about the host is written in the code.

/// The environment variable (and build-time variable) holding the portal's address.
pub const ENV_BASE_URL: &str = "FIGCRAFT_PORTAL_URL";

/// The address a build baked in.
pub const BUILD_BASE_URL: Option<&str> = option_env!("FIGCRAFT_PORTAL_URL");

/// Why an address can't be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigError {
    /// Nothing is configured.
    NotSet,
    /// What is configured isn't an `https://` address (`http://` is accepted for this computer only).
    Invalid(String),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::NotSet => write!(f, "the portal address isn't set ({ENV_BASE_URL})"),
            ConfigError::Invalid(why) => write!(f, "the portal address {why} ({ENV_BASE_URL})"),
        }
    }
}

/// `value` as a base address: `https://host[:port][/path]` without a trailing slash, query,
/// fragment or credentials. `http://` is only accepted for `localhost`, `127.0.0.1` and `[::1]`.
pub fn normalize_base_url(value: &str) -> Result<String, ConfigError> {
    let invalid = |why: &str| ConfigError::Invalid(why.into());
    let value = value.trim();
    if value.is_empty() {
        return Err(ConfigError::NotSet);
    }
    if value.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(invalid("has spaces or control characters"));
    }
    let (scheme, rest) = value.split_once("://").ok_or_else(|| invalid("isn't an https:// address"))?;
    let scheme = scheme.to_ascii_lowercase();
    if rest.contains(['?', '#']) {
        return Err(invalid("must not have a query or fragment"));
    }
    let (authority, path) = rest.find('/').map_or((rest, ""), |i| rest.split_at(i));
    if authority.is_empty() || authority.contains('@') {
        return Err(invalid("has no host or has credentials in it"));
    }
    let host = if let Some(v6) = authority.strip_prefix('[') {
        v6.split_once(']').map_or(authority, |(h, _)| h)
    } else {
        authority.rsplit_once(':').map_or(authority, |(h, _)| h)
    };
    let local = matches!(host.to_ascii_lowercase().as_str(), "localhost" | "127.0.0.1" | "::1");
    match scheme.as_str() {
        "https" => {}
        "http" if local => {}
        "http" => return Err(invalid("must use https:// (http:// is only for this computer)")),
        _ => return Err(invalid("isn't an https:// address")),
    }
    Ok(format!("{scheme}://{authority}{}", path.trim_end_matches('/')))
}

/// The configured address: `env` over `built`, normalized.
pub fn resolve_base_url(env: Option<&str>, built: Option<&str>) -> Result<String, ConfigError> {
    fn pick(v: Option<&str>) -> Option<&str> {
        v.map(str::trim).filter(|v| !v.is_empty())
    }
    normalize_base_url(pick(env).or_else(|| pick(built)).ok_or(ConfigError::NotSet)?)
}

/// The portal's address on this computer ([`ENV_BASE_URL`], else the build's).
pub fn base_url() -> Result<String, ConfigError> {
    resolve_base_url(std::env::var(ENV_BASE_URL).ok().as_deref(), BUILD_BASE_URL)
}

/// The portal's login page for `challenge` (a base64url value, so nothing in it needs escaping).
pub fn login_page_url(base: &str, challenge: &str) -> String {
    format!("{base}/app-login?challenge={challenge}")
}

/// The login page for `challenge` when the app listens on `127.0.0.1:port` for the callback that
/// carries `state` (all base64url or digits, so nothing needs escaping).
pub fn login_page_url_loopback(base: &str, challenge: &str, port: u16, state: &str) -> String {
    format!("{}&port={port}&state={state}", login_page_url(base, challenge))
}
