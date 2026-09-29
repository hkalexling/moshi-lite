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

use super::{FocusTarget, HerdrBackend, HerdrError, Snapshot};

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
        listener: tokio::net::UnixListener,
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
            let request = respond(listener, move |id| {
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
            let request =
                respond(listener, |id| serde_json::json!({ "id": id, "result": {} })).await;
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
    async fn rpc_error_is_reported() {
        let path = temp_socket("rpc-error");
        let listener = tokio::net::UnixListener::bind(&path).expect("bind");
        let server = tokio::spawn(async move {
            respond(
                listener,
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
                listener,
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
