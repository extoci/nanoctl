use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

use crate::codec::VideoCodec;
use anyhow::{Context, Result};
#[cfg(any(feature = "media", test))]
use bytes::Bytes;
use interceptor::registry::Registry;
use serde::{Deserialize, Serialize};
use webrtc::api::APIBuilder;
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::{MIME_TYPE_AV1, MIME_TYPE_H264, MIME_TYPE_HEVC, MediaEngine};
use webrtc::api::setting_engine::SettingEngine;
#[cfg(feature = "media")]
use webrtc::data_channel::RTCDataChannel;
#[cfg(feature = "media")]
use webrtc::data_channel::data_channel_message::DataChannelMessage;
use webrtc::ice_transport::ice_candidate::{RTCIceCandidate, RTCIceCandidateInit};
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::policy::ice_transport_policy::RTCIceTransportPolicy;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::rtcp::payload_feedbacks::full_intra_request::FullIntraRequest;
use webrtc::rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication;
use webrtc::rtcp::payload_feedbacks::receiver_estimated_maximum_bitrate::ReceiverEstimatedMaximumBitrate;
use webrtc::rtcp::receiver_report::ReceiverReport;
use webrtc::rtcp::reception_report::ReceptionReport;
use webrtc::rtcp::transport_feedbacks::transport_layer_nack::TransportLayerNack;
use webrtc::rtp::extension::abs_send_time_extension::unix2ntp;
use webrtc::rtp_transceiver::RTCPFeedback;
use webrtc::rtp_transceiver::rtp_codec::{
    RTCRtpCodecCapability, RTCRtpHeaderExtensionCapability, RTPCodecType,
};
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;

use crate::adaptive::LinkSample;

fn reception_report_sample(
    report: &ReceptionReport,
    video_ssrc: u32,
    remb_kbps: Option<u32>,
    compact_ntp: u32,
    at: Instant,
) -> Option<LinkSample> {
    if report.ssrc != video_ssrc {
        return None;
    }
    let ticks = compact_ntp
        .wrapping_sub(report.last_sender_report)
        .wrapping_sub(report.delay);
    let rtt = (report.last_sender_report != 0 && (1..=10 * 65_536).contains(&ticks))
        .then(|| Duration::from_secs_f64(f64::from(ticks) / 65_536.0));
    Some(LinkSample {
        fraction_lost: f64::from(report.fraction_lost) / 256.0,
        rtt,
        remb_kbps,
        at,
        receiver_report_at: Some(at),
    })
}

const PROTOCOL_VERSION: u8 = 2;
const MIN_SUPPORTED_PROTOCOL_VERSION: u8 = 1;
const MAX_ICE_GENERATION: u64 = 1_000_000;
// Level 5.2 is the highest H.264 level supported by the configured 4K60 release envelope. The
// WebRTC matcher treats the level byte asymmetrically, so this remains compatible with Chromium's
// lower-level offer while no longer advertising a 720p30 ceiling for a 4K-capable sender.
#[cfg(any(feature = "media", test))]
pub(crate) const MAX_CONTROL_MESSAGE_BYTES: usize = 64 * 1024;
#[cfg(any(feature = "media", test))]
const RELIABLE_CONTROL_QUEUE_CAPACITY: usize = 1024;

#[cfg(any(feature = "media", test))]
#[derive(Clone)]
enum InputSender {
    Reliable(tokio::sync::mpsc::Sender<Bytes>),
    Motion(MotionSlots<tokio::sync::watch::Sender<LatestReport>>),
}

#[cfg(any(feature = "media", test))]
enum InputReceiver {
    Reliable(tokio::sync::mpsc::Receiver<Bytes>),
    Motion(MotionSlots<tokio::sync::watch::Receiver<LatestReport>>),
}

#[cfg(any(feature = "media", test))]
type LatestReport = Option<Bytes>;

/// The pointer lane keeps the latest report per kind: one shared slot would let a per-frame
/// gamepad snapshot overwrite the pointer move sent in the same frame, and vice versa.
#[cfg(any(feature = "media", test))]
#[derive(Clone)]
struct MotionSlots<T> {
    pointer: T,
    gamepad: T,
}

#[cfg(any(feature = "media", test))]
fn motion_channel() -> (
    MotionSlots<tokio::sync::watch::Sender<LatestReport>>,
    MotionSlots<tokio::sync::watch::Receiver<LatestReport>>,
) {
    let (pointer, pointer_receiver) = tokio::sync::watch::channel(None);
    let (gamepad, gamepad_receiver) = tokio::sync::watch::channel(None);
    (
        MotionSlots { pointer, gamepad },
        MotionSlots {
            pointer: pointer_receiver,
            gamepad: gamepad_receiver,
        },
    )
}

#[cfg(any(feature = "media", test))]
fn is_gamepad_report(bytes: &[u8]) -> bool {
    // Input wire v3: byte zero is the kind; kinds 2/12 are gamepad reports
    // and kind 11 their heartbeat. Keep this local because the input module exists
    // only with the media feature.
    matches!(bytes.first().copied(), Some(2 | 11 | 12))
}

#[cfg(any(feature = "media", test))]
impl InputReceiver {
    async fn recv(&mut self) -> Option<Bytes> {
        match self {
            Self::Reliable(receiver) => receiver.recv().await,
            Self::Motion(slots) => {
                let gamepad = tokio::select! {
                    changed = slots.pointer.changed() => changed.map(|()| false),
                    changed = slots.gamepad.changed() => changed.map(|()| true),
                }
                .ok()?;
                let slot = if gamepad {
                    &mut slots.gamepad
                } else {
                    &mut slots.pointer
                };
                slot.borrow_and_update().clone()
            }
        }
    }
}

#[cfg(any(feature = "media", test))]
#[derive(Debug, PartialEq, Eq)]
enum InputAdmission {
    Accepted,
    Oversized,
    Closed,
}

#[cfg(any(feature = "media", test))]
fn parse_audio_enabled(bytes: &[u8]) -> Option<bool> {
    (bytes.len() == 2 && bytes[0] == 9).then(|| bytes[1] != 0)
}

#[cfg(any(feature = "media", test))]
async fn admit_input_message(sender: &InputSender, bytes: Bytes) -> InputAdmission {
    if bytes.len() > MAX_CONTROL_MESSAGE_BYTES {
        return InputAdmission::Oversized;
    }
    let accepted = match sender {
        InputSender::Reliable(sender) => sender.send(bytes).await.is_ok(),
        InputSender::Motion(slots) => {
            let slot = if is_gamepad_report(&bytes) {
                &slots.gamepad
            } else {
                &slots.pointer
            };
            // A heartbeat carries no state, so it must never evict a pending
            // full snapshot from the lossy lane's single coalescing slot.
            if bytes.len() == 1 && bytes[0] == 11 && slot.borrow().is_some() {
                return InputAdmission::Accepted;
            }
            slot.send(Some(bytes)).is_ok()
        }
    };
    if accepted {
        InputAdmission::Accepted
    } else {
        InputAdmission::Closed
    }
}

pub struct HostPeer {
    peer: Arc<RTCPeerConnection>,
    publisher: SignalPublisher,
    session_id: String,
    sequence: Arc<AtomicU64>,
    controller_epoch: String,
    host_epoch: String,
    ice_generation: Arc<AtomicU64>,
    publication_lock: Arc<tokio::sync::Mutex<()>>,
    ice_ufrag_generations: Arc<tokio::sync::RwLock<HashMap<String, u64>>>,
    terminal: Arc<AtomicBool>,
    rtcp_task: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    #[cfg(feature = "media")]
    input_tasks: tokio::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>,
    #[cfg(feature = "media")]
    keyframe_requests: tokio::sync::watch::Sender<u64>,
    #[cfg_attr(not(feature = "media"), allow(dead_code))]
    link_feedback: tokio::sync::watch::Sender<Option<LinkSample>>,
    #[cfg(feature = "media")]
    display_selection: tokio::sync::watch::Sender<String>,
    #[cfg(feature = "media")]
    display_selection_feedback: tokio::sync::watch::Sender<String>,
    #[cfg(feature = "media")]
    viewer_limits: tokio::sync::watch::Sender<crate::adaptive::ViewerLimits>,
    #[cfg(feature = "media")]
    input_shutdown: tokio::sync::watch::Sender<bool>,
    #[cfg(feature = "media")]
    input_revoked: Arc<AtomicBool>,
    #[cfg_attr(not(feature = "media"), allow(dead_code))]
    video: Arc<TrackLocalStaticSample>,
    /// Optional viewer-created unreliable channel carrying compressed access
    /// units straight to WebCodecs, bypassing the track's jitter buffer.
    #[cfg(feature = "media")]
    video_data_channel: Arc<DataChannelVideo>,
    #[cfg_attr(not(feature = "media"), allow(dead_code))]
    video_codec: VideoCodec,
    #[cfg(feature = "media")]
    h264_high: bool,
}

/// Viewer-created unreliable data channel carrying raw compressed access units
/// straight to WebCodecs. The viewer can request a keyframe by sending `KF`
/// when it detects a sequence gap. `idr_channel` is the reliable channel
/// (`nanoctl.video-idr.v1`) used to deliver keyframes, which are too large
/// to survive partial reliability.
#[cfg(feature = "media")]
pub(crate) struct DataChannelVideo {
    pub channel: std::sync::Mutex<Option<Arc<RTCDataChannel>>>,
    pub idr_channel: std::sync::Mutex<Option<Arc<RTCDataChannel>>>,
    pub keyframe_requests: AtomicU64,
    pub needs_keyframe: AtomicBool,
    pub replay: Arc<std::sync::Mutex<VideoReplay>>,
}

/// The most recent complete keyframe, kept so a freshly attached viewer can
/// show a warm frame while the encoder produces a fresh keyframe. Delta tails
/// are deliberately not retained: replaying them can burst megabytes into SCTP
/// and make the decoder catch up through stale video instead of presenting the
/// current desktop.
#[cfg(any(feature = "media", test))]
#[derive(Default)]
pub(crate) struct VideoReplay {
    idr: Vec<Bytes>,
    pending_idr: Vec<Bytes>,
}

/// A reference-counted keyframe. Cloning the snapshot is cheap; messages are
/// shared with the live send path instead of copied a second time.
#[cfg(any(feature = "media", test))]
pub(crate) type VideoReplaySnapshot = Vec<Bytes>;

/// The replay window outlives a single peer connection: a freshly attached
/// viewer replays the previous stream's latest keyframe while a fresh one is
/// encoded.
#[cfg(feature = "media")]
static VIDEO_REPLAY: std::sync::OnceLock<Arc<std::sync::Mutex<VideoReplay>>> =
    std::sync::OnceLock::new();

