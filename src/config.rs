use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

/// Runtime configuration for the gateway.
#[derive(Debug, Clone)]
pub struct Config {
    /// Loopback listen address of the gateway (default `127.0.0.1:24543`).
    pub listen: SocketAddr,
    /// Path to the Herdr server's Unix socket.
    pub herdr_socket: PathBuf,
    /// How often the poller rebuilds the workspace tree.
    pub poll_interval: Duration,
    /// Timeout for a single Herdr API request.
    pub herdr_timeout: Duration,
}

impl Config {
    pub fn new(listen: SocketAddr, herdr_socket: PathBuf, poll_interval: Duration) -> Self {
        Self {
            listen,
            herdr_socket,
            poll_interval,
            herdr_timeout: Duration::from_secs(2),
        }
    }

    /// Resolve the Herdr socket path: `$HERDR_SOCKET_PATH`, else the
    /// documented per-user default.
    pub fn default_herdr_socket() -> PathBuf {
        if let Some(path) = std::env::var_os("HERDR_SOCKET_PATH") {
            return PathBuf::from(path);
        }
        let home = std::env::var_os("HOME").unwrap_or_default();
        PathBuf::from(home).join(".config/herdr/herdr.sock")
    }
}
