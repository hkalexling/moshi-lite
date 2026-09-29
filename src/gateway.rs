//! Loopback HTTP + WebSocket gateway implementing the Moshi sidebar contract.

use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::herdr::{FocusTarget, HerdrError};
use crate::model::{
    GatewayFrame, GatewaySnapshot, Mux, MuxList, PROTOCOL_VERSION, VersionInfo, WorkspaceTree,
    capabilities,
};
use crate::state::AppState;

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/v1/version", get(get_version))
        .route("/v1/muxes", get(get_muxes))
        .route("/v1/workspaces", get(get_workspaces))
        .route("/v1/workspaces/panes", get(get_workspace_panes))
        .route("/v1/workspaces/focus", post(post_workspaces_focus))
        .route("/events", get(get_events))
        .with_state(state)
}

/// Query string of `/v1/workspaces`. Session-lookup parameters are accepted
/// and ignored for now: the tree falls back to the loopback Herdr resolution.
#[derive(Debug, Default, Deserialize)]
struct WorkspaceQuery {
    #[serde(default)]
    mux: Option<String>,
    #[serde(default, rename = "ssh-connection")]
    _ssh_connection: Option<String>,
    #[serde(default, rename = "mosh-port")]
    _mosh_port: Option<String>,
    #[serde(default, rename = "mosh-host")]
    _mosh_host: Option<String>,
    #[serde(default, rename = "et-client-id")]
    _et_client_id: Option<String>,
    #[serde(default)]
    _et: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct PanesQuery {
    #[serde(default, rename = "groupId")]
    group_id: Option<String>,
    #[serde(default, rename = "childId")]
    child_id: Option<String>,
}

/// Focus request. The exact Moshi body is not documented, so this accepts the
/// plausible key spellings and resolves the most specific target.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FocusRequest {
    #[serde(default, alias = "workspace_id")]
    workspace_id: Option<String>,
    #[serde(default, alias = "group_id")]
    group_id: Option<String>,
    #[serde(default, alias = "tab_id")]
    tab_id: Option<String>,
    #[serde(default, alias = "child_id")]
    child_id: Option<String>,
    #[serde(default, alias = "pane_id")]
    pane_id: Option<String>,
}

impl FocusRequest {
    fn into_target(self) -> Option<FocusTarget> {
        if let Some(pane) = self.pane_id {
            return Some(FocusTarget::Pane(pane));
        }
        if let Some(tab) = self.tab_id.or(self.child_id) {
            return Some(FocusTarget::Tab(tab));
        }
        self.workspace_id
            .or(self.group_id)
            .map(FocusTarget::Workspace)
    }
}

struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn unprocessable(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            message: message.into(),
        }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
        }
    }

    fn backend(error: &HerdrError) -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            message: error.to_string(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message }))).into_response()
    }
}

async fn get_version(State(state): State<Arc<AppState>>) -> Json<VersionInfo> {
    Json(VersionInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        protocol_version: PROTOCOL_VERSION,
        capabilities: capabilities(),
        hostname: state.hostname.clone(),
    })
}

async fn get_muxes(State(state): State<Arc<AppState>>) -> Json<MuxList> {
    let running = state.current_tree().is_some();
    Json(MuxList {
        muxes: vec![Mux {
            id: "herdr:default".to_string(),
            kind: "herdr".to_string(),
            session: Some("default".to_string()),
            running,
            active: Some(true),
        }],
    })
}

async fn get_workspaces(
    State(state): State<Arc<AppState>>,
    Query(query): Query<WorkspaceQuery>,
) -> Result<Json<WorkspaceTree>, ApiError> {
    if let Some(mux) = query.mux.as_deref()
        && !mux.starts_with("herdr")
    {
        return Err(ApiError::unprocessable(format!(
            "unsupported mux {mux:?}: this gateway serves Herdr only"
        )));
    }
    state
        .current_tree()
        .map(|tree| Json((*tree).clone()))
        .ok_or_else(|| ApiError::unprocessable("no local herdr mux available"))
}

