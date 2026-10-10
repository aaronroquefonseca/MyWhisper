//! Cancellable verified downloads and atomic installation into owned directories.
use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

fn epoch(path: &Path) -> Result<u64, String> {
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e.to_string()),
    };
    let mut bytes = Vec::new();
    file.take(9)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    let bytes: [u8; 8] = bytes.try_into().map_err(|_| "invalid installation epoch")?;
    Ok(u64::from_le_bytes(bytes))
}
fn advance(path: &Path) -> Result<(), String> {
    let next = epoch(path)?
        .checked_add(1)
        .ok_or("installation epoch overflow")?;
    crate::private_fs::replace(path, &next.to_le_bytes()).map_err(|e| e.to_string())
}
fn shared_lock(path: &Path) -> Result<File, String> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|e| e.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match fs2::FileExt::try_lock_shared(&file) {
            Ok(()) => return Ok(file),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(e.to_string()),
        }
        if Instant::now() >= deadline {
            return Err("installation reset lock deadline exceeded".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn valid_key(key: &str) -> bool {
    key.len() <= 128
        && key
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.'))
}

pub(crate) struct Stage {
    temporary: tempfile::TempDir,
    _global: File,
    _asset: File,
    destination: PathBuf,
    data: PathBuf,
    global_epoch: (PathBuf, u64),
    asset_epoch: (PathBuf, u64),
}
impl Stage {
    pub fn new(data: &Path, key: &str, destination: PathBuf) -> Result<Self, String> {
        if !valid_key(key) {
            return Err("invalid artifact key".into());
        }
        if crate::reset::requested(data).map_err(|error| error.to_string())? {
            return Err("reset cleanup is pending; installation is disabled".into());
        }
        let control = data.join(".install");
        crate::private_fs::directory(&control).map_err(|e| e.to_string())?;
        let global_epoch = control.join("epoch");
        let asset_epoch = control.join(format!("{key}.epoch"));
        let saved_global = epoch(&global_epoch)?;
        let saved_asset = epoch(&asset_epoch)?;
        let global = shared_lock(&control.join("reset.lock"))?;
        let asset = crate::private_fs::lock(&control.join(format!("{key}.lock")))
            .map_err(|e| e.to_string())?;
        if crate::reset::requested(data).map_err(|error| error.to_string())?
            || epoch(&global_epoch)? != saved_global
            || epoch(&asset_epoch)? != saved_asset
        {
            return Err("installation cancelled before startup".into());
        }
        let parent = destination.parent().ok_or("artifact has no parent")?;
        crate::private_fs::directory(parent).map_err(|e| e.to_string())?;
        let temporary = tempfile::Builder::new()
            .prefix(".mywhisper-install-")
            .tempdir_in(parent)
            .map_err(|e| e.to_string())?;
        Ok(Self {
            temporary,
            _global: global,
            _asset: asset,
            destination,
            data: data.to_owned(),
            global_epoch: (global_epoch, saved_global),
            asset_epoch: (asset_epoch, saved_asset),
        })
    }
    pub fn path(&self) -> &Path {
        self.temporary.path()
    }
    pub fn check(&self, cancel: &AtomicBool) -> Result<(), String> {
        if cancel.load(Ordering::Acquire)
            || crate::reset::requested(&self.data).map_err(|error| error.to_string())?
            || epoch(&self.global_epoch.0)? != self.global_epoch.1
            || epoch(&self.asset_epoch.0)? != self.asset_epoch.1
        {
            return Err("cancelled".into());
        }
        Ok(())
    }
    pub fn promote(self, cancel: &AtomicBool) -> Result<(), String> {
        use std::os::unix::ffi::OsStrExt;
        self.check(cancel)?;
        // Serialize the final check/rename against delete/reset epoch changes.
        let metadata = self.global_epoch.0.parent().unwrap().join("metadata.lock");
        let _metadata = crate::private_fs::lock(&metadata).map_err(|e| e.to_string())?;
        self.check(cancel)?;
        if self.destination.exists() {
            if std::fs::symlink_metadata(&self.destination)
                .map_err(|e| e.to_string())?
                .file_type()
                .is_symlink()
            {
                return Err("artifact destination is a symlink".into());
            }
            let from = std::ffi::CString::new(self.path().as_os_str().as_bytes())
                .map_err(|_| "invalid staging path")?;
            let to = std::ffi::CString::new(self.destination.as_os_str().as_bytes())
                .map_err(|_| "invalid artifact path")?;
            // SAFETY: both directories are on the same filesystem, exclusively
            // owned under the installation locks. Exchange is atomic.
            if unsafe {
                libc::renameat2(
                    libc::AT_FDCWD,
                    from.as_ptr(),
                    libc::AT_FDCWD,
                    to.as_ptr(),
                    libc::RENAME_EXCHANGE,
                )
            } < 0
            {
                return Err(format!(
                    "atomic installation: {}",
                    std::io::Error::last_os_error()
                ));
            }
        } else {
            std::fs::rename(self.path(), &self.destination).map_err(|e| e.to_string())?;
        }
        File::open(self.destination.parent().unwrap())
            .and_then(|parent| parent.sync_all())
            .map_err(|e| e.to_string())?;
        // TempDir now owns the previous installation after exchange.
        Ok(())
    }
}

pub(crate) fn delete(data: &Path, key: &str, destination: &Path) -> std::io::Result<()> {
    if !valid_key(key) {
        return Err(std::io::Error::other("invalid artifact key"));
    }
    let control = data.join(".install");
    crate::private_fs::directory(&control)?;
    let _global = shared_lock(&control.join("reset.lock")).map_err(std::io::Error::other)?;
    let metadata = crate::private_fs::lock(&control.join("metadata.lock"))?;
    advance(&control.join(format!("{key}.epoch"))).map_err(std::io::Error::other)?;
    drop(metadata);
    let _asset = crate::private_fs::lock(&control.join(format!("{key}.lock")))?;
    if destination.exists() {
        std::fs::remove_dir_all(destination)?;
    }
    Ok(())
}

/// Cancel active/queued installations before taking exclusive reset ownership.
pub(crate) fn reset(data: &Path) -> std::io::Result<File> {
    let control = data.join(".install");
    crate::private_fs::directory(&control)?;
    let metadata = crate::private_fs::lock(&control.join("metadata.lock"))?;
    advance(&control.join("epoch")).map_err(std::io::Error::other)?;
    drop(metadata);
    crate::private_fs::lock(&control.join("reset.lock"))
}

pub(crate) struct Source<'a> {
    pub url: &'a str,
    pub sha256: &'a str,
    pub limit: u64,
}
async fn cancellable<F: std::future::Future>(
    future: F,
    check: &impl Fn() -> Result<(), String>,
    limit: Duration,
) -> Result<F::Output, String> {
    let deadline = Instant::now() + limit;
    let mut pending = Box::pin(future);
    loop {
        check()?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("download progress deadline exceeded".into());
        }
        if let Ok(result) =
            tokio::time::timeout(remaining.min(Duration::from_millis(50)), pending.as_mut()).await
        {
            return Ok(result);
        }
    }
}
pub(crate) fn download(
    source: Source<'_>,
    path: &Path,
    check: impl Fn() -> Result<(), String>,
    progress: impl Fn(u64, Option<u64>),
) -> Result<u64, String> {
    use sha2::Digest;
    if source.sha256.len() != 64 || !source.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("artifact has no pinned SHA-256".into());
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let result = runtime.block_on(async {
        check()?;
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .read_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(7200))
            .build()
            .map_err(|e| e.without_url().to_string())?;
        let mut response = cancellable(
            client.get(source.url).send(),
            &check,
            Duration::from_secs(10),
        )
        .await?
        .map_err(|e| e.without_url().to_string())?;
        if !response.status().is_success() {
            return Err(format!("download HTTP {}", response.status()));
        }
        let total = response.content_length();
        if total.is_some_and(|total| total > source.limit) {
            return Err("download size limit exceeded".into());
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .map_err(|e| e.to_string())?;
        let mut digest = sha2::Sha256::new();
        let mut received = 0u64;
        let deadline = Instant::now() + Duration::from_secs(7200);
        loop {
            let limit = deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_secs(5));
            let chunk = cancellable(response.chunk(), &check, limit)
                .await?
                .map_err(|e| e.without_url().to_string())?;
            let Some(chunk) = chunk else {
                break;
            };
            if chunk.len() as u64 > source.limit.saturating_sub(received) {
                return Err("download size limit exceeded".into());
            }
            file.write_all(&chunk)
                .map_err(|e| format!("download write: {e}"))?;
            digest.update(&chunk);
            received += chunk.len() as u64;
            progress(received, total);
        }
        check()?;
        if total.is_some_and(|total| total != received) {
            return Err("download length mismatch".into());
        }
        let hash = digest
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        if hash != source.sha256 {
            return Err("download SHA-256 mismatch".into());
        }
        file.sync_all().map_err(|e| e.to_string())?;
        Ok(received)
    });
    runtime.shutdown_timeout(Duration::from_millis(100));
    result
}

