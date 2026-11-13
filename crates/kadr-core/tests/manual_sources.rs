use kadr_core::source::{ManualSource, SourceError, SourceKind};
use std::path::PathBuf;

const HEX: &str = "0123456789abcdef0123456789abcdef01234567";

#[test]
fn http_validation_precedes_lenient_url_normalization() {
    for input in [
        "",
        "http:example.com",
        "http:/example.com",
        "http:///example.com",
        "http:////example.com",
        "http://",
        "https://?query",
        "https://[::1",
        "https://example.com:99999",
        "https://example.com:",
        "https://exam ple.com",
        " https://example.com",
        "https://example.com\n",
        "https:\\\\example.com",
        "https://example.com/%",
        "https://example.com/%0",
        "https://example.com/%gg",
        "https://example.com/\0",
        "file:///private/movie.mp4",
        "ftp://example.com/movie",
        "data:text/plain,hello",
        "/private/input.torrent",
    ] {
        assert!(ManualSource::parse_link(input).is_err(), "{input:?}");
    }
}

#[test]
fn rejects_even_empty_credentials() {
    for input in [
        "https://username:password@example.com/video?token=private",
        "https://@example.com/",
        "http://user@example.com/",
    ] {
        assert_eq!(
            ManualSource::parse_link(input).unwrap_err(),
            SourceError::CredentialsNotAllowed
        );
    }
}

#[test]
fn valid_http_and_versioned_resource_identity() {
    let first =
        ManualSource::parse_link("HTTPS://EXAMPLE.COM:443/a/../movie.mp4?token=abc#chapter")
            .unwrap();
    let second = ManualSource::parse_link("https://example.com/movie.mp4?token=abc").unwrap();
    assert_eq!(first.kind(), SourceKind::Http);
    assert_eq!(first.id(), second.id());
    assert_eq!(
        first.http_url().unwrap().as_str(),
        "https://example.com/movie.mp4?token=abc"
    );
    assert!(first.id().as_str().starts_with("source:v1:http:"));
    assert_ne!(
        first.id(),
        ManualSource::parse_link("https://example.com/movie.mp4?token=def")
            .unwrap()
            .id()
    );
    for url in [
        "http://127.0.0.1:1234/video",
        "http://[::1]/",
        "https://例え.テスト/a%20b.mp4",
    ] {
        assert!(ManualSource::parse_link(url).is_ok());
    }
}

