use super::*;
use crate::LocalSession;
use tokio::time::timeout;

mod fixture {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/mod.rs"));
}

use fixture::tempdir;

fn local_options() -> EngineOptions {
    EngineOptions {
        network_enabled: true,
        discovery_enabled: false,
        upload_limit: RateLimit::Unlimited,
        ..Default::default()
    }
}

fn write_seed(fixture: &fixture::Fixture, output: &Path) {
    fixture.write_partial_seed(output);
    std::fs::write(output.join("kadr-fixture/selected.bin"), &fixture.selected).unwrap();
}

/// Canonical bencode for tiny, locally generated metadata with arbitrary UTF-8
/// path components. One byte per file; no networking or filesystem needed.
fn torrent_with_paths(paths: &[&[&str]]) -> Bytes {
    use sha1::{Digest, Sha1};

    fn string(output: &mut Vec<u8>, value: &[u8]) {
        output.extend_from_slice(format!("{}:", value.len()).as_bytes());
        output.extend_from_slice(value);
    }
    let mut output = b"d4:infod5:filesl".to_vec();
    for path in paths {
        output.extend_from_slice(b"d6:lengthi1e4:pathl");
        for component in *path {
            string(&mut output, component.as_bytes());
        }
        output.extend_from_slice(b"ee");
    }
    output.extend_from_slice(b"e4:name");
    string(&mut output, b"unicode-fixture");
    output.extend_from_slice(b"12:piece lengthi16384e6:pieces");
    string(&mut output, &Sha1::digest(vec![42; paths.len()]));
    output.extend_from_slice(b"7:privatei1eee");
    output.into()
}

