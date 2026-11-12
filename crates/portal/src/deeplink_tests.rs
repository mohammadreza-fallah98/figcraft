use crate::deeplink::{AuthLink, LinkError, is_auth_link, parse_auth_link};

const CODE: &str = "abc_DEF-123";

#[test]
fn reads_the_code() {
    assert_eq!(parse_auth_link(&format!("figiran://auth?code={CODE}")), Ok(AuthLink::Code(CODE.into())));
}

#[test]
fn scheme_and_host_are_case_insensitive_and_whitespace_is_trimmed() {
    assert_eq!(parse_auth_link(&format!("  FigIran://AUTH?code={CODE}\n")), Ok(AuthLink::Code(CODE.into())));
}

#[test]
fn a_trailing_slash_and_other_parameters_are_fine() {
    assert_eq!(parse_auth_link(&format!("figiran://auth/?x=1&code={CODE}&y=2")), Ok(AuthLink::Code(CODE.into())));
}

#[test]
fn a_fragment_is_not_part_of_the_code() {
    assert_eq!(parse_auth_link(&format!("figiran://auth?code={CODE}#frag")), Ok(AuthLink::Code(CODE.into())));
}

#[test]
fn percent_escapes_are_decoded() {
    assert_eq!(parse_auth_link("figiran://auth?code=%61bc"), Ok(AuthLink::Code("abc".into())));
}

#[test]
fn other_figiran_links_are_not_auth_links() {
    assert_eq!(parse_auth_link("figiran://join?host=1.2.3.4:5&file=f&token=t"), Err(LinkError::NotAuthLink));
    assert_eq!(parse_auth_link("figiran://authx?code=abc"), Err(LinkError::NotAuthLink));
    assert_eq!(parse_auth_link("https://auth?code=abc"), Err(LinkError::NotAuthLink));
    assert_eq!(parse_auth_link("figiran:auth?code=abc"), Err(LinkError::NotAuthLink));
    assert_eq!(parse_auth_link(""), Err(LinkError::NotAuthLink));
    assert!(!is_auth_link("figiran://join?x=1"));
    assert!(is_auth_link("figiran://auth"));
    assert!(is_auth_link("figiran://auth?code=abc"));
}

#[test]
fn a_missing_code_is_refused() {
    assert_eq!(parse_auth_link("figiran://auth"), Err(LinkError::MissingCode));
    assert_eq!(parse_auth_link("figiran://auth?state=1"), Err(LinkError::MissingCode));
}

#[test]
fn bad_codes_are_refused() {
    for bad in ["", "a b", "a%20b", "a%zz", "a%2", "a.b", "a/b", "%ff%fe"] {
        assert_eq!(parse_auth_link(&format!("figiran://auth?code={bad}")), Err(LinkError::BadCode), "{bad:?}");
    }
    let long = "a".repeat(257);
    assert_eq!(parse_auth_link(&format!("figiran://auth?code={long}")), Err(LinkError::BadCode));
    assert!(parse_auth_link(&format!("figiran://auth?code={}", "a".repeat(256))).is_ok());
}

#[test]
fn non_ascii_input_does_not_panic() {
    assert_eq!(parse_auth_link("figiran:\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}"), Err(LinkError::NotAuthLink));
    assert_eq!(parse_auth_link("\u{e9}figiran://auth?code=a"), Err(LinkError::NotAuthLink));
}
