//! Private, crash-safe files and bounded cross-process initialization locks.
use fs2::FileExt;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::time::{Duration, Instant};

pub fn directory(path: &Path) -> io::Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    let meta = std::fs::symlink_metadata(path)?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(io::Error::other("private directory is not a directory"));
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

fn lock_file(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("private lock is not a regular file"));
    }
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

pub fn try_lock(path: &Path) -> io::Result<File> {
    let file = lock_file(path)?;
    file.try_lock_exclusive()?;
    Ok(file)
}

pub fn lock(path: &Path) -> io::Result<File> {
    lock_guarded(path, || Ok(()))
}

/// Keep authorization/cancellation active while waiting for another process.
pub(crate) fn lock_guarded(
    path: &Path,
    mut check: impl FnMut() -> io::Result<()>,
) -> io::Result<File> {
    check()?;
    let file = lock_file(path)?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        check()?;
        match file.try_lock_exclusive() {
            Ok(()) => {
                check()?;
                return Ok(file);
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => return Err(e),
        }
    }
}

pub fn read_bounded(path: &Path, limit: usize) -> io::Result<Vec<u8>> {
    read_file(path, limit, false)
}

/// Identity migration explicitly protects legacy private material on its
/// opened descriptor. Ordinary reads never chmod files or mutate timestamps.
pub fn read_private(path: &Path, limit: usize) -> io::Result<Vec<u8>> {
    read_file(path, limit, true)
}

fn read_file(path: &Path, limit: usize, protect: bool) -> io::Result<Vec<u8>> {
    use std::io::Read;
    let file = open_regular(path)?;
    let metadata = file.metadata()?;
    if protect && metadata.permissions().mode() & 0o777 != 0o600 {
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "private file exceeds its format limit",
        ));
    }
    Ok(bytes)
}

/// Validate the opened descriptor before reading a prefix or changing modes.
/// O_NONBLOCK prevents a FIFO from waiting for a writer even during open().
pub(crate) fn open_regular(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::other("private data is not a regular file"));
    }
    Ok(file)
}

pub fn replace(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    #[cfg(test)]
    replacement_fault::point("created", path);
    tmp.write_all(bytes)?;
    #[cfg(test)]
    replacement_fault::point("written", path);
    tmp.as_file().sync_all()?;
    #[cfg(test)]
    replacement_fault::point("file-synced", path);
    tmp.persist(path).map_err(|e| e.error)?;
    #[cfg(test)]
    replacement_fault::point("renamed", path);
    File::open(parent)?.sync_all()?;
    #[cfg(test)]
    replacement_fault::point("directory-synced", path);
    Ok(())
}

// Test-only, thread-local barriers keep fault injection out of the production
// binary and out of unrelated parallel tests. The child owning the barrier is
// killed while the real replacement still owns its initialization lock.
#[cfg(test)]
pub(crate) mod replacement_fault {
    use std::cell::RefCell;
    use std::path::Path;

    type Hook = Box<dyn Fn(&str, &Path)>;
    thread_local! {
        static HOOK: RefCell<Option<Hook>> = const { RefCell::new(None) };
    }

    pub(crate) fn install(hook: impl Fn(&str, &Path) + 'static) {
        HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
    }

    pub(crate) fn point(phase: &str, path: &Path) {
        HOOK.with(|slot| {
            if let Some(hook) = slot.borrow().as_ref() {
                hook(phase, path);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, symlink};

    #[test]
    fn ordinary_reads_preserve_metadata_and_private_reads_protect_legacy_keys() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("synthetic-key");
        std::fs::write(&path, [17; 32]).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let before = std::fs::metadata(&path).unwrap();
        assert_eq!(read_bounded(&path, 32).unwrap(), vec![17; 32]);
        let after = std::fs::metadata(&path).unwrap();
        assert_eq!(after.mode(), before.mode());
        assert_eq!(
            (after.ctime(), after.ctime_nsec()),
            (before.ctime(), before.ctime_nsec())
        );
        assert_eq!(read_private(&path, 32).unwrap(), vec![17; 32]);
        let protected = std::fs::metadata(&path).unwrap();
        assert_eq!(protected.permissions().mode() & 0o777, 0o600);
        read_private(&path, 32).unwrap();
        let reread = std::fs::metadata(&path).unwrap();
        assert_eq!(
            (reread.ctime(), reread.ctime_nsec()),
            (protected.ctime(), protected.ctime_nsec())
        );
    }

    #[test]
    fn bounded_read_rejects_oversized_files_and_symlinks_without_modifying_them() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("synthetic-data");
        std::fs::write(&path, b"too large").unwrap();
        assert_eq!(
            read_bounded(&path, 2).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"too large");
        let link = root.path().join("link");
        symlink(&path, &link).unwrap();
        assert!(read_bounded(&link, 32).is_err());
        assert!(read_private(&link, 32).is_err());
    }

    #[test]
    fn fifo_reads_and_locks_fail_without_waiting_for_a_writer() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("fifo");
        let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        // No writer is opened: a blocking read would hang rather than reach
        // the format bound. Keep the test process recoverable on regression.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let (send, receive) = std::sync::mpsc::channel();
        let task = std::thread::spawn(move || {
            let result = (
                read_bounded(&path, 32),
                read_private(&path, 32),
                try_lock(&path),
            );
            send.send(result).unwrap();
        });
        let (read, private, lock) = receive.recv_timeout(Duration::from_millis(500)).unwrap();
        assert!(read.is_err() && private.is_err() && lock.is_err());
        task.join().unwrap();
    }
}