#[cfg(feature = "media")]
pub(crate) fn video_replay() -> Arc<std::sync::Mutex<VideoReplay>> {
    VIDEO_REPLAY
        .get_or_init(|| Arc::new(std::sync::Mutex::new(VideoReplay::default())))
        .clone()
}

#[cfg(any(feature = "media", test))]
impl VideoReplay {
    /// Records a wire-format video message. `is_idr`, `part` and `parts` come
    /// from the message header the producer already computed.
    pub fn push(&mut self, is_idr: bool, part: u16, parts: u16, message: Bytes) {
        if is_idr {
            if part == 0 {
                self.pending_idr.clear();
            }
            self.pending_idr.push(message);
            if part + 1 == parts {
                self.idr = std::mem::take(&mut self.pending_idr);
            }
        }
    }

    /// Installs a ready-made keyframe only while the window is still empty.
    /// Checking and installing under one lock keeps a warm frame from ever
    /// overwriting a live stream's keyframe.
    pub fn seed(&mut self, idr: Vec<Bytes>) -> bool {
        if !self.idr.is_empty() || !self.pending_idr.is_empty() {
            return false;
        }
        self.idr = idr;
        true
    }

    /// Clones the warm keyframe for a newly attached channel.
    pub fn snapshot(&self) -> Option<VideoReplaySnapshot> {
        if self.idr.is_empty() {
            return None;
        }
        Some(self.idr.clone())
    }
}

#[derive(Clone)]
struct SignalPublisher {
    inner: SignalPublisherInner,
    gate: Arc<tokio::sync::Mutex<()>>,
    closed: Arc<AtomicBool>,
}

#[derive(Clone)]
enum SignalPublisherInner {
    WebSocket(tokio::sync::mpsc::Sender<serde_json::Value>),
    #[cfg(test)]
    Test(tokio::sync::mpsc::UnboundedSender<TestSignal>),
    #[cfg(test)]
    FlakyTest {
        sender: tokio::sync::mpsc::UnboundedSender<TestSignal>,
        failures: Arc<AtomicU64>,
    },
}

#[cfg(test)]
#[derive(Debug)]
struct TestSignal {
    session_id: String,
    sequence: u64,
    envelope: String,
}

impl SignalPublisher {
    fn websocket(sender: tokio::sync::mpsc::Sender<serde_json::Value>) -> Self {
        Self {
            inner: SignalPublisherInner::WebSocket(sender),
            gate: Arc::new(tokio::sync::Mutex::new(())),
            closed: Arc::new(AtomicBool::new(false)),
        }
    }

    #[cfg(test)]
    fn test(sender: tokio::sync::mpsc::UnboundedSender<TestSignal>) -> Self {
        Self {
            inner: SignalPublisherInner::Test(sender),
            gate: Arc::new(tokio::sync::Mutex::new(())),
            closed: Arc::new(AtomicBool::new(false)),
        }
    }

    #[cfg(test)]
    fn flaky_test(
        sender: tokio::sync::mpsc::UnboundedSender<TestSignal>,
        failures: Arc<AtomicU64>,
    ) -> Self {
        Self {
            inner: SignalPublisherInner::FlakyTest { sender, failures },
            gate: Arc::new(tokio::sync::Mutex::new(())),
            closed: Arc::new(AtomicBool::new(false)),
        }
    }

    async fn send(&self, session_id: &str, sequence: u64, envelope: &str) -> Result<()> {
        if self.closed.load(Ordering::Acquire) {
            anyhow::bail!("signal publisher is closed");
        }
        let _gate = self.gate.lock().await;
        if self.closed.load(Ordering::Acquire) {
            anyhow::bail!("signal publisher is closed");
        }
        self.send_locked(
            session_id,
            sequence,
            envelope,
            std::time::Duration::from_secs(5),
            false,
        )
        .await
    }

    async fn send_terminal(&self, session_id: &str, sequence: u64, envelope: &str) -> Result<()> {
        let _gate = self.gate.lock().await;
        self.closed.store(true, Ordering::Release);
        self.send_locked(
            session_id,
            sequence,
            envelope,
            std::time::Duration::from_secs(2),
            true,
        )
        .await
    }

