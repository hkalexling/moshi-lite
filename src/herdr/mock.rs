//! In-memory Herdr backend for tests. Always compiled so integration tests can
//! use it; the production binary never constructs it.

use std::sync::Mutex;

use async_trait::async_trait;

use super::{FocusTarget, HerdrBackend, HerdrError, Snapshot};

#[derive(Default)]
pub struct MockBackend {
    snapshot: Mutex<Option<Snapshot>>,
    focus_calls: Mutex<Vec<FocusTarget>>,
}

impl MockBackend {
    /// A backend that answers with the given snapshot.
    pub fn new(snapshot: Snapshot) -> Self {
        Self {
            snapshot: Mutex::new(Some(snapshot)),
            focus_calls: Mutex::new(Vec::new()),
        }
    }

    /// A backend that fails every snapshot request (Herdr unavailable).
    pub fn unavailable() -> Self {
        Self::default()
    }

    /// Replace the snapshot the backend serves.
    pub fn set_snapshot(&self, snapshot: Snapshot) {
        *self.snapshot.lock().expect("snapshot lock poisoned") = Some(snapshot);
    }

    pub fn focus_calls(&self) -> Vec<FocusTarget> {
        self.focus_calls
            .lock()
            .expect("focus lock poisoned")
            .clone()
    }
}

#[async_trait]
impl HerdrBackend for MockBackend {
    async fn snapshot(&self) -> Result<Snapshot, HerdrError> {
        self.snapshot
            .lock()
            .expect("snapshot lock poisoned")
            .clone()
            .ok_or_else(|| HerdrError::Protocol("herdr unavailable (mock)".to_string()))
    }

    async fn focus(&self, target: FocusTarget) -> Result<(), HerdrError> {
        self.focus_calls
            .lock()
            .expect("focus lock poisoned")
            .push(target);
        Ok(())
    }
}
