//! Shared application state: config, Herdr backend, latest tree, and the
//! background poller that keeps the tree fresh.

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::watch;
use tracing::warn;

use crate::config::Config;
use crate::herdr::{HerdrBackend, HerdrError};
use crate::mapping::{StatusTracker, build_tree};
use crate::model::WorkspaceTree;

pub struct AppState {
    pub config: Config,
    pub backend: Arc<dyn HerdrBackend>,
    pub hostname: String,
    tree_tx: watch::Sender<Option<Arc<WorkspaceTree>>>,
    tracker: Mutex<StatusTracker>,
}

impl AppState {
    pub fn new(config: Config, backend: Arc<dyn HerdrBackend>, hostname: String) -> Self {
        let (tree_tx, _) = watch::channel(None);
        Self {
            config,
            backend,
            hostname,
            tree_tx,
            tracker: Mutex::new(StatusTracker::default()),
        }
    }

    /// A receiver that yields a new value whenever the tree changes.
    pub fn subscribe_tree(&self) -> watch::Receiver<Option<Arc<WorkspaceTree>>> {
        self.tree_tx.subscribe()
    }

    /// The most recently built tree, if any.
    pub fn current_tree(&self) -> Option<Arc<WorkspaceTree>> {
        self.tree_tx.borrow().clone()
    }

    /// Rebuild the tree once from the backend. Shared by the poller and tests.
    pub async fn poll_once(&self) -> Result<Arc<WorkspaceTree>, HerdrError> {
        let snapshot = self.backend.snapshot().await?;
        let tree = {
            let mut tracker = self.tracker.lock().expect("status tracker poisoned");
            Arc::new(build_tree(&snapshot, &mut tracker, now_secs()))
        };
        let changed = self
            .tree_tx
            .borrow()
            .as_ref()
            .is_none_or(|previous| previous.as_ref() != tree.as_ref());
        if changed {
            // `send_replace` stores even when no subscriber is attached.
            let _ = self.tree_tx.send_replace(Some(Arc::clone(&tree)));
        }
        Ok(tree)
    }

    /// Spawn the background poller. The first tick runs immediately.
    pub fn spawn_poller(self: &Arc<Self>) {
        let state = Arc::clone(self);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(state.config.poll_interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                if let Err(error) = state.poll_once().await {
                    warn!(%error, "herdr snapshot poll failed");
                }
            }
        });
    }
}

pub fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |duration| duration.as_secs_f64())
}