    async fn send_locked(
        &self,
        session_id: &str,
        sequence: u64,
        envelope: &str,
        attempt_timeout: std::time::Duration,
        terminal: bool,
    ) -> Result<()> {
        let mut last_error = None;
        for attempt in 0..3 {
            let result = match tokio::time::timeout(
                attempt_timeout,
                self.send_once(session_id, sequence, envelope),
            )
            .await
            {
                Ok(result) => result,
                Err(_) => Err(anyhow::anyhow!("signal publication timed out")),
            };
            match result {
                Ok(()) => return Ok(()),
                Err(error) => {
                    last_error = Some(error);
                    if !terminal && self.closed.load(Ordering::Acquire) {
                        break;
                    }
                    if attempt < 2 {
                        tokio::time::sleep(std::time::Duration::from_millis(25 * (1 << attempt)))
                            .await;
                    }
                }
            }
        }
        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("signal publication failed")))
    }

    async fn send_once(&self, session_id: &str, sequence: u64, envelope: &str) -> Result<()> {
        match &self.inner {
            SignalPublisherInner::WebSocket(sender) => {
                let _ = (session_id, sequence);
                sender.send(serde_json::json!({"type":"signal","envelope":serde_json::from_str::<serde_json::Value>(envelope)?})).await
                    .map_err(|_| anyhow::anyhow!("WebSocket receiver closed"))
            }
            #[cfg(test)]
            SignalPublisherInner::Test(sender) => sender
                .send(TestSignal {
                    session_id: session_id.to_owned(),
                    sequence,
                    envelope: envelope.to_owned(),
                })
                .map_err(|_| anyhow::anyhow!("test signal receiver closed")),
            #[cfg(test)]
            SignalPublisherInner::FlakyTest { sender, failures } => {
                if failures
                    .fetch_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
                        remaining.checked_sub(1)
                    })
                    .is_ok()
                {
                    anyhow::bail!("injected transient signal failure");
                }
                sender
                    .send(TestSignal {
                        session_id: session_id.to_owned(),
                        sequence,
                        envelope: envelope.to_owned(),
                    })
                    .map_err(|_| anyhow::anyhow!("test signal receiver closed"))
            }
        }
    }

    fn close_now(&self) {
        self.closed.store(true, Ordering::Release);
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SignalEnvelope {
    version: u8,
    session_id: String,
    #[serde(default)]
    controller_epoch: Option<String>,
    #[serde(default)]
    host_epoch: Option<String>,
    #[serde(default)]
    generation: Option<u64>,
    sequence: u64,
    sender: String,
    sent_at: u64,
    payload: SignalPayload,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
enum SignalPayload {
    Offer {
        sdp: String,
    },
    IceCandidate {
        candidate: String,
        #[serde(rename = "sdpMid")]
        sdp_mid: Option<String>,
        #[serde(rename = "sdpMLineIndex")]
        sdp_mline_index: Option<u16>,
        #[serde(rename = "usernameFragment", default)]
        username_fragment: Option<String>,
    },
    IceComplete,
    End {
        reason: String,
    },
    #[serde(other)]
    Unsupported,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OutgoingEnvelope<'a> {
    version: u8,
    session_id: &'a str,
    controller_epoch: &'a str,
    host_epoch: &'a str,
    generation: u64,
    sequence: u64,
    sender: &'static str,
    sent_at: u64,
    payload: OutgoingPayload,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
enum OutgoingPayload {
    Answer {
        sdp: String,
    },
    IceCandidate {
        candidate: String,
        #[serde(rename = "sdpMid")]
        sdp_mid: Option<String>,
        #[serde(rename = "sdpMLineIndex")]
        sdp_mline_index: Option<u16>,
        #[serde(rename = "usernameFragment")]
        username_fragment: Option<String>,
    },
    IceComplete,
    End {
        reason: String,
    },
}

#[cfg(feature = "media")]
async fn forward_clipboard(
    backend: crate::clipboard::Backend,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
    output: Arc<RTCDataChannel>,
    revoked: Arc<AtomicBool>,
) {
    let mut changes = backend.changes.clone();
    changes.borrow_and_update();
    if !revoked.load(Ordering::Acquire)
        && let Some(message) = backend.current()
    {
        let _ = output.send_text(message).await;
    }
    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            result = changes.changed() => {
                if result.is_err() || revoked.load(Ordering::Acquire) { break; }
                let message = changes.borrow_and_update().clone();
                if let Some(message) = message {
                    let _ = output.send_text(message).await;
                }
            }
        }
    }
}

impl HostPeer {
    #[allow(clippy::too_many_arguments)]
    pub async fn answer_with_host_epoch(
        sender: tokio::sync::mpsc::Sender<serde_json::Value>,
        session_id: String,
        serialized_offer: &str,
        ice_servers: Vec<RTCIceServer>,
        ice_transport_policy: RTCIceTransportPolicy,
        host_epoch: String,
        allow_remote_input: bool,
        allow_clipboard: bool,
        audio_output: Option<tokio::sync::watch::Receiver<Option<String>>>,
    ) -> Result<Self> {
        Self::answer_with_publisher(
            SignalPublisher::websocket(sender),
            session_id,
            serialized_offer,
            ice_servers,
            ice_transport_policy,
            host_epoch,
            allow_remote_input,
            allow_clipboard,
            audio_output,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn answer_with_publisher(
        publisher: SignalPublisher,
        session_id: String,
        serialized_offer: &str,
        ice_servers: Vec<RTCIceServer>,
        ice_transport_policy: RTCIceTransportPolicy,
        host_epoch: String,
        allow_remote_input: bool,
        allow_clipboard: bool,
        audio_output: Option<tokio::sync::watch::Receiver<Option<String>>>,
    ) -> Result<Self> {
        let (offer, initial_generation, controller_epoch) =
            parse_offer(serialized_offer, &session_id)?;
        if controller_epoch.is_empty()
            || controller_epoch.len() > 128
            || host_epoch.is_empty()
            || host_epoch.len() > 128
        {
            anyhow::bail!("peer epoch is invalid");
        }
        let negotiated = crate::codec::negotiate(
            &offer,
            &crate::codec::host_codecs(),
            crate::codec::host_preference(),
        );
        tracing::info!(codec = negotiated.codec.id(), "negotiated video codec");
        let mut media_engine = MediaEngine::default();
        media_engine.register_default_codecs()?;
        media_engine.register_header_extension(
            RTCRtpHeaderExtensionCapability {
                uri: "http://www.webrtc.org/experiments/rtp-hdrext/playout-delay".to_owned(),
            },
            RTPCodecType::Video,
            None,
        )?;
        let registry = register_default_interceptors(Registry::new(), &mut media_engine)?;
        let mut settings = SettingEngine::default();
        let dev_loopback = crate::server::dev_user().is_some();
        if matches!(publisher.inner, SignalPublisherInner::WebSocket(_)) {
            settings.set_include_loopback_candidate(dev_loopback);
            settings.set_ice_multicast_dns_mode(webrtc::ice::mdns::MulticastDnsMode::Disabled);
            settings.set_ip_filter(Box::new(move |ip| {
                crate::tailnet::is_tailnet_ip(ip) || (dev_loopback && ip.is_loopback())
            }));
        }
        let api = APIBuilder::new()
            .with_setting_engine(settings)
            .with_media_engine(media_engine)
            .with_interceptor_registry(registry)
            .build();
        #[cfg(not(feature = "media"))]
        let _ = (allow_remote_input, allow_clipboard, audio_output);
        let peer = Arc::new(
            api.new_peer_connection(RTCConfiguration {
                ice_servers,
                ice_transport_policy,
                ..Default::default()
            })
            .await?,
        );
        #[cfg(feature = "media")]
        let (display_selection, _) = tokio::sync::watch::channel(String::new());
        #[cfg(feature = "media")]
        let (viewer_limits, _) =
            tokio::sync::watch::channel(crate::adaptive::ViewerLimits::default());
        #[cfg(feature = "media")]
        let (audio_enabled, _) = tokio::sync::watch::channel(true);
        let (link_feedback, _) = tokio::sync::watch::channel(None);
        #[cfg(feature = "media")]
        let (display_selection_feedback, display_selection_feedback_receiver) =
            tokio::sync::watch::channel(String::new());
        #[cfg(feature = "media")]
        let (input_shutdown, input_shutdown_receiver) = tokio::sync::watch::channel(false);
        #[cfg(feature = "media")]
        let input_revoked = Arc::new(AtomicBool::new(false));
        #[cfg(feature = "media")]
        let mut input_tasks = Vec::new();
        #[cfg(feature = "media")]
        let video_data_channel = Arc::new(DataChannelVideo {
            channel: std::sync::Mutex::new(None),
            idr_channel: std::sync::Mutex::new(None),
            keyframe_requests: AtomicU64::new(0),
            needs_keyframe: AtomicBool::new(false),
            replay: video_replay(),
        });
        #[cfg(feature = "media")]
        {
            let input = if allow_remote_input {
                match crate::input::InputController::new() {
                    Ok(input) => Some(Arc::new(std::sync::Mutex::new(input))),
                    Err(error) => {
                        tracing::warn!(error = %error, "remote input is unavailable");
                        None
                    }
                }
            } else {
                None
            };
            if let Some(input) = &input {
                let input = Arc::downgrade(input);
                let mut shutdown = input_shutdown.subscribe();
                input_tasks.push(tokio::spawn(async move {
                    let mut interval = tokio::time::interval(Duration::from_millis(50));
                    loop {
                        tokio::select! {
                            _ = shutdown.changed() => break,
                            _ = interval.tick() => {
                                let Some(input) = input.upgrade() else {
                                    break;
                                };
                                if let Ok(mut input) = input.lock() {
                                    // Gamepad viewers heartbeat every 250ms while idle. Three
                                    // missed heartbeats mean the transport or viewer is badly
                                    // impaired; don't let a held stick/button keep driving.
                                    input.release_if_idle(
                                        Duration::from_secs(2),
                                        Duration::from_millis(750),
                                    );
                                }
                            }
                        }
                    }
                }));
            }
            let (control_sender, control_receiver) =
                tokio::sync::mpsc::channel(RELIABLE_CONTROL_QUEUE_CAPACITY);
            let (pointer_sender, pointer_receiver) = motion_channel();
            let blocked_notices = InputBlockedNotices::default();
            if let Some(input) = &input {
                input_tasks.push(spawn_input_worker(
                    InputReceiver::Reliable(control_receiver),
                    input.clone(),
                    crate::input::InputLane::Reliable,
                    Some(display_selection.clone()),
                    Some(display_selection_feedback_receiver),
                    input_shutdown_receiver.clone(),
                    input_revoked.clone(),
                    blocked_notices.clone(),
                ));
                input_tasks.push(spawn_input_worker(
                    InputReceiver::Motion(pointer_receiver),
                    input.clone(),
                    crate::input::InputLane::PointerMotion,
                    None,
                    None,
                    input_shutdown_receiver,
                    input_revoked.clone(),
                    blocked_notices.clone(),
                ));
            }
            let input_revoked_for_channel = input_revoked.clone();
            let clipboard = if allow_clipboard {
                crate::clipboard::backend()
            } else {
                None
            };
            let clipboard_shutdown = input_shutdown.subscribe();
            let video_dc_for_channel = video_data_channel.clone();
            let viewer_limits_for_channel = viewer_limits.clone();
            let audio_enabled_for_channel = audio_enabled.clone();
            // The data-channel video path carries the encoder's exact
            // temporal units: H.264/HEVC Annex-B or AV1 low-overhead OBUs.
            // A viewer in "auto" may open the channels before negotiation
            // decides the codec, so they are closed again when unsupported.
            let video_dc_codec_ok = matches!(
                negotiated.codec,
                crate::codec::VideoCodec::H264
                    | crate::codec::VideoCodec::Hevc
                    | crate::codec::VideoCodec::Av1
            );
            peer.on_data_channel(Box::new(move |channel: Arc<RTCDataChannel>| {
                let input = input.clone();
                let viewer_limits = viewer_limits_for_channel.clone();
                let input_revoked = input_revoked_for_channel.clone();
                if channel.label() == "nanoctl.video.v1"
                    || channel.label() == "nanoctl.video-idr.v1"
                {
                    if !video_dc_codec_ok {
                        tracing::info!(
                            codec = negotiated.codec.id(),
                            "video data channel ignored: negotiated codec cannot use data-channel video"
                        );
                        return Box::pin(async {});
                    }
                    let is_idr = channel.label() == "nanoctl.video-idr.v1";
                    tracing::info!(idr = is_idr, "video data channel opened");
                    let slot = video_dc_for_channel.clone();
                    if is_idr {
                        *slot.idr_channel.lock().unwrap() = Some(channel.clone());
                    } else {
                        // Publish the resync requirement with the channel so a
                        // just-encoded delta can never race ahead of the fresh
                        // keyframe this attachment is about to request.
                        slot.needs_keyframe.store(true, Ordering::Release);
                        *slot.channel.lock().unwrap() = Some(channel.clone());
                    }
                    let kf = video_dc_for_channel.clone();
                    channel.on_message(Box::new(move |message| {
                        if message.data.as_ref() == b"KF" {
                            tracing::info!("video data channel keyframe request received");
                            kf.keyframe_requests.fetch_add(1, Ordering::Relaxed);
                        }
                        Box::pin(async {})
                    }));
                    channel.on_close(Box::new(move || {
                        if is_idr {
                            *slot.idr_channel.lock().unwrap() = None;
                        } else {
                            *slot.channel.lock().unwrap() = None;
                        }
                        Box::pin(async {})
                    }));
                    return Box::pin(async {});
                }
                if channel.label() == "nanoctl.clipboard.v1" {
                    if let Some(backend) = clipboard.clone() {
                        let initial_backend = backend.clone();
                        let shutdown = clipboard_shutdown.clone();
                        let output = channel.clone();
                        let revoked = input_revoked.clone();
                        channel.on_open(Box::new(move || {
                            tokio::spawn(forward_clipboard(
                                initial_backend.clone(),
                                shutdown.clone(),
                                output.clone(),
                                revoked.clone(),
                            ));
                            Box::pin(async {})
                        }));
                        channel.on_message(Box::new(move |message: DataChannelMessage| {
                            if message.is_string
                                && !input_revoked.load(Ordering::Acquire)
                                && let Some(text) = crate::clipboard::parse(&message.data)
                            {
                                backend.set(text);
                            }
                            Box::pin(async {})
                        }));
                    }
                    return Box::pin(async {});
                }
                let reliable = channel.label() == "nanoctl.control.v1";
                if input.is_none() {
                    // View-only sessions still adjust stream quality and transport.
                    if reliable {
                        let audio_enabled = audio_enabled_for_channel.clone();
                        channel.on_message(Box::new(move |message: DataChannelMessage| {
                            if message.is_string
                                && let Some(limits) =
                                    crate::adaptive::ViewerLimits::parse(&message.data)
                            {
                                viewer_limits.send_replace(limits);
                                return Box::pin(async {});
                            }
                            if let Some(enabled) = parse_audio_enabled(&message.data) {
                                audio_enabled.send_replace(enabled);
                            }
                            Box::pin(async {})
                        }));
                    }
                    return Box::pin(async {});
                }
                if reliable {
                    blocked_notices.attach(&channel);
                }
                let sender = match channel.label() {
                    "nanoctl.control.v1" => InputSender::Reliable(control_sender.clone()),
                    "nanoctl.pointer.v1" | "nanoctl.gamepad.v1" => {
                        InputSender::Motion(pointer_sender.clone())
                    }
                    _ => return Box::pin(async {}),
                };
                let message_input = input.clone();
                let weak_channel = Arc::downgrade(&channel);
                let audio_enabled = audio_enabled_for_channel.clone();
                channel.on_message(Box::new(move |message: DataChannelMessage| {
                    if reliable {
                        if message.is_string
                            && let Some(limits) = crate::adaptive::ViewerLimits::parse(&message.data)
                        {
                            viewer_limits.send_replace(limits);
                            return Box::pin(async {});
                        }
                        if let Some(enabled) = parse_audio_enabled(&message.data) {
                            audio_enabled.send_replace(enabled);
                            return Box::pin(async {});
                        }
                    }
                    #[cfg(target_os = "linux")]
                    crate::media::trace::input(0, &message.data);
                    let sender = sender.clone();
                    let input = message_input.clone();
                    let weak_channel = weak_channel.clone();
                    let input_revoked = input_revoked.clone();
                    Box::pin(async move {
                        let binary_input = crate::input::is_binary_input_message(&message.data);
                        if (!message.is_string && !binary_input)
                            || input_revoked.load(Ordering::Acquire)
                        {
                            return;
                        }
                        let admission = admit_input_message(&sender, message.data).await;
                        if admission != InputAdmission::Accepted {
                            input_revoked.store(true, Ordering::Release);
                            tracing::warn!(?admission, "input rejected; closing control channel");
                            tokio::spawn(async move {
                                if let Some(channel) = weak_channel.upgrade() {
                                    let _ = channel.close().await;
                                }
                            });
                            tokio::task::spawn_blocking(move || {
                                if let Some(input) = input
                                    && let Ok(mut input) = input.lock()
                                {
                                    input.release_all();
                                    input.revoke_gamepads();
                                }
                            });
                        }
                    })
                }));
                channel.on_close(Box::new(move || {
                    let input = input.clone();
                    Box::pin(async move {
                        if let Some(input) = input
                            && let Ok(mut input) = input.lock()
                        {
                            input.release_all();
                            // A close can be transient (ICE restart): idle the
                            // emulated pads but keep them plugged in.
                            if reliable {
                                input.neutralize_gamepads();
                            }
                        }
                    })
                }));
                Box::pin(async {})
            }));
        }
        #[cfg(feature = "media")]
        if let Some(output) = audio_output
            && offer.lines().any(|line| line.starts_with("m=audio "))
        {
            let audio = Arc::new(
                webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP::new(
                    RTCRtpCodecCapability {
                        mime_type: webrtc::api::media_engine::MIME_TYPE_OPUS.into(),
                        clock_rate: 48_000,
                        channels: 2,
                        sdp_fmtp_line: "minptime=10;useinbandfec=1;stereo=1".into(),
                        ..Default::default()
                    },
                    "system-audio".into(),
                    "nanoctl-audio".into(),
                ),
            );
            let sender = peer.add_track(audio.clone()).await?;
            input_tasks.push(tokio::spawn(crate::audio::run(
                audio,
                peer.clone(),
                input_shutdown.subscribe(),
                output,
                audio_enabled.subscribe(),
            )));
            let mut shutdown = input_shutdown.subscribe();
            input_tasks.push(tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = shutdown.changed() => break,
                        result = sender.read_rtcp() => { if result.is_err() { break; } }
                    }
                }
            }));
        }
        let video = Arc::new(TrackLocalStaticSample::new(
            RTCRtpCodecCapability {
                mime_type: match negotiated.codec {
                    VideoCodec::H264 => MIME_TYPE_H264,
                    VideoCodec::Hevc => MIME_TYPE_HEVC,
                    VideoCodec::Av1 => MIME_TYPE_AV1,
                }
                .to_owned(),
                clock_rate: 90_000,
                channels: 0,
                sdp_fmtp_line: match negotiated.codec {
                    VideoCodec::H264 => format!(
                        "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id={}",
                        negotiated.h264_profile_level_id()
                    ),
                    VideoCodec::Hevc | VideoCodec::Av1 => negotiated.offered_fmtp.clone(),
                },
                rtcp_feedback: vec![
                    RTCPFeedback {
                        typ: "nack".to_owned(),
                        parameter: "".to_owned(),
                    },
                    RTCPFeedback {
                        typ: "nack".to_owned(),
                        parameter: "pli".to_owned(),
                    },
                    RTCPFeedback {
                        typ: "ccm".to_owned(),
                        parameter: "fir".to_owned(),
                    },
                    RTCPFeedback {
                        typ: "goog-remb".to_owned(),
                        parameter: "".to_owned(),
                    },
                ],
            },
            "desktop".to_owned(),
            "nanoctl".to_owned(),
        ));
        let sender = peer.add_track(video.clone()).await?;
        #[cfg(feature = "media")]
        let (keyframe_requests, _) = tokio::sync::watch::channel(0_u64);
        #[cfg(feature = "media")]
        let keyframe_feedback = keyframe_requests.clone();
        let link_samples = link_feedback.clone();
        let video_ssrc = sender
            .get_parameters()
            .await
            .encodings
            .first()
            .context("video sender has no encoding")?
            .ssrc;
        let rtcp_task = tokio::spawn(async move {
            let mut remb_kbps = None;
            let mut feedback = FeedbackSummary::default();
            let mut latest_sample: Option<LinkSample> = None;
            while let Ok((packets, _attributes)) = sender.read_rtcp().await {
                let at = std::time::Instant::now();
                if packets.iter().any(|packet| {
                    packet.as_any().is::<PictureLossIndication>()
                        || packet.as_any().is::<FullIntraRequest>()
                }) {
                    feedback.keyframe_requests += 1;
                }
                let compact_ntp = (unix2ntp(SystemTime::now()) >> 16) as u32;
                #[cfg(feature = "media")]
                {
                    if packets.iter().any(|packet| {
                        packet.as_any().is::<PictureLossIndication>()
                            || packet.as_any().is::<FullIntraRequest>()
                    }) {
                        keyframe_feedback.send_modify(|generation| {
                            *generation = generation.wrapping_add(1);
                        });
                    }
                }
                let previous_remb = remb_kbps;
                for packet in &packets {
                    if let Some(nack) = packet.as_any().downcast_ref::<TransportLayerNack>() {
                        tracing::debug!(
                            video_ssrc,
                            media_ssrc = nack.media_ssrc,
                            nack_pairs = nack.nacks.len(),
                            "video NACK"
                        );
                        feedback.nacks += 1;
                        feedback.nack_pairs += nack.nacks.len() as u64;
                    }
                    if let Some(estimate) = packet
                        .as_any()
                        .downcast_ref::<ReceiverEstimatedMaximumBitrate>()
                        && estimate.bitrate.is_finite()
                        && estimate.bitrate > 0.0
                    {
                        let kbps = (estimate.bitrate / 1_000.0).min(u32::MAX as f32) as u32;
                        tracing::debug!(video_ssrc, remb_kbps = kbps, "video REMB");
                        remb_kbps = Some(kbps.max(1));
                        feedback.record_remb(kbps);
                    }
                }
                let mut reported = false;
                for report in packets
                    .iter()
                    .filter_map(|packet| packet.as_any().downcast_ref::<ReceiverReport>())
                    .flat_map(|packet| &packet.reports)
                {
                    if let Some(sample) =
                        reception_report_sample(report, video_ssrc, remb_kbps, compact_ntp, at)
                    {
                        tracing::debug!(
                            video_ssrc,
                            lsr = report.last_sender_report,
                            dlsr = report.delay,
                            compact_ntp,
                            fraction_lost = sample.fraction_lost,
                            total_lost = report.total_lost,
                            rtt_ms = sample.rtt.map(|rtt| rtt.as_secs_f64() * 1000.0),
                            "video receiver report"
                        );
                        latest_sample = Some(sample);
                        link_samples.send_replace(Some(sample));
                        reported = true;
                    }
                }
                if let Some(summary) = feedback.take_due(at) {
                    tracing::info!(
                        video_ssrc,
                        nacks = summary.nacks,
                        nack_pairs = summary.nack_pairs,
                        keyframe_requests = summary.keyframe_requests,
                        remb_min_kbps = summary.remb_min_kbps,
                        remb_last_kbps = summary.remb_last_kbps,
                        "video feedback (last {}s)",
                        FEEDBACK_SUMMARY_INTERVAL.as_secs()
                    );
                }
                if !reported && remb_kbps != previous_remb {
                    let mut sample = latest_sample.unwrap_or(LinkSample {
                        fraction_lost: 0.0,
                        rtt: None,
                        remb_kbps,
                        at,
                        receiver_report_at: None,
                    });
                    sample.at = at;
                    sample.remb_kbps = remb_kbps;
                    latest_sample = Some(sample);
                    link_samples.send_replace(Some(sample));
                }
            }
        });

        // Host signal sequences must not collide with a previous in-memory peer after an agent
        // restart. They stay below JavaScript's safe integer limit because Convex stores them as
        // JSON numbers; the browser replays recent host rows by insertion time rather than using
        // this fresh seed as a cross-process ordering cursor.
        let initial_sequence = host_sequence_seed();
        let sequence = Arc::new(AtomicU64::new(initial_sequence.saturating_add(1)));
        let ice_generation = Arc::new(AtomicU64::new(initial_generation));
        let ice_ufrag_generations = Arc::new(tokio::sync::RwLock::new(HashMap::new()));
        let terminal = Arc::new(AtomicBool::new(false));
        // ICE callbacks are asynchronous and can run as soon as local description is installed.
        // Serialize them with the answer publication so a browser never observes a host
        // candidate before the SDP that gives that candidate a remote description.
        let publication_lock = Arc::new(tokio::sync::Mutex::new(()));
        let controller_epoch_for_ice = controller_epoch.clone();
        let host_epoch_for_ice = host_epoch.clone();
        peer.on_ice_candidate({
            let publisher = publisher.clone();
            let session_id = session_id.clone();
            let sequence = sequence.clone();
            let ice_generation = ice_generation.clone();
            let ice_ufrag_generations = ice_ufrag_generations.clone();
            let publication_lock = publication_lock.clone();
            Box::new(move |candidate: Option<RTCIceCandidate>| {
                let publisher = publisher.clone();
                let session_id = session_id.clone();
                let sequence = sequence.clone();
                let ice_generation = ice_generation.clone();
                let ice_ufrag_generations = ice_ufrag_generations.clone();
                let publication_lock = publication_lock.clone();
                let controller_epoch = controller_epoch_for_ice.clone();
                let host_epoch = host_epoch_for_ice.clone();
                Box::pin(async move {
                    let _publication_guard = publication_lock.lock().await;
                    let (payload, generation) = match candidate {
                        Some(candidate) => match candidate.to_json() {
                            Ok(candidate) => {
                                let current_generation = ice_generation.load(Ordering::Acquire);
                                let generation =
                                    if let Some(ufrag) = candidate.username_fragment.as_deref() {
                                        ice_ufrag_generations
                                            .read()
                                            .await
                                            .get(ufrag)
                                            .copied()
                                            .unwrap_or(current_generation)
                                    } else {
                                        current_generation
                                    };
                                (
                                    OutgoingPayload::IceCandidate {
                                        candidate: candidate.candidate,
                                        sdp_mid: candidate.sdp_mid,
                                        sdp_mline_index: candidate.sdp_mline_index,
                                        username_fragment: candidate.username_fragment,
                                    },
                                    generation,
                                )
                            }
                            Err(_) => return,
                        },
                        None => (
                            OutgoingPayload::IceComplete,
                            ice_generation.load(Ordering::Acquire),
                        ),
                    };
                    let index = sequence.fetch_add(1, Ordering::Relaxed);
                    if let Ok(envelope) = serialize_with_generation(
                        &session_id,
                        &controller_epoch,
                        &host_epoch,
                        generation,
                        index,
                        payload,
                    ) {
                        let _ = publisher.send(&session_id, index, &envelope).await;
                    }
                })
            })
        });

        let _publication_guard = publication_lock.lock().await;
        peer.set_remote_description(RTCSessionDescription::offer(offer)?)
            .await?;
        let answer = peer.create_answer(None).await?;
        peer.set_local_description(answer).await?;
        let local = peer
            .local_description()
            .await
            .context("WebRTC answer was not created")?;
        remember_ice_ufrag(&ice_ufrag_generations, &local.sdp, initial_generation).await;
        let envelope = serialize_with_generation(
            &session_id,
            &controller_epoch,
            &host_epoch,
            initial_generation,
            initial_sequence,
            OutgoingPayload::Answer { sdp: local.sdp },
        )?;
        publisher
            .send(&session_id, initial_sequence, &envelope)
            .await?;
        drop(_publication_guard);
        Ok(Self {
            peer,
            publisher,
            session_id,
            sequence,
            controller_epoch,
            host_epoch,
            ice_generation,
            publication_lock,
            ice_ufrag_generations,
            terminal,
            rtcp_task: tokio::sync::Mutex::new(Some(rtcp_task)),
            #[cfg(feature = "media")]
            input_tasks: tokio::sync::Mutex::new(input_tasks),
            #[cfg(feature = "media")]
            keyframe_requests,
            link_feedback,
            #[cfg(feature = "media")]
            display_selection,
            #[cfg(feature = "media")]
            display_selection_feedback,
            #[cfg(feature = "media")]
            viewer_limits,
            #[cfg(feature = "media")]
            input_shutdown,
            #[cfg(feature = "media")]
            input_revoked,
            video,
            #[cfg(feature = "media")]
            video_data_channel,
            video_codec: negotiated.codec,
            #[cfg(feature = "media")]
            h264_high: negotiated.h264_high,
        })
    }

    pub async fn add_signal(
        &self,
        serialized: &str,
        expected_session: &str,
        expected_sequence: u64,
    ) -> Result<bool> {
        if serialized.len() > 1_100_000 {
            anyhow::bail!("signal exceeds maximum size");
        }
        let envelope: SignalEnvelope = serde_json::from_str(serialized)?;
        if !supported_version(envelope.version)
            || envelope.session_id != expected_session
            || envelope.sequence != expected_sequence
            || envelope.sender != "controller"
            || envelope.sent_at == 0
            || envelope.controller_epoch.as_deref().unwrap_or("") != self.controller_epoch
        {
            anyhow::bail!("signal identity does not match the session");
        }
        let envelope_generation = envelope.generation.unwrap_or(0);
        if !envelope.host_epoch.as_deref().unwrap_or("").is_empty() {
            anyhow::bail!("controller signal cannot carry a host epoch");
        }
        if envelope_generation > MAX_ICE_GENERATION {
            anyhow::bail!("ICE generation is too large");
        }
        match envelope.payload {
            SignalPayload::IceCandidate {
                candidate,
                sdp_mid,
                sdp_mline_index,
                username_fragment,
            } => {
                let generation = envelope_generation;
                if generation != self.ice_generation.load(Ordering::Acquire) {
                    anyhow::bail!("ICE candidate belongs to an inactive generation");
                }
                if candidate.is_empty()
                    || candidate.len() > 8_192
                    || sdp_mid.as_ref().is_some_and(|value| value.len() > 256)
                    || username_fragment
                        .as_ref()
                        .is_some_and(|value| value.is_empty() || value.len() > 256)
                {
                    anyhow::bail!("ICE candidate is invalid");
                }
                self.peer
                    .add_ice_candidate(RTCIceCandidateInit {
                        candidate,
                        sdp_mid,
                        sdp_mline_index,
                        username_fragment,
                    })
                    .await?;
                Ok(true)
            }
            SignalPayload::IceComplete => {
                let generation = envelope_generation;
                if generation != self.ice_generation.load(Ordering::Acquire) {
                    anyhow::bail!("ICE completion belongs to an inactive generation");
                }
                self.peer
                    .add_ice_candidate(RTCIceCandidateInit {
                        candidate: String::new(),
                        sdp_mid: None,
                        sdp_mline_index: None,
                        username_fragment: None,
                    })
                    .await?;
                Ok(true)
            }
            SignalPayload::End { reason } => {
                if reason.is_empty() || reason.len() > 512 {
                    anyhow::bail!("end reason is invalid");
                }
                if envelope_generation != self.ice_generation.load(Ordering::Acquire) {
                    anyhow::bail!("end signal belongs to an inactive generation");
                }
                tracing::info!(reason_bytes = reason.len(), "controller ended session");
                self.close().await?;
                Ok(false)
            }
            SignalPayload::Offer { sdp } => {
                if sdp.is_empty() || sdp.len() > 1_000_000 {
                    anyhow::bail!("restart offer SDP is invalid");
                }
                let generation = envelope_generation;
                let current_generation = self.ice_generation.load(Ordering::Acquire);
                // The initial generation-0 offer was already consumed by `answer`. Any later
                // offer must explicitly advance the ICE generation; accepting another gen-0
                // offer would let a delayed/duplicated bootstrap replace a live peer.
                if generation <= current_generation {
                    anyhow::bail!("restart offer belongs to an inactive generation");
                }
                let _publication_guard = self.publication_lock.lock().await;
                self.peer
                    .set_remote_description(RTCSessionDescription::offer(sdp)?)
                    .await?;
                let answer = self.peer.create_answer(None).await?;
                self.peer.set_local_description(answer).await?;
                let local = self
                    .peer
                    .local_description()
                    .await
                    .context("WebRTC restart answer was not created")?;
                remember_ice_ufrag(&self.ice_ufrag_generations, &local.sdp, generation).await;
                self.ice_generation.store(generation, Ordering::Release);
                let index = self.sequence.fetch_add(1, Ordering::Relaxed);
                let response = serialize_with_generation(
                    &self.session_id,
                    &self.controller_epoch,
                    &self.host_epoch,
                    generation,
                    index,
                    OutgoingPayload::Answer { sdp: local.sdp },
                )?;
                self.publisher
                    .send(&self.session_id, index, &response)
                    .await?;
                Ok(true)
            }
            SignalPayload::Unsupported => Ok(true),
        }
    }

    pub async fn close(&self) -> Result<()> {
        self.terminal.store(true, Ordering::Release);
        #[cfg(feature = "media")]
        self.revoke_input();
        #[cfg(feature = "media")]
        let _ = self.input_shutdown.send(true);
        // Do not wait behind an in-flight control-plane publication while the WebRTC peer is
        // still alive. A slow signal request must never make native teardown unbounded.
        self.publisher.close_now();
        let close_result = self.peer.close().await;
        self.join_auxiliary_tasks().await;
        close_result?;
        Ok(())
    }

    pub async fn fail(&self, reason: &str) -> Result<()> {
        if self.terminal.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        #[cfg(feature = "media")]
        self.revoke_input();
        #[cfg(feature = "media")]
        let _ = self.input_shutdown.send(true);
        let reason = reason.chars().take(256).collect::<String>();
        let index = self.sequence.fetch_add(1, Ordering::Relaxed);
        let envelope = serialize_with_generation(
            &self.session_id,
            &self.controller_epoch,
            &self.host_epoch,
            self.ice_generation.load(Ordering::Acquire),
            index,
            OutgoingPayload::End { reason },
        )?;
        let signal_result = self
            .publisher
            .send_terminal(&self.session_id, index, &envelope)
            .await;
        let close_result = self.peer.close().await;
        self.join_auxiliary_tasks().await;
        signal_result?;
        close_result?;
        Ok(())
    }

    async fn join_auxiliary_tasks(&self) {
        if let Some(task) = self.rtcp_task.lock().await.take() {
            task.abort();
            let _ = task.await;
        }
        #[cfg(feature = "media")]
        {
            let tasks = std::mem::take(&mut *self.input_tasks.lock().await);
            for task in tasks {
                let _ = task.await;
            }
        }
    }

    pub fn connection_failed(&self) -> bool {
        self.peer.connection_state() == RTCPeerConnectionState::Failed
    }

    /// Revoke data-channel input before asynchronous peer/media teardown begins. This closes the
    /// handoff window where a stale browser could still enqueue a click or key after the control
    /// plane has assigned the session to a replacement host.
    #[cfg(feature = "media")]
    pub fn revoke_input(&self) {
        self.input_revoked.store(true, Ordering::Release);
        let _ = self.input_shutdown.send(true);
    }

    #[cfg(feature = "media")]
    pub fn h264_high(&self) -> bool {
        self.h264_high
    }

    #[cfg(feature = "media")]
    pub fn video_codec(&self) -> VideoCodec {
        self.video_codec
    }

    #[cfg(feature = "media")]
    pub fn video_track(&self) -> Arc<TrackLocalStaticSample> {
        self.video.clone()
    }

    #[cfg(feature = "media")]
    pub fn video_data_channel(&self) -> Arc<DataChannelVideo> {
        self.video_data_channel.clone()
    }

    #[cfg(feature = "media")]
    pub fn keyframe_requests(&self) -> tokio::sync::watch::Receiver<u64> {
        self.keyframe_requests.subscribe()
    }

    #[cfg_attr(not(feature = "media"), allow(dead_code))]
    pub fn link_feedback(&self) -> tokio::sync::watch::Receiver<Option<LinkSample>> {
        self.link_feedback.subscribe()
    }

    #[cfg(feature = "media")]
    pub fn viewer_limits(&self) -> tokio::sync::watch::Receiver<crate::adaptive::ViewerLimits> {
        self.viewer_limits.subscribe()
    }

    #[cfg(feature = "media")]
    pub fn display_selection(&self) -> tokio::sync::watch::Receiver<String> {
        self.display_selection.subscribe()
    }

    #[cfg(feature = "media")]
    pub fn display_selection_feedback_sender(&self) -> tokio::sync::watch::Sender<String> {
        self.display_selection_feedback.clone()
    }
}

