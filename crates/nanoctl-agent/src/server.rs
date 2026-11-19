use std::{
    collections::{HashMap, HashSet},
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

type Active = Option<(watch::Sender<String>, tokio::task::JoinHandle<()>)>;

pub struct ServerState {
    hosts: Mutex<Option<(std::time::Instant, Vec<tailnet::Peer>)>>,
    pub config: AgentConfig,
    tailnet: Arc<dyn Tailnet>,
    node: RwLock<Node>,
    active: Mutex<Active>,
    stopping: std::sync::atomic::AtomicBool,
}

impl ServerState {
    pub fn new(config: AgentConfig, tailnet: Arc<dyn Tailnet>) -> Arc<Self> {
        if dev_user().is_some() {
            tracing::warn!(
                "DANGER: NANOCTL_DEV_LOOPBACK_USER enabled: loopback authentication and loopback ICE are trusted. DEVELOPMENT ONLY."
            );
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
        if let Some((stop, task)) = self.active.lock().await.take() {
            stop.send_replace("shutdown".into());
            let _ = task.await;
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
            "hostName": if node.host_name.is_empty() {
                hostname::get().ok().and_then(|name| name.into_string().ok()).unwrap_or_else(|| "nanoctl host".into())
            } else { node.host_name.clone() }, "platform":crate::platform::PLATFORM,
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
        "self": {
            "hostName": node.host_name,
            "dnsName": node.dns_name,
            "url": host_url(&node, state.config.tailnet.port, tls),
        },
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
            if let Some((stop, task)) = active.take() {
                stop.send_replace("replaced".into());
                let _ = task.await;
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
    let (outgoing, mut messages) = tokio::sync::mpsc::channel::<Value>(64);
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
                            state.config.features.system_audio).await {
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
        let _ = peer.close().await;
    }
    let _ = send(&mut socket, json!({"type":"end","reason":reason})).await;
    let _ = tokio::time::timeout(Duration::from_secs(1), socket.close()).await;
}

pub async fn serve(state: Arc<ServerState>, mut shutdown: watch::Receiver<bool>) -> Result<()> {
    let mut listeners =
        HashMap::<IpAddr, (axum_server::Handle<SocketAddr>, tokio::task::JoinHandle<()>)>::new();
    let mut tls_config: Option<axum_server::tls_rustls::RustlsConfig> = None;
    let mut cert_node = String::new();
    let mut cert_checked = None::<tokio::time::Instant>;
    let mut interval = tokio::time::interval(Duration::from_secs(3));
    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            _ = interval.tick() => {}
        }
        let mut wanted = HashSet::new();
        match state.tailnet.status().await {
            Ok(node) => {
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
                        tracing::warn!("{}", result.detail);
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
                tracing::warn!(%error, "waiting for Tailscale");
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
        for ip in wanted {
            if listeners.contains_key(&ip) {
                continue;
            }
            let addr = SocketAddr::new(ip, state.config.tailnet.port);
            match tokio::net::TcpListener::bind(addr).await {
                Ok(listener) => {
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
                        let result: std::io::Result<()> = async {
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
                Err(error) => {
                    tracing::warn!(%addr, %error, "cannot bind tailnet listener; will retry")
                }
            }
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
        drop(listener);
        assert!(super::probe_peer(peer, port).await.is_none());
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
        second.close(None).await.unwrap();
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
