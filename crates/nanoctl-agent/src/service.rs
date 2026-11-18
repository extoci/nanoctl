use std::sync::Arc;
#[cfg(feature = "rtc")]
use std::{collections::HashMap, collections::HashSet};
use std::{path::PathBuf, time::Duration};

use anyhow::{Context, Result};
#[cfg(feature = "rtc")]
use tokio::time::Instant;
use tokio::time::MissedTickBehavior;
use tracing::{info, warn};
#[cfg(feature = "rtc")]
use webrtc::ice_transport::ice_server::RTCIceServer;
#[cfg(feature = "rtc")]
use webrtc::peer_connection::policy::ice_transport_policy::RTCIceTransportPolicy;

#[cfg(feature = "rtc")]
use crate::config::IceTransport;
use crate::{
    config::AgentConfig,
    control_plane::{ControlPlane, SessionList, SessionPollError},
    credential,
    service_control::ServiceControl,
};

// Reconciliation must not hold the lifecycle owner on a control-plane request for the full
// reqwest client timeout. The stop protocol waits for native capture teardown, so network setup
// gets a short independent budget and falls back to direct ICE when it misses that budget.
#[cfg(feature = "rtc")]
const CONTROL_PLANE_SETUP_TIMEOUT: Duration = Duration::from_secs(5);

pub async fn run(
    config: AgentConfig,
    ready_file: Option<PathBuf>,
    ready_token: Option<String>,
) -> Result<()> {
    let Some(service_control) = ServiceControl::acquire()? else {
        info!("service startup cancelled by an in-flight stop request");
        return Ok(());
    };
    let device_id = config
        .device_id
        .as_deref()
        .context("agent is not enrolled")?;
    let token = Arc::new(credential::load(device_id)?);
    let client = ControlPlane::new(config.control_plane_url.clone())?;
    let _ready_file = ready_file
        .map(|path| ReadyFile::create(path, ready_token.as_deref()))
        .transpose()?;
    let mut stop_requests = tokio::time::interval(Duration::from_millis(100));
    stop_requests.set_missed_tick_behavior(MissedTickBehavior::Skip);

    // Network work is isolated from the lifecycle loop. A slow control-plane request can no
    // longer delay stop requests, revocation handling, or session teardown.
    let (events_tx, mut events_rx) = tokio::sync::mpsc::channel(16);
    let heartbeat_client = client.clone();
    let heartbeat_token = token.clone();
    let heartbeat_config = config.clone();
    let heartbeat_events = events_tx.clone();
    let heartbeat_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(
            heartbeat_config.network.heartbeat_seconds,
        ));
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            let result = tokio::time::timeout(
                Duration::from_secs(10),
                heartbeat_client.heartbeat(
                    token_ref(&heartbeat_token),
                    heartbeat_config.network.heartbeat_seconds,
                    heartbeat_config.features.remote_input,
                    heartbeat_config.quality.encoder,
                ),
            )
            .await
            .map_err(|_| anyhow::anyhow!("heartbeat timed out"))
            .and_then(|result| result);
            if heartbeat_events
                .send(ServiceEvent::Heartbeat(result))
                .await
                .is_err()
            {
                break;
            }
        }
    });

    let poll_client = client.clone();
    let poll_token = token.clone();
    let poll_config = config.clone();
    let poll_events = events_tx.clone();
    let poll_task = tokio::spawn(async move {
        let mut interval =
            tokio::time::interval(Duration::from_millis(poll_config.network.poll_milliseconds));
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            let result = poll_client.sessions(token_ref(&poll_token)).await;
            let revoked = matches!(result, Err(SessionPollError::Revoked));
            if poll_events
                .send(ServiceEvent::Sessions(result))
                .await
                .is_err()
                || revoked
            {
                break;
            }
        }
    });
    drop(events_tx);

    #[cfg(feature = "rtc")]
    let (session_commands, mut session_manager) = {
        // Session updates are snapshots, not a work queue. Keep only one pending snapshot so a
        // stop command cannot sit behind a backlog of stale polls.
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        let manager = tokio::spawn(run_session_manager(
            receiver,
            client.clone(),
            token.clone(),
            config.clone(),
        ));
        (sender, manager)
    };

    info!(
        device_id,
        platform = crate::platform::PLATFORM,
        "agent service started"
    );

    loop {
        tokio::select! {
            _ = stop_requests.tick() => {
                if service_control.take_stop_request()? {
                    info!("graceful service stop requested");
                    heartbeat_task.abort();
                    poll_task.abort();
                    #[cfg(feature = "rtc")]
                    stop_session_manager(&session_commands, &mut session_manager).await;
                    return Ok(());
                }
            }
            _ = tokio::signal::ctrl_c() => {
                info!("shutdown requested");
                heartbeat_task.abort();
                poll_task.abort();
                #[cfg(feature = "rtc")]
                stop_session_manager(&session_commands, &mut session_manager).await;
                return Ok(());
            }
            event = events_rx.recv() => {
                match event {
                    Some(ServiceEvent::Heartbeat(Err(error))) => {
                        warn!(error = %redact(&error), "heartbeat failed");
                    }
                    Some(ServiceEvent::Heartbeat(Ok(()))) => {}
                    Some(ServiceEvent::Sessions(Ok(result))) => {
                        #[cfg(feature = "rtc")]
                        if session_commands.try_send(SessionCommand::Update(result.sessions)).is_err() {
                            warn!("session manager is busy; retaining its current lifecycle state");
                        }
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
                    Some(ServiceEvent::Sessions(Err(SessionPollError::Revoked))) => {
                        heartbeat_task.abort();
                        poll_task.abort();
                        #[cfg(feature = "rtc")]
                        stop_session_manager(&session_commands, &mut session_manager).await;
                        info!("agent credential revoked; service stopped");
                        return Ok(());
                    }
                    Some(ServiceEvent::Sessions(Err(SessionPollError::Request(error)))) => {
                        warn!(error = %redact(&error), "session poll failed");
                    }
                    None => {
                        heartbeat_task.abort();
                        poll_task.abort();
                        #[cfg(feature = "rtc")]
                        stop_session_manager(&session_commands, &mut session_manager).await;
                        return Ok(());
                    }
                }
            }
        }
    }
}

