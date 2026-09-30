//! In-memory Herdr backend for tests. Always compiled so integration tests can
//! use it; the production binary never constructs it.

use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::{Value, json};

use super::{
    CreateRequest, CreateTarget, FocusTarget, HerdrBackend, HerdrError, NodeTarget, Snapshot,
};

#[derive(Default)]
pub struct MockBackend {
    snapshot: Mutex<Option<Snapshot>>,
    focus_calls: Mutex<Vec<FocusTarget>>,
    create_calls: Mutex<Vec<(CreateTarget, CreateRequest)>>,
    rename_calls: Mutex<Vec<(NodeTarget, String)>>,
    close_calls: Mutex<Vec<NodeTarget>>,
}

impl MockBackend {
    /// A backend that answers with the given snapshot.
    pub fn new(snapshot: Snapshot) -> Self {
        Self {
            snapshot: Mutex::new(Some(snapshot)),
            focus_calls: Mutex::new(Vec::new()),
            create_calls: Mutex::new(Vec::new()),
            rename_calls: Mutex::new(Vec::new()),
            close_calls: Mutex::new(Vec::new()),
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

    pub fn create_calls(&self) -> Vec<(CreateTarget, CreateRequest)> {
        self.create_calls
            .lock()
            .expect("create lock poisoned")
            .clone()
    }

    pub fn rename_calls(&self) -> Vec<(NodeTarget, String)> {
        self.rename_calls
            .lock()
            .expect("rename lock poisoned")
            .clone()
    }

    pub fn close_calls(&self) -> Vec<NodeTarget> {
        self.close_calls
            .lock()
            .expect("close lock poisoned")
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

    async fn rename(&self, target: NodeTarget, label: &str) -> Result<(), HerdrError> {
        self.rename_calls
            .lock()
            .expect("rename lock poisoned")
            .push((target, label.to_string()));
        Ok(())
    }

    async fn close(&self, target: NodeTarget) -> Result<(), HerdrError> {
        self.close_calls
            .lock()
            .expect("close lock poisoned")
            .push(target);
        Ok(())
    }

    async fn create(
        &self,
        target: CreateTarget,
        request: CreateRequest,
    ) -> Result<Value, HerdrError> {
        let label = request
            .label
            .clone()
            .or_else(|| request.cwd.clone())
            .unwrap_or_else(|| "new".to_string());
        let result = match &target {
            CreateTarget::Workspace => json!({
                "type": "workspace_created",
                "workspace": { "workspace_id": "wNEW", "label": label, "focused": true },
            }),
            CreateTarget::Tab(workspace_id) => json!({
                "type": "tab_created",
                "tab": {
                    "tab_id": format!("{workspace_id}:tNEW"),
                    "workspace_id": workspace_id,
                    "label": label,
                    "focused": true,
                },
            }),
            CreateTarget::TabPane(_) | CreateTarget::Pane(_) => json!({
                "type": "pane_created",
                "pane": { "pane_id": "wNEW:pNEW" },
            }),
        };
        self.create_calls
            .lock()
            .expect("create lock poisoned")
            .push((target, request));
        Ok(result)
    }
}
