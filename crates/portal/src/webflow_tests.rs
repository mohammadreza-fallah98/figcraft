use super::*;

const CODE: &str = "Q123456789012345678901234567890123456789012";
const STATE: &str = "S123456789012345678901234567890123456789012";

#[test]
fn login_url_carries_the_challenge_and_the_webstate() {
    assert_eq!(login_url("http://192.168.10.5:4000", "abc", "def"), "http://192.168.10.5:4000/app-login?challenge=abc&webstate=def");
}

#[test]
fn origin_is_scheme_and_authority_only() {
    assert_eq!(origin_of("http://192.168.10.5:4000").as_deref(), Some("http://192.168.10.5:4000"));
    assert_eq!(origin_of("HTTPS://portal.example.com/sub/path").as_deref(), Some("https://portal.example.com"));
    assert_eq!(origin_of("not a url"), None);
    assert_eq!(origin_of("http://"), None);
}

#[test]
fn token_shape_is_43_base64url_characters() {
    assert!(is_token_shape(CODE));
    assert!(!is_token_shape(&CODE[..42]));
    assert!(!is_token_shape(&format!("{CODE}x")));
    assert!(!is_token_shape(&format!("{}+", &CODE[..42])));
    assert!(!is_token_shape(""));
}

#[test]
fn callback_needs_the_portal_origin_the_type_and_well_formed_values() {
    let origin = "http://192.168.10.5:4000";
    assert_eq!(parse_callback(origin, origin, MESSAGE_TYPE, CODE, STATE), Some(Callback { code: CODE.into(), state: STATE.into() }));
    assert_eq!(parse_callback("http://evil.test", origin, MESSAGE_TYPE, CODE, STATE), None);
    assert_eq!(parse_callback("http://192.168.10.5:4001", origin, MESSAGE_TYPE, CODE, STATE), None);
    assert_eq!(parse_callback(origin, origin, "other", CODE, STATE), None);
    assert_eq!(parse_callback(origin, origin, MESSAGE_TYPE, "short", STATE), None);
    assert_eq!(parse_callback(origin, origin, MESSAGE_TYPE, CODE, "short"), None);
}

#[test]
fn answer_must_carry_this_logins_state_and_arrive_in_time() {
    assert_eq!(check_answer(STATE, STATE, 1_000.0), Ok(()));
    assert!(check_answer(STATE, CODE, 1_000.0).is_err());
    assert!(check_answer(STATE, STATE, LOGIN_WAIT_MS + 1.0).is_err());
    assert_eq!(check_answer(STATE, STATE, LOGIN_WAIT_MS), Ok(()));
}

#[test]
fn renewal_starts_thirty_seconds_before_the_end() {
    assert!(!needs_renewal(0.0, 31_000.0));
    assert!(needs_renewal(0.0, 30_000.0));
    assert!(needs_renewal(100_000.0, 50_000.0));
}

#[test]
fn a_token_made_from_random_bytes_has_the_expected_shape() {
    assert!(is_token_shape(&token_from(&[7u8; pkce::VERIFIER_BYTES])));
}
