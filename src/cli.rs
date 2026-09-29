//! App-facing CLI compatibility.
//!
//! The Moshi app runs `moshi-hook probe --json` and `moshi-hook doctor --json`
//! over SSH to decide whether the host is usable. These implementations answer
//! from our own gateway instead of the official daemon's Unix socket.

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Commands moshi-lite deliberately refuses to forward to the official binary:
/// they would install Moshi-owned agent hooks or pair with the cloud account.
const BLOCKED_FORWARD: &[&str] = &["install", "pair", "host"];

/// `moshi-hook probe --json`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProbeReport {
    pub installed: bool,
    pub running: bool,
    pub gateway: bool,
    pub version: String,
}

/// Probe the local gateway: `running`/`gateway` mean the gateway answered.
pub async fn probe(addr: std::net::SocketAddr) -> ProbeReport {
    match gateway_version(addr).await {
        Some(version) => ProbeReport {
            installed: true,
            running: true,
            gateway: true,
            version,
        },
        None => ProbeReport {
            installed: true,
            running: false,
            gateway: false,
            version: env!("CARGO_PKG_VERSION").to_string(),
        },
    }
}

/// Minimal loopback HTTP GET of `/v1/version`, returning the advertised
/// version if it really is our gateway answering.
pub async fn gateway_version(addr: std::net::SocketAddr) -> Option<String> {
    let exchange = async {
        let mut stream = TcpStream::connect(addr).await.ok()?;
        let request =
            format!("GET /v1/version HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).await.ok()?;

        let mut buffer = Vec::new();
        let mut chunk = [0_u8; 4096];
        loop {
            let read = stream.read(&mut chunk).await.ok()?;
            if read == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..read]);
            if buffer.len() > 64 * 1024 {
                return None;
            }
        }

        let response = String::from_utf8_lossy(&buffer);
        let (head, body) = response.split_once("\r\n\r\n")?;
        if !head.starts_with("HTTP/1.1 200") {
            return None;
        }
        let body = decode_chunked(head, body)?;
        let value: Value = serde_json::from_str(&body).ok()?;
        value.get("version")?.as_str().map(str::to_string)
    };

    tokio::time::timeout(Duration::from_millis(800), exchange)
        .await
        .ok()
        .flatten()
}

fn decode_chunked(head: &str, body: &str) -> Option<String> {
    if !head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        return Some(body.to_string());
    }
    let mut decoded = String::new();
    let mut rest = body;
    loop {
        let (size_line, remainder) = rest.split_once("\r\n")?;
        let size = usize::from_str_radix(size_line.trim(), 16).ok()?;
        if size == 0 {
            return Some(decoded);
        }
        if remainder.len() < size {
            return None;
        }
        decoded.push_str(&remainder[..size]);
        rest = remainder[size..].strip_prefix("\r\n")?;
    }
}

/// `moshi-hook doctor --json`. Pure so tests can pin the shape.
pub fn doctor_report(
    hostname: &str,
    gateway: Option<&str>,
    tmux: Option<&str>,
    herdr: Option<&str>,
) -> Value {
    let (daemon_status, daemon_detail) = match gateway {
        Some(version) => ("ok", format!("running (moshi-lite {version})")),
        None => ("fail", "not running".to_string()),
    };
    let (gateway_status, gateway_detail) = match gateway {
        Some(version) => ("ok", format!("moshi-lite {version}")),
        None => ("fail", "moshi-lite gateway is not answering".to_string()),
    };
    let (workspaces_status, workspaces_detail) = match gateway {
        Some(version) => ("ok", format!("herdr gateway ready (moshi-lite {version})")),
        None => ("fail", "moshi-lite gateway is not answering".to_string()),
    };
    let mux_check = |subject: &str, version: Option<&str>| match version {
        Some(version) => json!({
            "group": "Multiplexers", "subject": subject, "status": "ok", "detail": version,
        }),
        None => json!({
            "group": "Multiplexers", "subject": subject, "status": "fail", "detail": "not found",
        }),
    };
    let unsupported = |id: &str, name: &str, about: &str| {
        json!({
            "id": id, "name": name, "about": about,
            "status": "fail", "detail": "not supported by moshi-lite",
        })
    };

    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "hostname": hostname,
        "features": [
            unsupported("inbox", "Agent inbox & alerts", "notifications, Live Activity, Apple Watch, approvals"),
            unsupported("chat_view", "Chat View", "live agent transcript, replies, stop"),
            {
                "id": "workspaces",
                "name": "Workspaces & Jump To",
                "about": "sidebar of tmux windows / herdr tabs with agent status",
                "status": workspaces_status,
                "detail": workspaces_detail,
            },
            {
                "id": "sessions",
                "name": "Session picker",
                "about": "attach to running tmux / herdr sessions when connecting",
                "status": "ok",
                "detail": "attach to running tmux / herdr sessions when connecting",
            },
            unsupported("diff", "Diff viewer", "git changes in the agent's repository"),
            unsupported("preview", "Browser & Simulator preview", "open dev servers running on this host"),
            unsupported("usage", "Usage", "rate-limit rings per agent account"),
        ],
        "checks": [
            {"group": "Host", "subject": "daemon", "status": daemon_status, "detail": daemon_detail},
            {"group": "Host", "subject": "gateway", "status": gateway_status, "detail": gateway_detail},
            mux_check("tmux", tmux),
            mux_check("herdr", herdr),
            {"group": "Account", "subject": "pairing", "status": "fail",
             "detail": "moshi-lite does not pair with Moshi cloud services"},
        ],
        "fixes": [],
    })
}

