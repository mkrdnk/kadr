use std::path::PathBuf;

use kadr_cache::CacheManager;

// Keep process spawning in its own test executable: fork can temporarily retain
// sibling tests' flock descriptors until exec, even with CLOEXEC, racing their
// immediate reopen after the last manager/lease is dropped.
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
            "process_exit_releases_stale_active_state_without_losing_bytes",
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
