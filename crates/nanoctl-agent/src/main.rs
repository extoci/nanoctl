mod config;
mod control_plane;
mod credential;
#[cfg(feature = "media")]
mod input;
#[cfg(all(feature = "media", target_os = "linux"))]
mod linux_encoder;
#[cfg(all(feature = "media", target_os = "macos"))]
mod macos_encoder;
#[cfg(feature = "media")]
mod media;
mod platform;
#[cfg(feature = "rtc")]
mod rtc;
mod service;
mod update;
#[cfg(all(feature = "media", target_os = "windows"))]
mod windows_encoder;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use crate::config::AgentConfig;
use crate::control_plane::ControlPlane;

#[derive(Debug, Parser)]
#[command(
    name = "nanoctl",
    version,
    about = "Headless nanoctl remote desktop agent"
)]
struct Cli {
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// Write this marker after the service has initialized and remove it on exit.
    #[arg(long, global = true)]
    ready_file: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Enroll this computer using a single-use setup code.
    Enroll {
        code: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        control_plane: Option<url::Url>,
    },
    /// Run the foreground service process.
    Run,
    /// Check configuration, credentials, capture, input, and network readiness.
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
    /// Delete this computer's local device credential and enrollment configuration.
    Unenroll,
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

#[tokio::main]
async fn main() -> Result<()> {
    control_plane::install_crypto_provider()?;
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("nanoctl=info")),
        )
        .compact()
        .init();

    let cli = Cli::parse();
    let config_path = cli.config.unwrap_or(AgentConfig::default_path()?);

    match cli.command {
        Command::Enroll {
            code,
            name,
            control_plane,
        } => {
            let mut config = AgentConfig::load_or_default(&config_path)?;
            if let Some(url) = control_plane {
                config.control_plane_url = url;
            }
            config.validate()?;
            let device_name = name.unwrap_or_else(platform::default_device_name);
            let client = ControlPlane::new(config.control_plane_url.clone())?;
            let enrollment = client
                .enroll(code, device_name, config.features.remote_input)
                .await
                .context("enrollment failed")?;
            credential::store(&enrollment.device_id, &enrollment.token)?;
            config.device_id = Some(enrollment.device_id);
            config.save(&config_path)?;
            println!("Enrolled successfully.");
        }
        Command::Run => {
            let config = AgentConfig::load(&config_path)?;
            config.validate_enrolled()?;
            service::run(config, cli.ready_file).await?;
        }
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
        Command::Config => {
            let config = AgentConfig::load_or_default(&config_path)?;
            println!("{}", toml::to_string_pretty(&config)?);
        }
        Command::Paths => {
            println!("config={}", config_path.display());
        }
        Command::Unenroll => {
            let config = AgentConfig::load_or_default(&config_path)?;
            if let Some(device_id) = config.device_id.as_deref() {
                credential::delete(device_id)?;
            }
            if config_path.exists() {
                std::fs::remove_file(&config_path)
                    .with_context(|| format!("cannot remove {}", config_path.display()))?;
            }
            println!("Local enrollment removed. Revoke the device in the web dashboard if needed.");
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