#[cfg(feature = "media")]
const INPUT_BLOCKED_NOTICE: &str = r#"{"type":"inputBlocked"}"#;
#[cfg(feature = "media")]
const INPUT_BLOCKED_NOTICE_INTERVAL: Duration = Duration::from_secs(5);

#[cfg(feature = "media")]
/// Tells the viewer when the host OS refuses injected input (for example Windows UIPI while an
/// elevated window, UAC prompt or the lock screen is in front), at most once per interval.
#[derive(Clone, Default)]
struct InputBlockedNotices(Arc<std::sync::Mutex<InputBlockedState>>);

#[cfg(feature = "media")]
#[derive(Default)]
struct InputBlockedState {
    channel: Option<std::sync::Weak<RTCDataChannel>>,
    last_sent: Option<Instant>,
}

#[cfg(feature = "media")]
impl InputBlockedNotices {
    fn attach(&self, channel: &Arc<RTCDataChannel>) {
        if let Ok(mut state) = self.0.lock() {
            state.channel = Some(Arc::downgrade(channel));
        }
    }

    fn notify(&self) {
        let Ok(mut state) = self.0.lock() else {
            return;
        };
        if state
            .last_sent
            .is_some_and(|sent| sent.elapsed() < INPUT_BLOCKED_NOTICE_INTERVAL)
        {
            return;
        }
        let Some(channel) = state.channel.as_ref().and_then(std::sync::Weak::upgrade) else {
            return;
        };
        state.last_sent = Some(Instant::now());
        tokio::spawn(async move {
            let _ = channel.send_text(INPUT_BLOCKED_NOTICE.to_owned()).await;
        });
    }
}

