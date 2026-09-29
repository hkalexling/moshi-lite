//! Wire types for the subset of the Moshi host-gateway contract this service
//! implements. Field names follow the observed Moshi `0.4.x` responses.

use serde::{Deserialize, Serialize};

/// Envelope protocol version Moshi clients expect from a watch-capable host.
pub const PROTOCOL_VERSION: u32 = 1;

/// Capabilities advertised on `/v1/version` and the `/events` gateway frame.
pub fn capabilities() -> Vec<String> {
    vec!["events.watch.workspaces".to_string()]
}

/// `GET /v1/version`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VersionInfo {
    pub version: String,
    pub protocol_version: u32,
    pub capabilities: Vec<String>,
    pub hostname: String,
}

/// First frame of an `/events` connection.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GatewayFrame {
    pub version: String,
    pub protocol_version: u32,
    pub capabilities: Vec<String>,
}

/// `{"gateway": {...}}` — the `/events` hello envelope.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GatewaySnapshot {
    pub gateway: GatewayFrame,
}

/// `GET /v1/muxes`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MuxList {
    pub muxes: Vec<Mux>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Mux {
    pub id: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    pub running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
}

/// Normalized two-level workspace tree (`GET /v1/workspaces`, `workspaces`
/// watch frames).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceTree {
    pub kind: String,
    pub capabilities: TreeCapabilities,
    pub groups: Vec<TreeGroup>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TreeCapabilities {
    pub pane_list: bool,
    pub pane_focus: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TreeGroup {
    pub id: String,
    #[serde(default)]
    pub label: String,
    pub focused: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_changed_at: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_changed_at_approx: Option<bool>,
    pub children: Vec<TreeChild>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TreeChild {
    pub id: String,
    #[serde(default)]
    pub label: String,
    pub focused: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_remaining: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    pub pane_count: u32,
    pub agent_pane_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_change_order: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_changed_at: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_changed_at_approx: Option<bool>,
    pub panes: Vec<TreePane>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TreePane {
    pub id: String,
    #[serde(default)]
    pub label: String,
    pub focused: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_remaining: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_change_order: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_changed_at: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_changed_at_approx: Option<bool>,
}
