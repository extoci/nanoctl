#[cfg_attr(not(feature = "media"), allow(dead_code))]
mod adaptive;
#[cfg(feature = "media")]
mod audio;
#[cfg(feature = "media")]
mod clipboard;
#[cfg(feature = "rtc")]
mod codec;
mod config;
#[cfg(feature = "media")]
mod gamepad;
#[cfg(feature = "media")]
mod input;
#[cfg(all(feature = "media", target_os = "linux"))]
mod linux_encoder;
#[cfg(all(feature = "media", target_os = "macos"))]
mod macos_encoder;
#[cfg(feature = "media")]
mod media;
#[cfg(all(feature = "media", target_os = "linux"))]
mod media_bench;
#[cfg(any(all(feature = "media", target_os = "windows"), test))]
mod nv12;
#[cfg(all(feature = "media", target_os = "linux"))]
mod nvenc_encoder;
mod platform;
#[cfg(feature = "rtc")]
mod rtc;
mod server;
mod service;
mod service_control;
#[cfg(any(windows, test))]
mod supervisor;
mod tailnet;
mod terminal;
mod tls;
mod update;
#[cfg(all(feature = "media", target_os = "windows"))]
mod windows_encoder;

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{ColorChoice, Parser, Subcommand};
use tracing_subscriber::{
    EnvFilter, Layer,
    filter::{LevelFilter, Targets},
    layer::SubscriberExt,
    util::SubscriberInitExt,
};

use crate::config::AgentConfig;

#[derive(Debug, Parser)]
#[command(
    name = "nanoctl",
    version,
    about = "Headless nanoctl remote desktop agent",
    color = if terminal::supports_styling() { ColorChoice::Auto } else { ColorChoice::Never }
)]
struct Cli {
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// Write this marker after the service has initialized and remove it on exit.
    #[arg(long, global = true)]
    ready_file: Option<PathBuf>,

    /// Bind the readiness marker to one installer transaction.
    #[arg(long, global = true)]
    ready_token: Option<String>,

    /// Append service diagnostics to this file instead of the process console.
    #[arg(long, global = true)]
    log_file: Option<PathBuf>,

    #[cfg(windows)]
    #[arg(long, hide = true)]
    supervised_worker: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Measure the real video pipeline over an in-process loopback WebRTC connection.
    #[cfg(all(feature = "media", target_os = "linux"))]
    #[command(hide = true)]
    MediaBench {
        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..=3600))]
        seconds: u64,
        #[arg(long)]
        json: bool,
    },
    /// Start the agent in this terminal, or show the viewer URL if it is already running.
    Run {
        /// Show detailed diagnostics instead of the short run status.
        #[arg(short, long)]
        verbose: bool,
    },
    /// Stop the running agent gracefully.
    Stop,
    /// Check configuration, Tailscale, capture, and input readiness.
    Doctor {
        #[arg(long)]
        json: bool,
    },
    /// Capture and encode real frames for a bounded native media acceptance run.
    #[cfg(feature = "media")]
    MediaSmoke {
        /// Duration of the run in seconds.
        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..=3600))]
        seconds: u64,
        /// Fail instead of falling back to the software encoder.
        #[arg(long)]
        require_hardware: bool,
        /// Print the acceptance record as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Print the effective redacted configuration.
    Config,
    /// Print the configuration path used by this invocation.
    Paths,
    /// Print this host's preferred tailnet viewer URL.
    Url {
        /// Wait briefly for the running agent to open its tailnet listener.
        #[arg(long)]
        wait: bool,
    },
    /// Verify a signed update manifest and print the artifact for this executable.
    VerifyUpdate {
        manifest: PathBuf,
        /// Base64-encoded 32-byte Ed25519 public key.
        #[arg(long)]
        public_key: String,
    },
    /// Verify and download an update into a private staging directory.
    StageUpdate {
        manifest: PathBuf,
        /// Base64-encoded 32-byte Ed25519 public key.
        #[arg(long)]
        public_key: String,
        /// Print a machine-readable staging result.
        #[arg(long)]
        json: bool,
    },
    /// Atomically activate a verified staged update (Unix; stop the service first).
    ActivateUpdate {
        manifest: PathBuf,
        /// Base64-encoded 32-byte Ed25519 public key.
        #[arg(long)]
        public_key: String,
    },
    /// Restore the binary retained by the last update activation.
    RollbackUpdate,
    /// Commit a healthy update and delete retained rollback binaries.
    CommitUpdate,
}

fn quiet_console<W>(writer: W) -> impl tracing::Subscriber
where
    W: for<'a> tracing_subscriber::fmt::MakeWriter<'a> + Send + Sync + 'static,
{
    tracing_subscriber::registry().with(
        tracing_subscriber::fmt::layer()
            .with_writer(writer)
            .with_ansi(false)
            .without_time()
            .with_level(false)
            .with_target(false)
            .compact()
            .with_filter(
                Targets::new()
                    .with_target("nanoctl", LevelFilter::ERROR)
                    .with_target("nanoctl::status", LevelFilter::INFO),
            ),
    )
}

