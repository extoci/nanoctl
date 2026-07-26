use std::time::Duration;

use anyhow::{Context, Result};
use tokio::time::{Instant, MissedTickBehavior};
use tracing::{info, warn};

use crate::{config::AgentConfig, control_plane::ControlPlane, credential};

pub async fn run(config: AgentConfig) -> Result<()> {
    let device_id = config
        .device_id
        .as_deref()
        .context("agent is not enrolled")?;
    let token = credential::load(device_id)?;
    let client = ControlPlane::new(config.control_plane_url.clone())?;
    let mut heartbeat =
        tokio::time::interval(Duration::from_secs(config.network.heartbeat_seconds));
    let mut sessions =
        tokio::time::interval(Duration::from_millis(config.network.poll_milliseconds));
    heartbeat.set_missed_tick_behavior(MissedTickBehavior::Skip);
    sessions.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut next_heartbeat = Instant::now();

    info!(
        device_id,
        platform = crate::platform::PLATFORM,
        "agent service started"
    );

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                info!("shutdown requested");
                return Ok(());
            }
            _ = heartbeat.tick() => {
                if Instant::now() >= next_heartbeat {
                    if let Err(error) = client.heartbeat(&token).await {
                        warn!(error = %redact(&error), "heartbeat failed");
                    }
                    next_heartbeat = Instant::now() + Duration::from_secs(config.network.heartbeat_seconds);
                }
            }
            _ = sessions.tick() => {
                match client.sessions(&token).await {
                    Ok(result) => {
                        for session in result.sessions {
                            // The media engine consumes authorized offers. Until capture readiness is
                            // true it deliberately leaves sessions pending instead of accepting input.
                            let signal_bytes = session
                                .signals
                                .iter()
                                .map(|signal| signal.envelope.len())
                                .sum::<usize>();
                            let last_controller_sequence = session
                                .signals
                                .iter()
                                .filter(|signal| signal.sender == "controller")
                                .map(|signal| signal.sequence)
                                .max();
                            tracing::debug!(
                                session_id = session.session_id,
                                expires_at = session.expires_at,
                                signals = session.signals.len(),
                                signal_bytes,
                                last_controller_sequence,
                                "authorized session observed"
                            );
                        }
                    }
                    Err(error) => warn!(error = %redact(&error), "session poll failed"),
                }
            }
        }
    }
}

fn redact(error: &anyhow::Error) -> String {
    let value = error.to_string();
    if value.len() > 256 {
        format!("{}…", &value[..256])
    } else {
        value
    }
}
