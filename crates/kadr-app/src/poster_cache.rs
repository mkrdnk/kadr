//! Private, direct-mapped artwork cache. There are 64 slots, each holding at
//! most 4 MiB plus a small record header, and one equally bounded staging file.
//! Colliding URLs replace a slot; there is no unbounded index or cleanup scan.
//! Unknown files and invalid slots are preserved; unsafe staging namespaces
//! fail closed.
//! A private, marked staging namespace lets us reclaim our single unfinished
//! write after a crash, including a zero-length or partially written record.
//!
//! Safe std filesystem operations reject symlinks and verify the private root
//! identity before reading/publishing. Like other private per-user caches, this
//! is not a sandbox against a malicious same-UID process racing path mutations.
#![forbid(unsafe_code)]
use std::io;

pub(super) const MAX_IMAGE_BYTES: usize = 4 * 1024 * 1024;
const MAX_URL_BYTES: usize = 2048;
const SLOTS: u64 = 64;
const MAGIC: &[u8] = b"KADR-ARTWORK-1\0";
const HEADER_BYTES: usize = MAGIC.len() + 2 + 1 + 4 + 8;
const MAX_RECORD_BYTES: usize = HEADER_BYTES + MAX_URL_BYTES + MAX_IMAGE_BYTES;

pub(super) struct Image {
    pub(super) mime: &'static str,
    pub(super) bytes: Vec<u8>,
}

impl Image {
    pub(super) fn new(mime: &str, bytes: Vec<u8>) -> io::Result<Self> {
        let mime = Self::validate(mime, &bytes)?;
        Ok(Self { mime, bytes })
    }

    fn validate(mime: &str, bytes: &[u8]) -> io::Result<&'static str> {
        let mime = match mime {
            "image/jpeg" if bytes.starts_with(b"\xff\xd8\xff") => "image/jpeg",
            "image/png" if bytes.starts_with(b"\x89PNG\r\n\x1a\n") => "image/png",
            "image/webp" if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") => {
                "image/webp"
            }
            "image/gif" if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") => {
                "image/gif"
            }
            _ => return Err(invalid()),
        };
        if bytes.len() > MAX_IMAGE_BYTES {
            return Err(invalid());
        }
        Ok(mime)
    }
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid artwork cache record")
}

// Stable across processes and Rust releases. This is an integrity checksum,
// not an authentication mechanism: the cache directory must be private.
fn checksum(parts: &[&[u8]]) -> u64 {
    parts
        .iter()
        .flat_map(|part| part.iter())
        .fold(0xcbf29ce484222325, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        })
}

fn slot(url: &str) -> String {
    format!("slot-{:02}.art", checksum(&[url.as_bytes()]) % SLOTS)
}

fn encode(url: &str, image: &Image) -> io::Result<Vec<u8>> {
    Image::validate(image.mime, &image.bytes)?;
    if url.is_empty() || url.len() > MAX_URL_BYTES {
        return Err(invalid());
    }
    let kind = match image.mime {
        "image/jpeg" => 0,
        "image/png" => 1,
        "image/webp" => 2,
        "image/gif" => 3,
        _ => return Err(invalid()),
    };
    let mut record = Vec::with_capacity(HEADER_BYTES + url.len() + image.bytes.len());
    record.extend_from_slice(MAGIC);
    record.extend_from_slice(&(url.len() as u16).to_be_bytes());
    record.push(kind);
    record.extend_from_slice(&(image.bytes.len() as u32).to_be_bytes());
    record.extend_from_slice(&checksum(&[url.as_bytes(), &[kind], &image.bytes]).to_be_bytes());
    record.extend_from_slice(url.as_bytes());
    record.extend_from_slice(&image.bytes);
    Ok(record)
}

