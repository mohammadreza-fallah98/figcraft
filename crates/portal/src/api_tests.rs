use crate::api::{Access, PortalError, is_join_url, parse_login, parse_opened, parse_projects};

const LOGIN: &str = r#"{"isSuccess":true,"data":{"id":7,"roleName":"Member","companyName":"Acme","username":"ada","firstName":"Ada","lastName":"Lovelace","token":"AT","expireTime":3600,"refreshToken":"RT","refreshExpireTime":2592000,"notice":null},"errorMessages":[]}"#;

#[test]
fn reads_a_login() {
    let l = parse_login(200, LOGIN).unwrap();
    assert_eq!((l.token.as_str(), l.refresh_token.as_str(), l.expire_time, l.refresh_expire_time), ("AT", "RT", 3600, 2_592_000));
    assert_eq!((l.username.as_str(), l.company_name.as_str(), l.role_name.as_str()), ("ada", "Acme", "Member"));
    assert_eq!(l.display_name(), "Ada Lovelace");
}

#[test]
fn display_name_falls_back_to_the_user_name() {
    let l = parse_login(200, r#"{"isSuccess":true,"data":{"username":" bob ","firstName":" ","lastName":null,"token":"AT"}}"#).unwrap();
    assert_eq!(l.display_name(), "bob");
    let l = parse_login(200, r#"{"isSuccess":true,"data":{"username":"bob","lastName":"Smith","token":"AT"}}"#).unwrap();
    assert_eq!(l.display_name(), "Smith");
}

#[test]
fn a_login_without_an_access_token_is_refused() {
    assert!(matches!(parse_login(200, r#"{"isSuccess":true,"data":{"username":"x"}}"#), Err(PortalError::BadResponse(_))));
}

#[test]
fn a_401_is_unauthorized_with_the_portals_message() {
    let body = r#"{"isSuccess":false,"data":null,"errorMessages":[{"code":"AppLoginCodeInvalid","message":"Code is invalid or expired."}]}"#;
    assert_eq!(parse_login(401, body), Err(PortalError::Unauthorized("Code is invalid or expired.".into())));
}

#[test]
fn other_failures_keep_the_status_and_message() {
    let body = r#"{"isSuccess":false,"errorMessages":[{"message":"Not found"}]}"#;
    assert_eq!(parse_projects(404, body), Err(PortalError::Rejected { status: 404, message: "Not found".into() }));
    assert_eq!(
        parse_projects(503, "<html>down</html>"),
        Err(PortalError::Rejected { status: 503, message: "The portal answered with status 503.".into() })
    );
}

#[test]
fn a_200_with_a_failed_flag_is_an_error() {
    let body = r#"{"isSuccess":false,"errorMessages":[{"message":"Nope"}]}"#;
    assert_eq!(parse_projects(200, body), Err(PortalError::Rejected { status: 200, message: "Nope".into() }));
    assert!(matches!(parse_projects(200, r#"{"isSuccess":true}"#), Err(PortalError::BadResponse(_))));
    assert!(matches!(parse_projects(200, "not json"), Err(PortalError::BadResponse(_))));
}

#[test]
fn messages_have_no_control_characters_and_are_capped() {
    let long = "x".repeat(500);
    let body = format!(r#"{{"isSuccess":false,"errorMessages":[{{"message":"a\u001bb{long}"}}]}}"#);
    let Err(PortalError::Rejected { message, .. }) = parse_projects(400, &body) else { panic!("expected a rejection") };
    assert_eq!(message.chars().count(), 200);
    assert!(message.starts_with("abx"));
}

#[test]
fn reads_the_project_list() {
    let body = r#"{"isSuccess":true,"data":[{"id":1,"name":"Brand","companyId":3,"companyName":"Acme","access":"Edit","sizeBytes":10,"updatedAt":null},{"id":2,"name":"Site","companyName":"Acme","access":"view"},{"id":3,"name":"X","access":"Owner"}]}"#;
    let p = parse_projects(200, body).unwrap();
    assert_eq!(p.len(), 3);
    assert_eq!((p[0].id, p[0].name.as_str(), p[0].company_name.as_str(), p[0].access.clone()), (1, "Brand", "Acme", Access::Edit));
    assert_eq!(p[1].access, Access::View);
    assert_eq!(p[2].access, Access::Other("Owner".into()));
    assert!(parse_projects(200, r#"{"isSuccess":true,"data":[]}"#).unwrap().is_empty());
}

#[test]
fn reads_an_opened_project() {
    let body =
        r#"{"isSuccess":true,"data":{"joinUrl":"figcraft://join?server=wss://s.example&file=f&token=t","access":"View","projectName":"Brand"}}"#;
    let o = parse_opened(5, 200, body).unwrap();
    assert_eq!((o.project_id, o.project_name.as_str(), o.access.clone()), (5, "Brand", Access::View));
    assert_eq!(o.join_url, "figcraft://join?server=wss://s.example&file=f&token=t");
}

#[test]
fn only_join_links_are_accepted_from_a_project() {
    assert!(is_join_url("figcraft://join?host=1.2.3.4:5&file=f&token=t"));
    assert!(is_join_url("FIGCRAFT://JOIN?x=1"));
    for bad in ["", "https://evil.example", "figcraft://auth?code=x", "figcraft://join", "figcraft://join?a b", "file:///etc/passwd"] {
        assert!(!is_join_url(bad), "{bad}");
    }
    assert!(!is_join_url("\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}"));
    let body = r#"{"isSuccess":true,"data":{"joinUrl":"https://evil.example","access":"Edit","projectName":"P"}}"#;
    assert!(matches!(parse_opened(1, 200, body), Err(PortalError::BadResponse(_))));
}

#[test]
fn opening_a_project_without_access_is_a_404_rejection() {
    let body = r#"{"isSuccess":false,"errorMessages":[{"code":"ProjectAccessNotFound","message":"No access"}]}"#;
    assert_eq!(parse_opened(1, 404, body), Err(PortalError::Rejected { status: 404, message: "No access".into() }));
}
