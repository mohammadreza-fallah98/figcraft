use crate::pkce::{VERIFIER_BYTES, challenge, is_valid_verifier, new_verifier, verifier_from};

#[test]
fn challenge_matches_the_rfc_7636_vector() {
    assert_eq!(challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
}

#[test]
fn challenge_is_43_url_safe_characters() {
    let c = challenge("anything");
    assert_eq!(c.len(), 43);
    assert!(c.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'), "{c}");
}

#[test]
fn verifier_from_32_bytes_is_valid_and_43_long() {
    let v = verifier_from(&[0xFF; VERIFIER_BYTES]);
    assert_eq!(v.len(), 43);
    assert!(is_valid_verifier(&v));
}

#[test]
fn new_verifiers_differ_and_are_valid() {
    let (a, b) = (new_verifier().unwrap(), new_verifier().unwrap());
    assert_ne!(a, b);
    assert!(is_valid_verifier(&a) && is_valid_verifier(&b));
}

#[test]
fn verifier_validity_bounds() {
    assert!(!is_valid_verifier(&"a".repeat(42)));
    assert!(is_valid_verifier(&"a".repeat(43)));
    assert!(is_valid_verifier(&"a".repeat(128)));
    assert!(!is_valid_verifier(&"a".repeat(129)));
    assert!(!is_valid_verifier(&format!("{}+", "a".repeat(43))));
    assert!(!is_valid_verifier(""));
}
