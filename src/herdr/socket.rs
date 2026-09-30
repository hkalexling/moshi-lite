//! Herdr socket client. One connection per request: write a single JSON-line
//! envelope, read a single response line.

use std::fmt::Display;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use super::{
    CreateRequest, CreateTarget, FocusTarget, HerdrBackend, HerdrError, NodeTarget, Snapshot,
};

pub struct SocketBackend {
    path: PathBuf,
    timeout: Duration,
    next_id: AtomicU64,
}

impl SocketBackend {
    pub fn new(path: impl Into<PathBuf>, timeout: Duration) -> Self {
        Self {
            path: path.into(),
            timeout,
            next_id: AtomicU64::new(1),
        }
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value, HerdrError> {
        let request = json!({
            "id": format!("moshi-lite-{}", self.next_id.fetch_add(1, Ordering::Relaxed)),
            "method": method,
            "params": params,
        });

        let exchange = async {
            let mut stream = UnixStream::connect(&self.path).await?;
            let mut bytes = serde_json::to_vec(&request).map_err(protocol)?;
            bytes.push(b'\n');
            stream.write_all(&bytes).await?;
            stream.flush().await?;

            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            let read = reader.read_line(&mut line).await?;
            if read == 0 {
                return Err(HerdrError::Protocol(
                    "herdr closed the connection before responding".to_string(),
                ));
            }
            let response: Value = serde_json::from_str(&line).map_err(protocol)?;
            if let Some(error) = response.get("error") {
                return Err(HerdrError::Rpc {
                    method: method.to_string(),
                    message: error.to_string(),
                });
            }
            Ok(response.get("result").cloned().unwrap_or(Value::Null))
        };

        match tokio::time::timeout(self.timeout, exchange).await {
            Ok(result) => result,
            Err(_) => Err(HerdrError::Timeout),
        }
    }
}

#[async_trait]
impl HerdrBackend for SocketBackend {
    async fn snapshot(&self) -> Result<Snapshot, HerdrError> {
        let result = self.call("session.snapshot", json!({})).await?;
        let snapshot = result.get("snapshot").ok_or(HerdrError::MissingSnapshot)?;
        serde_json::from_value(snapshot.clone()).map_err(protocol)
    }

    async fn focus(&self, target: FocusTarget) -> Result<(), HerdrError> {
        match target {
            FocusTarget::Workspace(id) => {
                self.call("workspace.focus", json!({ "workspace_id": id }))
                    .await?;
            }
            FocusTarget::Tab(id) => {
                self.call("tab.focus", json!({ "tab_id": id })).await?;
            }
            FocusTarget::Pane(id) => {
                self.call("pane.focus", json!({ "pane_id": id })).await?;
            }
            FocusTarget::Agent(target) => {
                self.call("agent.focus", json!({ "target": target }))
                    .await?;
            }
        }
        Ok(())
    }

    async fn rename(&self, target: NodeTarget, label: &str) -> Result<(), HerdrError> {
        match &target {
            NodeTarget::Workspace(id) => {
                self.call(
                    "workspace.rename",
                    json!({ "workspace_id": id, "label": label }),
                )
                .await?;
            }
            NodeTarget::Tab(id) => {
                self.call("tab.rename", json!({ "tab_id": id, "label": label }))
                    .await?;
            }
            NodeTarget::Pane(id) => {
                self.call("pane.rename", json!({ "pane_id": id, "label": label }))
                    .await?;
            }
        }
        Ok(())
    }

    async fn close(&self, target: NodeTarget) -> Result<(), HerdrError> {
        match &target {
            NodeTarget::Workspace(id) => {
                self.call("workspace.close", json!({ "workspace_id": id }))
                    .await?;
            }
            NodeTarget::Tab(id) => {
                self.call("tab.close", json!({ "tab_id": id })).await?;
            }
            NodeTarget::Pane(id) => {
                self.call("pane.close", json!({ "pane_id": id })).await?;
            }
        }
        Ok(())
    }

