use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::{Args, CommandFactory, Parser, Subcommand};
use moshi_lite::cli;
use moshi_lite::config::Config;
use moshi_lite::gateway;
use moshi_lite::herdr::socket::SocketBackend;
use moshi_lite::state::AppState;
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(
    name = "moshi-hook",
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
    /// Unknown commands fall back to the official binary when possible.
    #[command(external_subcommand)]
    External(Vec<String>),
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
    cli::log_invocation();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();

    // The Moshi app runs these two over SSH; handle them before clap so extra
    // flags can never send a health probe to the official binary by accident.
    match args.first().map(String::as_str) {
        Some("probe") => {
            run_probe(&args).await;
            return Ok(());
        }
        Some("doctor") => {
            run_doctor(&args).await;
            return Ok(());
        }
        Some("cwd-list") => {
            run_cwd_list(&args);
            return Ok(());
        }
        _ => {}
    }

    let cli = Cli::parse_from(std::env::args());
    match cli.command {
        Some(Command::Serve(args)) => serve(args).await,
        Some(Command::Version) => {
            println!("moshi-lite {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some(Command::External(args)) => {
            if cli::forward_allowed(&args) {
                cli::forward_to_official(&args)
            } else {
                eprintln!(
                    "moshi-lite: refusing to run {args:?}: agent hook installation and cloud pairing are disabled"
                );
                std::process::exit(2);
            }
        }
        None => {
            Cli::command().print_help()?;
            println!();
            Ok(())
        }
    }
}

async fn run_probe(args: &[String]) {
    let report = cli::probe(listen_arg(args)).await;
    if json_flag(args) {
        match serde_json::to_string(&report) {
            Ok(text) => println!("{text}"),
            Err(error) => {
                eprintln!("moshi-lite: encode probe report: {error}");
                std::process::exit(1);
            }
        }
    } else {
        println!("installed: {}", report.installed);
        println!("running:   {}", report.running);
        println!("gateway:   {}", report.gateway);
        println!("version:   {}", report.version);
    }
}

async fn run_doctor(args: &[String]) {
    let listen = listen_arg(args);
    let gateway = cli::gateway_version(listen).await;
    let tmux = cli::detect_tool("tmux", "-V");
    let herdr = cli::detect_tool("herdr", "--version");
    let hostname = gethostname::gethostname().to_string_lossy().into_owned();
    let report = cli::doctor_report(
        &hostname,
        gateway.as_deref(),
        tmux.as_deref(),
        herdr.as_deref(),
    );

    if json_flag(args) {
        println!("{}", serde_json::to_string(&report).unwrap_or_default());
    } else {
        if let Some(features) = report["features"].as_array() {
            for feature in features {
                println!(
                    "{:>5}  {:24}  {}",
                    feature["status"].as_str().unwrap_or("?"),
                    feature["name"].as_str().unwrap_or(""),
                    feature["detail"].as_str().unwrap_or("")
                );
            }
        }
    }
}

fn run_cwd_list(args: &[String]) {
    let limit = limit_arg(args).filter(|limit| *limit > 0).unwrap_or(10);
    let entries = cli::cwd_list(limit);
    if json_flag(args) {
        match serde_json::to_string(&entries) {
            Ok(text) => println!("{text}"),
            Err(error) => {
                eprintln!("moshi-lite: encode cwd-list: {error}");
                std::process::exit(1);
            }
        }
    } else {
        cli::print_cwd_list(&entries);
    }
}

fn limit_arg(args: &[String]) -> Option<usize> {
    let mut iterator = args.iter();
    while let Some(arg) = iterator.next() {
        if arg == "--limit" {
            return iterator.next().and_then(|value| value.parse().ok());
        }
        if let Some(value) = arg.strip_prefix("--limit=") {
            return value.parse().ok();
        }
    }
    None
}

fn json_flag(args: &[String]) -> bool {
    args.iter().any(|arg| arg == "--json")
}

fn listen_arg(args: &[String]) -> std::net::SocketAddr {
    let mut iterator = args.iter();
    while let Some(arg) = iterator.next() {
        if arg == "--listen"
            && let Some(value) = iterator.next()
            && let Ok(addr) = value.parse()
        {
            return addr;
        }
    }
    "127.0.0.1:24543"
        .parse()
        .expect("default listen address is valid")
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
