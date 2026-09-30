//! Integration tests for the gateway surface, driven by a mock Herdr backend.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use futures_util::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use moshi_lite::herdr::mock::MockBackend;
use moshi_lite::herdr::{CreateTarget, FocusTarget, HerdrBackend, NodeTarget, Snapshot};
use moshi_lite::state::AppState;
use moshi_lite::{config::Config, gateway};
use serde_json::Value;
use tower::ServiceExt;

fn fixture_snapshot() -> Snapshot {
    serde_json::from_str(include_str!("fixtures/herdr-snapshot.json")).expect("fixture parses")
}

fn test_state(backend: Arc<dyn HerdrBackend>) -> Arc<AppState> {
    let config = Config::new(
        "127.0.0.1:0".parse().expect("listen addr"),
        "/nonexistent/herdr.sock".into(),
        Duration::from_millis(20),
    );
    Arc::new(AppState::new(config, backend, "test-host".to_string()))
}

async fn get_json(app: axum::Router, path: &str) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::builder()
                .uri(path)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

async fn post_json(app: axum::Router, path: &str, body: &str) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .expect("request"),
        )
        .await
        .expect("response");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

#[tokio::test]
async fn version_advertises_watch_capability() {
    let mock = Arc::new(MockBackend::new(fixture_snapshot()));
    let app = gateway::router(test_state(mock));

    let (status, body) = get_json(app, "/v1/version").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["protocolVersion"], 1);
    assert_eq!(body["hostname"], "test-host");
    let capabilities = body["capabilities"].as_array().expect("capabilities");
    assert!(
        capabilities
            .iter()
            .any(|capability| capability == "events.watch.workspaces")
    );
    assert!(
        capabilities
            .iter()
            .any(|capability| capability == "workspaces.live-session")
    );
    assert!(
        capabilities
            .iter()
            .any(|capability| capability == "events.doctor")
    );
}

#[tokio::test]
async fn muxes_list_herdr_when_running() {
    let mock = Arc::new(MockBackend::new(fixture_snapshot()));
    let state = test_state(mock);
    state.poll_once().await.expect("poll");
    let app = gateway::router(state);

    let (status, body) = get_json(app, "/v1/muxes").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["muxes"][0]["id"], "herdr:default");
    assert_eq!(body["muxes"][0]["kind"], "herdr");
    assert_eq!(body["muxes"][0]["running"], true);
    assert_eq!(body["muxes"][0]["active"], true);
}

#[tokio::test]
async fn workspaces_returns_herdr_tree() {
    let mock = Arc::new(MockBackend::new(fixture_snapshot()));
    let state = test_state(mock);
    state.poll_once().await.expect("poll");
    let app = gateway::router(state);

    let (status, body) = get_json(app, "/v1/workspaces").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["kind"], "herdr");
    assert_eq!(body["capabilities"]["paneList"], true);
    assert_eq!(body["capabilities"]["paneFocus"], "exact");
    assert_eq!(body["groups"][0]["id"], "wA");
    assert_eq!(body["groups"][0]["children"][0]["agent"], "claude");
    assert_eq!(
        body["groups"][0]["children"][0]["sessionId"],
        "sess-claude-1"
    );
    assert_eq!(
        body["groups"][1]["children"][0]["sessionId"],
        "/home/user/.pi/agent/sessions/x.jsonl"
    );
}

#[tokio::test]
async fn workspaces_without_herdr_is_unprocessable() {
    let mock = Arc::new(MockBackend::unavailable());
    let state = test_state(mock);
    let app = gateway::router(state);

    let (status, body) = get_json(app, "/v1/workspaces").await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body["error"].is_string());
}

#[tokio::test]
async fn workspaces_rejects_non_herdr_mux() {
    let mock = Arc::new(MockBackend::new(fixture_snapshot()));
    let state = test_state(mock);
    state.poll_once().await.expect("poll");
    let app = gateway::router(state);

    let (status, _) = get_json(app, "/v1/workspaces?mux=tmux").await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn workspace_panes_echoes_target() {
    let mock = Arc::new(MockBackend::new(fixture_snapshot()));
    let state = test_state(mock);
    state.poll_once().await.expect("poll");
    let app = gateway::router(state);

    let (status, body) = get_json(app, "/v1/workspaces/panes?groupId=wB&childId=wB:t1").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["groupId"], "wB");
    assert_eq!(body["childId"], "wB:t1");
    assert_eq!(body["panes"][0]["id"], "wB:p1");
}

#[tokio::test]
async fn workspace_create_posts_a_workspace_to_the_backend() {
    let mock = Arc::new(MockBackend::new(fixture_snapshot()));
    let state = test_state(mock.clone());
    state.poll_once().await.expect("poll");
    let app = gateway::router(state);

    let (status, body) = post_json(
        app,
        "/v1/workspaces/create?session=ssh&sshConnection=1+2+3+4",
        r#"{"cwd":"/tmp","label":"test"}"#,
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);
    assert_eq!(body["kind"], "herdr");
    assert_eq!(body["scope"], "workspace");
    assert_eq!(body["workspace"]["workspace_id"], "wNEW");

    let calls = mock.create_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, CreateTarget::Workspace);
    assert_eq!(calls[0].1.cwd.as_deref(), Some("/tmp"));
    assert_eq!(calls[0].1.label.as_deref(), Some("test"));
}