#[cfg(feature = "media")]
#[allow(clippy::too_many_arguments)]
fn spawn_input_worker(
    mut receiver: InputReceiver,
    input: Arc<std::sync::Mutex<crate::input::InputController>>,
    lane: crate::input::InputLane,
    display_selection: Option<tokio::sync::watch::Sender<String>>,
    mut display_selection_feedback: Option<tokio::sync::watch::Receiver<String>>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
    input_revoked: Arc<AtomicBool>,
    blocked_notices: InputBlockedNotices,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = shutdown.changed() => break,
                feedback = async {
                    match display_selection_feedback.as_mut() {
                        Some(receiver) => receiver.changed().await.ok(),
                        None => std::future::pending::<Option<()>>().await,
                    }
                } => {
                    let Some(()) = feedback else {
                        display_selection_feedback = None;
                        continue;
                    };
                    if *shutdown.borrow() {
                        break;
                    }
                    let Some(receiver) = display_selection_feedback.as_ref() else {
                        continue;
                    };
                    let display_id = receiver.borrow().clone();
                    if display_id.is_empty() {
                        continue;
                    }
                    let input = input.clone();
                    let input_revoked = input_revoked.clone();
                    match tokio::task::spawn_blocking(move || {
                        if input_revoked.load(Ordering::Acquire) {
                            return Ok(());
                        }
                        let mut input = input
                            .lock()
                            .map_err(|_| anyhow::anyhow!("input controller lock poisoned"))?;
                        if input_revoked.load(Ordering::Acquire) {
                            return Ok(());
                        }
                        input.select_display(&display_id)
                    })
                    .await
                    {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => tracing::warn!(error = %error, "display feedback rejected"),
                        Err(error) => {
                            tracing::warn!(error = %error, "display feedback worker stopped unexpectedly");
                            break;
                        }
                    }
                }
                bytes = receiver.recv() => {
                    let Some(bytes) = bytes else {
                        break;
                    };
                    if *shutdown.borrow() {
                        break;
                    }
                    let input = input.clone();
                    let input_revoked = input_revoked.clone();
                    let result = tokio::task::spawn_blocking(move || {
                        let mut input = input
                            .lock()
                            .map_err(|_| anyhow::anyhow!("input controller lock poisoned"))?;
                        if input_revoked.load(Ordering::Acquire) {
                            return Ok(None);
                        }
                        #[cfg(target_os = "linux")]
                        crate::media::trace::input(1, &bytes);
                        let result = input.dispatch(&bytes, lane);
                        #[cfg(target_os = "linux")]
                        crate::media::trace::input(2, &bytes);
                        result
                    })
                    .await;
                    match result {
                        Ok(Ok(Some(display_id))) => {
                            if let Some(selection) = &display_selection {
                                selection.send_replace(display_id);
                            }
                        }
                        Ok(Ok(None)) => {}
                        Ok(Err(error)) => {
                            tracing::warn!(error = %error, "control message rejected");
                            if error.downcast_ref::<enigo::InputError>().is_some() {
                                blocked_notices.notify();
                            }
                        }
                        Err(error) => {
                            tracing::warn!(error = %error, "input worker stopped unexpectedly");
                            break;
                        }
                    }
                }
            }
        }
        if matches!(lane, crate::input::InputLane::Reliable)
            && let Ok(mut input) = input.lock()
        {
            input.revoke_gamepads();
        }
    })
}

