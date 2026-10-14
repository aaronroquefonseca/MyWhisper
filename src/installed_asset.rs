//! Authenticated installed content, separate from mere file presence.
//! Receipt signing uses a dedicated local key, never the network identity.
use crate::artifact::{Stage, VerifiedDownload};
use ed25519_dalek::{Signature, Signer, SigningKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

const RECEIPT: &str = ".mywhisper-installed.json";
const DOMAIN: &[u8] = b"mywhisper-installed-content-v1\0";
const RECEIPT_CAP: usize = 8 * 1024 * 1024;
const ENTRY_CAP: usize = 4096;
const BYTE_CAP: u64 = 12 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
enum Content {
    File {
        bytes: u64,
        sha256: String,
        executable: bool,
    },
    Directory,
    Link {
        target: String,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    path: String,
    content: Content,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Claim {
    version: u32,
    key: String,
    source_sha256: String,
    entries: Vec<Entry>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    locally_trusted: bool,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    claim: Claim,
    signature: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Stamp {
    dev: u64,
    ino: u64,
    bytes: u64,
    mode: u32,
    uid: u32,
    modified: (i64, i64),
    changed: (i64, i64),
}
fn stamp(path: &Path) -> Result<Stamp, String> {
    let meta = std::fs::symlink_metadata(path).map_err(|_| "cannot inspect installed content")?;
    Ok(Stamp {
        dev: meta.dev(),
        ino: meta.ino(),
        bytes: meta.len(),
        mode: meta.mode(),
        uid: meta.uid(),
        modified: (meta.mtime(), meta.mtime_nsec()),
        changed: (meta.ctime(), meta.ctime_nsec()),
    })
}

/// Retain the complete verified metadata snapshot; timestamp checks are only
/// cache hints. Admission to a new use can request a fresh content verification.
pub(crate) struct VerifiedAsset {
    paths: Vec<(PathBuf, Stamp)>,
    authority: crate::artifact::Authority,
    locally_trusted: bool,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Spec {
    pub data: PathBuf,
    pub root: PathBuf,
    pub key: String,
    pub source: String,
    pub required: Vec<String>,
    pub executables: Vec<String>,
    pub raw: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Status {
    Missing,
    Checking,
    Ready,
    ReadyLocal,
    NeedsVerification,
    Invalid(String),
}
struct Cached {
    spec: Spec,
    value: Status,
    proof: Option<Arc<VerifiedAsset>>,
    queued: bool,
    requested: Instant,
    full_check: Instant,
    cancel: Arc<AtomicBool>,
}
type CacheKey = (PathBuf, String);
static CACHE: std::sync::LazyLock<Mutex<HashMap<CacheKey, Cached>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

struct VerificationJob {
    id: CacheKey,
    cancel: Arc<AtomicBool>,
}
impl Drop for VerificationJob {
    fn drop(&mut self) {
        if let Ok(mut cache) = CACHE.lock()
            && let Some(entry) = cache.get_mut(&self.id)
            && Arc::ptr_eq(&entry.cancel, &self.cancel)
            && entry.queued
        {
            entry.queued = false;
            entry.proof = None;
            entry.value =
                Status::Invalid("installed content verification interrupted; retry".into());
        }
    }
}

/// GTK only reads/enqueues a bounded status snapshot. File enumeration,
/// key access, signature verification and hashing happen on existing workers.
pub(crate) fn status(spec: Spec) -> Status {
    let id = (spec.data.clone(), spec.key.clone());
    let Ok(mut cache) = CACHE.try_lock() else {
        return Status::Checking;
    };
    if cache.get(&id).is_some_and(|entry| entry.spec != spec)
        && let Some(old) = cache.remove(&id)
    {
        old.cancel.store(true, Ordering::Release);
    }
    if !cache.contains_key(&id) {
        if cache.len() >= 64 {
            let oldest = cache
                .iter()
                .filter(|(_, entry)| !entry.queued)
                .min_by_key(|(_, entry)| entry.requested)
                .map(|(id, _)| id.clone());
            let Some(oldest) = oldest else {
                return Status::Checking;
            };
            if let Some(old) = cache.remove(&oldest) {
                old.cancel.store(true, Ordering::Release);
            }
        }
        cache.insert(
            id.clone(),
            Cached {
                spec: spec.clone(),
                value: Status::Checking,
                proof: None,
                queued: false,
                requested: Instant::now() - Duration::from_secs(1),
                full_check: Instant::now(),
                cancel: Arc::new(AtomicBool::new(false)),
            },
        );
    }
    let entry = cache.get_mut(&id).unwrap();
    let value = entry.value.clone();
    if entry.queued || entry.requested.elapsed() < Duration::from_secs(1) {
        return value;
    }
    entry.queued = true;
    entry.requested = Instant::now();
    let previous = entry.proof.clone();
    let full = entry.full_check.elapsed() >= Duration::from_secs(300);
    let cancel = entry.cancel.clone();
    drop(cache);
    let task_id = id.clone();
    let task_cancel = cancel.clone();
    let submitted = crate::runtime::submit("verify-installed-asset", move || {
        let _owner = VerificationJob {
            id: task_id.clone(),
            cancel: task_cancel.clone(),
        };
        let check = || {
            if task_cancel.load(Ordering::Acquire)
                || crate::reset::requested(&spec.data)
                    .map_err(|_| "cannot read asset reset policy")?
            {
                Err("asset verification cancelled".into())
            } else {
                Ok(())
            }
        };
        let reuse = !full && previous.as_ref().is_some_and(|proof| proof.unchanged());
        let result = if check().is_err() {
            Err(Status::Checking)
        } else if reuse {
            Ok(previous.as_ref().unwrap().clone())
        } else {
            verify_spec(&spec, &check).map(Arc::new)
        };
        let Ok(mut cache) = CACHE.lock() else {
            return;
        };
        let Some(entry) = cache.get_mut(&task_id) else {
            return;
        };
        if !Arc::ptr_eq(&entry.cancel, &task_cancel) || task_cancel.load(Ordering::Acquire) {
            return;
        }
        entry.queued = false;
        match result {
            Ok(proof) => {
                entry.value = proof.status();
                entry.proof = Some(proof);
                if !reuse {
                    entry.full_check = Instant::now();
                }
            }
            Err(value) => {
                entry.proof = None;
                entry.value = value;
            }
        }
    });
    if submitted.is_err()
        && let Ok(mut cache) = CACHE.lock()
        && let Some(entry) = cache.get_mut(&id)
        && Arc::ptr_eq(&entry.cancel, &cancel)
    {
        entry.queued = false;
    }
    value
}

/// Provider admission always verifies content anew on its owned worker. UI
/// cache hints are never authority to execute an installed runtime/model.
pub(crate) fn admit(
    spec: &Spec,
    check: impl Fn() -> Result<(), String>,
) -> Result<Arc<VerifiedAsset>, String> {
    check()?;
    let proof = verify_spec(spec, &check).map_err(|status| match status {
        Status::NeedsVerification => "Existing runtime files need verification in Language Models before use; files have been preserved".into(),
        Status::Missing => "Required local files are missing; install them in Language Models".into(),
        value => verification_error(value),
    })?;
    check()?;
    if !proof.unchanged() {
        return Err("installed content changed during provider admission".into());
    }
    Ok(Arc::new(proof))
}

fn verify_spec(
    spec: &Spec,
    check: &impl Fn() -> Result<(), String>,
) -> Result<VerifiedAsset, Status> {
    if !spec.root.exists() {
        return Err(Status::Missing);
    }
    for required in &spec.required {
        if !safe_path(required) {
            return Err(Status::Invalid("invalid required asset path".into()));
        }
        if !std::fs::metadata(spec.root.join(required))
            .is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0)
        {
            return Err(Status::Invalid(
                "required installed files are missing or empty".into(),
            ));
        }
    }
    for executable in &spec.executables {
        if !safe_path(executable)
            || !std::fs::metadata(spec.root.join(executable)).is_ok_and(|metadata| {
                metadata.is_file() && metadata.len() > 0 && metadata.mode() & 0o111 != 0
            })
        {
            return Err(Status::Invalid(
                "installed runtime is not executable".into(),
            ));
        }
    }
    if let Some(raw) = &spec.raw {
        if !safe_path(raw) {
            return Err(Status::Invalid("invalid raw asset path".into()));
        }
        let authority =
            crate::artifact::observe_authority(&spec.data, &spec.key).map_err(Status::Invalid)?;
        let authorized = || {
            check()?;
            authority.check(&AtomicBool::new(false))
        };
        let path = spec.root.join(raw);
        let original = stamp(&path).map_err(Status::Invalid)?;
        let (_, actual) = hash_file(
            &path,
            Instant::now() + Duration::from_secs(180),
            &authorized,
        )
        .map_err(Status::Invalid)?;
        if actual != spec.source {
            if spec.root.join(RECEIPT).exists() {
                let local = verify(&spec.data, &spec.root, &spec.key, &spec.source, check)
                    .map_err(Status::Invalid)?;
                if local.locally_trusted {
                    return Ok(local);
                }
            }
            return Err(Status::Invalid(
                "installed file does not match its pinned SHA-256".into(),
            ));
        }
        authority
            .check(&AtomicBool::new(false))
            .map_err(Status::Invalid)?;
        return Ok(VerifiedAsset {
            paths: vec![(path, original)],
            authority,
            locally_trusted: false,
        });
    }
    if !spec.root.join(RECEIPT).exists() {
        return Err(Status::NeedsVerification);
    }
    verify(&spec.data, &spec.root, &spec.key, &spec.source, check).map_err(Status::Invalid)
}

/// Installers finish outside GTK and publish only their still-authorized
/// verified result. A stale UI verification job cannot overwrite that result.
pub(crate) fn install(stage: Stage, spec: Spec, cancel: &AtomicBool) -> Result<(), String> {
    if spec.data != stage.data() || spec.key != stage.key() || spec.root != stage.destination() {
        return Err("installation specification does not own staging".into());
    }
    let staged = Spec {
        root: stage.path().to_owned(),
        ..spec.clone()
    };
    verify_spec(&staged, &|| stage.check(cancel)).map_err(verification_error)?;
    let authority = stage.promote(cancel)?;
    publish(spec, || authority.check(cancel))
}

fn verification_error(status: Status) -> String {
    match status {
        Status::Invalid(message) => message,
        _ => "installed content verification did not complete".into(),
    }
}

pub(crate) fn publish(spec: Spec, check: impl Fn() -> Result<(), String>) -> Result<(), String> {
    let proof = verify_spec(&spec, &check).map_err(verification_error)?;
    let id = (spec.data.clone(), spec.key.clone());
    let mut cache = CACHE
        .lock()
        .map_err(|_| "installed verification state unavailable")?;
    check()?;
    if let Some(old) = cache.remove(&id) {
        old.cancel.store(true, Ordering::Release);
    }
    if cache.len() >= 64 {
        if let Some(id) = cache
            .iter()
            .filter(|(_, entry)| !entry.queued)
            .min_by_key(|(_, entry)| entry.requested)
            .map(|(id, _)| id.clone())
        {
            if let Some(old) = cache.remove(&id) {
                old.cancel.store(true, Ordering::Release);
            }
        } else {
            // The installation is verified; caching is optional and bounded.
            return Ok(());
        }
    }
    cache.insert(
        id,
        Cached {
            spec,
            value: proof.status(),
            proof: Some(Arc::new(proof)),
            queued: false,
            requested: Instant::now(),
            full_check: Instant::now(),
            cancel: Arc::new(AtomicBool::new(false)),
        },
    );
    Ok(())
}
impl VerifiedAsset {
    fn status(&self) -> Status {
        if self.locally_trusted {
            Status::ReadyLocal
        } else {
            Status::Ready
        }
    }
    pub fn unchanged(&self) -> bool {
        self.authority.check(&AtomicBool::new(false)).is_ok()
            && self
                .paths
                .iter()
                .all(|(path, expected)| stamp(path).as_ref() == Ok(expected))
            && self.authority.check(&AtomicBool::new(false)).is_ok()
    }
}

fn key_path(data: &Path) -> PathBuf {
    data.join(".install/installed-content.key")
}
fn read_key(data: &Path) -> Result<SigningKey, String> {
    let path = key_path(data);
    let metadata = std::fs::symlink_metadata(&path)
        .map_err(|_| "installation integrity key is unavailable")?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err("installation integrity key has unsafe ownership or permissions".into());
    }
    let bytes = crate::private_fs::read_private(&path, 32)
        .map_err(|_| "cannot read installation integrity key")?;
    let seed: [u8; 32] = bytes
        .try_into()
        .map_err(|_| "invalid installation integrity key length")?;
    Ok(SigningKey::from_bytes(&seed))
}
fn provision_key(data: &Path) -> Result<SigningKey, String> {
    crate::private_fs::directory(&data.join(".install"))
        .map_err(|_| "cannot protect installation control directory")?;
    let _guard = crate::private_fs::lock(&data.join(".install/installed-content.key.lock"))
        .map_err(|_| "installation integrity key lock unavailable")?;
    match std::fs::symlink_metadata(key_path(data)) {
        Ok(_) => read_key(data),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut seed = [0; 32];
            getrandom::fill(&mut seed)
                .map_err(|_| "cannot provision installation integrity key")?;
            crate::private_fs::replace(&key_path(data), &seed)
                .map_err(|_| "cannot persist installation integrity key")?;
            read_key(data)
        }
        Err(_) => Err("cannot inspect installation integrity key".into()),
    }
}
fn message(claim: &Claim) -> Result<Vec<u8>, String> {
    let encoded = serde_json::to_vec(claim).map_err(|_| "cannot encode installed content")?;
    if encoded.len() > RECEIPT_CAP {
        return Err("installation receipt exceeds limit".into());
    }
    let mut bytes = Vec::with_capacity(DOMAIN.len() + encoded.len());
    bytes.extend_from_slice(DOMAIN);
    bytes.extend(encoded);
    Ok(bytes)
}
fn safe_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 1024
        && Path::new(path)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}
fn hash_file(
    path: &Path,
    deadline: Instant,
    check: &impl Fn() -> Result<(), String>,
) -> Result<(u64, String), String> {
    check()?;
    let before = stamp(path)?;
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| "cannot open installed file")?;
    let metadata = file
        .metadata()
        .map_err(|_| "cannot inspect installed file")?;
    if !metadata.is_file()
        || metadata.len() > BYTE_CAP
        || (metadata.dev(), metadata.ino()) != (before.dev, before.ino)
    {
        return Err("installed file changed or exceeds limit".into());
    }
    let mut bytes = 0u64;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        check()?;
        if Instant::now() >= deadline {
            return Err("installed content verification deadline exceeded".into());
        }
        let count = file
            .read(&mut buffer)
            .map_err(|_| "cannot read installed content")?;
        if count == 0 {
            break;
        }
        bytes = bytes
            .checked_add(count as u64)
            .filter(|bytes| *bytes <= BYTE_CAP)
            .ok_or("installed file exceeds limit")?;
        digest.update(&buffer[..count]);
    }
    if stamp(path)? != before || bytes != metadata.len() {
        return Err("installed file changed during verification".into());
    }
    Ok((bytes, hex(&digest.finalize())))
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

struct Tree {
    entries: Vec<Entry>,
    paths: Vec<(PathBuf, Stamp)>,
}

fn scan(root: &Path, check: &impl Fn() -> Result<(), String>) -> Result<Tree, String> {
    let metadata = std::fs::symlink_metadata(root).map_err(|_| "installed directory is missing")?;
    if !metadata.is_dir() {
        return Err("installed directory is not an owned directory".into());
    }
    let canonical = root
        .canonicalize()
        .map_err(|_| "cannot resolve installed directory")?;
    let deadline = Instant::now() + Duration::from_secs(180);
    let mut pending = vec![(root.to_owned(), 0usize)];
    let mut entries = Vec::new();
    let mut stamps = vec![(root.to_owned(), stamp(root)?)];
    let mut total = 0u64;
    while let Some((directory, depth)) = pending.pop() {
        check()?;
        if depth > 32 || Instant::now() >= deadline {
            return Err("installed directory verification limit exceeded".into());
        }
        for child in
            std::fs::read_dir(directory).map_err(|_| "cannot enumerate installed directory")?
        {
            check()?;
            let child = child.map_err(|_| "cannot inspect installed directory entry")?;
            let path = child.path();
            if path == root.join(RECEIPT) {
                continue;
            }
            if entries.len() >= ENTRY_CAP {
                return Err("installed content entry limit exceeded".into());
            }
            let relative = path
                .strip_prefix(root)
                .map_err(|_| "installed path escaped directory")?
                .to_str()
                .ok_or("installed filename is not UTF-8")?
                .to_owned();
            if !safe_path(&relative) {
                return Err("invalid installed path".into());
            }
            let metadata =
                std::fs::symlink_metadata(&path).map_err(|_| "cannot inspect installed path")?;
            let original = stamp(&path)?;
            let content = if metadata.is_dir() {
                pending.push((path.clone(), depth + 1));
                Content::Directory
            } else if metadata.is_file() {
                let (bytes, sha256) = hash_file(&path, deadline, check)?;
                total = total
                    .checked_add(bytes)
                    .filter(|bytes| *bytes <= BYTE_CAP)
                    .ok_or("installed content size limit exceeded")?;
                Content::File {
                    bytes,
                    sha256,
                    executable: metadata.mode() & 0o111 != 0,
                }
            } else if metadata.file_type().is_symlink() {
                let target =
                    std::fs::read_link(&path).map_err(|_| "cannot inspect installed link")?;
                let target = target
                    .to_str()
                    .filter(|target| target.len() <= 1024)
                    .ok_or("invalid installed link")?
                    .to_owned();
                let resolved = path.canonicalize().map_err(|_| "broken installed link")?;
                if Path::new(&target).is_absolute() || !resolved.starts_with(&canonical) {
                    return Err("installed link escapes directory".into());
                }
                Content::Link { target }
            } else {
                return Err("unsupported installed file type".into());
            };
            if stamp(&path)? != original {
                return Err("installed entry changed during verification".into());
            }
            stamps.push((path, original));
            entries.push(Entry {
                path: relative,
                content,
            });
        }
    }
    if stamps
        .iter()
        .any(|(path, expected)| stamp(path).as_ref() != Ok(expected))
    {
        return Err("installed directory changed during verification".into());
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(Tree {
        entries,
        paths: stamps,
    })
}

/// Verify an existing archive against a freshly authenticated, normalized
/// reference. Only its receipt is added; the user's files and inodes survive.
pub(crate) fn adopt_reference(stage: Stage, spec: Spec, cancel: &AtomicBool) -> Result<(), String> {
    if spec.data != stage.data() || spec.key != stage.key() || spec.root != stage.destination() {
        return Err("verification reference does not own installation".into());
    }
    let check = || stage.check(cancel);
    let expected = verify_spec(
        &Spec {
            root: stage.path().into(),
            ..spec.clone()
        },
        &check,
    )
    .map_err(verification_error)?;
    if expected.locally_trusted {
        return Err("verification reference is not authenticated upstream content".into());
    }
    let reference = scan(stage.path(), &check)?;
    let original = scan(&spec.root, &check)?;
    if original.entries != reference.entries {
        return Err("Existing files differ from the verified source; files have been preserved. Download to repair or explicitly trust your local files.".into());
    }
    let encoded = crate::private_fs::read_private(&stage.path().join(RECEIPT), RECEIPT_CAP)
        .map_err(|_| "cannot read authenticated reference receipt")?;
    let _metadata = crate::private_fs::lock(&spec.data.join(".install/metadata.lock"))
        .map_err(|_| "verification commit lock unavailable")?;
    check()?;
    if !expected.unchanged()
        || original
            .paths
            .iter()
            .any(|(path, saved)| stamp(path).as_ref() != Ok(saved))
    {
        return Err("installed files changed before verification commit".into());
    }
    crate::private_fs::replace(&spec.root.join(RECEIPT), &encoded)
        .map_err(|_| "cannot persist verification receipt; model files preserved")?;
    File::open(&spec.root)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| "cannot sync verification receipt")?;
    check()?;
    publish(spec, check)
}

/// Explicit local trust preserves custom/quantized models and manual engines.
/// It attests to a stable local snapshot, never to upstream authenticity.
pub(crate) fn trust_local(
    spec: Spec,
    cancel: &AtomicBool,
    request: Option<&crate::artifact::Authority>,
) -> Result<(), String> {
    if cancel.load(Ordering::Acquire) {
        return Err("local trust cancelled".into());
    }
    let captured = crate::artifact::observe_authority(&spec.data, &spec.key)?;
    let request = request.unwrap_or(&captured);
    let stage = Stage::new_requested(&spec.data, &spec.key, spec.root.clone(), request, cancel)?;
    let check = || stage.check(cancel);
    check()?;
    // Required files and executable permissions remain mandatory, even when
    // the user intentionally accepts a different local digest.
    for required in &spec.required {
        if !safe_path(required)
            || !std::fs::metadata(spec.root.join(required))
                .is_ok_and(|meta| meta.is_file() && meta.len() > 0)
        {
            return Err("cannot trust missing or empty installed files".into());
        }
    }
    for executable in &spec.executables {
        if !safe_path(executable)
            || !std::fs::metadata(spec.root.join(executable))
                .is_ok_and(|meta| meta.is_file() && meta.len() > 0 && meta.mode() & 0o111 != 0)
        {
            return Err("cannot trust a runtime that is not executable".into());
        }
    }
    let original = scan(&spec.root, &check)?;
    let claim = Claim {
        version: 1,
        key: spec.key.clone(),
        source_sha256: spec.source.clone(),
        entries: original.entries,
        locally_trusted: true,
    };
    let authority = provision_key(&spec.data)?;
    let receipt = Receipt {
        signature: authority.sign(&message(&claim)?).to_bytes().to_vec(),
        claim,
    };
    let encoded = serde_json::to_vec(&receipt).map_err(|_| "cannot encode local trust receipt")?;
    if encoded.len() > RECEIPT_CAP {
        return Err("local trust receipt exceeds limit".into());
    }
    let _metadata = crate::private_fs::lock(&spec.data.join(".install/metadata.lock"))
        .map_err(|_| "local trust commit lock unavailable")?;
    check()?;
    if original
        .paths
        .iter()
        .any(|(path, saved)| stamp(path).as_ref() != Ok(saved))
    {
        return Err("local files changed before trust commit".into());
    }
    crate::private_fs::replace(&spec.root.join(RECEIPT), &encoded)
        .map_err(|_| "cannot persist local trust receipt")?;
    File::open(&spec.root)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| "cannot sync local trust receipt")?;
    check()?;
    publish(spec, check)
}

