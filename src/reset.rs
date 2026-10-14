//! Reset intent boundary; the owned reset worker is integrated by the session layer.
use std::path::Path;
const INTENT: &str = ".reset-intent";
pub(crate) fn requested(data: &Path) -> std::io::Result<bool> {
    match std::fs::symlink_metadata(data.join(INTENT)) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}
