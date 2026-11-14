use std::{
    collections::BTreeMap,
    fmt,
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};

use cap_std::fs::Dir;
use serde::{Deserialize, Serialize};

use crate::{Error, Result, fs, io_error};

const INDEX: &str = "index.json";
const PENDING: &str = "index.pending";
const MAX_INDEX: u64 = 16 * 1024 * 1024;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    reserved: u64,
    used: u64,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Index {
    version: u32,
    clock: u64,
    entries: BTreeMap<String, Entry>,
}

impl Index {
    fn tick(&mut self) -> Result<u64> {
        self.clock = self.clock.checked_add(1).ok_or(Error::CorruptIndex)?;
        Ok(self.clock)
    }
}

struct State {
    root: Dir,
    data: Dir,
    // The lock is retained by leases, even after CacheManager is dropped.
    _lock: File,
    index: Index,
    active: BTreeMap<String, u64>,
    limit: u64,
    failed: bool,
    #[cfg(test)]
    free_override: Option<u64>,
}

impl State {
    fn commit(&mut self, index: Index) -> Result<()> {
        let result = (|| {
            let bytes = serde_json::to_vec(&index).map_err(|_| Error::CorruptIndex)?;
            if bytes.len() as u64 > MAX_INDEX {
                return Err(Error::CorruptIndex);
            }
            let mut pending = fs::new_file(&self.root, PENDING)?;
            pending.write_all(&bytes).map_err(io_error)?;
            pending.sync_all().map_err(io_error)?;
            self.root
                .rename(PENDING, &self.root, INDEX)
                .map_err(io_error)?;
            fs::sync_dir(&self.root)?;
            Ok(())
        })();
        if result.is_err() {
            // A failed fsync/rename has an ambiguous persistence outcome.
            // Fail closed until reopen, never reserve from stale accounting.
            self.failed = true;
        } else {
            self.index = index;
        }
        result
    }

    fn actual(&self) -> Result<BTreeMap<String, u64>> {
        for entry in self.data.entries().map_err(io_error)? {
            let name = entry.map_err(io_error)?.file_name();
            let key = name.to_str().ok_or(Error::CorruptIndex)?;
            if !self.index.entries.contains_key(key) {
                // Never adopt/delete an unindexed directory.
                return Err(Error::CorruptIndex);
            }
        }
        self.index
            .entries
            .keys()
            .map(|key| {
                let bytes = if fs::exists(&self.data, key)? {
                    fs::tree_size(&fs::open_dir(&self.data, Path::new(key))?)?
                } else {
                    // Crash after index reservation / before directory creation,
                    // or after eviction / before the index update.
                    0
                };
                Ok((key.clone(), bytes))
            })
            .collect()
    }

    fn usage(&self) -> Result<CacheUsage> {
        let actual = self.actual()?;
        let sum = |values: Vec<u64>| {
            values
                .into_iter()
                .try_fold(0u64, |sum, value| sum.checked_add(value))
                .ok_or(Error::QuotaExceeded)
        };
        Ok(CacheUsage {
            limit_bytes: self.limit,
            reserved_bytes: sum(self.index.entries.values().map(|e| e.reserved).collect())?,
            actual_bytes: sum(actual.values().copied().collect())?,
            active_entries: self.active.len() as u64,
            active_leases: sum(self.active.values().copied().collect())?,
        })
    }

    fn evict(&mut self, key: &str) -> Result<()> {
        if self.active.contains_key(key) {
            return Err(Error::QuotaExceeded);
        }
        fs::remove_tree(&self.data, Path::new(key))?;
        fs::sync_dir(&self.data)?;
        let mut index = self.index.clone();
        index.entries.remove(key);
        self.commit(index)
    }

    fn inactive_lru(&self, except: Option<&str>) -> Vec<String> {
        let mut entries: Vec<_> = self
            .index
            .entries
            .iter()
            .filter(|(key, _)| !self.active.contains_key(*key) && Some(key.as_str()) != except)
            .map(|(key, entry)| (entry.used, key.clone()))
            .collect();
        entries.sort();
        entries.into_iter().map(|(_, key)| key).collect()
    }

    fn fit(&mut self, limit: u64, extra: u64, except: Option<&str>) -> Result<()> {
        let actual = self.actual()?;
        let charge = |key: &str| self.index.entries[key].reserved.max(actual[key]);
        let mut total = extra;
        let mut protected = extra;
        for key in self.index.entries.keys() {
            total = total.checked_add(charge(key)).ok_or(Error::QuotaExceeded)?;
            if self.active.contains_key(key) || Some(key.as_str()) == except {
                protected = protected
                    .checked_add(charge(key))
                    .ok_or(Error::QuotaExceeded)?;
            }
        }
        // Do not evict anything for a request that can never fit.
        if protected > limit {
            return Err(Error::QuotaExceeded);
        }
        let candidates = self.inactive_lru(except);
        for key in candidates {
            if total <= limit {
                break;
            }
            let bytes = charge_for(&self.index.entries[&key], actual[&key]);
            self.evict(&key)?;
            total -= bytes;
        }
        if total > limit {
            return Err(Error::QuotaExceeded);
        }
        Ok(())
    }