/// Called only on private staging after authenticated download and extraction.
/// A crash before signing/promotion cannot make partial content look verified.
pub(crate) fn seal(
    stage: &Stage,
    source: &VerifiedDownload,
    check: impl Fn() -> Result<(), String>,
) -> Result<(), String> {
    check()?;
    if std::fs::symlink_metadata(stage.path().join(RECEIPT)).is_ok() {
        return Err("archive contains reserved installation receipt".into());
    }
    let Tree { entries, .. } = scan(stage.path(), &check)?;
    let claim = Claim {
        version: 1,
        key: stage.key().into(),
        source_sha256: source.digest().into(),
        entries,
        locally_trusted: false,
    };
    let authority = provision_key(stage.data())?;
    let signature = authority.sign(&message(&claim)?).to_bytes().to_vec();
    let receipt = Receipt { claim, signature };
    let encoded = serde_json::to_vec(&receipt).map_err(|_| "cannot encode installation receipt")?;
    if encoded.len() > RECEIPT_CAP {
        return Err("installation receipt exceeds limit".into());
    }
    check()?;
    crate::private_fs::replace(&stage.path().join(RECEIPT), &encoded)
        .map_err(|_| "cannot persist installation receipt")?;
    File::open(stage.path())
        .and_then(|directory| directory.sync_all())
        .map_err(|_| "cannot sync installed content")?;
    check()
}