fn decode(record: &[u8]) -> io::Result<(&str, Image)> {
    if record.len() < HEADER_BYTES || record.len() > MAX_RECORD_BYTES {
        return Err(invalid());
    }
    let header = record.strip_prefix(MAGIC).ok_or_else(invalid)?;
    let url_len = u16::from_be_bytes(header[..2].try_into().unwrap()) as usize;
    let kind = header[2];
    let image_len = u32::from_be_bytes(header[3..7].try_into().unwrap()) as usize;
    let digest = u64::from_be_bytes(header[7..15].try_into().unwrap());
    if url_len == 0
        || url_len > MAX_URL_BYTES
        || image_len > MAX_IMAGE_BYTES
        || record.len() != HEADER_BYTES + url_len + image_len
    {
        return Err(invalid());
    }
    let url = &record[HEADER_BYTES..HEADER_BYTES + url_len];
    let bytes = &record[HEADER_BYTES + url_len..];
    if checksum(&[url, &[kind], bytes]) != digest {
        return Err(invalid());
    }
    let mime = match kind {
        0 => "image/jpeg",
        1 => "image/png",
        2 => "image/webp",
        3 => "image/gif",
        _ => return Err(invalid()),
    };
    Ok((
        std::str::from_utf8(url).map_err(|_| invalid())?,
        Image::new(mime, bytes.to_vec())?,
    ))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod platform {
    use super::*;
    use std::{
        fs::{self, DirBuilder, File, OpenOptions},
        io::{Read, Write},
        os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
        path::{Component, Path, PathBuf},
        sync::Mutex,
    };

    #[cfg(target_os = "linux")]
    const NOFOLLOW: i32 = 0o400000;
    #[cfg(target_os = "macos")]
    const NOFOLLOW: i32 = 0x100;
    #[cfg(target_os = "linux")]
    const NONBLOCK: i32 = 0o4000;
    #[cfg(target_os = "macos")]
    const NONBLOCK: i32 = 4;

    fn same_file(left: &fs::Metadata, right: &fs::Metadata) -> bool {
        left.dev() == right.dev() && left.ino() == right.ino()
    }

    fn regular(file: &File, owner: u32) -> io::Result<()> {
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.uid() != owner
            || metadata.mode() & 0o077 != 0
        {
            return Err(invalid());
        }
        Ok(())
    }

    // Reject symlinks in *every* component, not just in the final file name.
    // No canonicalize: it would silently accept user-controlled symlinks.
    fn directory_path(path: &Path, create: bool) -> io::Result<()> {
        if !path.is_absolute()
            || path
                .components()
                .any(|component| !matches!(component, Component::RootDir | Component::Normal(_)))
        {
            return Err(invalid());
        }
        let mut current = PathBuf::from("/");
        for component in path.components() {
            match component {
                Component::RootDir => continue,
                Component::Normal(component) => current.push(component),
                _ => return Err(invalid()),
            }
            if create {
                match DirBuilder::new().mode(0o700).create(&current) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error),
                }
            }
            if !fs::symlink_metadata(&current)?.is_dir() {
                return Err(invalid());
            }
        }
        Ok(())
    }

    fn owned_directory(path: &Path, owner: u32) -> io::Result<File> {
        directory_path(path, false)?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(NOFOLLOW | NONBLOCK)
            .open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_dir() || metadata.uid() != owner || metadata.mode() & 0o077 != 0 {
            return Err(invalid());
        }
        Ok(file)
    }

    fn staging_directory(root: &Path, owner: u32) -> io::Result<File> {
        let path = root.join("staging-v1");
        match DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        let dir = owned_directory(&path, owner)?;
        let marker = path.join("kadr-artwork-owned-v1");
        match fs::symlink_metadata(&marker) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                // Never adopt a nonempty unmarked directory: its contents may
                // be unrelated. A crash before mkdir(marker) leaves it empty;
                // mkdir itself is atomic, so ownership has no partial record.
                if fs::read_dir(&path)?.next().transpose()?.is_some() {
                    return Err(invalid());
                }
                DirBuilder::new().mode(0o700).create(&marker)?;
            }
            Err(error) => return Err(error),
        }
        validate_marker(&marker, owner)?.sync_all()?;
        dir.sync_all()?;
        Ok(dir)
    }

    fn validate_marker(path: &Path, owner: u32) -> io::Result<File> {
        let marker = owned_directory(path, owner)?;
        if fs::read_dir(path)?.next().transpose()?.is_some() {
            return Err(invalid());
        }
        Ok(marker)
    }

    pub(crate) struct Cache {
        path: PathBuf,
        dir: File,
        staging: File,
        writer: Mutex<()>,
    }

    impl Cache {
        pub(crate) fn open(path: &Path) -> io::Result<Self> {
            directory_path(path, true)?;
            let dir = OpenOptions::new()
                .read(true)
                .custom_flags(NOFOLLOW | NONBLOCK)
                .open(path)?;
            let metadata = dir.metadata()?;
            if !metadata.is_dir() || metadata.mode() & 0o077 != 0 {
                return Err(invalid());
            }
            let staging = {
                dir.try_lock().map_err(io::Error::other)?;
                let _lock = DirectoryLock(&dir);
                let staging = staging_directory(path, metadata.uid())?;
                dir.sync_all()?;
                staging
            };
            // A newly created 0600 probe verifies ownership/writability without
            // obtaining a UID via unsafe FFI, changing existing permissions, or
            // trusting environment variables. The fixed staging name is also
            // reserved by every writer and never clobbered.
            let cache = Self {
                path: path.to_owned(),
                dir,
                staging,
                writer: Mutex::new(()),
            };
            {
                cache.dir.try_lock().map_err(io::Error::other)?;
                let _lock = DirectoryLock(&cache.dir);
                cache.recover_pending()?;
                let pending = cache.pending()?;
                if pending.file.metadata()?.uid() != metadata.uid() {
                    return Err(invalid());
                }
            }
            Ok(cache)
        }

        fn validate_directory(&self) -> io::Result<()> {
            directory_path(&self.path, false)?;
            let expected = self.dir.metadata()?;
            let actual = fs::symlink_metadata(&self.path)?;
            if !same_file(&expected, &actual) || actual.mode() & 0o077 != 0 {
                return Err(invalid());
            }
            Ok(())
        }

        fn validate_staging(&self) -> io::Result<()> {
            self.validate_directory()?;
            let path = self.path.join("staging-v1");
            let actual = owned_directory(&path, self.dir.metadata()?.uid())?;
            if !same_file(&self.staging.metadata()?, &actual.metadata()?) {
                return Err(invalid());
            }
            validate_marker(
                &path.join("kadr-artwork-owned-v1"),
                self.dir.metadata()?.uid(),
            )?;
            Ok(())
        }

        // Called only under the root directory lock. The exact reserved name
        // in this marked private namespace is ours even if a hard stop happened
        // before its first byte was written. Unknown names/types are untouched.
        fn recover_pending(&self) -> io::Result<()> {
            self.validate_staging()?;
            let path = self.path.join("staging-v1/pending.art");
            let file = match OpenOptions::new()
                .read(true)
                .custom_flags(NOFOLLOW | NONBLOCK)
                .open(&path)
            {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
                Err(error) => return Err(error),
            };
            regular(&file, self.dir.metadata()?.uid())?;
            let metadata = file.metadata()?;
            if metadata.len() > MAX_RECORD_BYTES as u64 {
                return Err(invalid());
            }
            self.validate_staging()?;
            if !same_file(&metadata, &fs::symlink_metadata(&path)?) {
                return Err(invalid());
            }
            fs::remove_file(path)?;
            self.staging.sync_all()
        }

        fn pending(&self) -> io::Result<Pending<'_>> {
            self.validate_staging()?;
            let path = self.path.join("staging-v1/pending.art");
            let file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(NOFOLLOW | NONBLOCK)
                .open(&path)?;
            Ok(Pending {
                file,
                cache: self,
                path,
                published: false,
            })
        }

        fn record(&self, slot: &str) -> io::Result<Option<(String, Image)>> {
            self.validate_directory()?;
            let file = match OpenOptions::new()
                .read(true)
                .custom_flags(NOFOLLOW | NONBLOCK)
                .open(self.path.join(slot))
            {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error),
            };
            regular(&file, self.dir.metadata()?.uid())?;
            self.validate_directory()?;
            if file.metadata()?.len() > MAX_RECORD_BYTES as u64 {
                return Err(invalid());
            }
            let mut bytes = Vec::new();
            file.take(MAX_RECORD_BYTES as u64 + 1)
                .read_to_end(&mut bytes)?;
            // Validate even a collision before deciding it is ours to replace.
            let (url, image) = decode(&bytes)?;
            Ok(Some((url.to_owned(), image)))
        }

        pub(crate) fn get(&self, url: &str) -> io::Result<Option<Image>> {
            if url.is_empty() || url.len() > MAX_URL_BYTES {
                return Err(invalid());
            }
            let Some((stored_url, image)) = self.record(&slot(url))? else {
                return Ok(None);
            };
            Ok((stored_url == url).then_some(image))
        }

        pub(crate) fn put(&self, url: &str, image: &Image) -> io::Result<()> {
            let record = encode(url, image)?;
            let _writer = self.writer.lock().map_err(|_| invalid())?;
            // Separate cache instances/processes serialize on the pinned
            // directory inode. Nonblocking: contention fails closed.
            self.dir.try_lock().map_err(io::Error::other)?;
            let _lock = DirectoryLock(&self.dir);
            let target = slot(url);
            self.record(&target)?;
            self.recover_pending()?;
            // Never truncate an existing file; recovery recognizes only this
            // private namespace's bounded staging file under the same lock.
            let mut pending = self.pending()?;
            pending.file.write_all(&record)?;
            pending.file.sync_all()?;
            self.validate_staging()?;
            self.record(&target)?;
            fs::rename(&pending.path, self.path.join(target))?;
            pending.published = true;
            self.dir.sync_all()?;
            self.staging.sync_all()
        }
    }

    struct DirectoryLock<'a>(&'a File);
    impl Drop for DirectoryLock<'_> {
        fn drop(&mut self) {
            let _ = self.0.unlock();
        }
    }

    struct Pending<'a> {
        file: File,
        cache: &'a Cache,
        path: PathBuf,
        published: bool,
    }
    impl Drop for Pending<'_> {
        fn drop(&mut self) {
            if !self.published
                && self.cache.validate_staging().is_ok()
                && let (Ok(expected), Ok(actual)) =
                    (self.file.metadata(), fs::symlink_metadata(&self.path))
                && same_file(&expected, &actual)
            {
                let _ = fs::remove_file(&self.path);
            }
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) use platform::Cache;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) struct Cache;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
impl Cache {
    pub(super) fn open(_: &std::path::Path) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "safe artwork caching is unavailable on this platform",
        ))
    }
    pub(super) fn get(&self, _: &str) -> io::Result<Option<Image>> {
        Err(invalid())
    }
    pub(super) fn put(&self, _: &str, _: &Image) -> io::Result<()> {
        Err(invalid())
    }
}

