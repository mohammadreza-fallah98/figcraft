//! The link the browser hands back: `figcraft://auth?code=<one-time code>`. The system gives the
//! app any `figcraft:` link that is followed, so this only accepts the exact shape and a code made
//! of the characters the portal issues (base64url); anything else is refused, never guessed at.

/// What `figcraft://auth` carried.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthLink {
    /// The one-time code to exchange (with the verifier) for a session.
    Code(String),
}

/// Why a link isn't a usable auth callback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkError {
    /// A `figcraft:` link for something else (`figcraft://join?…`) or not a link at all.
    NotAuthLink,
    /// `figcraft://auth` without a `code`.
    MissingCode,
    /// A `code` that is empty, too long or has characters the portal never issues.
    BadCode,
}

/// The longest code accepted (the portal's are 43 characters).
const MAX_CODE: usize = 256;

/// The text after `figcraft://auth` when `link` is an auth link (`figcraft:` in any case).
fn auth_rest(link: &str) -> Option<&str> {
    let link = link.trim();
    let rest = link.get(..9).filter(|s| s.eq_ignore_ascii_case("figcraft:")).and(link.get(9..))?;
    let rest = rest.strip_prefix("//")?;
    let rest = rest.get(..4).filter(|s| s.eq_ignore_ascii_case("auth")).and(rest.get(4..))?;
    matches!(rest.chars().next(), None | Some('?' | '/' | '#')).then_some(rest)
}

/// `link` is a `figcraft://auth` link (with or without a usable code).
pub fn is_auth_link(link: &str) -> bool {
    auth_rest(link).is_some()
}

/// `%XX`-decode one query component (`+` is left as it is); `None` for a bad escape or invalid UTF-8.
fn decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        if b == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Read an auth callback link.
pub fn parse_auth_link(link: &str) -> Result<AuthLink, LinkError> {
    let rest = auth_rest(link).ok_or(LinkError::NotAuthLink)?;
    let rest = rest.split('#').next().unwrap_or_default();
    let query = rest.strip_prefix('?').or_else(|| rest.strip_prefix("/?")).unwrap_or_default();
    let code = query.split('&').filter_map(|kv| kv.split_once('=')).find(|(k, _)| *k == "code").map(|(_, v)| v).ok_or(LinkError::MissingCode)?;
    let code = decode(code).ok_or(LinkError::BadCode)?;
    let valid = !code.is_empty() && code.len() <= MAX_CODE && code.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if valid { Ok(AuthLink::Code(code)) } else { Err(LinkError::BadCode) }
}
