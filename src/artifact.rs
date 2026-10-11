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
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e.to_string()),
    };
    if !file
        .metadata()
        .map_err(|error| error.to_string())?
        .is_file()
    {
        return Err("installation epoch is not a regular file".into());
    }
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
    shared_lock_guarded(path, || Ok(()))
}
fn shared_lock_guarded(
    path: &Path,
    mut check: impl FnMut() -> Result<(), String>,
) -> Result<File, String> {
    check()?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| e.to_string())?;
    if !file
        .metadata()
        .map_err(|error| error.to_string())?
        .is_file()
    {
        return Err("installation reset lock is not a regular file".into());
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        check()?;
        match fs2::FileExt::try_lock_shared(&file) {
            Ok(()) => {
                check()?;
                return Ok(file);
            }
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

#[derive(Clone)]
pub(crate) struct Authority {
    data: PathBuf,
    global_epoch: (PathBuf, u64),
    asset_epoch: (PathBuf, u64),
    locks: Vec<(PathBuf, (u64, u64))>,
}
pub(crate) fn observe_authority(data: &Path, key: &str) -> Result<Authority, String> {
    use std::os::unix::fs::MetadataExt;
    if !valid_key(key) {
        return Err("invalid asset authority key".into());
    }
    let control = data.join(".install");
    let global_epoch = control.join("epoch");
    let asset_epoch = control.join(format!("{key}.epoch"));
    let mut locks = Vec::new();
    for path in [
        control.join("reset.lock"),
        control.join(format!("{key}.lock")),
    ] {
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_file() => locks.push((path, (meta.dev(), meta.ino()))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => return Err("invalid installation authority lock".into()),
        }
    }
    Ok(Authority {
        data: data.into(),
        global_epoch: (global_epoch.clone(), epoch(&global_epoch)?),
        asset_epoch: (asset_epoch.clone(), epoch(&asset_epoch)?),
        locks,
    })
}
impl Authority {
    pub fn check(&self, cancel: &AtomicBool) -> Result<(), String> {
        use std::os::unix::fs::MetadataExt;
        if cancel.load(Ordering::Acquire)
            || crate::reset::requested(&self.data).map_err(|error| error.to_string())?
            || epoch(&self.global_epoch.0)? != self.global_epoch.1
            || epoch(&self.asset_epoch.0)? != self.asset_epoch.1
        {
            return Err("asset operation cancelled".into());
        }
        for (path, identity) in &self.locks {
            let current =
                std::fs::symlink_metadata(path).map_err(|_| "installation control removed")?;
            if !current.is_file() || (current.dev(), current.ino()) != *identity {
                return Err("installation control replaced".into());
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DirectoryIdentity {
    device: u64,
    inode: u64,
}
impl DirectoryIdentity {
    fn of(metadata: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StagingRecord {
    version: u32,
    key: String,
    parent: PathBuf,
    name: String,
    identities: Vec<DirectoryIdentity>,
}

/// The per-asset lock owns this journal and directory. Persist reservation
/// before mkdir, then its identity before writing payloads. A crash at either
/// boundary leaves an empty reservation or an identifiable owned directory.
struct StagingDirectory {
    path: PathBuf,
    journal: PathBuf,
    record: StagingRecord,
    created: bool,
    closed: bool,
}
impl StagingDirectory {
    fn journal(data: &Path, key: &str) -> PathBuf {
        data.join(".install/staging").join(format!("{key}.json"))
    }

    fn read(journal: &Path) -> Result<Option<StagingRecord>, String> {
        match crate::private_fs::read_bounded(journal, 4096) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|_| "invalid interrupted installation record; files preserved".into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err("cannot read interrupted installation record; files preserved".into()),
        }
    }

    fn recover(data: &Path, key: &str, parent: &Path) -> Result<(), String> {
        let journal = Self::journal(data, key);
        let Some(record) = Self::read(&journal)? else {
            return Ok(());
        };
        let relative = parent
            .strip_prefix(data)
            .map_err(|_| "staging is outside app data")?;
        if record.version != 1
            || record.key != key
            || record.parent != relative
            || record.identities.len() > 2
            || !record.name.starts_with(".mywhisper-install-")
            || record.name.len() != ".mywhisper-install-".len() + 32
            || !record.name[".mywhisper-install-".len()..]
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(
                "interrupted installation ownership does not match; files preserved".into(),
            );
        }
        let mut previous = Self {
            path: parent.join(&record.name),
            journal,
            record,
            created: true,
            closed: false,
        };
        previous.close()
    }

    fn new(data: &Path, key: &str, parent: &Path) -> Result<Self, String> {
        let relative = parent
            .strip_prefix(data)
            .map_err(|_| "staging is outside app data")?;
        if relative
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err("invalid installation parent".into());
        }
        crate::private_fs::directory(&data.join(".install/staging")).map_err(|e| e.to_string())?;
        Self::recover(data, key, parent)?;
        let mut nonce = [0u8; 16];
        getrandom::fill(&mut nonce).map_err(|_| "cannot generate staging identity")?;
        let suffix: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
        let name = format!(".mywhisper-install-{suffix}");
        let mut owned = Self {
            path: parent.join(&name),
            journal: Self::journal(data, key),
            record: StagingRecord {
                version: 1,
                key: key.into(),
                parent: relative.into(),
                name,
                identities: Vec::new(),
            },
            created: false,
            closed: false,
        };
        owned.persist()?;
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&owned.path)
            .map_err(|e| e.to_string())?;
        owned.created = true;
        owned.record.identities.push(DirectoryIdentity::of(
            &std::fs::symlink_metadata(&owned.path).map_err(|e| e.to_string())?,
        ));
        owned.persist()?;
        Ok(owned)
    }

    fn persist(&self) -> Result<(), String> {
        let bytes =
            serde_json::to_vec(&self.record).map_err(|_| "cannot encode staging ownership")?;
        if bytes.len() > 4096 {
            return Err("staging ownership exceeds its format limit".into());
        }
        crate::private_fs::replace(&self.journal, &bytes).map_err(|e| e.to_string())
    }

    fn include_previous(&mut self, destination: &Path) -> Result<bool, String> {
        match std::fs::symlink_metadata(destination) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                let identity = DirectoryIdentity::of(&metadata);
                if !self.record.identities.contains(&identity) {
                    self.record.identities.push(identity);
                }
                // Exchange moves this previous directory under the journal's
                // temporary name. Either identity is safe to clean after kill.
                self.persist()?;
                Ok(true)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            _ => Err("artifact destination is not a regular directory".into()),
        }
    }

    fn close(&mut self) -> Result<(), String> {
        if self.closed {
            return Ok(());
        }
        if self.created {
            match std::fs::symlink_metadata(&self.path) {
                Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                    if self.record.identities.is_empty() {
                        // Payloads cannot have been written before identity
                        // persistence. Do not recursively delete a reservation.
                        std::fs::remove_dir(&self.path)
                            .map_err(|_| "staging reservation is not empty; files preserved")?;
                    } else if self
                        .record
                        .identities
                        .contains(&DirectoryIdentity::of(&metadata))
                    {
                        std::fs::remove_dir_all(&self.path)
                            .map_err(|_| "owned staging cleanup failed; retry required")?;
                    } else {
                        return Err("staging directory was replaced; files preserved".into());
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                _ => return Err("staging directory ownership changed; files preserved".into()),
            }
            File::open(self.path.parent().unwrap())
                .and_then(|parent| parent.sync_all())
                .map_err(|e| e.to_string())?;
        }
        if let Some(current) = Self::read(&self.journal)? {
            if current.key != self.record.key
                || current.name != self.record.name
                || current.parent != self.record.parent
            {
                return Err("staging cleanup record was replaced; files preserved".into());
            }
            std::fs::remove_file(&self.journal).map_err(|e| e.to_string())?;
            File::open(self.journal.parent().unwrap())
                .and_then(|parent| parent.sync_all())
                .map_err(|e| e.to_string())?;
        }
        self.closed = true;
        Ok(())
    }
}
impl Drop for StagingDirectory {
    fn drop(&mut self) {
        if let Err(error) = self.close() {
            eprintln!("[install] {error}");
        }
    }
}

pub(crate) struct Stage {
    temporary: StagingDirectory,
    _global: File,
    _asset: File,
    destination: PathBuf,
    data: PathBuf,
    global_epoch: (PathBuf, u64),
    asset_epoch: (PathBuf, u64),
    key: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PromotionPoint {
    BeforeExchange,
    AfterExchange,
    AfterSync,
}

fn exchange(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let from = std::ffi::CString::new(from.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::other("invalid staging path"))?;
    let to = std::ffi::CString::new(to.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::other("invalid artifact path"))?;
    // SAFETY: NUL-terminated paths. Callers hold installer ownership of both
    // directories on the same filesystem; exchange cannot expose half a tree.
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
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Flush payloads and nested directory entries before publishing their name.
/// Archive member writes already sync files, but their parent directories and
/// subsequent flattening/renames also need a durable boundary.
fn sync_tree(
    root: &Path,
    check: impl Fn() -> Result<(), String>,
    mut sync: impl FnMut(&Path, &File) -> std::io::Result<()>,
) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(180);
    let mut pending = vec![(root.to_owned(), 0)];
    let mut directories = Vec::new();
    let mut count = 0usize;
    while let Some((directory, depth)) = pending.pop() {
        check()?;
        if depth > 32 || Instant::now() >= deadline {
            return Err("installation durability limit exceeded".into());
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_DIRECTORY)
            .open(&directory)
            .map_err(|_| "cannot open installation directory for synchronization")?;
        for entry in std::fs::read_dir(&directory)
            .map_err(|_| "cannot enumerate installation for synchronization")?
        {
            check()?;
            count += 1;
            if count > 4097 || Instant::now() >= deadline {
                return Err("installation durability limit exceeded".into());
            }
            let path = entry
                .map_err(|_| "cannot inspect installation for synchronization")?
                .path();
            let metadata = std::fs::symlink_metadata(&path)
                .map_err(|_| "cannot inspect installation for synchronization")?;
            if metadata.is_dir() {
                pending.push((path, depth + 1));
            } else if metadata.is_file() {
                let payload = std::fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                    .open(&path)
                    .map_err(|_| "cannot open installation payload for synchronization")?;
                if !payload.metadata().is_ok_and(|meta| {
                    meta.is_file()
                        && DirectoryIdentity::of(&meta) == DirectoryIdentity::of(&metadata)
                }) {
                    return Err("installation payload changed during synchronization".into());
                }
                sync(&path, &payload).map_err(|_| "cannot sync installation payload")?;
            } else if !metadata.file_type().is_symlink() {
                return Err("unsupported installation entry".into());
            }
        }
        directories.push((
            directory,
            DirectoryIdentity::of(
                &file
                    .metadata()
                    .map_err(|_| "cannot inspect installation directory")?,
            ),
        ));
    }
    for (directory, identity) in directories.into_iter().rev() {
        check()?;
        if Instant::now() >= deadline {
            return Err("installation durability limit exceeded".into());
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_DIRECTORY)
            .open(&directory)
            .map_err(|_| "cannot open installation directory for synchronization")?;
        if !file
            .metadata()
            .is_ok_and(|meta| DirectoryIdentity::of(&meta) == identity)
        {
            return Err("installation directory changed during synchronization".into());
        }
        sync(&directory, &file).map_err(|_| "cannot sync installation directory")?;
    }
    check()
}

impl Stage {
    #[cfg(test)]
    pub fn new(data: &Path, key: &str, destination: PathBuf) -> Result<Self, String> {
        let request = observe_authority(data, key)?;
        Self::new_requested(data, key, destination, &request, &AtomicBool::new(false))
    }
    pub fn new_requested(
        data: &Path,
        key: &str,
        destination: PathBuf,
        request: &Authority,
        cancel: &AtomicBool,
    ) -> Result<Self, String> {
        if !valid_key(key) {
            return Err("invalid artifact key".into());
        }
        let control = data.join(".install");
        if request.data != data || request.asset_epoch.0 != control.join(format!("{key}.epoch")) {
            return Err("installation request belongs to another asset".into());
        }
        request.check(cancel)?;
        crate::private_fs::directory(&control).map_err(|e| e.to_string())?;
        let global = shared_lock_guarded(&control.join("reset.lock"), || request.check(cancel))?;
        let asset = crate::private_fs::lock_guarded(&control.join(format!("{key}.lock")), || {
            request.check(cancel).map_err(std::io::Error::other)
        })
        .map_err(|e| e.to_string())?;
        request.check(cancel)?;
        let parent = destination.parent().ok_or("artifact has no parent")?;
        crate::private_fs::directory(parent).map_err(|e| e.to_string())?;
        let temporary = StagingDirectory::new(data, key, parent)?;
        Ok(Self {
            temporary,
            _global: global,
            _asset: asset,
            destination,
            data: data.to_owned(),
            global_epoch: request.global_epoch.clone(),
            asset_epoch: request.asset_epoch.clone(),
            key: key.to_owned(),
        })
    }
    pub fn path(&self) -> &Path {
        &self.temporary.path
    }
    pub fn destination(&self) -> &Path {
        &self.destination
    }
    pub fn data(&self) -> &Path {
        &self.data
    }
    pub fn key(&self) -> &str {
        &self.key
    }
    pub fn authority(&self) -> Result<Authority, String> {
        use std::os::unix::fs::MetadataExt;
        let control = self.data.join(".install");
        let locks = [
            (&self._global, control.join("reset.lock")),
            (&self._asset, control.join(format!("{}.lock", self.key))),
        ]
        .into_iter()
        .map(|(file, path)| {
            let metadata = file.metadata().map_err(|error| error.to_string())?;
            Ok((path, (metadata.dev(), metadata.ino())))
        })
        .collect::<Result<Vec<_>, String>>()?;
        Ok(Authority {
            data: self.data.clone(),
            global_epoch: self.global_epoch.clone(),
            asset_epoch: self.asset_epoch.clone(),
            locks,
        })
    }
    pub fn check(&self, cancel: &AtomicBool) -> Result<(), String> {
        self.authority()?.check(cancel)
    }
    pub fn promote(self, cancel: &AtomicBool) -> Result<Authority, String> {
        self.promote_observed(cancel, |_| {}, |_, file| file.sync_all())
    }

    fn promote_observed(
        mut self,
        cancel: &AtomicBool,
        mut reached: impl FnMut(PromotionPoint),
        mut sync: impl FnMut(&Path, &File) -> std::io::Result<()>,
    ) -> Result<Authority, String> {
        self.check(cancel)?;
        sync_tree(self.path(), || self.check(cancel), &mut sync)?;
        let replacing = self.temporary.include_previous(&self.destination)?;
        // Serialize the final check/rename against delete/reset epoch changes.
        let metadata = self.global_epoch.0.parent().unwrap().join("metadata.lock");
        let _metadata = crate::private_fs::lock_guarded(&metadata, || {
            self.check(cancel).map_err(std::io::Error::other)
        })
        .map_err(|e| e.to_string())?;
        self.check(cancel)?;
        // Retire queued and active users of the old content before publishing
        // another installation under the same path, including first installs.
        advance(&self.asset_epoch.0)?;
        self.asset_epoch.1 = epoch(&self.asset_epoch.0)?;
        crate::installed_asset::invalidate(&self.data, Some(&self.key));
        crate::transcribe::retire_invalid_assets(&self.data);
        reached(PromotionPoint::BeforeExchange);
        self.check(cancel)?;
        if replacing {
            exchange(self.path(), &self.destination)
                .map_err(|e| format!("atomic installation: {e}"))?;
        } else {
            std::fs::rename(self.path(), &self.destination).map_err(|e| e.to_string())?;
        }
        reached(PromotionPoint::AfterExchange);
        let parent = self.destination.parent().unwrap();
        if File::open(parent)
            .and_then(|file| sync(parent, &file))
            .is_err()
        {
            // A failed durability boundary is a failed update. Restore the
            // previous complete installation before any backup cleanup.
            let rollback = if replacing {
                exchange(self.path(), &self.destination)
            } else {
                std::fs::rename(&self.destination, self.path())
            };
            if rollback.is_err() {
                // Preserve both complete trees and the ownership journal if
                // even rollback fails; a later retry can recover, not guess.
                self.temporary.closed = true;
                return Err(
                    "installation sync and rollback failed; files preserved for recovery".into(),
                );
            }
            File::open(parent)
                .and_then(|file| file.sync_all())
                .map_err(|_| "installation rollback could not be synchronized")?;
            return Err(
                "installation could not be synchronized; previous installation preserved".into(),
            );
        }
        reached(PromotionPoint::AfterSync);
        // Cleanup and its durable journal cover the previous installation
        // after exchange. A failed cleanup remains recoverable on retry.
        self.temporary.close()?;
        self.authority()
    }
}

pub(crate) fn delete(data: &Path, key: &str, destination: &Path) -> std::io::Result<()> {
    if !valid_key(key) {
        return Err(std::io::Error::other("invalid artifact key"));
    }
    crate::installed_asset::invalidate(data, Some(key));
    let control = data.join(".install");
    crate::private_fs::directory(&control)?;
    let _global = shared_lock(&control.join("reset.lock")).map_err(std::io::Error::other)?;
    let metadata = crate::private_fs::lock(&control.join("metadata.lock"))?;
    advance(&control.join(format!("{key}.epoch"))).map_err(std::io::Error::other)?;
    drop(metadata);
    crate::transcribe::retire_invalid_assets(data);
    let _asset = crate::private_fs::lock(&control.join(format!("{key}.lock")))?;
    StagingDirectory::recover(
        data,
        key,
        destination
            .parent()
            .ok_or_else(|| std::io::Error::other("artifact has no parent"))?,
    )
    .map_err(std::io::Error::other)?;
    if destination.exists() {
        std::fs::remove_dir_all(destination)?;
        File::open(destination.parent().unwrap())?.sync_all()?;
    }
    Ok(())
}

/// The exclusive reset owner has revoked and waited for all installers. Remove
/// their cache and journals together; keep epochs, locks and integrity keys.
pub(crate) fn clear_cache(data: &Path) -> std::io::Result<()> {
    let _reset = reset(data)?;
    for name in ["models", "bin"] {
        let path = data.join(name);
        if path.exists() {
            std::fs::remove_dir_all(&path)?;
        }
        crate::private_fs::directory(&path)?;
    }
    File::open(data)?.sync_all()?;
    let staging = data.join(".install/staging");
    match std::fs::symlink_metadata(&staging) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            std::fs::remove_dir_all(&staging)?;
            File::open(data.join(".install"))?.sync_all()?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        _ => {
            return Err(std::io::Error::other(
                "invalid installation journal directory",
            ));
        }
    }
    Ok(())
}

/// Cancel active/queued installations before taking exclusive reset ownership.
pub(crate) fn reset(data: &Path) -> std::io::Result<File> {
    crate::installed_asset::invalidate(data, None);
    let control = data.join(".install");
    crate::private_fs::directory(&control)?;
    let metadata = crate::private_fs::lock(&control.join("metadata.lock"))?;
    advance(&control.join("epoch")).map_err(std::io::Error::other)?;
    drop(metadata);
    crate::transcribe::retire_invalid_assets(data);
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
/// Evidence of the completed, length-checked and SHA-256-authenticated body.
/// Its private fields prevent installation code from minting download proof.
#[derive(Debug)]
pub(crate) struct VerifiedDownload {
    bytes: u64,
    digest: String,
}
impl VerifiedDownload {
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
}

pub(crate) fn download(
    source: Source<'_>,
    path: &Path,
    check: impl Fn() -> Result<(), String>,
    progress: impl Fn(u64, Option<u64>),
) -> Result<VerifiedDownload, String> {
    use sha2::Digest;
    if source.sha256.len() != 64 || !source.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("artifact has no pinned SHA-256".into());
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime.block_on(async {
        check()?;
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .read_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(7200))
            .dns_resolver(std::sync::Arc::new(crate::address::HttpResolver))
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
        Ok(VerifiedDownload {
            bytes: received,
            digest: hash,
        })
    })
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
    fn interrupted_staging_is_recovered_without_deleting_installed_or_foreign_files() {
        const NAME: &str =
            "interrupted_staging_is_recovered_without_deleting_installed_or_foreign_files";
        if let Some(path) = std::env::var_os("MYWHISPER_STAGE_RECOVERY_CHILD") {
            use std::os::unix::fs::MetadataExt;
            let data = PathBuf::from(path);
            assert!(
                data.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("mywhisper-staging-recovery-audit.")
            );
            assert_eq!(std::fs::metadata(&data).unwrap().uid(), unsafe {
                libc::geteuid()
            });
            assert_eq!(std::fs::metadata(&data).unwrap().mode() & 0o777, 0o700);
            assert!(data.join(".disposable").is_file());
            let stage = Stage::new(&data, "model-test", data.join("models/test")).unwrap();
            std::fs::write(
                stage.path().join("partial-model"),
                b"synthetic partial bytes",
            )
            .unwrap();
            crate::private_fs::replace(
                &data.join("stage-ready"),
                stage.path().as_os_str().as_encoded_bytes(),
            )
            .unwrap();
            // The parent terminates this exact owned process while it holds
            // both installer locks; no destructor is allowed to simulate
            // cleanup for the recovery assertion.
            loop {
                std::thread::park();
            }
        }
        let data = tempfile::Builder::new()
            .prefix("mywhisper-staging-recovery-audit.")
            .tempdir()
            .unwrap();
        crate::private_fs::directory(data.path()).unwrap();
        crate::private_fs::replace(&data.path().join(".disposable"), b"fixture").unwrap();
        let destination = data.path().join("models/test");
        crate::private_fs::directory(&destination).unwrap();
        std::fs::write(destination.join("model"), b"previous installation").unwrap();
        let foreign = data.path().join("models/.mywhisper-install-manual-backup");
        crate::private_fs::directory(&foreign).unwrap();
        std::fs::write(foreign.join("user-model"), b"synthetic user-managed model").unwrap();
        let another =
            Stage::new(data.path(), "model-other", data.path().join("models/other")).unwrap();
        std::fs::write(
            another.path().join("active-other-model"),
            b"other active asset",
        )
        .unwrap();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        let log = std::fs::File::create(data.path().join("staging-child.log")).unwrap();
        command
            .args([
                "--exact",
                &format!("artifact::tests::{NAME}"),
                "--nocapture",
            ])
            .env("MYWHISPER_STAGE_RECOVERY_CHILD", data.path())
            .env_remove("DBUS_SESSION_BUS_ADDRESS")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::from(log.try_clone().unwrap()))
            .stderr(std::process::Stdio::from(log));
        let child = crate::child::OwnedChild::spawn(&mut command).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !data.path().join("stage-ready").exists() {
            assert!(
                child.try_status().unwrap().is_none(),
                "staging child exited before barrier: {}",
                String::from_utf8_lossy(
                    &crate::private_fs::read_bounded(
                        &data.path().join("staging-child.log"),
                        64 * 1024
                    )
                    .unwrap()
                )
            );
            assert!(
                Instant::now() < deadline,
                "staging child did not reach barrier"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        let interrupted = PathBuf::from(
            String::from_utf8(
                crate::private_fs::read_bounded(&data.path().join("stage-ready"), 4096).unwrap(),
            )
            .unwrap(),
        );
        assert!(interrupted.starts_with(data.path().join("models")));
        assert!(interrupted.join("partial-model").is_file());
        drop(child); // actual SIGKILL and reap; locks are released by exit
        assert!(
            interrupted.is_dir(),
            "the kill must leave actual interrupted staging"
        );
        let fresh = Stage::new(data.path(), "model-test", destination.clone()).unwrap();
        assert!(
            !interrupted.exists(),
            "a retry left the killed installer's partial model files behind"
        );
        assert_eq!(
            std::fs::read(destination.join("model")).unwrap(),
            b"previous installation"
        );
        assert_eq!(
            std::fs::read(foreign.join("user-model")).unwrap(),
            b"synthetic user-managed model"
        );
        assert_eq!(
            std::fs::read(another.path().join("active-other-model")).unwrap(),
            b"other active asset"
        );
        let fresh_path = fresh.path().to_owned();
        drop(fresh);
        assert!(!fresh_path.exists());
    }

    #[test]
    fn killed_promotion_preserves_a_complete_installation_and_recovers_its_owned_backup() {
        const NAME: &str =
            "killed_promotion_preserves_a_complete_installation_and_recovers_its_owned_backup";
        if let Some(path) = std::env::var_os("MYWHISPER_PROMOTION_RECOVERY_CHILD") {
            use std::os::unix::fs::MetadataExt;
            let data = PathBuf::from(path);
            assert!(
                data.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("mywhisper-promotion-recovery-audit.")
            );
            assert_eq!(std::fs::metadata(&data).unwrap().uid(), unsafe {
                libc::geteuid()
            });
            assert_eq!(std::fs::metadata(&data).unwrap().mode() & 0o777, 0o700);
            assert!(data.join(".disposable").is_file());
            let phase = std::env::var("MYWHISPER_PROMOTION_RECOVERY_PHASE").unwrap();
            let expected = match phase.as_str() {
                "0" => PromotionPoint::BeforeExchange,
                "1" => PromotionPoint::AfterExchange,
                "2" => PromotionPoint::AfterSync,
                _ => panic!("unsupported promotion fixture phase"),
            };
            let stage = Stage::new(&data, "model-test", data.join("models/test")).unwrap();
            let temporary = stage.path().to_owned();
            std::fs::write(stage.path().join("model"), b"complete replacement").unwrap();
            stage
                .promote_observed(
                    &AtomicBool::new(false),
                    move |point| {
                        if point == expected {
                            crate::private_fs::replace(
                                &data.join("promotion-ready"),
                                temporary.as_os_str().as_encoded_bytes(),
                            )
                            .unwrap();
                            loop {
                                std::thread::park();
                            }
                        }
                    },
                    |_, file| file.sync_all(),
                )
                .unwrap();
            panic!("promotion fixture did not stop at its selected boundary");
        }
        for phase in 0..3 {
            let data = tempfile::Builder::new()
                .prefix("mywhisper-promotion-recovery-audit.")
                .tempdir()
                .unwrap();
            crate::private_fs::directory(data.path()).unwrap();
            crate::private_fs::replace(&data.path().join(".disposable"), b"fixture").unwrap();
            let destination = data.path().join("models/test");
            crate::private_fs::directory(&destination).unwrap();
            std::fs::write(destination.join("model"), b"complete previous").unwrap();
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            let log = std::fs::File::create(data.path().join("promotion-child.log")).unwrap();
            command
                .args([
                    "--exact",
                    &format!("artifact::tests::{NAME}"),
                    "--nocapture",
                ])
                .env("MYWHISPER_PROMOTION_RECOVERY_CHILD", data.path())
                .env("MYWHISPER_PROMOTION_RECOVERY_PHASE", phase.to_string())
                .env_remove("DBUS_SESSION_BUS_ADDRESS")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::from(log.try_clone().unwrap()))
                .stderr(std::process::Stdio::from(log));
            let child = crate::child::OwnedChild::spawn(&mut command).unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            while !data.path().join("promotion-ready").exists() {
                assert!(
                    child.try_status().unwrap().is_none(),
                    "promotion child exited: {}",
                    String::from_utf8_lossy(
                        &crate::private_fs::read_bounded(
                            &data.path().join("promotion-child.log"),
                            64 * 1024
                        )
                        .unwrap()
                    )
                );
                assert!(Instant::now() < deadline, "promotion did not reach barrier");
                std::thread::sleep(Duration::from_millis(5));
            }
            let interrupted = PathBuf::from(
                String::from_utf8(
                    crate::private_fs::read_bounded(&data.path().join("promotion-ready"), 4096)
                        .unwrap(),
                )
                .unwrap(),
            );
            assert!(interrupted.starts_with(data.path().join("models")));
            drop(child);
            let expected: &[u8] = if phase == 0 {
                b"complete previous"
            } else {
                b"complete replacement"
            };
            assert_eq!(std::fs::read(destination.join("model")).unwrap(), expected);
            assert!(interrupted.is_dir());
            let fresh = Stage::new(data.path(), "model-test", destination.clone()).unwrap();
            assert!(
                !interrupted.exists(),
                "recovery kept the killed promotion's owned directory"
            );
            assert_eq!(std::fs::read(destination.join("model")).unwrap(), expected);
            std::fs::write(fresh.path().join("model"), b"successful retry").unwrap();
            fresh.promote(&AtomicBool::new(false)).unwrap();
            assert_eq!(
                std::fs::read(destination.join("model")).unwrap(),
                b"successful retry"
            );
            assert!(!StagingDirectory::journal(data.path(), "model-test").exists());
        }
    }

    #[test]
    fn recovery_preserves_unidentified_or_replaced_staging_and_cache_clear_recovers() {
        use std::os::unix::fs::symlink;
        for fault in 0..10 {
            let data = tempfile::tempdir().unwrap();
            let parent = data.path().join("models");
            crate::private_fs::directory(&parent).unwrap();
            let staging = StagingDirectory::new(data.path(), "model-test", &parent).unwrap();
            let path = staging.path.clone();
            let journal = staging.journal.clone();
            let mut record = serde_json::to_value(&staging.record).unwrap();
            std::mem::forget(staging); // journal-only fixture; no held descriptors
            let retained = parent.join("preserved-model");
            crate::private_fs::directory(&retained).unwrap();
            std::fs::write(retained.join("model"), b"complete existing model").unwrap();
            std::fs::write(path.join("partial"), b"owned incomplete bytes").unwrap();
            let external = data.path().join("user-managed-backup");
            crate::private_fs::directory(&external).unwrap();
            std::fs::write(external.join("model"), b"user-managed backup").unwrap();
            match fault {
                0 => record["version"] = 2.into(),
                1 => record["key"] = "model-other".into(),
                2 => record["parent"] = "../user-managed-backup".into(),
                3 => record["name"] = "../user-managed-backup".into(),
                4 => record["name"] = ".mywhisper-install-😀".into(),
                5 => record["identities"][0]["inode"] = 0.into(),
                6 => record["identities"] = serde_json::json!([]),
                7 => {
                    std::fs::rename(&path, data.path().join("old-owned-staging")).unwrap();
                    crate::private_fs::directory(&path).unwrap();
                    std::fs::write(path.join("partial"), b"replacement user bytes").unwrap();
                }
                8 => {
                    std::fs::remove_dir_all(&path).unwrap();
                    symlink(&external, &path).unwrap();
                }
                _ => {}
            }
            let encoded = if fault == 9 {
                vec![b'x'; 4097]
            } else {
                serde_json::to_vec(&record).unwrap()
            };
            crate::private_fs::replace(&journal, &encoded).unwrap();
            assert!(StagingDirectory::new(data.path(), "model-test", &parent).is_err());
            assert_eq!(std::fs::read(&journal).unwrap(), encoded);
            assert_eq!(
                std::fs::read(retained.join("model")).unwrap(),
                b"complete existing model"
            );
            assert_eq!(
                std::fs::read(external.join("model")).unwrap(),
                b"user-managed backup"
            );
            assert!(std::fs::symlink_metadata(&path).is_ok());
            let stale = observe_authority(data.path(), "model-test").unwrap();
            crate::stt::clear_download_cache(data.path()).unwrap();
            assert!(!journal.exists());
            assert!(!retained.exists());
            assert_eq!(
                std::fs::read(external.join("model")).unwrap(),
                b"user-managed backup"
            );
            assert!(stale.check(&AtomicBool::new(false)).is_err());
            assert!(
                Stage::new_requested(
                    data.path(),
                    "model-test",
                    retained.clone(),
                    &stale,
                    &AtomicBool::new(false)
                )
                .is_err()
            );
            let retry = Stage::new(data.path(), "model-test", retained.clone()).unwrap();
            std::fs::write(retry.path().join("model"), b"successful retry").unwrap();
            retry.promote(&AtomicBool::new(false)).unwrap();
            assert_eq!(
                std::fs::read(retained.join("model")).unwrap(),
                b"successful retry"
            );
        }
    }

    #[test]
    fn empty_reservations_and_deleted_assets_recover_without_restoring_old_content() {
        for created in [false, true] {
            let data = tempfile::tempdir().unwrap();
            let parent = data.path().join("models");
            crate::private_fs::directory(&parent).unwrap();
            let staging = StagingDirectory::new(data.path(), "model-test", &parent).unwrap();
            let path = staging.path.clone();
            let journal = staging.journal.clone();
            let mut record = serde_json::to_value(&staging.record).unwrap();
            std::mem::forget(staging);
            record["identities"] = serde_json::json!([]);
            if !created {
                std::fs::remove_dir(&path).unwrap();
            }
            crate::private_fs::replace(&journal, &serde_json::to_vec(&record).unwrap()).unwrap();
            let retry = Stage::new(data.path(), "model-test", parent.join("test")).unwrap();
            assert!(!path.exists());
            drop(retry);
            assert!(!journal.exists());
        }
        let data = tempfile::tempdir().unwrap();
        let parent = data.path().join("models");
        crate::private_fs::directory(&parent).unwrap();
        let destination = parent.join("test");
        crate::private_fs::directory(&destination).unwrap();
        std::fs::write(destination.join("model"), b"old installed model").unwrap();
        let staging = StagingDirectory::new(data.path(), "model-test", &parent).unwrap();
        let path = staging.path.clone();
        let journal = staging.journal.clone();
        std::fs::write(path.join("model"), b"interrupted replacement").unwrap();
        std::mem::forget(staging);
        let stale = observe_authority(data.path(), "model-test").unwrap();
        delete(data.path(), "model-test", &destination).unwrap();
        assert!(!path.exists());
        assert!(!journal.exists());
        assert!(!destination.exists());
        assert!(
            Stage::new_requested(
                data.path(),
                "model-test",
                destination.clone(),
                &stale,
                &AtomicBool::new(false)
            )
            .is_err()
        );
        let retry = Stage::new(data.path(), "model-test", destination.clone()).unwrap();
        std::fs::write(retry.path().join("model"), b"explicit fresh request").unwrap();
        retry.promote(&AtomicBool::new(false)).unwrap();
        assert_eq!(
            std::fs::read(destination.join("model")).unwrap(),
            b"explicit fresh request"
        );
    }

    #[test]
    fn payload_and_nested_directory_sync_failures_preserve_previous_installation() {
        for fail_directory in [false, true] {
            let data = tempfile::tempdir().unwrap();
            let destination = data.path().join("models/test");
            crate::private_fs::directory(&destination).unwrap();
            std::fs::write(destination.join("model"), b"previous complete model").unwrap();
            let stage = Stage::new(data.path(), "model-test", destination.clone()).unwrap();
            crate::private_fs::directory(&stage.path().join("nested/deeper")).unwrap();
            std::fs::write(
                stage.path().join("nested/deeper/model"),
                b"replacement bytes",
            )
            .unwrap();
            let staging = stage.path().to_owned();
            let authority = stage.authority().unwrap();
            let mut failed = false;
            let mut reached_promotion = false;
            let result = stage.promote_observed(
                &AtomicBool::new(false),
                |_| reached_promotion = true,
                |path, file| {
                    if (fail_directory && path.ends_with("nested/deeper"))
                        || (!fail_directory && path.ends_with("nested/deeper/model"))
                    {
                        failed = true;
                        return Err(std::io::Error::from_raw_os_error(libc::ENOSPC));
                    }
                    file.sync_all()
                },
            );
            assert!(result.is_err());
            assert!(failed);
            assert!(!reached_promotion);
            assert!(!staging.exists());
            assert!(!StagingDirectory::journal(data.path(), "model-test").exists());
            authority.check(&AtomicBool::new(false)).unwrap();
            assert_eq!(
                std::fs::read(destination.join("model")).unwrap(),
                b"previous complete model"
            );
            let retry = Stage::new(data.path(), "model-test", destination.clone()).unwrap();
            std::fs::write(retry.path().join("model"), b"successful replacement").unwrap();
            retry.promote(&AtomicBool::new(false)).unwrap();
            assert_eq!(
                std::fs::read(destination.join("model")).unwrap(),
                b"successful replacement"
            );
        }
    }

    #[test]
    fn failed_promotion_parent_sync_restores_complete_previous_installation() {
        for replacing in [false, true] {
            let data = tempfile::tempdir().unwrap();
            let destination = data.path().join("models/test");
            if replacing {
                crate::private_fs::directory(&destination).unwrap();
                std::fs::write(destination.join("model"), b"previous complete model").unwrap();
            }
            let stage = Stage::new(data.path(), "model-test", destination.clone()).unwrap();
            let temporary = stage.path().to_owned();
            std::fs::write(stage.path().join("model"), b"complete replacement").unwrap();
            let mut failure = false;
            let result = stage.promote_observed(
                &AtomicBool::new(false),
                |_| {},
                |path, file| {
                    if path == destination.parent().unwrap() {
                        failure = true;
                        return Err(std::io::Error::from_raw_os_error(libc::ENOSPC));
                    }
                    file.sync_all()
                },
            );
            assert!(result.is_err());
            assert!(failure);
            assert!(!temporary.exists());
            assert!(!StagingDirectory::journal(data.path(), "model-test").exists());
            if replacing {
                assert_eq!(
                    std::fs::read(destination.join("model")).unwrap(),
                    b"previous complete model"
                );
            } else {
                assert!(!destination.exists());
            }
            let retry = Stage::new(data.path(), "model-test", destination.clone()).unwrap();
            std::fs::write(retry.path().join("model"), b"complete successful retry").unwrap();
            retry.promote(&AtomicBool::new(false)).unwrap();
            assert_eq!(
                std::fs::read(destination.join("model")).unwrap(),
                b"complete successful retry"
            );
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

    fn held_fixture_lock(path: &Path, marker: &Path) -> crate::child::OwnedChild {
        use std::process::{Command, Stdio};
        let child = crate::child::OwnedChild::spawn(
            Command::new("python3")
                .args(["-c", "import fcntl, pathlib, sys; f=open(sys.argv[1], 'a+'); fcntl.flock(f, fcntl.LOCK_EX); pathlib.Path(sys.argv[2]).touch(); sys.stdin.buffer.read(1)"])
                .arg(path)
                .arg(marker)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !marker.exists() {
            assert!(child.try_status().unwrap().is_none());
            assert!(Instant::now() < deadline, "fixture did not hold its lock");
            std::thread::sleep(Duration::from_millis(2));
        }
        child
    }

    fn wait_for_lock_descriptor(path: &Path) {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if std::fs::read_dir("/proc/self/fd").unwrap().any(|entry| {
                entry
                    .ok()
                    .and_then(|entry| std::fs::read_link(entry.path()).ok())
                    .is_some_and(|target| target == path)
            }) {
                return;
            }
            assert!(Instant::now() < deadline, "installer did not open its lock");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn installer_wait_cancellation_and_revocation_do_not_wait_for_another_process() {
        for mode in 0..4 {
            let data = tempfile::tempdir().unwrap();
            let destination = data.path().join("models/test");
            crate::private_fs::directory(&destination).unwrap();
            std::fs::write(destination.join("model"), b"previous").unwrap();
            drop(Stage::new(data.path(), "model-test", destination.clone()).unwrap());
            let request = observe_authority(data.path(), "model-test").unwrap();
            let lock = data
                .path()
                .join(".install")
                .join(if mode == 0 || mode == 3 {
                    "reset.lock"
                } else {
                    "model-test.lock"
                });
            let holder = held_fixture_lock(&lock, &data.path().join("held"));
            let cancel = Arc::new(AtomicBool::new(false));
            let token = cancel.clone();
            let queued = request.clone();
            let root = data.path().to_owned();
            let target = destination.clone();
            let (done, received) = mpsc::channel();
            let worker = std::thread::spawn(move || {
                let result = Stage::new_requested(&root, "model-test", target, &queued, &token);
                done.send(result.map(drop)).unwrap();
            });
            wait_for_lock_descriptor(&lock);
            let start = Instant::now();
            let mutation = if mode < 2 {
                cancel.store(true, Ordering::Release);
                None
            } else {
                let root = data.path().to_owned();
                let target = destination.clone();
                let task = std::thread::spawn(move || {
                    if mode == 2 {
                        delete(&root, "model-test", &target).unwrap();
                    } else {
                        drop(reset(&root).unwrap());
                    }
                });
                let deadline = Instant::now() + Duration::from_secs(2);
                while request.check(&AtomicBool::new(false)).is_ok() {
                    assert!(
                        Instant::now() < deadline,
                        "revocation did not precede cleanup"
                    );
                    std::thread::sleep(Duration::from_millis(2));
                }
                Some(task)
            };
            let result = received.recv_timeout(Duration::from_millis(500));
            let elapsed = start.elapsed();
            let still_held = holder.try_status().unwrap().is_none();
            // Release all owned fixture resources before asserting a failure.
            drop(holder);
            worker.join().unwrap();
            if let Some(task) = mutation {
                task.join().unwrap();
            }
            assert!(still_held);
            assert!(
                result.is_ok_and(|result| result.is_err()),
                "queued installer stayed blocked: mode={mode}"
            );
            assert!(elapsed < Duration::from_millis(500));
            if mode == 2 {
                assert!(!destination.exists());
            } else {
                assert_eq!(
                    std::fs::read(destination.join("model")).unwrap(),
                    b"previous"
                );
            }
            assert!(
                Stage::new_requested(
                    data.path(),
                    "model-test",
                    destination.clone(),
                    &request,
                    &cancel
                )
                .is_err()
            );
            let fresh = Stage::new(data.path(), "model-test", destination).unwrap();
            drop(fresh);
        }
    }

    #[test]
    fn installer_wait_promotion_cancellation_preserves_previous_installation() {
        let data = tempfile::tempdir().unwrap();
        let destination = data.path().join("models/test");
        crate::private_fs::directory(&destination).unwrap();
        std::fs::write(destination.join("model"), b"previous").unwrap();
        let stage = Stage::new(data.path(), "model-test", destination.clone()).unwrap();
        std::fs::write(stage.path().join("model"), b"replacement").unwrap();
        let original = stage.authority().unwrap();
        let lock = data.path().join(".install/metadata.lock");
        let holder = held_fixture_lock(&lock, &data.path().join("held"));
        let cancel = Arc::new(AtomicBool::new(false));
        let token = cancel.clone();
        let (done, received) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            done.send(stage.promote(&token).map(|_| ())).unwrap();
        });
        wait_for_lock_descriptor(&lock);
        let start = Instant::now();
        cancel.store(true, Ordering::Release);
        let result = received.recv_timeout(Duration::from_millis(500));
        let elapsed = start.elapsed();
        let still_held = holder.try_status().unwrap().is_none();
        drop(holder);
        worker.join().unwrap();
        assert!(still_held);
        assert!(
            result.is_ok_and(|result| result.is_err()),
            "promotion stayed blocked after cancellation"
        );
        assert!(elapsed < Duration::from_millis(500));
        original.check(&AtomicBool::new(false)).unwrap();
        assert_eq!(
            std::fs::read(destination.join("model")).unwrap(),
            b"previous"
        );
        assert_eq!(
            std::fs::read_dir(destination.parent().unwrap())
                .unwrap()
                .count(),
            1
        );
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
    fn cancellation_delete_and_reset_interrupt_partial_tar_and_zip_extraction() {
        for zipped in [false, true] {
            for action in 0..3 {
                let data = tempfile::tempdir().unwrap();
                let archive = data.path().join("synthetic-archive");
                let payload = vec![71u8; 2 * 1024 * 1024];
                if zipped {
                    let mut writer = zip::ZipWriter::new(File::create(&archive).unwrap());
                    writer
                        .start_file("model", zip::write::SimpleFileOptions::default())
                        .unwrap();
                    writer.write_all(&payload).unwrap();
                    writer.finish().unwrap();
                } else {
                    let encoder = bzip2::write::BzEncoder::new(
                        File::create(&archive).unwrap(),
                        bzip2::Compression::fast(),
                    );
                    let mut writer = tar::Builder::new(encoder);
                    let mut header = tar::Header::new_gnu();
                    header.set_mode(0o600);
                    header.set_size(payload.len() as u64);
                    header.set_cksum();
                    writer
                        .append_data(&mut header, "model", &payload[..])
                        .unwrap();
                    writer.into_inner().unwrap().finish().unwrap();
                }
                let destination = data.path().join("models/test");
                crate::private_fs::directory(&destination).unwrap();
                std::fs::write(destination.join("model"), b"previous complete model").unwrap();
                let stage = Stage::new(data.path(), "model-test", destination.clone()).unwrap();
                let temporary = stage.path().to_owned();
                let cancel = AtomicBool::new(false);
                let begun = AtomicBool::new(false);
                let mutation = std::sync::Mutex::new(None);
                let check = || {
                    if stage
                        .path()
                        .join("model")
                        .metadata()
                        .is_ok_and(|meta| meta.len() >= 65536)
                        && !begun.swap(true, Ordering::AcqRel)
                    {
                        assert!(
                            stage.path().join("model").metadata().unwrap().len()
                                < payload.len() as u64
                        );
                        assert_eq!(
                            std::fs::read(destination.join("model")).unwrap(),
                            b"previous complete model"
                        );
                        if action == 0 {
                            cancel.store(true, Ordering::Release);
                        } else {
                            let root = data.path().to_owned();
                            let target = destination.clone();
                            *mutation.lock().unwrap() = Some(std::thread::spawn(move || {
                                if action == 1 {
                                    delete(&root, "model-test", &target)
                                } else {
                                    crate::stt::clear_download_cache(&root)
                                }
                                .unwrap();
                            }));
                            let deadline = Instant::now() + Duration::from_secs(2);
                            while stage.check(&cancel).is_ok() {
                                assert!(
                                    Instant::now() < deadline,
                                    "extraction mutation did not revoke active writing"
                                );
                                std::thread::sleep(Duration::from_millis(2));
                            }
                        }
                    }
                    stage.check(&cancel)
                };
                let result = if zipped {
                    unpack_zip(&archive, stage.path(), check)
                } else {
                    unpack_tar(&archive, stage.path(), check)
                };
                assert!(result.is_err());
                assert!(begun.load(Ordering::Acquire));
                let partial = stage.path().join("model").metadata().unwrap().len();
                assert!(partial >= 65536 && partial < payload.len() as u64);
                assert!(stage.promote(&cancel).is_err());
                if let Some(task) = mutation.into_inner().unwrap() {
                    task.join().unwrap();
                }
                assert!(!temporary.exists());
                assert!(!StagingDirectory::journal(data.path(), "model-test").exists());
                if action == 0 {
                    assert_eq!(
                        std::fs::read(destination.join("model")).unwrap(),
                        b"previous complete model"
                    );
                } else {
                    assert!(!destination.exists());
                }
                let retry = Stage::new(data.path(), "model-test", destination.clone()).unwrap();
                if zipped {
                    unpack_zip(&archive, retry.path(), || {
                        retry.check(&AtomicBool::new(false))
                    })
                    .unwrap();
                } else {
                    unpack_tar(&archive, retry.path(), || {
                        retry.check(&AtomicBool::new(false))
                    })
                    .unwrap();
                }
                retry.promote(&AtomicBool::new(false)).unwrap();
                assert_eq!(std::fs::read(destination.join("model")).unwrap(), payload);
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