fn member(root: &Path, name: &Path) -> Result<PathBuf, String> {
    if name.components().any(|component| {
        !matches!(
            component,
            std::path::Component::Normal(_) | std::path::Component::CurDir
        )
    }) {
        return Err("archive path escapes installation".into());
    }
    let path = root.join(name);
    let parent = path.parent().ok_or("archive path has no parent")?;
    crate::private_fs::directory(parent).map_err(|e| e.to_string())?;
    Ok(path)
}
fn copy_member(
    reader: &mut impl Read,
    path: &Path,
    size: u64,
    total: &mut u64,
    check: &impl Fn() -> Result<(), String>,
) -> Result<(), String> {
    *total = total
        .checked_add(size)
        .filter(|bytes| *bytes <= 12 * 1024 * 1024 * 1024)
        .ok_or("archive expansion limit exceeded")?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|e| e.to_string())?;
    let mut copied = 0u64;
    let mut buffer = [0u8; 65536];
    loop {
        check()?;
        let count = reader.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        copied += count as u64;
        if copied > size {
            return Err("archive member exceeds declared size".into());
        }
        file.write_all(&buffer[..count])
            .map_err(|e| e.to_string())?;
    }
    if copied != size {
        return Err("archive member is truncated".into());
    }
    file.sync_all().map_err(|e| e.to_string())?;
    Ok(())
}
pub(crate) fn unpack_tar(
    archive: &Path,
    root: &Path,
    check: impl Fn() -> Result<(), String>,
) -> Result<(), String> {
    use std::os::unix::fs::symlink;
    let file = File::open(archive).map_err(|e| e.to_string())?;
    let mut archive = tar::Archive::new(bzip2::read::BzDecoder::new(file));
    let mut total = 0u64;
    let mut links = Vec::new();
    for (index, entry) in archive.entries().map_err(|e| e.to_string())?.enumerate() {
        check()?;
        if index >= 4096 {
            return Err("archive entry limit exceeded".into());
        }
        let mut entry = entry.map_err(|e| e.to_string())?;
        let path = member(root, &entry.path().map_err(|e| e.to_string())?)?;
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            crate::private_fs::directory(&path).map_err(|e| e.to_string())?;
        } else if kind.is_file() {
            let size = entry.size();
            copy_member(&mut entry, &path, size, &mut total, &check)?;
        } else if kind.is_symlink() {
            let target = entry
                .link_name()
                .map_err(|e| e.to_string())?
                .ok_or("archive link has no target")?
                .into_owned();
            if target.is_absolute() {
                return Err("archive link escapes installation".into());
            }
            let relative = path
                .parent()
                .unwrap()
                .strip_prefix(root)
                .map_err(|_| "archive link escapes installation")?
                .join(&target);
            let mut depth = 0usize;
            for component in relative.components() {
                match component {
                    std::path::Component::Normal(_) => depth += 1,
                    std::path::Component::CurDir => {}
                    std::path::Component::ParentDir if depth > 0 => depth -= 1,
                    _ => return Err("archive link escapes installation".into()),
                }
            }
            links.push((target, path));
        } else {
            return Err("unsupported archive entry type".into());
        }
    }
    // Links are installed only after every regular member; no member write
    // can follow an archive-controlled symlink.
    for (target, path) in &links {
        check()?;
        symlink(target, path).map_err(|e| e.to_string())?;
    }
    // Lexically safe targets can still escape through another link's parent
    // components. Resolve the complete graph before accepting the installation.
    let canonical_root = root.canonicalize().map_err(|e| e.to_string())?;
    for (_, path) in links {
        check()?;
        let resolved = path
            .canonicalize()
            .map_err(|e| format!("archive link: {e}"))?;
        if !resolved.starts_with(&canonical_root) {
            return Err("archive link escapes installation".into());
        }
    }
    Ok(())
}
pub(crate) fn unpack_zip(
    archive: &Path,
    root: &Path,
    check: impl Fn() -> Result<(), String>,
) -> Result<(), String> {
    let file = File::open(archive).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
    if archive.len() > 4096 {
        return Err("archive entry limit exceeded".into());
    }
    let mut total = 0u64;
    for index in 0..archive.len() {
        check()?;
        let mut entry = archive.by_index(index).map_err(|e| e.to_string())?;
        let name = entry
            .enclosed_name()
            .ok_or("archive path escapes installation")?;
        let path = member(root, &name)?;
        if entry.is_dir() {
            crate::private_fs::directory(&path).map_err(|e| e.to_string())?;
        } else {
            if entry
                .unix_mode()
                .is_some_and(|mode| mode & 0o170000 == 0o120000)
            {
                return Err("zip symlinks are not supported".into());
            }
            let size = entry.size();
            copy_member(&mut entry, &path, size, &mut total, &check)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::{Arc, mpsc};

    fn sha(bytes: &[u8]) -> String {
        use sha2::Digest;
        sha2::Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
    fn accept(listener: &TcpListener) -> std::net::TcpStream {
        listener.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match listener.accept() {
                Ok((socket, _)) => return socket,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "fixture client did not connect before the deadline"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("fixture accept: {error}"),
            }
        }
    }
    fn server(response: Vec<u8>) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/asset", listener.local_addr().unwrap());
        let task = std::thread::spawn(move || {
            let mut socket = accept(&listener);
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = [0; 4096];
            assert!(socket.read(&mut request).unwrap() > 0);
            let _ = socket.write_all(&response);
        });
        (url, task)
    }
    fn response(body: &[u8]) -> Vec<u8> {
        let mut response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        response.extend_from_slice(body);
        response
    }

    #[test]
    fn registry_ids_with_decimal_versions_are_valid_but_path_components_are_not() {
        let data = tempfile::tempdir().unwrap();
        for model in crate::stt::WHISPER_MODELS
            .iter()
            .chain(crate::stt::WHISTLE_MODELS)
            .chain(crate::stt::PARAKEET_MODELS)
        {
            let key = format!("model-{}", model.id);
            assert!(
                Stage::new(data.path(), &key, data.path().join("models/test")).is_ok(),
                "{}",
                model.id
            );
        }
        for key in ["", "../escape", "x/y", "..", ".private", "x\0y"] {
            assert!(Stage::new(data.path(), key, data.path().join("models/test")).is_err());
            assert!(delete(data.path(), key, &data.path().join("models/test")).is_err());
        }
    }

    #[test]
    fn verified_bytes_only_replace_a_complete_installation() {
        let data = tempfile::tempdir().unwrap();
        let destination = data.path().join("models/test");
        crate::private_fs::directory(&destination).unwrap();
        std::fs::write(destination.join("model"), b"previous").unwrap();
        let cancel = AtomicBool::new(false);
        for (expected, succeeds) in [(sha(b"incorrect"), false), (sha(b"replacement"), true)] {
            let stage = Stage::new(data.path(), "model-test", destination.clone()).unwrap();
            let (url, task) = server(response(b"replacement"));
            let result = download(
                Source {
                    url: &url,
                    sha256: &expected,
                    limit: 1024,
                },
                &stage.path().join("model"),
                || stage.check(&cancel),
                |_, _| {},
            );
            assert!(task.join().is_ok(), "fixture client result: {result:?}");
            assert_eq!(
                result.is_ok(),
                succeeds,
                "fixture client result: {result:?}"
            );
            if result.is_err() {
                drop(stage);
                assert_eq!(
                    std::fs::read(destination.join("model")).unwrap(),
                    b"previous"
                );
            } else {
                stage.promote(&cancel).unwrap();
                assert_eq!(
                    std::fs::read(destination.join("model")).unwrap(),
                    b"replacement"
                );
            }
        }
        assert_eq!(
            std::fs::read_dir(destination.parent().unwrap())
                .unwrap()
                .count(),
            1
        );
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(destination.join("model"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn truncated_oversized_and_unknown_length_bodies_fail_closed() {
        let data = tempfile::tempdir().unwrap();
        let hash = sha(b"data");
        for (index, response) in [
            b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\ndata".to_vec(),
            response(b"too large"),
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n9\r\ntoo large\r\n0\r\n\r\n".to_vec(),
        ].into_iter().enumerate() {
            let (url, task) = server(response);
            assert!(download(Source { url: &url, sha256: &hash, limit: 8 }, &data.path().join(index.to_string()), || Ok(()), |_, _| {}).is_err());
            task.join().unwrap();
        }
        // A complete matching body cannot be reported successful when writing fails.
        let (url, task) = server(response(b"data"));
        assert!(
            download(
                Source {
                    url: &url,
                    sha256: &hash,
                    limit: 8
                },
                &data.path().join("missing/file"),
                || Ok(()),
                |_, _| {}
            )
            .is_err()
        );
        task.join().unwrap();
    }

    #[test]
    fn cancellation_interrupts_stalled_headers_and_body_without_server_progress() {
        for send_headers in [false, true] {
            let data = tempfile::tempdir().unwrap();
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}/asset", listener.local_addr().unwrap());
            let (accepted_tx, accepted_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            let server = std::thread::spawn(move || {
                let mut socket = accept(&listener);
                let mut request = [0; 4096];
                assert!(socket.read(&mut request).unwrap() > 0);
                if send_headers {
                    socket
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n")
                        .unwrap();
                }
                accepted_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(3)).unwrap();
            });
            let cancel = Arc::new(AtomicBool::new(false));
            let worker_cancel = cancel.clone();
            let path = data.path().join("part");
            let (done_tx, done_rx) = mpsc::channel();
            let worker = std::thread::spawn(move || {
                let result = download(
                    Source {
                        url: &url,
                        sha256: &sha(b"data"),
                        limit: 100,
                    },
                    &path,
                    || {
                        if worker_cancel.load(Ordering::Acquire) {
                            Err("cancelled".into())
                        } else {
                            Ok(())
                        }
                    },
                    |_, _| {},
                );
                done_tx.send(result).unwrap();
            });
            accepted_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            cancel.store(true, Ordering::Release);
            let result = done_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("cancel must not wait for network progress");
            assert_eq!(result.unwrap_err(), "cancelled");
            release_tx.send(()).unwrap();
            server.join().unwrap();
            worker.join().unwrap();
        }
    }

    #[test]
    fn delete_and_reset_invalidate_active_installers_before_waiting_for_cleanup() {
        for reset_all in [false, true] {
            let data = tempfile::tempdir().unwrap();
            let destination = data.path().join("models/test");
            let stage = Stage::new(data.path(), "model-test", destination.clone()).unwrap();
            std::fs::write(stage.path().join("model"), b"complete").unwrap();
            let root = data.path().to_owned();
            let target = destination.clone();
            let mutation = std::thread::spawn(move || {
                if reset_all {
                    crate::stt::clear_download_cache(&root)
                } else {
                    delete(&root, "model-test", &target)
                }
            });
            let cancel = AtomicBool::new(false);
            let deadline = Instant::now() + Duration::from_secs(2);
            while stage.check(&cancel).is_ok() {
                assert!(
                    Instant::now() < deadline,
                    "invalidation must precede waiting for the installer"
                );
                std::thread::yield_now();
            }
            assert!(stage.promote(&cancel).is_err());
            mutation.join().unwrap().unwrap();
            assert!(
                !destination.exists(),
                "old download cannot resurrect deleted assets"
            );
            let fresh = Stage::new(data.path(), "model-test", destination.clone()).unwrap();
            std::fs::write(fresh.path().join("model"), b"fresh request").unwrap();
            fresh.promote(&cancel).unwrap();
            assert_eq!(
                std::fs::read(destination.join("model")).unwrap(),
                b"fresh request"
            );
        }
    }

    fn archive(path: &Path, members: &[(&str, Option<&str>)]) {
        let encoder =
            bzip2::write::BzEncoder::new(File::create(path).unwrap(), bzip2::Compression::fast());
        let mut archive = tar::Builder::new(encoder);
        for (name, link) in members {
            let mut header = tar::Header::new_gnu();
            header.set_mode(0o644);
            if let Some(target) = link {
                header.set_entry_type(tar::EntryType::Symlink);
                header.set_size(0);
                header.set_link_name(target).unwrap();
                header.set_cksum();
                archive
                    .append_data(&mut header, name, std::io::empty())
                    .unwrap();
            } else {
                header.set_size(4);
                header.set_cksum();
                archive
                    .append_data(&mut header, name, &b"data"[..])
                    .unwrap();
            }
        }
        archive.into_inner().unwrap().finish().unwrap();
    }

    #[test]
    fn extraction_preserves_safe_runtime_links_but_rejects_escape_graphs() {
        let data = tempfile::tempdir().unwrap();
        for (index, members, succeeds) in [
            (
                0,
                vec![("lib/library.1", None), ("lib/library", Some("library.1"))],
                true,
            ),
            (
                1,
                vec![("file", None), ("escape", Some("../outside"))],
                false,
            ),
            (
                2,
                vec![
                    ("dir/file", None),
                    ("dir/a", Some("../dir/b")),
                    ("dir/b", Some("../dir/a")),
                ],
                false,
            ),
            // b/c resolves to root/d, so a's apparently-contained target
            // traverses past the root after resolving b/c.
            (
                3,
                vec![
                    ("d/file", None),
                    ("b/c", Some("../d")),
                    ("a", Some("b/c/../..")),
                ],
                false,
            ),
        ] {
            let tar = data.path().join(format!("{index}.tar.bz2"));
            archive(&tar, &members);
            let root = data.path().join(index.to_string());
            crate::private_fs::directory(&root).unwrap();
            assert_eq!(unpack_tar(&tar, &root, || Ok(())).is_ok(), succeeds);
            if succeeds {
                assert_eq!(std::fs::read(root.join("lib/library")).unwrap(), b"data");
            }
        }
    }

    #[test]
    fn expansion_and_cancellation_limits_precede_member_writes() {
        let data = tempfile::tempdir().unwrap();
        let file = data.path().join("file");
        let mut total = 12 * 1024 * 1024 * 1024;
        assert!(copy_member(&mut &b"data"[..], &file, 4, &mut total, &|| Ok(())).is_err());
        assert!(!file.exists());
        total = 0;
        assert!(
            copy_member(&mut &b"data"[..], &file, 4, &mut total, &|| Err(
                "cancelled".into()
            ))
            .is_err()
        );
        assert_eq!(std::fs::metadata(file).unwrap().len(), 0);
        assert!(member(data.path(), Path::new("../escape")).is_err());
        assert!(member(data.path(), Path::new("/escape")).is_err());
    }
}
