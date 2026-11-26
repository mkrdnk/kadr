use super::*;
use std::os::unix::fs::{PermissionsExt, symlink};

fn setup(limit: u64) -> (tempfile::TempDir, CacheManager) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap().join("cache");
    let manager = CacheManager::open(root, limit).unwrap();
    (temp, manager)
}

fn put(manager: &CacheManager, key: &str, length: u64) -> PathBuf {
    let lease = manager.reserve(key, length).unwrap();
    let path = lease.path().to_owned();
    std::fs::write(path.join("payload"), vec![42; length as usize]).unwrap();
    drop(lease);
    path
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn darwin_system_aliases_produce_physical_leases_and_safe_save_parents() {
    for (alias, physical) in [("/tmp", "/private/tmp"), ("/var/tmp", "/private/var/tmp")] {
        // Intentionally do not canonicalize: exercise the standard OS spelling.
        let temp = tempfile::tempdir_in(alias).unwrap();
        let manager = CacheManager::open(temp.path().join("cache"), 8).unwrap();
        let lease = manager.reserve("torrent", 8).unwrap();
        assert!(lease.path().starts_with(physical));
        assert!(!lease.path().starts_with(alias));
        // A second no-follow walker (as in the torrent engine) can use the
        // physical lease path without depending on system-alias normalization.
        let mut dir =
            cap_std::fs::Dir::open_ambient_dir("/", cap_std::ambient_authority()).unwrap();
        for component in lease.path().components().skip(1) {
            dir = fs::open_dir(&dir, Path::new(component.as_os_str())).unwrap();
        }
        std::fs::write(lease.path().join("payload"), b"data").unwrap();
        manager.check_save_space(temp.path(), 4).unwrap();
        let source = Arc::new(
            kadr_media::LocalFile::open(lease.path().join("payload"))
                .await
                .unwrap(),
        );
        let destination = temp.path().join("saved");
        crate::save_source(
            source.clone(),
            destination.clone(),
            &tokio_util::sync::CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(destination).unwrap(), b"data");
        std::fs::create_dir(temp.path().join("real")).unwrap();
        let linked = temp.path().join("linked");
        symlink(temp.path().join("real"), &linked).unwrap();
        assert_eq!(
            CacheManager::open(linked.join("cache"), 8).unwrap_err(),
            Error::UnsafePath
        );
        assert_eq!(manager.check_save_space(&linked, 4), Err(Error::UnsafePath));
        assert_eq!(
            crate::save_source(
                source,
                linked.join("saved"),
                &tokio_util::sync::CancellationToken::new(),
                |_| {},
            )
            .await,
            Err(Error::UnsafePath)
        );
    }
}

#[test]
fn lru_touch_evicts_only_inactive_whole_torrents() {
    let (_temp, manager) = setup(30);
    let a = put(&manager, "a", 10);
    let b = put(&manager, "b", 10);
    let pinned = manager.reserve("pinned", 10).unwrap();
    std::fs::create_dir(pinned.path().join("otherfiles")).unwrap();
    std::fs::write(pinned.path().join("otherfiles/sharedpiece"), [1, 2]).unwrap();
    manager.touch("a").unwrap();
    let next = manager.reserve("next", 10).unwrap();
    assert!(a.exists());
    assert!(!b.exists());
    assert!(pinned.path().join("otherfiles/sharedpiece").exists());
    let usage = manager.clear_inactive().unwrap();
    assert_eq!(usage.reserved_bytes, 20);
    assert_eq!(usage.active_entries, 2);
    assert!(!a.exists());
    assert!(next.path().exists());
    drop(next);
    drop(pinned);
    assert_eq!(manager.clear_inactive().unwrap().reserved_bytes, 0);
}

#[test]
fn multiple_leases_protect_whole_entry_and_retain_process_lock() {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<CacheManager>();
    send_sync::<CacheLease>();
    let (_temp, manager) = setup(10);
    let root = manager.path.clone();
    let one = manager.reserve("shared", 10).unwrap();
    let two = manager.reserve("shared", 10).unwrap();
    assert_ne!(one.token(), two.token());
    assert_eq!(manager.usage().unwrap().active_leases, 2);
    assert_eq!(manager.usage().unwrap().reserved_bytes, 10);
    assert_eq!(
        manager.reserve("other", 1).unwrap_err(),
        Error::QuotaExceeded
    );
    drop(one);
    assert_eq!(manager.clear_inactive().unwrap().active_leases, 1);
    assert_eq!(
        CacheManager::open(root.clone(), 10).unwrap_err(),
        Error::Busy
    );
    drop(manager);
    assert_eq!(
        CacheManager::open(root.clone(), 10).unwrap_err(),
        Error::Busy
    );
    drop(two);
    let reopened = CacheManager::open(root, 10).unwrap();
    assert_eq!(reopened.usage().unwrap().active_leases, 0);
    reopened.clear_inactive().unwrap();
}

#[test]
fn reopen_reuses_bytes_and_persisted_lru() {
    let (_temp, manager) = setup(20);
    let root = manager.path.clone();
    let a = put(&manager, "aa11", 10);
    let b = put(&manager, "bb22", 10);
    manager.touch("aa11").unwrap();
    drop(manager);
    let manager = CacheManager::open(root, 20).unwrap();
    let again = manager.reserve("aa11", 10).unwrap();
    assert_eq!(again.path(), a);
    assert_eq!(std::fs::read(a.join("payload")).unwrap(), vec![42; 10]);
    let _next = manager.reserve("next", 10).unwrap();
    assert!(!b.exists());
    assert_eq!(manager.usage().unwrap().actual_bytes, 10);
}

#[test]
fn lower_limit_refuses_impossible_request_without_eviction() {
    let (_temp, manager) = setup(30);
    let a = put(&manager, "a", 10);
    let b = put(&manager, "b", 10);
    let active = manager.reserve("active", 10).unwrap();
    assert_eq!(manager.set_limit(9), Err(Error::QuotaExceeded));
    assert_eq!(manager.usage().unwrap().limit_bytes, 30);
    assert!(a.exists() && b.exists());
    manager.set_limit(10).unwrap();
    assert!(!a.exists() && !b.exists());
    assert!(active.path().exists());
    drop(active);
    manager.set_limit(0).unwrap();
    assert_eq!(manager.usage().unwrap().reserved_bytes, 0);
}

#[test]
fn quota_uses_reservations_and_overruns_fail_closed() {
    let (_temp, manager) = setup(10);
    assert_eq!(
        manager.reserve("large", 11).unwrap_err(),
        Error::QuotaExceeded
    );
    let active = manager.reserve("a", 10).unwrap();
    assert_eq!(manager.usage().unwrap().actual_bytes, 0);
    assert_eq!(manager.reserve("b", 1).unwrap_err(), Error::QuotaExceeded);
    assert_eq!(manager.reserve("a", 9).unwrap_err(), Error::InvalidInput);
    std::fs::write(active.path().join("badwriter"), vec![0; 11]).unwrap();
    assert_eq!(manager.usage().unwrap().actual_bytes, 11);
    assert_eq!(
        manager.reserve("empty", 0).unwrap_err(),
        Error::ReservationExceeded
    );
    assert_eq!(
        manager.reserve("a", 10).unwrap_err(),
        Error::ReservationExceeded
    );
    drop(active);
    manager.clear_inactive().unwrap();
    manager.reserve("safe", 10).unwrap();
}

#[test]
fn physical_space_accounts_for_all_outstanding_and_sparse_bytes() {
    let (_temp, manager) = setup(1024 * 1024);
    manager.state.lock().unwrap().free_override = Some(5);
    assert_eq!(manager.reserve("a", 10).unwrap_err(), Error::OutOfSpace);
    assert_eq!(manager.usage().unwrap().reserved_bytes, 0);
    manager.state.lock().unwrap().free_override = Some(20);
    let a = manager.reserve("a", 10).unwrap();
    assert_eq!(manager.reserve("b", 11).unwrap_err(), Error::OutOfSpace);
    let sparse = File::create(a.path().join("sparse")).unwrap();
    sparse.set_len(10).unwrap();
    manager.state.lock().unwrap().free_override = Some(9);
    assert_eq!(manager.reserve("a", 10).unwrap_err(), Error::OutOfSpace);
    assert_eq!(manager.usage().unwrap().active_leases, 1);
}

#[test]
fn allocation_padding_never_credits_unrelated_sparse_bytes() {
    let (_temp, manager) = setup(8192);
    let lease = manager.reserve("mixed", 8192).unwrap();
    std::fs::write(lease.path().join("tiny"), [1]).unwrap();
    File::create(lease.path().join("sparse"))
        .unwrap()
        .set_len(8191)
        .unwrap();
    manager.state.lock().unwrap().free_override = Some(4096);
    assert_eq!(
        manager.reserve("mixed", 8192).unwrap_err(),
        Error::OutOfSpace
    );
}

#[test]
fn save_preflight_keeps_space_for_all_outstanding_torrents_and_sparse_files() {
    let (temp, manager) = setup(16_384);
    let parent = temp.path().canonicalize().unwrap();
    let one = manager.reserve("one", 8192).unwrap();
    let two = manager.reserve("two", 8192).unwrap();
    File::create(one.path().join("sparse"))
        .unwrap()
        .set_len(8192)
        .unwrap();
    std::fs::write(two.path().join("tiny"), [1]).unwrap();
    File::create(two.path().join("sparse"))
        .unwrap()
        .set_len(8191)
        .unwrap();
    // Only one logical byte is backed by credited physical allocation. The
    // tiny file's block padding cannot reduce demand for either sparse file.
    manager.state.lock().unwrap().free_override = Some(16_383);
    assert_eq!(manager.check_save_space(&parent, 1), Err(Error::OutOfSpace));
    manager.state.lock().unwrap().free_override = Some(16_384);
    assert_eq!(manager.check_save_space(&parent, 1), Ok(()));
    assert_eq!(manager.usage().unwrap().active_leases, 2);
    assert_eq!(manager.usage().unwrap().reserved_bytes, 16_384);
}

#[test]
fn save_preflight_credits_fully_allocated_bytes_and_checks_overflow() {
    let (temp, manager) = setup(8192);
    let parent = temp.path().canonicalize().unwrap();
    let lease = manager.reserve("one", 8192).unwrap();
    let mut payload = File::create(lease.path().join("allocated")).unwrap();
    payload.write_all(&[1; 8192]).unwrap();
    payload.sync_all().unwrap();
    manager.state.lock().unwrap().free_override = Some(8192);
    assert_eq!(manager.check_save_space(&parent, 8192), Ok(()));
    assert_eq!(
        manager.check_save_space(&parent, 8193),
        Err(Error::OutOfSpace)
    );
    payload.set_len(0).unwrap();
    manager.state.lock().unwrap().free_override = Some(u64::MAX);
    assert_eq!(
        manager.check_save_space(&parent, u64::MAX),
        Err(Error::OutOfSpace)
    );
}

#[test]
fn other_filesystem_save_budget_does_not_charge_cache_reservations() {
    let (_temp, manager) = setup(8192);
    let _lease = manager.reserve("pending", 8192).unwrap();
    let state = manager.state.lock().unwrap();
    // Exercise the different-device branch without relying on a second mounted
    // filesystem in CI or filling a real disk.
    assert_eq!(state.check_save_budget(false, 10, 10), Ok(()));
    assert_eq!(
        state.check_save_budget(false, 9, 10),
        Err(Error::OutOfSpace)
    );
    assert_eq!(
        state.check_save_budget(true, 10, 10),
        Err(Error::OutOfSpace)
    );
}

#[test]
fn save_preflight_rejects_symlink_parents_and_existing_cache_overruns() {
    let (temp, manager) = setup(10);
    let parent = temp.path().canonicalize().unwrap();
    symlink(&parent, parent.join("linked")).unwrap();
    assert_eq!(
        manager.check_save_space(&parent.join("linked"), 0),
        Err(Error::UnsafePath)
    );
    let lease = manager.reserve("badwriter", 10).unwrap();
    std::fs::write(lease.path().join("oversized"), [0; 11]).unwrap();
    assert_eq!(
        manager.check_save_space(&parent, 0),
        Err(Error::ReservationExceeded)
    );
}

#[test]
fn case_aliases_share_one_reservation_and_active_pin_on_every_platform() {
    let (_temp, manager) = setup(10);
    let one = manager.reserve("FOO", 10).unwrap();
    let two = manager.reserve("foo", 10).unwrap();
    assert_eq!(one.path(), two.path());
    assert_eq!(manager.usage().unwrap().reserved_bytes, 10);
    drop(one);
    manager.touch("FoO").unwrap();
    assert_eq!(manager.clear_inactive().unwrap().active_leases, 1);
    assert!(two.path().exists());
    drop(two);
    assert_eq!(manager.clear_inactive().unwrap().reserved_bytes, 0);
}

#[test]
fn keys_and_symlinks_never_escape_or_delete_saved_data() {
    let (temp, manager) = setup(100);
    for key in ["", ".", "..", "../outside", "/absolute", "a/b", "a_b", "🌻"] {
        assert_eq!(manager.reserve(key, 1).unwrap_err(), Error::InvalidInput);
    }
    let external = temp.path().join("saved");
    std::fs::create_dir(&external).unwrap();
    std::fs::write(external.join("keep"), b"saved").unwrap();
    let lease = manager.reserve("a", 10).unwrap();
    symlink(&external, lease.path().join("escape")).unwrap();
    assert_eq!(manager.usage(), Err(Error::UnsafePath));
    drop(lease);
    manager.clear_inactive().unwrap(); // unlinks the link, never its target
    assert_eq!(std::fs::read(external.join("keep")).unwrap(), b"saved");
    symlink(&external, manager.path.join("data/unknown")).unwrap();
    assert_eq!(manager.reserve("new", 1).unwrap_err(), Error::CorruptIndex);
    assert!(external.join("keep").exists());
}

#[test]
fn linked_root_data_index_and_ancestors_are_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let outside = base.join("outside");
    std::fs::create_dir(&outside).unwrap();
    symlink(&outside, base.join("link")).unwrap();
    assert_eq!(
        CacheManager::open(base.join("link/cache"), 10).unwrap_err(),
        Error::UnsafePath
    );
    let root = base.join("root");
    let manager = CacheManager::open(root.clone(), 10).unwrap();
    drop(manager);
    std::fs::remove_dir(root.join("data")).unwrap();
    symlink(&outside, root.join("data")).unwrap();
    assert_eq!(
        CacheManager::open(root.clone(), 10).unwrap_err(),
        Error::UnsafePath
    );
    std::fs::remove_file(root.join("data")).unwrap();
    std::fs::create_dir(root.join("data")).unwrap();
    std::fs::remove_file(root.join(INDEX)).unwrap();
    symlink(outside.join("private"), root.join(INDEX)).unwrap();
    assert!(CacheManager::open(root, 10).is_err());
    assert!(!outside.join("private").exists());
}

