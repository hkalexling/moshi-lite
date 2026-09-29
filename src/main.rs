use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::{Args, CommandFactory, Parser, Subcommand};
use moshi_lite::config::Config;
use moshi_lite::gateway;
use moshi_lite::herdr::socket::SocketBackend;
use moshi_lite::state::AppState;
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(
    name = "moshi-lite",
    version,
    about = "Thin local Moshi gateway backed by Herdr"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the gateway.
    Serve(ServeArgs),
    /// Print the version.
    Version,
}

#[derive(Debug, Args)]
struct ServeArgs {
    /// Loopback address to listen on.
    #[arg(long, default_value = "127.0.0.1:24543")]
    listen: std::net::SocketAddr,
    /// Herdr socket path (defaults to $HERDR_SOCKET_PATH or ~/.config/herdr/herdr.sock).
    #[arg(long)]
    herdr_socket: Option<PathBuf>,
    /// Workspace-tree poll interval in milliseconds.
    #[arg(long, default_value_t = 1000)]
    poll_interval_ms: u64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    match cli.command {
        Some(Command::Serve(args)) => serve(args).await,
        Some(Command::Version) => {
            println!("moshi-lite {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        None => {
            Cli::command().print_help()?;
            println!();
            Ok(())
        }
    }
}

async fn serve(args: ServeArgs) -> anyhow::Result<()> {
    anyhow::ensure!(
        args.listen.ip().is_loopback(),
        "listen address must be loopback, got {}",
        args.listen
    );

    let herdr_socket = args
        .herdr_socket
        .unwrap_or_else(Config::default_herdr_socket);
    let config = Config::new(
        args.listen,
        herdr_socket,
        Duration::from_millis(args.poll_interval_ms),
    );
    let backend = Arc::new(SocketBackend::new(
        config.herdr_socket.clone(),
        config.herdr_timeout,
    ));
    let hostname = gethostname::gethostname().to_string_lossy().into_owned();
    let state = Arc::new(AppState::new(config.clone(), backend, hostname));

    state.spawn_poller();

    let app = gateway::router(Arc::clone(&state));
    let listener = TcpListener::bind(config.listen).await?;
    info!(
        listen = %config.listen,
        herdr_socket = %config.herdr_socket.display(),
        "moshi-lite gateway listening"
    );

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