enum ServiceEvent {
    Heartbeat(Result<()>),
    Sessions(std::result::Result<SessionList, SessionPollError>),
}

struct ReadyFile {
    path: PathBuf,
}

impl ReadyFile {
    fn create(path: PathBuf, token: Option<&str>) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("cannot create readiness directory {}", parent.display())
            })?;
        }
        if token.is_some_and(|value| value.contains('\r') || value.contains('\n')) {
            anyhow::bail!("readiness token cannot contain a newline");
        }
        let token_line = token.map_or_else(String::new, |value| format!("token={value}\n"));
        std::fs::write(
            &path,
            format!(
                "pid={}\nversion={}\n{}",
                std::process::id(),
                env!("CARGO_PKG_VERSION"),
                token_line
            ),
        )
        .with_context(|| format!("cannot write readiness marker {}", path.display()))?;
        Ok(Self { path })
    }
}

impl Drop for ReadyFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod readiness_tests {
    use super::ReadyFile;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn readiness_marker_is_transaction_bound_and_removed() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after the Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "nanoctl-ready-{}-{unique}.marker",
            std::process::id()
        ));
        let marker = ReadyFile::create(path.clone(), Some("transaction-123"))
            .expect("readiness marker should be writable");
        let contents = std::fs::read_to_string(&path).expect("readiness marker should be readable");
        assert!(contents.contains("pid="));
        assert!(contents.contains("version="));
        assert!(contents.contains("token=transaction-123"));
        drop(marker);
        assert!(!path.exists());
    }
}

fn token_ref(token: &Credential) -> &zeroize::Zeroizing<String> {
    token.as_ref()
}

type Credential = Arc<zeroize::Zeroizing<String>>;

#[cfg(feature = "rtc")]
enum SessionCommand {
    Update(Vec<crate::control_plane::PendingSession>),
    Stop,
}

#[cfg(feature = "rtc")]
async fn run_session_manager(
    mut commands: tokio::sync::mpsc::Receiver<SessionCommand>,
    client: ControlPlane,
    token: Credential,
    config: AgentConfig,
) {
    let mut active = HashMap::new();
    while let Some(command) = commands.recv().await {
        match command {
            SessionCommand::Update(pending) => {
                // Reconciliation owns native capture tasks. Cancelling it after a wall-clock
                // deadline would drop VideoPipeline::stop while its spawn_blocking worker still
                // owns the desktop, allowing the next peer to overlap native resources.
                reconcile_sessions(&mut active, &client, &token, &config, &pending).await;
            }
            SessionCommand::Stop => break,
        }
    }
    close_all_sessions(&mut active).await;
}

#[cfg(feature = "rtc")]
async fn stop_session_manager(
    commands: &tokio::sync::mpsc::Sender<SessionCommand>,
    manager: &mut tokio::task::JoinHandle<()>,
) {
    let _ = commands.send(SessionCommand::Stop).await;
    if let Err(error) = manager.await {
        warn!(error = %error, "session manager stopped unexpectedly");
    }
}

