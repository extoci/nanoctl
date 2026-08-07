#[cfg(feature = "rtc")]
use std::{collections::HashMap, collections::HashSet, sync::Arc};
use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use directories::ProjectDirs;
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

pub async fn run(
    config: AgentConfig,
    ready_file: Option<PathBuf>,
    ready_token: Option<String>,
) -> Result<()> {
    let service_control = ServiceControl::acquire(ServiceControlPaths::default()?)?;
    let device_id = config
        .device_id
        .as_deref()
        .context("agent is not enrolled")?;
    let token = credential::load(device_id)?;
    let client = ControlPlane::new(config.control_plane_url.clone())?;
    let _ready_file = ready_file
        .map(|path| ReadyFile::create(path, ready_token.as_deref()))
        .transpose()?;
    let mut heartbeat =
        tokio::time::interval(Duration::from_secs(config.network.heartbeat_seconds));
    let mut sessions =
        tokio::time::interval(Duration::from_millis(config.network.poll_milliseconds));
    let mut stop_requests = tokio::time::interval(Duration::from_millis(100));
    heartbeat.set_missed_tick_behavior(MissedTickBehavior::Skip);
    sessions.set_missed_tick_behavior(MissedTickBehavior::Skip);
    stop_requests.set_missed_tick_behavior(MissedTickBehavior::Skip);
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
            _ = stop_requests.tick() => {
                if service_control.take_stop_request()? {
                    info!("graceful service stop requested");
                    #[cfg(feature = "rtc")]
                    close_all_sessions(&mut active_sessions).await;
                    return Ok(());
                }
            }
            _ = tokio::signal::ctrl_c() => {
                info!("shutdown requested");
                #[cfg(feature = "rtc")]
                close_all_sessions(&mut active_sessions).await;
                return Ok(());
            }
            _ = heartbeat.tick() => {
                if Instant::now() >= next_heartbeat {
                    if let Err(error) = client
                        .heartbeat(token_ref(&token), config.features.remote_input)
                        .await
                    {
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

#[derive(Clone, Debug)]
struct ServiceControlPaths {
    lock: PathBuf,
    request: PathBuf,
}

impl ServiceControlPaths {
    fn default() -> Result<Self> {
        let project = ProjectDirs::from("dev", "nanoctl", "nanoctl")
            .context("operating system has no local data directory")?;
        Ok(Self::in_directory(
            project.data_local_dir().join("service-control"),
        ))
    }

    fn in_directory(directory: PathBuf) -> Self {
        Self {
            lock: directory.join("agent.lock"),
            request: directory.join("stop.request"),
        }
    }

    fn prepare(&self) -> Result<()> {
        let directory = self.lock.parent().context("service lock has no parent")?;
        std::fs::create_dir_all(directory).with_context(|| {
            format!(
                "cannot create service control directory {}",
                directory.display()
            )
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))
                .with_context(|| {
                    format!(
                        "cannot protect service control directory {}",
                        directory.display()
                    )
                })?;
        }
        Ok(())
    }

    fn open_lock(&self) -> Result<File> {
        self.prepare()?;
        OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&self.lock)
            .with_context(|| format!("cannot open service lock {}", self.lock.display()))
    }
}

struct ServiceControl {
    _lock: File,
    request: PathBuf,
}

impl ServiceControl {
    fn acquire(paths: ServiceControlPaths) -> Result<Self> {
        let lock = paths.open_lock()?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                bail!("the nanoctl background agent is already running")
            }
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(error).context("cannot lock the nanoctl background agent");
            }
        }
        remove_if_present(&paths.request)?;
        Ok(Self {
            _lock: lock,
            request: paths.request,
        })
    }

    fn take_stop_request(&self) -> Result<bool> {
        match std::fs::remove_file(&self.request) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error)
                .with_context(|| format!("cannot consume stop request {}", self.request.display())),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StopOutcome {
    Stopped,
    AlreadyStopped,
}

