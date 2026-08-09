use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
#[cfg(feature = "media")]
use std::time::Duration;
use std::time::SystemTime;

use anyhow::{Context, Result};
use interceptor::registry::Registry;
#[cfg(feature = "media")]
use rtcp::payload_feedbacks::full_intra_request::FullIntraRequest;
#[cfg(feature = "media")]
use rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication;
#[cfg(feature = "media")]
use rtcp::payload_feedbacks::receiver_estimated_maximum_bitrate::ReceiverEstimatedMaximumBitrate;
use serde::{Deserialize, Serialize};
use webrtc::api::APIBuilder;
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::{MIME_TYPE_H264, MediaEngine};
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
use webrtc::rtp_transceiver::RTCPFeedback;
use webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;
use zeroize::Zeroizing;

use crate::control_plane::ControlPlane;

const PROTOCOL_VERSION: u8 = 2;
const MIN_SUPPORTED_PROTOCOL_VERSION: u8 = 1;
const MAX_ICE_GENERATION: u64 = 1_000_000;
// Level 5.2 is the highest H.264 level supported by the configured 4K60 release envelope. The
// WebRTC matcher treats the level byte asymmetrically, so this remains compatible with Chromium's
// lower-level offer while no longer advertising a 720p30 ceiling for a 4K-capable sender.
const H264_PROFILE_LEVEL_ID: &str = "42e034";
#[cfg(any(feature = "media", test))]
pub(crate) const MAX_CONTROL_MESSAGE_BYTES: usize = 64 * 1024;
#[cfg(feature = "media")]
// Reliable control is ordered, but it must remain a short queue. A remote desktop should fail
// closed before stale keystrokes and clicks accumulate behind a slow native input call.
const RELIABLE_CONTROL_QUEUE_CAPACITY: usize = 16;

#[cfg(any(feature = "media", test))]
#[derive(Debug, PartialEq, Eq)]
enum InputAdmission {
    Accepted,
    Full,
    Oversized,
    Closed,
}