#[cfg(feature = "rtc")]
async fn close_all_sessions(active: &mut HashMap<String, ActiveSession>) {
    // Sessions own independent capture and peer resources. Stop them concurrently so the service
    // stop deadline is governed by the slowest native teardown rather than session count. Each
    // pipeline is still awaited; dropping its wrapper would allow its spawn_blocking worker to
    // overlap a later capture session.
    let shutdowns = active
        .drain()
        .map(|(session_id, session)| async move {
            #[cfg(feature = "media")]
            session.peer.revoke_input();
            #[cfg(feature = "media")]
            session.media_task.stop().await;
            if let Err(error) = session.peer.close().await {
                warn!(
                    session_id,
                    error = %redact(&error),
                    "peer did not close cleanly during shutdown"
                );
            }
        })
        .collect::<Vec<_>>();
    futures_util::future::join_all(shutdowns).await;
}

#[cfg(feature = "rtc")]
struct ActiveSession {
    peer: crate::rtc::HostPeer,
    processed: HashSet<u64>,
    offer_sequence: u64,
    offer_created_at: u64,
    offer_controller_epoch: String,
    offer_generation: u64,
    host_epoch: String,
    failed_since: Option<Instant>,
    #[cfg(feature = "media")]
    media_task: crate::media::VideoPipeline,
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
            session.peer.revoke_input();
            #[cfg(feature = "media")]
            session.media_task.stop().await;
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
            session.peer.revoke_input();
            #[cfg(feature = "media")]
            session.media_task.stop().await;
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
                offer_sequence,
                offer_created_at,
                offer_controller_epoch,
                offer_generation,
                host_epoch,
                failed_since,
                media_task,
                media_restarts,
            } = session;
            let media_result = media_task.finish().await;
            if let Err(error) = &media_result
                && crate::media::is_video_delivery_unavailable(error)
            {
                warn!(
                    session_id,
                    "video transport did not recover; failing peer without restarting native media"
                );
                if let Err(error) = peer.fail("video transport did not recover").await {
                    warn!(
                        session_id,
                        error = %redact(&error),
                        "could not publish terminal video transport status"
                    );
                }
                continue;
            }
            let detail = match media_result {
                Ok(()) => "media pipeline ended unexpectedly".to_owned(),
                Err(error) => redact(&error),
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
                        offer_sequence,
                        offer_created_at,
                        offer_controller_epoch,
                        offer_generation,
                        host_epoch,
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
        // The control-plane snapshot is also the host input lease. A controller CAS claim clears
        // the host epoch before its replacement offer exists, so revoke the old input lane before
        // looking for that next offer; otherwise the stale data channel could remain usable while
        // the manager waits for a second poll.
        let lease_replaced = active
            .get(&session.session_id)
            .is_some_and(|active_session| {
                let controller_replaced = session
                    .controller_epoch
                    .as_deref()
                    .is_some_and(|epoch| epoch != active_session.offer_controller_epoch);
                let host_replaced =
                    session.host_epoch.as_deref() != Some(active_session.host_epoch.as_str());
                controller_replaced || host_replaced
            });
        if lease_replaced && let Some(previous) = active.remove(&session.session_id) {
            #[cfg(feature = "media")]
            previous.peer.revoke_input();
            #[cfg(feature = "media")]
            previous.media_task.stop().await;
            let _ = previous.peer.close().await;
        }

        let offer = session
            .signals
            .iter()
            .filter(|signal| {
                signal.sender == "controller"
                    && crate::rtc::is_offer_for_session(&signal.envelope, &session.session_id)
            })
            .max_by_key(|signal| (signal.created_at, signal.sequence));
        let Some(offer) = offer else { continue };
        let Ok((offer_generation, offer_controller_epoch)) =
            crate::rtc::offer_generation_and_epoch(&offer.envelope, &session.session_id)
        else {
            continue;
        };

        // A browser refresh creates a new controller peer for the same session. It intentionally
        // starts a fresh generation, so an existing host peer must be replaced instead of
        // treating the new generation-0 offer as a stale duplicate. In-place ICE restarts have a
        // strictly higher generation and stay on the existing peer.
        let replace_active = active
            .get(&session.session_id)
            .is_some_and(|active_session| {
                (active_session.offer_sequence != offer.sequence
                    || active_session.offer_created_at != offer.created_at)
                    && (offer_controller_epoch != active_session.offer_controller_epoch
                        || offer_generation <= active_session.offer_generation)
            });
        if replace_active && let Some(previous) = active.remove(&session.session_id) {
            #[cfg(feature = "media")]
            previous.peer.revoke_input();
            #[cfg(feature = "media")]
            previous.media_task.stop().await;
            let _ = previous.peer.close().await;
        }

        if !active.contains_key(&session.session_id) {
            let mut ice_servers = if _config.network.stun_urls.is_empty() {
                Vec::new()
            } else {
                vec![RTCIceServer {
                    urls: _config.network.stun_urls.clone(),
                    ..Default::default()
                }]
            };
            let turn_result = tokio::time::timeout(
                CONTROL_PLANE_SETUP_TIMEOUT,
                client.turn_credentials(token.as_ref(), &session.session_id),
            )
            .await
            .map_err(|_| anyhow::anyhow!("TURN credentials request timed out"))
            .and_then(|result| result);
            match turn_result {
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
            let host_epoch = crate::rtc::host_epoch_seed();
            let claim_result = tokio::time::timeout(
                CONTROL_PLANE_SETUP_TIMEOUT,
                client.claim_host(
                    token.as_ref(),
                    &session.session_id,
                    &offer_controller_epoch,
                    &host_epoch,
                    session.host_epoch.as_deref(),
                    offer_generation,
                ),
            )
            .await
            .map_err(|_| anyhow::anyhow!("native peer claim request timed out"))
            .and_then(|result| result);
            if let Err(error) = claim_result {
                warn!(
                    session_id = session.session_id,
                    error = %redact(&error),
                    "native peer claim was rejected"
                );
                continue;
            }
            let peer_result = crate::rtc::HostPeer::answer_with_host_epoch(
                client.clone(),
                token.clone(),
                session.session_id.clone(),
                &offer.envelope,
                ice_servers,
                ice_transport_policy,
                host_epoch.clone(),
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
                            offer_sequence: offer.sequence,
                            offer_created_at: offer.created_at,
                            offer_controller_epoch: offer_controller_epoch.clone(),
                            offer_generation,
                            host_epoch: host_epoch.clone(),
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
                    if let Err(publish_error) = crate::rtc::report_negotiation_failure(
                        client,
                        token,
                        &session.session_id,
                        &offer_controller_epoch,
                        &host_epoch,
                        offer_generation,
                        "agent could not initialize the remote desktop session",
                    )
                    .await
                    {
                        warn!(
                            session_id = session.session_id,
                            error = %redact(&publish_error),
                            "could not publish terminal negotiation status"
                        );
                    }
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
                    if let Ok((generation, controller_epoch)) =
                        crate::rtc::offer_generation_and_epoch(
                            &signal.envelope,
                            &session.session_id,
                        )
                    {
                        active_session.offer_sequence = signal.sequence;
                        active_session.offer_created_at = signal.created_at;
                        active_session.offer_controller_epoch = controller_epoch;
                        active_session.offer_generation = generation;
                    }
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
                session.peer.revoke_input();
                session.media_task.stop().await;
                let _ = session.peer.close().await;
            }
            #[cfg(not(feature = "media"))]
            drop(removed);
        }
    }
}

