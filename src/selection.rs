//! Clipboard-permission guard contract owned by the installer layer.
//!
//! The full clipboard delivery implementation (owned Wayland selection,
//! CLI fallback) lives in the clipboard layer (#21,
//! `fix/audit-clipboard-ownership`), which restores this module's
//! implementation body on restack from blob
//! `0dfdf7a64a012d2f64ad8560cbb5bf8e3711db34`. This module retains the
//! real guard type consumed by `crate::operation::Operation::guard`; it
//! is constructed from live installer authorization state, not a stub.
use std::sync::Arc;

/// Permission closure consulted before provider IO proceeds.
/// `true` keeps the operation permitted; `false` revokes it.
pub(crate) type Guard = Arc<dyn Fn() -> bool + Send + Sync>;