/// First line of `binary <version-arg>`, if the binary exists and succeeds.
pub fn detect_tool(binary: &str, version_arg: &str) -> Option<String> {
    let output = Command::new(binary).arg(version_arg).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let first = stdout.lines().next()?.trim().to_string();
    (!first.is_empty()).then_some(first)
}

/// Path of the official binary kept as a compatibility fallback.
pub fn official_binary() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("MOSHI_LITE_OFFICIAL") {
        return Some(PathBuf::from(path));
    }
    let home = std::env::var_os("HOME")?;
    let path = PathBuf::from(home).join(".local/bin/moshi-hook.official");
    path.exists().then_some(path)
}

/// Whether an unknown invocation may be forwarded to the official binary.
pub fn forward_allowed(args: &[String]) -> bool {
    args.first()
        .is_none_or(|command| !BLOCKED_FORWARD.contains(&command.as_str()))
}

/// Run the official binary with the original arguments and exit with its status.
pub fn forward_to_official(args: &[String]) -> ! {
    let Some(official) = official_binary() else {
        eprintln!("moshi-lite: unsupported command {args:?} and no official fallback found");
        std::process::exit(2);
    };
    match Command::new(&official).args(args).status() {
        Ok(status) => std::process::exit(status.code().unwrap_or(1)),
        Err(error) => {
            eprintln!("moshi-lite: failed to run {}: {error}", official.display());
            std::process::exit(1);
        }
    }
}

/// Best-effort log of CLI invocations, for observing what the app asks for.
pub fn log_invocation() {
    let Some(home) = std::env::var_os("HOME") else {
        return;
    };
    let path = PathBuf::from(home).join(".local/state/moshi-lite/invocations.log");
    let Some(parent) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    let ssh = std::env::var("SSH_CONNECTION").unwrap_or_else(|_| "local".to_string());
    let line = format!("{timestamp}\tssh={ssh}\targv={}\n", args.join(" "));
    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = file.write_all(line.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;
    use crate::config::Config;
    use crate::gateway;
    use crate::herdr::Snapshot;
    use crate::herdr::mock::MockBackend;
    use crate::state::AppState;

    fn fixture() -> Snapshot {
        serde_json::from_str(include_str!("../tests/fixtures/herdr-snapshot.json"))
            .expect("fixture parses")
    }

    async fn spawn_gateway() -> std::net::SocketAddr {
        let config = Config::new(
            "127.0.0.1:0".parse().expect("addr"),
            "/nonexistent/herdr.sock".into(),
            Duration::from_millis(50),
        );
        let state = Arc::new(AppState::new(
            config,
            Arc::new(MockBackend::new(fixture())),
            "test-host".to_string(),
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let app = gateway::router(state);
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        addr
    }

    #[tokio::test]
    async fn probe_reports_running_when_gateway_answers() {
        let addr = spawn_gateway().await;
        let report = probe(addr).await;
        assert!(report.installed);
        assert!(report.running);
        assert!(report.gateway);
        assert_eq!(report.version, env!("CARGO_PKG_VERSION"));
    }

    #[tokio::test]
    async fn probe_reports_down_when_gateway_is_missing() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        drop(listener);

        let report = probe(addr).await;
        assert!(report.installed);
        assert!(!report.running);
        assert!(!report.gateway);
        assert_eq!(report.version, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn doctor_marks_workspaces_ok_when_gateway_answers() {
        let report = doctor_report(
            "arch",
            Some("0.1.0"),
            Some("tmux 3.7c"),
            Some("herdr 0.9.1"),
        );
        let features = report["features"].as_array().expect("features");
        let workspaces = features
            .iter()
            .find(|feature| feature["id"] == "workspaces")
            .expect("workspaces feature");
        assert_eq!(workspaces["status"], "ok");

        let checks = report["checks"].as_array().expect("checks");
        let daemon = checks
            .iter()
            .find(|check| check["subject"] == "daemon")
            .expect("daemon check");
        assert_eq!(daemon["status"], "ok");
        let inbox = features
            .iter()
            .find(|feature| feature["id"] == "inbox")
            .expect("inbox feature");
        assert_eq!(inbox["status"], "fail");
    }

    #[test]
    fn doctor_marks_workspaces_broken_without_gateway() {
        let report = doctor_report("arch", None, None, None);
        let features = report["features"].as_array().expect("features");
        let workspaces = features
            .iter()
            .find(|feature| feature["id"] == "workspaces")
            .expect("workspaces feature");
        assert_eq!(workspaces["status"], "fail");
    }

    #[test]
    fn chunked_body_is_decoded() {
        let head = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked";
        let body = "7\r\n{\"a\":1}\r\n0\r\n\r\n";
        assert_eq!(decode_chunked(head, body).as_deref(), Some("{\"a\":1}"));
    }

    #[test]
    fn forwarding_blocks_hook_installation() {
        assert!(forward_allowed(&["context".to_string()]));
        assert!(forward_allowed(&["cwd-list".to_string()]));
        assert!(!forward_allowed(&["install".to_string()]));
        assert!(!forward_allowed(&["pair".to_string()]));
    }
}