    async fn create(
        &self,
        target: CreateTarget,
        request: CreateRequest,
    ) -> Result<Value, HerdrError> {
        match target {
            CreateTarget::TabPane(tab_id) => {
                let pane_id = self.focused_pane_in_tab(&tab_id).await?;
                self.split_pane(&pane_id, &request).await
            }
            CreateTarget::Pane(pane_id) => self.split_pane(&pane_id, &request).await,
            CreateTarget::Workspace => {
                let params = create_params(json!({ "focus": true }), &request);
                self.call("workspace.create", params).await
            }
            CreateTarget::Tab(workspace_id) => {
                let params = create_params(
                    json!({ "workspace_id": workspace_id, "focus": true }),
                    &request,
                );
                self.call("tab.create", params).await
            }
        }
    }
}

/// Add the optional create fields to a `workspace.create`/`tab.create` params
/// object. Absent fields are omitted, matching the official daemon.
fn create_params(mut params: Value, request: &CreateRequest) -> Value {
    if let Some(cwd) = &request.cwd {
        params["cwd"] = json!(cwd);
    }
    if let Some(label) = &request.label {
        params["label"] = json!(label);
    }
    if !request.env.is_empty() {
        params["env"] = json!(request.env);
    }
    params
}

impl SocketBackend {
    async fn split_pane(
        &self,
        pane_id: &str,
        request: &CreateRequest,
    ) -> Result<Value, HerdrError> {
        let mut params = json!({
            "target_pane_id": pane_id,
            "direction": "down",
            "focus": true,
        });
        if let Some(cwd) = &request.cwd {
            params["cwd"] = json!(cwd);
        }
        if !request.env.is_empty() {
            params["env"] = json!(request.env);
        }
        self.call("pane.split", params).await
    }

