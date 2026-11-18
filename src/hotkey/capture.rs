//! Coordinate shortcut recording between Settings and the service.
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const LOCK_NAME: &str = "hotkey-capture.lock";
type Nonce = [u8; 16];

pub(super) struct Suspension {
    pub(super) path: PathBuf,
    pub(super) ready: Box<dyn Fn(Nonce) + Send + Sync>,
}

pub(super) static SUSPENSION: OnceLock<Suspension> = OnceLock::new();

pub fn configure_capture_suspend(data_dir: &Path, ready: impl Fn(Nonce) + Send + Sync + 'static) {
    let _ = SUSPENSION.set(Suspension {
        path: data_dir.join(LOCK_NAME),
        ready: Box::new(ready),
    });
}

fn read_nonce(path: &Path) -> Option<Nonce> {
    crate::private_fs::read_private(path, 16)
        .ok()?
        .try_into()
        .ok()
}

impl Suspension {
    pub(super) fn request(&self) -> Option<Nonce> {
        read_nonce(&self.path.with_extension("request"))
    }

    fn acknowledge(&self, nonce: Nonce) -> io::Result<()> {
        match crate::private_fs::try_lock_shared(&self.path) {
            Ok(_) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                if self.request() == Some(nonce) {
                    crate::private_fs::replace(&self.path.with_extension("ack"), &nonce)?;
                }
                Ok(())
            }
            Err(e) => Err(e),
        }
    }
}

/// Called on the service's UI thread after it has handled the listener's
/// cancellation and any earlier queued key edges.
pub fn acknowledge_capture_suspend(nonce: Nonce) -> io::Result<()> {
    if let Some(suspension) = SUSPENSION.get() {
        suspension.acknowledge(nonce)?;
    }
    Ok(())
}

pub(crate) struct CaptureRequest {
    path: PathBuf,
    nonce: Nonce,
    _guard: File,
}

impl CaptureRequest {
    pub(crate) fn begin(data_dir: &Path) -> io::Result<Self> {
        let path = data_dir.join(LOCK_NAME);
        let guard = crate::private_fs::try_lock(&path)?;
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).map_err(io::Error::other)?;
        crate::private_fs::replace(&path.with_extension("request"), &nonce)?;
        Ok(Self {
            path,
            nonce,
            _guard: guard,
        })
    }

    pub(crate) fn service_running(&self) -> io::Result<bool> {
        match crate::private_fs::try_lock(&self.path.with_file_name("engine.lock")) {
            Ok(_) => Ok(false),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(true),
            Err(e) => Err(e),
        }
    }

    pub(crate) fn nonce(&self) -> Nonce {
        self.nonce
    }

    pub(crate) fn acknowledged(&self) -> bool {
        read_nonce(&self.path.with_extension("ack")) == Some(self.nonce)
    }
}

impl Drop for CaptureRequest {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.path.with_extension("request"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_requires_the_current_ack_and_releases_the_lock() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(LOCK_NAME);
        let service = Suspension {
            path: path.clone(),
            ready: Box::new(|_| {}),
        };
        let engine = crate::private_fs::try_lock(&root.path().join("engine.lock")).unwrap();
        let request = CaptureRequest::begin(root.path()).unwrap();
        assert!(request.service_running().unwrap());
        assert!(!request.acknowledged());
        assert_eq!(service.request(), Some(request.nonce()));
        assert_eq!(
            crate::private_fs::try_lock_shared(&path)
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock
        );
        service.acknowledge(request.nonce()).unwrap();
        assert!(request.acknowledged());
        let stale = request.nonce();
        drop(request);
        assert!(service.request().is_none());
        assert!(crate::private_fs::try_lock_shared(&path).is_ok());

        let next = CaptureRequest::begin(root.path()).unwrap();
        service.acknowledge(stale).unwrap();
        assert!(!next.acknowledged());
        drop(engine);
        assert!(!next.service_running().unwrap());
    }
}