pub(crate) fn verify(
    data: &Path,
    root: &Path,
    key: &str,
    source: &str,
    check: impl Fn() -> Result<(), String>,
) -> Result<VerifiedAsset, String> {
    check()?;
    let authority_epoch = crate::artifact::observe_authority(data, key)?;
    let authorized = || {
        check()?;
        authority_epoch.check(&AtomicBool::new(false))
    };
    let key_stamp = stamp(&key_path(data))?;
    let authority = read_key(data)?;
    let receipt_stamp = stamp(&root.join(RECEIPT))?;
    let encoded = crate::private_fs::read_private(&root.join(RECEIPT), RECEIPT_CAP)
        .map_err(|_| "installed content needs verification or repair")?;
    let receipt: Receipt =
        serde_json::from_slice(&encoded).map_err(|_| "invalid installation receipt")?;
    if receipt.claim.version != 1
        || receipt.claim.key != key
        || receipt.claim.source_sha256 != source
        || receipt.claim.entries.len() > ENTRY_CAP
    {
        return Err("installation receipt does not match selected asset".into());
    }
    let signature = Signature::from_slice(&receipt.signature)
        .map_err(|_| "invalid installation receipt signature")?;
    authority
        .verifying_key()
        .verify_strict(&message(&receipt.claim)?, &signature)
        .map_err(|_| "installation receipt authentication failed")?;
    let Tree {
        entries: actual,
        mut paths,
    } = scan(root, &authorized)?;
    if actual != receipt.claim.entries {
        return Err("installed content is corrupt, changed or incomplete".into());
    }
    if stamp(&key_path(data))? != key_stamp || stamp(&root.join(RECEIPT))? != receipt_stamp {
        return Err("installation authority changed during verification".into());
    }
    check()?;
    paths.push((key_path(data), key_stamp));
    paths.push((root.join(RECEIPT), receipt_stamp));
    authority_epoch.check(&AtomicBool::new(false))?;
    Ok(VerifiedAsset {
        paths,
        authority: authority_epoch,
        locally_trusted: receipt.claim.locally_trusted,
    })
}

