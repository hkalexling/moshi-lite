//! Herdr adapter: typed snapshot reads and focus actions over the Herdr
//! socket API (protocol 22, JSON lines with `{id, method, params}`).

pub mod mock;
pub mod socket;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;

/// Result of `session.snapshot` — the subset used by the sidebar.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Snapshot {
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub protocol: Option<u32>,
    #[serde(default)]
    pub workspaces: Vec<HerdrWorkspace>,
    #[serde(default)]
    pub tabs: Vec<HerdrTab>,
    #[serde(default)]
    pub panes: Vec<HerdrPane>,
    #[serde(default)]
    pub agents: Vec<HerdrAgent>,
    #[serde(default)]
    pub focused_workspace_id: Option<String>,
    #[serde(default)]
    pub focused_tab_id: Option<String>,
    #[serde(default)]
    pub focused_pane_id: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct HerdrWorkspace {
    pub workspace_id: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub number: Option<u32>,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub pane_count: Option<u32>,
    #[serde(default)]
    pub tab_count: Option<u32>,
    #[serde(default)]
    pub active_tab_id: Option<String>,
    #[serde(default)]
    pub agent_status: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct HerdrTab {
    pub tab_id: String,
    pub workspace_id: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub number: Option<u32>,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub pane_count: Option<u32>,
    #[serde(default)]
    pub agent_status: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct HerdrPane {
    pub pane_id: String,
    pub tab_id: String,
    pub workspace_id: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub foreground_cwd: Option<String>,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub agent_status: Option<String>,
    #[serde(default)]
    pub agent_session: Option<AgentSession>,
    #[serde(default)]
    pub terminal_title: Option<String>,
    #[serde(default)]
    pub revision: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct HerdrAgent {
    pub pane_id: String,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub agent_status: Option<String>,
    #[serde(default)]
    pub agent_session: Option<AgentSession>,
    #[serde(default)]
    pub state_change_seq: Option<u64>,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AgentSession {
    #[serde(default)]
    pub agent: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub value: String,
}

/// A focus action expressed in the gateway's terms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FocusTarget {
    Workspace(String),
    Tab(String),
    Pane(String),
    Agent(String),
}

/// A rename/close target expressed in the gateway's terms. The HTTP layer
/// resolves the most specific id first: pane, then tab, then workspace, which
/// is also the precedence the official daemon uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeTarget {
    Workspace(String),
    Tab(String),
    Pane(String),
}

impl NodeTarget {
    /// JSON field name the app uses for this id.
    pub fn field(&self) -> &'static str {
        match self {
            NodeTarget::Workspace(_) => "workspaceId",
            NodeTarget::Tab(_) => "tabId",
            NodeTarget::Pane(_) => "paneId",
        }
    }

    pub fn id(&self) -> &str {
        match self {
            NodeTarget::Workspace(id) | NodeTarget::Tab(id) | NodeTarget::Pane(id) => id,
        }
    }
}

/// A workspace-create action expressed in the gateway's terms.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CreateRequest {
    pub cwd: Option<String>,
    pub label: Option<String>,
    pub env: std::collections::BTreeMap<String, String>,
}

/// Which node a create action should produce. The official daemon keys this on
/// the most specific id in the request body (`paneId` > `tabId` >
/// `workspaceId` > `cwd`) and always focuses the new node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateTarget {
    /// No parent: a new workspace (`workspace.create`).
    Workspace,
    /// A new tab in this workspace (`tab.create`).
    Tab(String),
    /// A new split in the focused pane of this tab (`pane.split`).
    TabPane(String),
    /// A new split in this pane (`pane.split`).
    Pane(String),
}

impl CreateTarget {
    /// The scope name the app sees in the create response.
    pub fn scope(&self) -> &'static str {
        match self {
            CreateTarget::Workspace => "workspace",
            CreateTarget::Tab(_) => "tab",
            CreateTarget::TabPane(_) | CreateTarget::Pane(_) => "pane",
        }
    }
}

/// Errors surfaced by a Herdr backend.
#[derive(Debug, thiserror::Error)]
pub enum HerdrError {
    #[error("herdr io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("herdr protocol error: {0}")]
    Protocol(String),
    #[error("herdr rpc error for {method}: {message}")]
    Rpc { method: String, message: String },
    #[error("herdr response did not contain a snapshot payload")]
    MissingSnapshot,
    #[error("herdr request timed out")]
    Timeout,
}

/// Read/write access to a Herdr server.
#[async_trait]
pub trait HerdrBackend: Send + Sync + 'static {
    /// Fetch the full live session snapshot.
    async fn snapshot(&self) -> Result<Snapshot, HerdrError>;

    /// Focus a workspace, tab, pane, or agent.
    async fn focus(&self, target: FocusTarget) -> Result<(), HerdrError>;

    /// Rename a workspace, tab, or pane.
    async fn rename(&self, target: NodeTarget, label: &str) -> Result<(), HerdrError>;

    /// Close a workspace, tab, or pane.
    async fn close(&self, target: NodeTarget) -> Result<(), HerdrError>;

    /// Create a workspace, tab, or split pane. Returns the raw Herdr result.
    async fn create(
        &self,
        target: CreateTarget,
        request: CreateRequest,
    ) -> Result<Value, HerdrError>;
}