#[tokio::test]
async fn workspace_create_with_a_workspace_id_creates_a_tab() {
    let mock = Arc::new(MockBackend::new(fixture_snapshot()));
    let state = test_state(mock.clone());
    let app = gateway::router(state);

    let (status, body) = post_json(app, "/v1/workspaces/create", r#"{"workspaceId":"wB"}"#).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["scope"], "tab");
    assert_eq!(body["tab"]["tab_id"], "wB:tNEW");

    let calls = mock.create_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, CreateTarget::Tab("wB".to_string()));
    assert_eq!(calls[0].1.cwd, None);
}

#[tokio::test]
async fn workspace_create_with_a_tab_id_creates_a_pane() {
    let mock = Arc::new(MockBackend::new(fixture_snapshot()));
    let state = test_state(mock.clone());
    let app = gateway::router(state);

    let (status, body) = post_json(app, "/v1/workspaces/create", r#"{"tabId":"wB:t1"}"#).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["scope"], "pane");

    let calls = mock.create_calls();
    assert_eq!(calls[0].0, CreateTarget::TabPane("wB:t1".to_string()));
}

#[tokio::test]
async fn workspace_create_prefers_the_most_specific_target() {
    let mock = Arc::new(MockBackend::new(fixture_snapshot()));
    let state = test_state(mock.clone());
    let app = gateway::router(state);

    let (status, body) = post_json(
        app,
        "/v1/workspaces/create",
        r#"{"workspaceId":"wB","tabId":"wB:t1","paneId":"wB:p1","cwd":"/tmp"}"#,
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["scope"], "pane");

    let calls = mock.create_calls();
    assert_eq!(calls[0].0, CreateTarget::Pane("wB:p1".to_string()));
}

#[tokio::test]
async fn workspace_create_requires_a_target_or_cwd() {
    let mock = Arc::new(MockBackend::new(fixture_snapshot()));
    let app = gateway::router(test_state(mock.clone()));

    let (status, _) = post_json(app.clone(), "/v1/workspaces/create", "{}").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = post_json(app, "/v1/workspaces/create", r#"{"label":"x"}"#).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(mock.create_calls().is_empty());
}

#[tokio::test]
async fn focus_posts_selected_target() {
    let mock = Arc::new(MockBackend::new(fixture_snapshot()));
    let state = test_state(mock.clone());
    state.poll_once().await.expect("poll");
    let app = gateway::router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/workspaces/focus")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"paneId":"wA:p1"}"#))
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        mock.focus_calls(),
        vec![FocusTarget::Pane("wA:p1".to_string())]
    );
}

#[tokio::test]
async fn focus_requires_a_target() {
    let mock = Arc::new(MockBackend::new(fixture_snapshot()));
    let state = test_state(mock);
    let app = gateway::router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/workspaces/focus")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn workspace_rename_targets_the_app_tab() {
    let mock = Arc::new(MockBackend::new(fixture_snapshot()));
    let state = test_state(mock.clone());
    let app = gateway::router(state);

    let (status, body) = post_json(
        app,
        "/v1/workspaces/rename?session=ssh&sshConnection=1+2+3+4",
        r#"{"tabId":"wB:t1","label":"Rename"}"#,
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);
    assert_eq!(body["kind"], "herdr");
    assert_eq!(body["renamed"]["tabId"], "wB:t1");
    assert_eq!(body["renamed"]["label"], "Rename");
    assert_eq!(
        mock.rename_calls(),
        vec![(NodeTarget::Tab("wB:t1".to_string()), "Rename".to_string())]
    );
}

#[tokio::test]
async fn workspace_rename_prefers_the_most_specific_target() {
    let mock = Arc::new(MockBackend::new(fixture_snapshot()));
    let state = test_state(mock.clone());
    let app = gateway::router(state);

    let (status, body) = post_json(
        app,
        "/v1/workspaces/rename",
        r#"{"workspaceId":"wB","tabId":"wB:t1","paneId":"wB:p1","label":"Triple"}"#,
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["renamed"]["paneId"], "wB:p1");
    assert_eq!(
        mock.rename_calls(),
        vec![(NodeTarget::Pane("wB:p1".to_string()), "Triple".to_string())]
    );
}

#[tokio::test]
async fn workspace_rename_requires_a_label_and_target() {
    let mock = Arc::new(MockBackend::new(fixture_snapshot()));
    let app = gateway::router(test_state(mock.clone()));

    let (status, _) = post_json(app.clone(), "/v1/workspaces/rename", r#"{"tabId":"wB:t1"}"#).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = post_json(app, "/v1/workspaces/rename", r#"{"label":"x"}"#).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(mock.rename_calls().is_empty());
}

#[tokio::test]
async fn workspace_close_targets_the_app_tab() {
    let mock = Arc::new(MockBackend::new(fixture_snapshot()));
    let state = test_state(mock.clone());
    let app = gateway::router(state);

    let (status, body) = post_json(
        app,
        "/v1/workspaces/close?session=ssh&sshConnection=1+2+3+4",
        r#"{"tabId":"wB:t1"}"#,
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);
    assert_eq!(body["kind"], "herdr");
    assert_eq!(body["closed"]["tabId"], "wB:t1");
    assert_eq!(
        mock.close_calls(),
        vec![NodeTarget::Tab("wB:t1".to_string())]
    );
}

#[tokio::test]
async fn workspace_close_requires_a_target() {
    let mock = Arc::new(MockBackend::new(fixture_snapshot()));
    let app = gateway::router(test_state(mock.clone()));

    let (status, _) = post_json(app, "/v1/workspaces/close", "{}").await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(mock.close_calls().is_empty());
}

#[tokio::test]
async fn diff_start_probe_answers_405() {
    let mock = Arc::new(MockBackend::new(fixture_snapshot()));
    let app = gateway::router(test_state(mock));

    let (status, body) = get_json(app, "/v1/diff/start").await;

    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(body["error"], "method not allowed");
}

#[tokio::test]
async fn events_watch_sends_context_frame() {
    let mock = Arc::new(MockBackend::new(fixture_snapshot()));
    let state = test_state(mock);
    state.poll_once().await.expect("poll");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let app = gateway::router(Arc::clone(&state));
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });

    let (mut socket, _) =
        tokio_tungstenite::connect_async(format!("ws://{addr}/events?doctor=refresh"))
            .await
            .expect("connect");
    let hello = next_json(&mut socket).await;
    assert!(hello.get("gateway").is_some());
    assert!(hello.get("servers").is_some());
    let doctor = next_json(&mut socket).await;
    assert!(doctor.get("doctor").is_some());

    socket
        .send(tokio_tungstenite::tungstenite::Message::Text(
            r#"{"watch":{"workspaces":true,"context":true}}"#.into(),
        ))
        .await
        .expect("watch request");

    let ack = next_json(&mut socket).await;
    assert_eq!(ack["watching"]["workspaces"], true);
    assert_eq!(ack["watching"]["context"], true);
    assert!(ack.get("doctor").is_some());

    let mut saw_workspaces = false;
    let mut saw_context = false;
    for _ in 0..4 {
        let frame = next_json(&mut socket).await;
        if frame.get("workspaces").is_some() {
            saw_workspaces = true;
        }
        if frame.get("context").is_some() {
            saw_context = true;
            assert_eq!(frame["context"]["herdr"]["paneId"], "wA:p1");
        }
        if saw_workspaces && saw_context {
            break;
        }
    }
    assert!(saw_workspaces, "missing workspaces frame");
    assert!(saw_context, "missing context frame");
    server.abort();
}