#[test]
fn magnet_identity_ignores_trackers_names_and_hash_encoding() {
    let first = ManualSource::parse_link(&format!(
        "magnet:?xt=urn:btih:{HEX}&dn=private&tr=https%3A%2F%2Fprivate.test"
    ))
    .unwrap();
    let second =
        ManualSource::parse_link(&format!("MAGNET:?xt=urn%3Abtih%3A{}", HEX.to_uppercase()))
            .unwrap();
    assert_eq!(first.id(), second.id());
    let mixed_base32 =
        ManualSource::parse_link("magnet:?xt=urn:btih:AERUKZ4JVPG66AJDIVTYTK6N54ASGRLH").unwrap();
    assert_eq!(first.id(), mixed_base32.id());
    assert_eq!(first.kind(), SourceKind::Magnet);
    assert!(first.magnet_uri().unwrap().contains("dn=private"));
    let hex =
        ManualSource::parse_link("magnet:?xt=urn:btih:0000000000000000000000000000000000000000")
            .unwrap();
    let base32 =
        ManualSource::parse_link("magnet:?xt=urn:btih:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();
    assert_eq!(hex.id(), base32.id());
    // Non-zero base32 vector guards bit packing, not just alphabet/length.
    let all_ff =
        ManualSource::parse_link("magnet:?xt=urn:btih:ffffffffffffffffffffffffffffffffffffffff")
            .unwrap();
    let base32_ff =
        ManualSource::parse_link("magnet:?xt=urn:btih:77777777777777777777777777777777").unwrap();
    assert_eq!(all_ff.id(), base32_ff.id());
}

#[test]
fn magnet_rejects_bad_hashes_and_conflicting_exact_topics() {
    for input in [
        "magnet:",
        "magnet:?dn=movie",
        "magnet://host?xt=urn:btih:bad",
        "magnet://?xt=urn:btih:0000000000000000000000000000000000000000",
        "magnet:content?xt=urn:btih:bad",
        "magnet:?xt=urn:btih:abc",
        "magnet:?xt=urn:btih:000000000000000000000000000000000000000g",
        "magnet:?xt=urn:btih:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA1",
        "magnet:?xt=urn:btih:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        "magnet:?xt=urn:btmh:1220abc",
        "magnet:?xt=urn:btmh:1120abc",
        "magnet:?xt=%ZZ",
        "magnet:?xt=urn:btih:abc#secret",
    ] {
        assert!(ManualSource::parse_link(input).is_err(), "{input}");
    }
    let input =
        format!("magnet:?xt=urn:btih:{HEX}&xt=urn:btih:0000000000000000000000000000000000000000");
    assert_eq!(
        ManualSource::parse_link(&input).unwrap_err(),
        SourceError::ConflictingInfoHashes
    );
    let duplicate = format!("magnet:?xt=urn:btih:{HEX}&xt=urn:btih:{HEX}");
    assert!(ManualSource::parse_link(&duplicate).is_ok());
}

#[test]
fn v2_and_hybrid_magnets() {
    let v2 = format!("urn:btmh:1220{}", "ab".repeat(32));
    let source = ManualSource::parse_link(&format!("magnet:?xt={v2}")).unwrap();
    assert!(source.id().as_str().starts_with("source:v1:btmh:"));
    let hybrid = ManualSource::parse_link(&format!("magnet:?xt={v2}&xt=urn:btih:{HEX}")).unwrap();
    assert_eq!(
        hybrid.id(),
        ManualSource::parse_link(&format!("magnet:?xt=urn:btih:{HEX}"))
            .unwrap()
            .id()
    );
}

#[test]
fn local_torrent_requires_explicit_absolute_path_but_never_reads_it() {
    for path in [
        "relative.torrent",
        "/tmp/input.mp4",
        "/tmp/input.torrent/",
        "/tmp/../input.torrent",
        "/tmp/\0.torrent",
    ] {
        assert!(
            ManualSource::torrent_file(PathBuf::from(path)).is_err(),
            "{path:?}"
        );
    }
    let source =
        ManualSource::torrent_file(PathBuf::from("/nonexistent/private/file.TORRENT")).unwrap();
    assert_eq!(source.kind(), SourceKind::TorrentFile);
    assert_eq!(
        source.torrent_path().unwrap(),
        std::path::Path::new("/nonexistent/private/file.TORRENT")
    );
    let normalized =
        ManualSource::torrent_file(PathBuf::from("/nonexistent/./private/file.TORRENT")).unwrap();
    assert_eq!(source.id(), normalized.id());
}

#[test]
fn diagnostics_never_expose_input() {
    let secret = "do-not-log-this";
    let inputs = [
        ManualSource::parse_link(&format!("https://private.test/{secret}?token={secret}")).unwrap(),
        ManualSource::parse_link(&format!("magnet:?xt=urn:btih:{HEX}&dn={secret}")).unwrap(),
        ManualSource::torrent_file(PathBuf::from(format!("/private/{secret}.torrent"))).unwrap(),
    ];
    for input in inputs {
        let diagnostic = format!("{input:?} {:?}", input.id());
        assert!(!diagnostic.contains(secret));
        assert!(!diagnostic.contains("private.test"));
        assert!(!diagnostic.contains(HEX));
    }
    let error =
        ManualSource::parse_link(&format!("https://{secret}:password@private.test")).unwrap_err();
    assert!(!format!("{error} {error:?}").contains(secret));
    assert!(std::error::Error::source(&error).is_none());
}

#[test]
fn source_id_storage_roundtrip_and_frozen_v1_vectors() {
    use kadr_core::source::SourceId;
    let http = ManualSource::parse_link("https://example.com/movie.mp4?token=abc")
        .unwrap()
        .id();
    assert_eq!(
        http.as_str(),
        "source:v1:http:515198676972f0cbe2ab9dc4084b7368407939a9d3ae601506b099e00d54b25b"
    );
    let magnet = ManualSource::parse_link(&format!("magnet:?xt=urn:btih:{HEX}"))
        .unwrap()
        .id();
    assert_eq!(
        magnet.as_str(),
        "source:v1:btih:592b03985cbaf03c8b0db8d228961edc53a762a4261178f379930ab3e666eed5"
    );
    assert_eq!(http.as_str().parse::<SourceId>().unwrap(), http);
    for bad in [
        "",
        "https://private.test",
        "source:v2:http:abc",
        "source:v1:unknown:abc",
        "source:v1:http:abc",
    ] {
        assert!(bad.parse::<SourceId>().is_err());
    }
    assert!(
        format!("source:v1:http:{}", "A".repeat(64))
            .parse::<SourceId>()
            .is_err()
    );
}

#[cfg(unix)]
#[test]
fn non_utf8_local_paths_are_not_lossily_conflated() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};
    let first = ManualSource::torrent_file(PathBuf::from(OsString::from_vec(
        b"/private/\xff.torrent".to_vec(),
    )))
    .unwrap();
    let second = ManualSource::torrent_file(PathBuf::from(OsString::from_vec(
        b"/private/\xfe.torrent".to_vec(),
    )))
    .unwrap();
    assert_ne!(first.id(), second.id());
    assert_eq!(
        first.torrent_path().unwrap().as_os_str().as_encoded_bytes(),
        b"/private/\xff.torrent"
    );
}