    fn outstanding(&self, extra: u64) -> Result<u64> {
        self.index
            .entries
            .iter()
            .try_fold(extra, |sum, (key, entry)| {
                let allocated = if fs::exists(&self.data, key)? {
                    fs::tree_allocated(&fs::open_dir(&self.data, Path::new(key))?)?
                } else {
                    0
                };
                sum.checked_add(entry.reserved.saturating_sub(allocated))
                    .ok_or(Error::QuotaExceeded)
            })
    }

    fn available_at(&self, dir: &Dir) -> Result<u64> {
        #[cfg(test)]
        if let Some(bytes) = self.free_override {
            return Ok(bytes);
        }
        fs::free_space(dir)
    }

    fn check_save_budget(&self, same_filesystem: bool, free: u64, bytes: u64) -> Result<()> {
        let needed = if same_filesystem {
            let actual = self.actual()?;
            if actual
                .iter()
                .any(|(key, bytes)| *bytes > self.index.entries[key].reserved)
            {
                return Err(Error::ReservationExceeded);
            }
            bytes
                .checked_add(self.outstanding(0)?)
                .ok_or(Error::OutOfSpace)?
        } else {
            bytes
        };
        fs::ensure_space(free, needed)
    }
}

fn charge_for(entry: &Entry, actual: u64) -> u64 {
    entry.reserved.max(actual)
}

/// Synchronous disk manager. Every method, including lease drop, is worker-only.
/// One manager per owned/private root; a second open fails while any lease lives.
pub struct CacheManager {
    state: Arc<Mutex<State>>,
    path: PathBuf,
}

/// An active pin on the entire torrent's data, not just its selected file.
/// Stop the torrent and all readers before dropping. Drop performs blocking I/O.
/// The explicit path/token getters are sensitive and must not be logged.
pub struct CacheLease {
    state: Arc<Mutex<State>>,
    path: PathBuf,
    key: String,
    token: String,
}

/// Logical bytes, not filesystem allocation. Reservations include absent bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheUsage {
    pub limit_bytes: u64,
    pub reserved_bytes: u64,
    pub actual_bytes: u64,
    pub active_entries: u64,
    pub active_leases: u64,
}