fn init_logging(log_file: Option<&Path>, command: &Command) -> Result<()> {
    let env_filter = EnvFilter::try_from_default_env().ok();
    if log_file.is_none()
        && env_filter.is_none()
        && matches!(command, Command::Run { verbose: false })
    {
        quiet_console(std::io::stdout).init();
        return Ok(());
    }
    let filter = env_filter.unwrap_or_else(|| EnvFilter::new("nanoctl=info"));
    if let Some(path) = log_file {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("cannot create log directory {}", parent.display()))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("cannot open log file {}", path.display()))?;
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_ansi(false)
            .with_writer(file)
            .compact()
            .init();
    } else {
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_ansi(terminal::supports_styling())
            .compact();
        if matches!(command, Command::Url { .. }) {
            subscriber.with_writer(std::io::stderr).init();
        } else {
            subscriber.init();
        }
    }
    Ok(())
}

async fn print_viewer_url(config: &config::TailnetConfig, wait: bool) -> Result<()> {
    let node = tailnet::real(config)
        .status()
        .await
        .context("Start Tailscale on this PC, sign in, then run nanoctl url again")?;
    if wait {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let mut listening = false;
            for ip in &node.ips {
                let connect = tokio::net::TcpStream::connect((*ip, config.port));
                if matches!(
                    tokio::time::timeout(Duration::from_millis(250), connect).await,
                    Ok(Ok(_))
                ) {
                    listening = true;
                    break;
                }
            }
            if listening {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!(
                    "Viewer is not listening yet. If nanoctl is starting, retry nanoctl url --wait shortly"
                );
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
    let tls = tls::inspect(config, &node);
    if tls.config.is_none() && config.tls == config::TlsMode::Auto {
        eprintln!("{}", tls.detail);
    }
    let url = tls::viewer_url(&node, config.port, tls.config.is_some())
        .context("Tailscale has no viewer address")?;
    println!("{url}");
    Ok(())
}

async fn run(cli: Cli) -> Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| anyhow::anyhow!("cannot install TLS provider"))?;
    let config_path = cli.config.unwrap_or(AgentConfig::default_path()?);

    match cli.command {
        Command::Run { .. } => {
            let config = AgentConfig::load_or_default(&config_path)?;
            config.validate()?;
            #[cfg(windows)]
            if cli.supervised_worker {
                return service::run_worker(config).await;
            }
            match service_control::ServiceControl::acquire()? {
                service_control::StartOutcome::Acquired(control) => {
                    tracing::info!(target: "nanoctl::status", "nanoctl {} starting", env!("CARGO_PKG_VERSION"));
                    #[cfg(windows)]
                    if cli.ready_file.is_none() && cli.ready_token.is_none() {
                        return supervisor::run(control).await;
                    }
                    service::run(config, control, cli.ready_file, cli.ready_token).await?;
                }
                service_control::StartOutcome::AlreadyRunning => {
                    if cli.ready_file.is_some() {
                        anyhow::bail!("the nanoctl background agent is already running");
                    }
                    println!("nanoctl is already running.");
                    if let Err(error) = print_viewer_url(&config.tailnet, true).await {
                        eprintln!("{error:#}");
                    }
                }
                service_control::StartOutcome::Stopping => {
                    println!("nanoctl is stopping. Retry nanoctl run when it has stopped.");
                }
            }
        }
        Command::Stop => match service_control::request_stop().await? {
            service_control::StopOutcome::Stopped => {
                println!("nanoctl background agent stopped.")
            }
            service_control::StopOutcome::AlreadyStopped => {
                println!("nanoctl background agent is already stopped.")
            }
        },
        Command::Doctor { json } => {
            let config = AgentConfig::load_or_default(&config_path)?;
            let report = platform::doctor(&config).await;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                report.print();
            }
            if !report.ready {
                std::process::exit(2);
            }
        }
        #[cfg(feature = "media")]
        Command::MediaSmoke {
            seconds,
            require_hardware,
            json,
        } => {
            let mut config = AgentConfig::load_or_default(&config_path)?;
            config.validate()?;
            if require_hardware {
                config.quality.encoder = crate::config::EncoderPreference::Hardware;
            }
            let report = crate::media::run_smoke(&config.quality, seconds)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                report.print();
            }
            if !report.passed {
                std::process::exit(2);
            }
        }
        #[cfg(all(feature = "media", target_os = "linux"))]
        Command::MediaBench { seconds, json } => {
            let report = media_bench::run(seconds).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!("{report}");
            }
        }
        Command::Config => {
            let config = AgentConfig::load_or_default(&config_path)?;
            println!(
                "# tailnet.serve_loopback: only enable on single-user machines when fronted by tailscale serve"
            );
            println!("{}", toml::to_string_pretty(&config)?);
        }
        Command::Paths => {
            println!("config={}", config_path.display());
        }
        Command::Url { wait } => {
            let config = AgentConfig::load_or_default(&config_path)?;
            print_viewer_url(&config.tailnet, wait).await?;
        }
        Command::VerifyUpdate {
            manifest,
            public_key,
        } => {
            let bytes = std::fs::read(&manifest)
                .with_context(|| format!("cannot read {}", manifest.display()))?;
            let artifact = update::verify_for_current_target(
                &bytes,
                &public_key,
                env!("CARGO_PKG_VERSION"),
                update::unix_time_now()?,
            )?;
            println!("{}", serde_json::to_string_pretty(&artifact)?);
        }
        Command::StageUpdate {
            manifest,
            public_key,
            json,
        } => {
            let bytes = std::fs::read(&manifest)
                .with_context(|| format!("cannot read {}", manifest.display()))?;
            let artifact = update::verify_for_current_target(
                &bytes,
                &public_key,
                env!("CARGO_PKG_VERSION"),
                update::unix_time_now()?,
            )?;
            let client = reqwest::Client::builder()
                .https_only(true)
                .redirect(reqwest::redirect::Policy::limited(3))
                .build()
                .context("cannot initialize update client")?;
            let path =
                update::stage_artifact(&client, &artifact, &update::default_staging_directory()?)
                    .await?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&serde_json::json!({
                        "path": path,
                        "artifact": artifact,
                    }))?
                );
            } else {
                println!("Verified update staged at {}", path.display());
            }
        }
        Command::ActivateUpdate {
            manifest,
            public_key,
        } => {
            let bytes = std::fs::read(&manifest)
                .with_context(|| format!("cannot read {}", manifest.display()))?;
            let artifact = update::verify_for_current_target(
                &bytes,
                &public_key,
                env!("CARGO_PKG_VERSION"),
                update::unix_time_now()?,
            )?;
            let installed = update::activate_staged(&artifact)?;
            println!(
                "Activated {}. Restart the nanoctl service and run `nanoctl doctor`.",
                installed.display()
            );
        }
        Command::RollbackUpdate => {
            let installed = update::rollback_update()?;
            println!(
                "Restored {}. Restart the nanoctl service and run `nanoctl doctor`.",
                installed.display()
            );
        }
        Command::CommitUpdate => {
            update::commit_update()?;
            println!("Update committed; retained rollback binaries were removed.");
        }
    }
    Ok(())
}