    /// Resolve the tab's focused pane (else its first pane) via `pane.list`.
    async fn focused_pane_in_tab(&self, tab_id: &str) -> Result<String, HerdrError> {
        let result = self.call("pane.list", json!({})).await?;
        let list: PaneList = serde_json::from_value(result).map_err(protocol)?;
        let mut fallback: Option<String> = None;
        for pane in list.panes {
            if pane.tab_id == tab_id {
                if pane.focused {
                    return Ok(pane.pane_id);
                }
                fallback.get_or_insert(pane.pane_id);
            }
        }
        fallback
            .ok_or_else(|| HerdrError::Protocol(format!("no panes found in herdr tab {tab_id:?}")))
    }
}

#[derive(serde::Deserialize)]
struct PaneList {
    #[serde(default)]
    panes: Vec<PaneEntry>,
}

#[derive(serde::Deserialize)]
struct PaneEntry {
    pane_id: String,
    #[serde(default)]
    tab_id: String,
    #[serde(default)]
    focused: bool,
}

fn protocol(error: impl Display) -> HerdrError {
    HerdrError::Protocol(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    fn temp_socket(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("moshi-lite-test-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir.join("herdr.sock")
    }

    async fn respond(
        listener: &tokio::net::UnixListener,
        response: impl FnOnce(Value) -> Value + Send + 'static,
    ) -> Value {
        let (mut stream, _) = listener.accept().await.expect("accept");
        let mut reader = BufReader::new(&mut stream);
        let mut line = String::new();
        reader.read_line(&mut line).await.expect("read request");
        let request: Value = serde_json::from_str(&line).expect("request json");
        let response = response(request["id"].clone());
        let mut bytes = serde_json::to_vec(&response).expect("encode response");
        bytes.push(b'\n');
        stream.write_all(&bytes).await.expect("write response");
        request
    }

    #[tokio::test]
    async fn snapshot_round_trips_over_socket() {
        let path = temp_socket("snapshot");
        let listener = tokio::net::UnixListener::bind(&path).expect("bind");
        let fixture = include_str!("../../tests/fixtures/herdr-snapshot.json");
        let server = tokio::spawn(async move {
            let request = respond(&listener, move |id| {
                serde_json::json!({
                    "id": id,
                    "result": {
                        "type": "success",
                        "snapshot": serde_json::from_str::<Value>(fixture).expect("fixture"),
                    },
                })
            })
            .await;
            assert_eq!(request["method"], "session.snapshot");
            assert_eq!(request["params"], serde_json::json!({}));
        });

        let backend = SocketBackend::new(&path, Duration::from_secs(2));
        let snapshot = backend.snapshot().await.expect("snapshot");

        assert_eq!(snapshot.workspaces.len(), 2);
        assert_eq!(snapshot.protocol, Some(22));
        server.await.expect("server task");
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn focus_sends_the_method_matching_the_target() {
        let path = temp_socket("focus");
        let listener = tokio::net::UnixListener::bind(&path).expect("bind");
        let server = tokio::spawn(async move {
            let request = respond(
                &listener,
                |id| serde_json::json!({ "id": id, "result": {} }),
            )
            .await;
            assert_eq!(request["method"], "pane.focus");
            assert_eq!(request["params"]["pane_id"], "wA:p1");
        });

        let backend = SocketBackend::new(&path, Duration::from_secs(2));
        backend
            .focus(FocusTarget::Pane("wA:p1".to_string()))
            .await
            .expect("focus");
        server.await.expect("server task");
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn create_workspace_sends_workspace_create() {
        let path = temp_socket("create");
        let listener = tokio::net::UnixListener::bind(&path).expect("bind");
        let server = tokio::spawn(async move {
            let request = respond(&listener, |id| {
                serde_json::json!({
                    "id": id,
                    "result": {
                        "type": "workspace_created",
                        "workspace": { "workspace_id": "wNEW", "label": "scratch" },
                    },
                })
            })
            .await;
            assert_eq!(request["method"], "workspace.create");
            assert_eq!(request["params"]["cwd"], "/tmp");
            assert_eq!(request["params"]["label"], "scratch");
            assert_eq!(request["params"]["focus"], true);
            assert!(request["params"].get("env").is_none());
        });

        let backend = SocketBackend::new(&path, Duration::from_secs(2));
        let result = backend
            .create(
                CreateTarget::Workspace,
                CreateRequest {
                    cwd: Some("/tmp".to_string()),
                    label: Some("scratch".to_string()),
                    env: std::collections::BTreeMap::new(),
                },
            )
            .await
            .expect("create");
        assert_eq!(result["workspace"]["workspace_id"], "wNEW");
        server.await.expect("server task");
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn create_tab_sends_tab_create() {
        let path = temp_socket("create-tab");
        let listener = tokio::net::UnixListener::bind(&path).expect("bind");
        let server = tokio::spawn(async move {
            let request = respond(
                &listener,
                |id| serde_json::json!({ "id": id, "result": {} }),
            )
            .await;
            assert_eq!(request["method"], "tab.create");
            assert_eq!(request["params"]["workspace_id"], "wA");
            assert_eq!(request["params"]["focus"], true);
        });

        let backend = SocketBackend::new(&path, Duration::from_secs(2));
        backend
            .create(
                CreateTarget::Tab("wA".to_string()),
                CreateRequest::default(),
            )
            .await
            .expect("create tab");
        server.await.expect("server task");
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn create_splits_a_pane_downward() {
        let path = temp_socket("create-pane");
        let listener = tokio::net::UnixListener::bind(&path).expect("bind");
        let server = tokio::spawn(async move {
            let request = respond(
                &listener,
                |id| serde_json::json!({ "id": id, "result": {} }),
            )
            .await;
            assert_eq!(request["method"], "pane.split");
            assert_eq!(request["params"]["target_pane_id"], "wA:p1");
            assert_eq!(request["params"]["direction"], "down");
            assert_eq!(request["params"]["focus"], true);
        });

        let backend = SocketBackend::new(&path, Duration::from_secs(2));
        backend
            .create(
                CreateTarget::Pane("wA:p1".to_string()),
                CreateRequest::default(),
            )
            .await
            .expect("split pane");
        server.await.expect("server task");
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn create_from_tab_resolves_its_focused_pane() {
        let path = temp_socket("create-tab-pane");
        let listener = tokio::net::UnixListener::bind(&path).expect("bind");
        let server = tokio::spawn(async move {
            let list = respond(&listener, |id| {
                serde_json::json!({
                    "id": id,
                    "result": {
                        "type": "pane_list",
                        "panes": [
                            { "pane_id": "wA:p1", "tab_id": "wA:t1", "focused": false },
                            { "pane_id": "wA:p2", "tab_id": "wA:t1", "focused": true },
                            { "pane_id": "wA:p3", "tab_id": "wA:t2", "focused": false },
                        ],
                    },
                })
            })
            .await;
            assert_eq!(list["method"], "pane.list");

            let split = respond(
                &listener,
                |id| serde_json::json!({ "id": id, "result": {} }),
            )
            .await;
            assert_eq!(split["method"], "pane.split");
            assert_eq!(split["params"]["target_pane_id"], "wA:p2");
            assert_eq!(split["params"]["direction"], "down");
        });

        let backend = SocketBackend::new(&path, Duration::from_secs(2));
        backend
            .create(
                CreateTarget::TabPane("wA:t1".to_string()),
                CreateRequest::default(),
            )
            .await
            .expect("split tab pane");
        server.await.expect("server task");
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn create_from_tab_without_panes_is_an_error() {
        let path = temp_socket("create-tab-empty");
        let listener = tokio::net::UnixListener::bind(&path).expect("bind");
        let server = tokio::spawn(async move {
            respond(&listener, |id| {
                serde_json::json!({ "id": id, "result": { "type": "pane_list", "panes": [] } })
            })
            .await;
        });

        let backend = SocketBackend::new(&path, Duration::from_secs(2));
        let error = backend
            .create(
                CreateTarget::TabPane("wA:t9".to_string()),
                CreateRequest::default(),
            )
            .await
            .expect_err("no pane");
        assert!(error.to_string().contains("no panes found"));
        server.await.expect("server task");
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn rename_sends_the_method_matching_the_target() {
        let path = temp_socket("rename");
        let listener = tokio::net::UnixListener::bind(&path).expect("bind");
        let server = tokio::spawn(async move {
            let request = respond(
                &listener,
                |id| serde_json::json!({ "id": id, "result": {} }),
            )
            .await;
            assert_eq!(request["method"], "tab.rename");
            assert_eq!(request["params"]["tab_id"], "wA:t1");
            assert_eq!(request["params"]["label"], "renamed");
        });

        let backend = SocketBackend::new(&path, Duration::from_secs(2));
        backend
            .rename(NodeTarget::Tab("wA:t1".to_string()), "renamed")
            .await
            .expect("rename");
        server.await.expect("server task");
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn close_sends_the_method_matching_the_target() {
        let path = temp_socket("close");
        let listener = tokio::net::UnixListener::bind(&path).expect("bind");
        let server = tokio::spawn(async move {
            let request = respond(
                &listener,
                |id| serde_json::json!({ "id": id, "result": {} }),
            )
            .await;
            assert_eq!(request["method"], "pane.close");
            assert_eq!(request["params"]["pane_id"], "wA:p1");
        });

        let backend = SocketBackend::new(&path, Duration::from_secs(2));
        backend
            .close(NodeTarget::Pane("wA:p1".to_string()))
            .await
            .expect("close");
        server.await.expect("server task");
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn rpc_error_is_reported() {
        let path = temp_socket("rpc-error");
        let listener = tokio::net::UnixListener::bind(&path).expect("bind");
        let server = tokio::spawn(async move {
            respond(
                &listener,
                |id| serde_json::json!({ "id": id, "error": { "message": "boom" } }),
            )
            .await;
        });

        let backend = SocketBackend::new(&path, Duration::from_secs(2));
        let error = backend
            .focus(FocusTarget::Workspace("wA".to_string()))
            .await
            .expect_err("focus fails");
        assert!(
            matches!(error, HerdrError::Rpc { ref method, .. } if method == "workspace.focus"),
            "unexpected error: {error:?}"
        );
        server.await.expect("server task");
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn missing_snapshot_payload_is_reported() {
        let path = temp_socket("missing-snapshot");
        let listener = tokio::net::UnixListener::bind(&path).expect("bind");
        let server = tokio::spawn(async move {
            respond(
                &listener,
                |id| serde_json::json!({ "id": id, "result": { "type": "success" } }),
            )
            .await;
        });

        let backend = SocketBackend::new(&path, Duration::from_secs(2));
        let error = backend.snapshot().await.expect_err("snapshot fails");
        assert!(
            matches!(error, HerdrError::MissingSnapshot),
            "unexpected error: {error:?}"
        );
        server.await.expect("server task");
        let _ = std::fs::remove_file(&path);
    }
}