#[tokio::test]
async fn integrations_returns_an_empty_list() {
    let mock = Arc::new(MockBackend::new(fixture_snapshot()));
    let app = gateway::router(test_state(mock));

    let (status, body) = get_json(app, "/v1/integrations").await;

    assert_eq!(status, StatusCode::OK);
    assert!(
        body["integrations"]
            .as_array()
            .expect("integrations array")
            .is_empty()
    );
}

#[tokio::test]
async fn integrations_install_is_not_implemented() {
    let mock = Arc::new(MockBackend::new(fixture_snapshot()));
    let app = gateway::router(test_state(mock));

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/integrations")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
}

#[tokio::test]
async fn events_watch_streams_tree_updates() {
    let mock = Arc::new(MockBackend::new(fixture_snapshot()));
    let state = test_state(mock.clone());
    state.spawn_poller();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let app = gateway::router(Arc::clone(&state));
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });

    let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/events"))
        .await
        .expect("connect");

    let hello = next_json(&mut socket).await;
    assert!(hello.get("gateway").is_some());

    socket
        .send(tokio_tungstenite::tungstenite::Message::Text(
            r#"{"watch":{"workspaces":true}}"#.into(),
        ))
        .await
        .expect("watch request");

    let ack = next_json(&mut socket).await;
    assert_eq!(ack["watching"]["workspaces"], true);

    let first = next_json(&mut socket).await;
    assert!(first.get("workspaces").is_some());

    let mut changed = fixture_snapshot();
    changed.workspaces[0].label = Some("renamed".to_string());
    mock.set_snapshot(changed);

    let mut saw_change = false;
    for _ in 0..5 {
        let frame = next_json(&mut socket).await;
        if frame["workspaces"]["groups"][0]["label"] == "renamed" {
            saw_change = true;
            break;
        }
    }
    assert!(saw_change, "did not receive the updated workspace frame");

    server.abort();
}

async fn next_json<S>(socket: &mut S) -> Value
where
    S: futures_util::Stream<
            Item = Result<
                tokio_tungstenite::tungstenite::Message,
                tokio_tungstenite::tungstenite::Error,
            >,
        > + Unpin,
{
    let message = tokio::time::timeout(Duration::from_secs(3), socket.next())
        .await
        .expect("websocket timeout")
        .expect("websocket closed")
        .expect("websocket error");
    serde_json::from_str(message.to_text().expect("text frame")).expect("json frame")
}