impl CacheManager {
    pub fn open(root: PathBuf, limit: u64) -> Result<Self> {
        let dir = fs::absolute_dir(&root, true)?;
        let metadata = rustix::fs::fstat(&dir).map_err(|error| io_error(error.into()))?;
        if metadata.st_mode & 0o077 != 0 || metadata.st_uid != rustix::process::geteuid().as_raw() {
            return Err(Error::UnsafePath);
        }
        let lock = fs::open_file(&dir, "manager.lock", true)?;
        fs2::FileExt::try_lock_exclusive(&lock).map_err(|_| Error::Busy)?;
        let index = if fs::exists(&dir, INDEX)? {
            let mut file = fs::open_file(&dir, INDEX, false)?;
            if file.metadata().map_err(io_error)?.len() > MAX_INDEX {
                return Err(Error::CorruptIndex);
            }
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).map_err(io_error)?;
            let index: Index = serde_json::from_slice(&bytes).map_err(|_| Error::CorruptIndex)?;
            if index.version != 1
                || index.clock == u64::MAX
                || index.entries.iter().any(|(key, entry)| {
                    !fs::valid_key(key)
                        || *key != key.to_ascii_lowercase()
                        || entry.used > index.clock
                })
            {
                return Err(Error::CorruptIndex);
            }
            index
        } else {
            Index {
                version: 1,
                clock: 0,
                entries: BTreeMap::new(),
            }
        };
        // Only our fixed pending filename is disposable, and only after the
        // committed index version has been validated. Never clean future formats.
        if fs::exists(&dir, PENDING)? {
            fs::remove_tree(&dir, Path::new(PENDING))?;
        }
        if !fs::exists(&dir, "data")? {
            dir.create_dir("data").map_err(io_error)?;
        }
        let data = fs::open_dir(&dir, Path::new("data"))?;
        let mut state = State {
            root: dir,
            data,
            _lock: lock,
            index,
            active: BTreeMap::new(),
            limit,
            failed: false,
            #[cfg(test)]
            free_override: None,
        };
        // Validate managed data before modifying the index.
        state.actual()?;
        state.fit(limit, 0, None)?;
        if !fs::exists(&state.root, INDEX)? {
            state.commit(state.index.clone())?;
        }
        Ok(Self {
            state: Arc::new(Mutex::new(state)),
            path: root,
        })
    }

    fn lock(&self) -> Result<MutexGuard<'_, State>> {
        let state = self.state.lock().map_err(|_| Error::Io)?;
        if state.failed {
            return Err(Error::Io);
        }
        Ok(state)
    }

    /// Reserve WHOLE torrent length before the engine creates any file.
    /// Reuse requires the exact original length. Keys: 1..=128 ASCII
    /// alphanumerics, normalized to lowercase on every platform.
    pub fn reserve(&self, content_key: &str, total_bytes: u64) -> Result<CacheLease> {
        if !fs::valid_key(content_key) {
            return Err(Error::InvalidInput);
        }
        let canonical = content_key.to_ascii_lowercase();
        let content_key = canonical.as_str();
        let mut state = self.lock()?;
        let actual = state.actual()?;
        if actual
            .iter()
            .any(|(key, bytes)| *bytes > state.index.entries[key].reserved)
        {
            return Err(Error::ReservationExceeded);
        }
        let existing = state.index.entries.get(content_key);
        if existing.is_some_and(|entry| entry.reserved != total_bytes) {
            return Err(Error::InvalidInput);
        }
        let extra = if existing.is_some() { 0 } else { total_bytes };
        let limit = state.limit;
        state.fit(limit, extra, Some(content_key))?;
        // Hold enough physical space for ALL outstanding reservations, not
        // merely this request (sparse/unwritten files have not consumed it yet).
        let outstanding = state.outstanding(extra)?;
        fs::ensure_space(state.available_at(&state.root)?, outstanding)?;
        let mut index = state.index.clone();
        let used = index.tick()?;
        index.entries.insert(
            content_key.to_owned(),
            Entry {
                reserved: total_bytes,
                used,
            },
        );
        // Durable reservation comes BEFORE directory creation/output handoff.
        state.commit(index)?;
        if !fs::exists(&state.data, content_key)? {
            state.data.create_dir(content_key).map_err(io_error)?;
            fs::sync_dir(&state.data)?;
        }
        fs::open_dir(&state.data, Path::new(content_key))?;
        let active = state.active.entry(content_key.to_owned()).or_default();
        *active = active.checked_add(1).ok_or(Error::QuotaExceeded)?;
        Ok(CacheLease {
            state: self.state.clone(),
            path: self.path.join("data").join(content_key),
            key: content_key.to_owned(),
            token: fs::token(),
        })
    }

    /// Worker-only save preflight. On the cache filesystem, require space for
    /// the saved copy PLUS all outstanding whole-torrent reservations.
    ///
    /// This is a check, not a reservation: the application must serialize saves
    /// and prevent new cache reservations/root switches until the save finishes.
    /// Other processes can still consume space. Destination containment policy
    /// (including historical cache roots) remains the application's responsibility.
    pub fn check_save_space(&self, destination_parent: &Path, total_bytes: u64) -> Result<()> {
        let destination = fs::absolute_dir(destination_parent, false)?;
        let state = self.lock()?;
        let cache_stat = rustix::fs::fstat(&state.root).map_err(|error| io_error(error.into()))?;
        let destination_stat =
            rustix::fs::fstat(&destination).map_err(|error| io_error(error.into()))?;
        state.check_save_budget(
            cache_stat.st_dev == destination_stat.st_dev,
            state.available_at(&destination)?,
            total_bytes,
        )
    }

    /// Refuse below the sum of active reservations; otherwise evict inactive LRU.
    pub fn set_limit(&self, limit: u64) -> Result<()> {
        let mut state = self.lock()?;
        state.fit(limit, 0, None)?;
        state.limit = limit;
        Ok(())
    }

    pub fn usage(&self) -> Result<CacheUsage> {
        self.lock()?.usage()
    }

    /// Only indexed, inactive entries are removed. Saved files are not indexed.
    pub fn clear_inactive(&self) -> Result<CacheUsage> {
        let mut state = self.lock()?;
        for key in state.inactive_lru(None) {
            state.evict(&key)?;
        }
        state.usage()
    }

    pub fn touch(&self, content_key: &str) -> Result<()> {
        if !fs::valid_key(content_key) {
            return Err(Error::InvalidInput);
        }
        let canonical = content_key.to_ascii_lowercase();
        let content_key = canonical.as_str();
        let mut state = self.lock()?;
        let mut index = state.index.clone();
        let used = index.tick()?;
        index
            .entries
            .get_mut(content_key)
            .ok_or(Error::InvalidInput)?
            .used = used;
        state.commit(index)
    }
}

impl CacheLease {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A random per-lease identifier; never a URL, filename, or content key.
    pub fn token(&self) -> &str {
        &self.token
    }
}

impl Drop for CacheLease {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            if let Some(count) = state.active.get_mut(&self.key) {
                *count -= 1;
                if *count == 0 {
                    state.active.remove(&self.key);
                }
            }
            if !state.failed {
                let mut index = state.index.clone();
                if let Ok(used) = index.tick() {
                    if let Some(entry) = index.entries.get_mut(&self.key) {
                        entry.used = used;
                    }
                    // Errors cannot be returned from Drop; commit poisons the
                    // state, and the next explicit operation reports failure.
                    let _ = state.commit(index);
                } else {
                    state.failed = true;
                }
            }
        }
    }
}

impl fmt::Debug for CacheManager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CacheManager([redacted])")
    }
}

impl fmt::Debug for CacheLease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CacheLease([redacted])")
    }
}

#[cfg(test)]
#[path = "cache_tests.rs"]
mod tests;