async fn read_all(source: &dyn ByteSource) -> Vec<u8> {
    let mut bytes = Vec::new();
    while (bytes.len() as u64) < source.len() {
        let next = timeout(
            Duration::from_secs(10),
            source.read_at(bytes.len() as u64, 64 * 1024),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(!next.is_empty());
        bytes.extend_from_slice(&next);
    }
    bytes
}

#[test]
fn preparation_is_offline_stable_and_private_in_debug() {
    let fixture = fixture::Fixture::new(true);
    let prepared = PreparedTorrent::from_bytes(fixture.torrent.clone()).unwrap();
    assert_eq!(prepared.info_hash.len(), 40);
    assert_eq!(prepared.files.len(), 3);
    assert_eq!(prepared.files[1].len, fixture.selected.len() as u64);
    assert_eq!(
        prepared.total_bytes,
        (fixture::PREFIX + fixture::SELECTED + 3 * fixture::PIECE) as u64
    );
    assert_eq!(
        prepared.info_hash,
        PreparedTorrent::from_bytes(fixture.torrent)
            .unwrap()
            .info_hash
    );
    assert!(!format!("{prepared:?}").contains("selected.bin"));
    let error = PreparedTorrent::from_bytes(Bytes::from_static(b"SENSITIVE.torrent")).unwrap_err();
    assert!(!format!("{error:?} {error:#}").contains("SENSITIVE"));
}

#[tokio::test]
async fn consent_and_cancel_fail_before_files_or_sockets() {
    let fixture = fixture::Fixture::new(true);
    let prepared = PreparedTorrent::from_bytes(fixture.torrent).unwrap();
    let temp = tempdir();
    let root = temp.path().join("not-created");
    let error = prepared
        .start(
            1,
            root.clone(),
            EngineOptions::default(),
            CancellationToken::new(),
        )
        .await
        .err()
        .unwrap();
    assert!(error.to_string().contains("consent"));
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(
        prepared
            .start(1, root.clone(), local_options(), cancel)
            .await
            .is_err()
    );
    assert!(!root.exists());
    assert!(
        PreparedTorrent::from_magnet(
            &format!("magnet:?xt=urn:btih:{}", prepared.info_hash),
            EngineOptions::default(),
            CancellationToken::new(),
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("consent")
    );
}

#[test]
fn limits_never_interpret_paused_or_zero_as_unlimited() {
    assert_eq!(RateLimit::Unlimited.bps().unwrap(), None);
    assert_eq!(RateLimit::Paused.bps().unwrap(), None);
    assert!(RateLimit::Bps(0).bps().is_err());
    assert!(RateLimit::Bps(1).bps().is_err());
    assert_eq!(RateLimit::Bps(16_384).bps().unwrap().unwrap().get(), 16_384);
    let mut options = local_options();
    options.peer_limit = 0;
    assert!(options.validate().is_err());
    options.peer_limit = 1;
    options.initial_peers = vec!["192.0.2.1:1234".parse().unwrap()];
    assert!(options.validate().is_err());
}

#[test]
fn unicode_aliases_are_rejected_before_payload_storage_on_every_host() {
    for (first, second) in [
        ("é.mp4", "e\u{301}.mp4"),
        ("Straße.mp4", "STRASSE.mp4"),
        ("Σ.mp4", "σ.mp4"),
        ("σ.mp4", "ς.mp4"),
        ("ФИЛЬМ.mp4", "фильм.mp4"),
        ("Ａ.mp4", "A.mp4"),
    ] {
        for (first, second) in [(first, second), (second, first)] {
            let result = PreparedTorrent::from_bytes(torrent_with_paths(&[&[first], &[second]]));
            let error = result.expect_err("aliased metadata paths must be rejected");
            assert!(
                !format!("{error:?} {error:#}").contains(first),
                "path rejection must not expose private metadata"
            );
        }
    }
}

#[test]
fn normalized_directory_prefixes_and_unsafe_components_are_rejected() {
    for (file, directory) in [("É", "e\u{301}"), ("Straße", "STRASSE"), ("Σ", "ς")] {
        assert!(
            PreparedTorrent::from_bytes(torrent_with_paths(&[&[file], &[directory, "child.mp4"]]))
                .is_err()
        );
        assert!(
            PreparedTorrent::from_bytes(torrent_with_paths(&[&[directory, "child.mp4"], &[file]]))
                .is_err()
        );
    }
    for component in [
        "／secret.mp4", // FULLWIDTH SOLIDUS becomes a separator.
        "＼secret.mp4", // FULLWIDTH REVERSE SOLIDUS becomes a separator.
        "．．",         // FULLWIDTH FULL STOP becomes parent traversal.
        "ＣＯＮ.mp4",   // Compatibility characters become a reserved name.
        "file：ads.mp4",
        "name\u{a0}", // NBSP becomes a forbidden trailing space.
    ] {
        assert!(PreparedTorrent::from_bytes(torrent_with_paths(&[&[component]])).is_err());
    }
}

#[test]
fn distinct_russian_paths_keep_original_names_and_sizes() {
    let paths: &[&[&str]] = &[
        &["Сериал", "Серия 01.mp4"],
        &["Сериал", "Серия 02.mp4"],
        &["Фильмы", "Ёжик.mp4"],
        &["Фильмы", "Ежик.mp4"],
    ];
    let prepared = PreparedTorrent::from_bytes(torrent_with_paths(paths)).unwrap();
    assert_eq!(prepared.total_bytes, paths.len() as u64);
    for (file, components) in prepared.files.iter().zip(paths) {
        assert_eq!(file.relative_filename, PathBuf::from(components.join("/")));
        assert_eq!(file.len, 1);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn selected_download_seek_save_readiness_and_rehash_resume() {
    timeout(Duration::from_secs(25), async {
        let fixture = fixture::Fixture::new(true);
        let seed_dir = tempdir();
        write_seed(&fixture, seed_dir.path());
        let seed = LocalSession::new(seed_dir.path().into()).await.unwrap();
        let _seed_file = seed
            .add_selected(fixture.torrent.clone(), 1, vec![], true)
            .await
            .unwrap();
        let prepared = PreparedTorrent::from_bytes(fixture.torrent).unwrap();
        let download_dir = tempdir();
        let mut options = local_options();
        options.initial_peers = vec![seed.listen_addr()];
        options.upload_limit = RateLimit::Paused;
        options.peer_limit = 1;
        let playback = prepared
            .start(
                1,
                download_dir.path().into(),
                options.clone(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(playback.upload_disabled);
        assert_eq!(playback.lease.0.peer_limit, Some(1));
        // Independent readers reprioritize widely separated verified pieces.
        let (front, back) = tokio::join!(
            playback.source.read_at(0, 1024),
            playback
                .source
                .read_at(fixture.selected.len() as u64 - 1024, 1024)
        );
        let front = front.unwrap();
        let back = back.unwrap();
        assert_eq!(&front[..], &fixture.selected[..front.len()]);
        assert_eq!(
            &back[..],
            &fixture.selected[fixture.selected.len() - 1024..][..back.len()]
        );
        playback
            .wait_until_ready(CancellationToken::new())
            .await
            .unwrap();
        assert!(playback.selected_file_ready());
        assert_eq!(
            playback.stats().selected_file_bytes,
            fixture.selected.len() as u64
        );
        assert_eq!(read_all(playback.source.as_ref()).await, fixture.selected);
        assert!(
            playback
                .set_limits(RateLimit::Unlimited, RateLimit::Unlimited)
                .await
                .is_err()
        );
        playback.pause().await.unwrap();
        playback.pause().await.unwrap();
        assert_eq!(playback.stats().state, EngineState::Paused);
        playback.resume().await.unwrap();
        playback.stop().await;
        playback.stop().await;
        assert_eq!(playback.stats().state, EngineState::Stopped);
        assert!(playback.source.read_at(0, 1).await.is_err());
        seed.stop().await;
        drop(playback);

        // No peer is alive. A fresh engine must discover verified cached
        // pieces by hashing, including bytes across selected-file boundaries.
        options.initial_peers.clear();
        let reopened = prepared
            .start(
                1,
                download_dir.path().into(),
                options,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(reopened.selected_file_ready());
        assert_eq!(read_all(reopened.source.as_ref()).await, fixture.selected);
        assert_eq!(reopened.stats().download_bps, 0);
        reopened.stop().await;
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn magnet_exchange_is_metadata_only_with_explicit_loopback_seed() {
    timeout(Duration::from_secs(25), async {
        // Nonprivate fixture: librqbit deliberately suppresses BEP 9 metadata
        // upload for private torrents. No DHT, trackers or public peers.
        let fixture = fixture::Fixture::new(false);
        let prepared = PreparedTorrent::from_bytes(fixture.torrent.clone()).unwrap();
        let seed_dir = tempdir();
        write_seed(&fixture, seed_dir.path());
        std::fs::rename(
            seed_dir.path().join("kadr-fixture"),
            seed_dir.path().join("payload"),
        )
        .unwrap();
        let mut seed_options = local_options();
        seed_options.upload_limit = RateLimit::Unlimited;
        let seed = prepared
            .start(
                1,
                seed_dir.path().into(),
                seed_options,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let mut options = local_options();
        options.initial_peers = vec![seed.lease.0.listen_addr().unwrap()];
        let resolved = PreparedTorrent::from_magnet(
            &format!("magnet:?xt=urn:btih:{}", prepared.info_hash),
            options.clone(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(resolved.info_hash, prepared.info_hash);
        assert_eq!(resolved.files, prepared.files);
        assert_eq!(resolved.total_bytes, prepared.total_bytes);
        // Only now allocate a cache root and start payload activity.
        let output = tempdir();
        assert_eq!(std::fs::read_dir(output.path()).unwrap().count(), 0);
        let playback = resolved
            .start(1, output.path().into(), options, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(read_all(playback.source.as_ref()).await, fixture.selected);
        playback
            .wait_until_ready(CancellationToken::new())
            .await
            .unwrap();
        playback.stop().await;
        seed.stop().await;
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_magnet_and_dropped_session_release_listener() {
    timeout(Duration::from_secs(10), async {
        let cancel = CancellationToken::new();
        let child = cancel.clone();
        let resolve = tokio::spawn(async move {
            PreparedTorrent::from_magnet(
                "magnet:?xt=urn:btih:0123456789012345678901234567890123456789",
                EngineOptions {
                    initial_peers: vec!["127.0.0.1:1".parse().unwrap()],
                    ..local_options()
                },
                child,
            )
            .await
        });
        tokio::time::sleep(Duration::from_millis(30)).await;
        cancel.cancel();
        assert!(resolve.await.unwrap().is_err());
        let options = local_options();
        let token = CancellationToken::new();
        let session = create_session(PathBuf::new(), &options, token.clone())
            .await
            .unwrap();
        let address = session.listen_addr().unwrap();
        let lease = SessionLease(session.clone());
        drop(lease);
        assert!(token.is_cancelled());
        timeout(Duration::from_secs(3), async {
            loop {
                if let Ok(listener) = tokio::net::TcpListener::bind(address).await {
                    drop(listener);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
    })
    .await
    .unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn authorized_temp_parent_alias_is_resolved_without_relaxing_output_checks() {
    let directory = tempdir();
    let physical_parent = directory.path().join("physical");
    std::fs::create_dir(&physical_parent).unwrap();
    let parent_alias = directory.path().join("alias");
    std::os::unix::fs::symlink(&physical_parent, &parent_alias).unwrap();
    // Reproduce /var -> /private/var on Linux too: only the fixture's explicitly
    // authorized allocation parent is resolved, before creating its child.
    let output = fixture::tempdir_in(&parent_alias);
    assert!(output.path().starts_with(&physical_parent));
    let metadata = fixture::Fixture::new(true);
    let prepared = PreparedTorrent::from_bytes(metadata.torrent).unwrap();
    let options = EngineOptions {
        download_limit: RateLimit::Paused,
        ..local_options()
    };
    let playback = prepared
        .start(
            1,
            output.path().into(),
            options.clone(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    playback.stop().await;
    // The engine itself must still reject the alias, even though it refers to
    // the very same authorized tree. Never canonicalize untrusted output paths.
    let aliased_output = parent_alias.join("unallocated-output");
    let error = prepared
        .start(1, aliased_output, options, CancellationToken::new())
        .await
        .err()
        .expect("symlink ancestor must remain forbidden");
    assert!(error.to_string().contains("symlink"));
    assert!(!physical_parent.join("unallocated-output").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn unsafe_metadata_and_symlink_output_are_rejected_before_writes() {
    let fixture = fixture::Fixture::new(true);
    let bytes = fixture.torrent.to_vec();
    // Equal-length substitution keeps canonical bencode lengths valid.
    let needle = b"selected.bin";
    let index = bytes
        .windows(needle.len())
        .position(|part| part == needle)
        .unwrap();
    let mut unsafe_bytes = bytes;
    unsafe_bytes[index..index + needle.len()].copy_from_slice(b"../evil_.bin");
    assert!(PreparedTorrent::from_bytes(unsafe_bytes.into()).is_err());
    let prepared = PreparedTorrent::from_bytes(fixture.torrent).unwrap();
    let output = tempdir();
    let elsewhere = tempdir();
    std::os::unix::fs::symlink(elsewhere.path(), output.path().join("payload")).unwrap();
    let error = prepared
        .start(
            1,
            output.path().into(),
            local_options(),
            CancellationToken::new(),
        )
        .await
        .err()
        .unwrap();
    assert!(!format!("{error:?} {error:#}").contains(&output.path().display().to_string()));
    assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disabled_uploads_refuse_payload_and_live_limits_are_applied() {
    timeout(Duration::from_secs(15), async {
        let fixture = fixture::Fixture::new(true);
        let prepared = PreparedTorrent::from_bytes(fixture.torrent.clone()).unwrap();
        let seed_dir = tempdir();
        write_seed(&fixture, seed_dir.path());
        std::fs::rename(
            seed_dir.path().join("kadr-fixture"),
            seed_dir.path().join("payload"),
        )
        .unwrap();
        let seed = prepared
            .start(
                1,
                seed_dir.path().into(),
                EngineOptions {
                    upload_limit: RateLimit::Paused,
                    ..local_options()
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(seed.selected_file_ready());
        let output = tempdir();
        let leecher = prepared
            .start(
                1,
                output.path().into(),
                EngineOptions {
                    download_limit: RateLimit::Paused,
                    initial_peers: vec![seed.lease.0.listen_addr().unwrap()],
                    ..local_options()
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(leecher.stats().state, EngineState::Paused);
        assert!(!leecher.selected_file_ready());
        leecher
            .set_limits(RateLimit::Bps(65_536), RateLimit::Bps(32_768))
            .await
            .unwrap();
        assert_eq!(
            leecher.lease.0.ratelimits.get_download_bps().unwrap().get(),
            65_536
        );
        assert_eq!(
            leecher.lease.0.ratelimits.get_upload_bps().unwrap().get(),
            32_768
        );
        let result = timeout(Duration::from_millis(500), leecher.source.read_at(0, 1024)).await;
        assert!(
            !matches!(result, Ok(Ok(_))),
            "disabled seed uploaded payload"
        );
        assert_eq!(seed.stats().uploaded_bytes, 0);
        assert_eq!(leecher.stats().selected_file_bytes, 0);
        leecher
            .set_limits(RateLimit::Unlimited, RateLimit::Paused)
            .await
            .unwrap();
        assert_eq!(leecher.stats().state, EngineState::Paused);
        leecher.stop().await;
        seed.stop().await;
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn startup_scheduler_spends_initial_burst_on_contiguous_prefix() {
    timeout(Duration::from_secs(10), async {
        let fixture = fixture::Fixture::new(true);
        let seed_dir = tempdir();
        write_seed(&fixture, seed_dir.path());
        let seed = LocalSession::new(seed_dir.path().into()).await.unwrap();
        let _seed_file = seed
            .add_selected(fixture.torrent.clone(), 1, vec![], true)
            .await
            .unwrap();
        let prepared = PreparedTorrent::from_bytes(fixture.torrent).unwrap();
        let output = tempdir();
        let playback = prepared
            .start(
                1,
                output.path().into(),
                EngineOptions {
                    download_limit: RateLimit::Bps(65_536),
                    upload_limit: RateLimit::Paused,
                    initial_peers: vec![seed.listen_addr()],
                    ..local_options()
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        // Wait until the initial four-request token burst has arrived, before
        // registering any player read. Ordinary upstream scheduling spends
        // its second request on the last piece instead of the fourth prefix
        // piece. A startup stream must change that ordering before unpause.
        while playback.stats().progress_bytes < (4 * fixture::PIECE) as u64 {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let before = playback.stats();
        assert!(!before.selected_file_ready);
        let offset = 3 * fixture::PIECE - fixture::PREFIX;
        let prefix = timeout(
            Duration::from_millis(80),
            playback.source.read_at(offset as u64, 1),
        )
        .await;
        playback.stop().await;
        seed.stop().await;
        assert!(
            prefix.is_ok(),
            "fourth prefix piece missed initial burst: {before:?}"
        );
        assert_eq!(
            &prefix.unwrap().unwrap()[..],
            &fixture.selected[offset..offset + 1]
        );
    })
    .await
    .unwrap();
}