#[cfg(test)]
pub(super) struct TestDirectory(pub(super) std::path::PathBuf);

#[cfg(test)]
impl TestDirectory {
    pub(super) fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        // Resolve only the OS temp root for tests (Darwin's /var alias), never
        // the application cache or user-controlled descendants in production.
        let root = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        loop {
            let path = root.join(format!(
                "kadr-artwork-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("{error}"),
            }
        }
    }
}

#[cfg(test)]
impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::{PermissionsExt, symlink},
    };

    const URL: &str = "https://media.trakt.tv/movies/1398468/medium/poster.webp";

    fn image() -> Image {
        Image::new("image/webp", b"RIFF\x04\0\0\0WEBP".to_vec()).unwrap()
    }

    fn cache(temp: &TestDirectory) -> Cache {
        Cache::open(&temp.0.join("catalog-images-v1")).unwrap()
    }

    #[test]
    fn persists_validated_hit_across_reopen() {
        let temp = TestDirectory::new();
        let cache = cache(&temp);
        assert!(cache.get(URL).unwrap().is_none());
        cache.put(URL, &image()).unwrap();
        drop(cache);
        let reopened = self::cache(&temp);
        let hit = reopened.get(URL).unwrap().unwrap();
        assert_eq!(hit.mime, "image/webp");
        assert_eq!(hit.bytes, image().bytes);
        let files: Vec<_> = fs::read_dir(temp.0.join("catalog-images-v1"))
            .unwrap()
            .collect();
        assert_eq!(files.len(), 2, "only a slot and staging namespace remain");
        assert!(
            !temp
                .0
                .join("catalog-images-v1/staging-v1/pending.art")
                .exists()
        );
    }

    #[test]
    fn accepts_only_bounded_raster_records_and_detects_corruption() {
        for (mime, bytes) in [
            ("image/jpeg", b"\xff\xd8\xff".as_slice()),
            ("image/png", b"\x89PNG\r\n\x1a\n".as_slice()),
            ("image/webp", b"RIFF\x04\0\0\0WEBP".as_slice()),
            ("image/gif", b"GIF89a".as_slice()),
        ] {
            let image = Image::new(mime, bytes.to_vec()).unwrap();
            let record = encode(URL, &image).unwrap();
            let (url, decoded) = decode(&record).unwrap();
            assert_eq!(url, URL);
            assert_eq!(decoded.bytes, bytes);
            assert_eq!(decoded.mime, mime);
            let mut corrupt = record.clone();
            *corrupt.last_mut().unwrap() ^= 1;
            assert!(decode(&corrupt).is_err());
            assert!(decode(&record[..record.len() - 1]).is_err());
            let mut extra = record;
            extra.push(0);
            assert!(decode(&extra).is_err());
        }
        assert!(Image::new("image/svg+xml", b"<svg/>".to_vec()).is_err());
        assert!(Image::new("image/jpeg", b"<html>error</html>".to_vec()).is_err());
        assert!(Image::new("image/png", Vec::new()).is_err());
        let mut large = vec![0; MAX_IMAGE_BYTES];
        large[..3].copy_from_slice(b"\xff\xd8\xff");
        let maximum = Image::new("image/jpeg", large.clone()).unwrap();
        assert!(encode(&"u".repeat(MAX_URL_BYTES), &maximum).unwrap().len() <= MAX_RECORD_BYTES);
        assert!(encode(&"u".repeat(MAX_URL_BYTES + 1), &maximum).is_err());
        large.push(0);
        assert!(Image::new("image/jpeg", large).is_err());
    }

    #[test]
    fn collisions_replace_only_their_slot_and_never_return_the_wrong_url() {
        let temp = TestDirectory::new();
        let cache = cache(&temp);
        let collision = (0..10_000)
            .map(|id| format!("https://media.trakt.tv/movies/{id}/medium/poster.webp"))
            .find(|url| url != URL && slot(url) == slot(URL))
            .unwrap();
        cache.put(URL, &image()).unwrap();
        assert!(cache.get(&collision).unwrap().is_none());
        cache.put(&collision, &image()).unwrap();
        assert!(cache.get(URL).unwrap().is_none());
        assert!(cache.get(&collision).unwrap().is_some());
    }

    #[test]
    fn arbitrary_many_urls_stay_within_the_fixed_budget_and_leave_unrelated_files() {
        let temp = TestDirectory::new();
        let cache = cache(&temp);
        let path = temp.0.join("catalog-images-v1");
        fs::write(path.join("unrelated.txt"), b"keep").unwrap();
        for id in 0..512 {
            let url = format!("https://media.trakt.tv/movies/{id}/medium/poster.webp");
            cache.put(&url, &image()).unwrap();
        }
        let mut records = 0;
        let mut total = 0;
        for file in fs::read_dir(&path).unwrap() {
            let file = file.unwrap();
            if file.file_name() != "unrelated.txt" && file.file_name() != "staging-v1" {
                records += 1;
                total += file.metadata().unwrap().len();
                assert!(file.metadata().unwrap().len() <= MAX_RECORD_BYTES as u64);
            }
        }
        assert_eq!(records, SLOTS);
        assert!(total <= SLOTS * MAX_RECORD_BYTES as u64);
        assert_eq!(fs::read(path.join("unrelated.txt")).unwrap(), b"keep");
    }

    #[test]
    fn invalid_slots_and_unrelated_root_staging_are_never_overwritten_or_deleted() {
        let temp = TestDirectory::new();
        let cache = cache(&temp);
        let path = temp.0.join("catalog-images-v1");
        let slot_path = path.join(slot(URL));
        fs::write(&slot_path, b"unrelated").unwrap();
        fs::set_permissions(&slot_path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(cache.get(URL).is_err());
        assert!(cache.put(URL, &image()).is_err());
        assert_eq!(fs::read(&slot_path).unwrap(), b"unrelated");
        fs::remove_file(&slot_path).unwrap();
        fs::write(path.join("pending.art"), b"do not delete").unwrap();
        cache.put(URL, &image()).unwrap();
        assert!(Cache::open(&path).is_ok());
        assert_eq!(
            fs::read(path.join("pending.art")).unwrap(),
            b"do not delete"
        );
    }

    #[test]
    fn recovers_empty_partial_and_complete_staging_after_hard_stop() {
        let temp = TestDirectory::new();
        let path = temp.0.join("catalog-images-v1");
        let cache = cache(&temp);
        cache.put(URL, &image()).unwrap();
        drop(cache);
        let staging = path.join("staging-v1");
        fs::write(staging.join("unrelated.txt"), b"keep").unwrap();
        for interrupted in [Vec::new(), MAGIC.to_vec(), encode(URL, &image()).unwrap()] {
            let pending = staging.join("pending.art");
            fs::write(&pending, interrupted).unwrap();
            fs::set_permissions(&pending, fs::Permissions::from_mode(0o600)).unwrap();
            let reopened = Cache::open(&path).unwrap();
            assert!(!pending.exists(), "recognized crash remnant is reclaimed");
            assert!(reopened.get(URL).unwrap().is_some());
            reopened.put(URL, &image()).unwrap();
            assert!(!pending.exists(), "subsequent writes remain available");
            assert_eq!(fs::read(staging.join("unrelated.txt")).unwrap(), b"keep");
        }
    }

    #[test]
    fn recovers_empty_namespace_left_before_atomic_ownership_marker() {
        let temp = TestDirectory::new();
        let path = temp.0.join("catalog-images-v1");
        fs::create_dir_all(path.join("staging-v1")).unwrap();
        for directory in [&path, &path.join("staging-v1")] {
            fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let cache = Cache::open(&path).unwrap();
        cache.put(URL, &image()).unwrap();
        assert!(cache.get(URL).unwrap().is_some());
    }

    #[test]
    fn recovery_never_reclaims_an_active_writers_staging_file() {
        let temp = TestDirectory::new();
        let cache = cache(&temp);
        let path = temp.0.join("catalog-images-v1");
        let pending = path.join("staging-v1/pending.art");
        fs::write(&pending, b"active writer").unwrap();
        fs::set_permissions(&pending, fs::Permissions::from_mode(0o600)).unwrap();
        let lock = fs::File::open(&path).unwrap();
        lock.try_lock().unwrap();
        assert!(Cache::open(&path).is_err());
        assert!(cache.put(URL, &image()).is_err());
        assert_eq!(fs::read(&pending).unwrap(), b"active writer");
        lock.unlock().unwrap();
        let reopened = Cache::open(&path).unwrap();
        assert!(!pending.exists());
        reopened.put(URL, &image()).unwrap();
    }

    #[test]
    fn refuses_to_reclaim_unowned_or_unsafe_staging() {
        let temp = TestDirectory::new();
        let cache = cache(&temp);
        let path = temp.0.join("catalog-images-v1");
        let staging = path.join("staging-v1");
        let pending = staging.join("pending.art");
        let marker = staging.join("kadr-artwork-owned-v1");
        fs::remove_dir(&marker).unwrap();
        fs::write(&pending, b"unknown namespace").unwrap();
        fs::set_permissions(&pending, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(Cache::open(&path).is_err());
        assert!(cache.put(URL, &image()).is_err());
        assert_eq!(fs::read(&pending).unwrap(), b"unknown namespace");
        fs::remove_file(&pending).unwrap();
        fs::create_dir(&marker).unwrap();
        fs::set_permissions(&marker, fs::Permissions::from_mode(0o700)).unwrap();
        let outside = temp.0.join("outside");
        fs::write(&outside, b"outside secret").unwrap();
        fs::set_permissions(&outside, fs::Permissions::from_mode(0o600)).unwrap();
        symlink(&outside, &pending).unwrap();
        assert!(Cache::open(&path).is_err());
        assert!(cache.put(URL, &image()).is_err());
        assert_eq!(fs::read(&outside).unwrap(), b"outside secret");
        fs::remove_file(&pending).unwrap();
        fs::hard_link(&outside, &pending).unwrap();
        assert!(Cache::open(&path).is_err());
        assert!(cache.put(URL, &image()).is_err());
        assert_eq!(fs::read(&outside).unwrap(), b"outside secret");
        fs::remove_file(&pending).unwrap();
        let too_large = fs::File::create(&pending).unwrap();
        too_large.set_len(MAX_RECORD_BYTES as u64 + 1).unwrap();
        fs::set_permissions(&pending, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(Cache::open(&path).is_err());
        assert!(cache.put(URL, &image()).is_err());
        assert_eq!(
            pending.metadata().unwrap().len(),
            MAX_RECORD_BYTES as u64 + 1
        );
    }

    #[test]
    fn refuses_symlinked_roots_ancestors_slots_and_hardlinks() {
        let temp = TestDirectory::new();
        let cache = cache(&temp);
        let path = temp.0.join("catalog-images-v1");
        let outside = temp.0.join("outside");
        fs::create_dir(&outside).unwrap();
        symlink(&path, temp.0.join("root-link")).unwrap();
        assert!(Cache::open(&temp.0.join("root-link")).is_err());
        symlink(&outside, temp.0.join("ancestor-link")).unwrap();
        assert!(Cache::open(&temp.0.join("ancestor-link/child")).is_err());
        assert!(!outside.join("child").exists());
        let outside_file = outside.join("private");
        fs::write(&outside_file, b"outside secret").unwrap();
        let slot_path = path.join(slot(URL));
        symlink(&outside_file, &slot_path).unwrap();
        assert!(cache.get(URL).is_err());
        assert!(cache.put(URL, &image()).is_err());
        fs::remove_file(&slot_path).unwrap();
        fs::hard_link(&outside_file, &slot_path).unwrap();
        assert!(cache.get(URL).is_err());
        assert!(cache.put(URL, &image()).is_err());
        assert_eq!(fs::read(&outside_file).unwrap(), b"outside secret");
    }

    #[test]
    fn detects_root_replacement_and_unsafe_permissions() {
        let temp = TestDirectory::new();
        let cache = cache(&temp);
        let path = temp.0.join("catalog-images-v1");
        fs::rename(&path, temp.0.join("moved")).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(cache.get(URL).is_err());
        assert!(cache.put(URL, &image()).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(Cache::open(&path).is_err());
        assert!(Cache::open(std::path::Path::new("relative/cache")).is_err());
        assert!(Cache::open(&temp.0.join("unused/../escape")).is_err());
        assert!(!temp.0.join("unused").exists());
    }
}