pub(crate) fn invalidate(data: &Path, key: Option<&str>) {
    if let Ok(mut cache) = CACHE.lock() {
        cache.retain(|(directory, asset), entry| {
            let keep = directory != data || key.is_some_and(|key| key != asset);
            if !keep {
                entry.cancel.store(true, Ordering::Release);
            }
            keep
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::AtomicUsize;

    fn prepared_fixture() -> (tempfile::TempDir, Stage, Spec) {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("asset");
        let stage = Stage::new(root.path(), "fixture", destination.clone()).unwrap();
        let mut archive = tar::Builder::new(bzip2::write::BzEncoder::new(
            Vec::new(),
            bzip2::Compression::best(),
        ));
        for (path, bytes) in [
            ("model", b"synthetic model weights".as_slice()),
            ("lib/runtime.so.1", b"synthetic bundled library".as_slice()),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o600);
            header.set_cksum();
            archive.append_data(&mut header, path, bytes).unwrap();
        }
        let mut header = tar::Header::new_gnu();
        header.set_size(0);
        header.set_mode(0o777);
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_link_name("runtime.so.1").unwrap();
        header.set_cksum();
        archive
            .append_data(&mut header, "lib/runtime.so", std::io::empty())
            .unwrap();
        let bytes = archive.into_inner().unwrap().finish().unwrap();
        let source = hex(&Sha256::digest(&bytes));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/synthetic", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            socket
                .set_write_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut request = [0; 4096];
            assert!(socket.read(&mut request).unwrap() > 0);
            write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                bytes.len()
            )
            .unwrap();
            socket.write_all(&bytes).unwrap();
        });
        let archive = stage.path().join("download.part");
        let verified = crate::artifact::download(
            crate::artifact::Source {
                url: &url,
                sha256: &source,
                limit: 1024 * 1024,
            },
            &archive,
            || Ok(()),
            |_, _| {},
        )
        .unwrap();
        server.join().unwrap();
        crate::artifact::unpack_tar(&archive, stage.path(), || Ok(())).unwrap();
        std::fs::remove_file(archive).unwrap();
        seal(&stage, &verified, || Ok(())).unwrap();
        let spec = Spec {
            data: root.path().into(),
            root: destination,
            key: "fixture".into(),
            source,
            required: vec!["model".into(), "lib/runtime.so".into()],
            executables: Vec::new(),
            raw: None,
        };
        (root, stage, spec)
    }

    fn fixture() -> (tempfile::TempDir, Spec) {
        let (root, stage, spec) = prepared_fixture();
        install(stage, spec.clone(), &AtomicBool::new(false)).unwrap();
        (root, spec)
    }

    fn copy_legacy(stage: &Stage, spec: &Spec) {
        std::fs::create_dir_all(spec.root.join("lib")).unwrap();
        for file in ["model", "lib/runtime.so.1"] {
            std::fs::copy(stage.path().join(file), spec.root.join(file)).unwrap();
        }
        std::os::unix::fs::symlink("runtime.so.1", spec.root.join("lib/runtime.so")).unwrap();
    }

    #[test]
    fn legacy_comparison_preserves_files_and_refuses_changed_or_cancelled_reference() {
        for case in ["matching", "changed", "extra", "cancelled"] {
            let (_root, stage, spec) = prepared_fixture();
            copy_legacy(&stage, &spec);
            let path = spec.root.join("model");
            if case == "changed" {
                std::fs::write(&path, b"intentional local model").unwrap();
            }
            if case == "extra" {
                std::fs::write(spec.root.join("personal-notes"), b"preserve me").unwrap();
            }
            let saved = std::fs::read(&path).unwrap();
            let inode = stamp(&path).unwrap().ino;
            let root_inode = stamp(&spec.root).unwrap().ino;
            let temporary = stage.path().to_owned();
            let result =
                adopt_reference(stage, spec.clone(), &AtomicBool::new(case == "cancelled"));
            assert_eq!(result.is_ok(), case == "matching", "{case}: {result:?}");
            assert_eq!(std::fs::read(&path).unwrap(), saved);
            assert_eq!(stamp(&path).unwrap().ino, inode);
            assert_eq!(stamp(&spec.root).unwrap().ino, root_inode);
            assert!(!temporary.exists());
            if case == "matching" {
                assert_eq!(admit(&spec, || Ok(())).unwrap().status(), Status::Ready);
            } else {
                assert!(!spec.root.join(RECEIPT).exists());
                assert!(admit(&spec, || Ok(())).is_err());
            }
        }
    }

    #[test]
    fn explicit_local_trust_preserves_manual_content_and_detects_later_corruption() {
        let (_root, stage, mut spec) = prepared_fixture();
        copy_legacy(&stage, &spec);
        drop(stage);
        spec.raw = Some("model".into());
        let path = spec.root.join("model");
        std::fs::write(&path, b"explicitly chosen manual model").unwrap();
        let saved = std::fs::read(&path).unwrap();
        let inode = stamp(&path).unwrap().ino;
        assert!(admit(&spec, || Ok(())).is_err());
        trust_local(spec.clone(), &AtomicBool::new(true), None).unwrap_err();
        assert!(!spec.root.join(RECEIPT).exists());
        trust_local(spec.clone(), &AtomicBool::new(false), None).unwrap();
        let proof = admit(&spec, || Ok(())).unwrap();
        assert_eq!(proof.status(), Status::ReadyLocal);
        assert_eq!(std::fs::read(&path).unwrap(), saved);
        assert_eq!(stamp(&path).unwrap().ino, inode);
        let operation =
            crate::operation::Operation::unrestricted().with_assets(vec![proof.clone()]);
        operation.check().unwrap();
        let mut altered = saved;
        altered[0] ^= 1;
        std::fs::write(&path, &altered).unwrap();
        assert!(!proof.unchanged());
        assert!(operation.check().is_err());
        assert!(admit(&spec, || Ok(())).is_err());
        assert_eq!(std::fs::read(path).unwrap(), altered);
    }

    #[test]
    fn local_trust_never_attests_missing_empty_or_non_executable_files() {
        for case in ["missing", "empty", "permissions"] {
            let (_root, stage, mut spec) = prepared_fixture();
            copy_legacy(&stage, &spec);
            drop(stage);
            let path = spec.root.join("model");
            match case {
                "missing" => std::fs::remove_file(&path).unwrap(),
                "empty" => std::fs::write(&path, b"").unwrap(),
                "permissions" => spec.executables.push("model".into()),
                _ => unreachable!(),
            }
            assert!(trust_local(spec.clone(), &AtomicBool::new(false), None).is_err());
            assert!(!spec.root.join(RECEIPT).exists());
        }
    }

    #[test]
    fn queued_verification_and_trust_cannot_renew_deleted_or_reset_authority() {
        for reset in [false, true] {
            let (_root, stage, spec) = prepared_fixture();
            copy_legacy(&stage, &spec);
            let request = stage.authority().unwrap();
            drop(stage);
            if reset {
                drop(crate::artifact::reset(&spec.data).unwrap());
            } else {
                crate::artifact::delete(&spec.data, &spec.key, &spec.root).unwrap();
                std::fs::create_dir_all(spec.root.join("lib")).unwrap();
                std::fs::write(
                    spec.root.join("model"),
                    b"new installation at the same path",
                )
                .unwrap();
            }
            assert!(
                Stage::new_requested(
                    &spec.data,
                    &spec.key,
                    spec.root.clone(),
                    &request,
                    &AtomicBool::new(false)
                )
                .is_err()
            );
            assert!(trust_local(spec.clone(), &AtomicBool::new(false), Some(&request)).is_err());
            assert!(!spec.root.join(RECEIPT).exists());
            assert!(spec.root.join("model").exists());
        }
    }

    #[test]
    fn promotion_retires_queued_and_active_asset_authority_without_losing_data() {
        let (_root, stage, spec) = prepared_fixture();
        copy_legacy(&stage, &spec);
        let authority = crate::artifact::observe_authority(&spec.data, &spec.key).unwrap();
        let paths = scan(&spec.root, &|| Ok(())).unwrap().paths;
        let old = Arc::new(VerifiedAsset {
            paths,
            authority: authority.clone(),
            locally_trusted: true,
        });
        let operation = crate::operation::Operation::unrestricted().with_assets(vec![old]);
        operation.check().unwrap();
        install(stage, spec.clone(), &AtomicBool::new(false)).unwrap();
        assert!(authority.check(&AtomicBool::new(false)).is_err());
        assert!(operation.check().is_err());
        assert_eq!(
            std::fs::read(spec.root.join("model")).unwrap(),
            b"synthetic model weights"
        );
        assert_eq!(admit(&spec, || Ok(())).unwrap().status(), Status::Ready);
    }

    #[test]
    fn rejected_staging_preserves_previous_installation_and_removes_temporary_content() {
        for failure in [
            "empty",
            "missing",
            "permissions",
            "receipt",
            "cancel",
            "destination",
        ] {
            let (_root, stage, mut spec) = prepared_fixture();
            let staging = stage.path().to_owned();
            let previous = spec.root.clone();
            std::fs::create_dir_all(&spec.root).unwrap();
            std::fs::write(spec.root.join("model"), b"previous synthetic model").unwrap();
            std::fs::write(
                spec.root.join(RECEIPT),
                b"previous receipt retained verbatim",
            )
            .unwrap();
            let cancel = AtomicBool::new(false);
            match failure {
                "empty" => std::fs::write(stage.path().join("model"), b"").unwrap(),
                "missing" => std::fs::remove_file(stage.path().join("lib/runtime.so.1")).unwrap(),
                "permissions" => spec.executables.push("model".into()),
                "receipt" => std::fs::write(stage.path().join(RECEIPT), b"malformed").unwrap(),
                "cancel" => cancel.store(true, Ordering::Release),
                "destination" => spec.root = spec.data.join("wrong-target"),
                _ => unreachable!(),
            }
            assert!(install(stage, spec.clone(), &cancel).is_err(), "{failure}");
            assert_eq!(
                std::fs::read(previous.join("model")).unwrap(),
                b"previous synthetic model",
                "{failure}"
            );
            assert_eq!(
                std::fs::read(previous.join(RECEIPT)).unwrap(),
                b"previous receipt retained verbatim",
                "{failure}"
            );
            assert!(!staging.exists(), "failed stage survived: {failure}");
        }
    }

    #[test]
    fn verified_archive_detects_same_size_corruption_missing_libraries_and_extra_files() {
        let (_root, spec) = fixture();
        let proof = verify_spec(&spec, &|| Ok(()))
            .unwrap_or_else(|status| panic!("verification: {status:?}"));
        assert!(proof.unchanged());
        let before = std::fs::metadata(spec.root.join("model")).unwrap();
        let bytes = std::fs::read(spec.root.join("model")).unwrap();
        let mut altered = bytes.clone();
        altered[0] ^= 1;
        std::fs::write(spec.root.join("model"), &altered).unwrap();
        let file = OpenOptions::new()
            .write(true)
            .open(spec.root.join("model"))
            .unwrap();
        file.set_times(std::fs::FileTimes::new().set_modified(before.modified().unwrap()))
            .unwrap();
        assert!(
            !proof.unchanged(),
            "restoring mtime must not conceal changed bytes"
        );
        assert!(matches!(
            verify_spec(&spec, &|| Ok(())),
            Err(Status::Invalid(_))
        ));
        std::fs::write(spec.root.join("model"), bytes).unwrap();
        assert!(verify_spec(&spec, &|| Ok(())).is_ok());
        std::fs::remove_file(spec.root.join("lib/runtime.so.1")).unwrap();
        assert!(
            verify_spec(&spec, &|| Ok(())).is_err(),
            "a present executable is not proof of complete runtime installation"
        );
        std::fs::write(
            spec.root.join("lib/runtime.so.1"),
            b"synthetic bundled library",
        )
        .unwrap();
        std::fs::write(spec.root.join("lib/unlisted.so"), b"synthetic injection").unwrap();
        assert!(verify_spec(&spec, &|| Ok(())).is_err());
    }

    #[test]
    fn receipt_source_signature_and_local_authority_cannot_be_substituted() {
        let (_root, spec) = fixture();
        let receipt = spec.root.join(RECEIPT);
        let original = std::fs::read(&receipt).unwrap();
        let mut value: serde_json::Value = serde_json::from_slice(&original).unwrap();
        value["claim"]["entries"][0]["path"] = "substituted".into();
        crate::private_fs::replace(&receipt, &serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(verify_spec(&spec, &|| Ok(())).is_err());
        crate::private_fs::replace(&receipt, &original).unwrap();
        let mut other = spec.clone();
        other.source = "00".repeat(32);
        assert!(verify_spec(&other, &|| Ok(())).is_err());
        let authority = key_path(&spec.data);
        let original_key = std::fs::read(&authority).unwrap();
        crate::private_fs::replace(&authority, &[7; 32]).unwrap();
        assert!(verify_spec(&spec, &|| Ok(())).is_err());
        crate::private_fs::replace(&authority, &original_key).unwrap();
        assert!(verify_spec(&spec, &|| Ok(())).is_ok());
        crate::private_fs::replace(&authority, &[]).unwrap();
        assert!(provision_key(&spec.data).is_err());
        assert!(
            std::fs::read(&authority).unwrap().is_empty(),
            "corrupt authority must not be silently replaced"
        );
    }

    #[test]
    fn verification_cancels_and_installer_authority_cannot_survive_control_replacement() {
        let (_root, spec) = fixture();
        let checks = AtomicUsize::new(0);
        let check = || {
            if checks.fetch_add(1, Ordering::AcqRel) >= 3 {
                Err("synthetic cancellation".into())
            } else {
                Ok(())
            }
        };
        assert!(verify_spec(&spec, &check).is_err());
        let guard = crate::artifact::observe_authority(&spec.data, &spec.key).unwrap();
        assert!(guard.check(&AtomicBool::new(false)).is_ok());
        let reset_lock = spec.data.join(".install/reset.lock");
        std::fs::rename(&reset_lock, spec.data.join(".install/preserved-reset.lock")).unwrap();
        std::fs::write(reset_lock, b"").unwrap();
        assert!(
            guard.check(&AtomicBool::new(false)).is_err(),
            "zero epochs must not permit an orphaned lock namespace"
        );
    }

    #[test]
    fn raw_content_requires_exact_sha_and_cold_archive_presence_is_unverified() {
        let (_root, mut spec) = fixture();
        std::fs::remove_file(spec.root.join(RECEIPT)).unwrap();
        assert!(matches!(
            verify_spec(&spec, &|| Ok(())),
            Err(Status::NeedsVerification)
        ));
        spec.raw = Some("model".into());
        spec.source = hex(&Sha256::digest(b"synthetic model weights"));
        assert!(verify_spec(&spec, &|| Ok(())).is_ok());
        std::fs::write(spec.root.join("model"), b"synthetic model weigh").unwrap();
        assert!(verify_spec(&spec, &|| Ok(())).is_err());
        std::fs::write(spec.root.join("model"), b"").unwrap();
        assert!(verify_spec(&spec, &|| Ok(())).is_err());
    }

    #[test]
    fn runtime_executability_and_unsafe_installed_paths_are_rejected() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let (_root, mut spec) = fixture();
        spec.raw = Some("model".into());
        spec.source = hex(&Sha256::digest(b"synthetic model weights"));
        spec.executables = vec!["model".into()];
        assert!(verify_spec(&spec, &|| Ok(())).is_err());
        std::fs::set_permissions(
            spec.root.join("model"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        assert!(verify_spec(&spec, &|| Ok(())).is_ok());
        std::fs::set_permissions(
            spec.root.join("model"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        assert!(verify_spec(&spec, &|| Ok(())).is_err());
        let (_root, spec) = fixture();
        symlink("../../outside", spec.root.join("escape")).unwrap();
        assert!(verify_spec(&spec, &|| Ok(())).is_err());
        std::fs::remove_file(spec.root.join("escape")).unwrap();
        let name =
            std::ffi::CString::new(spec.root.join("fifo").as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(verify_spec(&spec, &|| Ok(())).is_err());
    }

    #[test]
    fn deletion_during_verification_retires_authority_before_another_chunk() {
        let (_root, spec) = fixture();
        let checks = AtomicUsize::new(0);
        let check = || {
            if checks.fetch_add(1, Ordering::AcqRel) == 3 {
                crate::artifact::delete(&spec.data, &spec.key, &spec.root).unwrap();
            }
            Ok(())
        };
        assert!(verify_spec(&spec, &check).is_err());
        assert!(!spec.root.exists());
        assert!(
            checks.load(Ordering::Acquire) < 10,
            "revoked verification must not finish scanning"
        );
    }

    #[test]
    fn concurrent_receipt_key_creators_reuse_one_private_authority() {
        let root = tempfile::tempdir().unwrap();
        let mut tasks = Vec::new();
        for _ in 0..8 {
            let data = root.path().to_owned();
            tasks.push(std::thread::spawn(move || {
                provision_key(&data).unwrap().verifying_key().to_bytes()
            }));
        }
        let expected = tasks.remove(0).join().unwrap();
        for task in tasks {
            assert_eq!(task.join().unwrap(), expected);
        }
        assert_eq!(
            std::fs::metadata(key_path(root.path())).unwrap().mode() & 0o777,
            0o600
        );
        let before = stamp(&key_path(root.path())).unwrap();
        read_key(root.path()).unwrap();
        assert_eq!(
            before,
            stamp(&key_path(root.path())).unwrap(),
            "verification reads must not mutate the authority's timestamps"
        );
    }

    #[test]
    fn zero_byte_required_file_never_promotes() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("asset");
        crate::private_fs::directory(&destination).unwrap();
        std::fs::write(destination.join("model"), b"previous complete model").unwrap();
        let stage = Stage::new(root.path(), "fixture", destination.clone()).unwrap();
        // Empty staged payload plus a valid sibling: the empty file must
        // block installation before any promotion.
        std::fs::write(stage.path().join("model"), b"").unwrap();
        std::fs::write(stage.path().join("tokens.txt"), b"synthetic tokens").unwrap();
        let spec = Spec {
            data: root.path().into(),
            root: destination.clone(),
            key: "fixture".into(),
            source: "synthetic-source".into(),
            required: vec!["model".into()],
            executables: Vec::new(),
            raw: None,
        };
        assert!(install(stage, spec, &AtomicBool::new(false)).is_err());
        assert_eq!(
            std::fs::read(destination.join("model")).unwrap(),
            b"previous complete model"
        );
    }

    /// Installer-side active-use guarantee: content admitted for use must
    /// observe a later delete/reset before its next boundary. Engine
    /// retirement call sites live in the session layer (#22); this test
    /// pins the installer half of the joint review.
    #[test]
    fn delete_and_reset_retire_active_verified_content_during_use() {
        for reset_all in [false, true] {
            let (_root, spec) = fixture();
            let proof = admit(&spec, || Ok(())).unwrap();
            assert!(proof.unchanged());
            let operation =
                crate::operation::Operation::unrestricted().with_assets(vec![proof.clone()]);
            operation.check().unwrap();
            if reset_all {
                drop(crate::artifact::reset(&spec.data).unwrap());
            } else {
                crate::artifact::delete(&spec.data, &spec.key, &spec.root).unwrap();
            }
            assert!(!proof.unchanged(), "active proof survived mutation");
            assert!(
                operation
                    .check()
                    .unwrap_err()
                    .contains("deleted, reset or changed"),
                "active use was not revoked"
            );
            if reset_all {
                // Reset revokes authority without deleting bytes; a stale
                // proof must still fail even though files remain, while a
                // fresh admission re-verifies the preserved content.
                assert!(spec.root.join("model").exists());
                assert!(admit(&spec, || Ok(())).is_ok());
            } else {
                assert!(admit(&spec, || Ok(())).is_err());
                assert!(!spec.root.exists());
            }
        }
    }
}
