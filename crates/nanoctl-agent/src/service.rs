use std::time::Duration;
#[cfg(feature = "rtc")]
use std::{collections::HashMap, collections::HashSet, sync::Arc};

use anyhow::{Context, Result};
use tokio::time::{Instant, MissedTickBehavior};
use tracing::{info, warn};
#[cfg(feature = "rtc")]
use webrtc::ice_transport::ice_server::RTCIceServer;
#[cfg(feature = "rtc")]
use webrtc::peer_connection::policy::ice_transport_policy::RTCIceTransportPolicy;

#[cfg(feature = "rtc")]
use crate::config::IceTransport;
use crate::{
    config::AgentConfig,
    control_plane::{ControlPlane, SessionPollError},
    credential,
};

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
                    Err(SessionPollError::Revoked) => {
                        #[cfg(feature = "rtc")]
                        close_all_sessions(&mut active_sessions).await;
                        info!("agent credential revoked; service stopped");
                        return Ok(());
                    }
                    Err(SessionPollError::Request(error)) => {
                        warn!(error = %redact(&error), "session poll failed");
                    }
                }
            }
        }
    }
}

#[cfg(feature = "rtc")]
async fn close_all_sessions(active: &mut HashMap<String, ActiveSession>) {
    for (_, session) in active.drain() {
        #[cfg(feature = "media")]
        session.media_task.abort();
        let _ = session.peer.close().await;
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
    failed_since: Option<Instant>,
    #[cfg(feature = "media")]
    media_task: tokio::task::JoinHandle<anyhow::Result<()>>,
    #[cfg(feature = "media")]
    media_restarts: u8,
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

    let failed = active
        .iter_mut()
        .filter_map(|(session_id, session)| {
            failure_grace_elapsed(
                session.peer.connection_failed(),
                &mut session.failed_since,
                Instant::now(),
            )
            .then(|| session_id.clone())
        })
        .collect::<Vec<_>>();
    for session_id in failed {
        if let Some(session) = active.remove(&session_id) {
            #[cfg(feature = "media")]
            session.media_task.abort();
            if let Err(error) = session.peer.fail("peer connection failed").await {
                warn!(
                    session_id,
                    error = %redact(&error),
                    "could not publish terminal peer status"
                );
            }
        }
    }

    #[cfg(feature = "media")]
    {
        let finished = active
            .iter()
            .filter(|(_, session)| session.media_task.is_finished())
            .map(|(session_id, _)| session_id.clone())
            .collect::<Vec<_>>();
        for session_id in finished {
            let Some(session) = active.remove(&session_id) else {
                continue;
            };
            let ActiveSession {
                peer,
                processed,
                failed_since,
                media_task,
                media_restarts,
            } = session;
            let detail = match media_task.await {
                Ok(Ok(())) => "media pipeline ended unexpectedly".to_owned(),
                Ok(Err(error)) => redact(&error),
                Err(error) => format!("media task stopped: {error}"),
            };
            if media_restarts == 0 {
                warn!(
                    session_id,
                    error = detail,
                    "media pipeline stopped; attempting one bounded restart"
                );
                let media_task = spawn_media(&peer, _config);
                active.insert(
                    session_id,
                    ActiveSession {
                        peer,
                        processed,
                        failed_since,
                        media_task,
                        media_restarts: 1,
                    },
                );
            } else {
                warn!(
                    session_id,
                    error = detail,
                    "media pipeline stopped after restart; failing session"
                );
                if let Err(error) = peer.fail("media pipeline failed after restart").await {
                    warn!(
                        session_id,
                        error = %redact(&error),
                        "could not publish terminal media status"
                    );
                }
            }
        }
    }

    for session in pending.iter().filter(|session| session.expires_at > now) {
        if !active.contains_key(&session.session_id) {
            let offer = session.signals.iter().find(|signal| {
                signal.sender == "controller"
                    && crate::rtc::is_offer_for_session(&signal.envelope, &session.session_id)
            });
            let Some(offer) = offer else { continue };
            let mut ice_servers = if _config.network.stun_urls.is_empty() {
                Vec::new()
            } else {
                vec![RTCIceServer {
                    urls: _config.network.stun_urls.clone(),
                    ..Default::default()
                }]
            };
            match client
                .turn_credentials(token.as_ref(), &session.session_id)
                .await
            {
                Ok(Some(turn)) => {
                    if turn.expires_at <= now {
                        warn!(
                            session_id = session.session_id,
                            "received expired TURN credentials"
                        );
                        if matches!(_config.network.ice_transport, IceTransport::Relay) {
                            warn!(
                                session_id = session.session_id,
                                "relay-only session has no valid TURN credentials"
                            );
                            continue;
                        }
                    } else {
                        ice_servers.push(RTCIceServer {
                            urls: turn.urls,
                            username: turn.username,
                            credential: turn.credential,
                        });
                    }
                }
                Ok(None) => {
                    if matches!(_config.network.ice_transport, IceTransport::Relay) {
                        warn!(
                            session_id = session.session_id,
                            "relay-only session cannot start because TURN is not configured"
                        );
                        continue;
                    }
                }
                Err(error) => {
                    warn!(
                        session_id = session.session_id,
                        error = %redact(&error),
                        "TURN credentials unavailable; trying direct ICE"
                    );
                    if matches!(_config.network.ice_transport, IceTransport::Relay) {
                        continue;
                    }
                }
            }
            let ice_transport_policy = match _config.network.ice_transport {
                IceTransport::All => RTCIceTransportPolicy::All,
                IceTransport::Relay => RTCIceTransportPolicy::Relay,
            };
            let peer_result = crate::rtc::HostPeer::answer(
                client.clone(),
                token.clone(),
                session.session_id.clone(),
                &offer.envelope,
                ice_servers,
                ice_transport_policy,
                _config.features.remote_input,
            )
            .await;
            match peer_result {
                Ok(peer) => {
                    #[cfg(feature = "media")]
                    let media_task = spawn_media(&peer, _config);
                    active.insert(
                        session.session_id.clone(),
                        ActiveSession {
                            peer,
                            processed: HashSet::from([offer.sequence]),
                            failed_since: None,
                            #[cfg(feature = "media")]
                            media_task,
                            #[cfg(feature = "media")]
                            media_restarts: 0,
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
        let mut terminal = false;
        for signal in &session.signals {
            if signal.sender != "controller" || active_session.processed.contains(&signal.sequence)
            {
                continue;
            }
            match active_session
                .peer
                .add_signal(&signal.envelope, &session.session_id, signal.sequence)
                .await
            {
                Ok(keep_open) => {
                    active_session.processed.insert(signal.sequence);
                    if !keep_open {
                        terminal = true;
                        break;
                    }
                }
                Err(error) => {
                    warn!(
                        session_id = session.session_id,
                        error = %redact(&error),
                        "controller signal rejected"
                    );
                }
            }
        }
        if terminal {
            let removed = active.remove(&session.session_id);
            #[cfg(feature = "media")]
            if let Some(session) = removed {
                session.media_task.abort();
            }
            #[cfg(not(feature = "media"))]
            drop(removed);
        }
    }
}

#[cfg(feature = "rtc")]
fn failure_grace_elapsed(
    failed: bool,
    failed_since: &mut Option<Instant>,
    now: Instant,
) -> bool {
    if !failed {
        *failed_since = None;
        return false;
    }
    let started = failed_since.get_or_insert(now);
    now.duration_since(*started) >= Duration::from_secs(15)
}

#[cfg(feature = "media")]
fn spawn_media(
    peer: &crate::rtc::HostPeer,
    config: &AgentConfig,
) -> tokio::task::JoinHandle<anyhow::Result<()>> {
    crate::media::spawn_video(
        peer.video_track(),
        peer.keyframe_requests(),
        peer.bitrate_estimate_kbps(),
        peer.display_selection(),
        crate::media::VideoQuality {
            max_bitrate_kbps: config.quality.max_bitrate_kbps,
            max_fps: config.quality.max_fps,
            max_width: config.quality.max_width,
            max_height: config.quality.max_height,
            latency_mode: config.quality.latency_mode,
            encoder_preference: config.quality.encoder,
        },
    )
}

fn redact(error: &anyhow::Error) -> String {
    let value = error.to_string();
    if value.len() > 256 {
        format!("{}…", &value[..256])
    } else {
        value
    }
}

#[cfg(all(test, feature = "rtc"))]
mod tests {
    use super::failure_grace_elapsed;
    use std::time::Duration;
    use tokio::time::Instant;

    #[test]
    fn peer_failure_requires_a_continuous_grace_period() {
        let start = Instant::now();
        let mut failed_since = None;
        assert!(!failure_grace_elapsed(true, &mut failed_since, start));
        assert!(!failure_grace_elapsed(
            true,
            &mut failed_since,
            start + Duration::from_secs(14),
        ));
        assert!(failure_grace_elapsed(
            true,
            &mut failed_since,
            start + Duration::from_secs(15),
        ));
        assert!(!failure_grace_elapsed(
            false,
            &mut failed_since,
            start + Duration::from_secs(16),
        ));
        assert_eq!(failed_since, None);
    }
}