fn parse_offer(serialized: &str, expected_session: &str) -> Result<(String, u64, String)> {
    if serialized.len() > 1_100_000 {
        anyhow::bail!("signal exceeds maximum size");
    }
    let envelope: SignalEnvelope =
        serde_json::from_str(serialized).context("offer envelope is invalid")?;
    if !supported_version(envelope.version)
        || envelope.session_id != expected_session
        || envelope.sender != "controller"
        || envelope.sent_at == 0
        || !envelope.host_epoch.as_deref().unwrap_or("").is_empty()
    {
        anyhow::bail!("offer identity does not match the session");
    }
    let controller_epoch = envelope.controller_epoch.unwrap_or_default();
    if controller_epoch.len() > 128 {
        anyhow::bail!("controller epoch is too long");
    }
    let generation = envelope.generation.unwrap_or(0);
    if generation > MAX_ICE_GENERATION {
        anyhow::bail!("ICE generation is too large");
    }
    match envelope.payload {
        SignalPayload::Offer { sdp } if !sdp.is_empty() && sdp.len() <= 1_000_000 => {
            Ok((sdp, generation, controller_epoch))
        }
        _ => anyhow::bail!("signal is not a valid offer"),
    }
}

pub fn is_offer_for_session(serialized: &str, expected_session: &str) -> bool {
    parse_offer(serialized, expected_session).is_ok()
}

#[cfg(test)]
pub fn offer_generation_and_epoch(
    serialized: &str,
    expected_session: &str,
) -> Result<(u64, String)> {
    parse_offer(serialized, expected_session)
        .map(|(_, generation, controller_epoch)| (generation, controller_epoch))
}

#[cfg(test)]
fn serialize(session_id: &str, sequence: u64, payload: OutgoingPayload) -> Result<String> {
    serialize_with_generation(session_id, "", "", 0, sequence, payload)
}

pub(crate) fn host_epoch_seed() -> String {
    format!("host-{}", uuid::Uuid::new_v4())
}

fn host_sequence_seed() -> u64 {
    // Keep sequence numbers exactly representable by JavaScript clients.
    let random = (uuid::Uuid::new_v4().as_u128() & ((1_u128 << 53) - 1)) as u64;
    random.clamp(1, 9_007_199_254_740_991_u64 - 4_096)
}

fn serialize_with_generation(
    session_id: &str,
    controller_epoch: &str,
    host_epoch: &str,
    generation: u64,
    sequence: u64,
    payload: OutgoingPayload,
) -> Result<String> {
    Ok(serde_json::to_string(&OutgoingEnvelope {
        version: PROTOCOL_VERSION,
        session_id,
        controller_epoch,
        host_epoch,
        generation,
        sequence,
        sender: "host",
        sent_at: SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_millis() as u64,
        payload,
    })?)
}

async fn remember_ice_ufrag(
    generations: &tokio::sync::RwLock<HashMap<String, u64>>,
    sdp: &str,
    generation: u64,
) {
    let Some(ufrag) = sdp
        .lines()
        .find_map(|line| line.trim().strip_prefix("a=ice-ufrag:"))
        .filter(|value| !value.is_empty() && value.len() <= 256)
    else {
        return;
    };
    generations
        .write()
        .await
        .insert(ufrag.to_owned(), generation);
}

fn supported_version(version: u8) -> bool {
    (MIN_SUPPORTED_PROTOCOL_VERSION..=PROTOCOL_VERSION).contains(&version)
}

const FEEDBACK_SUMMARY_INTERVAL: Duration = Duration::from_secs(10);

/// Viewer RTCP feedback folded into one info line per interval: per-packet NACK and REMB
/// lines would otherwise crowd everything else out of the diagnostics log tail.
#[derive(Debug, Default, PartialEq)]
struct FeedbackSummary {
    nacks: u64,
    nack_pairs: u64,
    keyframe_requests: u64,
    remb_min_kbps: Option<u32>,
    remb_last_kbps: Option<u32>,
    since: Option<Instant>,
}

impl FeedbackSummary {
    fn record_remb(&mut self, kbps: u32) {
        self.remb_min_kbps = Some(self.remb_min_kbps.map_or(kbps, |min| min.min(kbps)));
        self.remb_last_kbps = Some(kbps);
    }