fn append_diagnostic(log_file: Option<&Path>, message: std::fmt::Arguments<'_>) {
    if let Some(path) = log_file
        && let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path)
    {
        let _ = writeln!(file, "{message}");
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let log_file = cli.log_file.clone();
    if let Some(path) = log_file.clone() {
        let previous_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |panic| {
            append_diagnostic(Some(&path), format_args!("{panic}"));
            previous_hook(panic);
        }));
    }
    if let Err(error) = init_logging(log_file.as_deref(), &cli.command) {
        append_diagnostic(
            log_file.as_deref(),
            format_args!("nanoctl command failed: {error:#}"),
        );
        return Err(error);
    }
    let result = run(cli).await;
    if let Err(error) = &result {
        append_diagnostic(
            log_file.as_deref(),
            format_args!("nanoctl command failed: {error:#}"),
        );
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn run_diagnostics_are_opt_in() {
        for (args, verbose) in [
            (vec!["nanoctl", "run"], false),
            (vec!["nanoctl", "run", "-v"], true),
            (vec!["nanoctl", "run", "--verbose"], true),
        ] {
            let cli = Cli::try_parse_from(args).unwrap();
            assert!(matches!(cli.command, Command::Run { verbose: value } if value == verbose));
        }
    }

    #[test]
    fn quiet_console_prints_only_plain_status_and_agent_errors() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let subscriber = quiet_console({
            let output = Arc::clone(&output);
            move || SharedOutput(Arc::clone(&output))
        });
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "nanoctl::rtc", "video REMB");
            tracing::warn!(target: "nanoctl::media", "frame discarded");
            tracing::debug!(target: "nanoctl::status", "debug details");
            tracing::info!(target: "nanoctl::status", "Viewer connected");
            tracing::warn!(target: "nanoctl::status", "Waiting for Tailscale");
            tracing::error!(target: "nanoctl::service", "Agent failed");
        });
        assert_eq!(
            *output.lock().unwrap(),
            b"Viewer connected\nWaiting for Tailscale\nAgent failed\n"
        );
    }

    struct SharedOutput(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedOutput {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().write(bytes)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
}
