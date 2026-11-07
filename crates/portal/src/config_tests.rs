use crate::config::{ConfigError, login_page_url, normalize_base_url, resolve_base_url};

#[test]
fn https_addresses_are_kept_without_a_trailing_slash() {
    assert_eq!(normalize_base_url("https://portal.example.com/").unwrap(), "https://portal.example.com");
    assert_eq!(normalize_base_url("  https://portal.example.com:8443/p/  ").unwrap(), "https://portal.example.com:8443/p");
    assert_eq!(normalize_base_url("HTTPS://Portal.Example.com").unwrap(), "https://Portal.Example.com");
}

#[test]
fn http_is_for_this_computer_only() {
    assert_eq!(normalize_base_url("http://localhost:5000").unwrap(), "http://localhost:5000");
    assert_eq!(normalize_base_url("http://127.0.0.1:5000/").unwrap(), "http://127.0.0.1:5000");
    assert_eq!(normalize_base_url("http://[::1]:5000").unwrap(), "http://[::1]:5000");
    assert!(matches!(normalize_base_url("http://portal.example.com"), Err(ConfigError::Invalid(_))));
    assert!(matches!(normalize_base_url("http://localhost.evil.com"), Err(ConfigError::Invalid(_))));
}

#[test]
fn other_shapes_are_refused() {
    assert_eq!(normalize_base_url("   "), Err(ConfigError::NotSet));
    for bad in ["portal.example.com", "ftp://x.com", "https://", "https://u:p@x.com", "https://x.com?a=1", "https://x.com#f", "https://x .com"] {
        assert!(matches!(normalize_base_url(bad), Err(ConfigError::Invalid(_))), "{bad}");
    }
}

#[test]
fn the_environment_wins_over_the_build() {
    assert_eq!(resolve_base_url(Some("https://a.example"), Some("https://b.example")).unwrap(), "https://a.example");
    assert_eq!(resolve_base_url(None, Some("https://b.example")).unwrap(), "https://b.example");
    assert_eq!(resolve_base_url(Some("  "), Some("https://b.example")).unwrap(), "https://b.example");
    assert_eq!(resolve_base_url(None, None), Err(ConfigError::NotSet));
    assert!(matches!(resolve_base_url(Some("nope"), Some("https://b.example")), Err(ConfigError::Invalid(_))));
}

#[test]
fn the_login_page_carries_the_challenge() {
    assert_eq!(login_page_url("https://p.example/x", "abc-_"), "https://p.example/x/app-login?challenge=abc-_");
}

#[test]
fn http_is_accepted_for_private_network_hosts_only() {
    for ok in ["http://10.1.2.3:4000", "http://172.16.0.1", "http://172.31.255.254:80", "http://192.168.10.5:4000/", "http://127.0.0.2:5000"] {
        assert!(normalize_base_url(ok).is_ok(), "{ok} should be accepted");
    }
    for bad in ["http://8.8.8.8", "http://172.15.0.1", "http://172.32.0.1", "http://192.169.1.1", "http://11.0.0.1", "http://[2001:db8::1]:4000", "http://[fd00::1]", "http://192.168.10.5.evil.com"] {
        assert!(matches!(normalize_base_url(bad), Err(ConfigError::Invalid(_))), "{bad} should be refused");
    }
    assert_eq!(normalize_base_url("http://192.168.10.5:4000/").unwrap(), "http://192.168.10.5:4000");
}