async fn get_workspace_panes(
    State(state): State<Arc<AppState>>,
    Query(query): Query<PanesQuery>,
) -> Result<Json<Value>, ApiError> {
    let tree = state
        .current_tree()
        .ok_or_else(|| ApiError::unprocessable("no local herdr mux available"))?;
    let group = query
        .group_id
        .as_deref()
        .and_then(|id| tree.groups.iter().find(|group| group.id == id))
        .or_else(|| tree.groups.first())
        .ok_or_else(|| ApiError::not_found("workspace not found"))?;
    let child = query
        .child_id
        .as_deref()
        .and_then(|id| group.children.iter().find(|child| child.id == id))
        .or_else(|| group.children.first())
        .ok_or_else(|| ApiError::not_found("tab not found"))?;
    Ok(Json(json!({
        "groupId": group.id,
        "childId": child.id,
        "panes": child.panes,
    })))
}

async fn post_workspaces_focus(
    State(state): State<Arc<AppState>>,
    Json(request): Json<FocusRequest>,
) -> Result<Json<Value>, ApiError> {
    let target = request
        .into_target()
        .ok_or_else(|| ApiError::unprocessable("focus needs a workspace, tab, or pane id"))?;

    let is_pane = matches!(target, FocusTarget::Pane(_));
    match state.backend.focus(target.clone()).await {
        Ok(()) => Ok(Json(json!({ "ok": true }))),
        Err(primary) if is_pane => {
            // Fall back to agent focus for servers that predate pane.focus.
            let FocusTarget::Pane(pane_id) = target else {
                return Err(ApiError::backend(&primary));
            };
            match state.backend.focus(FocusTarget::Agent(pane_id)).await {
                Ok(()) => Ok(Json(json!({ "ok": true, "fallback": "agent" }))),
                Err(_) => Err(ApiError::backend(&primary)),
            }
        }
        Err(error) => Err(ApiError::backend(&error)),
    }
}

async fn get_events(ws: WebSocketUpgrade, State(state): State<Arc<AppState>>) -> Response {
    ws.on_upgrade(move |socket| events_session(socket, state))
}

async fn events_session(mut socket: WebSocket, state: Arc<AppState>) {
    let hello = GatewaySnapshot {
        gateway: GatewayFrame {
            version: env!("CARGO_PKG_VERSION").to_string(),
            protocol_version: PROTOCOL_VERSION,
            capabilities: capabilities(),
        },
    };
    if send_json(&mut socket, &hello).await.is_err() {
        return;
    }

    let mut trees = state.subscribe_tree();
    let mut watching = false;
    loop {
        tokio::select! {
            message = socket.recv() => {
                match message {
                    Some(Ok(Message::Text(text))) => {
                        if !watch_requests_workspaces(text.as_str()) {
                            continue;
                        }
                        watching = true;
                        if send_json(&mut socket, &json!({ "watching": { "workspaces": true } }))
                            .await
                            .is_err()
                        {
                            break;
                        }
                        if let Some(tree) = state.current_tree()
                            && send_json(&mut socket, &json!({ "workspaces": tree.as_ref() })).await.is_err()
                        {
                            break;
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(_)) => break,
                }
            }
            changed = trees.changed(), if watching => {
                if changed.is_err() {
                    break;
                }
                let tree = { trees.borrow_and_update().as_ref().cloned() };
                if let Some(tree) = tree
                    && send_json(&mut socket, &json!({ "workspaces": tree.as_ref() })).await.is_err()
                {
                    break;
                }
            }
        }
    }
}

fn watch_requests_workspaces(text: &str) -> bool {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|value| {
            value
                .get("watch")
                .and_then(|watch| watch.get("workspaces"))
                .and_then(Value::as_bool)
        })
        .unwrap_or(false)
}

async fn send_json<T: serde::Serialize>(
    socket: &mut WebSocket,
    value: &T,
) -> Result<(), axum::Error> {
    let text = serde_json::to_string(value).map_err(axum::Error::new)?;
    socket.send(Message::Text(text.into())).await
}