#[test]
fn restart_recovers_pending_index_and_missing_managed_directory() {
    let (_temp, manager) = setup(20);
    let root = manager.path.clone();
    let a = put(&manager, "a", 10);
    drop(manager);
    // Simulate interruption after durable reservation but before mkdir, or
    // deletion before its durable index update.
    std::fs::remove_dir_all(&a).unwrap();
    std::fs::write(root.join(PENDING), b"incomplete index bytes").unwrap();
    let reopened = CacheManager::open(root.clone(), 20).unwrap();
    assert!(!root.join(PENDING).exists());
    assert_eq!(reopened.usage().unwrap().reserved_bytes, 10);
    assert_eq!(reopened.usage().unwrap().actual_bytes, 0);
    let recovered = reopened.reserve("a", 10).unwrap();
    assert!(recovered.path().is_dir());
}

#[test]
fn process_exit_releases_stale_active_state_without_losing_bytes() {
    const CHILD_ROOT: &str = "KADR_CACHE_CRASH_TEST_ROOT";
    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        let manager = CacheManager::open(PathBuf::from(root), 10).unwrap();
        let lease = manager.reserve("inprogress", 10).unwrap();
        std::fs::write(lease.path().join("verified"), b"verified").unwrap();
        // No CacheLease/CacheManager destructors: emulate abrupt termination.
        std::process::exit(0);
    }
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap().join("cache");
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "cache::tests::process_exit_releases_stale_active_state_without_losing_bytes",
        ])
        .env(CHILD_ROOT, &root)
        .status()
        .unwrap();
    assert!(status.success());
    let manager = CacheManager::open(root, 10).unwrap();
    assert_eq!(manager.usage().unwrap().active_leases, 0);
    let lease = manager.reserve("inprogress", 10).unwrap();
    assert_eq!(
        std::fs::read(lease.path().join("verified")).unwrap(),
        b"verified"
    );
}

#[test]
fn corrupt_or_future_index_is_not_reset_and_permissions_are_private() {
    let (_temp, manager) = setup(10);
    let root = manager.path.clone();
    assert_eq!(
        std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(root.join(INDEX))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let lease = manager.reserve("privatekey", 1).unwrap();
    assert!(!format!("{manager:?}{lease:?}").contains("privatekey"));
    assert!(!format!("{manager:?}{lease:?}").contains(&root.to_string_lossy().to_string()));
    drop(lease);
    drop(manager);
    for bytes in [
        b"broken".as_slice(),
        br#"{"version":2,"clock":0,"entries":{}}"#,
    ] {
        std::fs::write(root.join(INDEX), bytes).unwrap();
        assert_eq!(
            CacheManager::open(root.clone(), 10).unwrap_err(),
            Error::CorruptIndex
        );
        assert_eq!(std::fs::read(root.join(INDEX)).unwrap(), bytes);
    }
}
