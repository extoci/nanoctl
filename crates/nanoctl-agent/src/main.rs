mod config;
mod control_plane;
mod credential;
#[cfg(feature = "media")]
mod input;
#[cfg(feature = "media")]
mod media;
mod platform;
#[cfg(feature = "rtc")]
mod rtc;
mod service;

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
    /// Print the effective redacted configuration.
    Config,
    /// Print the configuration path used by this invocation.
    Paths,
    /// Delete this computer's local device credential and enrollment configuration.
    Unenroll,
}

#[tokio::main]
async fn main() -> Result<()> {
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
                .enroll(code, device_name)
                .await
                .context("enrollment failed")?;
            credential::store(&enrollment.device_id, &enrollment.token)?;
            config.device_id = Some(enrollment.device_id);
            config.save(&config_path)?;
            println!(
                "Enrolled successfully. Run `nanoctl doctor`, then install/start the service."
            );
        }
        Command::Run => {
            let config = AgentConfig::load(&config_path)?;
            config.validate_enrolled()?;
            service::run(config).await?;
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
    }
    Ok(())
}
