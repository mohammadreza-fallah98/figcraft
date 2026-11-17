//! The browser build's sign-in, the parts that decide things. The page can't receive `figiran://auth`
//! or listen on a loopback port, so it opens the portal's login page in a tab it keeps a handle to
//! (`window.open` without `noopener`); once the person confirms there, the portal's page posts the
//! one-time code back with `window.opener.postMessage`, addressed to this page's origin only. Nothing
//! here calls the browser, so it runs under native tests; `web` does the calls.

use crate::pkce;

/// The `type` of the message the portal's page posts.
pub const MESSAGE_TYPE: &str = "figiran-auth";

/// An access token is renewed when it has less than this left (milliseconds).
pub const RENEW_BEFORE_MS: f64 = 30_000.0;

/// How long a started login waits for the portal's message (milliseconds): the native app's 10 minutes.
pub const LOGIN_WAIT_MS: f64 = 10.0 * 60.0 * 1000.0;

/// The portal's login page for `challenge`, asking for the code to come back to the opener with `state`
/// (both base64url, so nothing needs escaping).
pub fn login_url(base: &str, challenge: &str, state: &str) -> String {
    format!("{base}/app-login?challenge={challenge}&webstate={state}")
}

/// `scheme://host[:port]` of a normalized portal address (what `MessageEvent.origin` is for its pages).
pub fn origin_of(base: &str) -> Option<String> {
    let (scheme, rest) = base.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next().filter(|a| !a.is_empty())?;
    Some(format!("{}://{authority}", scheme.to_ascii_lowercase()))
}

/// The one-time code and the state it came back with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Callback {
    pub code: String,
    pub state: String,
}

/// 43 base64url characters: the shape of a code and of a state (the portal checks the same).
pub fn is_token_shape(value: &str) -> bool {
    value.len() == 43 && value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// A posted message that is the portal's answer, or `None` for anything else on the page (extensions,
/// other windows). Only the portal's own origin, the right type and well-formed values count.
pub fn parse_callback(event_origin: &str, portal_origin: &str, kind: &str, code: &str, state: &str) -> Option<Callback> {
    (event_origin == portal_origin && kind == MESSAGE_TYPE && is_token_shape(code) && is_token_shape(state))
        .then(|| Callback { code: code.to_string(), state: state.to_string() })
}

/// Whether `callback_state` answers the login that was started with `expected_state` `waited_ms` ago:
/// the reason in words when not.
pub fn check_answer(expected_state: &str, callback_state: &str, waited_ms: f64) -> Result<(), &'static str> {
    if !same_secret(expected_state, callback_state) {
        return Err("The login answer doesn't belong to this login. Press Log in to try again.");
    }
    if waited_ms > LOGIN_WAIT_MS {
        return Err("The login took too long. Press Log in to try again.");
    }
    Ok(())
}

/// Equality that doesn't stop at the first difference.
fn same_secret(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// An access token ending at `until_ms` is renewed at `now_ms` (the clock is `Date.now()`).
pub fn needs_renewal(now_ms: f64, until_ms: f64) -> bool {
    until_ms - now_ms <= RENEW_BEFORE_MS
}

/// A random state or verifier from `bytes` (the same 43-character shape as the app's verifier).
pub fn token_from(bytes: &[u8; pkce::VERIFIER_BYTES]) -> String {
    pkce::verifier_from(bytes)
}

#[cfg(test)]
#[path = "webflow_tests.rs"]
mod tests;