#[cfg(any(feature = "media", test))]
fn admit_input_message(
    sender: &tokio::sync::mpsc::Sender<Vec<u8>>,
    bytes: Vec<u8>,
) -> InputAdmission {
    if bytes.len() > MAX_CONTROL_MESSAGE_BYTES {
        return InputAdmission::Oversized;
    }
    match sender.try_send(bytes) {
        Ok(()) => InputAdmission::Accepted,
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => InputAdmission::Full,
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => InputAdmission::Closed,
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
    #[cfg(feature = "media")]
    bitrate_estimate_kbps: tokio::sync::watch::Sender<u32>,
    #[cfg(feature = "media")]
    display_selection: tokio::sync::watch::Sender<String>,
    #[cfg(feature = "media")]
    display_selection_feedback: tokio::sync::watch::Sender<String>,
    #[cfg(feature = "media")]
    input_shutdown: tokio::sync::watch::Sender<bool>,
    #[cfg(feature = "media")]
    input_revoked: Arc<AtomicBool>,
    #[cfg_attr(not(feature = "media"), allow(dead_code))]
    video: Arc<TrackLocalStaticSample>,
}

#[derive(Clone)]
struct SignalPublisher {
    inner: SignalPublisherInner,
    gate: Arc<tokio::sync::Mutex<()>>,
    closed: Arc<AtomicBool>,
}

#[derive(Clone)]
enum SignalPublisherInner {
    ControlPlane {
        client: ControlPlane,
        token: Arc<Zeroizing<String>>,
    },
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
    fn control_plane(client: ControlPlane, token: Arc<Zeroizing<String>>) -> Self {
        Self {
            inner: SignalPublisherInner::ControlPlane { client, token },
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
            SignalPublisherInner::ControlPlane { client, token } => {
                client
                    .send_signal(token, session_id, sequence, envelope)
                    .await
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

impl HostPeer {
    #[allow(clippy::too_many_arguments)]
    pub async fn answer_with_host_epoch(
        control_plane: ControlPlane,
        token: Arc<Zeroizing<String>>,
        session_id: String,
        serialized_offer: &str,
        ice_servers: Vec<RTCIceServer>,
        ice_transport_policy: RTCIceTransportPolicy,
        host_epoch: String,
        allow_remote_input: bool,
    ) -> Result<Self> {
        Self::answer_with_publisher(
            SignalPublisher::control_plane(control_plane, token),
            session_id,
            serialized_offer,
            ice_servers,
            ice_transport_policy,
            host_epoch,
            allow_remote_input,
        )
        .await
    }

    async fn answer_with_publisher(
        publisher: SignalPublisher,
        session_id: String,
        serialized_offer: &str,
        ice_servers: Vec<RTCIceServer>,
        ice_transport_policy: RTCIceTransportPolicy,
        host_epoch: String,
        allow_remote_input: bool,
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
        let mut media_engine = MediaEngine::default();
        media_engine.register_default_codecs()?;
        let registry = register_default_interceptors(Registry::new(), &mut media_engine)?;
        let api = APIBuilder::new()
            .with_media_engine(media_engine)
            .with_interceptor_registry(registry)
            .build();
        #[cfg(not(feature = "media"))]
        let _ = allow_remote_input;
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
        let (display_selection_feedback, display_selection_feedback_receiver) =
            tokio::sync::watch::channel(String::new());
        #[cfg(feature = "media")]
        let (input_shutdown, input_shutdown_receiver) = tokio::sync::watch::channel(false);
        #[cfg(feature = "media")]
        let input_revoked = Arc::new(AtomicBool::new(false));
        #[cfg(feature = "media")]
        let mut input_tasks = Vec::new();
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
                    let mut interval = tokio::time::interval(Duration::from_millis(500));
                    loop {
                        tokio::select! {
                            _ = shutdown.changed() => break,
                            _ = interval.tick() => {
                                let Some(input) = input.upgrade() else {
                                    break;
                                };
                                if let Ok(mut input) = input.lock() {
                                    input.release_if_idle(Duration::from_secs(2));
                                }
                            }
                        }
                    }
                }));
            }
            let (control_sender, control_receiver) =
                tokio::sync::mpsc::channel(RELIABLE_CONTROL_QUEUE_CAPACITY);
            let (pointer_sender, pointer_receiver) = tokio::sync::mpsc::channel(1);
            let fail_safe_release_pending = Arc::new(AtomicBool::new(false));
            if let Some(input) = &input {
                input_tasks.push(spawn_input_worker(
                    control_receiver,
                    input.clone(),
                    crate::input::InputLane::Reliable,
                    Some(display_selection.clone()),
                    Some(display_selection_feedback_receiver),
                    input_shutdown_receiver.clone(),
                    input_revoked.clone(),
                ));
                input_tasks.push(spawn_input_worker(
                    pointer_receiver,
                    input.clone(),
                    crate::input::InputLane::PointerMotion,
                    None,
                    None,
                    input_shutdown_receiver,
                    input_revoked.clone(),
                ));
            }
            let input_revoked_for_channel = input_revoked.clone();
            peer.on_data_channel(Box::new(move |channel: Arc<RTCDataChannel>| {
                let input = input.clone();
                let input_revoked = input_revoked_for_channel.clone();
                if input.is_none() {
                    return Box::pin(async {});
                }
                let (sender, drop_when_full) = match channel.label() {
                    "nanoctl.control.v1" => (control_sender.clone(), false),
                    "nanoctl.pointer.v1" => (pointer_sender.clone(), true),
                    _ => return Box::pin(async {}),
                };
                let message_input = input.clone();
                let release_pending = fail_safe_release_pending.clone();
                channel.on_message(Box::new(move |message: DataChannelMessage| {
                    let sender = sender.clone();
                    let input = message_input.clone();
                    let release_pending = release_pending.clone();
                    let input_revoked = input_revoked.clone();
                    Box::pin(async move {
                        if !message.is_string || input_revoked.load(Ordering::Acquire) {
                            return;
                        }
                        let admission = admit_input_message(&sender, message.data.to_vec());
                        let must_release = !drop_when_full
                            && matches!(
                                admission,
                                InputAdmission::Full | InputAdmission::Oversized
                            );
                        if must_release
                            && release_pending
                                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                                .is_ok()
                        {
                            tracing::warn!(
                                ?admission,
                                "reliable input flood triggered fail-safe release"
                            );
                            let release_pending = release_pending.clone();
                            tokio::task::spawn_blocking(move || {
                                if let Some(input) = input
                                    && let Ok(mut input) = input.lock()
                                {
                                    input.release_all();
                                }
                                release_pending.store(false, Ordering::Release);
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
                        }
                    })
                }));
                Box::pin(async {})
            }));
        }
        let video = Arc::new(TrackLocalStaticSample::new(
            RTCRtpCodecCapability {
                mime_type: MIME_TYPE_H264.to_owned(),
                clock_rate: 90_000,
                channels: 0,
                sdp_fmtp_line: format!(
                    "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id={H264_PROFILE_LEVEL_ID}"
                ),
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
        #[cfg(feature = "media")]
        let (bitrate_estimate_kbps, _) = tokio::sync::watch::channel(0_u32);
        #[cfg(feature = "media")]
        let bitrate_feedback = bitrate_estimate_kbps.clone();
        let rtcp_task = tokio::spawn(async move {
            while let Ok((_packets, _attributes)) = sender.read_rtcp().await {
                #[cfg(feature = "media")]
                {
                    if _packets.iter().any(|packet| {
                        packet.as_any().is::<PictureLossIndication>()
                            || packet.as_any().is::<FullIntraRequest>()
                    }) {
                        keyframe_feedback.send_modify(|generation| {
                            *generation = generation.wrapping_add(1);
                        });
                    }
                    if let Some(estimate) = _packets.iter().find_map(|packet| {
                        packet
                            .as_any()
                            .downcast_ref::<ReceiverEstimatedMaximumBitrate>()
                    }) && estimate.bitrate.is_finite()
                        && estimate.bitrate > 0.0
                    {
                        let kbps = (estimate.bitrate / 1_000.0).min(u32::MAX as f32) as u32;
                        bitrate_feedback.send_replace(kbps.max(1));
                    }
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
            #[cfg(feature = "media")]
            bitrate_estimate_kbps,
            #[cfg(feature = "media")]
            display_selection,
            #[cfg(feature = "media")]
            display_selection_feedback,
            #[cfg(feature = "media")]
            input_shutdown,
            #[cfg(feature = "media")]
            input_revoked,
            video,
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
    pub fn video_track(&self) -> Arc<TrackLocalStaticSample> {
        self.video.clone()
    }

    #[cfg(feature = "media")]
    pub fn keyframe_requests(&self) -> tokio::sync::watch::Receiver<u64> {
        self.keyframe_requests.subscribe()
    }

    #[cfg(feature = "media")]
    pub fn bitrate_estimate_kbps(&self) -> tokio::sync::watch::Receiver<u32> {
        self.bitrate_estimate_kbps.subscribe()
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
fn spawn_input_worker(
    mut receiver: tokio::sync::mpsc::Receiver<Vec<u8>>,
    input: Arc<std::sync::Mutex<crate::input::InputController>>,
    lane: crate::input::InputLane,
    display_selection: Option<tokio::sync::watch::Sender<String>>,
    mut display_selection_feedback: Option<tokio::sync::watch::Receiver<String>>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
    input_revoked: Arc<AtomicBool>,
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
                        input.dispatch(&bytes, lane)
                    })
                    .await;
                    match result {
                        Ok(Ok(Some(display_id))) => {
                            if let Some(selection) = &display_selection {
                                selection.send_replace(display_id);
                            }
                        }
                        Ok(Ok(None)) => {}
                        Ok(Err(error)) => tracing::warn!(error = %error, "control message rejected"),
                        Err(error) => {
                            tracing::warn!(error = %error, "input worker stopped unexpectedly");
                            break;
                        }
                    }
                }
            }
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
    // Convex transports sequences as JSON numbers, so use 53 random bits. Unlike a timestamp/PID
    // recipe this remains fresh across rapid process restarts and PID reuse; the chance of
    // colliding with retained signals is negligible. Database insertion time, not this seed,
    // determines replay order across process epochs.
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

pub async fn report_negotiation_failure(
    control_plane: &ControlPlane,
    token: &Arc<Zeroizing<String>>,
    session_id: &str,
    controller_epoch: &str,
    host_epoch: &str,
    generation: u64,
    reason: &str,
) -> Result<()> {
    // A terminal signal gets its own fresh host epoch. Sequence zero has no semantic meaning and
    // must not be required for an offer because ICE callbacks can race local-description setup.
    let reason = reason.chars().take(256).collect::<String>();
    let sequence = host_sequence_seed();
    let envelope = serialize_with_generation(
        session_id,
        controller_epoch,
        host_epoch,
        generation,
        sequence,
        OutgoingPayload::End { reason },
    )?;
    SignalPublisher::control_plane(control_plane.clone(), Arc::clone(token))
        .send_terminal(session_id, sequence, &envelope)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use serde_json::Value;
    use tokio::time::{Duration, Instant, timeout};
    use webrtc::peer_connection::offer_answer_options::RTCOfferOptions;
    use webrtc::rtp_transceiver::rtp_codec::RTPCodecType;

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

    #[test]
    fn bounds_control_queue_memory_and_work() {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        assert_eq!(
            admit_input_message(&sender, vec![0; MAX_CONTROL_MESSAGE_BYTES]),
            InputAdmission::Accepted
        );
        assert_eq!(
            admit_input_message(&sender, vec![0; 1]),
            InputAdmission::Full
        );
        assert_eq!(
            receiver.try_recv().unwrap().len(),
            MAX_CONTROL_MESSAGE_BYTES
        );
        assert_eq!(
            admit_input_message(&sender, vec![0; MAX_CONTROL_MESSAGE_BYTES + 1]),
            InputAdmission::Oversized
        );
        drop(receiver);
        assert_eq!(
            admit_input_message(&sender, vec![0; 1]),
            InputAdmission::Closed
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

        host.close().await.unwrap();
        controller.close().await.unwrap();
    }
}
