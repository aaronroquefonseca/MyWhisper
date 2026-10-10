//! Cancellation and durable capture authorization shared by provider IO.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub(crate) struct Operation {
    cancel: Arc<AtomicBool>,
    parent_cancel: Option<Arc<AtomicBool>>,
    permitted: Arc<dyn Fn() -> Result<(), Denial> + Send + Sync>,
    denial: Arc<Mutex<Option<Denial>>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Denial {
    Revoked,
    Unavailable,
}

impl Denial {
    fn description(self) -> &'static str {
        match self {
            Self::Revoked => "capture policy changed or its database was replaced",
            Self::Unavailable => "capture policy could not be read; existing data preserved",
        }
    }
}

impl Operation {
    pub(crate) fn unrestricted() -> Self {
        Self::cancellable(Arc::new(AtomicBool::new(false)))
    }

    pub(crate) fn cancellable(cancel: Arc<AtomicBool>) -> Self {
        Self {
            cancel,
            parent_cancel: None,
            permitted: Arc::new(|| Ok(())),
            denial: Arc::new(Mutex::new(None)),
        }
    }

    pub(crate) fn capture(
        store: crate::store::Store,
        epoch: i64,
        cancel: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        use std::os::unix::fs::MetadataExt;
        let file = store
            .path()
            .zip(store.file_identity())
            .map(|(path, (device, inode))| (path.to_owned(), device, inode));
        store
            .policy_reads_only()
            .map_err(|_| "capture policy reader unavailable")?;
        let store = Mutex::new(store);
        let operation = Self {
            cancel,
            parent_cancel: None,
            denial: Arc::new(Mutex::new(None)),
            permitted: Arc::new(move || {
                if let Some((path, device, inode)) = &file {
                    let metadata = std::fs::symlink_metadata(path).map_err(|error| {
                        if error.kind() == std::io::ErrorKind::NotFound {
                            Denial::Revoked
                        } else {
                            Denial::Unavailable
                        }
                    })?;
                    if !metadata.is_file() || metadata.dev() != *device || metadata.ino() != *inode
                    {
                        return Err(Denial::Revoked);
                    }
                }
                let policy = store.lock().map_err(|_| Denial::Unavailable)?;
                if policy.reset_pending().map_err(|_| Denial::Unavailable)? {
                    return Err(Denial::Revoked);
                }
                let current = policy.capture_epoch().map_err(|_| Denial::Unavailable)?;
                if current == epoch {
                    Ok(())
                } else {
                    Err(Denial::Revoked)
                }
            }),
        };
        operation.check()?;
        Ok(operation)
    }

    pub(crate) fn permitted(&self) -> bool {
        self.check().is_ok()
    }

    pub(crate) fn check(&self) -> Result<(), String> {
        if let Some(reason) = *self.denial.lock().unwrap_or_else(|p| p.into_inner()) {
            self.cancel.store(true, Ordering::Release);
            return Err(reason.description().into());
        }
        if self.cancel.load(Ordering::Acquire)
            || self
                .parent_cancel
                .as_ref()
                .is_some_and(|cancel| cancel.load(Ordering::Acquire))
        {
            self.cancel.store(true, Ordering::Release);
            return Err("operation cancelled".into());
        }
        if let Err(reason) = (self.permitted)() {
            self.denial
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get_or_insert(reason);
            self.cancel.store(true, Ordering::Release);
            return Err(reason.description().into());
        }
        Ok(())
    }

    pub(crate) fn policy_unavailable(&self) -> bool {
        *self.denial.lock().unwrap_or_else(|p| p.into_inner()) == Some(Denial::Unavailable)
    }

    pub(crate) fn guard(&self) -> crate::selection::Guard {
        let operation = self.clone();
        Arc::new(move || operation.permitted())
    }

    pub(crate) fn cancel_flag(&self) -> &AtomicBool {
        &self.cancel
    }

    pub(crate) fn with_cancel(&self, cancel: Arc<AtomicBool>) -> Self {
        Self {
            cancel,
            parent_cancel: Some(self.cancel.clone()),
            permitted: self.permitted.clone(),
            denial: self.denial.clone(),
        }
    }

    pub(crate) fn next_phase(&self, cancel: Arc<AtomicBool>) -> Self {
        Self {
            cancel,
            parent_cancel: None,
            permitted: self.permitted.clone(),
            denial: self.denial.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unreadable_capture_policy_stays_denied_after_recovery() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state.db");
        let key = [0x21; 32];
        let store = crate::store::Store::open_with_key(&path, &key).unwrap();
        let epoch = store.capture_epoch().unwrap();
        let operation = Operation::capture(store, epoch, Arc::new(AtomicBool::new(false))).unwrap();
        let writer = rusqlite::Connection::open(&path).unwrap();
        writer
            .execute_batch(&format!("PRAGMA key = \"x'{}'\"", "21".repeat(32)))
            .unwrap();
        writer
            .execute_batch("ALTER TABLE sync_meta RENAME TO held_policy")
            .unwrap();
        assert!(operation.check().unwrap_err().contains("could not be read"));
        assert!(operation.policy_unavailable());
        writer
            .execute_batch("ALTER TABLE held_policy RENAME TO sync_meta")
            .unwrap();
        assert!(operation.check().is_err());
        let next = operation.next_phase(Arc::new(AtomicBool::new(false)));
        assert!(next.check().is_err() && next.policy_unavailable());
        drop(writer);
        let recovered = crate::store::Store::open_with_key(&path, &key).unwrap();
        let fresh = Operation::capture(recovered, epoch, Arc::new(AtomicBool::new(false))).unwrap();
        assert!(fresh.permitted());
    }

    #[test]
    fn capture_authority_survives_phase_change_but_not_database_replacement() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state.db");
        let store = crate::store::Store::open_with_key(&path, &[21; 32]).unwrap();
        store.checkpoint().unwrap();
        let epoch = store.capture_epoch().unwrap();
        let held_cancel = Arc::new(AtomicBool::new(false));
        let held = Operation::capture(store, epoch, held_cancel.clone()).unwrap();
        let delayed_reader = crate::store::Store::open_with_key(&path, &[21; 32]).unwrap();
        let released = held.next_phase(Arc::new(AtomicBool::new(false)));
        held_cancel.store(true, Ordering::Release);
        assert!(!held.permitted() && released.permitted());
        let replacement = root.path().join("replacement.db");
        let fresh = crate::store::Store::open_with_key(&replacement, &[21; 32]).unwrap();
        assert_eq!(fresh.capture_epoch().unwrap(), epoch);
        fresh.checkpoint().unwrap();
        drop(fresh);
        std::fs::rename(&path, root.path().join("old.db")).unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        assert!(
            Operation::capture(delayed_reader, epoch, Arc::new(AtomicBool::new(false))).is_err(),
            "authority cannot adopt a replacement made after its database was opened"
        );
        assert!(
            !released.permitted(),
            "a fresh database with the same epoch cannot inherit old authority"
        );
        let another_phase = released.next_phase(Arc::new(AtomicBool::new(false)));
        assert!(!another_phase.permitted());
    }
}
