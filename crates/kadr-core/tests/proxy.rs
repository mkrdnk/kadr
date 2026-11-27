use kadr_core::proxy::is_valid_endpoint;

#[test]
fn accepts_only_hosts_with_optional_nonzero_ports_and_root_slash() {
    for scheme in ["http", "https", "socks5", "socks5h"] {
        for host in [
            "proxy.example",
            "localhost",
            "127.0.0.1",
            "0.0.0.0",
            "[::1]",
            "[2001:db8::1]",
        ] {
            for port in ["", ":1", ":1080", ":65535"] {
                for path in ["", "/"] {
                    assert!(is_valid_endpoint(&format!("{scheme}://{host}{port}{path}")));
                }
            }
        }
    }
}

#[test]
fn rejects_credentials_even_when_userinfo_is_empty() {
    for scheme in ["http", "https", "socks5", "socks5h"] {
        for authority in [
            "@localhost",
            ":@localhost",
            "user@localhost",
            ":password@localhost",
            "user:password@localhost",
            "user%40name@localhost",
            "user@@localhost",
        ] {
            assert!(!is_valid_endpoint(&format!("{scheme}://{authority}:1080")));
        }
    }
}

#[test]
fn rejects_resources_whitespace_and_malformed_authorities_without_url_repairs() {
    for scheme in ["http", "https", "socks5", "socks5h"] {
        for address in [
            "",
            "/",
            "/localhost",
            "localhost//",
            "localhost/path",
            "localhost/.",
            "localhost/..",
            "localhost/%2e",
            "localhost?",
            "localhost?query",
            "localhost/#",
            "localhost#fragment",
            "localhost:",
            "localhost:0",
            "localhost:65536",
            "localhost:-1",
            "localhost:+80",
            "localhost:abc",
            "localhost:80:90",
            ":1080",
            "::1",
            "::1:1080",
            "[]",
            "[::1",
            "[::1]extra",
            "[::1]:",
            "[::1]:0",
            "[::1]:65536",
            "[localhost]:1080",
            "[::1%25eth0]:1080",
            "local\\host",
            "localhost\\",
            "localhost:80\\path",
            "localhost ",
            "local host",
            "localhost\n",
            "local\thost",
            "local\rhost",
            "local\0host",
            "local\u{7f}host",
            "localhost\u{a0}",
            "local\u{2003}host",
            "%00",
            "%20",
            "local%2fhost",
            "local%40host",
            "256.256.256.256",
        ] {
            assert!(!is_valid_endpoint(&format!("{scheme}://{address}")));
        }
    }
    for endpoint in [
        "",
        "localhost:1080",
        "//localhost:1080",
        "http:localhost",
        "http:/localhost",
        "ftp://localhost",
        "socks4://localhost",
        "socks://localhost",
        " https://localhost",
        "http\n://localhost",
    ] {
        assert!(!is_valid_endpoint(endpoint));
    }
}
