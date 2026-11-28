use std::{
    collections::{HashMap, HashSet},
    io,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use anyhow::{Result, bail};
use axum::{
    Extension, Router,
    extract::{
        ConnectInfo, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use futures_util::SinkExt;
use serde_json::{Value, json};
use tokio::sync::{Mutex, RwLock, watch};

use crate::{
    config::AgentConfig,
    tailnet::{self, Identity, Node, Tailnet},
};

include!(concat!(env!("OUT_DIR"), "/viewer.rs"));

type ActiveSession = (watch::Sender<String>, tokio::task::JoinHandle<()>);

async fn stop_session((stop, task): ActiveSession, reason: &'static str) {
    stop.send_replace(reason.into());
    match tokio::time::timeout(Duration::from_secs(5), task).await {
        Ok(Err(error)) => tracing::warn!(%error, reason, "viewer session task failed"),
        Err(_) => tracing::warn!(reason, "viewer session teardown timed out"),
        Ok(Ok(())) => {}
    }
}

pub struct ServerState {
    hosts: Mutex<Option<(std::time::Instant, Vec<tailnet::Peer>)>>,
    pub config: AgentConfig,
    tailnet: Arc<dyn Tailnet>,
    node: RwLock<Node>,
    active: Mutex<Option<ActiveSession>>,
    stopping: std::sync::atomic::AtomicBool,
}

impl ServerState {
    pub fn new(config: AgentConfig, tailnet: Arc<dyn Tailnet>) -> Arc<Self> {
        if dev_user().is_some() {
            tracing::warn!(target: "nanoctl::status",
                "DANGER: NANOCTL_DEV_LOOPBACK_USER enabled: loopback authentication and loopback ICE are trusted. DEVELOPMENT ONLY."
            );
        }
        #[cfg(feature = "rtc")]
        crate::codec::set_host_preference(config.quality.codec);
        #[cfg(all(feature = "media", not(test)))]
        {
            let preference = config.quality.encoder;
            std::thread::spawn(move || crate::media::ensure_codecs_probed(preference));
        }
        Arc::new(Self {
            hosts: Mutex::new(None),
            config,
            tailnet,
            node: RwLock::new(Node::default()),
            active: Mutex::new(None),
            stopping: std::sync::atomic::AtomicBool::new(false),
        })
    }

    pub async fn stop(&self) {
        self.stopping
            .store(true, std::sync::atomic::Ordering::Release);
        if let Some(active) = self.active.lock().await.take() {
            stop_session(active, "shutdown").await;
        }
    }

    async fn info(&self, identity: &Identity, control: bool) -> Value {
        let node = self.node.read().await;
        let capabilities = serde_json::to_value(crate::platform::capabilities(
            self.config.features.remote_input && control,
            self.config.features.clipboard && self.config.features.remote_input && control,
            self.config.features.system_audio,
            self.config.quality.encoder,
        ))
        .unwrap();
        json!({
            "hostName": host_display_name(&node), "platform":crate::platform::PLATFORM,
            "version":env!("CARGO_PKG_VERSION"), "displays":capabilities["displays"],
            "remoteInput": capabilities["remoteInput"], "capabilities":capabilities,
            "viewer":{"identity":identity,"mayControl":control}
        })
    }

    async fn authenticate(
        &self,
        peer: SocketAddr,
        headers: &HeaderMap,
        ws: bool,
        tls: bool,
    ) -> Result<(Identity, bool)> {
        let node = self.node.read().await.clone();
        let identity = if peer.ip().is_loopback() {
            let login = if self.config.tailnet.serve_loopback {
                headers
                    .get("Tailscale-User-Login")
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned)
            } else {
                None
            }
            .or_else(dev_user);
            let Some(login) = login.filter(|login| !login.is_empty()) else {
                bail!("identity=anonymous: loopback requires tailscale serve identity");
            };
            Identity {
                login,
                ..Default::default()
            }
        } else {
            if !tailnet::is_tailnet_ip(peer.ip()) {
                bail!("identity=anonymous: not a tailnet address");
            }
            self.tailnet
                .whois(peer)
                .await
                .map_err(|_| anyhow::anyhow!("identity=unknown: tailscale whois failed"))?
        };
        let label = format!("identity={} tags={:?}", identity.login, identity.tags);
        if !valid_host(headers, &node, peer.ip().is_loopback()) {
            bail!("{label}: invalid Host");
        }
        let serve = peer.ip().is_loopback()
            && self.config.tailnet.serve_loopback
            && headers.get("Tailscale-User-Login").is_some();
        if ws && !valid_origin(headers, tls, serve) {
            bail!("{label}: invalid WebSocket Origin");
        }
        let control = tailnet::authorize(&self.config.tailnet, &node, &identity)
            .or_else(|| {
                if peer.ip().is_loopback() && dev_user().as_deref() == Some(identity.login.as_str())
                {
                    Some(true)
                } else {
                    None
                }
            })
            .ok_or_else(|| anyhow::anyhow!("{label}: access denied by tailnet policy"))?;
        Ok((identity, control))
    }
}

pub fn dev_user() -> Option<String> {
    std::env::var("NANOCTL_DEV_LOOPBACK_USER")
        .ok()
        .filter(|user| !user.is_empty())
}

pub fn valid_host(headers: &HeaderMap, node: &Node, loopback: bool) -> bool {
    let Some(host) = headers
        .get(header::HOST)
        .and_then(|host| host.to_str().ok())
    else {
        return false;
    };
    let Ok(authority) = host.parse::<axum::http::uri::Authority>() else {
        return false;
    };
    if authority.as_str().contains('@') {
        return false;
    }
    let name = authority
        .host()
        .trim_start_matches('[')
        .trim_end_matches(']');
    if let Ok(ip) = name.parse::<IpAddr>() {
        return node.ips.contains(&ip) || (loopback && ip.is_loopback());
    }
    (!node.dns_name.is_empty() && name.eq_ignore_ascii_case(&node.dns_name))
        || (!node.host_name.is_empty() && name.eq_ignore_ascii_case(&node.host_name))
        || (loopback && name.eq_ignore_ascii_case("localhost"))
}

pub fn valid_origin(headers: &HeaderMap, tls: bool, serve: bool) -> bool {
    let Some(host) = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let Some(origin) = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let scheme = if tls { "https" } else { "http" };
    origin == format!("{scheme}://{host}")
        || (serve && (origin == format!("https://{host}") || origin == format!("http://{host}")))
}

pub fn router(state: Arc<ServerState>, tls: bool) -> Router {
    Router::new()
        .route("/v1/info", get(info))
        .route("/v1/debug", get(debug_report))
        .route("/v1/hosts", get(hosts))
        .route("/v1/connect", get(connect))
        .fallback(get(asset))
        .with_state(state)
        .layer(Extension(tls))
        .layer(middleware::from_fn(security_headers))
}

async fn security_headers(request: axum::extract::Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, "nosniff".parse().unwrap());
    headers.insert(header::X_FRAME_OPTIONS, "DENY".parse().unwrap());
    headers.insert(header::CONTENT_SECURITY_POLICY, "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'".parse().unwrap());
    headers.insert(header::REFERRER_POLICY, "no-referrer".parse().unwrap());
    response
}

fn forbidden(error: anyhow::Error) -> Response {
    (StatusCode::FORBIDDEN, format!("{error}\n")).into_response()
}

fn host_url(node: &Node, port: u16, tls: bool) -> String {
    let url = tailnet::urls(node, port)
        .into_iter()
        .next()
        .unwrap_or_default();
    if tls {
        url.replacen("http://", "https://", 1)
    } else {
        url
    }
}

fn host_display_name(node: &Node) -> String {
    if !node.host_name.is_empty() {
        return node.host_name.clone();
    }
    hostname::get()
        .ok()
        .and_then(|name| name.into_string().ok())
        .unwrap_or_else(|| "nanoctl host".into())
}

fn self_host(node: &Node, port: u16, tls: bool, loopback: bool) -> Value {
    let mut url = host_url(node, port, tls);
    if url.is_empty() && loopback {
        let scheme = if tls { "https" } else { "http" };
        url = format!("{scheme}://127.0.0.1:{port}/");
    }
    json!({
        "hostName": host_display_name(node),
        "dnsName": node.dns_name,
        "os": crate::platform::PLATFORM,
        "online": true,
        "url": url,
    })
}

async fn probe_peer(peer: tailnet::Peer, port: u16) -> Option<tailnet::Peer> {
    if !peer.online {
        return None;
    }
    let ip = *peer.ips.first()?;
    matches!(
        tokio::time::timeout(
            Duration::from_millis(400),
            tokio::net::TcpStream::connect(SocketAddr::new(ip, port))
        )
        .await,
        Ok(Ok(_))
    )
    .then_some(peer)
}

async fn hosts(
    State(state): State<Arc<ServerState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(tls): Extension<bool>,
    headers: HeaderMap,
) -> Response {
    if let Err(error) = state.authenticate(peer, &headers, false, tls).await {
        return forbidden(error);
    }
    let node = state.node.read().await.clone();
    let mut discovered = Vec::new();
    if state.config.tailnet.discover_peers {
        let mut cache = state.hosts.lock().await;
        if cache
            .as_ref()
            .is_none_or(|(time, _)| time.elapsed() >= Duration::from_secs(15))
        {
            let peers = state.tailnet.peers().await.unwrap_or_default();
            let probes = peers
                .into_iter()
                .filter(|p| {
                    !p.ips.iter().any(|ip| node.ips.contains(ip))
                        && (node.dns_name.is_empty() || p.dns_name != node.dns_name)
                })
                .map(|p| probe_peer(p, state.config.tailnet.port));
            let mut peers: Vec<_> = futures_util::future::join_all(probes)
                .await
                .into_iter()
                .flatten()
                .collect();
            peers.sort_by(|a, b| a.host_name.cmp(&b.host_name));
            *cache = Some((std::time::Instant::now(), peers));
        }
        discovered = cache.as_ref().unwrap().1.clone();
    }
    let hosts: Vec<_> = discovered
        .into_iter()
        .map(|p| {
            let url = host_url(
                &Node {
                    dns_name: p.dns_name.clone(),
                    ips: p.ips,
                    ..Default::default()
                },
                state.config.tailnet.port,
                tls,
            );
            json!({
                "hostName": p.host_name,
                "dnsName": p.dns_name,
                "os": p.os,
                "online": p.online,
                "url": url,
            })
        })
        .collect();
    axum::Json(json!({
        "self": self_host(&node, state.config.tailnet.port, tls, peer.ip().is_loopback()),
        "hosts": hosts,
    }))
    .into_response()
}

async fn info(
    State(state): State<Arc<ServerState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(tls): Extension<bool>,
    headers: HeaderMap,
) -> Response {
    match state.authenticate(peer, &headers, false, tls).await {
        Ok((identity, control)) => axum::Json(state.info(&identity, control).await).into_response(),
        Err(error) => forbidden(error),
    }
}

/// The host half of the viewer's "Copy debug info", limited to viewers that may control.
async fn debug_report(
    State(state): State<Arc<ServerState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(tls): Extension<bool>,
    headers: HeaderMap,
) -> Response {
    match state.authenticate(peer, &headers, false, tls).await {
        Ok((_, true)) => (
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            crate::diagnostics::report(&state.config, None, crate::diagnostics::REPORT_LOG_LINES)
                .await,
        )
            .into_response(),
        Ok((identity, false)) => forbidden(anyhow::anyhow!(
            "identity={}: the debug report requires control permission",
            identity.login
        )),
        Err(error) => forbidden(error),
    }
}

async fn asset(
    State(state): State<Arc<ServerState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(tls): Extension<bool>,
    headers: HeaderMap,
    uri: axum::http::Uri,
) -> Response {
    if let Err(error) = state.authenticate(peer, &headers, false, tls).await {
        return forbidden(error);
    }
    let path = if uri.path() == "/" {
        "/index.html"
    } else {
        uri.path()
    };
    if let Some((_, bytes)) = ASSETS.iter().find(|(name, _)| *name == path) {
        let mime = match path.rsplit('.').next() {
            Some("html") => "text/html; charset=utf-8",
            Some("js") => "text/javascript",
            Some("css") => "text/css",
            Some("svg") => "image/svg+xml",
            Some("png") => "image/png",
            Some("ico") => "image/x-icon",
            Some("json") => "application/json",
            Some("webmanifest") => "application/manifest+json",
            Some("woff2") => "font/woff2",
            _ => "application/octet-stream",
        };
        return ([(header::CONTENT_TYPE, mime)], *bytes).into_response();
    }
    if path == "/index.html" {
        return axum::response::Html("<!doctype html><title>nanoctl</title><h1>nanoctl</h1><p>The viewer wasn't built. Build apps/viewer/dist and rebuild the agent.</p>").into_response();
    }
    StatusCode::NOT_FOUND.into_response()
}

async fn connect(
    State(state): State<Arc<ServerState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(tls): Extension<bool>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let (identity, control) = match state.authenticate(peer, &headers, true, tls).await {
        Ok(result) => result,
        Err(error) => return forbidden(error),
    };
    ws.max_message_size(64 * 1024)
        .max_frame_size(64 * 1024)
        .on_upgrade(move |socket| async move {
            let mut active = state.active.lock().await;
            if state.stopping.load(std::sync::atomic::Ordering::Acquire) {
                return;
            }
            if let Some(previous) = active.take() {
                // Do not await the old session's teardown here: it can take
                // seconds or hang on a dead transport, and this lock is held
                // for the whole wait — so every reconnect wedged before the
                // new session even existed (no hello, no ICE, exactly the
                // field failure). The stop signal already started its exit;
                // let it finish in the background.
                tokio::spawn(stop_session(previous, "replaced"));
            }
            let (stop, receiver) = watch::channel(String::new());
            let session_state = state.clone();
            *active = Some((
                stop,
                tokio::spawn(async move {
                    session(socket, session_state, identity, control, receiver).await;
                }),
            ));
        })
        .into_response()
}

async fn send(socket: &mut WebSocket, value: Value) -> Result<()> {
    tokio::time::timeout(
        Duration::from_secs(3),
        socket.send(Message::Text(value.to_string().into())),
    )
    .await??;
    Ok(())
}

async fn session(
    mut socket: WebSocket,
    state: Arc<ServerState>,
    identity: Identity,
    control: bool,
    mut stop: watch::Receiver<String>,
) {
    let id = uuid::Uuid::new_v4().to_string();
    let info = state.info(&identity, control).await;
    if send(
        &mut socket,
        json!({"type":"hello","sessionId":id,"host":info}),
    )
    .await
    .is_err()
    {
        return;
    }
    tracing::info!(target: "nanoctl::status", "Viewer connected");
    #[cfg(feature = "media")]
    {
        // Seed the replay window while signaling runs: when it wins the race
        // to the channel attach the very first join is as fast as a rejoin.
        let (bitrate, fps, width, height, mode, encoder) = (
            state.config.quality.max_bitrate_kbps,
            crate::media::effective_max_fps(state.config.quality.max_fps),
            state.config.quality.max_width,
            state.config.quality.max_height,
            state.config.quality.latency_mode,
            state.config.quality.encoder,
        );
        tokio::task::spawn_blocking(move || {
            if let Err(error) =
                crate::media::prime_replay(bitrate, fps, width, height, mode, encoder)
            {
                tracing::debug!(%error, "video replay warmup failed");
            }
        });
    }
    let (outgoing, mut messages) = tokio::sync::mpsc::channel::<Value>(64);
    #[cfg(feature = "rtc")]
    let (audio_output, _) = watch::channel(None);
    #[cfg(all(feature = "media", target_os = "windows"))]
    let mut audio_selection = state
        .config
        .features
        .system_audio
        .then(crate::audio::OutputSelection::default);
    #[cfg(all(feature = "media", target_os = "windows"))]
    let mut last_audio_message = None;
    #[cfg(all(feature = "media", target_os = "windows"))]
    let mut reports_audio_outputs = false;
    #[cfg(feature = "rtc")]
    let mut peer: Option<crate::rtc::HostPeer> = None;
    #[cfg(feature = "media")]
    let mut media: Option<crate::media::VideoPipeline> = None;
    #[cfg(feature = "media")]
    let mut media_restarts = 0;
    #[cfg(feature = "rtc")]
    let mut last_sequence = None;
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    #[cfg(feature = "rtc")]
    let mut failed_since: Option<tokio::time::Instant> = None;
    let reason = loop {
        tokio::select! {
            _ = stop.changed() => { break stop.borrow().clone(); }
            message = messages.recv() => {
                if let Some(message) = message && send(&mut socket, message).await.is_err() { break "disconnected".into(); }
            }
            _ = tick.tick() => {
                #[cfg(all(feature = "media", target_os = "windows"))]
                if let Some(selection) = &mut audio_selection {
                    selection.refresh(crate::audio::outputs());
                    let message = audio_output_state(selection, &audio_output);
                    if reports_audio_outputs && last_audio_message.as_ref() != Some(&message) {
                        if send(&mut socket, message.clone()).await.is_err() { break "disconnected".into(); }
                        last_audio_message = Some(message);
                    }
                }
                #[cfg(feature = "media")]
                if media.as_ref().is_some_and(|media| media.is_finished()) {
                    if let Some(finished) = media.take() && let Err(error) = finished.finish().await {
                        if crate::media::is_video_delivery_unavailable(&error) { break "media-failed".into(); }
                        tracing::warn!(%error, "media pipeline failed");
                    }
                    if media_restarts >= 2 { break "media-failed".into(); }
                    if let Some(peer) = &peer { media = Some(crate::service::spawn_media(peer, &state.config)); }
                    media_restarts += 1;
                }
                #[cfg(feature = "rtc")]
                if let Some(peer) = &peer {
                    if peer.connection_failed() {
                        if failed_since.get_or_insert_with(tokio::time::Instant::now).elapsed() >= Duration::from_secs(15) { break "connection-failed".into(); }
                    } else { failed_since = None; }
                }
            }
            message = socket.recv() => {
                let text = match message {
                    Some(Ok(Message::Text(text))) => text,
                    Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => continue,
                    _ => break "disconnected".into(),
                };
                let message: Value = match serde_json::from_str(&text) { Ok(value) => value, Err(_) => break "invalid-message".into() };
                if message["type"] == "end" { break "viewer-ended".into(); }
                #[cfg(all(feature = "media", target_os = "windows"))]
                if message["type"] == "select-audio-output" {
                    if let Some(selection) = &mut audio_selection {
                        let requested = match message.get("outputId") {
                            Some(Value::Null) => None,
                            Some(Value::String(id)) => Some(id.as_str()),
                            _ => break "invalid-message".into(),
                        };
                        selection.refresh(crate::audio::outputs());
                        selection.select(requested);
                        reports_audio_outputs = true;
                        let message = audio_output_state(selection, &audio_output);
                        if send(&mut socket, message.clone()).await.is_err() { break "disconnected".into(); }
                        last_audio_message = Some(message);
                    }
                    continue;
                }
                if message["type"] != "signal" || !message["envelope"].is_object() { break "invalid-message".into(); }
                #[cfg(not(feature = "rtc"))]
                { let _ = &outgoing; break "rtc-unavailable".into(); }
                #[cfg(feature = "rtc")]
                {
                    let envelope = &message["envelope"];
                    let Some(sequence) = envelope["sequence"].as_u64() else { break "invalid-signal".into(); };
                    if last_sequence.is_some_and(|last| sequence <= last) { continue; }
                    let serialized = envelope.to_string();
                    let result = if let Some(peer) = &peer {
                        peer.add_signal(&serialized, &id, sequence).await
                    } else if crate::rtc::is_offer_for_session(&serialized, &id) {
                        match crate::rtc::HostPeer::answer_with_host_epoch(
                            outgoing.clone(), id.clone(), &serialized, Vec::new(),
                            webrtc::peer_connection::policy::ice_transport_policy::RTCIceTransportPolicy::All,
                            crate::rtc::host_epoch_seed(), state.config.features.remote_input && control,
                            state.config.features.clipboard && state.config.features.remote_input && control,
                            state.config.features.system_audio.then(|| audio_output.subscribe())).await {
                            Ok(new_peer) => {
                                #[cfg(feature = "media")]
                                { media = Some(crate::service::spawn_media(&new_peer, &state.config)); }
                                peer = Some(new_peer);
                                Ok(true)
                            }
                            Err(error) => Err(error)
                        }
                    } else { Err(anyhow::anyhow!("first signal must be an offer")) };
                    match result { Ok(true) => { last_sequence = Some(sequence); }, Ok(false) => break "viewer-ended".into(), Err(error) => {
                        tracing::warn!(%error, "signaling failed"); break "invalid-signal".into();
                    }}
                }
            }
        }
    };
    #[cfg(feature = "media")]
    if let Some(peer) = &peer {
        peer.revoke_input();
    }
    #[cfg(feature = "media")]
    if let Some(media) = media {
        media.stop().await;
    }
    #[cfg(feature = "rtc")]
    if let Some(peer) = peer {
        if reason == "connection-failed" || reason == "media-failed" {
            let _ = peer.fail(&reason).await;
        }
        if let Err(error) = peer.close().await {
            tracing::warn!(%error, reason, "WebRTC peer teardown failed");
        }
        drop(peer);
        #[cfg(feature = "media")]
        crate::media::release_session_memory().await;
    }
    let _ = send(&mut socket, json!({"type":"end","reason":reason})).await;
    let _ = tokio::time::timeout(Duration::from_secs(1), socket.close()).await;
    tracing::info!(target: "nanoctl::status", %reason, "Viewer disconnected");
}

#[cfg(all(feature = "media", target_os = "windows"))]
fn audio_output_state(
    selection: &crate::audio::OutputSelection,
    output: &watch::Sender<Option<String>>,
) -> Value {
    let effective = selection.effective();
    output.send_if_modified(|current| {
        if *current == effective {
            return false;
        }
        *current = effective;
        true
    });
    json!({"type":"audio-outputs","outputs":selection.outputs,"selectedId":selection.selected})
}

async fn bind_listener(
    addr: SocketAddr,
    ipv4_ready: bool,
    unavailable_ipv6: &mut HashSet<IpAddr>,
    bind: impl AsyncFnOnce(SocketAddr) -> io::Result<tokio::net::TcpListener>,
) -> Option<tokio::net::TcpListener> {
    let ip = addr.ip();
    let can_skip = ip.is_ipv6() && ipv4_ready;
    if !can_skip {
        unavailable_ipv6.remove(&ip);
    } else if unavailable_ipv6.contains(&ip) {
        return None;
    }
    match bind(addr).await {
        Ok(listener) => Some(listener),
        Err(error) => {
            if can_skip && error.kind() == io::ErrorKind::AddrNotAvailable {
                unavailable_ipv6.insert(ip);
                tracing::info!(%addr, %error, "skipping unavailable IPv6 listener; IPv4 is ready");
            } else {
                tracing::warn!(%addr, %error, "cannot bind tailnet listener; will retry");
            }
            None
        }
    }
}

pub async fn serve(state: Arc<ServerState>, mut shutdown: watch::Receiver<bool>) -> Result<()> {
    let mut listeners =
        HashMap::<IpAddr, (axum_server::Handle<SocketAddr>, tokio::task::JoinHandle<()>)>::new();
    let mut unavailable_ipv6 = HashSet::new();
    let mut tls_config: Option<axum_server::tls_rustls::RustlsConfig> = None;
    let mut cert_node = String::new();
    let mut cert_checked = None::<tokio::time::Instant>;
    let mut announced_url = None;
    let mut waiting_for_tailscale = false;
    let mut waiting_for_listener = false;
    let mut interval = tokio::time::interval(Duration::from_secs(3));
    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            _ = interval.tick() => {}
        }
        let mut wanted = HashSet::new();
        match state.tailnet.status().await {
            Ok(node) => {
                waiting_for_tailscale = false;
                wanted.extend(node.ips.iter().copied());
                if cert_node != node.dns_name
                    || cert_checked.is_none_or(|checked| {
                        checked.elapsed() >= Duration::from_secs(24 * 60 * 60)
                    })
                {
                    let result = tokio::select! {
                        result = crate::tls::prepare(&state.config.tailnet, &node, state.tailnet.as_ref()) => result,
                        _ = shutdown.changed() => break,
                    };
                    if result.config.is_none()
                        && state.config.tailnet.tls != crate::config::TlsMode::Off
                    {
                        tracing::warn!(target: "nanoctl::status", "{}", result.detail);
                    }
                    match (&tls_config, result.config) {
                        (Some(current), Some(config)) => current.reload_from_config(config),
                        (_, next) => {
                            let changed = tls_config.is_some() != next.is_some();
                            tls_config =
                                next.map(axum_server::tls_rustls::RustlsConfig::from_config);
                            if changed {
                                for (_, (stop, task)) in listeners.drain() {
                                    stop.graceful_shutdown(Some(Duration::from_secs(5)));
                                    let _ = task.await;
                                }
                            }
                        }
                    }
                    cert_node = node.dns_name.clone();
                    cert_checked = Some(tokio::time::Instant::now());
                }
                *state.node.write().await = node;
            }
            Err(error) => {
                if waiting_for_tailscale {
                    tracing::warn!(%error, "Waiting for Tailscale");
                } else {
                    tracing::warn!(target: "nanoctl::status", %error, "Waiting for Tailscale");
                    waiting_for_tailscale = true;
                }
                *state.node.write().await = Node::default();
            }
        }
        if state.config.tailnet.serve_loopback || dev_user().is_some() {
            wanted.insert("127.0.0.1".parse().unwrap());
            wanted.insert("::1".parse().unwrap());
        }
        let old = listeners
            .keys()
            .copied()
            .filter(|ip| !wanted.contains(ip))
            .collect::<Vec<_>>();
        for ip in old {
            if let Some((stop, task)) = listeners.remove(&ip) {
                stop.graceful_shutdown(Some(Duration::from_secs(5)));
                let _ = task.await;
            }
        }
        unavailable_ipv6.retain(|ip| wanted.contains(ip));
        let mut wanted = wanted.into_iter().collect::<Vec<_>>();
        wanted.sort_unstable_by_key(IpAddr::is_ipv6);
        let needs_listener = !wanted.is_empty();
        for ip in wanted {
            if listeners.contains_key(&ip) {
                continue;
            }
            let addr = SocketAddr::new(ip, state.config.tailnet.port);
            let ipv4_ready = ip.is_ipv6()
                && listeners.iter().any(|(other, (_, task))| {
                    other.is_ipv4()
                        && other.is_loopback() == ip.is_loopback()
                        && !task.is_finished()
                });
            let Some(listener) =
                bind_listener(addr, ipv4_ready, &mut unavailable_ipv6, async |addr| {
                    tokio::net::TcpListener::bind(addr).await
                })
                .await
            else {
                continue;
            };
            let tls = if ip.is_loopback() {
                None
            } else {
                tls_config.clone()
            };
            tracing::info!(%addr, tls = tls.is_some(), "tailnet listener ready");
            let app = router(state.clone(), tls.is_some());
            let stop = axum_server::Handle::new();
            let handle = stop.clone();
            let listener = listener.into_std()?;
            let task = tokio::spawn(async move {
                let service = app.into_make_service_with_connect_info::<SocketAddr>();
                let result: io::Result<()> = async {
                    if let Some(tls) = tls {
                        axum_server::from_tcp_rustls(listener, tls)?
                            .handle(handle)
                            .serve(service)
                            .await
                    } else {
                        axum_server::from_tcp(listener)?
                            .handle(handle)
                            .serve(service)
                            .await
                    }
                }
                .await;
                if let Err(error) = result {
                    tracing::warn!(%addr, %error, "listener stopped");
                }
            });
            listeners.insert(ip, (stop, task));
        }
        let retrying_bind = needs_listener && listeners.is_empty();
        if retrying_bind && !waiting_for_listener {
            tracing::warn!(target: "nanoctl::status", port = state.config.tailnet.port, "Could not open a viewer listener; retrying");
        }
        waiting_for_listener = retrying_bind;
        let node = state.node.read().await;
        let url = node
            .ips
            .iter()
            .any(|ip| listeners.contains_key(ip))
            .then(|| crate::tls::viewer_url(&node, state.config.tailnet.port, tls_config.is_some()))
            .flatten();
        if url != announced_url {
            if let Some(url) = &url {
                tracing::info!(target: "nanoctl::status", "Open on a device connected to Tailscale: {url}");
            }
            announced_url = url;
        }
    }
    for (_, (stop, task)) in listeners {
        stop.graceful_shutdown(Some(Duration::from_secs(5)));
        let _ = task.await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unavailable_ipv6_is_skipped_until_ipv4_is_lost() {
        let addr = "[fd7a:115c:a1e0::1234]:7337".parse().unwrap();
        let mut unavailable = HashSet::new();
        let mut attempts = 0;
        for ipv4_ready in [true, true, false, false, true, true] {
            let listener = bind_listener(addr, ipv4_ready, &mut unavailable, async |_| {
                attempts += 1;
                Err(io::ErrorKind::AddrNotAvailable.into())
            })
            .await;
            assert!(listener.is_none());
            assert_eq!(unavailable.contains(&addr.ip()), ipv4_ready);
        }
        assert_eq!(attempts, 4);
    }

    #[tokio::test]
    async fn transient_and_ipv4_bind_errors_are_retried() {
        for (addr, error) in [
            ("[fd7a:115c:a1e0::1234]:7337", io::ErrorKind::AddrInUse),
            ("100.102.202.1:7337", io::ErrorKind::AddrNotAvailable),
        ] {
            let addr = addr.parse().unwrap();
            let mut unavailable = HashSet::new();
            let mut attempts = 0;
            for _ in 0..2 {
                let listener = bind_listener(addr, true, &mut unavailable, async |_| {
                    attempts += 1;
                    Err(error.into())
                })
                .await;
                assert!(listener.is_none());
            }
            assert!(unavailable.is_empty());
            assert_eq!(attempts, 2);
        }
    }

    #[tokio::test]
    async fn peer_probe_connects_and_skips_offline() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut peer = crate::tailnet::Peer {
            ips: vec!["127.0.0.1".parse().unwrap()],
            online: true,
            ..Default::default()
        };
        assert!(super::probe_peer(peer.clone(), port).await.is_some());
        peer.online = false;
        assert!(super::probe_peer(peer.clone(), port).await.is_none());
        peer.online = true;
        let closed_port = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        assert!(super::probe_peer(peer, closed_port).await.is_none());
    }
    #[test]
    fn self_host_has_stable_identity_fields_and_loopback_fallback() {
        let empty = Node::default();
        let host = self_host(&empty, 7337, false, true);
        assert_eq!(host["url"], "http://127.0.0.1:7337/");
        assert_eq!(host["online"], true);
        assert_eq!(host["os"], crate::platform::PLATFORM);
        assert!(!host["hostName"].as_str().unwrap().is_empty());

        let node = Node {
            ips: vec!["100.64.1.2".parse().unwrap()],
            dns_name: "host.tailnet.ts.net".into(),
            host_name: "host".into(),
            ..Default::default()
        };
        let host = self_host(&node, 7337, false, true);
        assert_eq!(host["url"], "http://host.tailnet.ts.net:7337/");
        assert_eq!(host["hostName"], "host");
    }

    #[tokio::test]
    async fn view_only_info_never_advertises_clipboard() {
        for (remote_input, control) in [(false, true), (true, false), (false, false)] {
            let mut config = AgentConfig::default();
            config.features.remote_input = remote_input;
            assert!(config.features.clipboard);
            let state = ServerState::new(
                config,
                Arc::new(crate::tailnet::FakeTailnet {
                    node: Node::default(),
                    identity: Identity::default(),
                }),
            );
            let info = state.info(&Identity::default(), control).await;
            assert_eq!(info["capabilities"]["clipboard"], false);
            assert_eq!(info["remoteInput"], false);
        }
    }

    #[cfg(feature = "rtc")]
    #[tokio::test]
    async fn websocket_offer_answer_and_replacement() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::{Message as WsMessage, client::IntoClientRequest};
        use webrtc::{
            api::{APIBuilder, media_engine::MediaEngine},
            peer_connection::{
                configuration::RTCConfiguration, sdp::session_description::RTCSessionDescription,
            },
            rtp_transceiver::rtp_codec::RTPCodecType,
        };
        let node = Node {
            ips: vec!["100.64.1.2".parse().unwrap()],
            owner: "owner@example.com".into(),
            ..Default::default()
        };
        let fake = Arc::new(crate::tailnet::FakeTailnet {
            node: node.clone(),
            identity: Identity {
                login: node.owner.clone(),
                ..Default::default()
            },
        });
        let mut config = AgentConfig::default();
        config.tailnet.serve_loopback = true;
        let state = ServerState::new(config, fake);
        *state.node.write().await = state.tailnet.status().await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = router(state.clone(), false);
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        let client = reqwest::Client::new();
        let denied = client
            .get(format!("http://{addr}/v1/info"))
            .send()
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
        assert!(denied.text().await.unwrap().contains("identity=anonymous"));
        let info: Value = client
            .get(format!("http://{addr}/v1/info"))
            .header("Tailscale-User-Login", "owner@example.com")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(info["viewer"]["mayControl"], true);
        let denied = client
            .get(format!("http://{addr}/v1/hosts"))
            .send()
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
        let hosts: Value = client
            .get(format!("http://{addr}/v1/hosts"))
            .header("Tailscale-User-Login", "owner@example.com")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(hosts["hosts"], json!([]));
        assert_eq!(hosts["self"]["url"], "http://100.64.1.2:7337/");
        assert_eq!(hosts["self"]["online"], true);
        assert_eq!(hosts["self"]["os"], crate::platform::PLATFORM);
        assert!(hosts["self"]["hostName"].is_string());
        let request = || {
            let mut request = format!("ws://{addr}/v1/connect")
                .into_client_request()
                .unwrap();
            request
                .headers_mut()
                .insert("Origin", format!("http://{addr}").parse().unwrap());
            request
                .headers_mut()
                .insert("Tailscale-User-Login", "owner@example.com".parse().unwrap());
            request
        };
        let (mut socket, _) = tokio_tungstenite::connect_async(request()).await.unwrap();
        let hello: Value =
            serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(hello["type"], "hello");
        let id = hello["sessionId"].as_str().unwrap();
        let mut engine = MediaEngine::default();
        engine.register_default_codecs().unwrap();
        let controller = APIBuilder::new()
            .with_media_engine(engine)
            .build()
            .new_peer_connection(RTCConfiguration::default())
            .await
            .unwrap();
        controller
            .add_transceiver_from_kind(RTPCodecType::Video, None)
            .await
            .unwrap();
        let offer = controller.create_offer(None).await.unwrap();
        controller
            .set_local_description(offer.clone())
            .await
            .unwrap();
        socket.send(WsMessage::Text(json!({"type":"signal","envelope":{
            "version":2,"sessionId":id,"controllerEpoch":"browser-epoch","hostEpoch":"","generation":0,
            "sequence":1,"sender":"controller","sentAt":1,"payload":{"type":"offer","sdp":offer.sdp}
        }}).to_string().into())).await.unwrap();
        let answer = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let message: Value =
                    serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap())
                        .unwrap();
                if message["envelope"]["payload"]["type"] == "answer" {
                    break message;
                }
                assert_ne!(message["type"], "end", "{message}");
            }
        })
        .await
        .unwrap();
        assert_eq!(answer["envelope"]["sessionId"], id);
        controller
            .set_remote_description(
                RTCSessionDescription::answer(
                    answer["envelope"]["payload"]["sdp"]
                        .as_str()
                        .unwrap()
                        .into(),
                )
                .unwrap(),
            )
            .await
            .unwrap();
        let (mut second, _) = tokio_tungstenite::connect_async(request()).await.unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let message: Value =
                    serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap())
                        .unwrap();
                if message["type"] == "end" {
                    assert_eq!(message["reason"], "replaced");
                    break;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(second.next().await.unwrap().unwrap().to_text().unwrap())
                .unwrap()["type"],
            "hello"
        );
        {
            let mut active = state.active.lock().await;
            let (stop, task) = active.take().unwrap();
            *active = Some((
                stop,
                tokio::spawn(async move {
                    task.await.unwrap();
                    panic!("injected viewer teardown panic");
                }),
            ));
        }
        second.close(None).await.unwrap();
        let (mut third, _) = tokio_tungstenite::connect_async(request()).await.unwrap();
        let hello = tokio::time::timeout(Duration::from_secs(10), third.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(hello.to_text().unwrap()).unwrap()["type"],
            "hello"
        );
        state.stop().await;
        controller.close().await.unwrap();
        server.abort();
    }

    #[test]
    fn host_and_origin() {
        let node = Node {
            ips: vec![
                "100.64.1.2".parse().unwrap(),
                "fd7a:115c:a1e0::1".parse().unwrap(),
            ],
            dns_name: "host.tail.ts.net".into(),
            host_name: "host".into(),
            ..Default::default()
        };
        let mut headers = HeaderMap::new();
        assert!(!valid_host(&headers, &node, false));
        for host in [
            "100.64.1.2:7337",
            "[fd7a:115c:a1e0::1]:7337",
            "host.tail.ts.net:7337",
            "host:7337",
        ] {
            headers.insert(header::HOST, host.parse().unwrap());
            headers.insert(header::ORIGIN, format!("http://{host}").parse().unwrap());
            assert!(valid_host(&headers, &node, false));
            assert!(valid_origin(&headers, false, false));
        }
        headers.insert(header::ORIGIN, "http://evil.test".parse().unwrap());
        assert!(!valid_origin(&headers, false, false));
        headers.insert(header::HOST, "evil.test:7337".parse().unwrap());
        assert!(!valid_host(&headers, &node, false));
        headers.insert(header::HOST, "127.0.0.1:7337".parse().unwrap());
        assert!(!valid_host(&headers, &node, false));
        assert!(valid_host(&headers, &node, true));
        headers.insert(header::ORIGIN, "https://127.0.0.1:7337".parse().unwrap());
        headers.insert("X-Forwarded-Proto", "https".parse().unwrap());
        assert!(valid_origin(&headers, true, false));
        assert!(!valid_origin(&headers, false, false));
        assert!(valid_origin(&headers, false, true));
        headers.insert(header::ORIGIN, "http://127.0.0.1:7337".parse().unwrap());
        assert!(valid_origin(&headers, false, true));
        assert!(!valid_origin(&headers, true, false));
        headers.insert(header::ORIGIN, "https://evil.test".parse().unwrap());
        assert!(!valid_origin(&headers, true, true));
    }

    #[tokio::test]
    async fn default_rejects_forged_loopback_header() {
        assert!(!AgentConfig::default().tailnet.serve_loopback);
        let fake = Arc::new(crate::tailnet::FakeTailnet {
            node: Node::default(),
            identity: Identity::default(),
        });
        let state = ServerState::new(AgentConfig::default(), fake);
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "localhost:7337".parse().unwrap());
        headers.insert("Tailscale-User-Login", "owner@example.com".parse().unwrap());
        let error = state
            .authenticate("127.0.0.1:12345".parse().unwrap(), &headers, false, false)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("loopback requires"));
    }
}
