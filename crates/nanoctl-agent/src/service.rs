use std::time::Duration;
#[cfg(feature = "rtc")]
use std::{collections::HashMap, collections::HashSet, sync::Arc};

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
    #[cfg(feature = "rtc")]
    let token = Arc::new(token);
    #[cfg(feature = "rtc")]
    let mut active_sessions: HashMap<String, ActiveSession> = HashMap::new();

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
                    if let Err(error) = client.heartbeat(token_ref(&token)).await {
                        warn!(error = %redact(&error), "heartbeat failed");
                    }
                    next_heartbeat = Instant::now() + Duration::from_secs(config.network.heartbeat_seconds);
                }
            }
            _ = sessions.tick() => {
                match client.sessions(token_ref(&token)).await {
                    Ok(result) => {
                        #[cfg(feature = "rtc")]
                        reconcile_sessions(
                            &mut active_sessions,
                            &client,
                            &token,
                            &config,
                            &result.sessions,
                        ).await;
                        #[cfg(not(feature = "rtc"))]
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

fn token_ref(token: &Credential) -> &zeroize::Zeroizing<String> {
    #[cfg(feature = "rtc")]
    {
        token.as_ref()
    }
    #[cfg(not(feature = "rtc"))]
    {
        token
    }
}

#[cfg(feature = "rtc")]
type Credential = Arc<zeroize::Zeroizing<String>>;
#[cfg(not(feature = "rtc"))]
type Credential = zeroize::Zeroizing<String>;

#[cfg(feature = "rtc")]
struct ActiveSession {
    peer: crate::rtc::HostPeer,
    processed: HashSet<u64>,
    #[cfg(feature = "media")]
    media_task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

#[cfg(feature = "rtc")]
async fn reconcile_sessions(
    active: &mut HashMap<String, ActiveSession>,
    client: &ControlPlane,
    token: &Arc<zeroize::Zeroizing<String>>,
    _config: &AgentConfig,
    pending: &[crate::control_plane::PendingSession],
) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64);
    let visible = pending
        .iter()
        .map(|session| session.session_id.as_str())
        .collect::<HashSet<_>>();
    let stale = active
        .keys()
        .filter(|session_id| !visible.contains(session_id.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    for session_id in stale {
        if let Some(session) = active.remove(&session_id) {
            #[cfg(feature = "media")]
            session.media_task.abort();
            let _ = session.peer.close().await;
        }
    }

    for session in pending.iter().filter(|session| session.expires_at > now) {
        if !active.contains_key(&session.session_id) {
            let offer = session.signals.iter().find(|signal| {
                signal.sender == "controller" && signal.envelope.contains(r#""type":"offer""#)
            });
            let Some(offer) = offer else { continue };
            let ice_servers = match client
                .turn_credentials(token.as_ref(), &session.session_id)
                .await
            {
                Ok(Some(turn)) => {
                    if turn.expires_at <= now {
                        warn!(
                            session_id = session.session_id,
                            "received expired TURN credentials"
                        );
                        Vec::new()
                    } else {
                        vec![webrtc::ice_transport::ice_server::RTCIceServer {
                            urls: turn.urls,
                            username: turn.username,
                            credential: turn.credential,
                        }]
                    }
                }
                Ok(None) => Vec::new(),
                Err(error) => {
                    warn!(
                        session_id = session.session_id,
                        error = %redact(&error),
                        "TURN credentials unavailable; trying direct ICE"
                    );
                    Vec::new()
                }
            };
            match crate::rtc::HostPeer::answer(
                client.clone(),
                token.clone(),
                session.session_id.clone(),
                &offer.envelope,
                ice_servers,
                _config.features.remote_input,
            )
            .await
            {
                Ok(peer) => {
                    #[cfg(feature = "media")]
                    let media_task = crate::media::spawn_video(
                        peer.video_track(),
                        _config.quality.max_bitrate_kbps,
                        _config.quality.max_fps,
                        _config.quality.max_width,
                        _config.quality.max_height,
                    );
                    active.insert(
                        session.session_id.clone(),
                        ActiveSession {
                            peer,
                            processed: HashSet::from([offer.sequence]),
                            #[cfg(feature = "media")]
                            media_task,
                        },
                    );
                    info!(
                        session_id = session.session_id,
                        "session negotiation started"
                    );
                }
                Err(error) => {
                    warn!(
                        session_id = session.session_id,
                        error = %redact(&error),
                        "session negotiation failed"
                    );
                    continue;
                }
            }
        }
        let Some(active_session) = active.get_mut(&session.session_id) else {
            continue;
        };
        for signal in &session.signals {
            if signal.sender != "controller" || !active_session.processed.insert(signal.sequence) {
                continue;
            }
            if let Err(error) = active_session
                .peer
                .add_signal(&signal.envelope, &session.session_id)
                .await
            {
                warn!(
                    session_id = session.session_id,
                    error = %redact(&error),
                    "controller signal rejected"
                );
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
