//! Loopback HTTP + WebSocket gateway implementing the Moshi sidebar contract.

use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::cli;
use crate::herdr::{CreateRequest, CreateTarget, FocusTarget, HerdrError, NodeTarget};
use crate::model::{
    GatewayFrame, Mux, MuxList, PROTOCOL_VERSION, TreeChild, TreeGroup, TreePane, VersionInfo,
    WorkspaceTree, capabilities,
};
use crate::state::AppState;

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/v1/version", get(get_version))
        .route("/v1/muxes", get(get_muxes))
        .route("/v1/workspaces", get(get_workspaces))
        .route("/v1/workspaces/panes", get(get_workspace_panes))
        .route("/v1/workspaces/focus", post(post_workspaces_focus))
        .route("/v1/workspaces/create", post(post_workspaces_create))
        .route("/v1/workspaces/rename", post(post_workspaces_rename))
        .route("/v1/workspaces/close", post(post_workspaces_close))
        .route("/v1/diff/start", get(diff_start_probe).post(diff_start))
        .route(
            "/v1/integrations",
            get(get_integrations).post(post_integrations),
        )
        .route("/events", get(get_events))
        .layer(middleware::from_fn(log_request))
        .with_state(state)
}

/// Per-request debug logging, including request bodies up to 1 MiB so the
/// app's undocumented actions can be inspected with `RUST_LOG=debug`.
async fn log_request(request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let uri = request.uri().clone();
    let user_agent = request
        .headers()
        .get("user-agent")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    let (parts, body) = request.into_parts();
    let bytes = axum::body::to_bytes(body, 1024 * 1024)
        .await
        .unwrap_or_default();
    if !bytes.is_empty() {
        tracing::debug!(body = %String::from_utf8_lossy(&bytes), "gateway request body");
    }
    let response = next
        .run(Request::from_parts(parts, Body::from(bytes)))
        .await;
    tracing::debug!(
        %method,
        %uri,
        status = %response.status(),
        %user_agent,
        "gateway request"
    );
    response
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

/// `/events` query parameters. `doctor=refresh` asks for an immediate doctor
/// frame; the session-lookup parameters are accepted but ignored for now.
#[derive(Debug, Default, Deserialize)]
struct EventsQuery {
    #[serde(default)]
    doctor: Option<String>,
    #[serde(default, rename = "session")]
    _session: Option<String>,
    #[serde(default, rename = "sshConnection")]
    _ssh_connection: Option<String>,
    #[serde(default, rename = "moshPort")]
    _mosh_port: Option<String>,
    #[serde(default, rename = "moshHost")]
    _mosh_host: Option<String>,
    #[serde(default, rename = "etClientId")]
    _et_client_id: Option<String>,
    #[serde(default)]
    _mux: Option<String>,
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

/// Rename/close request. The app sends `tabId`; the official daemon also
/// accepts `workspaceId` and `paneId`, preferring the most specific id.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NodeRequest {
    #[serde(default, alias = "workspace_id", alias = "group_id")]
    workspace_id: Option<String>,
    #[serde(default, alias = "tab_id", alias = "child_id")]
    tab_id: Option<String>,
    #[serde(default, alias = "pane_id")]
    pane_id: Option<String>,
    #[serde(default, alias = "name")]
    label: Option<String>,
}

impl NodeRequest {
    fn resolve_target(&self) -> Option<NodeTarget> {
        if let Some(pane) = &self.pane_id {
            return Some(NodeTarget::Pane(pane.clone()));
        }
        if let Some(tab) = &self.tab_id {
            return Some(NodeTarget::Tab(tab.clone()));
        }
        self.workspace_id.clone().map(NodeTarget::Workspace)
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

    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
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

/// The app's create action. It is part of the app-mux contract but not listed
/// in the published API docs, so the body is accepted leniently. The most
/// specific target wins: `paneId` splits that pane, `tabId` splits the tab's
/// focused pane, `workspaceId` creates a tab, and a bare `cwd` creates a
/// workspace. The new node is always focused, like the official daemon.
async fn post_workspaces_create(
    State(state): State<Arc<AppState>>,
    body: Bytes,
) -> Result<Json<Value>, ApiError> {
    let request: Value = if body.is_empty() {
        Value::Object(serde_json::Map::new())
    } else {
        serde_json::from_slice(&body).map_err(|error| {
            ApiError::bad_request(format!("invalid workspace create body: {error}"))
        })?
    };
    let cwd = string_field(&request, &["cwd", "path", "directory"]);
    let target = create_target(&request, cwd.as_deref()).ok_or_else(|| {
        ApiError::bad_request(
            "workspace create requires a parent target or cwd: missing paneId, tabId, workspaceId, or cwd",
        )
    })?;
    let create = CreateRequest {
        cwd,
        label: string_field(&request, &["label", "name"]),
        env: string_map_field(&request, "env"),
    };
    let scope = target.scope();
    let result = state
        .backend
        .create(target, create)
        .await
        .map_err(|error| ApiError::backend(&error))?;

    let mut response = json!({
        "ok": true,
        "kind": "herdr",
        "scope": scope,
        "result": result,
    });
    for key in ["workspace", "tab", "pane"] {
        if let Some(value) = result.get(key) {
            response[key] = value.clone();
        }
    }
    Ok(Json(response))
}

/// Resolve a create body to the node level the official daemon would create.
fn create_target(request: &Value, cwd: Option<&str>) -> Option<CreateTarget> {
    if let Some(pane) = string_field(request, &["paneId", "pane_id"]) {
        return Some(CreateTarget::Pane(pane));
    }
    if let Some(tab) = string_field(request, &["tabId", "tab_id", "childId", "child_id"]) {
        return Some(CreateTarget::TabPane(tab));
    }
    if let Some(workspace) = string_field(
        request,
        &["workspaceId", "workspace_id", "groupId", "group_id"],
    ) {
        return Some(CreateTarget::Tab(workspace));
    }
    cwd.map(|_| CreateTarget::Workspace)
}

/// The app's rename action, also outside the published API docs. The app sends
/// `{"tabId":"…","label":"…"}`; the official daemon additionally accepts
/// `workspaceId` and `paneId`, preferring the most specific id.
async fn post_workspaces_rename(
    State(state): State<Arc<AppState>>,
    body: Bytes,
) -> Result<Json<Value>, ApiError> {
    let request: NodeRequest = parse_request_body(&body)?;
    let label = request.label.as_deref().ok_or_else(|| {
        ApiError::bad_request("workspace rename requires a target and label: missing label")
    })?;
    let target = request.resolve_target().ok_or_else(|| {
        ApiError::bad_request(
            "workspace rename requires a target and label: missing workspaceId, tabId, or paneId",
        )
    })?;
    state
        .backend
        .rename(target.clone(), label)
        .await
        .map_err(|error| ApiError::backend(&error))?;

    let mut renamed = serde_json::Map::new();
    renamed.insert(target.field().to_string(), json!(target.id()));
    renamed.insert("label".to_string(), json!(label));
    Ok(Json(json!({
        "ok": true,
        "kind": "herdr",
        "renamed": Value::Object(renamed),
    })))
}

/// The app's close action: `{"tabId":"…"}`, or `workspaceId`/`paneId`.
async fn post_workspaces_close(
    State(state): State<Arc<AppState>>,
    body: Bytes,
) -> Result<Json<Value>, ApiError> {
    let request: NodeRequest = parse_request_body(&body)?;
    let target = request.resolve_target().ok_or_else(|| {
        ApiError::bad_request(
            "workspace close requires a target: missing workspaceId, tabId, or paneId",
        )
    })?;
    state
        .backend
        .close(target.clone())
        .await
        .map_err(|error| ApiError::backend(&error))?;

    let mut closed = serde_json::Map::new();
    closed.insert(target.field().to_string(), json!(target.id()));
    Ok(Json(json!({
        "ok": true,
        "kind": "herdr",
        "closed": Value::Object(closed),
    })))
}

fn parse_request_body<T>(body: &Bytes) -> Result<T, ApiError>
where
    T: serde::de::DeserializeOwned + Default,
{
    if body.is_empty() {
        return Ok(T::default());
    }
    serde_json::from_slice(body)
        .map_err(|error| ApiError::bad_request(format!("invalid request body: {error}")))
}

fn string_field(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str).map(str::to_string))
}

fn string_map_field(value: &Value, key: &str) -> std::collections::BTreeMap<String, String> {
    value
        .get(key)
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .filter_map(|(key, value)| {
                    value.as_str().map(|text| (key.clone(), text.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The app probes diff support with `GET /v1/diff/start`; the official daemon
/// answers 405 (it is a POST endpoint), so mirror that.
async fn diff_start_probe() -> Response {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        Json(json!({ "error": "method not allowed" })),
    )
        .into_response()
}

async fn diff_start() -> Response {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({ "error": "the diff viewer is not supported by moshi-lite" })),
    )
        .into_response()
}

/// The hooks/integrations sheet: moshi-lite installs no agent hooks, so the
/// list is genuinely empty.
async fn get_integrations() -> Json<Value> {
    Json(json!({ "integrations": [] }))
}

async fn post_integrations() -> Response {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({ "error": "moshi-lite does not install agent hooks" })),
    )
        .into_response()
}

async fn get_events(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
    Query(query): Query<EventsQuery>,
) -> Response {
    ws.on_upgrade(move |socket| events_session(socket, state, query))
}

async fn events_session(mut socket: WebSocket, state: Arc<AppState>, query: EventsQuery) {
    tracing::debug!(
        doctor = query.doctor.as_deref().unwrap_or(""),
        "events: client connected"
    );
    let doctor = doctor_frame(&state);
    let hello = json!({
        "gateway": GatewayFrame {
            version: env!("CARGO_PKG_VERSION").to_string(),
            protocol_version: PROTOCOL_VERSION,
            capabilities: capabilities(),
        },
        "servers": [],
        "simulators": [],
    });
    if send_json(&mut socket, &hello).await.is_err() {
        return;
    }
    if query.doctor.as_deref() == Some("refresh")
        && send_json(&mut socket, &json!({ "doctor": doctor }))
            .await
            .is_err()
    {
        return;
    }

    let mut trees = state.subscribe_tree();
    let mut watching_workspaces = false;
    let mut watching_context = false;
    let mut last_context: Option<String> = None;
    loop {
        tokio::select! {
            message = socket.recv() => {
                match message {
                    Some(Ok(Message::Text(text))) => {
                        tracing::debug!(frame = %text.as_str(), "events: client frame");
                        let Some((workspaces, context)) = parse_watch(text.as_str()) else {
                            continue;
                        };
                        watching_workspaces = workspaces;
                        watching_context = context;
                        let ack = json!({
                            "watching": {
                                "workspaces": workspaces,
                                "agent": false,
                                "context": context,
                                "usage": false,
                            },
                            "doctor": doctor.clone(),
                        });
                        if send_json(&mut socket, &ack).await.is_err() {
                            break;
                        }
                        if let Some(tree) = state.current_tree() {
                            if workspaces
                                && send_json(&mut socket, &json!({ "workspaces": tree.as_ref() }))
                                    .await
                                    .is_err()
                            {
                                break;
                            }
                            if let Some(frame) = context_frame(&tree) {
                                last_context = serde_json::to_string(&frame).ok();
                                if context && send_json(&mut socket, &frame).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(_)) => break,
                }
            }
            changed = trees.changed(), if watching_workspaces || watching_context => {
                if changed.is_err() {
                    break;
                }
                let tree = { trees.borrow_and_update().as_ref().cloned() };
                if let Some(tree) = tree {
                    if watching_workspaces
                        && send_json(&mut socket, &json!({ "workspaces": tree.as_ref() }))
                            .await
                            .is_err()
                    {
                        break;
                    }
                    if let Some(frame) = context_frame(&tree) {
                        let encoded = serde_json::to_string(&frame).ok();
                        if watching_context && encoded != last_context {
                            last_context = encoded;
                            if send_json(&mut socket, &frame).await.is_err() {
                                break;
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Parse a watch request, returning `(workspaces, context)`.
fn parse_watch(text: &str) -> Option<(bool, bool)> {
    let value: Value = serde_json::from_str(text).ok()?;
    let watch = value.get("watch")?;
    let workspaces = watch
        .get("workspaces")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let context = watch
        .get("context")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let wants_anything =
        watch.get("agent").is_some() || watch.get("usage").is_some() || workspaces || context;
    wants_anything.then_some((workspaces, context))
}

fn doctor_frame(state: &AppState) -> Value {
    let tmux = cli::detect_tool("tmux", "-V");
    let herdr = cli::detect_tool("herdr", "--version");
    cli::doctor_report(
        &state.hostname,
        Some(env!("CARGO_PKG_VERSION")),
        tmux.as_deref(),
        herdr.as_deref(),
    )
}

/// Build a context frame for the currently focused Herdr pane, if known.
fn context_frame(tree: &WorkspaceTree) -> Option<Value> {
    let (group, child, pane) = find_focused(tree)?;
    let mut context = json!({
        "kind": "herdr",
        "herdr": {
            "session": "default",
            "rawSession": "default",
            "paneId": pane.id,
            "copyMode": false,
            "scrollPosition": 0,
            "historySize": 0,
            "workspaceId": group.id,
            "tabId": child.id,
            "tab": child.label,
        },
    });
    if let Some(cwd) = &pane.cwd {
        context["cwd"] = json!(cwd);
    }
    Some(json!({ "context": context }))
}

fn find_focused(tree: &WorkspaceTree) -> Option<(&TreeGroup, &TreeChild, &TreePane)> {
    for group in &tree.groups {
        for child in &group.children {
            for pane in &child.panes {
                if pane.focused {
                    return Some((group, child, pane));
                }
            }
        }
    }
    None
}

async fn send_json<T: serde::Serialize>(
    socket: &mut WebSocket,
    value: &T,
) -> Result<(), axum::Error> {
    let text = serde_json::to_string(value).map_err(axum::Error::new)?;
    socket.send(Message::Text(text.into())).await
}