pub async fn request_stop() -> Result<StopOutcome> {
    request_stop_with_paths(ServiceControlPaths::default()?, Duration::from_secs(30)).await
}

async fn request_stop_with_paths(
    paths: ServiceControlPaths,
    timeout: Duration,
) -> Result<StopOutcome> {
    let lock = paths.open_lock()?;
    match lock.try_lock() {
        Ok(()) => {
            remove_if_present(&paths.request)?;
            return Ok(StopOutcome::AlreadyStopped);
        }
        Err(std::fs::TryLockError::WouldBlock) => {}
        Err(std::fs::TryLockError::Error(error)) => {
            return Err(error).context("cannot inspect the nanoctl background agent");
        }
    }
    std::fs::write(&paths.request, b"stop\n")
        .with_context(|| format!("cannot request service stop at {}", paths.request.display()))?;
    let deadline = Instant::now() + timeout;
    loop {
        match lock.try_lock() {
            Ok(()) => {
                remove_if_present(&paths.request)?;
                return Ok(StopOutcome::Stopped);
            }
            Err(std::fs::TryLockError::WouldBlock) => {}
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(error).context("cannot wait for the nanoctl background agent to stop");
            }
        }
        if Instant::now() >= deadline {
            bail!("timed out waiting for the nanoctl background agent to stop")
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn remove_if_present(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("cannot remove {}", path.display())),
    }
}

struct ReadyFile {
    path: PathBuf,
}

#[cfg(test)]
mod control_tests {
    use super::{ServiceControl, ServiceControlPaths, StopOutcome, request_stop_with_paths};
    use std::time::Duration;

    #[tokio::test]
    async fn stop_command_requests_shutdown_and_waits_for_service_exit() {
        let temporary = tempfile::tempdir().expect("temporary service directory");
        let paths = ServiceControlPaths::in_directory(temporary.path().to_owned());
        let control = ServiceControl::acquire(paths.clone()).expect("running service lock");
        let stop = tokio::spawn(request_stop_with_paths(
            paths.clone(),
            Duration::from_secs(1),
        ));

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if control.take_stop_request().expect("read stop request") {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("stop request should arrive");
        drop(control);

        assert_eq!(
            stop.await
                .expect("stop task should finish")
                .expect("stop request should succeed"),
            StopOutcome::Stopped
        );
    }

    #[tokio::test]
    async fn stop_command_is_idempotent_when_service_is_not_running() {
        let temporary = tempfile::tempdir().expect("temporary service directory");
        let paths = ServiceControlPaths::in_directory(temporary.path().to_owned());
        paths.prepare().expect("service control directory");
        std::fs::write(&paths.request, b"stale\n").expect("stale stop request");

        assert_eq!(
            request_stop_with_paths(paths.clone(), Duration::from_secs(1))
                .await
                .expect("idempotent stop"),
            StopOutcome::AlreadyStopped
        );
        assert!(!paths.request.exists());
    }

    #[test]
    fn a_second_service_process_cannot_acquire_the_agent_lock() {
        let temporary = tempfile::tempdir().expect("temporary service directory");
        let paths = ServiceControlPaths::in_directory(temporary.path().to_owned());
        let _control = ServiceControl::acquire(paths.clone()).expect("first service lock");

        let error = ServiceControl::acquire(paths)
            .err()
            .expect("duplicate service must fail");
        assert!(error.to_string().contains("already running"));
    }
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

#[cfg(feature = "rtc")]
async fn close_all_sessions(active: &mut HashMap<String, ActiveSession>) {
    for (_, session) in active.drain() {
        #[cfg(feature = "media")]
        session.media_task.stop().await;
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
                failed_since,
                media_task,
                media_restarts,
            } = session;
            let detail = match media_task.finish().await {
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
                    if let Err(publish_error) = crate::rtc::report_negotiation_failure(
                        client,
                        token.as_ref(),
                        &session.session_id,
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
