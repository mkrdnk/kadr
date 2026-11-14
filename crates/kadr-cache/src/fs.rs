use std::{
    fs::File,
    path::{Component, Path},
};

use cap_std::fs::Dir;
use rustix::fs::{AtFlags, Mode, OFlags};

use crate::{Error, Result, io_error};

pub(crate) fn open_dir(parent: &Dir, name: &Path) -> Result<Dir> {
    let fd = rustix::fs::openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| Error::UnsafePath)?;
    Ok(Dir::from_std_file(File::from(fd)))
}

/// Walk every component with O_NOFOLLOW, including ancestors of the chosen root.
pub(crate) fn absolute_dir(path: &Path, create: bool) -> Result<Dir> {
    if !path.is_absolute() {
        return Err(Error::InvalidInput);
    }
    let mut dir = Dir::open_ambient_dir("/", cap_std::ambient_authority()).map_err(io_error)?;
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                if create {
                    match rustix::fs::mkdirat(&dir, name, Mode::from_raw_mode(0o700)) {
                        Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                        Err(error) => return Err(io_error(error.into())),
                    }
                }
                dir = open_dir(&dir, Path::new(name))?;
            }
            _ => return Err(Error::InvalidInput),
        }
    }
    Ok(dir)
}

pub(crate) fn open_file(dir: &Dir, name: &str, create: bool) -> Result<File> {
    let mut flags = OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
    if create {
        flags |= OFlags::CREATE;
    }
    let fd = rustix::fs::openat(dir, name, flags, Mode::from_raw_mode(0o600))
        .map_err(|error| io_error(error.into()))?;
    let file = File::from(fd);
    let metadata = file.metadata().map_err(io_error)?;
    use std::os::unix::fs::MetadataExt;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(Error::UnsafePath);
    }
    Ok(file)
}

pub(crate) fn new_file(dir: &Dir, name: &str) -> Result<File> {
    let fd = rustix::fs::openat(
        dir,
        name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )
    .map_err(|error| io_error(error.into()))?;
    Ok(File::from(fd))
}

pub(crate) fn sync_dir(dir: &Dir) -> Result<()> {
    rustix::fs::fsync(dir).map_err(|error| io_error(error.into()))
}

pub(crate) fn free_space(dir: &Dir) -> Result<u64> {
    let stats = rustix::fs::fstatvfs(dir).map_err(|error| io_error(error.into()))?;
    Ok(stats.f_bavail.saturating_mul(stats.f_frsize))
}

pub(crate) fn ensure_space(available: u64, needed: u64) -> Result<()> {
    if available < needed {
        Err(Error::OutOfSpace)
    } else {
        Ok(())
    }
}

pub(crate) fn exists(dir: &Dir, name: impl AsRef<Path>) -> Result<bool> {
    match dir.symlink_metadata(name) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(io_error(error)),
    }
}

/// Never follows links, even during recursive traversal. Reject special files.
pub(crate) fn tree_size(dir: &Dir) -> Result<u64> {
    tree_bytes(dir, false)
}

pub(crate) fn tree_allocated(dir: &Dir) -> Result<u64> {
    tree_bytes(dir, true)
}

fn tree_bytes(dir: &Dir, allocated: bool) -> Result<u64> {
    let mut size = 0u64;
    for entry in dir.entries().map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        let name = entry.file_name();
        let metadata = dir.symlink_metadata(&name).map_err(io_error)?;
        let bytes = if metadata.is_dir() {
            tree_bytes(&open_dir(dir, Path::new(&name))?, allocated)?
        } else if metadata.is_file() {
            if allocated {
                let stat = rustix::fs::statat(dir, &name, AtFlags::SYMLINK_NOFOLLOW)
                    .map_err(|error| io_error(error.into()))?;
                // Block padding/preallocation in one file cannot satisfy
                // unwritten bytes in another file's reservation.
                (stat.st_blocks as u64)
                    .saturating_mul(512)
                    .min(metadata.len())
            } else {
                metadata.len()
            }
        } else {
            return Err(Error::UnsafePath);
        };
        size = size.checked_add(bytes).ok_or(Error::QuotaExceeded)?;
    }
    Ok(size)
}

/// Unlink leaves (including symlinks) without opening their targets. Opening
/// directories is NOFOLLOW, so a replaced directory can only cause an error.
pub(crate) fn remove_tree(parent: &Dir, name: &Path) -> Result<()> {
    let metadata = match parent.symlink_metadata(name) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(io_error(error)),
    };
    if metadata.is_dir() {
        let dir = open_dir(parent, name)?;
        for entry in dir.entries().map_err(io_error)? {
            remove_tree(&dir, Path::new(&entry.map_err(io_error)?.file_name()))?;
        }
        rustix::fs::unlinkat(parent, name, AtFlags::REMOVEDIR)
            .map_err(|error| io_error(error.into()))?;
    } else {
        rustix::fs::unlinkat(parent, name, AtFlags::empty())
            .map_err(|error| io_error(error.into()))?;
    }
    Ok(())
}

pub(crate) fn valid_key(key: &str) -> bool {
    !key.is_empty() && key.len() <= 128 && key.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

pub(crate) fn token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