#[cfg(feature = "rtc")]
fn failure_grace_elapsed(failed: bool, failed_since: &mut Option<Instant>, now: Instant) -> bool {
    if !failed {
        *failed_since = None;
        return false;
    }
    let started = failed_since.get_or_insert(now);
    now.duration_since(*started) >= Duration::from_secs(15)
}

#[cfg(feature = "media")]
fn spawn_media(peer: &crate::rtc::HostPeer, config: &AgentConfig) -> crate::media::VideoPipeline {
    crate::media::spawn_video(
        peer.video_track(),
        peer.keyframe_requests(),
        peer.bitrate_estimate_kbps(),
        peer.display_selection(),
        peer.display_selection_feedback_sender(),
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
    let value = error.to_string().to_ascii_lowercase();
    if value.contains("timed out") || value.contains("timeout") {
        "operation timed out".to_owned()
    } else if value.contains("connect") || value.contains("dns") {
        "connection unavailable".to_owned()
    } else if value.contains("status") || value.contains("rejected") {
        "remote service rejected the operation".to_owned()
    } else if value.contains("invalid") || value.contains("malformed") {
        "invalid data".to_owned()
    } else if value.contains("permission") || value.contains("denied") {
        "permission denied".to_owned()
    } else if value.contains("unavailable") || value.contains("not available") {
        "resource unavailable".to_owned()
    } else {
        "operation failed".to_owned()
    }
}

#[cfg(all(test, feature = "rtc"))]
mod tests {
    use super::{failure_grace_elapsed, redact};
    use std::time::Duration;

    #[test]
    fn logged_error_snapshots_never_include_private_session_material() {
        for (sensitive, expected) in [
            (
                "Bearer secret-device-token",
                "remote service rejected the operation",
            ),
            (
                "v=0\r\na=fingerprint:sha-256 11:22",
                "remote service rejected the operation",
            ),
            (
                "candidate:1 1 UDP 1 192.0.2.1 5000 typ host",
                "remote service rejected the operation",
            ),
            (
                "turns://user:password@turn.example.com",
                "remote service rejected the operation",
            ),
            (
                "clipboard: private recovery phrase",
                "remote service rejected the operation",
            ),
        ] {
            let rendered = redact(&anyhow::anyhow!("request rejected: {sensitive}"));
            assert_eq!(rendered, expected);
            assert!(!rendered.contains(sensitive));
        }
    }

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