    fn take_due(&mut self, now: Instant) -> Option<Self> {
        let since = *self.since.get_or_insert(now);
        if now.duration_since(since) < FEEDBACK_SUMMARY_INTERVAL {
            return None;
        }
        let summary = std::mem::take(self);
        self.since = Some(now);
        (summary.nacks > 0 || summary.keyframe_requests > 0 || summary.remb_last_kbps.is_some())
            .then_some(summary)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn feedback_summary_reports_once_per_interval() {
        let start = std::time::Instant::now();
        let mut feedback = FeedbackSummary::default();
        assert_eq!(feedback.take_due(start), None);
        feedback.nacks = 3;
        feedback.nack_pairs = 5;
        feedback.record_remb(9_000);
        feedback.record_remb(7_000);
        feedback.record_remb(8_000);
        assert_eq!(feedback.take_due(start + Duration::from_secs(5)), None);
        let summary = feedback
            .take_due(start + FEEDBACK_SUMMARY_INTERVAL)
            .expect("due");
        assert_eq!(
            (
                summary.nacks,
                summary.nack_pairs,
                summary.remb_min_kbps,
                summary.remb_last_kbps
            ),
            (3, 5, Some(7_000), Some(8_000))
        );
        assert_eq!(
            feedback.take_due(start + FEEDBACK_SUMMARY_INTERVAL * 2),
            None
        );
    }

    use super::*;
    use proptest::prelude::*;
    use serde_json::Value;
    use tokio::time::{Duration, Instant, timeout};
    use webrtc::peer_connection::offer_answer_options::RTCOfferOptions;
    use webrtc::rtp_transceiver::rtp_codec::RTPCodecType;

    #[test]
    fn parses_audio_enabled_control() {
        assert_eq!(parse_audio_enabled(&[9, 0]), Some(false));
        assert_eq!(parse_audio_enabled(&[9, 1]), Some(true));
        assert_eq!(
            parse_audio_enabled(br#"{"type":"audio","enabled":true}"#),
            None
        );
        assert_eq!(parse_audio_enabled(&[9]), None);
    }

    #[test]
    fn video_replay_snapshot_shares_keyframe_allocations_and_drops_deltas() {
        let mut replay = VideoReplay::default();
        let idr = Bytes::from(vec![1, 2, 3, 4]);
        let delta = Bytes::from(vec![5, 6, 7]);
        replay.push(true, 0, 1, idr.clone());
        replay.push(false, 0, 1, delta);

        let idr_replay = replay.snapshot().expect("complete replay window");
        assert_eq!(idr_replay.len(), 1);
        assert_eq!(idr_replay[0].as_ptr(), idr.as_ptr());
        assert_eq!(idr_replay[0].len(), idr.len());
    }

    #[test]
    fn video_replay_requires_complete_keyframe_and_resets_at_next_idr() {
        let mut replay = VideoReplay::default();
        assert!(replay.seed(vec![Bytes::from_static(b"seed-idr")]));
        assert!(!replay.seed(vec![Bytes::from_static(b"replacement")]));
        assert_eq!(replay.snapshot().unwrap().len(), 1);

        let mut replay = VideoReplay::default();
        replay.push(true, 0, 2, Bytes::from_static(b"first-half"));
        assert!(replay.snapshot().is_none());
        replay.push(true, 1, 2, Bytes::from_static(b"second-half"));
        assert_eq!(replay.snapshot().unwrap().len(), 2);
        replay.push(false, 0, 1, Bytes::from_static(b"delta"));
        assert_eq!(replay.snapshot().unwrap().len(), 2);
        replay.push(true, 0, 1, Bytes::from_static(b"new-idr"));
        assert_eq!(replay.snapshot().unwrap().len(), 1);
    }

    #[test]
    fn receiver_report_sample_converts_loss_rtt_and_remb() {
        let at = std::time::Instant::now();
        let report = super::ReceptionReport {
            ssrc: 42,
            fraction_lost: 64,
            last_sender_report: 100 * 65_536,
            delay: 65_536 / 2,
            ..Default::default()
        };
        let sample =
            super::reception_report_sample(&report, 42, Some(3_000), 101 * 65_536, at).unwrap();
        assert_eq!(sample.fraction_lost, 0.25);
        assert_eq!(sample.rtt, Some(std::time::Duration::from_millis(500)));
        assert_eq!(sample.remb_kbps, Some(3_000));
        assert_eq!(sample.at, at);
        assert!(super::reception_report_sample(&report, 43, None, 101 * 65_536, at).is_none());
    }

    #[test]
    fn receiver_report_sample_rejects_invalid_rtt_but_preserves_loss() {
        let at = std::time::Instant::now();
        for (last_sender_report, delay, now) in [
            (0, 0, 65_536),
            (2 * 65_536, 0, 65_536),
            (65_536, 2 * 65_536, 2 * 65_536),
            (65_536, 0, 65_536),
            (65_536, 0, 11 * 65_536 + 1),
        ] {
            let report = super::ReceptionReport {
                ssrc: 42,
                fraction_lost: 255,
                last_sender_report,
                delay,
                ..Default::default()
            };
            let sample = super::reception_report_sample(&report, 42, None, now, at).unwrap();
            assert!(sample.rtt.is_none());
            assert_eq!(sample.fraction_lost, 255.0 / 256.0);
            assert!(sample.remb_kbps.is_none());
        }
    }

    #[test]
    fn receiver_report_sample_handles_compact_ntp_wrap_and_rtt_boundaries() {
        let at = std::time::Instant::now();
        for (last_sender_report, delay, now, expected_ms) in [
            (u32::MAX - 65_535, 32_768, 0, 500),
            (65_536, 0, 11 * 65_536, 10_000),
        ] {
            let report = super::ReceptionReport {
                ssrc: 42,
                last_sender_report,
                delay,
                ..Default::default()
            };
            let sample = super::reception_report_sample(&report, 42, None, now, at).unwrap();
            assert_eq!(
                sample.rtt,
                Some(std::time::Duration::from_millis(expected_ms))
            );
            assert_eq!(sample.fraction_lost, 0.0);
        }
    }

    #[test]
    fn missing_feedback_and_zero_rtt_do_not_lower_the_initial_rate() {
        let start = std::time::Instant::now();
        let mut controller = crate::adaptive::RateController::new(24_000, 60);
        let initial = controller.decision();
        for second in 0..=30 {
            let at = start + Duration::from_secs(second);
            controller.update_sender(3_000.0, false, at);
            if second < 5 {
                assert_eq!(controller.decision(), initial);
                continue;
            }
            let (last_sender_report, ticks) = match second {
                5 => (0, 65_536),
                6 => (65_536, 0),
                _ => (65_536, 2_621),
            };
            let report = super::ReceptionReport {
                ssrc: 42,
                last_sender_report,
                ..Default::default()
            };
            let sample =
                super::reception_report_sample(&report, 42, None, 65_536 + ticks, at).unwrap();
            controller.observe(sample);
            assert!(controller.decision().bitrate_kbps >= initial.bitrate_kbps);
            assert_eq!(controller.decision().fps, initial.fps);
        }
    }

    #[test]
    fn rejects_cross_session_offer() {
        let value = r#"{"version":1,"sessionId":"other","sequence":0,"sender":"controller","sentAt":1,"payload":{"type":"offer","sdp":"v=0"}}"#;
        assert!(parse_offer(value, "expected").is_err());
    }

    #[test]
    fn accepts_initial_generation_and_rejects_missing_timestamp_offer() {
        let nonzero_sequence = r#"{"version":1,"sessionId":"session","sequence":7,"sender":"controller","sentAt":1,"payload":{"type":"offer","sdp":"v=0"}}"#;
        let restart_generation = r#"{"version":2,"sessionId":"session","controllerEpoch":"controller-a","generation":1,"sequence":0,"sender":"controller","sentAt":1,"payload":{"type":"offer","sdp":"v=0"}}"#;
        let missing_timestamp = r#"{"version":1,"sessionId":"session","sequence":0,"sender":"controller","sentAt":0,"payload":{"type":"offer","sdp":"v=0"}}"#;
        assert!(parse_offer(nonzero_sequence, "session").is_ok());
        assert!(parse_offer(restart_generation, "session").is_ok());
        assert_eq!(
            offer_generation_and_epoch(restart_generation, "session").unwrap(),
            (1, "controller-a".to_owned())
        );
        assert!(parse_offer(missing_timestamp, "session").is_err());
    }

    #[test]
    fn does_not_mistake_candidate_text_for_an_offer() {
        let value = r#"{"version":1,"sessionId":"session","sequence":0,"sender":"controller","sentAt":1,"payload":{"type":"ice-candidate","candidate":"candidate with \\\"type\\\":\\\"offer\\\" text","sdpMid":"0","sdpMLineIndex":0}}"#;
        assert!(!is_offer_for_session(value, "session"));
    }

    #[test]
    fn serializes_host_identity() {
        let value = serialize("session", 3, OutgoingPayload::IceComplete).unwrap();
        assert!(value.contains(r#""sender":"host""#));
        assert!(value.contains(r#""sequence":3"#));
    }

    #[tokio::test]
    async fn terminal_signal_retries_after_normal_publication_is_closed() {
        let (signals_tx, mut signals_rx) = tokio::sync::mpsc::unbounded_channel();
        let publisher = SignalPublisher::flaky_test(signals_tx, Arc::new(AtomicU64::new(1)));
        publisher
            .send_terminal("session", 9, "terminal")
            .await
            .expect("terminal publication should retry once");
        let signal = signals_rx
            .recv()
            .await
            .expect("terminal signal should arrive");
        assert_eq!(signal.session_id, "session");
        assert_eq!(signal.sequence, 9);
        assert_eq!(signal.envelope, "terminal");
    }

    #[test]
    fn serializes_bounded_terminal_negotiation_reason() {
        let value = serialize(
            "session",
            0,
            OutgoingPayload::End {
                reason: "agent could not initialize the remote desktop session".to_owned(),
            },
        )
        .unwrap();
        let envelope: Value = serde_json::from_str(&value).unwrap();
        assert_eq!(envelope["sequence"], 0);
        assert_eq!(envelope["payload"]["type"], "end");
        assert_eq!(
            envelope["payload"]["reason"],
            "agent could not initialize the remote desktop session"
        );
    }

    #[test]
    fn serializes_terminal_media_status() {
        let value = serialize(
            "session",
            4,
            OutgoingPayload::End {
                reason: "media pipeline failed".to_owned(),
            },
        )
        .unwrap();
        assert!(value.contains(r#""type":"end""#));
        assert!(value.contains(r#""reason":"media pipeline failed""#));
    }

    #[tokio::test]
    async fn bounds_control_queue_memory_and_work() {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        let sender = InputSender::Reliable(sender);
        assert_eq!(
            admit_input_message(&sender, Bytes::from(vec![0; MAX_CONTROL_MESSAGE_BYTES])).await,
            InputAdmission::Accepted
        );
        let pending = admit_input_message(&sender, Bytes::from_static(&[1]));
        tokio::pin!(pending);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut pending)
                .await
                .is_err()
        );
        assert_eq!(
            receiver.try_recv().unwrap().len(),
            MAX_CONTROL_MESSAGE_BYTES
        );
        assert_eq!(pending.await, InputAdmission::Accepted);
        assert_eq!(receiver.recv().await.unwrap(), vec![1]);
        assert_eq!(
            admit_input_message(&sender, Bytes::from(vec![0; MAX_CONTROL_MESSAGE_BYTES + 1])).await,
            InputAdmission::Oversized
        );
        drop(receiver);
        assert_eq!(
            admit_input_message(&sender, Bytes::from_static(&[0])).await,
            InputAdmission::Closed
        );
    }

    #[tokio::test]
    async fn reliable_key_and_text_bursts_arrive_in_order_without_pacing() {
        let (sender, receiver) = tokio::sync::mpsc::channel(RELIABLE_CONTROL_QUEUE_CAPACITY);
        let sender = InputSender::Reliable(sender);
        let mut receiver = InputReceiver::Reliable(receiver);
        let events: Vec<_> = (0..3000).map(|index| {
            let event = match index % 3 {
                0 => serde_json::json!({"type":"key", "action":"down", "code":"KeyA", "key":"a", "repeat":false, "sequence":index}),
                1 => serde_json::json!({"type":"key", "action":"up", "code":"KeyA", "key":"a", "repeat":false, "sequence":index}),
                _ => serde_json::json!({"type":"text", "text":format!("commit-{index}-日本語"), "sequence":index}),
            };
            serde_json::to_vec(&event).unwrap()
        }).collect();
        for event in &events[..1000] {
            assert_eq!(
                admit_input_message(&sender, Bytes::from(event.clone())).await,
                InputAdmission::Accepted
            );
        }
        let expected = events.clone();
        let producer = tokio::spawn(async move {
            for event in &events[1000..] {
                assert_eq!(
                    admit_input_message(&sender, Bytes::from(event.clone())).await,
                    InputAdmission::Accepted
                );
            }
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            for event in expected {
                assert_eq!(receiver.recv().await.unwrap(), event);
            }
            producer.await.unwrap();
            assert!(receiver.recv().await.is_none());
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn pointer_motion_is_coalesced_to_the_latest_event() {
        let (sender, receiver) = motion_channel();
        let sender = InputSender::Motion(sender);
        let mut receiver = InputReceiver::Motion(receiver);
        for index in 0_u32..500 {
            assert_eq!(
                admit_input_message(&sender, Bytes::from(index.to_be_bytes().to_vec())).await,
                InputAdmission::Accepted
            );
        }
        assert_eq!(
            receiver.recv().await.unwrap().as_ref(),
            499_u32.to_be_bytes()
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), receiver.recv())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn gamepad_snapshots_and_pointer_motion_coalesce_in_separate_slots() {
        let (sender, receiver) = motion_channel();
        let sender = InputSender::Motion(sender);
        let mut receiver = InputReceiver::Motion(receiver);
        let pointer = |x: u32| {
            let x = x.min(65535) as u16;
            vec![1, x as u8, (x >> 8) as u8, 0, 0]
        };
        let gamepad = |x: u32| {
            let x = x.min(32767) as u16;
            vec![
                2,
                1,
                0,
                0,
                0,
                0,
                0,
                x as u8,
                (x >> 8) as u8,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
            ]
        };
        // Each viewer frame sends its pointer move just before the frame's gamepad poll.
        for frame in 1_u32..=100 {
            for message in [pointer(frame), gamepad(frame)] {
                assert_eq!(
                    admit_input_message(&sender, Bytes::from(message)).await,
                    InputAdmission::Accepted
                );
            }
        }
        let mut delivered = vec![
            receiver.recv().await.unwrap().to_vec(),
            receiver.recv().await.unwrap().to_vec(),
        ];
        delivered.sort();
        assert_eq!(delivered, [pointer(100), gamepad(100)]);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), receiver.recv())
                .await
                .is_err()
        );
    }

    proptest! {
        #[test]
        fn arbitrary_signaling_text_never_bypasses_offer_identity(
            version in any::<u8>(),
            sequence in any::<u64>(),
            sender in ".{0,32}",
            sent_at in any::<u64>(),
            session_id in ".{0,64}",
            expected_session in ".{0,64}",
            sdp in ".{0,2048}",
        ) {
            let envelope = serde_json::json!({
                "version": version,
                "sessionId": session_id,
                "sequence": sequence,
                "sender": sender,
                "sentAt": sent_at,
                "payload": {"type": "offer", "sdp": sdp},
            })
            .to_string();
            let accepted = parse_offer(&envelope, &expected_session).is_ok();
            let should_accept = supported_version(version)
                && session_id == expected_session
                && sender == "controller"
                && sent_at != 0
                && !sdp.is_empty();
            prop_assert_eq!(accepted, should_accept);
        }

        #[test]
        fn arbitrary_text_does_not_panic_the_signaling_parser(
            serialized in proptest::collection::vec(any::<u8>(), 0..4096),
            expected_session in ".{0,64}",
        ) {
            if let Ok(text) = std::str::from_utf8(&serialized) {
                let _ = is_offer_for_session(text, &expected_session);
            }
        }
    }

    #[tokio::test]
    async fn production_host_rejects_malformed_sdp() {
        let malformed_offer = serde_json::json!({
            "version": PROTOCOL_VERSION,
            "sessionId": "malformed-session",
            "sequence": 0,
            "sender": "controller",
            "sentAt": 1,
            "payload": {"type": "offer", "sdp": "not-an-sdp"},
        })
        .to_string();
        let (signals_tx, _signals_rx) = tokio::sync::mpsc::unbounded_channel();

        let result = HostPeer::answer_with_publisher(
            SignalPublisher::test(signals_tx),
            "malformed-session".to_owned(),
            &malformed_offer,
            Vec::new(),
            RTCIceTransportPolicy::All,
            "host-test".to_owned(),
            false,
            false,
            None,
        )
        .await;

        assert!(result.is_err(), "malformed SDP must not create a host peer");
    }

    #[tokio::test]
    async fn production_host_peer_connects_to_a_real_controller_peer() {
        let mut media_engine = MediaEngine::default();
        media_engine.register_default_codecs().unwrap();
        let registry = register_default_interceptors(Registry::new(), &mut media_engine).unwrap();
        let controller = Arc::new(
            APIBuilder::new()
                .with_media_engine(media_engine)
                .with_interceptor_registry(registry)
                .build()
                .new_peer_connection(RTCConfiguration::default())
                .await
                .unwrap(),
        );
        controller
            .add_transceiver_from_kind(RTPCodecType::Video, None)
            .await
            .unwrap();
        let control = controller
            .create_data_channel("nanoctl.control.v1", None)
            .await
            .unwrap();
        let opened = Arc::new(tokio::sync::Notify::new());
        control.on_open({
            let opened = opened.clone();
            Box::new(move || {
                let opened = opened.clone();
                Box::pin(async move {
                    opened.notify_one();
                })
            })
        });

        let offer = controller.create_offer(None).await.unwrap();
        controller.set_local_description(offer).await.unwrap();
        let mut gathering_complete = controller.gathering_complete_promise().await;
        let _ = gathering_complete.recv().await;
        let offer = controller.local_description().await.unwrap();
        let session_id = "integration-session";
        let serialized_offer = serde_json::json!({
            "version": PROTOCOL_VERSION,
            "sessionId": session_id,
            "controllerEpoch": "controller-test",
            "hostEpoch": "",
            "sequence": 0,
            "sender": "controller",
            "sentAt": 1,
            "payload": {"type": "offer", "sdp": offer.sdp},
        })
        .to_string();

        let (signals_tx, mut signals_rx) = tokio::sync::mpsc::unbounded_channel();
        let host = HostPeer::answer_with_publisher(
            SignalPublisher::test(signals_tx),
            session_id.to_owned(),
            &serialized_offer,
            Vec::new(),
            RTCIceTransportPolicy::All,
            "host-test".to_owned(),
            false,
            false,
            None,
        )
        .await
        .unwrap();

        let deadline = Instant::now() + Duration::from_secs(10);
        let mut answer_applied = false;
        let mut first_signal = true;
        while Instant::now() < deadline
            && (controller.connection_state() != RTCPeerConnectionState::Connected
                || !answer_applied)
        {
            let Some(signal) = timeout(Duration::from_millis(250), signals_rx.recv())
                .await
                .ok()
                .flatten()
            else {
                continue;
            };
            assert_eq!(signal.session_id, session_id);
            let envelope: Value = serde_json::from_str(&signal.envelope).unwrap();
            assert_eq!(envelope["sequence"].as_u64(), Some(signal.sequence));
            if first_signal {
                assert_eq!(envelope["payload"]["type"].as_str(), Some("answer"));
                first_signal = false;
            }
            match envelope["payload"]["type"].as_str() {
                Some("answer") => {
                    assert!(signal.sequence > 0);
                    let sdp = envelope["payload"]["sdp"].as_str().unwrap().to_owned();
                    controller
                        .set_remote_description(RTCSessionDescription::answer(sdp).unwrap())
                        .await
                        .unwrap();
                    answer_applied = true;
                }
                Some("ice-candidate") => {
                    controller
                        .add_ice_candidate(RTCIceCandidateInit {
                            candidate: envelope["payload"]["candidate"]
                                .as_str()
                                .unwrap()
                                .to_owned(),
                            sdp_mid: envelope["payload"]["sdpMid"].as_str().map(str::to_owned),
                            sdp_mline_index: envelope["payload"]["sdpMLineIndex"]
                                .as_u64()
                                .map(|value| value as u16),
                            username_fragment: None,
                        })
                        .await
                        .unwrap();
                }
                Some("ice-complete") => {}
                other => panic!("unexpected host signal: {other:?}"),
            }
        }

        assert!(answer_applied, "host did not publish its SDP answer");
        assert_eq!(
            controller.connection_state(),
            RTCPeerConnectionState::Connected
        );
        timeout(Duration::from_secs(3), opened.notified())
            .await
            .expect("negotiated control channel did not open");

        let malformed_candidate = serde_json::json!({
            "version": PROTOCOL_VERSION,
            "sessionId": session_id,
            "controllerEpoch": "controller-test",
            "hostEpoch": "",
            "sequence": 1,
            "sender": "controller",
            "sentAt": 2,
            "payload": {
                "type": "ice-candidate",
                "candidate": "",
                "sdpMid": "0",
                "sdpMLineIndex": 0,
            },
        })
        .to_string();
        assert!(
            host.add_signal(&malformed_candidate, session_id, 1)
                .await
                .is_err(),
            "malformed ICE candidate must be rejected"
        );

        while signals_rx.try_recv().is_ok() {}
        let initial_remote_sdp = controller.remote_description().await.unwrap().sdp;
        let restart_offer = controller
            .create_offer(Some(RTCOfferOptions {
                ice_restart: true,
                ..Default::default()
            }))
            .await
            .unwrap();
        controller
            .set_local_description(restart_offer)
            .await
            .unwrap();
        let mut restart_gathering_complete = controller.gathering_complete_promise().await;
        let _ = restart_gathering_complete.recv().await;
        let restart_offer = controller.local_description().await.unwrap();
        let restart_envelope = serde_json::json!({
            "version": PROTOCOL_VERSION,
            "sessionId": session_id,
            "controllerEpoch": "controller-test",
            "hostEpoch": "",
            "generation": 1,
            "sequence": 2,
            "sender": "controller",
            "sentAt": 3,
            "payload": {"type": "offer", "sdp": restart_offer.sdp},
        })
        .to_string();
        assert!(
            host.add_signal(&restart_envelope, session_id, 2)
                .await
                .unwrap()
        );

        let restart_deadline = Instant::now() + Duration::from_secs(10);
        let mut restart_answer_applied = false;
        let mut pending_candidates = Vec::new();
        while Instant::now() < restart_deadline && !restart_answer_applied {
            let Some(signal) = timeout(Duration::from_millis(250), signals_rx.recv())
                .await
                .ok()
                .flatten()
            else {
                continue;
            };
            let envelope: Value = serde_json::from_str(&signal.envelope).unwrap();
            match envelope["payload"]["type"].as_str() {
                Some("answer") if signal.sequence > 0 => {
                    let sdp = envelope["payload"]["sdp"].as_str().unwrap().to_owned();
                    assert_ne!(sdp, initial_remote_sdp);
                    controller
                        .set_remote_description(RTCSessionDescription::answer(sdp).unwrap())
                        .await
                        .unwrap();
                    restart_answer_applied = true;
                }
                Some("ice-candidate") => pending_candidates.push(envelope),
                Some("ice-complete") => {}
                Some("answer") => {}
                other => panic!("unexpected host restart signal: {other:?}"),
            }
        }
        assert!(
            restart_answer_applied,
            "host did not publish an ICE restart answer"
        );
        for envelope in pending_candidates {
            controller
                .add_ice_candidate(RTCIceCandidateInit {
                    candidate: envelope["payload"]["candidate"]
                        .as_str()
                        .unwrap()
                        .to_owned(),
                    sdp_mid: envelope["payload"]["sdpMid"].as_str().map(str::to_owned),
                    sdp_mline_index: envelope["payload"]["sdpMLineIndex"]
                        .as_u64()
                        .map(|value| value as u16),
                    username_fragment: None,
                })
                .await
                .unwrap();
        }
        timeout(Duration::from_secs(10), async {
            while controller.connection_state() != RTCPeerConnectionState::Connected {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("controller did not reconnect after ICE restart");

        let weak_peer = Arc::downgrade(&host.peer);
        let weak_video = Arc::downgrade(&host.video);
        host.close().await.unwrap();
        controller.close().await.unwrap();
        drop(host);
        timeout(Duration::from_secs(2), async {
            while weak_peer.strong_count() != 0 || weak_video.strong_count() != 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("closed host peer and video track must be released");
    }
}
