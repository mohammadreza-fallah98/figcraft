//! The verifier the app keeps and the challenge the portal is given: `challenge =
//! base64url(SHA-256(verifier))`, both unpadded (RFC 7636's S256), as the portal checks them
//! (43 to 128 characters of `A-Za-z0-9_-` for the verifier, 43 for the challenge).

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

/// Random bytes in a verifier: 32 of them are 43 characters.
pub const VERIFIER_BYTES: usize = 32;

/// The challenge for `verifier`.
pub fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// A verifier made of `bytes`.
pub fn verifier_from(bytes: &[u8; VERIFIER_BYTES]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// A fresh verifier from the system's secure random numbers.
#[cfg(not(target_arch = "wasm32"))]
pub fn new_verifier() -> Result<String, String> {
    let mut bytes = [0u8; VERIFIER_BYTES];
    getrandom::fill(&mut bytes).map_err(|e| format!("no secure random numbers: {e}"))?;
    Ok(verifier_from(&bytes))
}

/// Signing in needs the desktop app (the browser build has no way to get the code back).
#[cfg(target_arch = "wasm32")]
pub fn new_verifier() -> Result<String, String> {
    Err("signing in to a portal needs the desktop app".into())
}

/// `verifier` is something the portal accepts: 43 to 128 characters of `A-Za-z0-9_-`.
pub fn is_valid_verifier(verifier: &str) -> bool {
    (43..=128).contains(&verifier.len()) && verifier.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
